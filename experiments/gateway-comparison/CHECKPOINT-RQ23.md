# Checkpoint RQ2 / RQ3 — 2026-11-06

Coleta final não executada. `protocol/rq23-final-candidate.json` está com `execution_authorized: false`. RQ1 permanece a coleta `col-3bcdfa2f0aeb`, análise primária com 15 blocos, célula mínima, JSON, concorrência 1. Este bloco não a repetiu.

Os pilotos abaixo são desenvolvimento. Nenhum entra no conjunto final.

## Matriz

| RQ | Alegação que o desenho sustenta | Evidência existente | Evidência faltante | Comparador | Unidade | CSV |
|---|---|---|---|---|---|---|
| RQ1 | Overhead na célula mínima, JSON, concorrência 1 | Avaliação final, serviço real, n=15 | SSE, concorrência maior, saturação e backend de 100 ms, se a redação os exigir | Melis e LiteLLM contra direto | bloco | `requests.csv` da coleta já auditada |
| RQ2 | Estimativa pareada de retenção e de resposta sob um orçamento comum | Piloto, modelo local, 24 casos de desenvolvimento | Conjunto de avaliação, duas avaliações humanas | Direto sem compactação; Melis sem compactação é controle de mediação | família de geração | `quality.csv`, `facts.csv` |
| RQ3 | Comportamento de um cliente de diagnóstico sob agenda controlada | Piloto, mock, 5 episódios por cenário | Comparação Melis contra LiteLLM com mecanismos alinhados | Direto diagnostica o backend | episódio | `episodes.csv`, `resilience_events.csv` |
| Cache | Contrato de chave, identidade e falha aberta | Testes de unidade já no artefato | Execução rotulada de paráfrase e negativo | Limiar proposto 0,92, não ajustado | caso | `quality/cache_dev_labels.json` |
| Guardrails | Contrato de bloqueio, redação e borda de chunk | Testes de unidade já no artefato, inclusive o prefixo que não pode ser recolhido | Nova campanha não foi aberta | Conteúdo recebido pelo cliente no teste | caso | sem CSV novo |

Uma suíte G1 verde não substitui qualidade de resposta nem recuperação sob falha.

## O que foi executado

Instrumentação do esquema 1.3.0: `facts.csv` e `episodes.csv`. Arquivos 1.2.0 continuam válidos. `aggregate.py` não mudou; o identificador confirmatório segue `agg-1.2.0-19978d3e1d10`.

Testes locais: `harness/tests/test_rq23.py` e `instrumentation/tests/test_instrumentation.py`, 24 testes, OK. A rubrica separa nota 1 (contradição não crítica) de nota 2 (omissão não crítica). Controles negativos do gerador receberam nota 0 em todos os casos do piloto de registro.

RQ2, piloto de registro `col-rq2-dev-pilot-20261106-relabel`, 24 casos, 1 geração, semente 20261106, `llama3.2` local, temperatura 0, `max_tokens` 64. Quatro braços: direto, Melis `none`, `sliding_window` (cronológico), `semantic_guarded_trimming` sem `semantic_scoring`. Imagem Melis `sha256:79adb313b506…` numa porta separada (`17071`); o contêiner de RQ1 não foi reiniciado. Cache e guardrails desligados. Orçamento comum das políticas compactadoras: `compress_above_tokens` 80. Duração observada: 420 s. Validação de esquema: ok.

Uma execução anterior, no diretório `rq2-pilot-dev-20261106`, ficou marcada `SUPERSEDED.json`. Quatro rótulos não eram substrings do texto-fonte. O gerador foi corrigido e agora recusa esse caso. Esse diretório não é o piloto de registro.

RQ3, piloto `col-rq3-dev-pilot-20261106`: 6 cenários, 5 episódios, clientes `direct` e `aligned_failover`, prazo 1000 ms, mock em porta privada. Duração observada: 11 s. Validação de esquema: ok. `aligned_failover` tenta o destino de reserva uma vez e não repete o mesmo destino. Esse cliente não é Melis nem LiteLLM.

## Preflight de compactação

No piloto de registro, direto e Melis `none` encaminharam o contexto integral nos 24 casos: 0 truncamentos. As duas políticas compactadoras reduziram os 16 casos apertados e não reduziram os 8 casos folgados, que ficaram abaixo do orçamento. Fato crítico no contexto encaminhado: direto 24/24, Melis `none` 24/24, cronológico 13/24, heurística 17/24. Instrução protegida: 3/3 nos quatro braços. Nome de ferramenta no contexto: direto e `none` 3/3, cronológico 2/3, heurística 1/3. O corpo HTTP deste piloto não envia um array `tools`; a preservação medida é a do nome no texto.

