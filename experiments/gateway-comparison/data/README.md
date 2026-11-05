# Experiment data directories

Raw data is separated by phase, and phases are **never** mixed:

| Directory | Contents | Versioned in git? |
|-----------|----------|-------------------|
| `smoke/` | plumbing-check data; identified as `phase=smoke` | only sanitized **synthetic** examples |
| `pilot/` | pilot data to size the final collection; `phase=pilot` | **no** raw pilot data (kept local) |
| `final/` | confirmatory collection; `phase=final` | **no** (local; added only with explicit authorization) |
| `examples/` | small, synthetic, known-result CSVs used to test the scripts | yes (sanitized) |

Rules (from `../PROTOCOL.md`):

- Smoke and pilot results are explicitly separated from final results and are
  never promoted into the confirmatory set.
- The example CSVs under `examples/` are **synthetic** and exist only to
  exercise the instrumentation scripts. They are **not** experimental evidence.
- Raw data is never overwritten when generating summaries; `runs.csv` and
  `comparisons.csv` are derived into the same phase directory by `aggregate.py`.
- Each phase directory that holds real data also holds its `manifest.json` and a
  `validation_report.json`.

`.gitignore` in this folder keeps the directory structure and the synthetic
examples while excluding real raw data, logs, and large result blobs.
