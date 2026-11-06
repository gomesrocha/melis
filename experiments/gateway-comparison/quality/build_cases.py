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


def _build(family, position, pressure, lines, question, critical, other, anchors, protected=None, tool_name=None, inventions=None, filler_stem=None):
    stem = filler_stem or "Anotacao lateral {i}: nada aqui responde a pergunta."
    count = 12 if pressure == "tight" else 2
    fillers = [stem.format(i=i) for i in range(1, count + 1)]
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


EVAL_GRID = (
    ("start", "tight"),
    ("middle", "tight"),
    ("end", "tight"),
    ("distributed", "tight"),
    ("middle", "loose"),
    ("end", "loose"),
)

# Each family has its own sentences. None of these lines is a development sentence.
EVAL_FAMILIES = [
    {
        "family": "shelf-code",
        "filler": "Etiqueta solta {i}: este adesivo nao indica o lugar.",
        "lines": [
            "A prateleira marcada no inventario e a B7.",
            "O corredor desse inventario e o leste.",
            "A etiqueta antiga citava a prateleira B1, mas foi rasurada.",
        ],
        "question": "Onde o inventario manda guardar a caixa, deixando de lado o adesivo rasurado?",
        "critical": [_fact("shelf", ["prateleira marcada no inventario e a B7", "B7"], ["prateleira B1"])],
        "other": [_fact("aisle", ["corredor desse inventario e o leste", "leste"], ["corredor oeste"])],
        "anchors": ["inventario", "caixa"],
    },
    {
        "family": "clinic-room",
        "filler": "Recado de corredor {i}: este papel nao marca a reserva.",
        "lines": [
            "A sala de curativo reservada hoje e a de numero 12.",
            "O turno dessa reserva e o vespertino.",
            "A sala 4 aparece no mapa, porem nao entra na reserva de hoje.",
        ],
        "question": "Qual numero ficou na reserva de curativo de hoje?",
        "critical": [_fact("room", ["numero 12", "sala de curativo reservada hoje e a de numero 12"], ["sala 4"])],
        "other": [_fact("shift", ["turno dessa reserva e o vespertino", "vespertino"], ["noturno"])],
        "anchors": ["reserva", "curativo"],
    },
    {
        "family": "orchard-crate",
        "filler": "Folha de caderno {i}: esta linha nao descreve a caixa da feira.",
        "lines": [
            "A caixa que segue para a feira e a de macas.",
            "A quantidade contada nessa caixa e 30.",
            "As peras ficam na caixa ao lado e nao vao para a feira.",
        ],
        "question": "O que segue para a feira e quantas unidades foram contadas?",
        "critical": [_fact("fruit", ["caixa que segue para a feira e a de macas", "macas"], ["peras"])],
        "other": [_fact("count", ["quantidade contada nessa caixa e 30", "30"], ["40"])],
        "anchors": ["feira", "unidades"],
    },
    {
        "family": "ferry-time",
        "filler": "Aviso de deck {i}: este aviso nao marca o horario pedido.",
        "lines": [
            "A balsa da manha parte as 06h10.",
            "A rampa dessa partida e a rampa norte.",
            "A balsa da tarde parte as 16h40 e nao marca a partida da manha.",
        ],
        "question": "Quando parte a travessia da manha?",
        "critical": [_fact("departure", ["parte as 06h10", "06h10"], ["16h40"])],
        "other": [_fact("ramp", ["rampa dessa partida e a rampa norte", "rampa norte"], ["rampa sul"])],
        "anchors": ["travessia", "manha"],
    },
    {
        "family": "kiln-batch",
        "filler": "Bilhete de forno {i}: este bilhete nao libera fornada.",
        "lines": [
            "A fornada liberada para sair e a segunda.",
            "O operador dessa fornada assina com o nome Lina.",
            "A primeira fornada ficou retida e nao pode sair.",
        ],
        "question": "Qual fornada pode deixar o forno?",
        "critical": [_fact("batch", ["fornada liberada para sair e a segunda", "segunda"], ["primeira fornada"])],
        "other": [_fact("operator", ["operador dessa fornada assina com o nome Lina", "Lina"], ["Rita"])],
        "anchors": ["fornada", "forno"],
    },
    {
        "family": "archive-box",
        "filler": "Ficha avulsa {i}: esta ficha nao guarda o codigo.",
        "lines": [
            "A caixa do arquivo morto leva o codigo AZ-4.",
            "Essa caixa fica na estante inferior.",
            "A caixa AZ-9 foi descartada e nao esta no arquivo morto.",
        ],
        "question": "Qual codigo identifica a caixa guardada no arquivo morto?",
        "critical": [_fact("code", ["codigo AZ-4", "AZ-4"], ["AZ-9"])],
        "other": [_fact("shelf", ["estante inferior"], ["estante superior"])],
        "anchors": ["codigo", "arquivo"],
        "protected": "Mantenha a resposta em uma unica linha.",
    },
    {
        "family": "pump-valve",
        "filler": "Nota de valvula {i}: esta nota nao traz a leitura.",
        "lines": [
            "Antes de fechar o registro, use a ferramenta ler_pressao.",
            "A pressao anotada no visor e 3 bar.",
            "O visor ao lado, que nao vale, marcava 5 bar.",
        ],
        "question": "O que o visor valido mostrou na leitura?",
        "critical": [_fact("pressure", ["pressao anotada no visor e 3 bar", "3 bar"], ["5 bar"])],
        "other": [_fact("tool", ["ler_pressao"], ["ler_vazao"])],
        "anchors": ["visor", "leitura"],
        "tool_name": "ler_pressao",
    },
    {
        "family": "trail-marker",
        "filler": "Placa caiada {i}: esta placa nao aponta o abrigo.",
        "lines": [
            "O marco que indica a trilha certa e o marco verde.",
            "A distancia desse marco ate o abrigo e de 2 quilometros.",
            "O marco vermelho aponta para o desvio e nao para a trilha certa.",
        ],
        "question": "Qual cor guia quem procura o abrigo?",
        "critical": [_fact("marker", ["marco que indica a trilha certa e o marco verde", "marco verde"], ["marco vermelho"])],
        "other": [_fact("distance", ["2 quilometros"], ["5 quilometros"])],
        "anchors": ["cor", "abrigo"],
    },
]


