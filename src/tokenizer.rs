//! Token counting module.
//!
//! Provides [`TokenCounter`], a selectable strategy for counting tokens:
//!
//! - [`TokenCounter::Real`] — real BPE token counting via `tiktoken-rs`, using a
//!   cached (`&'static`) encoding instance. No per-request allocation of the vocab.
//! - [`TokenCounter::Estimated`] — the legacy character-count approximation
//!   `(len + 3) / 4`, preserved bit-for-bit for backward compatibility.
//!
//! The counting mode is controlled by `compactor.token_counting` in `config.yaml`
//! (default `estimated`). `tokenizer_name` selects the encoding only when the mode
//! is `real`; it is never a trigger for enabling real counting.

use tiktoken_rs::CoreBPE;

use crate::config::TokenCountingMode;

/// A token counter with a selectable counting strategy.
///
/// Constructed via [`TokenCounter::resolve`]. The real variant borrows a
/// process-wide singleton encoding, so constructing a counter is cheap and never
/// re-initializes the BPE vocabulary.
pub enum TokenCounter {
    /// Real BPE counting backed by a cached tiktoken encoding.
    Real(&'static CoreBPE),
    /// Character-count approximation (legacy behavior).
    Estimated,
}

impl TokenCounter {
    /// Resolve a counter from the configured mode and encoding name.
    ///
    /// - `Estimated` mode → [`TokenCounter::Estimated`] (`tokenizer_name` ignored).
    /// - `Real` mode + supported encoding → [`TokenCounter::Real`].
    /// - `Real` mode + unknown encoding → [`TokenCounter::Estimated`] (logs a warning;
    ///   never fails the request).
    ///
    /// Supported encodings: `cl100k_base`, `o200k_base`.
    pub fn resolve(mode: TokenCountingMode, tokenizer_name: &str) -> TokenCounter {
        match mode {
            TokenCountingMode::Estimated => TokenCounter::Estimated,
            TokenCountingMode::Real => match tokenizer_name {
                "cl100k_base" => TokenCounter::Real(tiktoken_rs::cl100k_base_singleton()),
                "o200k_base" => TokenCounter::Real(tiktoken_rs::o200k_base_singleton()),
                other => {
                    tracing::warn!(
                        tokenizer_name = %other,
                        "token_counting is 'real' but tokenizer_name is not a supported encoding \
                         (cl100k_base, o200k_base); falling back to estimated counting"
                    );
                    TokenCounter::Estimated
                }
            },
        }
    }

    /// Count the number of tokens in `text` according to the active strategy.
    pub fn count(&self, text: &str) -> usize {
        match self {
            TokenCounter::Real(bpe) => bpe.encode_with_special_tokens(text).len(),
            TokenCounter::Estimated => estimate_tokens(text),
        }
    }

    /// Prometheus label for the active mode: `"real"` or `"estimated"`.
    pub fn mode_label(&self) -> &'static str {
        match self {
            TokenCounter::Real(_) => "real",
            TokenCounter::Estimated => "estimated",
        }
    }
}

/// Legacy character-count approximation: `(len + 3) / 4`, with `0` for empty text.
///
/// This is the exact formula historically used by the compactor and MUST remain
/// bit-for-bit identical to preserve backward compatibility in `estimated` mode.
#[inline]
pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.len();
    if chars == 0 {
        0
    } else {
        // `(chars + 3) / 4` — ceiling division, bit-for-bit equal to the legacy formula.
        chars.div_ceil(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimated_mode_ignores_tokenizer_name() {
        let counter = TokenCounter::resolve(TokenCountingMode::Estimated, "cl100k_base");
        assert_eq!(counter.mode_label(), "estimated");
        assert!(matches!(counter, TokenCounter::Estimated));
    }

    #[test]
    fn real_mode_with_cl100k_resolves_real() {
        let counter = TokenCounter::resolve(TokenCountingMode::Real, "cl100k_base");
        assert_eq!(counter.mode_label(), "real");
        assert!(matches!(counter, TokenCounter::Real(_)));
    }

    #[test]
    fn real_mode_with_o200k_resolves_real() {
        let counter = TokenCounter::resolve(TokenCountingMode::Real, "o200k_base");
        assert_eq!(counter.mode_label(), "real");
    }

    #[test]
    fn real_mode_with_unknown_encoding_falls_back_to_estimated() {
        let counter = TokenCounter::resolve(TokenCountingMode::Real, "not-a-real-encoding");
        assert_eq!(counter.mode_label(), "estimated");
        assert!(matches!(counter, TokenCounter::Estimated));
    }

    #[test]
    fn estimated_count_matches_legacy_formula() {
        // 4 chars = 1 token
        assert_eq!(estimate_tokens("abcd"), 1);
        // 5 chars = 2 tokens (ceiling)
        assert_eq!(estimate_tokens("abcde"), 2);
        // empty = 0
        assert_eq!(estimate_tokens(""), 0);
        // 1 char = 1 token (minimum for non-empty)
        assert_eq!(estimate_tokens("a"), 1);

        let counter = TokenCounter::Estimated;
        assert_eq!(counter.count("abcd"), 1);
        assert_eq!(counter.count(""), 0);
    }

    #[test]
    fn real_count_differs_from_estimate_for_sample_text() {
        // A representative chat sentence: BPE tokenization differs from the
        // char/4 approximation. This is the core premise of Phase 1.
        let sample = "The quick brown fox jumps over the lazy dog, and then \
                      it asks: how many tokens is this, really?";
        let real = TokenCounter::resolve(TokenCountingMode::Real, "cl100k_base");
        let estimated = TokenCounter::Estimated;
        assert_ne!(
            real.count(sample),
            estimated.count(sample),
            "real BPE count should differ from the char-count approximation"
        );
    }

    #[test]
    fn real_count_is_nonzero_for_nonempty_text() {
        let real = TokenCounter::resolve(TokenCountingMode::Real, "cl100k_base");
        assert!(real.count("hello world") > 0);
        assert_eq!(real.count(""), 0);
    }

    /// Rough benchmark of real BPE counting cost for a typical chat payload.
    ///
    /// Ignored by default (timing is environment-dependent). Run with:
    /// `cargo test tokenizer::tests::bench_real_count_typical_chat -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_real_count_typical_chat() {
        let counter = TokenCounter::resolve(TokenCountingMode::Real, "cl100k_base");
        // ~a few hundred tokens of chat context.
        let payload = "You are a helpful assistant. ".repeat(20)
            + &"Summarize the attached quarterly report and highlight risks. ".repeat(20);

        // Warm the singleton.
        let _ = counter.count(&payload);

        let iters = 1000u32;
        let start = std::time::Instant::now();
        let mut sink = 0usize;
        for _ in 0..iters {
            sink = sink.wrapping_add(counter.count(&payload));
        }
        let per_call = start.elapsed() / iters;
        // Print-only: timing depends on build profile and host. For reference,
        // a release build counts a ~341-token payload in ~80µs (< 1ms); debug
        // builds are ~1ms. Run with --release for representative numbers.
        println!(
            "real BPE count: {} tokens, avg {:?} per call over {} iters (sink={})",
            counter.count(&payload),
            per_call,
            iters,
            sink
        );
    }
}
