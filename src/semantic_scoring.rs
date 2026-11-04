//! Phase 5 — opt-in embedding-based scoring for `semantic_guarded_trimming`.
//!
//! Split of responsibilities:
//! - **async** ([`compact_with_scoring`], called by the handler): segments the
//!   payload into units, decides whether scoring is needed, embeds the candidates
//!   (batched, bounded) and validates the vectors;
//! - **sync & deterministic** ([`select_with_scores`]): receives ready-made
//!   similarities, normalizes, combines, removes the lowest-scoring candidates and
//!   restores the original order.
//!
//! Any failure falls back to the legacy pin-aware path
//! ([`compact_json_messages`] + [`SemanticGuardedCompactor`], bugfix D1) for the
//! whole request. Nothing here logs or returns message text or vectors.
//!
//! The similarity mapping `(cos + 1) / 2` is a scale transform to `[0, 1]`, **not**
//! a probability of relevance. Default weights (0.5 / 0.5) are provisional and
//! not empirically calibrated.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

use crate::compactor::{
    compact_json_messages, must_pin, text_repr, CompactMessage, CompactionResult, JsonCompaction,
    SemanticGuardedCompactor,
};
use crate::config::CompactorConfig;
use crate::embeddings::EmbeddingProvider;
use crate::tokenizer::TokenCounter;

// ─── Configuration ────────────────────────────────────────────────────────────

/// Scoring mode. `heuristic` = legacy path (no embeddings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScoringMode {
    #[default]
    Heuristic,
    Embedding,
    Hybrid,
}

impl ScoringMode {
    pub fn label(&self) -> &'static str {
        match self {
            ScoringMode::Heuristic => "heuristic",
            ScoringMode::Embedding => "embedding",
            ScoringMode::Hybrid => "hybrid",
        }
    }
}

/// What may happen to a *complete* tool-call group during scoring (D2 closure).
///
/// `preserve` (default) treats a complete group as protected: it is never removed.
/// `eligible_for_removal` lets a complete group be removed, but only as a whole
/// unit. Incomplete/ambiguous groups and protected instructions are pinned under
/// both policies. Preserving the call↔result protocol dependency is NOT a claim of
/// semantic preservation of the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolGroupPolicy {
    #[default]
    Preserve,
    EligibleForRemoval,
}

/// Budget semantics. `legacy` keeps the existing ratio-based target unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode {
    #[default]
    Legacy,
    /// Hard limit `max_context_tokens`; error when mandatory content can't fit.
    Strict,
}

/// Per-message formatting overhead profile (D6). Only applied in `strict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FormattingOverhead {
    /// Content tokens only (legacy counting).
    #[default]
    None,
    /// Estimate: 3 tokens per message + 3 reply-priming tokens. An estimate of
    /// OpenAI chat framing, not a billing guarantee for any model.
    OpenaiChatEstimate,
}

impl FormattingOverhead {
    pub fn per_message(&self) -> usize {
        match self {
            FormattingOverhead::None => 0,
            FormattingOverhead::OpenaiChatEstimate => 3,
        }
    }

    pub fn priming(&self) -> usize {
        match self {
            FormattingOverhead::None => 0,
            FormattingOverhead::OpenaiChatEstimate => 3,
        }
    }

    /// Overhead for `messages` forwarded messages.
    pub fn total(&self, messages: usize) -> usize {
        self.per_message() * messages + self.priming()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    #[serde(default)]
    pub mode: BudgetMode,
    /// Required in `strict`: hard limit for content + overhead + output reserve.
    #[serde(default)]
    pub max_context_tokens: Option<usize>,
    /// `strict` only: tokens kept free for the model's answer.
    #[serde(default)]
    pub output_reserve_tokens: usize,
    #[serde(default)]
    pub formatting_overhead: FormattingOverhead,
}

fn default_weight() -> f64 {
    0.5
}
fn default_embedding_model() -> String {
    "nomic-embed-text".to_string()
}
fn default_timeout_ms() -> u64 {
    1500
}
fn default_max_concurrency() -> usize {
    4
}
fn default_batch_size() -> usize {
    32
}
fn default_max_messages() -> usize {
    64
}
fn default_max_chars() -> usize {
    2000
}

/// Full scoring configuration (`token_optimization.semantic_scoring` block).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoringConfig {
    #[serde(default)]
    pub mode: ScoringMode,
    /// Provisional default 0.5 — not calibrated.
    #[serde(default = "default_weight")]
    pub semantic_weight: f64,
    /// Provisional default 0.5 — not calibrated.
    #[serde(default = "default_weight")]
    pub structural_weight: f64,
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
    /// Total time budget for scoring (all embedding calls) per request.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Maximum embedding batches in flight.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    /// Texts per embedding call (when the backend batches natively).
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Maximum candidates; above it → full fallback to the heuristic (D5).
    #[serde(default = "default_max_messages")]
    pub max_messages: usize,
    /// Characters sent per text (scoring only; the message is not changed).
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// Whether complete tool-call groups may be removed. Default `preserve`.
    #[serde(default)]
    pub tool_group_policy: ToolGroupPolicy,
    #[serde(default)]
    pub budget: BudgetConfig,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            mode: ScoringMode::default(),
            semantic_weight: default_weight(),
            structural_weight: default_weight(),
            embedding_model: default_embedding_model(),
            timeout_ms: default_timeout_ms(),
            max_concurrency: default_max_concurrency(),
            batch_size: default_batch_size(),
            max_messages: default_max_messages(),
            max_chars: default_max_chars(),
            tool_group_policy: ToolGroupPolicy::default(),
            budget: BudgetConfig::default(),
        }
    }
}

/// `semantic_scoring: hybrid` (short form) or a full block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ScoringSetting {
    Mode(ScoringMode),
    Full(ScoringConfig),
}

impl ScoringSetting {
    pub fn resolve(&self) -> ScoringConfig {
        match self {
            ScoringSetting::Mode(mode) => ScoringConfig {
                mode: *mode,
                ..Default::default()
            },
            ScoringSetting::Full(c) => c.clone(),
        }
    }
}