def _eval_cases():
    cases = []
    for family in EVAL_FAMILIES:
        for position, pressure in EVAL_GRID:
            cases.append(_build(
                family["family"], position, pressure, family["lines"], family["question"],
                family["critical"], family["other"], family["anchors"],
                protected=family.get("protected"), tool_name=family.get("tool_name"),
                filler_stem=family["filler"],
            ))
    return cases


def _assert_labels(cases, require_long_form=False):
    for case in cases:
        text = "\n".join(m["content"] for m in case["messages"]).casefold()
        question = case["messages"][-1]["content"].casefold()
        for fact in case["critical_facts"] + case["other_facts"]:
            if not any(item.casefold() in text for item in fact["equivalents"]):
                raise SystemExit(f"label is not in the source text: {case['case_id']} {fact['id']}")
            if require_long_form:
                long_forms = [item for item in fact["equivalents"] if len(item) >= 8]
                if not any(item.casefold() in text for item in long_forms):
                    raise SystemExit(f"label has no long form in the source: {case['case_id']} {fact['id']}")
        if require_long_form:
            for fact in case["critical_facts"]:
                for item in fact["equivalents"]:
                    if item.casefold() in question:
                        raise SystemExit(f"question repeats a critical equivalent: {case['case_id']} {item}")
        if case.get("protected_instruction") and case["protected_instruction"].casefold() not in text:
            raise SystemExit(f"protected instruction missing from source: {case['case_id']}")
        if case.get("tool_name") and case["tool_name"].casefold() not in text:
            raise SystemExit(f"tool name missing from source: {case['case_id']}")


def _assert_split(dev, evaluation):
    dev_families = {case["generation_family"] for case in dev}
    eval_families = {case["generation_family"] for case in evaluation}
    if dev_families & eval_families:
        raise SystemExit(f"family overlap: {sorted(dev_families & eval_families)}")
    dev_lines = {m["content"].casefold() for case in dev for m in case["messages"]}
    for case in evaluation:
        for message in case["messages"]:
            if message["content"].casefold() in dev_lines:
                raise SystemExit(f"evaluation reuses a development sentence: {case['case_id']}")


def _from_spec(spec, protected=None, tool_name=None):
    family, position, pressure, lines, question, critical, other, anchors = spec
    if family == "banner-rule":
        protected = "Regra protegida: responda em uma frase."
    if family == "tool-kit":
        tool_name = "consultar_estoque"
    return _build(family, position, pressure, lines, question, critical, other, anchors, protected, tool_name)


def main():
    dev = [_from_spec(spec) for spec in DEV_SPECS]
    evaluation = _eval_cases()
    if len(dev) != 24:
        raise SystemExit(f"development set must have 24 cases, has {len(dev)}")
    if len(evaluation) != 48:
        raise SystemExit(f"evaluation set must have 48 cases, has {len(evaluation)}")
    _assert_labels(dev)
    _assert_labels(evaluation, require_long_form=True)
    _assert_split(dev, evaluation)
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
        "written_cases": "quality/eval_cases.json",
    }
    evaluation_doc = {
        "role": "evaluation",
        "frozen_for_execution": True,
        "not_development": True,
        "seed": SEED,
        "generation_reps": 1,
        "generation_reps_do_not_multiply_n": True,
        "independent_unit": "generation_family",
        "family_count": len(eval_families),
        "cases_per_family": 6,
        "grid": [list(item) for item in EVAL_GRID],
        "grid_note": "The six cells are start/middle/end/distributed at tight pressure, plus middle and end at loose pressure. start-loose and distributed-loose are not in this grid.",
        "dependence": proposal["dependence"],
        "rights": RIGHTS,
        "families": eval_families,
        "cases": evaluation,
    }
    with open(os.path.join(HERE, "eval_cases.json"), "w", encoding="utf-8") as fh:
        json.dump(evaluation_doc, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    with open(os.path.join(HERE, "eval_proposal.json"), "w", encoding="utf-8") as fh:
        json.dump(proposal, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    print(f"dev {len(dev)} families {len(families)} eval {len(evaluation)} eval_families {len(eval_families)}")


if __name__ == "__main__":
    main()
