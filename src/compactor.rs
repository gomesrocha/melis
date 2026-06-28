//! Context Compactor module for adaptive compression of message history.
//!
//! Implements token-aware pruning of conversation messages to reduce
//! costs and improve efficiency when forwarding to LLM providers.
//!
//! Uses a char-count approximation (4 chars ≈ 1 token) by default,
//! with optional HuggingFace Tokenizer integration for precise counting.

use crate::config::CompactorConfig;

/// A message with owned data, suitable for mutation during compaction.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactMessage {
    pub role: String,
    pub content: String,
}

/// Result of a compaction operation.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    /// Token count of the original message history.
    pub original_tokens: usize,
    /// Token count after compaction.
    pub final_tokens: usize,
    /// Number of messages pruned (removed entirely).
    pub messages_pruned: usize,
    /// Compression ratio (final_tokens / original_tokens). 1.0 = no compression.
    pub compression_ratio: f64,
    /// Whether any compaction was applied.
    pub was_compressed: bool,
    /// The resulting messages after compaction.
    pub messages: Vec<CompactMessage>,
}

/// Trait for context compaction implementations.
pub trait ContextCompactor: Send + Sync {
    /// Compact the given messages according to the provided configuration.
    ///
    /// Returns a `CompactionResult` containing the (possibly modified) messages
    /// along with compression metrics.
    fn compact(&self, messages: Vec<CompactMessage>, config: &CompactorConfig) -> CompactionResult;
}

/// Simple compactor that uses character-count approximation for token counting.
///
/// Approximation: 4 characters ≈ 1 token. This avoids the heavy dependency
/// on HuggingFace tokenizer models for fast compilation and testing.
pub struct SimpleCompactor;

impl SimpleCompactor {
    pub fn new() -> Self {
        Self
    }

    /// Approximate token count for a string (4 chars ≈ 1 token).
    pub fn count_tokens(text: &str) -> usize {
        // Minimum 1 token for non-empty text
        let chars = text.len();
        if chars == 0 {
            0
        } else {
            (chars + 3) / 4 // ceiling division
        }
    }

    /// Count total tokens across all messages (role + content).
    pub fn total_tokens(messages: &[CompactMessage]) -> usize {
        messages
            .iter()
            .map(|m| Self::count_tokens(&m.role) + Self::count_tokens(&m.content))
            .sum()
    }

    /// Remove stop-words from a string, preserving other content.
    fn remove_stop_words(text: &str, stop_words: &[String]) -> String {
        if stop_words.is_empty() {
            return text.to_string();
        }

        let words: Vec<&str> = text.split_whitespace().collect();
        let filtered: Vec<&str> = words
            .into_iter()
            .filter(|word| {
                let lower = word.to_lowercase();
                // Strip punctuation for comparison
                let clean: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
                !stop_words.iter().any(|sw| sw.to_lowercase() == clean)
            })
            .collect();

        filtered.join(" ")
    }
}

impl Default for SimpleCompactor {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextCompactor for SimpleCompactor {
    fn compact(&self, messages: Vec<CompactMessage>, config: &CompactorConfig) -> CompactionResult {
        let original_tokens = Self::total_tokens(&messages);
        let original_count = messages.len();

        // Step 1: If tokens < threshold AND message count <= max_history_messages → no compression
        if original_tokens < config.token_threshold
            && original_count <= config.max_history_messages
        {
            return CompactionResult {
                original_tokens,
                final_tokens: original_tokens,
                messages_pruned: 0,
                compression_ratio: 1.0,
                was_compressed: false,
                messages,
            };
        }

        // Step 2: Apply max_history_messages sliding window
        // Keep all system messages + last N non-system messages
        let non_system_messages: Vec<(usize, &CompactMessage)> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role != "system")
            .collect();

        let windowed_messages: Vec<CompactMessage> = if non_system_messages.len() > config.max_history_messages {
            // Keep all system messages
            let system_messages: Vec<CompactMessage> = messages
                .iter()
                .filter(|m| m.role == "system")
                .cloned()
                .collect();

            // Keep last N non-system messages
            let keep_non_system: Vec<CompactMessage> = non_system_messages
                .iter()
                .rev()
                .take(config.max_history_messages)
                .rev()
                .map(|(_, m)| (*m).clone())
                .collect();

            // Reconstruct: system messages first (in order), then kept non-system messages
            // Actually, preserve original interleaving order for system messages
            // by keeping all system messages at their relative positions among kept messages.
            // Simpler approach: system messages come first, then the windowed non-system messages.
            // But this breaks order. Better: iterate original order, keep system + kept non-system.
            let kept_non_system_start = non_system_messages.len() - config.max_history_messages;
            let kept_indices: std::collections::HashSet<usize> = non_system_messages
                .iter()
                .skip(kept_non_system_start)
                .map(|(i, _)| *i)
                .collect();

            messages
                .iter()
                .enumerate()
                .filter(|(i, m)| m.role == "system" || kept_indices.contains(i))
                .map(|(_, m)| m.clone())
                .collect()
        } else {
            messages.clone()
        };

