//! TEST-ONLY reference: the compaction algorithms exactly as they were before
//! bugfix D1 (copied verbatim from `feat/phase-4-redis-cb`, only `Self::` made
//! explicit and an empty `kept_indices` added). Used to prove legacy parity of
//! the new pin-aware code on text-only conversations. Do not modify.
#![allow(clippy::all)]
#![allow(unused)]

use super::*;

pub fn legacy_simple(messages: Vec<CompactMessage>, config: &CompactorConfig) -> CompactionResult {
    // Resolve the token counter once per call. Real BPE instances are cached
    // singletons, so this does not allocate the vocabulary per request.
    let counter = TokenCounter::resolve(config.token_counting, &config.tokenizer_name);

    let original_tokens = SimpleCompactor::total_tokens_with(&counter, &messages);
    let original_count = messages.len();

    // Step 1: If tokens < threshold AND message count <= max_history_messages → no compression
    if original_tokens < config.token_threshold && original_count <= config.max_history_messages {
        return CompactionResult {
            original_tokens,
            final_tokens: original_tokens,
            messages_pruned: 0,
            compression_ratio: 1.0,
            kept_indices: Vec::new(),
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

    let windowed_messages: Vec<CompactMessage> =
        if non_system_messages.len() > config.max_history_messages {
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
    let tokens_after_window = SimpleCompactor::total_tokens_with(&counter, &windowed_messages);
    if tokens_after_window < config.token_threshold {
        // SAFETY: never return empty messages array
        if windowed_messages.is_empty() {
            return CompactionResult {
                original_tokens,
                final_tokens: original_tokens,
                messages_pruned: 0,
                compression_ratio: 1.0,
                kept_indices: Vec::new(),
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
            kept_indices: Vec::new(),
            was_compressed: messages_pruned_by_window > 0,
            messages: windowed_messages,
        };
    }

    // Step 4: Still above threshold → apply token-based pruning on eligible messages
    // Find the index of the last user message in the windowed set
    let last_user_idx = windowed_messages.iter().rposition(|m| m.role == "user");

    // Identify eligible messages (not system, not last user)
    let eligible_indices: Vec<usize> = windowed_messages
        .iter()
        .enumerate()
        .filter(|(i, m)| m.role != "system" && Some(*i) != last_user_idx)
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
                    content: SimpleCompactor::remove_stop_words(&m.content, &config.stop_words),
                }
            } else {
                m.clone()
            }
        })
        .collect();

    // Calculate target: 25% reduction means final should be <= 75% of original
    let target_tokens = (original_tokens as f64 * 0.75) as usize;

    // Check if stop-word removal alone achieved the target
    let tokens_after_stopwords = SimpleCompactor::total_tokens_with(&counter, &working_messages);
    if tokens_after_stopwords <= target_tokens {
        // SAFETY: never return empty messages array
        if working_messages.is_empty() {
            return CompactionResult {
                original_tokens,
                final_tokens: original_tokens,
                messages_pruned: 0,
                compression_ratio: 1.0,
                kept_indices: Vec::new(),
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
            kept_indices: Vec::new(),
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
            .map(|(_, m)| {
                SimpleCompactor::count_tokens_with(&counter, &m.role)
                    + SimpleCompactor::count_tokens_with(&counter, &m.content)
            })
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
                kept_indices: Vec::new(),
                was_compressed: false,
                messages,
            };
        }
    } else {
        final_messages
    };

    let final_tokens = SimpleCompactor::total_tokens_with(&counter, &final_messages);
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
        kept_indices: Vec::new(),
        was_compressed: true,
        messages: final_messages,
    }
}

pub fn legacy_semantic(
    messages: Vec<CompactMessage>,
    config: &CompactorConfig,
) -> CompactionResult {
    // Resolve the token counter once per call (cached singleton for real mode).
    let counter = TokenCounter::resolve(config.token_counting, &config.tokenizer_name);

    let original_tokens = SimpleCompactor::total_tokens_with(&counter, &messages);
    let original_count = messages.len();

    // Step 1: If below threshold, return unchanged
    if original_tokens < config.token_threshold {
        return CompactionResult {
            original_tokens,
            final_tokens: original_tokens,
            messages_pruned: 0,
            compression_ratio: 1.0,
            kept_indices: Vec::new(),
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
        .map(|idx| SemanticGuardedCompactor::extract_query_terms(&messages[idx].content))
        .unwrap_or_default();

    // Step 4: Score all messages
    let scored: Vec<ScoredMessage> = messages
        .iter()
        .enumerate()
        .map(|(i, msg)| {
            let is_last_user = Some(i) == last_user_idx;
            let score = SemanticGuardedCompactor::calculate_score(
                msg,
                is_last_user,
                &last_user_terms,
                &config.critical_markers,
            );
            let token_count = SimpleCompactor::count_tokens_with(&counter, &msg.role)
                + SimpleCompactor::count_tokens_with(&counter, &msg.content);
            let is_protected = msg.role == "system"
                || is_last_user
                || (config.preserve_critical_markers
                    && config
                        .critical_markers
                        .iter()
                        .any(|m| msg.content.to_lowercase().contains(&m.to_lowercase())));

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
        let final_tokens = SimpleCompactor::total_tokens_with(&counter, &final_messages);
        return CompactionResult {
            original_tokens,
            final_tokens,
            messages_pruned: original_count - final_messages.len(),
            compression_ratio: if original_tokens > 0 {
                final_tokens as f64 / original_tokens as f64
            } else {
                1.0
            },
            kept_indices: Vec::new(),
            was_compressed: true,
            messages: final_messages,
        };
    }

    // Step 6: Sort non-protected by score (ascending) to remove lowest first
    let mut removable: Vec<&ScoredMessage> = scored.iter().filter(|s| !s.is_protected).collect();
    removable.sort_by_key(|s| s.score);

    // Step 7: Remove lowest-scoring messages until within budget
    let mut removed_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
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

    let final_tokens = SimpleCompactor::total_tokens_with(&counter, &final_messages);
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
        kept_indices: Vec::new(),
        was_compressed: true,
        messages: final_messages,
    }
}