impl ScoringConfig {
    /// Validate values. Errors are human-readable and contain no request data.
    pub fn validate(&self) -> Result<(), String> {
        let (s, t) = (self.semantic_weight, self.structural_weight);
        if !s.is_finite() || !t.is_finite() {
            return Err("semantic_weight/structural_weight must be finite".into());
        }
        if s < 0.0 || t < 0.0 {
            return Err("semantic_weight/structural_weight must be >= 0".into());
        }
        if s + t <= 0.0 {
            return Err("semantic_weight + structural_weight must be > 0".into());
        }
        for (name, v) in [
            ("timeout_ms", self.timeout_ms as usize),
            ("max_concurrency", self.max_concurrency),
            ("batch_size", self.batch_size),
            ("max_messages", self.max_messages),
            ("max_chars", self.max_chars),
        ] {
            if v == 0 {
                return Err(format!("{name} must be > 0"));
            }
        }
        if self.embedding_model.trim().is_empty() {
            return Err("embedding_model must not be empty".into());
        }
        if self.budget.mode == BudgetMode::Strict {
            match self.budget.max_context_tokens {
                None | Some(0) => {
                    return Err("budget.mode strict requires budget.max_context_tokens > 0".into())
                }
                Some(max) if self.budget.output_reserve_tokens >= max => {
                    return Err(
                        "budget.output_reserve_tokens must be < budget.max_context_tokens".into(),
                    )
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Normalized `(w_semantic, w_structural)`, summing to 1.
    /// `embedding` mode is `(1, 0)` by definition.
    pub fn effective_weights(&self) -> (f64, f64) {
        match self.mode {
            ScoringMode::Embedding => (1.0, 0.0),
            _ => {
                let sum = self.semantic_weight + self.structural_weight;
                (self.semantic_weight / sum, self.structural_weight / sum)
            }
        }
    }

    fn strict(&self) -> Option<(usize, usize, FormattingOverhead)> {
        match (self.budget.mode, self.budget.max_context_tokens) {
            (BudgetMode::Strict, Some(max)) => Some((
                max,
                self.budget.output_reserve_tokens,
                self.budget.formatting_overhead,
            )),
            _ => None,
        }
    }
}

// ─── Units (structural preservation, D1/D2) ───────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// Plain text message: may be removed.
    Text,
    /// `assistant.tool_calls` + all of its `role: tool` results: removed or kept
    /// as a whole.
    ToolGroup,
    /// Anything that can't be handled safely (D1 pins, incomplete tool
    /// dependencies, orphan results): always kept verbatim.
    Pinned,
}

#[derive(Debug, Clone)]
pub struct Unit {
    /// Original message indices (contiguous, increasing).
    pub indices: Vec<usize>,
    pub kind: UnitKind,
    pub role: String,
    /// Scoring/counting text only; never forwarded.
    pub text: String,
    /// Content tokens (role + text per message, active counter).
    pub tokens: usize,
}

fn role_of(m: &Value) -> &str {
    m.get("role").and_then(|r| r.as_str()).unwrap_or_default()
}

fn message_tokens(counter: &TokenCounter, m: &Value) -> usize {
    // Same counting as the D1 adapter: role + text representation.
    counter.count(role_of(m)) + counter.count(&text_repr(m))
}

/// Text of ONE message for scoring, excluding opaque tool-call/result identifiers.
///
/// `text_repr` includes the whole `tool_calls` JSON (ids + arguments) and a tool
/// result keeps its `tool_call_id` only via its content. Opaque ids like `call_1`
/// carry no semantics but perturb the structural heuristic (digits read as a
/// "technical" signal) and add noise to embeddings. This representation is used
/// ONLY for scoring/counting; the forwarded payload is never modified, and the
/// legacy text-only path (`text_repr`) is unchanged.
fn scoring_text(m: &Value) -> String {
    let mut out = match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(
                |p| match (p.get("type").and_then(|t| t.as_str()), p.get("text")) {
                    (Some("text"), Some(Value::String(t))) => Some(t.clone()),
                    _ => None,
                },
            )
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    // Function name + arguments are semantic; the call id is not.
    for call in m
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        if let Some(f) = call.get("function") {
            for key in ["name", "arguments"] {
                if let Some(Value::String(s)) = f.get(key) {
                    out.push(' ');
                    out.push_str(s);
                }
            }
        }
    }
    if let Some(Value::String(s)) = m.get("function_call").and_then(|f| f.get("arguments")) {
        out.push(' ');
        out.push_str(s);
    }
    out
}

/// Ids of a well-formed assistant tool-call message, or `None`.
fn tool_call_ids(m: &Value) -> Option<Vec<String>> {
    if role_of(m) != "assistant" || m.get("function_call").is_some_and(|v| !v.is_null()) {
        return None;
    }
    match m.get("content") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        _ => return None,
    }
    let calls = m.get("tool_calls")?.as_array()?;
    if calls.is_empty() {
        return None;
    }
    calls
        .iter()
        .map(|c| c.get("id").and_then(|i| i.as_str()).map(str::to_string))
        .collect()
}

/// Segment messages into units. Conservative: a tool group is formed only when an
/// assistant call is immediately followed by exactly one result per call id
/// (no duplicates, no extras). Anything else involved is pinned.
pub fn build_units(messages: &[Value], counter: &TokenCounter) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        let m = &messages[i];
        if let Some(ids) = tool_call_ids(m) {
            let mut j = i + 1;
            while j < messages.len() && role_of(&messages[j]) == "tool" {
                j += 1;
            }
            let mut wanted: std::collections::BTreeSet<&str> =
                ids.iter().map(String::as_str).collect();
            let unique_ids = wanted.len() == ids.len();
            let mut complete = unique_ids && (j - i - 1) == ids.len();
            if complete {
                for r in &messages[i + 1..j] {
                    let id = r.get("tool_call_id").and_then(|v| v.as_str());
                    if !id.is_some_and(|id| wanted.remove(id)) {
                        complete = false;
                        break;
                    }
                }
                complete &= wanted.is_empty();
            }
            let members: Vec<usize> = (i..j).collect();
            if complete {
                units.push(Unit {
                    text: members
                        .iter()
                        .map(|&k| scoring_text(&messages[k]))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    tokens: members
                        .iter()
                        .map(|&k| message_tokens(counter, &messages[k]))
                        .sum(),
                    indices: members,
                    kind: UnitKind::ToolGroup,
                    role: "assistant".to_string(),
                });
            } else {
                for k in members {
                    units.push(single(messages, k, UnitKind::Pinned, counter));
                }
            }
            i = j;
            continue;
        }
        let kind = if must_pin(m) {
            UnitKind::Pinned
        } else {
            UnitKind::Text
        };
        units.push(single(messages, i, kind, counter));
        i += 1;
    }
    units
}

fn single(messages: &[Value], k: usize, kind: UnitKind, counter: &TokenCounter) -> Unit {
    Unit {
        indices: vec![k],
        kind,
        role: role_of(&messages[k]).to_string(),
        // For plain text messages this equals `text_repr`; it only differs when
        // tool fields are present, which never happens for a `Text` unit.
        text: scoring_text(&messages[k]),
        tokens: message_tokens(counter, &messages[k]),
    }
}

// ─── Outcome types ────────────────────────────────────────────────────────────

/// Why scoring fell back to the heuristic (bounded label set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    LimitExceeded,
    Timeout,
    ProviderError,
    CountMismatch,
    InvalidDimension,
    NonFinite,
    ZeroNorm,
    NoQuery,
}

impl FallbackReason {
    pub const ALL: [FallbackReason; 8] = [
        FallbackReason::LimitExceeded,
        FallbackReason::Timeout,
        FallbackReason::ProviderError,
        FallbackReason::CountMismatch,
        FallbackReason::InvalidDimension,
        FallbackReason::NonFinite,
        FallbackReason::ZeroNorm,
        FallbackReason::NoQuery,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            FallbackReason::LimitExceeded => "limit_exceeded",
            FallbackReason::Timeout => "timeout",
            FallbackReason::ProviderError => "provider_error",
            FallbackReason::CountMismatch => "count_mismatch",
            FallbackReason::InvalidDimension => "invalid_dimension",
            FallbackReason::NonFinite => "non_finite",
            FallbackReason::ZeroNorm => "zero_norm",
            FallbackReason::NoQuery => "no_query",
        }
    }
}

/// Selection doesn't depend on scores, so no embedding is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    NoCandidates,
    MandatoryExceedsTarget,
}