        let messages_pruned_by_window = original_count - windowed_messages.len();

        // Step 3: If after windowing, tokens are below threshold → done
        let tokens_after_window = Self::total_tokens(&windowed_messages);
        if tokens_after_window < config.token_threshold {
            // SAFETY: never return empty messages array
            if windowed_messages.is_empty() {
                return CompactionResult {
                    original_tokens,
                    final_tokens: original_tokens,
                    messages_pruned: 0,
                    compression_ratio: 1.0,
                    was_compressed: false,
                    messages,
                };
            }

            let compression_ratio = if original_tokens > 0 {
                tokens_after_window as f64 / original_tokens as f64
            } else {
                1.0
            };
            return CompactionResult {
                original_tokens,
                final_tokens: tokens_after_window,
                messages_pruned: messages_pruned_by_window,
                compression_ratio,
                was_compressed: messages_pruned_by_window > 0,
                messages: windowed_messages,
            };
        }

        // Step 4: Still above threshold → apply token-based pruning on eligible messages
        // Find the index of the last user message in the windowed set
        let last_user_idx = windowed_messages
            .iter()
            .rposition(|m| m.role == "user");

        // Identify eligible messages (not system, not last user)
        let eligible_indices: Vec<usize> = windowed_messages
            .iter()
            .enumerate()
            .filter(|(i, m)| {
                m.role != "system" && Some(*i) != last_user_idx
            })
            .map(|(i, _)| i)
            .collect();

        // Remove stop-words from eligible message contents
        let working_messages: Vec<CompactMessage> = windowed_messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if eligible_indices.contains(&i) && !config.stop_words.is_empty() {
                    CompactMessage {
                        role: m.role.clone(),
                        content: Self::remove_stop_words(&m.content, &config.stop_words),
                    }
                } else {
                    m.clone()
                }
            })
            .collect();

        // Calculate target: 25% reduction means final should be <= 75% of original
        let target_tokens = (original_tokens as f64 * 0.75) as usize;

        // Check if stop-word removal alone achieved the target
        let tokens_after_stopwords = Self::total_tokens(&working_messages);
        if tokens_after_stopwords <= target_tokens {
            // SAFETY: never return empty messages array
            if working_messages.is_empty() {
                return CompactionResult {
                    original_tokens,
                    final_tokens: original_tokens,
                    messages_pruned: 0,
                    compression_ratio: 1.0,
                    was_compressed: false,
                    messages,
                };
            }

            let compression_ratio = if original_tokens > 0 {
                tokens_after_stopwords as f64 / original_tokens as f64
            } else {
                1.0
            };
            return CompactionResult {
                original_tokens,
                final_tokens: tokens_after_stopwords,
                messages_pruned: messages_pruned_by_window,
                compression_ratio,
                was_compressed: true,
                messages: working_messages,
            };
        }

        // Prune oldest eligible messages until target is reached
        let mut token_messages_pruned = 0;
        let mut indices_to_remove: Vec<usize> = Vec::new();

        for &idx in &eligible_indices {
            indices_to_remove.push(idx);
            token_messages_pruned += 1;

            // Calculate token count without the removed messages
            let remaining_tokens: usize = working_messages
                .iter()
                .enumerate()
                .filter(|(i, _)| !indices_to_remove.contains(i))
                .map(|(_, m)| Self::count_tokens(&m.role) + Self::count_tokens(&m.content))
                .sum();

            if remaining_tokens <= target_tokens {
                break;
            }
        }

        // Build final message list preserving order
        let final_messages: Vec<CompactMessage> = working_messages
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !indices_to_remove.contains(i))
            .map(|(_, m)| m)
            .collect();

        // SAFETY: never return an empty message array — always keep at least the last message
        let final_messages = if final_messages.is_empty() {
            if let Some(last) = messages.last() {
                vec![last.clone()]
            } else {
                return CompactionResult {
                    original_tokens,
                    final_tokens: original_tokens,
                    messages_pruned: 0,
                    compression_ratio: 1.0,
                    was_compressed: false,
                    messages,
                };
            }
        } else {
            final_messages
        };

        let final_tokens = Self::total_tokens(&final_messages);
        let compression_ratio = if original_tokens > 0 {
            final_tokens as f64 / original_tokens as f64
        } else {
            1.0
        };

        CompactionResult {
            original_tokens,
            final_tokens,
            messages_pruned: messages_pruned_by_window + token_messages_pruned,
            compression_ratio,
            was_compressed: true,
            messages: final_messages,
        }
    }
}

// ─── Semantic Guarded Trimming Compactor ──────────────────────────────────────

/// Semantic Guarded Trimming compactor.
///
/// Uses importance scoring to preserve semantically critical messages
/// while removing low-value messages to meet token budget.
pub struct SemanticGuardedCompactor;

/// Importance score for a message.
#[derive(Debug, Clone)]
struct ScoredMessage {
    index: usize,
    message: CompactMessage,
    score: i32,
    is_protected: bool,
    token_count: usize,
}

