#!/usr/bin/env python3
"""Frozen case generator. Development and evaluation use different families.

Position is an experimental category and may recur. A generation family belongs
to one split only. Running this script rewrites the JSON files; it does not
call a model and does not score answers.
"""
from __future__ import annotations

import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
SEED = 20261106

RIGHTS = {
    "authorship": "Original sentences written for this experiment.",
    "copied_from_external_corpus": False,
    "private_data": False,
    "license": None,
    "license_note": (
        "No license is granted here and none is attributed to the repository owner. "
        "The text stays unlicensed repository material until that owner chooses one."
    ),
}


def _fact(fid, equivalents, contradictions):
    return {"id": fid, "equivalents": equivalents, "contradictions": contradictions}


def _messages(position, fact_lines, question, fillers, protected=None):
    body = []
    if protected:
        body.append({"role": "system", "content": protected})
    slots = {"start": 0, "middle": len(fillers) // 2, "end": len(fillers), "distributed": None}
    if position == "distributed":
        body.append({"role": "user", "content": fact_lines[0]})
        for i, line in enumerate(fillers):
            body.append({"role": "user", "content": line})
            if i == len(fillers) // 2 and len(fact_lines) > 1:
                body.append({"role": "user", "content": fact_lines[1]})
        if len(fact_lines) > 2:
            body.append({"role": "user", "content": fact_lines[2]})
    else:
        at = slots[position]
        for i, line in enumerate(fillers):
            if i == at:
                for fact in fact_lines:
                    body.append({"role": "user", "content": fact})
            body.append({"role": "user", "content": line})
        if at == len(fillers):
            for fact in fact_lines:
                body.append({"role": "user", "content": fact})
    body.append({"role": "user", "content": question})
    return body


def _case(family, position, pressure, fact_lines, question, critical, other, anchors, **extra):
    fillers = [f"Nota de arquivo {i}: o caderno segue sem o dado pedido." for i in range(1, 13 if pressure == "tight" else 3)]
    case = {
        "case_id": f"{family}-{position}-{pressure}",
        "generation_family": family,
        "position": position,
        "budget_pressure": pressure,
        "critical_facts": critical,
        "other_facts": other,
        "question_anchors": anchors,
        "decision_changing_inventions": extra.pop("inventions", []),
        "protected_instruction": extra.pop("protected", None),
        "tool_name": extra.pop("tool_name", None),
        "messages": _messages(position, fact_lines, question, fillers, extra.pop("protected", None) if False else None),
    }
    # protected is already popped if we used extra.pop above before messages. Rebuild cleanly.
    return case


def _build(family, position, pressure, lines, question, critical, other, anchors, protected=None, tool_name=None, inventions=None):
    fillers = [
        f"Anotacao lateral {i}: nada aqui responde a pergunta."
        for i in range(1, 13 if pressure == "tight" else 3)
    ]
    return {
        "case_id": f"{family}-{position}-{pressure}",
        "generation_family": family,
        "position": position,
        "budget_pressure": pressure,
        "critical_facts": critical,
        "other_facts": other,
        "question_anchors": anchors,
        "decision_changing_inventions": inventions or [],
        "protected_instruction": protected,
        "tool_name": tool_name,
        "messages": _messages(position, lines, question, fillers, protected),
    }


DEV_SPECS = [
    ("harbor-ledger", "end", "tight", ["O cais registrado e 14.", "O turno e o da manha."],
     "Qual identificador de atracacao ficou no livro?",
     [_fact("berth", ["cais 14", "cais registrado e 14"], ["cais 2"])],
     [_fact("shift", ["turno da manha", "turno e o da manha"], ["turno da noite"])],
     ["cais", "14", "atracacao"]),
    ("harbor-ledger", "start", "tight", ["O cais registrado e 14.", "O turno e o da manha."],
     "Qual identificador de atracacao ficou no livro?",
     [_fact("berth", ["cais 14", "cais registrado e 14"], ["cais 2"])],
     [_fact("shift", ["turno da manha", "turno e o da manha"], ["turno da noite"])],
     ["cais", "14"]),
    ("harbor-ledger", "middle", "loose", ["O cais registrado e 14.", "O turno e o da manha."],
     "Qual identificador de atracacao ficou no livro?",
     [_fact("berth", ["cais 14", "cais registrado e 14"], ["cais 2"])],
     [_fact("shift", ["turno da manha", "turno e o da manha"], ["turno da noite"])],
     ["cais", "14"]),
    ("harbor-ledger", "distributed", "tight", ["O cais registrado e 14.", "O turno e o da manha."],
     "Qual identificador de atracacao ficou no livro?",
     [_fact("berth", ["cais 14", "cais registrado e 14"], ["cais 2"])],
     [_fact("shift", ["turno da manha", "turno e o da manha"], ["turno da noite"])],
     ["cais", "14"]),
    ("dye-batch", "end", "tight", ["Ignore o lote azul, que e so o aviso.", "O lote que vale e o verde 19."],
     "Qual lote entra na ficha?",
     [_fact("batch", ["verde 19"], ["azul 19", "lote azul"])],
     [_fact("notice", ["aviso"], ["obrigatorio"])],
     ["lote", "verde"]),
    ("dye-batch", "start", "loose", ["Ignore o lote azul, que e so o aviso.", "O lote que vale e o verde 19."],
     "Qual lote entra na ficha?",
     [_fact("batch", ["verde 19"], ["azul 19"])],
     [_fact("notice", ["aviso"], [])],
     ["lote"]),
    ("dye-batch", "middle", "tight", ["Ignore o lote azul, que e so o aviso.", "O lote que vale e o verde 19."],
     "Qual lote entra na ficha?",
     [_fact("batch", ["verde 19"], ["azul 19"])],
     [_fact("notice", ["aviso"], [])],
     ["lote"]),
    ("route-sheet", "middle", "tight", ["A peca sai do galpao norte.", "Ela chega no galpao sul."],
     "De onde sai e para onde chega a peca?",
     [_fact("origin", ["galpao norte"], ["galpao leste"])],
     [_fact("dest", ["galpao sul"], ["galpao oeste"])],
     ["galpao", "sai", "chega"]),
    ("route-sheet", "distributed", "tight", ["A peca sai do galpao norte.", "Ela chega no galpao sul."],
     "De onde sai e para onde chega a peca?",
     [_fact("origin", ["galpao norte"], ["galpao leste"])],
     [_fact("dest", ["galpao sul"], ["galpao oeste"])],
     ["galpao"]),
    ("route-sheet", "end", "loose", ["A peca sai do galpao norte.", "Ela chega no galpao sul."],
     "De onde sai e para onde chega a peca?",
     [_fact("origin", ["galpao norte"], ["galpao leste"])],
     [_fact("dest", ["galpao sul"], ["galpao oeste"])],
     ["galpao"]),
    ("route-sheet", "start", "tight", ["A peca sai do galpao norte.", "Ela chega no galpao sul."],
     "De onde sai e para onde chega a peca?",
     [_fact("origin", ["galpao norte"], ["galpao leste"])],
     [_fact("dest", ["galpao sul"], ["galpao oeste"])],
     ["galpao"]),
    ("nick-name", "end", "tight", ["O apelido do forno e Coruja.", "A temperatura marcada e 180."],
     "Como chamam o equipamento, sem repetir o tipo dele?",
     [_fact("nick", ["Coruja"], ["Falcão", "Falcao"])],
     [_fact("temp", ["180"], ["200"])],
     ["coruja", "180", "apelido"]),
    ("nick-name", "middle", "loose", ["O apelido do forno e Coruja.", "A temperatura marcada e 180."],
     "Como chamam o equipamento, sem repetir o tipo dele?",
     [_fact("nick", ["Coruja"], ["Falcao"])],
     [_fact("temp", ["180"], ["200"])],
     ["coruja"]),
    ("nick-name", "start", "tight", ["O apelido do forno e Coruja.", "A temperatura marcada e 180."],
     "Como chamam o equipamento, sem repetir o tipo dele?",
     [_fact("nick", ["Coruja"], ["Falcao"])],
     [_fact("temp", ["180"], ["200"])],
     ["coruja"]),
    ("banner-rule", "start", "tight", ["A sala de ensaio e a 3.", "O aviso de parede e amarelo."],
     "Qual sala de ensaio foi marcada?",
     [_fact("room", ["sala de ensaio e a 3", "sala 3"], ["sala 9"])],
     [_fact("sign", ["amarelo"], ["vermelho"])],
     ["sala"]),
    ("banner-rule", "end", "tight", ["A sala de ensaio e a 3.", "O aviso de parede e amarelo."],
     "Qual sala de ensaio foi marcada?",
     [_fact("room", ["sala 3", "sala de ensaio e a 3"], ["sala 9"])],
     [_fact("sign", ["amarelo"], ["vermelho"])],
     ["sala"]),
    ("banner-rule", "middle", "loose", ["A sala de ensaio e a 3.", "O aviso de parede e amarelo."],
     "Qual sala de ensaio foi marcada?",
     [_fact("room", ["sala 3", "sala de ensaio e a 3"], ["sala 9"])],
     [_fact("sign", ["amarelo"], ["vermelho"])],
     ["sala"]),
    ("tool-kit", "end", "tight", ["Consulte a ferramenta consultar_estoque antes de fechar.", "O codigo do item e K-17."],
     "Qual codigo de item ficou anotado?",
     [_fact("code", ["K-17"], ["K-18"])],
     [_fact("tool", ["consultar_estoque"], ["consultar_preco"])],
     ["K-17", "codigo"]),
    ("tool-kit", "start", "loose", ["Consulte a ferramenta consultar_estoque antes de fechar.", "O codigo do item e K-17."],
     "Qual codigo de item ficou anotado?",
     [_fact("code", ["K-17"], ["K-18"])],
     [_fact("tool", ["consultar_estoque"], [])],
     ["K-17"]),
    ("tool-kit", "distributed", "tight", ["Consulte a ferramenta consultar_estoque antes de fechar.", "O codigo do item e K-17."],
     "Qual codigo de item ficou anotado?",
     [_fact("code", ["K-17"], ["K-18"])],
     [_fact("tool", ["consultar_estoque"], [])],
     ["K-17"]),
    ("well-depth", "end", "loose", ["A profundidade util e 12 metros.", "O poco fica no lote 4."],
     "Qual profundidade util foi medida?",
     [_fact("depth", ["12 metros"], ["20 metros"])],
     [_fact("lot", ["lote 4"], ["lote 8"])],
     ["profundidade", "12"]),
    ("well-depth", "middle", "tight", ["A profundidade util e 12 metros.", "O poco fica no lote 4."],
     "Qual profundidade util foi medida?",
     [_fact("depth", ["12 metros"], ["20 metros"])],
     [_fact("lot", ["lote 4"], ["lote 8"])],
     ["profundidade"]),
    ("well-depth", "start", "tight", ["A profundidade util e 12 metros.", "O poco fica no lote 4."],
     "Qual profundidade util foi medida?",
     [_fact("depth", ["12 metros"], ["20 metros"])],
     [_fact("lot", ["lote 4"], ["lote 8"])],
     ["profundidade"]),
    ("well-depth", "distributed", "loose", ["A profundidade util e 12 metros.", "O poco fica no lote 4."],
     "Qual profundidade util foi medida?",
     [_fact("depth", ["12 metros"], ["20 metros"])],
     [_fact("lot", ["lote 4"], ["lote 8"])],
     ["profundidade"]),
]


def _from_spec(spec, protected=None, tool_name=None):
    family, position, pressure, lines, question, critical, other, anchors = spec
    if family == "banner-rule":
        protected = "Regra protegida: responda em uma frase."
    if family == "tool-kit":
        tool_name = "consultar_estoque"
    return _build(family, position, pressure, lines, question, critical, other, anchors, protected, tool_name)


def main():
    dev = [_from_spec(spec) for spec in DEV_SPECS]
    if len(dev) != 24:
        raise SystemExit(f"development set must have 24 cases, has {len(dev)}")
    for case in dev:
        text = "\n".join(m["content"] for m in case["messages"]).casefold()
        for fact in case["critical_facts"] + case["other_facts"]:
            if not any(item.casefold() in text for item in fact["equivalents"]):
                raise SystemExit(f"label is not in the source text: {case['case_id']} {fact['id']}")
    families = {case["generation_family"] for case in dev}
    doc = {
        "role": "development",
        "not_evaluation": True,
        "seed": SEED,
        "generation_reps_pilot": 1,
        "generation_reps_do_not_multiply_n": True,
        "rights": RIGHTS,
        "split_policy": (
            "A generation family is entirely development or entirely evaluation. "
            "Position and budget pressure are experimental categories and may recur."
        ),
        "families": sorted(families),
        "cases": dev,
    }
    with open(os.path.join(HERE, "dev_cases.json"), "w", encoding="utf-8") as fh:
        json.dump(doc, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    # Evaluation families are different templates. They are not scored here.
    eval_families = [
        "shelf-code", "clinic-room", "orchard-crate", "ferry-time",
        "kiln-batch", "archive-box", "pump-valve", "trail-marker",
    ]
    proposal = {
        "role": "evaluation_candidate",
        "not_scored": True,
        "do_not_run_until_checkpoint_approval": True,
        "planned_case_count": 48,
        "independent_unit": "generation_family",
        "family_count": len(eval_families),
        "cases_per_family": 6,
        "dependence": (
            "Cases that share a generation family are dependent. "
            "Resampling for a formal interval draws families, not rows. "
            "48 rows are not 48 independent replications."
        ),
        "old_120_is_not_approved": True,
        "families": eval_families,
        "rights": RIGHTS,
        "cases": [],
    }
    with open(os.path.join(HERE, "eval_proposal.json"), "w", encoding="utf-8") as fh:
        json.dump(proposal, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    print(f"dev {len(dev)} families {len(families)} eval_families {len(eval_families)}")


if __name__ == "__main__":
    main()