impl SkipReason {
    pub const ALL: [SkipReason; 2] = [SkipReason::NoCandidates, SkipReason::MandatoryExceedsTarget];

    pub fn label(&self) -> &'static str {
        match self {
            SkipReason::NoCandidates => "no_candidates",
            SkipReason::MandatoryExceedsTarget => "mandatory_exceeds_target",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScoringOutcome {
    /// Forwardable messages (original objects) + metrics.
    pub compaction: JsonCompaction,
    /// False when compaction wasn't needed (below threshold): nothing was scored.
    pub needed: bool,
    pub requested: ScoringMode,
    /// Mode that actually ranked the candidates (`heuristic` on fallback/skip).
    pub effective: ScoringMode,
    pub fallback: Option<FallbackReason>,
    pub skipped: Option<SkipReason>,
    pub candidates: usize,
    /// Texts sent to the embedding provider (query included).
    pub embedded: usize,
    pub elapsed: Duration,
}

/// `strict` budget cannot hold the mandatory content (D6). Counts only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetInsufficient {
    pub required_tokens: usize,
    pub overhead_tokens: usize,
    pub output_reserve_tokens: usize,
    pub max_context_tokens: usize,
}

impl BudgetInsufficient {
    pub fn message(&self) -> String {
        format!(
            "Insufficient context budget: mandatory content ({} tokens) + formatting overhead ({}) + output reserve ({}) exceeds max_context_tokens ({}). Protected instructions and tool messages are never removed to fit.",
            self.required_tokens,
            self.overhead_tokens,
            self.output_reserve_tokens,
            self.max_context_tokens
        )
    }
}

// ─── Embedding step (async, bounded, validated) ───────────────────────────────

fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Embed `texts` within the configured limits and validate the vectors.
pub async fn embed_validated(
    provider: &dyn EmbeddingProvider,
    sc: &ScoringConfig,
    texts: &[String],
) -> Result<Vec<Vec<f64>>, FallbackReason> {
    let call = provider.embed_batch(
        &sc.embedding_model,
        texts,
        sc.batch_size,
        sc.max_concurrency,
    );
    let vectors = match tokio::time::timeout(Duration::from_millis(sc.timeout_ms), call).await {
        Err(_) => return Err(FallbackReason::Timeout),
        Ok(None) => return Err(FallbackReason::ProviderError),
        Ok(Some(v)) => v,
    };
    validate_vectors(&vectors, texts.len())?;
    Ok(vectors)
}

/// Checks done before any cosine: count, equal non-zero dimension, finite, norm > 0.
pub fn validate_vectors(vectors: &[Vec<f64>], expected: usize) -> Result<(), FallbackReason> {
    if vectors.len() != expected {
        return Err(FallbackReason::CountMismatch);
    }
    let dim = vectors.first().map(Vec::len).unwrap_or(0);
    if dim == 0 || vectors.iter().any(|v| v.len() != dim) {
        return Err(FallbackReason::InvalidDimension);
    }
    if vectors.iter().flatten().any(|x| !x.is_finite()) {
        return Err(FallbackReason::NonFinite);
    }
    if vectors.iter().any(|v| norm(v) == 0.0) {
        return Err(FallbackReason::ZeroNorm);
    }
    Ok(())
}

fn norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// `(cos + 1) / 2`, clamped to `[0, 1]`. A scale transform, not a probability.
pub fn similarity(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let cos = dot / (norm(a) * norm(b));
    if !cos.is_finite() {
        // Unreachable after `validate_vectors`; neutral value as a last resort.
        return 0.5;
    }
    ((cos.clamp(-1.0, 1.0) + 1.0) / 2.0).clamp(0.0, 1.0)
}

/// Min-max normalization to `[0, 1]`; all equal → 0.5.
pub fn normalize_structural(raw: &[i32]) -> Vec<f64> {
    let (Some(&min), Some(&max)) = (raw.iter().min(), raw.iter().max()) else {
        return Vec::new();
    };
    if max == min {
        return vec![0.5; raw.len()];
    }
    let span = (max - min) as f64;
    raw.iter().map(|&r| (r - min) as f64 / span).collect()
}

// ─── Selection (sync, deterministic) ──────────────────────────────────────────

/// Request analysis shared by the async and sync steps.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub units: Vec<Unit>,
    pub original_tokens: usize,
    pub message_count: usize,
    pub target_tokens: usize,
    pub mandatory_tokens: usize,
    pub mandatory_messages: usize,
    /// Indices into `units` of removable candidates, in original order.
    pub candidates: Vec<usize>,
    /// Text of the last user message (scoring only).
    pub query: Option<String>,
    pub query_terms: Vec<String>,
}

pub fn prepare(messages: &[Value], cfg: &CompactorConfig, policy: ToolGroupPolicy) -> Prepared {
    let counter = TokenCounter::resolve(cfg.token_counting, &cfg.tokenizer_name);
    let units = build_units(messages, &counter);
    let last_user = messages.iter().rposition(|m| role_of(m) == "user");
    let markers: Vec<String> = cfg
        .critical_markers
        .iter()
        .map(|m| m.to_lowercase())
        .collect();

    let protected: Vec<bool> = units
        .iter()
        .map(|u| {
            u.kind == UnitKind::Pinned
                || u.role == "system"
                || last_user.is_some_and(|l| u.indices.contains(&l))
                // Complete tool groups are protected unless removal is opted in.
                || (u.kind == UnitKind::ToolGroup && policy == ToolGroupPolicy::Preserve)
                || (cfg.preserve_critical_markers && {
                    let lower = u.text.to_lowercase();
                    markers.iter().any(|m| lower.contains(m.as_str()))
                })
        })
        .collect();

    let original_tokens = units.iter().map(|u| u.tokens).sum();
    let target_tokens =
        (((original_tokens as f64) * cfg.target_token_ratio) as usize).max(cfg.min_final_tokens);
    let mandatory: Vec<&Unit> = units
        .iter()
        .zip(&protected)
        .filter(|(_, p)| **p)
        .map(|(u, _)| u)
        .collect();
    let query = last_user.map(|l| text_repr(&messages[l]));
    let query_terms = query
        .as_deref()
        .map(SemanticGuardedCompactor::extract_query_terms)
        .unwrap_or_default();

    Prepared {
        mandatory_tokens: mandatory.iter().map(|u| u.tokens).sum(),
        mandatory_messages: mandatory.iter().map(|u| u.indices.len()).sum(),
        candidates: (0..units.len()).filter(|&k| !protected[k]).collect(),
        units,
        original_tokens,
        message_count: messages.len(),
        target_tokens,
        query,
        query_terms,
    }
}

/// Combined scores for `prep.candidates` (same order). `sims[k]` = similarity of
/// candidate `k` to the query, already mapped to `[0, 1]`.
pub fn combined_scores(
    prep: &Prepared,
    cfg: &CompactorConfig,
    sc: &ScoringConfig,
    sims: &[f64],
) -> Vec<f64> {
    let raw: Vec<i32> = prep
        .candidates
        .iter()
        .map(|&k| {
            let u = &prep.units[k];
            SemanticGuardedCompactor::calculate_score(
                &CompactMessage {
                    role: u.role.clone(),
                    content: u.text.clone(),
                },
                false,
                &prep.query_terms,
                &cfg.critical_markers,
            )
        })
        .collect();
    let structural = normalize_structural(&raw);
    let (ws, wt) = sc.effective_weights();
    sims.iter()
        .zip(&structural)
        .map(|(s, t)| ws * s + wt * t)
        .collect()
}