/// Simple check for date-like patterns (YYYY-MM-DD or DD/MM/YYYY).
fn regex_lite_date_check(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 10 {
        return false;
    }
    for window in chars.windows(10) {
        // Check YYYY-MM-DD pattern
        if window[4] == '-' && window[7] == '-'
            && window[0].is_ascii_digit()
            && window[1].is_ascii_digit()
            && window[2].is_ascii_digit()
            && window[3].is_ascii_digit()
            && window[5].is_ascii_digit()
            && window[6].is_ascii_digit()
            && window[8].is_ascii_digit()
            && window[9].is_ascii_digit()
        {
            return true;
        }
        // Check DD/MM/YYYY pattern
        if window[2] == '/' && window[5] == '/'
            && window[0].is_ascii_digit()
            && window[1].is_ascii_digit()
            && window[3].is_ascii_digit()
            && window[4].is_ascii_digit()
            && window[6].is_ascii_digit()
            && window[7].is_ascii_digit()
            && window[8].is_ascii_digit()
            && window[9].is_ascii_digit()
        {
            return true;
        }
    }
    false
}

impl SemanticGuardedCompactor {
    pub fn new() -> Self {
        Self
    }

    /// Calculate importance score for a message.
    fn calculate_score(
        msg: &CompactMessage,
        is_last_user: bool,
        last_user_terms: &[String],
        critical_markers: &[String],
    ) -> i32 {
        let mut score: i32 = 0;
        let content_lower = msg.content.to_lowercase();

        // Role-based scoring
        if msg.role == "system" {
            score += 100;
        }
        if is_last_user {
            score += 100;
        }

        // Critical markers
        for marker in critical_markers {
            if content_lower.contains(&marker.to_lowercase()) {
                score += 80;
            }
        }

        // Technical content indicators (numbers, ports, percentages, dates, routes)
        let has_numbers = content_lower.chars().any(|c| c.is_ascii_digit());
        let has_routes = content_lower.contains("/v1/")
            || content_lower.contains("http://")
            || content_lower.contains("https://");
        let has_percentages = content_lower.contains('%');
        let has_dates = regex_lite_date_check(&content_lower);

        if has_numbers || has_percentages {
            score += 30;
        }
        if has_routes {
            score += 30;
        }
        if has_dates {
            score += 20;
        }

        // Terms from the last user question (semantic relevance)
        let terms_found = last_user_terms
            .iter()
            .filter(|term| content_lower.contains(&term.to_lowercase()))
            .count();
        score += (terms_found as i32) * 20;

        // Technical keywords
        let tech_keywords = [
            "api", "endpoint", "token", "error", "config", "deploy",
            "database", "server", "port", "latency", "timeout", "cluster",
        ];
        let tech_count = tech_keywords
            .iter()
            .filter(|kw| content_lower.contains(*kw))
            .count();
        score += (tech_count as i32) * 15;

        // Negative signals (low-value content)
        if content_lower.contains("contexto auxiliar") {
            score -= 30;
        }
        if content_lower.contains("serve apenas para aumentar") {
            score -= 20;
        }
        // Short assistant responses like "Entendido", "Ok", "Certo"
        if msg.role == "assistant" && msg.content.len() < 30 {
            score -= 20;
        }

        score
    }

    /// Extract meaningful terms from the last user message for relevance scoring.
    fn extract_query_terms(content: &str) -> Vec<String> {
        let stop_words = [
            "o", "a", "de", "do", "da", "em", "no", "na", "que", "é",
            "the", "is", "of", "in", "to", "and", "for", "it", "on",
            "um", "uma", "com", "por", "para", "se", "como", "mais",
        ];

        content
            .split_whitespace()
            .map(|w| {
                w.to_lowercase()
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
            })
            .filter(|w| w.len() > 3 && !stop_words.contains(&w.as_str()))
            .collect()
    }
}

impl Default for SemanticGuardedCompactor {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextCompactor for SemanticGuardedCompactor {
    fn compact(&self, messages: Vec<CompactMessage>, config: &CompactorConfig) -> CompactionResult {
        let original_tokens = SimpleCompactor::total_tokens(&messages);
        let original_count = messages.len();

        // Step 1: If below threshold, return unchanged
        if original_tokens < config.token_threshold {
            return CompactionResult {
                original_tokens,
                final_tokens: original_tokens,
                messages_pruned: 0,
                compression_ratio: 1.0,
                was_compressed: false,
                messages,
            };
        }

        // Step 2: Calculate target budget
        let target_tokens = ((original_tokens as f64) * config.target_token_ratio) as usize;
        let target_tokens = target_tokens.max(config.min_final_tokens);

        // Step 3: Find last user message and extract query terms
        let last_user_idx = messages.iter().rposition(|m| m.role == "user");
        let last_user_terms = last_user_idx
            .map(|idx| Self::extract_query_terms(&messages[idx].content))
            .unwrap_or_default();

        // Step 4: Score all messages
        let scored: Vec<ScoredMessage> = messages
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                let is_last_user = Some(i) == last_user_idx;
                let score = Self::calculate_score(
                    msg,
                    is_last_user,
                    &last_user_terms,
                    &config.critical_markers,
                );
                let token_count =
                    SimpleCompactor::count_tokens(&msg.role) + SimpleCompactor::count_tokens(&msg.content);
                let is_protected = msg.role == "system"
                    || is_last_user
                    || (config.preserve_critical_markers
                        && config.critical_markers.iter().any(|m| {
                            msg.content.to_lowercase().contains(&m.to_lowercase())
                        }));

                ScoredMessage {
                    index: i,
                    message: msg.clone(),
                    score,
                    is_protected,
                    token_count,
                }
            })
            .collect();

