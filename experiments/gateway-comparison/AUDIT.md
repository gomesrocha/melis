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
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest test_provenance.py test_final_protocol.py -q
```

O protocolo 1.0.0 preservado está em `protocol/rq1-final-candidate.json`.
A emenda de host compartilhado está em `protocol/rq1-final-amendment-1.1.0.json`,
SHA-256 `953e5bfee33803b883f163ba6696e34ec7de6ab7064d72c3666ebcbf329985b0`.
A versão autorizada está em `protocol/rq1-final-authorized-1.2.0.json`,
SHA-256 `93f7a11a3d2d55668e2c461d21a418644998a80c776e68b2eba76a688d5335d0`,
com `authorizes_sha256` igual ao hash da emenda e `execution_authorized` verdadeiro.
O hash
desse arquivo é a identidade; o sidecar `.sha256` só a reproduz. `--dry-run`
imprime 54 células e 4320 s de carga, com o overhead de troca à parte, e
não amostra o host nem sobe contêiner. A flag
`--confirm-operational-start` não autoriza a coleta. A admissão de 60 s só
corre numa execução real, imediatamente antes da primeira carga, e não
reaproveita a recusa de 2026-11-05. Para ver a imagem local, sem subir
nada: `docker image inspect gateway-comparison-melis:latest`. O id esperado
é `sha256:79adb313b5060df96c6e5218bb108a289cd7d17c1f1fa08df9b221a6232c43bb`.
Depois que um contêiner for criado, o campo Image tem de ser esse id. O
contêiner parado `gateway-comparison-melis-1` ainda é `f87640f8e886`.
