//! Offline evaluation harness for Phase 5 scoring (test-only).
//!
//! Compares `heuristic`, `embedding` and `hybrid` on the SAME conversations and the
//! SAME budget, and writes a reproducible JSON report (code revision, dataset hash,
//! provider, embedding model, weights, full configuration).
//!
//! Data split (do not mix):
//! - `eval/compaction/dev.jsonl`  — development / any tuning of weights or limits;
//! - `eval/compaction/eval.jsonl` — final evaluation only. Running it requires
//!   `MELIS_EVAL_CONFIRM_FINAL=1`, as a guard against tuning on it.
//!
//! The bundled fixtures are SYNTHETIC and tiny: they exercise the harness, they do
//! not support conclusions. The default `stub` provider is a lexical bag-of-words
//! hash (not a semantic model): stub results say nothing about real embeddings.
//!
//! Run (writes `target/compaction-eval/*.json`):
//! ```text
//! cargo test --bin melis-gateway semantic_scoring::eval::harness_report -- --ignored --nocapture
//! MELIS_EVAL_PROVIDER=ollama MELIS_EMBED_MODEL=nomic-embed-text cargo test ... (same)
//! MELIS_EVAL_SPLIT=eval MELIS_EVAL_CONFIRM_FINAL=1 ... (final run only)
//! ```
//! Other knobs: `MELIS_OLLAMA_URL`, `MELIS_EVAL_SEMANTIC_WEIGHT`,
//! `MELIS_EVAL_STRUCTURAL_WEIGHT`, `MELIS_EVAL_TARGET_RATIO`.

use super::*;
use serde_json::json;