        // Step 5: Calculate protected tokens
        let protected_tokens: usize = scored
            .iter()
            .filter(|s| s.is_protected)
            .map(|s| s.token_count)
            .sum();

        // If protected alone exceeds budget, just keep protected messages
        if protected_tokens >= target_tokens {
            let final_messages: Vec<CompactMessage> = scored
                .iter()
                .filter(|s| s.is_protected)
                .map(|s| s.message.clone())
                .collect();
            let final_tokens = SimpleCompactor::total_tokens(&final_messages);
            return CompactionResult {
                original_tokens,
                final_tokens,
                messages_pruned: original_count - final_messages.len(),
                compression_ratio: if original_tokens > 0 {
                    final_tokens as f64 / original_tokens as f64
                } else {
                    1.0
                },
                was_compressed: true,
                messages: final_messages,
            };
        }

        // Step 6: Sort non-protected by score (ascending) to remove lowest first
        let mut removable: Vec<&ScoredMessage> = scored.iter().filter(|s| !s.is_protected).collect();
        removable.sort_by_key(|s| s.score);

        // Step 7: Remove lowest-scoring messages until within budget
        let mut removed_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        let mut current_tokens = original_tokens;

        for sm in &removable {
            if current_tokens <= target_tokens {
                break;
            }
            removed_indices.insert(sm.index);
            current_tokens -= sm.token_count;
        }

        // Step 8: Build final message list preserving original order
        let final_messages: Vec<CompactMessage> = scored
            .iter()
            .filter(|s| !removed_indices.contains(&s.index))
            .map(|s| s.message.clone())
            .collect();

        let final_tokens = SimpleCompactor::total_tokens(&final_messages);
        let messages_pruned = original_count - final_messages.len();

        CompactionResult {
            original_tokens,
            final_tokens,
            messages_pruned,
            compression_ratio: if original_tokens > 0 {
                final_tokens as f64 / original_tokens as f64
            } else {
                1.0
            },
            was_compressed: true,
            messages: final_messages,
        }
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// **Validates: Requirements 3.1, 3.4**
    ///
    /// Property 5: Context Compactor Preserva Mensagens Invariantes e Poda Corretamente
    /// For any message history with a mix of system, user, assistant roles:
    /// - all system messages preserved unchanged
    /// - last user message preserved unchanged
    /// - order maintained
    mod property_compactor_invariants {
        use super::*;

        /// Strategy to generate a random message with a given role.
        fn message_strategy() -> impl Strategy<Value = CompactMessage> {
            let role_strategy = prop_oneof![
                2 => Just("user".to_string()),
                2 => Just("assistant".to_string()),
                1 => Just("system".to_string()),
            ];
            // Generate content long enough to contribute meaningful tokens
            let content_strategy = "[a-zA-Z ]{10,80}";

            (role_strategy, content_strategy).prop_map(|(role, content)| CompactMessage {
                role,
                content,
            })
        }

