# Relatório das coletas RQ1, RQ2 e RQ3

A coleta final de RQ3 terminou com 360 episódios e zero falhas de execução. O contraste pareado entre Melis e LiteLLM tem N = 20 episódios por cenário. Os 120 episódios diretos ficam fora desse N. Em todos os 20 pares de cada cenário os dois gateways concordaram, então a diferença observada é zero. Isso é uma contagem, não um teste e não uma prova de equivalência.

## RQ1

Overhead na célula mínima, JSON, concorrência 1. A seleção primária continua com n = 15 blocos. Os blocos 5, 7 e 8 permanecem `uncertain_held_out`. A meia-largura de 0,50 ms é alvo de planejamento, não margem de equivalência. Não há cálculo retrospectivo de tamanho de amostra. A recomputação dos 18 blocos é descritiva e não substitui a análise primária.

Meias-larguras primárias, em milissegundos:

| Contraste | Meia-largura | Atinge 0,50 ms |
| --- | ---: | --- |
| Melis − direto | 0,153 | sim |
| LiteLLM − direto | 1,620 | não |
| Melis − LiteLLM | 1,619 | não |

A ausência de `gateway_memory_delta_bytes` nas 156 linhas é a limitação conhecida do monitor construído sem leitor de memória, registrada antes da carga. Não é evidência de interferência externa.

## RQ2

Trade-off observado na presença do fato crítico na resposta. A estimativa primária, heurística menos direto, é −0,2708333333333333, cerca de −27,1 pontos percentuais. O intervalo percentil é −0,3958333333333333 a −0,14583333333333331. A seleção da política não muda depois desse resultado. O resultado não é não inferioridade, não é preservação da qualidade e não é ausência de perda.

A unidade independente são as oito famílias. Os 48 casos entram primeiro na média de cada família. O intervalo reamostra as oito médias, com reposição, 2000 vezes, semente 20261106. A amostra ordenada usa o índice `int(0,025 × 2000) − 1` = 49 e o índice `int(0,975 × 2000) − 1` = 1949. Não há valor-p nem margem.

Diferenças por família, antes da média final, no indicador da resposta:

| Família | Direto | Heurística | Cronológica | Melis sem compactação | Heurística − direto |
| --- | ---: | ---: | ---: | ---: | ---: |
| archive-box | 1,000 | 0,667 | 0,333 | 1,000 | −0,333 |
| clinic-room | 0,000 | 0,000 | 0,000 | 0,000 | 0,000 |
| ferry-time | 0,833 | 0,833 | 0,500 | 0,833 | 0,000 |
| kiln-batch | 0,667 | 0,333 | 0,333 | 0,667 | −0,333 |
| orchard-crate | 0,333 | 0,000 | 0,000 | 0,333 | −0,333 |
| pump-valve | 0,667 | 0,500 | 0,500 | 0,833 | −0,167 |
| shelf-code | 0,833 | 0,333 | 0,333 | 0,833 | −0,500 |
| trail-marker | 0,500 | 0,000 | 0,333 | 0,667 | −0,500 |
| Média não ponderada |  |  |  |  | −0,271 |

Contrastes secundários, já declarados, exploratórios: cronológica − direto −0,3125; Melis sem compactação − direto +0,0417; fato crítico no contexto encaminhado, heurística − direto −0,4792 e cronológica − direto −0,6042. Não há hipótese nova nem teste contra a cronológica.

A redução de contexto da heurística foi 7,0417 mensagens e 361,2292 caracteres. A da cronológica foi 7,0833 mensagens e 365,25 caracteres. São contagens do corpo visto pelo proxy. Não são tokens medidos e não comprovam economia financeira.

O avaliador de presença é a função `fact_present`: um fato está presente quando algum equivalente listado ocorre como substring, com `casefold`. Contradição é outra lista e não se confunde com ausência. O caso vale 1 quando todos os fatos críticos estão presentes na resposta. Os controles negativos são respostas formadas por uma contradição listada de um fato crítico; a rubrica automática tem de dar 0. A coleção registrou `negative_controls_failed` vazio. A rubrica humana continua sem notas. A presença automática do fato não é qualidade humana geral.