/// Deterministic LEXICAL stub: hashed bag of words (dim 256). Not semantic.
pub struct BagOfWordsEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for BagOfWordsEmbedding {
    async fn embed(&self, _model: &str, text: &str) -> Option<Vec<f64>> {
        let mut v = vec![0.0; 256];
        for w in text.split_whitespace() {
            let w: String = w
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            if w.len() < 3 {
                continue;
            }
            v[(fnv1a(w.as_bytes()) % 256) as usize] += 1.0;
        }
        if v.iter().all(|x| *x == 0.0) {
            v[0] = 1.0; // never a zero vector
        }
        Some(v)
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn git_revision() -> Value {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    json!({
        "commit": run(&["rev-parse", "HEAD"]),
        "dirty": run(&["status", "--porcelain"]).map(|s| !s.is_empty()),
        "crate_version": env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Debug, Clone)]
pub struct EvalOptions {
    pub split: String,
    pub provider: String,
    pub embedding_model: String,
    pub ollama_url: String,
    pub semantic_weight: f64,
    pub structural_weight: f64,
    pub target_ratio: f64,
}

impl EvalOptions {
    pub fn from_env() -> Self {
        Self {
            split: env_or("MELIS_EVAL_SPLIT", "dev"),
            provider: env_or("MELIS_EVAL_PROVIDER", "stub"),
            embedding_model: env_or("MELIS_EMBED_MODEL", "nomic-embed-text"),
            ollama_url: env_or("MELIS_OLLAMA_URL", "http://localhost:11434"),
            semantic_weight: env_f64("MELIS_EVAL_SEMANTIC_WEIGHT", 0.5),
            structural_weight: env_f64("MELIS_EVAL_STRUCTURAL_WEIGHT", 0.5),
            target_ratio: env_f64("MELIS_EVAL_TARGET_RATIO", 0.5),
        }
    }
}

fn provider_for(o: &EvalOptions) -> Box<dyn EmbeddingProvider> {
    match o.provider.as_str() {
        "ollama" => {
            let gw = crate::config::GatewayConfig::from_yaml(&format!(
                "server:\n  host: \"0.0.0.0\"\n  port: 8080\nredis:\n  cluster_urls: [\"redis://localhost:6379\"]\nproviders:\n  - id: ollama\n    provider_type: ollama\n    api_key: ollama\n    base_url: \"{}\"\n",
                o.ollama_url
            ))
            .expect("valid ollama config");
            Box::new(crate::embeddings::OllamaEmbeddingProvider::new(
                gw.providers,
                std::sync::Arc::new(crate::client::ReqwestLlmClient::new()),
            ))
        }
        "stub" => Box::new(BagOfWordsEmbedding),
        other => panic!("unknown MELIS_EVAL_PROVIDER {other:?} (stub|ollama)"),
    }
}

/// Run the three modes on one split. Same budget for every mode.
pub async fn run_eval(o: &EvalOptions) -> Value {
    assert!(
        o.split == "dev" || o.split == "eval",
        "MELIS_EVAL_SPLIT must be dev or eval"
    );
    if o.split == "eval" {
        assert_eq!(
            std::env::var("MELIS_EVAL_CONFIRM_FINAL").as_deref(),
            Ok("1"),
            "the eval split is for the final run only; set MELIS_EVAL_CONFIRM_FINAL=1 (never tune on it)"
        );
    }
    let path = format!(
        "{}/eval/compaction/{}.jsonl",
        env!("CARGO_MANIFEST_DIR"),
        o.split
    );
    let raw = std::fs::read_to_string(&path).expect("dataset file");
    let rows: Vec<Value> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSONL row"))
        .collect();

    let cfg = CompactorConfig {
        token_threshold: 40,
        max_history_messages: 1000,
        target_token_ratio: o.target_ratio,
        min_final_tokens: 10,
        strategy: "semantic_guarded_trimming".to_string(),
        ..Default::default()
    };
    let provider = provider_for(o);

    let mut per_mode = serde_json::Map::new();
    for mode in [
        ScoringMode::Heuristic,
        ScoringMode::Embedding,
        ScoringMode::Hybrid,
    ] {
        let sc = ScoringConfig {
            mode,
            semantic_weight: o.semantic_weight,
            structural_weight: o.structural_weight,
            embedding_model: o.embedding_model.clone(),
            timeout_ms: if o.provider == "ollama" {
                60_000
            } else {
                1_500
            },
            ..Default::default()
        };
        sc.validate().expect("valid scoring config");

        let mut convs = Vec::new();
        let (mut recall_sum, mut tokens_sum, mut kept_sum, mut fallbacks) =
            (0.0, 0usize, 0usize, 0usize);
        for row in &rows {
            let messages = row["messages"].as_array().expect("messages");
            let relevant: Vec<usize> = row["relevant"]
                .as_array()
                .expect("relevant")
                .iter()
                .map(|v| v.as_u64().unwrap() as usize)
                .collect();
            let outcome = compact_with_scoring(messages, &cfg, &sc, provider.as_ref())
                .await
                .expect("legacy budget never errors");
            let kept = if outcome.compaction.result.was_compressed {
                outcome.compaction.result.kept_indices.clone()
            } else {
                (0..messages.len()).collect()
            };
            let hit = relevant.iter().filter(|i| kept.contains(i)).count();
            let recall = if relevant.is_empty() {
                1.0
            } else {
                hit as f64 / relevant.len() as f64
            };
            recall_sum += recall;
            tokens_sum += outcome.compaction.result.final_tokens;
            kept_sum += kept.len();
            fallbacks += outcome.fallback.is_some() as usize;
            convs.push(json!({
                "id": row["id"],
                "original_tokens": outcome.compaction.result.original_tokens,
                "final_tokens": outcome.compaction.result.final_tokens,
                "kept_indices": kept,
                "relevant": relevant,
                "recall": recall,
                "effective_mode": outcome.effective.label(),
                "fallback": outcome.fallback.map(|r| r.label()),
                "skipped": outcome.skipped.map(|r| r.label()),
            }));
        }
        let n = rows.len().max(1) as f64;
        per_mode.insert(
            mode.label().to_string(),
            json!({
                "mean_recall": recall_sum / n,
                "mean_final_tokens": tokens_sum as f64 / n,
                "mean_kept_messages": kept_sum as f64 / n,
                "fallbacks": fallbacks,
                "conversations": convs,
            }),
        );
    }

    let (ws, wt) = ScoringConfig {
        mode: ScoringMode::Hybrid,
        semantic_weight: o.semantic_weight,
        structural_weight: o.structural_weight,
        ..Default::default()
    }
    .effective_weights();

    json!({
        "metadata": {
            "code": git_revision(),
            "split": o.split,
            "dataset": {
                "path": format!("eval/compaction/{}.jsonl", o.split),
                "fnv1a64": format!("{:016x}", fnv1a(raw.as_bytes())),
                "conversations": rows.len(),
                "synthetic": rows.iter().all(|r| r["synthetic"] == true),
            },
            "embedding_provider": o.provider,
            "embedding_model": if o.provider == "stub" { "bag-of-words-hash-256 (lexical stub)".to_string() } else { o.embedding_model.clone() },
            "weights": {"semantic": o.semantic_weight, "structural": o.structural_weight,
                        "normalized_hybrid": [ws, wt], "calibrated": false},
            "similarity_mapping": "(cos + 1) / 2, clamped; scale transform, not a probability",
            "compactor": {
                "strategy": cfg.strategy,
                "token_threshold": cfg.token_threshold,
                "target_token_ratio": cfg.target_token_ratio,
                "min_final_tokens": cfg.min_final_tokens,
                "token_counting": format!("{:?}", cfg.token_counting),
                "preserve_critical_markers": cfg.preserve_critical_markers,
            },
            "scoring_limits": {
                "max_messages": default_max_messages(), "max_chars": default_max_chars(),
                "batch_size": default_batch_size(), "max_concurrency": default_max_concurrency(),
            },
            "same_budget_all_modes": true,
        },
        "modes": per_mode,
    })
}

/// Shape check on the dev split with the stub (runs in the default suite).
#[tokio::test]
async fn harness_runs_three_modes_with_same_budget_and_metadata() {
    let o = EvalOptions {
        split: "dev".into(),
        provider: "stub".into(),
        embedding_model: "unused".into(),
        ollama_url: String::new(),
        semantic_weight: 0.5,
        structural_weight: 0.5,
        target_ratio: 0.5,
    };
    let r = run_eval(&o).await;
    for mode in ["heuristic", "embedding", "hybrid"] {
        let m = &r["modes"][mode];
        assert_eq!(m["conversations"].as_array().unwrap().len(), 3, "{mode}");
        assert!(m["mean_recall"].as_f64().is_some());
    }
    // Same original tokens per conversation across modes = same input & budget.
    for k in 0..3 {
        let t = |mode: &str| r["modes"][mode]["conversations"][k]["original_tokens"].clone();
        assert_eq!(t("heuristic"), t("embedding"));
        assert_eq!(t("heuristic"), t("hybrid"));
    }
    let md = &r["metadata"];
    assert_eq!(md["split"], "dev");
    assert_eq!(md["weights"]["calibrated"], false);
    assert_eq!(md["dataset"]["synthetic"], true);
    assert!(md["dataset"]["fnv1a64"].as_str().unwrap().len() == 16);
}

#[tokio::test]
async fn eval_split_requires_explicit_confirmation() {
    if std::env::var("MELIS_EVAL_CONFIRM_FINAL").is_ok() {
        return; // explicitly confirmed environment; guard intentionally bypassed
    }
    let o = EvalOptions {
        split: "eval".into(),
        ..EvalOptions::from_env()
    };
    let r = tokio::spawn(async move { run_eval(&o).await }).await;
    assert!(r.is_err(), "eval split must not run without confirmation");
}

/// Writes the full report. `--ignored` because it is an experiment, not a test.
#[tokio::test]
#[ignore = "offline evaluation; writes target/compaction-eval/*.json"]
async fn harness_report() {
    let o = EvalOptions::from_env();
    let report = run_eval(&o).await;
    let dir = format!("{}/target/compaction-eval", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let file = format!("{dir}/report-{}-{}-{ts}.json", o.split, o.provider);
    std::fs::write(&file, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!("report written to {file}");
    for mode in ["heuristic", "embedding", "hybrid"] {
        let m = &report["modes"][mode];
        eprintln!(
            "{mode:<10} mean_recall={:.3} mean_final_tokens={:.1} mean_kept={:.2} fallbacks={}",
            m["mean_recall"].as_f64().unwrap(),
            m["mean_final_tokens"].as_f64().unwrap(),
            m["mean_kept_messages"].as_f64().unwrap(),
            m["fallbacks"]
        );
    }
}