/// Remove the lowest-scoring candidates until the budget holds, then emit the
/// ORIGINAL objects of the kept messages in original order. Deterministic:
/// ties broken by original position (oldest removed first). Content is never
/// rewritten (no stop-word removal in this mode).
pub fn select_with_scores(
    messages: &[Value],
    prep: &Prepared,
    sc: &ScoringConfig,
    scores: &[f64],
) -> JsonCompaction {
    let mut order: Vec<(f64, usize, usize)> = prep
        .candidates
        .iter()
        .zip(scores)
        .map(|(&k, &s)| (s, prep.units[k].indices[0], k))
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

    let strict = sc.strict();
    let target = match strict {
        // Hard limit can only tighten the legacy target, never loosen it.
        Some((max, reserve, _)) => prep.target_tokens.min(max.saturating_sub(reserve)),
        None => prep.target_tokens,
    };
    let over = |content: usize, msgs: usize| {
        content > target
            || strict.is_some_and(|(max, reserve, ov)| content + ov.total(msgs) + reserve > max)
    };

    let mut removed = vec![false; prep.units.len()];
    let mut cur = prep.original_tokens;
    let mut cur_msgs = prep.message_count;
    for &(_, _, k) in &order {
        if !over(cur, cur_msgs) {
            break;
        }
        removed[k] = true;
        cur -= prep.units[k].tokens;
        cur_msgs -= prep.units[k].indices.len();
    }

    let mut kept: Vec<usize> = prep
        .units
        .iter()
        .zip(&removed)
        .filter(|(_, r)| !**r)
        .flat_map(|(u, _)| u.indices.iter().copied())
        .collect();
    kept.sort_unstable();

    let out: Vec<Value> = kept.iter().map(|&i| messages[i].clone()).collect();
    let compact: Vec<CompactMessage> = kept
        .iter()
        .map(|&i| CompactMessage {
            role: role_of(&messages[i]).to_string(),
            content: text_repr(&messages[i]),
        })
        .collect();

    JsonCompaction {
        result: CompactionResult {
            original_tokens: prep.original_tokens,
            final_tokens: cur,
            messages_pruned: messages.len() - kept.len(),
            compression_ratio: if prep.original_tokens > 0 {
                cur as f64 / prep.original_tokens as f64
            } else {
                1.0
            },
            was_compressed: true,
            messages: compact,
            kept_indices: kept,
        },
        messages: out,
    }
}

// ─── Orchestration (handler entry point) ──────────────────────────────────────

fn legacy(messages: &[Value], cfg: &CompactorConfig) -> JsonCompaction {
    compact_json_messages(&SemanticGuardedCompactor::new(), messages, cfg)
}

/// `strict` guarantee: the forwarded context fits, or the request is rejected.
fn check_strict(sc: &ScoringConfig, out: &JsonCompaction) -> Result<(), BudgetInsufficient> {
    if let Some((max, reserve, ov)) = sc.strict() {
        let overhead = ov.total(out.messages.len());
        if out.result.final_tokens + overhead + reserve > max {
            return Err(BudgetInsufficient {
                required_tokens: out.result.final_tokens,
                overhead_tokens: overhead,
                output_reserve_tokens: reserve,
                max_context_tokens: max,
            });
        }
    }
    Ok(())
}

