#!/usr/bin/env python3
"""Development pilot for RQ2. Development cases only. One generation each.

Arms, in a rotating order so no policy is always first:
  direct          quality reference, no gateway and no compaction
  none            Melis mediation with strategy none
  chronological   product strategy sliding_window
  heuristic       semantic_guarded_trimming with semantic_scoring omitted

Embedding and hybrid are not in this profile. LiteLLM is not a compaction arm.
generation_reps is frozen at 1 and does not multiply the number of cases.
Token columns stay empty: the proxy records characters and messages, not a
tokenizer count and not a billed count. Scoring time is inside latency_ms and
is not separately observable.
"""
from __future__ import annotations

import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "instrumentation"))
sys.path.insert(0, HERE)

import quality_checks  # noqa: E402
from csv_writer import CsvTableWriter  # noqa: E402

CASES = os.path.join(ROOT, "quality", "dev_cases.json")
OUT = os.environ.get("RQ2_OUT", os.path.join(ROOT, "data", "rq2-pilot-dev-20261106"))
PROXY_LOG = os.environ.get(
    "PROXY_LOG", os.path.join(ROOT, "data", "rq2-pilot-dev-20261106", "forwarded.jsonl")
)
COLLECTION = os.environ.get("RQ2_COLLECTION", "col-rq2-dev-pilot-20261106")
EXPERIMENT = "exp-rq2-dev-pilot"
def _run(arm):
    # Policy none is both the direct reference and the Melis control, so the
    # run id, not the policy name, separates those two arms.
    return f"run-rq2-dev-{arm}"
SEED = 20261106
MAX_TOKENS = 64
ARMS = (
    ("direct", "none", "http://127.0.0.1:11435/v1/chat/completions"),
    ("none", "none", "http://127.0.0.1:17071/v1/chat/none"),
    ("chronological", "chronological", "http://127.0.0.1:17071/v1/chat/chronological"),
    ("heuristic", "heuristic", "http://127.0.0.1:17071/v1/chat/heuristic"),
)


def _now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def _post(url, messages):
    body = json.dumps({
        "model": "llama3.2",
        "messages": messages,
        "temperature": 0,
        "max_tokens": MAX_TOKENS,
        "seed": SEED,
    }).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            raw = resp.read()
            code = resp.status
    except urllib.error.HTTPError as exc:
        raw = exc.read()
        code = exc.code
    except (TimeoutError, urllib.error.URLError):
        return 0, (time.perf_counter() - started) * 1000.0, ""
    latency = (time.perf_counter() - started) * 1000.0
    answer = ""
    try:
        payload = json.loads(raw.decode())
        answer = payload["choices"][0]["message"]["content"]
    except (json.JSONDecodeError, KeyError, IndexError, TypeError):
        answer = ""
    return code, latency, answer


def _forwarded_after(line_count):
    if not os.path.exists(PROXY_LOG):
        return None
    lines = open(PROXY_LOG, encoding="utf-8").read().splitlines()
    if len(lines) <= line_count:
        return None
    body = json.loads(json.loads(lines[-1])["body"])
    messages = body.get("messages") or []
    text = "\n".join(m.get("content", "") for m in messages if isinstance(m, dict))
    return {"messages": len(messages), "chars": len(text), "text": text, "lines": len(lines)}


def _order(index):
    shift = index % len(ARMS)
    return ARMS[shift:] + ARMS[:shift]


