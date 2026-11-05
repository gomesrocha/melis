# Releitura da coleta `col-33439a62b8ab`

A coleta confirmatória continua bloqueada. Este arquivo descreve como
reproduzir a auditoria dos dados já gravados. Não inicia coleta.

O diretório original fica só para leitura:

```
experiments/gateway-comparison/data/pilot-variance-20261105T1750
```

No checkout em que essa pasta existe:

```
PYTHONDONTWRITEBYTECODE=1 python3 harness/audit_collection.py \
  --data-dir data/pilot-variance-20261105T1750 \
  --out-dir /caminho/novo/audit-col-33439a62b8ab \
  --process-wall-s 3191.249
```

`--process-wall-s` é a parede observada do processo, 3191,249 s, de
2026-11-05T20:48:16Z a 2026-11-05T21:41:27Z. Sem esse valor a coluna de
overhead de troca fica vazia. O script recusa escrever dentro do diretório
de origem.

A meia-largura é o intervalo t marginal de 95%. A sensibilidade de sigma é
o limite superior de 95% da qui-quadrado com df = 11. N e as metas de
0,50 ms e 0,25 ms não estão aprovados. Holm não torna o intervalo marginal
simultâneo.

Testes dos módulos novos, a partir de `harness/tests`:

```
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest test_provenance.py -q
```
