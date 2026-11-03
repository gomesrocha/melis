# Changelog

All notable changes to the Melis AI Gateway are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Phase 1 — Real BPE Token Counting

#### Added

- **Opt-in real BPE token counting.** New `compactor.token_counting` setting in
  `config.yaml` with values `estimated` (default) and `real`. In `real` mode the
  compactor counts tokens with a real tiktoken BPE encoding selected by
  `compactor.tokenizer_name` (supported: `cl100k_base`, `o200k_base`).
- **`tiktoken-rs`** dependency for real token counting. Encodings are cached as
  process-wide singletons, so no vocabulary is loaded per request.
- **`melis_token_counting_mode`** Prometheus counter with label `mode`
  (`real` | `estimated`). Both series are present from boot so dashboards can tell
  which base the token numbers were computed on.
- **Synthetic `usage` fill.** In `real` mode, when a provider omits `usage` (or
  returns zeros), the gateway fills `usage.prompt_tokens` / `completion_tokens` /
  `total_tokens` with real counts (prompt from the request messages, completion
  from the response content). Provider-supplied non-zero values are preserved.
- **Startup discoverability log.** If `compactor.tokenizer_name` is explicitly set
  while `token_counting` is still `estimated`, Melis logs a one-line `info`
  suggesting `token_counting: real`.
- Documentation: `config.yaml.example`, README "Token Counting" section and metrics
  reference.

#### Changed

- **`src/compactor.rs`** now routes all counting through a `TokenCounter`
  (`SimpleCompactor::count_tokens_with` / `total_tokens_with`). The legacy
  `count_tokens` / `total_tokens` remain as wrappers delegating to the estimated
  strategy, preserving behavior bit-for-bit.

#### Removed

- Unused `tokenizers` (HuggingFace) dependency, which was declared but never used.
  Replaced by `tiktoken-rs`, which matches the project's OpenAI-compatible
  encodings and avoids runtime file/network access.

#### Notes / Migration

- **Backward compatible by default.** Configs without `token_counting` continue to
  use the `estimated` approximation (`len / 4`), identical to previous behavior.
  `tokenizer_name` alone does **not** enable real counting.
- **Behavior change when opting in:** switching to `token_counting: real` changes
  the token numbers that drive compaction thresholds (`compress_above_tokens`) and
  the `usage` / token metrics. Real counts differ from the approximation, so
  compaction may trigger at different points. This is intentional and documented.