Famílias de desenvolvimento, 24 casos: banner-rule, dye-batch, harbor-ledger, nick-name, route-sheet, tool-kit, well-depth. Famílias de avaliação, 48 casos, seis em cada: archive-box, clinic-room, ferry-time, kiln-batch, orchard-crate, pump-valve, shelf-code, trail-marker. A interseção de famílias é vazia e a interseção das sentenças das mensagens é vazia. As 192 requisições retornaram HTTP 200. Falhas de execução: zero.

O arquivo `preflight.json` ainda traz o campo `not_final` verdadeiro, herdado do escritor. A identidade da coleta é `col-rq2-eval-20261106`, fase final, protocolo `rq2-eval-tradeoff-1`. O arquivo não foi reescrito.

## RQ3

Comportamento de fallback por requisição lógica, sob agenda controlada e reinício antes de cada episódio de gateway. Não é avaliação de circuit breaker distribuído, de carga nem de recuperação contínua. Os 20 episódios repetem a mesma agenda e o mesmo reinício. Não exploram a diversidade das falhas reais.

Totais: 240 episódios de gateway e 120 de controle direto. O direto não entra no N pareado. Prazo da requisição lógica: 2500 ms. Tempo limite por destino, nos dois gateways: 1 s. Sem repetir o mesmo destino. Uma reserva. O direto não tem essa política: chama só o primário e o tempo limite do cliente é o próprio prazo de 2500 ms. A diferença do direto no cenário de atraso vem dessa configuração, não de um fallback inexistente.

Coleta `col-rq3-final-20261106`, diretório `data/rq3-final-20261106`, fase final. Semente 20261106. Em cada cenário, Melis abre 10 episódios e LiteLLM abre 10. O piloto de cinco episódios não entra nos 20. A primeira tentativa incompleta do piloto, interrompida pela espera do log, continua fora desta coleta. Antes de qualquer episódio medido, uma confirmação que usava `cat` dentro da imagem falhou porque a imagem não tem esse comando; a confirmação passou a copiar o arquivo montado. Também houve uma partida com caminho relativo errado, encerrada antes de abrir o protocolo. Nenhuma das duas escreveu episódio.

Imagem Melis `sha256:79adb313b5060df96c6e5218bb108a289cd7d17c1f1fa08df9b221a6232c43bb`, sem digest de registro e sem commit de build atribuído. LiteLLM local `sha256:7924ee4aa63fc9d831e39056f838dea2c7623b86aff42426362bf023adf1127f`, digest de registro `sha256:6c82d338a60e7b273ae46bf1d8db814d2856ae010f96c44eeadde574d3893f76`. Os identificadores no fim coincidem com os do início. O commit do harness congelado é `cd162e788b7b8270c8c36418a9203229518722ea`. O HEAD no início e no fim da carga foi `30bfa164bfb12086d0503446f4009b90ca65d2ad`. Os arquivos congelados não mudaram entre esses commits.

Desfecho primário, descritivo: conclusão dentro do prazo. Em cada cenário os 20 pares Melis/LiteLLM foram idênticos em sucesso e em número de chamadas. Pares discordantes: zero. A estimativa da diferença é 0. Não há intervalo nem valor-p. “Nenhuma diferença observada” não prova equivalência.

| Cenário | Melis e LiteLLM | Chamadas | Direto, à parte | Chamadas do direto |
| --- | --- | ---: | --- | ---: |
| Saudável | 20/20 no prazo | 1, só o primário | 20/20 no prazo | 1 |
| 429 | 20/20 no prazo | primário 429, reserva 200 | 0/20, HTTP 429 | 1 |
| 5xx | 20/20 no prazo | primário 503, reserva 200 | 0/20, HTTP 503 | 1 |
| Atraso de 1500 ms | 20/20 no prazo | primário 200, reserva 200 | 20/20 no prazo | 1 |
| Primário indisponível | 20/20 no prazo | primário 503, reserva 200 | 0/20, HTTP 503 | 1 |
| Nenhum destino saudável | 0/20 no prazo | primário 503, reserva 503 | 0/20, HTTP 503 | 1 |

No 429, a agenda pede `Retry-After` de 1 s. A latência do episódio Melis ficou entre 4,8 e 11,7 ms, e a do LiteLLM entre 194 e 394 ms. A troca para a reserva ocorreu sem uma espera de 1 s. A regra observada é a do caminho OpenAI já existente: o destino atual é excluído e o seguinte é tentado, sem dormir no header. Isso não é uma falha geral de conformidade.

