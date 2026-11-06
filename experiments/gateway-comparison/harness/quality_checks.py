#!/usr/bin/env python3
"""Automatic checks for controlled facts. Not a human evaluation.

Equivalence is case-insensitive substring of a listed equivalent. A listed
contradiction is a different event from absence. Labels stay with the checker;
nothing here is sent to a compactor.
"""
from __future__ import annotations


def _fold(text: str) -> str:
    return (text or "").casefold()


def fact_present(text: str, fact: dict) -> bool:
    hay = _fold(text)
    return any(_fold(item) in hay for item in fact.get("equivalents") or [])


def fact_contradicted(text: str, fact: dict) -> bool:
    hay = _fold(text)
    return any(_fold(item) in hay for item in fact.get("contradictions") or [])


def _facts(case: dict):
    for fact in case.get("critical_facts") or []:
        yield "critical", fact
    for fact in case.get("other_facts") or []:
        yield "other", fact


def addresses_question(case: dict, answer: str) -> bool:
    anchors = case.get("question_anchors") or []
    if not anchors:
        return any(fact_present(answer, fact) for _role, fact in _facts(case))
    hay = _fold(answer)
    return any(_fold(item) in hay for item in anchors)


def rubric(case: dict, answer: str) -> dict:
    """0–3 with no overlap between 1 and 2.

    3: every required fact is present and none is contradicted.
    2: every critical fact is present and uncontradicted, at least one other
       required fact is absent, and no required fact is contradicted.
    1: every critical fact is present and uncontradicted, the answer addresses
       the question, and a non-critical fact is contradicted.
    0: a critical fact is absent or contradicted, the answer does not address
       the question, or a listed decision-changing invention is present.
    """
    critical_missing = []
    critical_contradicted = []
    other_missing = []
    other_contradicted = []
    for role, fact in _facts(case):
        present = fact_present(answer, fact)
        contradicted = fact_contradicted(answer, fact)
        if role == "critical" and not present:
            critical_missing.append(fact["id"])
        if role == "critical" and contradicted:
            critical_contradicted.append(fact["id"])
        if role == "other" and not present:
            other_missing.append(fact["id"])
        if role == "other" and contradicted:
            other_contradicted.append(fact["id"])
    invented = fact_present(answer, {"equivalents": case.get("decision_changing_inventions") or []})
    addressed = addresses_question(case, answer)
    if critical_missing or critical_contradicted or invented or not addressed:
        score = 0
        rule = "critical_failure_or_irrelevant"
    elif other_contradicted:
        score = 1
        rule = "noncritical_contradiction"
    elif other_missing:
        score = 2
        rule = "noncritical_omission"
    else:
        score = 3
        rule = "all_required_facts"
    return {
        "score": score,
        "rule": rule,
        "critical_missing": critical_missing,
        "critical_contradicted": critical_contradicted,
        "other_missing": other_missing,
        "other_contradicted": other_contradicted,
        "addresses_question": addressed,
        "evaluator_type": "automatic",
    }


def channel_rows(case: dict, text: str, channel: str) -> list[dict]:
    rows = []
    for role, fact in _facts(case):
        rows.append({
            "fact_id": fact["id"],
            "fact_role": role,
            "channel": channel,
            "expected_present": True,
            "observed_present": fact_present(text, fact),
            "equivalence_rule": "casefold_substring",
        })
    if case.get("protected_instruction"):
        rows.append({
            "fact_id": "protected_instruction",
            "fact_role": "protected",
            "channel": channel,
            "expected_present": channel == "forwarded_context",
            "observed_present": _fold(case["protected_instruction"]) in _fold(text),
            "equivalence_rule": "casefold_substring",
        })
    if case.get("tool_name"):
        rows.append({
            "fact_id": "tool_name",
            "fact_role": "tool",
            "channel": channel,
            "expected_present": channel == "forwarded_context",
            "observed_present": _fold(case["tool_name"]) in _fold(text),
            "equivalence_rule": "casefold_substring",
        })
    return rows


def negative_controls(case: dict) -> list[dict]:
    """Answers that must not receive a passing automatic score."""
    wrong = []
    for _role, fact in _facts(case):
        for bad in fact.get("contradictions") or []:
            if fact.get("role_hint") == "other":
                continue
            if any(f.get("id") == fact["id"] for f in case.get("critical_facts") or []):
                wrong.append({"answer": bad, "must_score": 0, "fact_id": fact["id"]})
    return wrong
