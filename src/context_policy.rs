//! Internal `ContextPolicy` contract (Phase 6B, Part 1).
//!
//! A thin, internal trait that names the existing context-compaction entry points
//! so future policies have one extension point. It is **not** a plugin API, an ABI,
//! or a stable third-party contract — it wraps the in-tree implementations with no
//! intentional behavior change.
//!
//! Responsibilities are deliberately split:
//! - a **policy** decides which messages to keep and (for scored modes) how to rank
//!   them, returning a [`JsonCompaction`] that references the original messages by
//!   index (`kept_indices`) and carries the original JSON objects;
//! - the **host** (the request handler) still owns and validates the invariants:
//!   message/field preservation, order, tool-group atomicity, budget semantics
//!   (legacy/strict), token counting, metrics, and the strict-budget error → 400.
//!   Those guarantees are not delegated to an interchangeable implementation.
//!
//! The two internal policies have genuinely different shapes and that difference is
//! preserved rather than forced into one signature:
//! - [`LegacyCompactionPolicy`] is synchronous and makes no embedding calls;
//! - [`ScoredCompactionPolicy`] computes embeddings asynchronously in the handler
//!   (Phase 5) and hands ready scores to the synchronous selector.
//!
//! Both return the same [`JsonCompaction`] so the host treats the result uniformly.

use async_trait::async_trait;
use serde_json::Value;

use crate::compactor::{
    compact_json_messages, ContextCompactor, JsonCompaction, SemanticGuardedCompactor,
    SimpleCompactor,
};
use crate::config::CompactorConfig;
use crate::embeddings::EmbeddingProvider;
use crate::semantic_scoring::{BudgetInsufficient, ScoringConfig, ScoringOutcome};

/// A context-compaction error surfaced to the host. Currently only the strict
/// budget rejection; the host maps it to a client-visible 400.
#[derive(Debug, Clone)]
pub enum ContextError {
    BudgetInsufficient(BudgetInsufficient),
}

/// Result of applying a [`ContextPolicy`].
///
/// `scoring` is present only for the scored policy and carries the Phase 5
/// telemetry (requested/effective mode, fallback/skip reasons, counts) the host
/// records; its `compaction` is the same value as [`Self::compaction`].
pub struct ContextOutcome {
    pub compaction: JsonCompaction,
    pub scoring: Option<ScoringOutcome>,
}

/// Internal contract for a context-compaction policy. See the module docs for the
/// host/policy responsibility split.
#[async_trait]
pub trait ContextPolicy: Send + Sync {
    /// Compact `messages` under `config`. `embeddings` is used only by policies
    /// that need it (scored modes); legacy policies ignore it and make no calls.
    async fn compact(
        &self,
        messages: &[Value],
        config: &CompactorConfig,
        embeddings: &dyn EmbeddingProvider,
    ) -> Result<ContextOutcome, ContextError>;

    /// Stable internal name for logs/metrics (not a public identifier).
    fn name(&self) -> &'static str;
}

/// Legacy, synchronous compaction (bugfix D1 preservation). No embedding calls.
/// Wraps [`compact_json_messages`] with the strategy's compactor, unchanged.
pub struct LegacyCompactionPolicy;

#[async_trait]
impl ContextPolicy for LegacyCompactionPolicy {
    async fn compact(
        &self,
        messages: &[Value],
        config: &CompactorConfig,
        _embeddings: &dyn EmbeddingProvider,
    ) -> Result<ContextOutcome, ContextError> {
        let compactor: Box<dyn ContextCompactor> = if config.strategy == "semantic_guarded_trimming"
        {
            Box::new(SemanticGuardedCompactor::new())
        } else {
            Box::new(SimpleCompactor::new())
        };
        let compaction = compact_json_messages(compactor.as_ref(), messages, config);
        Ok(ContextOutcome {
            compaction,
            scoring: None,
        })
    }

    fn name(&self) -> &'static str {
        "legacy"
    }
}

/// Phase 5 scored compaction (`embedding`/`hybrid`). Computes embeddings via the
/// injected provider, then runs the synchronous deterministic selector. Wraps
/// [`crate::semantic_scoring::compact_with_scoring`] unchanged; a strict-budget
/// shortfall becomes [`ContextError::BudgetInsufficient`].
pub struct ScoredCompactionPolicy {
    pub config: ScoringConfig,
}