        /// Strategy to generate a message history that will exceed a low threshold.
        fn message_history_strategy() -> impl Strategy<Value = Vec<CompactMessage>> {
            proptest::collection::vec(message_strategy(), 3..12)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn system_messages_preserved_unchanged(
                messages in message_history_strategy(),
            ) {
                let compactor = SimpleCompactor::new();
                // Use a very low threshold to force compression
                let config = CompactorConfig {
                    token_threshold: 1,
                    max_history_messages: 20,
                    stop_words: vec![],
                    tokenizer_name: "cl100k_base".to_string(),
                    ..Default::default()
                };

                let original_system_messages: Vec<&CompactMessage> = messages
                    .iter()
                    .filter(|m| m.role == "system")
                    .collect();

                let result = compactor.compact(messages.clone(), &config);

                let result_system_messages: Vec<&CompactMessage> = result
                    .messages
                    .iter()
                    .filter(|m| m.role == "system")
                    .collect();

                // All original system messages must be in the result
                prop_assert_eq!(
                    original_system_messages.len(),
                    result_system_messages.len(),
                    "System message count changed: expected {}, got {}",
                    original_system_messages.len(),
                    result_system_messages.len()
                );

                for (orig, res) in original_system_messages.iter().zip(result_system_messages.iter()) {
                    prop_assert_eq!(
                        &orig.content, &res.content,
                        "System message content changed from '{}' to '{}'",
                        orig.content, res.content
                    );
                }
            }

            #[test]
            fn last_user_message_preserved_unchanged(
                messages in message_history_strategy(),
            ) {
                let compactor = SimpleCompactor::new();
                let config = CompactorConfig {
                    token_threshold: 1,
                    max_history_messages: 20,
                    stop_words: vec![],
                    tokenizer_name: "cl100k_base".to_string(),
                    ..Default::default()
                };

                let last_user = messages.iter().rposition(|m| m.role == "user");

                let result = compactor.compact(messages.clone(), &config);

                if let Some(last_user_idx) = last_user {
                    let original_last_user = &messages[last_user_idx];
                    // The last user message must appear in the result with same content
                    let result_has_last_user = result.messages.iter().any(|m| {
                        m.role == "user" && m.content == original_last_user.content
                    });
                    prop_assert!(
                        result_has_last_user,
                        "Last user message '{}' not found in result",
                        original_last_user.content
                    );
                }
            }

            #[test]
            fn message_order_maintained(
                messages in message_history_strategy(),
            ) {
                let compactor = SimpleCompactor::new();
                let config = CompactorConfig {
                    token_threshold: 1,
                    max_history_messages: 20,
                    stop_words: vec![],
                    tokenizer_name: "cl100k_base".to_string(),
                    ..Default::default()
                };

                let result = compactor.compact(messages.clone(), &config);

                // Every message in the result should appear in the original
                // in the same relative order.
                let mut last_found_idx: Option<usize> = None;
                for result_msg in &result.messages {
                    // Find this message in the original list (by role+content match
                    // after the last found position)
                    let search_start = last_found_idx.map(|i| i + 1).unwrap_or(0);
                    let found = messages[search_start..].iter().position(|orig| {
                        orig.role == result_msg.role
                            && (orig.content == result_msg.content
                                || result_msg.role == "system" && orig.content == result_msg.content)
                    });

                    prop_assert!(
                        found.is_some(),
                        "Result message (role='{}', content='{}') not found in original after index {}",
                        result_msg.role, result_msg.content, search_start
                    );
                    last_found_idx = Some(search_start + found.unwrap());
                }
            }
        }
    }

    /// **Validates: Requirements 3.3**
    ///
    /// Property 6: Context Compactor Atinge Redução Mínima de 25%
    /// For any large message history that exceeds the threshold and has sufficient
    /// eligible messages, compression_ratio <= 0.75.
    mod property_compactor_25_percent_reduction {
        use super::*;

        /// Strategy to generate a large message that contributes many tokens.
        fn large_message_strategy() -> impl Strategy<Value = CompactMessage> {
            let role_strategy = prop_oneof![
                3 => Just("assistant".to_string()),
                3 => Just("user".to_string()),
                1 => Just("system".to_string()),
            ];
            // Generate content that is 100-300 chars (25-75 tokens each)
            let content_strategy = "[a-zA-Z ]{100,300}";

            (role_strategy, content_strategy).prop_map(|(role, content)| CompactMessage {
                role,
                content,
            })
        }

