# Protocolo experimental: Melis, LiteLLM e provedor direto

Versão 1.0, proposta para implementação e piloto. Não contém resultados experimentais. A coleta final depende do fechamento dos parâmetros listados na seção de aprovação.

> Cópia versionada do protocolo anexo, trazida para dentro do repositório junto
> da instrumentação que o implementa (`instrumentation/`). O texto é preservado
> integralmente; a correspondência entre as seções deste protocolo e os schemas
> CSV concretos está em `instrumentation/data_dictionary.md` (seção "Rastreamento
> protocolo → schema"). Onde um nome de campo foi adaptado à estrutura existente,
> a semântica e o pareamento são mantidos e a adaptação é registrada no dicionário.

## Objetivo e artefato

Avaliar a mediação de inferência, a governança de contexto e a resiliência do Melis, distinguindo benefícios das políticas de custos da infraestrutura. A comparação não pressupõe superioridade do Melis.

O usuário informou como candidato o commit `b8506766ab16e66ee86e3168b2e1956d0147a02d`. O Kiro deve confirmar esse hash no repositório; qualquer alteração posterior nos executáveis ou no harness produz um novo candidato, identificado no manifesto. Os resultados anteriores relatados pelo Kiro são evidências de verificação técnica, não resultados deste protocolo.

Comparadores:

- Provedor direto: cliente comum acessando o backend.
- Melis: cliente comum, gateway e mesmo backend.
- LiteLLM: cliente comum, versão e digest explícitos, mesmo backend.

O pin existente do LiteLLM deve ser conferido antes da coleta: identificar versão, origem, funcionalidade e justificativa de seleção. Não apresentar uma versão antiga como estado atual sem justificativa; decidir a versão antes dos resultados, não após observar quem vence.

## Questões e desfechos

| Questão | Comparação | Desfecho primário | Secundários |
|---|---|---|---|
| RQ1: custo da mediação | Direto, Melis mínimo, LiteLLM mínimo | Latência fim a fim por execução em carga fixa | p95, throughput útil, erros, CPU, RAM e tempos de streaming |
| RQ2: compromisso contexto–qualidade | Sem compactação, cronológica, heurística, embedding e híbrida | Qualidade da política primária versus baseline sem compactação | Tokens, retenção de fatos, latência total, custo de scoring e rejeições |
| RQ3: resiliência | Melis e LiteLLM, com direto como diagnóstico | Taxa de sucesso em episódios de falha controlada | Recuperação, chamadas ao backend, tentativas, failover e latência |
| RQ4: cache, escopo secundário | Cache desligado versus ligado; equivalente externo quando validado | Reutilização correta em consultas elegíveis | Falsos hits, hit rate, isolamento, TTL, latência e degradação |

Guardrails e integridade são avaliados por testes de contrato e por uma suíte de casos rotulados. Não transformar indiscriminadamente cada teste unitário em uma hipótese estatística.

RQ1–RQ3 compõem o núcleo recomendado do artigo. RQ4 pode ser suplemento. Não executar todas as combinações de todas as features.

## Desenho em duas etapas

### Piloto autorizado

O piloto verifica Docker/Compose, instrumentação, schemas, consumo de recursos e duração. Também estima variabilidade para dimensionar a coleta final; seus dados não entram nos testes confirmatórios.

Proposta inicial:

- RQ1: 5 blocos, concorrências 1 e 8, JSON e SSE, com os três caminhos.
- RQ2: 20–30 casos exclusivamente de desenvolvimento, cobrindo posições e tamanhos.
- RQ3: 5 episódios por cenário e gateway.
- RQ4 e guardrails: smoke tests com rótulos conhecidos, sem alegação de eficácia geral.

São valores de partida, não dimensionamento final. Antes de executar, o Kiro deve calcular o total de execuções, duração esperada a partir dos smoke tests, requisitos de memória/disco e eventual custo financeiro. Não estimar créditos da plataforma.

### Coleta final

Após o piloto, congelar protocolo, versões, configurações, datasets e scripts de análise. Definir amostra por poder/sensibilidade ou precisão do intervalo de confiança, usando variabilidade do piloto e efeito mínimo relevante. Não usar apenas um número convencional de repetições.

Como candidato de planejamento, considerar 30 blocos independentes para desempenho e 120 casos de avaliação para qualidade, com gerações repetidas por caso. Esses números não estão aprovados como suficientes: recalcular e justificar, inclusive se precisarem aumentar ou diminuir.

Qualquer mudança de software que afete resultados exige novo manifesto. Preservar os dados anteriores e repetir as células impactadas. Não misturar silenciosamente resultados de versões distintas.

## RQ1: mediação

### Controle

Usar backend determinístico para isolar o caminho do gateway e backend real para verificar relevância operacional. Reportar separadamente; não extrapolar throughput do mock para desempenho de inferência real.

Perfil mínimo: cache, compactação e guardrails desativados; retries e fallback desativados; autenticação e telemetria equivalentes ou diferenças explicitadas. Mesmo modelo, parâmetros, payloads, tamanho de resposta e timeouts.

Executar um gateway por vez. Recursos do backend não devem variar entre comparadores. Registrar host compartilhado, GPU, temperatura/throttling quando observáveis e versão do runtime. Não misturar execuções com atividades pesadas não controladas.

### Blocos e carga

Cada bloco contém os três caminhos com mesma carga e corpus, em ordem aleatorizada e balanceada. Um bloco é uma unidade independente de repetição; requisições internas são observações aninhadas.

Separar:

- Carga fixa comum: concorrência/taxa iguais para comparação principal.
- Exploração de saturação: curva separada, sem comparar uma configuração saturada com outra subutilizada como se fosse equivalente.

Concorrências candidatas: 1, 8 e 32; ajustar no piloto e congelar. Aquecimento candidato de 20 s e janela medida de 60 s, a confirmar pelo piloto. Usar mesmos critérios de aquecimento e caches quentes/frios explicitamente definidos.

Para carga aberta, registrar instante agendado e efetivo do envio e atrasos de geração. Para carga fechada, declarar a limitação. Não usar exclusivamente latência dos sucessos para ocultar falhas.

### Streaming

Separar TTFB (primeiro byte HTTP), tempo até primeiro evento de conteúdo e tempo até conclusão. TTFT só pode ser chamado de tempo até primeiro token se a instrumentação realmente observar tokens; um delta SSE pode conter vários tokens.

Verificar integridade do SSE e da resposta reconstruída. Uma conexão rápida com resposta inválida não conta como sucesso.

## RQ2: governança de contexto

### Comparações e ablações

Usar os mesmos casos e parâmetros de geração, em desenho pareado:

- Provedor direto sem compactação: baseline de qualidade.
- Melis sem compactação: controle da mediação.
- Política cronológica.
- Heurística.
- Embedding.
- Híbrida.

Definir a política primária antes do conjunto final. As demais são comparações secundárias/ablações. Não selecionar a vencedora no conjunto final e tratá-la como hipótese prévia.

LiteLLM sem compactação pode ser um controle de sistema, mas não é um baseline equivalente de algoritmo. Investigar a versão fixada e documentar capacidades e integrações. Se for necessário adicionar a mesma política externa antes dos gateways, avaliar isso como comparação complementar, distinguindo algoritmo de integração.

Cache desligado na comparação principal. Guardrails desligados ou idênticos para não alterar os casos de forma desigual. Grupos de ferramentas preservados por padrão; sua remoção opt-in é ablação separada.

### Dados e rótulos

Usar conversas sintéticas controladas e/ou dataset externo com licença e proveniência registradas. Nenhum dado pessoal ou corporativo privado sem autorização.

Estratificar por posição dos fatos necessários (início, meio, fim e distribuídos), tamanho do contexto, domínio e distratores. Adicionar casos que exijam mais de um fato e casos em que a última mensagem não contenha lexicalmente a informação relevante.

Separar desenvolvimento e avaliação antes de ajustes. Não criar splits quase duplicados a partir do mesmo template sem separar por família/template/documento. Os três fixtures atuais por split permanecem smoke tests.

Para cada caso: fatos indispensáveis, resposta/rubrica, posição, domínio, tamanho e mensagens protegidas. Verificar rótulos antes de congelar.

Qualidade: manter rubrica 0–3 apenas se houver critérios explícitos e exemplos. Preferir avaliação humana cega quanto à política, com dois avaliadores em uma fração predefinida e resolução de discordâncias. Se usar juiz LLM, fixar modelo/prompt, validar contra humanos e não usá-lo como única evidência sem discussão.

### Orçamento e tokens

Comparar políticas sob o mesmo orçamento, contador e reserva de saída. Registrar tokens de entrada antes/depois, origem da contagem e modelo de tokenizer; estimativas locais não equivalem automaticamente a tokens faturados.

Separar tokens informados pelo provedor, estimados pelo gateway e overhead de formatação. Registrar tempo/custo de embeddings e eventual uso de acelerador compartilhado.

A latência fim a fim deve incluir scoring e compactação. Não subtrair o custo da política da alegação de ganho total.

### Qualidade e hipótese

Para a expressão "sem degradação relevante", a análise primária proposta é não inferioridade da diferença pareada de qualidade `política − baseline`. A margem δ deve ser justificada com significado prático e aprovada antes da coleta final; não adotar automaticamente 0,5 ou outra fração da escala.

Critério proposto: limite inferior unilateral de 95% da diferença acima de −δ. Para alegar equivalência, usar desenho e critério próprios, com intervalo bilateral de 90% inteiramente em [−δ,+δ]. Ausência de significância em teste de diferença não demonstra nenhum deles.

O método de intervalo/teste deve respeitar a escala e o pareamento, com bootstrap por caso ou método adequado validado por simulação no piloto. Declarar se o estimando é média da rubrica; não apresentar isso como equivalência de toda a distribuição.

Relatar também proporção de respostas abaixo de um piso de qualidade, retenção de fatos e análise por posição. Aprovação por média não deve esconder falhas graves em um estrato.

## RQ3: resiliência

Cenários determinísticos: saudável; 429 com Retry-After; 5xx transitório; timeout; primário indisponível com fallback saudável; retorno do primário; duas réplicas; Redis indisponível e recuperado. Incluir cenário sem fallback saudável para evitar uma avaliação exclusivamente favorável.

Equivalência funcional antes de equivalência de parâmetros: comparar limites de tentativas, deadlines e objetivo de recuperação; documentar mecanismos sem correspondência direta.

Unidade independente: episódio com estados reiniciados e cronograma de falha reproduzível. As requisições de um episódio não são repetições independentes. Usar cronograma/seed pareados entre gateways, sem compartilhar estado entre suas execuções.

Medir sucesso dentro do deadline, tempo de recuperação, amplificação de chamadas e latência. Registrar sonda, tentativa e destino quando observáveis independentemente. Limitações de coordenação durante partição devem ser avaliadas conforme a garantia documentada, não como garantia global absoluta.

## RQ4 e guardrails

Cache: separar repetições exatas, paráfrases elegíveis e negativos difíceis semanticamente próximos com respostas diferentes. Medir falsos hits e não apenas economia. Controle de TTL e identidades com fixtures conhecidos; separar cold/warm.

Guardrails: rótulos de permitido/bloqueado/redigido e conteúdo final realmente recebido pelo cliente. Incluir padrões entre chunks e nas bordas da janela, sem presumir proteção geral fora do escopo implementado.

Precisão/recall dependem de um conjunto rotulado e representativo; passar testes de contrato não demonstra resistência geral a ataques. Não chamar regex/keywords de solução completa contra prompt injection ou alucinação.

## CSVs obrigatórios e dicionário de dados

Exportar dados brutos de todas as observações mensuráveis, não apenas médias. CSV UTF-8, separador vírgula, ponto decimal, cabeçalho estável, campos textuais escapados e ausentes vazios, nunca convertidos silenciosamente a zero. Tempos em ms ou s explicitados no nome; timestamps UTC e durações por relógio monotônico.

### `requests.csv`

Uma linha por requisição lógica, incluindo falhas:

`schema_version,experiment_id,phase,run_id,block_id,pair_id,case_id,request_id,gateway,scenario,backend_profile,streaming,concurrency,policy,started_at_utc,scheduled_delay_ms,latency_ms,ttfb_ms,first_content_ms,status,http_status,error_class,timeout,completion_valid,backend_call_count,cache_result,requested_model,resolved_model,effective_model,input_tokens_before,input_tokens_after,output_tokens,token_count_source,scoring_ms,compaction_ms`

Campos não observáveis ficam ausentes com limitação no manifesto. Não inventar precisão nem extrair medidas inexistentes de um log. Tentativas não duplicam a requisição lógica neste arquivo.

### `attempts.csv`

Uma linha por tentativa observável: `run_id,request_id,attempt_index,destination_id,started_at_utc,latency_ms,status,http_status,is_probe,is_fallback`. Destinos pseudonimizados e sem segredos. Se a instrumentação externa não distinguir tentativas, registrar apenas contagem agregada e declarar a limitação.

### `runs.csv`

Uma linha por execução medida: fatores, seeds, ordem no bloco, aquecimento, duração, número de requisições, sucessos, falhas, throughput útil, mediana, p95, p99, modelo, hashes e artefatos. Só reportar p99 quando o volume permitir estimativa útil; indicar incerteza.

### `resources.csv`

Amostragem temporal comum: `run_id,timestamp_utc,elapsed_s,component,cpu_seconds_total,memory_rss_bytes,memory_working_set_bytes,network_rx_bytes,network_tx_bytes,gpu_utilization_pct,gpu_memory_bytes`.

Registrar definição, origem e intervalo de coleta. CPU/memória do gateway, embeddings, Redis e backend separados quando possível. Dados indisponíveis permanecem ausentes. RAM de pico e CPU média devem ser derivados com denominadores documentados.

### `quality.csv`

Uma linha por avaliação: `run_id,case_id,pair_id,policy,generation_rep,answer_id,evaluator_id,evaluator_type,score,required_facts,retained_facts,position_stratum,domain,context_length_stratum,blinded`.

Múltiplos avaliadores e gerações são observações aninhadas; não multiplicam artificialmente o número de casos independentes.

### `resilience_events.csv` e `safety_cases.csv`

Eventos de falha/recuperação: episódio, instante relativo, evento programado/observado, estado e destino.

Casos de segurança/cache: caso, família, rótulo esperado, resultado observado, identidade pseudonimizada, categoria de hit, leak_detected e detecção/ação. Não armazenar conteúdo sensível em CSV público.

### `comparisons.csv` e arquivos auxiliares

Resultados derivados: questão, desfecho, contraste, população, unidades independentes, n, estimativa, unidade, tamanho de efeito, limites do IC, método, seed, p bruto/corrigido quando aplicável, margem e decisão pré-especificada.

Adicionar `manifest.json`, `data_dictionary.md`, hashes dos datasets/configurações, stdout/stderr sanitizados e relatórios de validação de schema. Respostas e prompts sintéticos podem ficar em JSONL separados por `answer_id`; não poluir CSV com grandes blobs.

Nem toda métrica exige p-valor. Testes de contrato podem exportar esperado/observado e pass/fail, sem análise inferencial artificial.

## Plano de análise

- Desempenho: calcular resumo por execução; comparar blocos pareados. Usar ICs por reamostragem de blocos e diferenças/razões com estimando definido. Para três caminhos, Friedman com Kendall W é uma opção se o desenho corresponder; comparações pareadas com correção de Holm e efeito pareado apropriado.
- Latências: apresentar distribuição, mediana/p95 e falhas. Evitar inferência que trate toda requisição da mesma execução como independente. Bootstrap de quantis respeita blocos e volume.
- Qualidade: análise pareada por caso, agregando repetições como definido ou usando modelo hierárquico. Bootstrap por caso, mantendo gerações e avaliadores agrupados. Reportar diferença na escala e proporções de falha, além do critério de não inferioridade.
- Resiliência: comparar proporções e recuperação por episódio. McNemar somente para desfechos binários realmente pareados e independentes; Fisher somente quando sua estrutura de independência for válida. Não aplicar Fisher a milhares de chamadas autocorrelacionadas como se fossem independentes.
- Segurança/cache: contagens e intervalos por caso/família, com pareamento adequado quando houver comparação; distinguir zero falhas observadas de garantia de ausência de falhas.
- Multiplicidade: definir desfecho primário de cada RQ, uma política primária e famílias de contrastes. Corrigir comparações secundárias planejadas e rotular exploração. Não selecionar apenas resultados favoráveis.

Fixar seeds, versões e scripts. Gerar tabelas/gráficos a partir dos CSVs sem edição manual dos resultados. Registrar exclusões e execuções abortadas; manter falhas e timeouts. Diferenciar falha do equipamento de falha do sistema sob teste.

## Aprovação antes da coleta final

O Kiro deve apresentar, após o piloto:

- Versões e configurações finais dos três caminhos e diferenças inevitáveis.
- Total de células, duração prevista e recursos.
- Dataset, licença, splits e rubrica.
- Política primária, orçamento e margem de qualidade justificada.
- Tamanho amostral e sua justificativa/sensibilidade.
- Hipóteses, critérios de exclusão e plano de análise congelados.
- CSVs de exemplo válidos e relatório de completude.
- Resultados de smoke/pilot identificados, sem inclusão na coleta final.

Aprovação final do usuário precede coleta confirmatória. Nenhuma exigência de "ganhar do LiteLLM": resultados nulos, perdas e trade-offs devem ser preservados.