/// Compact `messages` with opt-in scoring. Embeds only when the mode is enabled
/// AND compaction is needed AND the selection actually depends on scores.
///
/// `Err` only in `strict` budget mode (mandatory content can't fit); scoring
/// failures never fail the request, they fall back to the legacy D1 path.
pub async fn compact_with_scoring(
    messages: &[Value],
    cfg: &CompactorConfig,
    sc: &ScoringConfig,
    provider: &dyn EmbeddingProvider,
) -> Result<ScoringOutcome, BudgetInsufficient> {
    let started = Instant::now();
    let prep = prepare(messages, cfg, sc.tool_group_policy);
    let mut outcome = ScoringOutcome {
        compaction: JsonCompaction {
            result: CompactionResult {
                original_tokens: prep.original_tokens,
                final_tokens: prep.original_tokens,
                messages_pruned: 0,
                compression_ratio: 1.0,
                was_compressed: false,
                messages: Vec::new(),
                kept_indices: Vec::new(),
            },
            messages: Vec::new(),
        },
        needed: false,
        requested: sc.mode,
        effective: ScoringMode::Heuristic,
        fallback: None,
        skipped: None,
        candidates: prep.candidates.len(),
        embedded: 0,
        elapsed: Duration::ZERO,
    };

    // Not needed: identical to the legacy path (which returns the input as is).
    if sc.mode == ScoringMode::Heuristic || prep.original_tokens < cfg.token_threshold {
        outcome.compaction = legacy(messages, cfg);
        check_strict(sc, &outcome.compaction)?;
        outcome.elapsed = started.elapsed();
        return Ok(outcome);
    }
    outcome.needed = true;

    // Strict pre-check: no embedding call when the mandatory part can't fit.
    if let Some((max, reserve, ov)) = sc.strict() {
        let overhead = ov.total(prep.mandatory_messages);
        if prep.mandatory_tokens + overhead + reserve > max {
            return Err(BudgetInsufficient {
                required_tokens: prep.mandatory_tokens,
                overhead_tokens: overhead,
                output_reserve_tokens: reserve,
                max_context_tokens: max,
            });
        }
    }

    let finish_legacy = |mut o: ScoringOutcome| -> Result<ScoringOutcome, BudgetInsufficient> {
        o.compaction = legacy(messages, cfg);
        check_strict(sc, &o.compaction)?;
        o.elapsed = started.elapsed();
        Ok(o)
    };

    if prep.candidates.is_empty() {
        outcome.skipped = Some(SkipReason::NoCandidates);
        return finish_legacy(outcome);
    }
    if prep.mandatory_tokens >= prep.target_tokens && sc.strict().is_none() {
        outcome.skipped = Some(SkipReason::MandatoryExceedsTarget);
        return finish_legacy(outcome);
    }
    if prep.candidates.len() > sc.max_messages {
        outcome.fallback = Some(FallbackReason::LimitExceeded);
        return finish_legacy(outcome);
    }
    let Some(query) = prep.query.as_deref() else {
        outcome.fallback = Some(FallbackReason::NoQuery);
        return finish_legacy(outcome);
    };

    let mut texts = Vec::with_capacity(prep.candidates.len() + 1);
    texts.push(truncate_chars(query, sc.max_chars));
    texts.extend(
        prep.candidates
            .iter()
            .map(|&k| truncate_chars(&prep.units[k].text, sc.max_chars)),
    );
    outcome.embedded = texts.len();

    let vectors = match embed_validated(provider, sc, &texts).await {
        Ok(v) => v,
        Err(reason) => {
            outcome.fallback = Some(reason);
            return finish_legacy(outcome);
        }
    };
    let sims: Vec<f64> = vectors[1..]
        .iter()
        .map(|v| similarity(&vectors[0], v))
        .collect();
    let scores = combined_scores(&prep, cfg, sc, &sims);
    outcome.compaction = select_with_scores(messages, &prep, sc, &scores);
    outcome.effective = sc.mode;
    check_strict(sc, &outcome.compaction)?;
    outcome.elapsed = started.elapsed();
    Ok(outcome)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// Offline evaluation harness (test-only).
#[cfg(test)]
#[path = "semantic_scoring_eval.rs"]
mod eval;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::ScriptedEmbeddingProvider;
    use serde_json::json;
    use std::sync::atomic::Ordering::SeqCst;

    fn long(tag: &str, n: usize) -> String {
        format!("{tag} ").repeat(n)
    }

    fn cfg() -> CompactorConfig {
        CompactorConfig {
            token_threshold: 50,
            max_history_messages: 50,
            target_token_ratio: 0.5,
            min_final_tokens: 10,
            strategy: "semantic_guarded_trimming".to_string(),
            ..Default::default()
        }
    }

    fn sc(mode: ScoringMode) -> ScoringConfig {
        ScoringConfig {
            mode,
            ..Default::default()
        }
    }

    /// Topic vectors: "pods" and "kubernetes" share a topic (no lexical overlap),
    /// "recipe"/"garden" are orthogonal.
    fn topic_provider() -> ScriptedEmbeddingProvider {
        ScriptedEmbeddingProvider::new(
            vec![
                ("pods", vec![1.0, 0.0, 0.0]),
                ("kubernetes", vec![1.0, 0.0, 0.0]),
                ("recipe", vec![0.0, 1.0, 0.0]),
                ("garden", vec![0.0, 0.0, 1.0]),
            ],
            vec![0.3, 0.3, 0.3],
        )
    }

    /// Old relevant message vs recent irrelevant one, structurally neutral (no
    /// digits, routes, tech keywords, and NO lexical overlap with the question),
    /// so the heuristic can only rank by recency.
    fn relevance_conversation() -> Vec<Value> {
        vec![
            json!({"role": "system", "content": "You are helpful."}),
            json!({"role": "user", "content": long("container pods rollout", 30)}),
            json!({"role": "assistant", "content": long("recipe with tomatoes", 30)}),
            json!({"role": "user", "content": long("garden flowers", 30)}),
            json!({"role": "user", "content": "kubernetes question again"}),
        ]
    }

    fn assert_original_objects(input: &[Value], out: &JsonCompaction) {
        let kept = &out.result.kept_indices;
        assert_eq!(kept.len(), out.messages.len());
        assert!(kept.windows(2).all(|w| w[0] < w[1]), "order: {kept:?}");
        for (&i, m) in kept.iter().zip(&out.messages) {
            assert_eq!(m, &input[i], "message {i} must be forwarded verbatim");
        }
    }

    // ── config ────────────────────────────────────────────────────────────────

    #[test]
    fn config_accepts_short_and_block_forms() {
        let short: ScoringSetting = serde_yaml::from_str("hybrid").unwrap();
        assert_eq!(short.resolve().mode, ScoringMode::Hybrid);
        assert_eq!(short.resolve().semantic_weight, 0.5);
        let block: ScoringSetting = serde_yaml::from_str(
            "mode: embedding\nsemantic_weight: 0.7\nstructural_weight: 0.3\nbudget:\n  mode: strict\n  max_context_tokens: 100\n",
        )
        .unwrap();
        let c = block.resolve();
        assert_eq!(c.mode, ScoringMode::Embedding);
        assert_eq!(c.budget.mode, BudgetMode::Strict);
        assert!(c.validate().is_ok());
        assert!(serde_yaml::from_str::<ScoringSetting>("mode: hybrid\ntypo_field: 1\n").is_err());
    }

    #[test]
    fn config_validation_rejects_invalid_values() {
        let bad = |f: &dyn Fn(&mut ScoringConfig)| {
            let mut c = sc(ScoringMode::Hybrid);
            f(&mut c);
            c.validate().is_err()
        };
        assert!(bad(&|c| c.semantic_weight = f64::NAN));
        assert!(bad(&|c| c.structural_weight = f64::INFINITY));
        assert!(bad(&|c| c.semantic_weight = -0.1));
        assert!(bad(&|c| {
            c.semantic_weight = 0.0;
            c.structural_weight = 0.0
        }));
        assert!(bad(&|c| c.timeout_ms = 0));
        assert!(bad(&|c| c.max_concurrency = 0));
        assert!(bad(&|c| c.batch_size = 0));
        assert!(bad(&|c| c.max_messages = 0));
        assert!(bad(&|c| c.max_chars = 0));
        assert!(bad(&|c| c.embedding_model = " ".into()));
        assert!(bad(&|c| c.budget.mode = BudgetMode::Strict));
        assert!(bad(&|c| {
            c.budget.mode = BudgetMode::Strict;
            c.budget.max_context_tokens = Some(10);
            c.budget.output_reserve_tokens = 10
        }));
        assert!(sc(ScoringMode::Hybrid).validate().is_ok());
    }

    #[test]
    fn weights_are_normalized_and_embedding_mode_is_pure_similarity() {
        let mut c = sc(ScoringMode::Hybrid);
        c.semantic_weight = 3.0;
        c.structural_weight = 1.0;
        assert_eq!(c.effective_weights(), (0.75, 0.25));
        c.mode = ScoringMode::Embedding;
        assert_eq!(c.effective_weights(), (1.0, 0.0));
    }

    // ── math ──────────────────────────────────────────────────────────────────

    #[test]
    fn similarity_maps_cosine_to_unit_interval() {
        assert!((similarity(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-12);
        assert!((similarity(&[1.0, 0.0], &[0.0, 1.0]) - 0.5).abs() < 1e-12);
        assert!(similarity(&[1.0, 0.0], &[-1.0, 0.0]).abs() < 1e-12);
        // Norm underflow: rejected by validation before any cosine, and
        // `similarity` itself never returns NaN.
        let tiny = vec![vec![1e-300, 1e-300], vec![1.0, 1.0]];
        assert_eq!(validate_vectors(&tiny, 2), Err(FallbackReason::ZeroNorm));
        let s = similarity(&tiny[0], &tiny[1]);
        assert!((0.0..=1.0).contains(&s), "{s}");
    }

    #[test]
    fn structural_normalization() {
        assert_eq!(normalize_structural(&[10, 20, 30]), vec![0.0, 0.5, 1.0]);
        assert_eq!(normalize_structural(&[7, 7]), vec![0.5, 0.5]);
        assert!(normalize_structural(&[]).is_empty());
    }

    #[test]
    fn vector_validation_reasons() {
        let ok = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        assert!(validate_vectors(&ok, 2).is_ok());
        assert_eq!(validate_vectors(&ok, 3), Err(FallbackReason::CountMismatch));
        assert_eq!(
            validate_vectors(&[vec![1.0], vec![1.0, 2.0]], 2),
            Err(FallbackReason::InvalidDimension)
        );
        assert_eq!(
            validate_vectors(&[vec![], vec![]], 2),
            Err(FallbackReason::InvalidDimension)
        );
        assert_eq!(
            validate_vectors(&[vec![f64::NAN, 1.0], vec![1.0, 1.0]], 2),
            Err(FallbackReason::NonFinite)
        );
        assert_eq!(
            validate_vectors(&[vec![f64::INFINITY, 1.0], vec![1.0, 1.0]], 2),
            Err(FallbackReason::NonFinite)
        );
        assert_eq!(
            validate_vectors(&[vec![0.0, 0.0], vec![1.0, 1.0]], 2),
            Err(FallbackReason::ZeroNorm)
        );
    }

    // ── units ─────────────────────────────────────────────────────────────────

    fn call(ids: &[&str]) -> Value {
        json!({"role": "assistant", "content": null, "tool_calls": ids.iter().map(|id| json!({
            "id": id, "type": "function", "function": {"name": "f", "arguments": "{}"}
        })).collect::<Vec<_>>()})
    }
    fn result(id: &str) -> Value {
        json!({"role": "tool", "tool_call_id": id, "content": "r"})
    }
    fn kinds(msgs: &[Value]) -> Vec<(UnitKind, Vec<usize>)> {
        build_units(msgs, &TokenCounter::Estimated)
            .into_iter()
            .map(|u| (u.kind, u.indices))
            .collect()
    }

    #[test]
    fn complete_call_with_multiple_results_is_one_group() {
        let msgs = vec![
            json!({"role": "user", "content": "hi"}),
            call(&["a", "b"]),
            result("b"),
            result("a"),
            json!({"role": "user", "content": "next"}),
        ];
        assert_eq!(
            kinds(&msgs),
            vec![
                (UnitKind::Text, vec![0]),
                (UnitKind::ToolGroup, vec![1, 2, 3]),
                (UnitKind::Text, vec![4]),
            ]
        );
    }

    #[test]
    fn incomplete_or_ambiguous_tool_dependencies_are_pinned() {
        // Missing a result.
        let k = kinds(&[call(&["a", "b"]), result("a")]);
        assert!(k.iter().all(|(kind, _)| *kind == UnitKind::Pinned), "{k:?}");
        // Unknown result id.
        let k = kinds(&[call(&["a"]), result("zzz")]);
        assert!(k.iter().all(|(kind, _)| *kind == UnitKind::Pinned), "{k:?}");
        // Duplicate result.
        let k = kinds(&[call(&["a"]), result("a"), result("a")]);
        assert!(k.iter().all(|(kind, _)| *kind == UnitKind::Pinned), "{k:?}");
        // Duplicate call ids.
        let k = kinds(&[call(&["a", "a"]), result("a"), result("a")]);
        assert!(k.iter().all(|(kind, _)| *kind == UnitKind::Pinned), "{k:?}");
        // Result not immediately after its call.
        let k = kinds(&[
            call(&["a"]),
            json!({"role": "user", "content": "x"}),
            result("a"),
        ]);
        assert_eq!(k[0].0, UnitKind::Pinned);
        assert_eq!(k[2].0, UnitKind::Pinned, "orphan result");
        // Orphan result alone; non-text content (GAP-OAI-1 shapes, internal only).
        let k = kinds(&[
            result("x"),
            json!({"role": "user", "content": [{"type": "text", "text": "t"}]}),
            json!({"role": "assistant", "content": null}),
        ]);
        assert!(k.iter().all(|(kind, _)| *kind == UnitKind::Pinned), "{k:?}");
    }

    // ── orchestration ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn heuristic_mode_and_below_threshold_make_no_embedding_calls() {
        let input = relevance_conversation();
        let p = topic_provider();
        // Heuristic mode: legacy D1 output, zero calls.
        let o = compact_with_scoring(&input, &cfg(), &sc(ScoringMode::Heuristic), &p)
            .await
            .unwrap();
        assert!(!o.needed);
        assert_eq!(o.compaction.messages, legacy(&input, &cfg()).messages);
        // Enabled mode, but below threshold: unchanged, zero calls.
        let mut c = cfg();
        c.token_threshold = 1_000_000;
        let o = compact_with_scoring(&input, &c, &sc(ScoringMode::Hybrid), &p)
            .await
            .unwrap();
        assert!(!o.needed);
        assert!(!o.compaction.result.was_compressed);
        assert_eq!(o.compaction.messages, input);
        assert_eq!(p.total_calls(), 0);
    }

    #[tokio::test]
    async fn embedding_corrects_recency_bias_of_the_heuristic() {
        let input = relevance_conversation();
        // Heuristic: equal structural scores → oldest removed first → the old
        // but relevant message (index 1) is dropped.
        let h = legacy(&input, &cfg());
        assert!(
            !h.messages.contains(&input[1]),
            "heuristic drops by recency"
        );

        for mode in [ScoringMode::Embedding, ScoringMode::Hybrid] {
            let p = topic_provider();
            let o = compact_with_scoring(&input, &cfg(), &sc(mode), &p)
                .await
                .unwrap();
            assert_eq!(o.effective, mode);
            assert_eq!(o.fallback, None);
            assert_original_objects(&input, &o.compaction);
            assert!(
                o.compaction.messages.contains(&input[1]),
                "{mode:?}: relevant kept"
            );
            assert!(
                !o.compaction.messages.contains(&input[2]),
                "{mode:?}: irrelevant dropped"
            );
            assert!(
                o.compaction.result.final_tokens
                    <= prepare(&input, &cfg(), ToolGroupPolicy::Preserve).target_tokens
            );
            assert_eq!(p.batch_calls.load(SeqCst), 1, "one batched call");
            assert_eq!(p.embed_calls.load(SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn protected_and_pinned_messages_survive_with_all_fields() {
        let mut input = relevance_conversation();
        input[0]["x_meta"] = json!({"keep": true});
        input.insert(
            3,
            json!({"role": "user", "content": [{"type": "text", "text": "recipe image"}], "name": "n"}),
        );
        input.insert(4, call(&["a"]));
        input.insert(5, result("zzz")); // incomplete → pinned
        let mut c = cfg();
        c.stop_words = vec!["the".into(), "with".into()];
        let p = topic_provider();
        let o = compact_with_scoring(&input, &c, &sc(ScoringMode::Embedding), &p)
            .await
            .unwrap();
        assert_original_objects(&input, &o.compaction);
        for i in [0, 3, 4, 5, input.len() - 1] {
            assert!(
                o.compaction.result.kept_indices.contains(&i),
                "index {i} must be kept"
            );
        }
        // No stop-word rewrite in this mode: kept text is byte-identical.
        assert!(o.compaction.messages.iter().all(|m| input.contains(m)));
    }

    /// Relevance fixture with an irrelevant complete tool group at indices 2,3,4.
    fn conversation_with_tool_group() -> Vec<Value> {
        let mut input = relevance_conversation();
        input.insert(2, call(&["a", "b"]));
        input.insert(
            3,
            json!({"role": "tool", "tool_call_id": "a", "content": long("recipe salt", 20)}),
        );
        input.insert(
            4,
            json!({"role": "tool", "tool_call_id": "b", "content": long("recipe sugar", 20)}),
        );
        input
    }

    #[tokio::test]
    async fn complete_tool_group_is_preserved_by_default() {
        // Default policy = preserve: even an irrelevant complete group, under a
        // tight budget, is never removed.
        let input = conversation_with_tool_group();
        let mut c = cfg();
        c.target_token_ratio = 0.2;
        let p = topic_provider();
        let o = compact_with_scoring(&input, &c, &sc(ScoringMode::Embedding), &p)
            .await
            .unwrap();
        assert_eq!(
            sc(ScoringMode::Embedding).tool_group_policy,
            ToolGroupPolicy::Preserve
        );
        assert_original_objects(&input, &o.compaction);
        let kept = &o.compaction.result.kept_indices;
        for i in [2, 3, 4] {
            assert!(
                kept.contains(&i),
                "preserve: group member {i} removed: {kept:?}"
            );
        }
    }

    #[tokio::test]
    async fn complete_tool_group_is_removed_as_a_whole_when_opted_in() {
        let input = conversation_with_tool_group();
        let mut scoring = sc(ScoringMode::Embedding);
        scoring.tool_group_policy = ToolGroupPolicy::EligibleForRemoval;
        let mut removed_somewhere = false;
        for (ratio, label) in [(0.2, "tight"), (0.9, "loose")] {
            let mut c = cfg();
            c.target_token_ratio = ratio;
            let p = topic_provider();
            let o = compact_with_scoring(&input, &c, &scoring, &p)
                .await
                .unwrap();
            assert_original_objects(&input, &o.compaction);
            let kept = &o.compaction.result.kept_indices;
            let present = [2, 3, 4].iter().filter(|i| kept.contains(i)).count();
            assert!(
                present == 0 || present == 3,
                "{label}: group split: {kept:?}"
            );
            removed_somewhere |= present == 0;
        }
        assert!(
            removed_somewhere,
            "eligible_for_removal must allow the group to be dropped"
        );
    }

    #[tokio::test]
    async fn incomplete_group_stays_pinned_even_when_removal_is_opted_in() {
        // eligible_for_removal applies to COMPLETE groups only; an incomplete one
        // (missing result) is still pinned.
        let mut input = relevance_conversation();
        input.insert(2, call(&["a", "b"]));
        input.insert(
            3,
            json!({"role": "tool", "tool_call_id": "a", "content": long("recipe salt", 20)}),
        );
        let mut scoring = sc(ScoringMode::Embedding);
        scoring.tool_group_policy = ToolGroupPolicy::EligibleForRemoval;
        let mut c = cfg();
        c.target_token_ratio = 0.1;
        let p = topic_provider();
        let o = compact_with_scoring(&input, &c, &scoring, &p)
            .await
            .unwrap();
        let kept = &o.compaction.result.kept_indices;
        assert!(
            kept.contains(&2) && kept.contains(&3),
            "incomplete group must stay: {kept:?}"
        );
    }

    /// Diagnostic (and regression for the bounded fix): changing ONLY opaque
    /// tool-call ids, keeping the semantic content, must not change the selection.
    #[tokio::test]
    async fn opaque_tool_call_ids_do_not_affect_selection() {
        let base = |ids: (&str, &str)| {
            let mut input = relevance_conversation();
            input.insert(2, call(&[ids.0, ids.1]));
            input.insert(
                3,
                json!({"role": "tool", "tool_call_id": ids.0, "content": long("recipe salt", 20)}),
            );
            input.insert(
                4,
                json!({"role": "tool", "tool_call_id": ids.1, "content": long("recipe sugar", 20)}),
            );
            input
        };
        let mut scoring = sc(ScoringMode::Hybrid);
        scoring.tool_group_policy = ToolGroupPolicy::EligibleForRemoval;
        let mut c = cfg();
        c.target_token_ratio = 0.5;

        let run = |input: Vec<Value>| {
            let scoring = scoring.clone();
            let c = c.clone();
            async move {
                let p = topic_provider();
                let o = compact_with_scoring(&input, &c, &scoring, &p)
                    .await
                    .unwrap();
                o.compaction.result.kept_indices
            }
        };
        // Numeric-looking ids vs opaque alphabetic ids: same semantic content.
        let a = run(base(("call_1", "call_2"))).await;
        let b = run(base(("tooluse_xyz", "tooluse_abc"))).await;
        assert_eq!(
            a, b,
            "selection must not depend on opaque tool-call identifiers (scoring text excludes ids)"
        );
    }

    #[tokio::test]
    async fn deterministic_tie_break_removes_oldest_first_and_is_stable() {
        let input = vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "user", "content": long("alpha", 30)}),
            json!({"role": "assistant", "content": long("beta", 30)}),
            json!({"role": "user", "content": long("gamma", 30)}),
            json!({"role": "user", "content": "q"}),
        ];
        let p = ScriptedEmbeddingProvider::new(vec![], vec![1.0, 1.0]); // all equal
        let a = compact_with_scoring(&input, &cfg(), &sc(ScoringMode::Hybrid), &p)
            .await
            .unwrap();
        let b = compact_with_scoring(&input, &cfg(), &sc(ScoringMode::Hybrid), &p)
            .await
            .unwrap();
        assert_eq!(
            a.compaction.result.kept_indices,
            b.compaction.result.kept_indices
        );
        let kept = &a.compaction.result.kept_indices;
        assert!(
            !kept.contains(&1),
            "oldest tied candidate removed first: {kept:?}"
        );
    }

    async fn expect_fallback(
        p: ScriptedEmbeddingProvider,
        sc: ScoringConfig,
        reason: FallbackReason,
    ) {
        let input = relevance_conversation();
        let o = compact_with_scoring(&input, &cfg(), &sc, &p).await.unwrap();
        assert_eq!(o.fallback, Some(reason));
        assert_eq!(o.effective, ScoringMode::Heuristic);
        assert_eq!(
            o.compaction.messages,
            legacy(&input, &cfg()).messages,
            "{reason:?}: full fallback = legacy D1 output"
        );
    }

    #[tokio::test]
    async fn every_failure_falls_back_to_the_full_heuristic() {
        let mut p = topic_provider();
        p.fail = true;
        expect_fallback(p, sc(ScoringMode::Hybrid), FallbackReason::ProviderError).await;

        let mut p = topic_provider();
        p.delay = Some(Duration::from_millis(200));
        let mut c = sc(ScoringMode::Hybrid);
        c.timeout_ms = 20;
        expect_fallback(p, c, FallbackReason::Timeout).await;

        let mut p = topic_provider();
        p.override_output = Some(vec![vec![1.0, 0.0]]);
        expect_fallback(p, sc(ScoringMode::Hybrid), FallbackReason::CountMismatch).await;

        let mut p = topic_provider();
        p.override_output = Some(vec![vec![1.0], vec![1.0, 0.0], vec![1.0], vec![1.0]]);
        expect_fallback(p, sc(ScoringMode::Hybrid), FallbackReason::InvalidDimension).await;

        let mut p = topic_provider();
        p.override_output = Some(vec![vec![f64::NAN, 1.0]; 4]);
        expect_fallback(p, sc(ScoringMode::Hybrid), FallbackReason::NonFinite).await;

        let mut p = topic_provider();
        p.override_output = Some(vec![vec![0.0, 0.0]; 4]);
        expect_fallback(p, sc(ScoringMode::Hybrid), FallbackReason::ZeroNorm).await;
    }

    #[tokio::test]
    async fn too_many_candidates_fall_back_without_any_embedding_call() {
        let p = topic_provider();
        let mut c = sc(ScoringMode::Embedding);
        c.max_messages = 2; // 3 candidates in the fixture
        let input = relevance_conversation();
        let o = compact_with_scoring(&input, &cfg(), &c, &p).await.unwrap();
        assert_eq!(o.fallback, Some(FallbackReason::LimitExceeded));
        assert_eq!(p.total_calls(), 0, "no partial (prefix/suffix) embedding");
        assert_eq!(o.compaction.messages, legacy(&input, &cfg()).messages);
    }

    #[tokio::test]
    async fn no_user_message_falls_back() {
        let input = vec![
            json!({"role": "system", "content": "s"}),
            json!({"role": "assistant", "content": long("a", 100)}),
            json!({"role": "assistant", "content": long("b", 100)}),
        ];
        let p = topic_provider();
        let o = compact_with_scoring(&input, &cfg(), &sc(ScoringMode::Hybrid), &p)
            .await
            .unwrap();
        assert_eq!(o.fallback, Some(FallbackReason::NoQuery));
        assert_eq!(p.total_calls(), 0);
    }

    #[tokio::test]
    async fn skips_embedding_when_selection_cannot_depend_on_scores() {
        let p = topic_provider();
        // Only protected messages → no candidates.
        let input = vec![
            json!({"role": "system", "content": long("s", 60)}),
            json!({"role": "user", "content": long("kubernetes", 60)}),
        ];
        let o = compact_with_scoring(&input, &cfg(), &sc(ScoringMode::Hybrid), &p)
            .await
            .unwrap();
        assert_eq!(o.skipped, Some(SkipReason::NoCandidates));
        // Mandatory content already above the target → candidates all go anyway.
        let input = vec![
            json!({"role": "system", "content": long("s", 200)}),
            json!({"role": "assistant", "content": long("x", 10)}),
            json!({"role": "user", "content": "q"}),
        ];
        let o = compact_with_scoring(&input, &cfg(), &sc(ScoringMode::Hybrid), &p)
            .await
            .unwrap();
        assert_eq!(o.skipped, Some(SkipReason::MandatoryExceedsTarget));
        assert_eq!(p.total_calls(), 0);
    }

    #[tokio::test]
    async fn batch_size_concurrency_and_truncation_limits_are_applied() {
        let mut input = vec![json!({"role": "system", "content": "s"})];
        for i in 0..6 {
            input.push(
                json!({"role": "assistant", "content": format!("{} {i}", long("garden", 300))}),
            );
        }
        input.push(json!({"role": "user", "content": "kubernetes?"}));
        let mut p = topic_provider();
        p.delay = Some(Duration::from_millis(30));
        let mut c = sc(ScoringMode::Embedding);
        c.batch_size = 2;
        c.max_concurrency = 2;
        c.max_chars = 50;
        let o = compact_with_scoring(&input, &cfg(), &c, &p).await.unwrap();
        assert_eq!(o.fallback, None);
        assert_eq!(o.embedded, 7, "query + 6 candidates");
        assert_eq!(p.last_batch_size.load(SeqCst), 2);
        assert_eq!(p.chunks.load(SeqCst), 4, "ceil(7 / 2)");
        assert!(p.max_in_flight.load(SeqCst) <= 2);
        assert!(p
            .texts_seen
            .lock()
            .unwrap()
            .iter()
            .all(|t| t.chars().count() <= 50));
    }

    #[tokio::test]
    async fn strict_budget_rejects_when_mandatory_content_cannot_fit() {
        let input = relevance_conversation();
        let p = topic_provider();
        let mut c = sc(ScoringMode::Hybrid);
        c.budget = BudgetConfig {
            mode: BudgetMode::Strict,
            max_context_tokens: Some(20),
            output_reserve_tokens: 5,
            formatting_overhead: FormattingOverhead::OpenaiChatEstimate,
        };
        let err = compact_with_scoring(&input, &cfg(), &c, &p)
            .await
            .unwrap_err();
        assert_eq!(err.max_context_tokens, 20);
        assert_eq!(err.output_reserve_tokens, 5);
        assert!(err.message().contains("Insufficient context budget"));
        assert_eq!(p.total_calls(), 0, "no embedding when it can't fit anyway");
    }

    #[tokio::test]
    async fn strict_budget_fits_content_plus_overhead_plus_reserve() {
        let input = relevance_conversation();
        let p = topic_provider();
        let mut c = sc(ScoringMode::Hybrid);
        c.budget = BudgetConfig {
            mode: BudgetMode::Strict,
            max_context_tokens: Some(120),
            output_reserve_tokens: 30,
            formatting_overhead: FormattingOverhead::OpenaiChatEstimate,
        };
        let o = compact_with_scoring(&input, &cfg(), &c, &p).await.unwrap();
        let r = &o.compaction.result;
        let ov = FormattingOverhead::OpenaiChatEstimate.total(o.compaction.messages.len());
        assert!(
            r.final_tokens + ov + 30 <= 120,
            "{} + {ov} + 30",
            r.final_tokens
        );
        assert_original_objects(&input, &o.compaction);
    }

    /// G1 (real service): Ollama at `MELIS_OLLAMA_URL` (default
    /// http://localhost:11434) with `MELIS_EMBED_MODEL` (default nomic-embed-text).
    #[tokio::test]
    #[ignore = "requires a live Ollama with an embedding model; run in pre-merge env (G1)"]
    async fn g1_real_ollama_scoring_runs_without_fallback() {
        let url = std::env::var("MELIS_OLLAMA_URL").unwrap_or("http://localhost:11434".into());
        let model = std::env::var("MELIS_EMBED_MODEL").unwrap_or("nomic-embed-text".into());
        let cfg_yaml = format!(
            "server:\n  host: \"0.0.0.0\"\n  port: 8080\nredis:\n  cluster_urls: [\"redis://localhost:6379\"]\nproviders:\n  - id: ollama\n    provider_type: ollama\n    api_key: ollama\n    base_url: \"{url}\"\n"
        );
        let gw = crate::config::GatewayConfig::from_yaml(&cfg_yaml).unwrap();
        let provider = crate::embeddings::OllamaEmbeddingProvider::new(
            gw.providers,
            std::sync::Arc::new(crate::client::ReqwestLlmClient::new()),
        );
        let mut c = sc(ScoringMode::Hybrid);
        c.embedding_model = model;
        c.timeout_ms = 30_000; // cold model load
        c.batch_size = 2;
        let input = relevance_conversation();
        let o = compact_with_scoring(&input, &cfg(), &c, &provider)
            .await
            .unwrap();
        assert_eq!(o.fallback, None, "real embeddings must validate");
        assert_eq!(o.effective, ScoringMode::Hybrid);
        assert_original_objects(&input, &o.compaction);
        eprintln!(
            "G1 scoring: embedded={} pruned={} elapsed_ms={}",
            o.embedded,
            o.compaction.result.messages_pruned,
            o.elapsed.as_millis()
        );
    }

    #[tokio::test]
    async fn legacy_budget_mode_keeps_legacy_target_semantics() {
        // Same config, legacy budget: target = max(original × ratio, min_final),
        // content tokens only (no overhead).
        let input = relevance_conversation();
        let p = topic_provider();
        let o = compact_with_scoring(&input, &cfg(), &sc(ScoringMode::Hybrid), &p)
            .await
            .unwrap();
        let prep = prepare(&input, &cfg(), ToolGroupPolicy::Preserve);
        assert_eq!(
            o.compaction.result.original_tokens,
            legacy(&input, &cfg()).result.original_tokens
        );
        assert!(o.compaction.result.final_tokens <= prep.target_tokens);
    }
}