No atraso, o direto espera o primário e recebe 200 por volta de 1501 ms, dentro dos 2500 ms, com uma chamada. Melis encerra o destino perto de 1 s e completa pela reserva, média 1011 ms. LiteLLM também completa pela reserva, média 1461 ms. As duas chamadas do gateway nesse cenário retornaram 200 no backend: a primeira depois do atraso de 1500 ms no relógio do mock, a segunda na reserva.

No cenário sem destino saudável, a requisição lógica falhou nos três clientes. A sonda posterior no primário, com tempo limite de 1 s, observou HTTP 200 nas 60 execuções. `recovery_censored` é falso e `recovery_ms` fica vazio. A sonda mostra recuperação naquela chamada. Não marca o instante da recuperação e não mostra recuperação sustentada.

As latências médias estão em `scenario_summary.csv`, com N = 20 por cliente e cenário, sem teste. A unidade é o episódio. As 560 linhas de `attempts.csv` são as chamadas do mock durante a requisição lógica, com o horário observado pelo mock. A sonda de recuperação não entra nessa contagem. Falhas de execução e episódios com zero chamadas: zero. Duração da carga: 2.688.880 ms.

O reinício do gateway antes de cada episódio apaga o estado local do circuit breaker. A alegação cobre episódios independentes de failover. Os padrões do produto continuam 50%, janela de 60 s, mínimo de 5 pedidos e TTL aberto de 30 s, e não foram alterados para o resultado.

## Suplemento

Contratos já existentes no repositório cobrem isolamento de cache, falha aberta quando o embedding não está disponível e bloqueio ou redação em streaming, inclusive a impossibilidade de recolher um prefixo já emitido. Não há `safety_cases.csv` desta campanha e não há avaliação comparativa de cache ou de guardrails. O limiar de cosseno 0,92 em `quality/cache_dev_labels.json` está proposto nos rótulos de desenvolvimento, não ajustado e não validado. Não é configuração final. Dois réplicas e Redis continuam evidência arquitetural de Melis, sem configuração LiteLLM correspondente neste estudo.

## Lacunas

Ficam sem esta evidência: streaming SSE como carga comparativa, concorrência acima de 1, saturação, generalização para outros modelos e qualidade humana. A rubrica humana de RQ2 não foi preenchida e não deve ser substituída pelo avaliador automático.

## Pacotes

Os pacotes anteriores permanecem. Os novos, com SHA-256:

- `review-package-rq23-20261106.tar`: `5a2e59ce6ff781bb53aaa5905bd3c468a26c931db0b1782a03ae6697f95fc3c8`
- `rq2-eval-20261106.tar`: `33fa64dd6d8ab52eb8e266d9c04a7b992e57be5a197290e56638fe0060597da4`
- `rq3-final-20261106.tar`: `102346c86c854d820a050fd08173692085e4f4b5ba28fb92d9c9ea3779a046c7`

O pacote compacto traz os CSVs, os manifestos, o estimador de RQ2 e as tabelas por família. `forwarded.jsonl` e `answers.jsonl` ficam só no tar bruto, com os hashes registrados em `HASHES.json`.

## Matriz de cobertura

| Pergunta | Alegação | Evidência | Limitação | Arquivo |
| --- | --- | --- | --- | --- |
| RQ1 | Overhead na célula mínima | Primário n = 15; meias-larguras acima | Blocos 5, 7 e 8 fora; 0,50 ms não é margem; memória do gateway não medida | `audit-exclusions-20261106.json` na coleta RQ1 |
| RQ2 | Trade-off da compactação heurística | −0,2708 no fato crítico da resposta | Oito famílias; rubrica humana ausente; mensagens não são tokens | `comparisons.csv` e `rq2_family_differences.csv` |
| RQ3 | Failover por episódio sob agenda fixa | 20 pares por cenário; diferença observada 0 entre gateways | Repetição da mesma agenda; reinício; não é equivalência | `scenario_summary.csv` e `comparisons.csv` de RQ3 |
| Suplemento | Contratos de cache, guardrails e breaker | Testes de contrato no repositório | Limiar 0,92 não validado; sem comparação LiteLLM de réplicas | `quality/cache_dev_labels.json` |
| Lacunas | SSE, concorrência, saturação, outros modelos, qualidade humana | Não coletado | Não inferir a partir das três coletas | — |