Contagem de tokens: ausente. O proxy registrou mensagens e caracteres em `preflight.json`. Isso não é token faturado nem contagem de tokenizador. `scoring_ms` e `compaction_ms` ficam vazios; a latência total os inclui quando ocorrem e não os separa.

## Respostas no piloto

Fato crítico presente na resposta: direto 9/24, Melis `none` 9/24, cronológico 5/24, heurística 7/24. A rubrica 0–3 é descritiva. Médias: direto e `none` 0,833; cronológico e heurística 0,417. Não houve nota 1. Diferença pareada do fato crítico na resposta, braço menos direto, média dos 24 casos: `none` 0; cronológico −0,167; heurística −0,083. A mesma heurística na média das 7 famílias: −0,095. Sete famílias não são 24 réplicas. Não há intervalo, teste, nem alegação de não inferioridade. δ=0,25 continua sem aprovação.

Embedding e híbrida não entraram: o perfil congelado deste piloto não as configura. LiteLLM sem compactação não é braço de compactação.

## RQ3 observado

Dentro do prazo de 1000 ms, em 5 de 5 episódios: saudável nos dois clientes; 429, 5xx e primário indisponível só no cliente com reserva. Direto falha nesses três, com uma chamada de backend. O cliente com reserva conclui com duas chamadas. No atraso de 1500 ms os dois ficam fora do prazo e a recuperação é censurada, não zero; o cliente com reserva ainda emite a segunda chamada e o 200 chega depois do prazo. No cenário sem destino saudável, os dois falham a tentativa lógica e a sonda posterior observa recuperação. A ordem dos clientes alternou. A variância dentro do cenário foi zero neste mock determinístico.

A imagem LiteLLM fixada é `ghcr.io/berriai/litellm@sha256:6c82d338a60e…`, comentada no compose como v1.83.10. A configuração determinística tem `cache: false`, `num_retries: 0`, `request_timeout: 600` e não declara fallbacks. Isso registra a configuração usada, não uma ausência de recurso do produto. Melis, no caminho OpenAI já presente no código, troca de provedor em 429 em vez de dormir em `Retry-After`. A comparação de produto não foi executada.

Duas réplicas e Redis indisponível continuam validação arquitetural do Melis. Não houve configuração LiteLLM correspondente neste bloco, então não houve disputa.

## Cache e guardrails

Não houve campanha nova. Evidência de contrato já no artefato: isolamento de identidade e de destino em `semantic_cache.rs`; falha aberta quando o embedding não está disponível em `integration_tests.rs`; bloqueio e redação entre chunks, e o prefixo já emitido que não pode ser recolhido, em `streaming.rs`. Esses testes não foram reexecutados aqui e não são avaliação final.

`quality/cache_dev_labels.json` define, antes de qualquer conjunto final, uma paráfrase elegível e um negativo próximo com resposta diferente. O limiar cosseno candidato é 0,92 e não foi ajustado. Não há hit observado, então não há `safety_cases.csv` desta rodada e não há p-valor.

## Duas opções para a decisão

1. Estimação do trade-off. Desfecho proposto: diferença pareada da presença do fato crítico na resposta, heurística menos direto. A retenção no contexto encaminhado e a rubrica 0–3 ficam como descrições separadas. Incerteza por reamostragem de família, não de linha. Sem margem e sem não inferioridade.
2. Não inferioridade. O desfecho, a margem prática e o tamanho ainda precisam de aprovação. δ=0,25 não está aprovado. Oito famílias não são um estudo dimensionado para essa margem. O piloto não fornece esse cálculo.

## Grade final, se aprovada

Obrigatório para manter a alegação no artigo:

- RQ2: escrever os 48 casos das 8 famílias já nomeadas, sem reutilizar família de desenvolvimento. Uma geração por caso. Quatro braços. No ritmo observado, cerca de 14 minutos no Ollama local. Comando ainda não executado.
- RQ3: alinhar Melis e LiteLLM no mesmo objetivo, prazo, máximo de tentativas e destinos, e registrar o contraste de mecanismo. Só então os seis cenários. O piloto de 11 segundos não justifica multiplicar o mock.

Opcional, e fora do encerramento enquanto a alegação não os exigir: embedding, híbrida, LiteLLM como controle de mediação sem compactação, avaliação humana do export cego, execução ao vivo do cache rotulado, SSE, concorrência maior, saturação, backend de 100 ms.

## Dependências humanas

Dois avaliadores não estão disponíveis. `blinded_export.jsonl` tem 96 respostas com `rater_1` e `rater_2` nulos. Um juiz LLM não foi usado.

A decisão única pedida é a opção 1 ou a opção 2, mais a autorização para escrever e então executar o conjunto de avaliação. Até lá os comandos finais permanecem no protocolo candidato e não devem ser disparados.