        /// Strategy to generate a large history guaranteed to have many eligible messages.
        /// We ensure at least 8 non-system messages so there's enough to prune for 25%.
        fn large_history_strategy() -> impl Strategy<Value = Vec<CompactMessage>> {
            // Generate 1 system + 8-15 assistant/user messages + 1 final user
            let eligible_msgs = proptest::collection::vec(
                "[a-zA-Z ]{100,300}".prop_map(|content| CompactMessage {
                    role: "assistant".to_string(),
                    content,
                }),
                8..15,
            );
            eligible_msgs.prop_map(|mut msgs| {
                // Prepend a system message
                msgs.insert(0, CompactMessage {
                    role: "system".to_string(),
                    content: "System prompt.".to_string(),
                });
                // Append a final user message
                msgs.push(CompactMessage {
                    role: "user".to_string(),
                    content: "Final user question".to_string(),
                });
                msgs
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn achieves_25_percent_reduction_when_enough_eligible(
                messages in large_history_strategy(),
            ) {
                let compactor = SimpleCompactor::new();
                let config = CompactorConfig {
                    token_threshold: 1, // Always trigger compression
                    max_history_messages: 100, // High limit so window doesn't interfere
                    stop_words: vec![],
                    tokenizer_name: "cl100k_base".to_string(),
                    ..Default::default()
                };

                let original_tokens = SimpleCompactor::total_tokens(&messages);

                // Calculate eligible tokens (not system, not last user)
                let last_user_idx = messages.iter().rposition(|m| m.role == "user");
                let eligible_tokens: usize = messages.iter().enumerate()
                    .filter(|(i, m)| m.role != "system" && Some(*i) != last_user_idx)
                    .map(|(_, m)| SimpleCompactor::count_tokens(&m.role) + SimpleCompactor::count_tokens(&m.content))
                    .sum();

                let result = compactor.compact(messages, &config);

                // Only assert 25% reduction when eligible tokens are sufficient
                // (eligible_tokens must be > 25% of original to make target achievable)
                if eligible_tokens > original_tokens / 4 {
                    prop_assert!(
                        result.compression_ratio <= 0.76, // small tolerance for rounding
                        "Expected compression_ratio <= 0.76, got {} (original={}, final={}, eligible={})",
                        result.compression_ratio, original_tokens, result.final_tokens, eligible_tokens
                    );
                }

                // Always verify compression was applied
                prop_assert!(result.was_compressed);
            }
        }
    }

    /// **Validates: Requirements 3.2**
    ///
    /// Property 7: Context Compactor Remove Stop-Words
    /// For any messages with known stop-words inserted, after compression
    /// no eligible message contains any of the configured stop-words.
    mod property_compactor_stop_words {
        use super::*;

        /// Known stop-words that will be used in the test.
        fn stop_word_list() -> Vec<String> {
            vec![
                "the".to_string(),
                "is".to_string(),
                "a".to_string(),
                "an".to_string(),
                "of".to_string(),
            ]
        }

        /// Strategy to generate content that includes known stop-words.
        fn content_with_stop_words() -> impl Strategy<Value = String> {
            // Generate base words then intersperse stop-words
            proptest::collection::vec("[a-z]{4,10}", 3..8).prop_map(|words| {
                let stop_words = ["the", "is", "a", "an", "of"];
                let mut result = Vec::new();
                for (i, word) in words.iter().enumerate() {
                    result.push(word.as_str());
                    // Insert a stop-word between non-stop words
                    if i < words.len() - 1 {
                        result.push(stop_words[i % stop_words.len()]);
                    }
                }
                result.join(" ")
            })
        }

        /// Strategy to generate a message history with stop-words in eligible messages.
        fn history_with_stop_words() -> impl Strategy<Value = Vec<CompactMessage>> {
            let eligible_msgs = proptest::collection::vec(
                content_with_stop_words().prop_flat_map(|content| {
                    prop_oneof![
                        Just("assistant".to_string()),
                        Just("user".to_string()),
                    ].prop_map(move |role| CompactMessage {
                        role,
                        content: content.clone(),
                    })
                }),
                4..10,
            );

            eligible_msgs.prop_map(|mut msgs| {
                // Prepend system message (exempt from stop-word removal)
                msgs.insert(0, CompactMessage {
                    role: "system".to_string(),
                    content: "The system is a helpful assistant.".to_string(),
                });
                // Append final user message (exempt from stop-word removal)
                msgs.push(CompactMessage {
                    role: "user".to_string(),
                    content: "What is the answer?".to_string(),
                });
                msgs
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn stop_words_removed_from_eligible_messages(
                messages in history_with_stop_words(),
            ) {
                let compactor = SimpleCompactor::new();
                let stop_words = stop_word_list();
                let config = CompactorConfig {
                    token_threshold: 1, // Always trigger compression
                    max_history_messages: 100, // High limit so window doesn't interfere
                    stop_words: stop_words.clone(),
                    tokenizer_name: "cl100k_base".to_string(),
                    ..Default::default()
                };

                let result = compactor.compact(messages.clone(), &config);

                // System messages and last user message are exempt
                let last_user_idx = result.messages.iter().rposition(|m| m.role == "user");

                for (i, msg) in result.messages.iter().enumerate() {
                    // Skip system messages and the last user message
                    if msg.role == "system" || Some(i) == last_user_idx {
                        continue;
                    }

                    // Check that no stop-word appears in the content
                    let words: Vec<&str> = msg.content.split_whitespace().collect();
                    for word in &words {
                        let clean: String = word.to_lowercase()
                            .chars()
                            .filter(|c| c.is_alphanumeric())
                            .collect();
                        prop_assert!(
                            !stop_words.iter().any(|sw| sw.to_lowercase() == clean),
                            "Stop-word '{}' found in eligible message (role='{}', content='{}')",
                            clean, msg.role, msg.content
                        );
                    }
                }
            }
        }
    }

    /// **Validates: Requirements 3.6**
    ///
    /// Property 8: Context Compactor é Identidade Abaixo do Limiar
    /// For any message history whose token count is below the threshold,
    /// the output is identical to the input (no modification), was_compressed == false.
    mod property_compactor_identity_below_threshold {
        use super::*;

        /// Strategy to generate short messages that won't exceed a high threshold.
        fn short_message_strategy() -> impl Strategy<Value = CompactMessage> {
            let role_strategy = prop_oneof![
                2 => Just("user".to_string()),
                2 => Just("assistant".to_string()),
                1 => Just("system".to_string()),
            ];
            // Short content: 5-30 chars, so total tokens across all messages stays small
            let content_strategy = "[a-zA-Z ]{5,30}";

            (role_strategy, content_strategy).prop_map(|(role, content)| CompactMessage {
                role,
                content,
            })
        }

        /// Strategy to generate short message histories.
        fn short_history_strategy() -> impl Strategy<Value = Vec<CompactMessage>> {
            proptest::collection::vec(short_message_strategy(), 1..6)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn identity_when_below_threshold(
                messages in short_history_strategy(),
            ) {
                let compactor = SimpleCompactor::new();
                // Very high threshold guarantees we stay below it
                let config = CompactorConfig {
                    token_threshold: 100_000,
                    max_history_messages: 100,
                    stop_words: vec!["the".to_string(), "a".to_string()],
                    tokenizer_name: "cl100k_base".to_string(),
                    ..Default::default()
                };

                let result = compactor.compact(messages.clone(), &config);

                // Output must be identical to input
                prop_assert_eq!(
                    &result.messages, &messages,
                    "Messages were modified despite being below threshold"
                );
                prop_assert!(!result.was_compressed, "was_compressed should be false");
                prop_assert_eq!(result.messages_pruned, 0);
                prop_assert_eq!(result.compression_ratio, 1.0);
                prop_assert_eq!(result.original_tokens, result.final_tokens);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: &str) -> CompactMessage {
        CompactMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    fn config_with_threshold(threshold: usize) -> CompactorConfig {
        CompactorConfig {
            token_threshold: threshold,
            max_history_messages: 20,
            stop_words: Vec::new(),
            tokenizer_name: "cl100k_base".to_string(),
            ..Default::default()
        }
    }

    fn config_with_stop_words(threshold: usize, stop_words: Vec<&str>) -> CompactorConfig {
        CompactorConfig {
            token_threshold: threshold,
            max_history_messages: 20,
            stop_words: stop_words.into_iter().map(String::from).collect(),
            tokenizer_name: "cl100k_base".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_below_threshold_passthrough() {
        let compactor = SimpleCompactor::new();
        let messages = vec![
            make_msg("system", "You are helpful."),
            make_msg("user", "Hello!"),
        ];

        // These messages are short, set a high threshold
        let config = config_with_threshold(4096);
        let result = compactor.compact(messages.clone(), &config);

        assert!(!result.was_compressed);
        assert_eq!(result.messages_pruned, 0);
        assert_eq!(result.compression_ratio, 1.0);
        assert_eq!(result.messages, messages);
        assert_eq!(result.original_tokens, result.final_tokens);
    }

    #[test]
    fn test_system_messages_preserved() {
        let compactor = SimpleCompactor::new();
        // Create enough content to exceed a low threshold
        let long_content = "a".repeat(200); // ~50 tokens
        let messages = vec![
            make_msg("system", "Critical system instructions that must be preserved intact."),
            make_msg("assistant", &long_content),
            make_msg("user", "Old user message with content"),
            make_msg("assistant", &long_content),
            make_msg("user", "Latest user question"),
        ];

        // Set a low threshold so compaction triggers
        let config = config_with_threshold(20);
        let result = compactor.compact(messages, &config);

        assert!(result.was_compressed);
        // System message must be preserved
        let system_msgs: Vec<&CompactMessage> = result.messages.iter()
            .filter(|m| m.role == "system")
            .collect();
        assert_eq!(system_msgs.len(), 1);
        assert_eq!(
            system_msgs[0].content,
            "Critical system instructions that must be preserved intact."
        );
    }

    #[test]
    fn test_last_user_message_preserved() {
        let compactor = SimpleCompactor::new();
        let long_content = "word ".repeat(100); // ~125 tokens (500 chars / 4)
        let messages = vec![
            make_msg("system", "System prompt."),
            make_msg("user", &long_content),
            make_msg("assistant", &long_content),
            make_msg("user", "This is the last user message and must be preserved"),
        ];

        let config = config_with_threshold(20);
        let result = compactor.compact(messages, &config);

        assert!(result.was_compressed);
        // The last user message must be present unchanged
        let last_msg = result.messages.last().unwrap();
        assert_eq!(last_msg.role, "user");
        assert_eq!(last_msg.content, "This is the last user message and must be preserved");
    }

    #[test]
    fn test_stop_words_removed() {
        let compactor = SimpleCompactor::new();
        // Need enough content to exceed threshold
        let padding = "content ".repeat(50); // ~100 tokens
        let messages = vec![
            make_msg("system", "System prompt."),
            make_msg("user", &format!("the quick brown fox jumps over the lazy dog {}", padding)),
            make_msg("assistant", &format!("here is a response with the word the in it {}", padding)),
            make_msg("user", "final user message the end"),
        ];

        let config = config_with_stop_words(20, vec!["the", "a", "is", "in", "it"]);
        let result = compactor.compact(messages, &config);

        assert!(result.was_compressed);

        // Check that eligible messages had stop-words removed
        // System message and last user message should be untouched
        let system_msg = result.messages.iter().find(|m| m.role == "system").unwrap();
        assert_eq!(system_msg.content, "System prompt.");

        // The last user message should be preserved intact (not eligible for stop-word removal)
        let last_user = result.messages.last().unwrap();
        assert_eq!(last_user.role, "user");
        assert_eq!(last_user.content, "final user message the end");

        // Check that remaining eligible messages don't contain stop-words
        for msg in &result.messages {
            if msg.role != "system" && msg != result.messages.last().unwrap() {
                let words: Vec<&str> = msg.content.split_whitespace().collect();
                for word in &words {
                    let clean: String = word.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
                    assert!(
                        !["the", "a", "is", "in", "it"].contains(&clean.as_str()),
                        "Stop-word '{}' found in eligible message content: {}",
                        clean,
                        msg.content
                    );
                }
            }
        }
    }

    #[test]
    fn test_25_percent_target_hit() {
        let compactor = SimpleCompactor::new();
        // Create messages where pruning oldest will achieve 25% reduction
        let chunk = "abcdefgh ".repeat(50); // ~112 tokens per message (450 chars / 4)
        let messages = vec![
            make_msg("system", "sys"),
            make_msg("user", &chunk),       // oldest eligible
            make_msg("assistant", &chunk),   // second oldest eligible
            make_msg("user", &chunk),        // third oldest eligible
            make_msg("assistant", &chunk),   // fourth oldest eligible
            make_msg("user", "last user"),   // last user - protected
        ];

        let config = config_with_threshold(20);
        let result = compactor.compact(messages, &config);

        assert!(result.was_compressed);
        // Compression ratio should be <= 0.75 (25% reduction achieved)
        assert!(
            result.compression_ratio <= 0.76, // small tolerance for rounding
            "Expected compression_ratio <= 0.76, got {}",
            result.compression_ratio
        );
        assert!(result.messages_pruned > 0);
    }

    #[test]
    fn test_best_effort_when_target_unreachable() {
        let compactor = SimpleCompactor::new();
        // Create a scenario where system + last user take most of the tokens,
        // so pruning all eligible messages won't reach 25%
        let big_system = "system ".repeat(200); // ~350 tokens
        let big_user = "user content ".repeat(200); // ~650 tokens
        let small_eligible = "hi"; // ~1 token

        let messages = vec![
            make_msg("system", &big_system),
            make_msg("assistant", small_eligible),
            make_msg("user", &big_user),
        ];

        let config = config_with_threshold(20);
        let result = compactor.compact(messages, &config);

        assert!(result.was_compressed);
        // Even though 25% target may not be hit, it should still compress what it can
        // The assistant message (the only eligible one) should be pruned
        assert_eq!(result.messages_pruned, 1);
        // Compression ratio will be > 0.75 since we couldn't reach the target
        assert!(result.compression_ratio > 0.0);
        // System and last user must still be present
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, "system");
        assert_eq!(result.messages[1].role, "user");
    }

    #[test]
    fn test_message_order_preserved() {
        let compactor = SimpleCompactor::new();
        let chunk = "abcdefghij ".repeat(40); // ~110 tokens per message
        let messages = vec![
            make_msg("system", "First system"),
            make_msg("user", &chunk),         // eligible, oldest
            make_msg("assistant", &chunk),     // eligible
            make_msg("system", "Second system"),
            make_msg("user", &chunk),          // eligible
            make_msg("assistant", &chunk),     // eligible
            make_msg("user", "final question"), // last user, protected
        ];

        let config = config_with_threshold(20);
        let result = compactor.compact(messages, &config);

        assert!(result.was_compressed);
        // Verify order: system messages should remain in their relative positions
        let roles: Vec<&str> = result.messages.iter().map(|m| m.role.as_str()).collect();
        // System messages must appear before any non-system messages that were originally after them
        let sys_indices: Vec<usize> = roles.iter().enumerate()
            .filter(|(_, r)| **r == "system")
            .map(|(i, _)| i)
            .collect();

        // First system should always be first
        if !sys_indices.is_empty() {
            assert_eq!(sys_indices[0], 0);
        }

        // Last message should be the protected user message
        assert_eq!(result.messages.last().unwrap().content, "final question");
    }

    #[test]
    fn test_count_tokens_approximation() {
        // 4 chars = 1 token
        assert_eq!(SimpleCompactor::count_tokens("abcd"), 1);
        // 5 chars = 2 tokens (ceiling)
        assert_eq!(SimpleCompactor::count_tokens("abcde"), 2);
        // empty = 0
        assert_eq!(SimpleCompactor::count_tokens(""), 0);
        // 8 chars = 2 tokens
        assert_eq!(SimpleCompactor::count_tokens("abcdefgh"), 2);
        // 1 char = 1 token (minimum)
        assert_eq!(SimpleCompactor::count_tokens("a"), 1);
    }

    #[test]
    fn test_empty_messages() {
        let compactor = SimpleCompactor::new();
        let messages: Vec<CompactMessage> = vec![];
        let config = config_with_threshold(4096);
        let result = compactor.compact(messages, &config);

        assert!(!result.was_compressed);
        assert_eq!(result.original_tokens, 0);
        assert_eq!(result.final_tokens, 0);
        assert_eq!(result.messages_pruned, 0);
        assert!(result.messages.is_empty());
    }

    #[test]
    fn test_only_system_and_last_user_no_pruning_candidates() {
        let compactor = SimpleCompactor::new();
        // Only system and a single user message - nothing eligible to prune
        let big_content = "word ".repeat(200);
        let messages = vec![
            make_msg("system", &big_content),
            make_msg("user", &big_content),
        ];

        let config = config_with_threshold(20);
        let result = compactor.compact(messages.clone(), &config);

        assert!(result.was_compressed);
        // No messages should be pruned since none are eligible
        assert_eq!(result.messages_pruned, 0);
        // Messages should remain unchanged (no stop-words configured)
        assert_eq!(result.messages.len(), 2);
    }
}