#[async_trait]
impl ContextPolicy for ScoredCompactionPolicy {
    async fn compact(
        &self,
        messages: &[Value],
        config: &CompactorConfig,
        embeddings: &dyn EmbeddingProvider,
    ) -> Result<ContextOutcome, ContextError> {
        match crate::semantic_scoring::compact_with_scoring(
            messages,
            config,
            &self.config,
            embeddings,
        )
        .await
        {
            Ok(outcome) => Ok(ContextOutcome {
                compaction: outcome.compaction.clone(),
                scoring: Some(outcome),
            }),
            Err(insufficient) => Err(ContextError::BudgetInsufficient(insufficient)),
        }
    }

    fn name(&self) -> &'static str {
        "scored"
    }
}

/// Selects the policy for a request exactly as the handler did inline: the scored
/// policy when `semantic_scoring` opted in, otherwise the legacy policy.
pub fn select_policy(scoring: &Option<ScoringConfig>) -> Box<dyn ContextPolicy> {
    match scoring {
        Some(sc) => Box::new(ScoredCompactionPolicy { config: sc.clone() }),
        None => Box::new(LegacyCompactionPolicy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::ScriptedEmbeddingProvider;
    use serde_json::json;

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

    fn long(tag: &str, n: usize) -> String {
        format!("{tag} ").repeat(n)
    }

    fn conversation() -> Vec<Value> {
        vec![
            json!({"role": "system", "content": "You are helpful."}),
            json!({"role": "user", "content": long("old filler", 30), "name": "a"}),
            json!({"role": "assistant", "content": long("old answer", 30)}),
            json!({"role": "user", "content": long("more", 30)}),
            json!({"role": "user", "content": "final question"}),
        ]
    }

    fn stub() -> ScriptedEmbeddingProvider {
        ScriptedEmbeddingProvider::new(vec![], vec![1.0, 0.0])
    }

    /// The legacy policy produces exactly the same JsonCompaction as calling the
    /// underlying function directly (no behavior change from the wrapper).
    #[tokio::test]
    async fn legacy_policy_matches_direct_call() {
        let input = conversation();
        let c = cfg();
        let direct = compact_json_messages(&SemanticGuardedCompactor::new(), &input, &c);
        let via = LegacyCompactionPolicy
            .compact(&input, &c, &stub())
            .await
            .unwrap();
        assert!(via.scoring.is_none());
        assert_eq!(via.compaction.messages, direct.messages);
        assert_eq!(
            via.compaction.result.kept_indices,
            direct.result.kept_indices
        );
        assert_eq!(
            via.compaction.result.was_compressed,
            direct.result.was_compressed
        );
    }

    /// The scored policy matches a direct `compact_with_scoring` call.
    #[tokio::test]
    async fn scored_policy_matches_direct_call() {
        let input = conversation();
        let c = cfg();
        let sc = ScoringConfig {
            mode: crate::semantic_scoring::ScoringMode::Hybrid,
            ..Default::default()
        };
        let direct = crate::semantic_scoring::compact_with_scoring(&input, &c, &sc, &stub())
            .await
            .unwrap();
        let via = ScoredCompactionPolicy { config: sc }
            .compact(&input, &c, &stub())
            .await
            .unwrap();
        assert_eq!(
            via.compaction.result.kept_indices,
            direct.compaction.result.kept_indices
        );
        assert_eq!(via.scoring.as_ref().unwrap().effective, direct.effective);
    }

    /// A strict-budget shortfall surfaces as ContextError (host maps it to 400).
    #[tokio::test]
    async fn scored_policy_surfaces_budget_error() {
        let input = conversation();
        let c = cfg();
        let sc = ScoringConfig {
            mode: crate::semantic_scoring::ScoringMode::Hybrid,
            budget: crate::semantic_scoring::BudgetConfig {
                mode: crate::semantic_scoring::BudgetMode::Strict,
                max_context_tokens: Some(5),
                output_reserve_tokens: 1,
                formatting_overhead: crate::semantic_scoring::FormattingOverhead::None,
            },
            ..Default::default()
        };
        let result = ScoredCompactionPolicy { config: sc }
            .compact(&input, &c, &stub())
            .await;
        match result {
            Err(ContextError::BudgetInsufficient(b)) => assert!(b.message().contains("budget")),
            Ok(_) => panic!("expected a strict-budget error"),
        }
    }

    #[tokio::test]
    async fn select_policy_picks_scored_only_when_opted_in() {
        assert_eq!(select_policy(&None).name(), "legacy");
        let sc = ScoringConfig::default();
        assert_eq!(select_policy(&Some(sc)).name(), "scored");
    }
}
