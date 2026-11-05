# Instrumentation — CSV schemas, validation, and aggregation

Implements the data layer of `../PROTOCOL.md`: versioned CSV schemas, stable
ids, a safe writer, a manifest builder, a validator, and a reproducible
aggregation step. The **core is stdlib-only** (runs on Python 3.12 — the pinned
backend image — and 3.14), so raw-data writing and validation have **no external
dependencies**. Optional statistical-analysis deps are pinned in
`requirements.txt` and used only by the post-approval confirmatory analysis.

## Modules

| File | Role |
|------|------|
| `SCHEMA_VERSION` | current schema version (`1.0.0`); every row carries it |
| `schemas.py` | single source of truth: columns, types, units, enums, keys |
| `ids.py` | stable, deterministic run/block/case/pair/request ids (pairing-aware) |
| `csv_writer.py` | UTF-8 safe writer; **missing ≠ zero**; enforces required/enum/bool |
| `manifest.py` | `manifest.json`: clean-commit, digests, models, config/dataset hashes, seeds, hardware, measurement origin, field limitations |
| `validate.py` | required fields, keys, duplicates, referential integrity, requests↔runs consistency (record loss / dropped failures) |
| `aggregate.py` | derives `runs.csv` + `comparisons.csv` from raw `requests.csv`; never overwrites raw data; blocks/episodes/cases are the independent units |
| `data_dictionary.md` | human-readable field reference + protocol→schema traceability |
| `requirements.txt` | pinned optional analysis deps (not needed for core) |
| `tests/` | small tests on synthetic known-result data (not evidence) |

## Design guarantees

- **Phases separated**: `phase ∈ {smoke,pilot,final}` is mandatory on every row,
  and phases live in separate directories (`../data/smoke|pilot|final`).
- **Missing ≠ zero**: unobservable values are empty cells; genuine zeros are `0`.
- **Units explicit**: encoded in column names (`_ms`, `_s`, `_bytes`, `_pct`, `_rps`).
- **Independence respected**: a contrast's `n` counts independent units
  (blocks/episodes/cases), never nested requests or per-case generations.
- **Raw data immutable**: summaries are derived to separate files; `aggregate.py`
  refuses to write over `requests.csv`.
- **No fabricated statistics**: contract checks export expected/observed/pass-fail
  with empty p-values; inferential p-values are produced only by the
  pre-registered analysis step.
- **Failures retained**: failures/timeouts/invalid responses are rows; the
  validator flags any run summary that drops them.
- **TTFB, first SSE content, and completion are distinct** columns; a chunk is
  not called a token unless tokens are observed.

## Quick start (synthetic example — not evidence)

```bash
cd instrumentation
# 1) generate a small synthetic dataset with a KNOWN result
python3 tests/make_synthetic.py ../data/examples
# 2) validate the raw data against the versioned schema
python3 validate.py --data-dir ../data/examples --report ../data/examples/validation_report_raw.json
# 3) derive runs.csv + comparisons.csv (never overwrites requests.csv)
python3 aggregate.py --data-dir ../data/examples \
    --runs-out ../data/examples/runs.csv \
    --comparisons-out ../data/examples/comparisons.csv --seed 20261105
# 4) validate again (now also checks requests↔runs consistency)
python3 validate.py --data-dir ../data/examples --report ../data/examples/validation_report.json
# 5) run the script tests
python3 tests/test_instrumentation.py
```

The committed `../data/examples/` directory contains the output of exactly these
steps. Its `manifest.json` honestly reports the tree as not-clean because the
experiment worktree carries the intentional `bedrock.rs` WIP (excluded from
commits) plus these untracked experiment files; a real collection runs from a
clean, committed artifact and its manifest will report `clean: true`.

## Wiring to the harness (next)

`../harness/bench_client.py` already classifies each request as
`ok/http_error/transport_error/invalid_response` and reads the deterministic
backend `/__count`. The pilot step maps those per-request results into
`requests.csv` rows (and `attempts.csv` where attempts are observable), tags
them `phase=pilot`, and writes a `manifest.json` next to them via `manifest.py`.
Raw pilot data stays local (see `../data/.gitignore`); only sanitized synthetic
examples are versioned.