def main():
    os.makedirs(OUT, exist_ok=True)
    doc = json.load(open(CASES, encoding="utf-8"))
    if doc.get("role") != "development" or not doc.get("not_evaluation"):
        raise SystemExit("refusing to run: file is not the development set")
    if doc.get("generation_reps_pilot") != 1:
        raise SystemExit("refusing to run: generation_reps_pilot is not frozen at 1")
    cases = doc["cases"]
    requests = CsvTableWriter("requests", os.path.join(OUT, "requests.csv"))
    quality = CsvTableWriter("quality", os.path.join(OUT, "quality.csv"))
    facts = CsvTableWriter("facts", os.path.join(OUT, "facts.csv"))
    answers_path = os.path.join(OUT, "answers.jsonl")
    blinded_path = os.path.join(OUT, "blinded_export.jsonl")
    preflight = []
    for index, case in enumerate(cases):
        baseline_text = "\n".join(m["content"] for m in case["messages"])
        for position, (arm, policy, url) in enumerate(_order(index)):
            before = 0 if not os.path.exists(PROXY_LOG) else len(open(PROXY_LOG, encoding="utf-8").read().splitlines())
            code, latency, answer = _post(url, case["messages"])
            forwarded = _forwarded_after(before)
            forwarded_text = forwarded["text"] if forwarded else ""
            ok = code == 200 and bool(answer)
            request_id = f"{case['case_id']}-{arm}"
            requests.write_row({
                "experiment_id": EXPERIMENT,
                "collection_id": COLLECTION,
                "phase": "pilot",
                "run_id": _run(arm),
                "block_id": case["generation_family"],
                "pair_id": case["case_id"],
                "case_id": case["case_id"],
                "request_id": request_id,
                "gateway": "direct" if arm == "direct" else "melis",
                "scenario": "rq2_dev_quality",
                "backend_profile": "real",
                "streaming": False,
                "concurrency": 1,
                "policy": policy,
                "started_at_utc": _now(),
                "latency_ms": round(latency, 3),
                "status": "ok" if ok else ("http_error" if code else "transport_error"),
                "http_status": code or None,
                "timeout": False,
                "completion_valid": ok,
                "requested_model": "llama3.2",
                "resolved_model": "llama3.2",
                "effective_model": "llama3.2",
                "feature_profile": arm,
                "window": "measured",
            })
            scored = quality_checks.rubric(case, answer)
            quality.write_row({
                "run_id": _run(arm),
                "case_id": case["case_id"],
                "pair_id": case["case_id"],
                "policy": policy,
                "generation_rep": 1,
                "answer_id": request_id,
                "evaluator_id": "automatic-rubric-1",
                "evaluator_type": "automatic",
                "score": scored["score"],
                "required_facts": len(case["critical_facts"]) + len(case["other_facts"]),
                "retained_facts": sum(
                    1 for role, fact in (
                        [("critical", f) for f in case["critical_facts"]]
                        + [("other", f) for f in case["other_facts"]]
                    ) if quality_checks.fact_present(answer, fact)
                ),
                "position_stratum": case["position"],
                "domain": case["generation_family"],
                "context_length_stratum": case["budget_pressure"],
                "blinded": False,
            })
            for channel, text in (("forwarded_context", forwarded_text), ("answer", answer)):
                for row in quality_checks.channel_rows(case, text, channel):
                    facts.write_row({
                        "collection_id": COLLECTION,
                        "run_id": _run(arm),
                        "case_id": case["case_id"],
                        "pair_id": case["case_id"],
                        "policy": policy,
                        "evaluator_id": "automatic-rubric-1",
                        **row,
                    })
            preflight.append({
                "case_id": case["case_id"],
                "arm": arm,
                "order": position,
                "baseline_messages": len(case["messages"]),
                "baseline_chars": len(baseline_text),
                "forwarded_messages": None if not forwarded else forwarded["messages"],
                "forwarded_chars": None if not forwarded else forwarded["chars"],
                "critical_in_forwarded": all(
                    quality_checks.fact_present(forwarded_text, fact)
                    for fact in case["critical_facts"]
                ) if forwarded else None,
                "score": scored["score"],
                "rule": scored["rule"],
                "latency_ms": round(latency, 3),
            })
            blind_id = hashlib.sha256(f"{SEED}:{case['case_id']}:{arm}".encode()).hexdigest()[:12]
            with open(answers_path, "a", encoding="utf-8") as fh:
                fh.write(json.dumps({
                    "answer_id": request_id,
                    "blind_id": blind_id,
                    "case_id": case["case_id"],
                    "arm": arm,
                    "policy": policy,
                    "answer": answer,
                }, ensure_ascii=False) + "\n")
            with open(blinded_path, "a", encoding="utf-8") as fh:
                fh.write(json.dumps({
                    "blind_id": blind_id,
                    "case_id": case["case_id"],
                    "answer": answer,
                    "rater_1": None,
                    "rater_2": None,
                }, ensure_ascii=False) + "\n")
            print(case["case_id"], arm, "score", scored["score"], "msgs",
                  None if not forwarded else forwarded["messages"], "s", round(latency / 1000.0, 2), flush=True)
    requests.close()
    quality.close()
    facts.close()
    controls = []
    for case in cases:
        for item in quality_checks.negative_controls(case):
            got = quality_checks.rubric(case, item["answer"])["score"]
            controls.append({"case_id": case["case_id"], "fact_id": item["fact_id"], "score": got, "must_score": 0})
    summary = {
        "collection_id": COLLECTION,
        "phase": "pilot",
        "not_final": True,
        "generation_reps": 1,
        "seed": SEED,
        "max_tokens": MAX_TOKENS,
        "temperature": 0,
        "model": "llama3.2",
        "token_count_source": "not_counted",
        "token_note": "Message and character counts come from the proxy body. They are not billed tokens and not a tokenizer count.",
        "negative_controls_failed": [c for c in controls if c["score"] != 0],
        "rows": preflight,
    }
    with open(os.path.join(OUT, "preflight.json"), "w", encoding="utf-8") as fh:
        json.dump(summary, fh, indent=2)
        fh.write("\n")
    print("cases", len(cases), "rows", len(preflight), "negative_control_failures", len(summary["negative_controls_failed"]))


if __name__ == "__main__":
    main()
