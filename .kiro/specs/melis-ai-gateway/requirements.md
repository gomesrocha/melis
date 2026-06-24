# Requirements Document

## Introduction

O Melis é um AI Gateway stateless de ultra-alta performance, implementado em Rust, projetado para atuar como camada de infraestrutura inteligente entre aplicações cliente (Java/Python) e múltiplos provedores de LLM. O gateway unifica o contrato de consumo sob a especificação OpenAI, oferece compressão adaptativa de contexto, resiliência distribuída via Circuit Breaker compartilhado e overhead mínimo no fluxo de requisições.

## Glossary

- **Gateway**: O serviço Melis AI Gateway, responsável por rotear e transformar requisições entre clientes e provedores de LLM.
- **Provedor_LLM**: Um serviço externo de Large Language Model (ex: OpenAI, Anthropic, Google Vertex AI).
- **Cliente**: Uma aplicação consumidora que envia requisições ao Gateway via protocolo HTTP.
- **Context_Compactor**: O módulo interno do Gateway responsável pela compressão adaptativa do histórico de mensagens, removendo tokens redundantes.
- **Payload_Transpiler**: O módulo interno responsável pela tradução bidirecional de payloads JSON entre contratos distintos de provedores.
- **Circuit_Breaker**: Mecanismo distribuído de proteção que interrompe temporariamente o tráfego para um Provedor_LLM em estado de falha.
- **Rate_Limiter**: Componente que controla a taxa de requisições por segundo por Cliente, utilizando o algoritmo Token Bucket armazenado no Redis.
- **Redis_Cluster**: O cluster Redis utilizado para armazenamento de estado volátil de infraestrutura (rate limiting, circuit breaker).
- **SSE**: Server-Sent Events, protocolo de streaming unidirecional sobre HTTP.
- **Token_Bucket**: Algoritmo de rate limiting que permite rajadas controladas de requisições.
- **Overhead**: Tempo adicional de processamento introduzido pelo Gateway no fluxo de uma requisição.
- **Route_Config**: Arquivo de configuração YAML que define rotas com configurações por rota de provedor, modelo e otimização de tokens.
- **Token_Optimization**: Configuração por rota que define a estratégia de compressão de tokens, limites e tokenizador local.
- **Hot_Reload**: Capacidade de recarregar a configuração em tempo de execução sem necessidade de reinicialização do serviço.

## Requirements

### Requirement 1: Endpoint Unificado de Chat Completions

**User Story:** Como um desenvolvedor de aplicação cliente, eu quero consumir um único endpoint padronizado no formato OpenAI, para que eu possa trocar provedores de LLM sem alterar o código da minha aplicação.

#### Acceptance Criteria

1. THE Gateway SHALL expor um endpoint HTTP POST no caminho `/v1/chat/completions` que aceite payloads no formato da especificação OpenAI Chat Completions, contendo obrigatoriamente os campos `model` (string) e `messages` (array com pelo menos uma mensagem contendo `role` e `content`).
2. WHEN o header `Accept: text/event-stream` estiver presente na requisição, THE Gateway SHALL retornar a resposta utilizando o protocolo SSE para streaming de texto com content-type `text/event-stream`.
3. WHEN o header `Accept: text/event-stream` não estiver presente na requisição, THE Gateway SHALL retornar a resposta completa em formato JSON com content-type `application/json` em uma única resposta HTTP.
4. WHEN uma requisição válida for recebida, THE Gateway SHALL encaminhar a requisição ao Provedor_LLM selecionado e retornar a resposta ao Cliente no formato OpenAI.
5. IF o payload da requisição não contiver os campos obrigatórios `model` ou `messages`, ou se `messages` for um array vazio, THEN THE Gateway SHALL retornar erro HTTP 400 Bad Request com mensagem indicando quais campos estão ausentes ou inválidos.
6. THE Gateway SHALL aceitar payloads com tamanho máximo de 10MB; requisições excedendo esse limite SHALL resultar em erro HTTP 413 Payload Too Large.

### Requirement 2: Tradução Bidirecional de Payloads (Payload Transpiler)

**User Story:** Como um operador de plataforma, eu quero que o gateway traduza automaticamente os payloads entre diferentes provedores, para que eu possa adicionar novos backends sem impacto nas aplicações cliente.

#### Acceptance Criteria

1. WHEN uma requisição no formato OpenAI for recebida e o Provedor_LLM de destino for Anthropic, THE Payload_Transpiler SHALL converter o payload para o formato Anthropic Messages API.
2. WHEN uma requisição no formato OpenAI for recebida e o Provedor_LLM de destino for Google Vertex AI, THE Payload_Transpiler SHALL converter o payload para o formato Google Generative AI API.
3. WHEN uma resposta for recebida de um Provedor_LLM em formato nativo, THE Payload_Transpiler SHALL converter a resposta de volta para o formato OpenAI Chat Completions antes de enviá-la ao Cliente, incluindo respostas em streaming onde cada chunk SSE recebido do provedor SHALL ser convertido individualmente para o formato de chunk SSE OpenAI e encaminhado ao Cliente sem aguardar a resposta completa.
4. THE Payload_Transpiler SHALL realizar a conversão de payloads sem alocações adicionais de memória heap para campos de texto que não necessitem de transformação estrutural, copiando referências ao buffer original (zero-copy) para campos transferidos sem modificação.
5. THE Payload_Transpiler SHALL garantir a propriedade de round-trip: para todo payload válido no formato OpenAI, a conversão para formato nativo do provedor seguida de reconversão para formato OpenAI SHALL preservar os mesmos valores nos campos `model`, `messages` (incluindo `role` e `content` de cada mensagem), `temperature`, `max_tokens` e `stop`, desconsiderando campos que não possuam equivalente no provedor de destino.
6. IF o payload OpenAI de entrada contiver campos sem equivalente no formato do Provedor_LLM de destino, THEN THE Payload_Transpiler SHALL omitir esses campos na conversão e registrar um aviso na camada de observabilidade indicando quais campos foram descartados, sem interromper o processamento da requisição.

### Requirement 3: Compressão Adaptativa de Contexto (Context Compactor)

**User Story:** Como um operador de plataforma, eu quero reduzir o número de tokens enviados aos provedores de LLM, para que eu possa diminuir custos operacionais e melhorar a eficiência do uso de contexto.

#### Acceptance Criteria

1. WHEN o histórico de mensagens de uma requisição exceder um limiar configurável de tokens (valor padrão: 4096 tokens, configurável entre 512 e 128000 tokens), THE Context_Compactor SHALL executar poda (pruning) das mensagens mais antigas do histórico que não sejam mensagens de sistema nem a última mensagem do usuário, preservando a ordem cronológica das mensagens restantes.
2. WHEN o histórico de mensagens contiver stop-words presentes na lista configurável de stop-words do Gateway, THE Context_Compactor SHALL removê-las do conteúdo textual das mensagens antes do encaminhamento ao Provedor_LLM.
3. IF a compressão for ativada (histórico exceder o limiar configurável), THEN THE Context_Compactor SHALL reduzir o número de tokens em pelo menos 25% em relação ao payload original, medido pela contagem de tokens antes e após a compressão utilizando o Hugging Face Tokenizer SDK.
4. THE Context_Compactor SHALL preservar intactas, sem qualquer modificação, todas as mensagens com role "system" e a última mensagem com role "user" do histórico, independentemente do nível de compressão aplicado.
5. THE Context_Compactor SHALL utilizar o Hugging Face Tokenizer SDK para contagem precisa de tokens.
6. IF o histórico de mensagens de uma requisição não exceder o limiar configurável de tokens, THEN THE Context_Compactor SHALL encaminhar o payload ao Provedor_LLM sem aplicar qualquer compressão ou modificação.
7. IF a compressão não conseguir atingir a meta de redução de 25% após a poda completa das mensagens elegíveis, THEN THE Context_Compactor SHALL encaminhar o payload com a melhor compressão obtida e registrar a métrica `melis_context_compression_ratio` com o valor real de compressão alcançado.

### Requirement 4: Balanceamento de Carga e Failover

**User Story:** Como um operador de plataforma, eu quero distribuir requisições entre múltiplos provedores de LLM com pesos configuráveis, para que eu possa otimizar custos e garantir disponibilidade.

#### Acceptance Criteria

1. THE Gateway SHALL distribuir requisições entre Provedores_LLM configurados proporcionalmente aos pesos definidos na Route_Config, com tolerância de desvio de até 5% em relação à proporção ideal ao longo de uma janela de 1000 requisições.
2. WHEN um Provedor_LLM retornar erro HTTP 5xx ou timeout (configurável, padrão: 30 segundos), THE Gateway SHALL redirecionar a requisição automaticamente para o próximo Provedor_LLM disponível conforme a ordem de prioridade definida na Route_Config, com no máximo 2 tentativas de failover por requisição.
3. WHEN todos os Provedores_LLM configurados estiverem indisponíveis, THE Gateway SHALL retornar ao Cliente um erro HTTP 503 Service Unavailable com mensagem descritiva indicando que nenhum provedor está disponível.
4. WHEN os pesos de distribuição forem atualizados na Route_Config, THE Gateway SHALL aplicar a nova distribuição em até 5 segundos sem necessidade de reinicialização do serviço (Hot_Reload).

### Requirement 5: Circuit Breaker Distribuído

**User Story:** Como um operador de plataforma, eu quero que todas as instâncias do gateway compartilhem o estado de saúde dos provedores, para que uma falha detectada por uma instância proteja imediatamente todas as outras.

#### Acceptance Criteria

1. WHEN um Provedor_LLM retornar erro HTTP 429 (Too Many Requests), ou WHEN a taxa de falhas (respostas HTTP 5xx ou timeout) de um Provedor_LLM exceder o limiar configurável (padrão: 50%) dentro de uma janela deslizante configurável (padrão: 60 segundos, mínimo de 5 requisições na janela), THE Circuit_Breaker SHALL registrar uma flag de indisponibilidade com TTL configurável (padrão: 30 segundos) no Redis_Cluster.
2. WHILE a flag de indisponibilidade de um Provedor_LLM estiver ativa no Redis_Cluster, THE Gateway SHALL interromper o encaminhamento de requisições para esse provedor em todas as instâncias.
3. WHEN o TTL da flag de indisponibilidade expirar no Redis_Cluster, THE Circuit_Breaker SHALL encaminhar exatamente 1 requisição de teste (half-open state) ao Provedor_LLM e aguardar resposta dentro do timeout configurado para o provedor.
4. WHEN a requisição de teste no estado half-open receber resposta HTTP 2xx dentro do timeout configurado, THE Circuit_Breaker SHALL remover a flag de indisponibilidade do Redis_Cluster e restaurar o provedor ao estado ativo.
5. WHEN a requisição de teste no estado half-open receber resposta HTTP 5xx, HTTP 429, ou timeout, THE Circuit_Breaker SHALL renovar a flag de indisponibilidade no Redis_Cluster com TTL multiplicado por fator de backoff configurável (padrão: fator 2), até um TTL máximo configurável (padrão: 300 segundos).
6. IF o Redis_Cluster estiver indisponível, THEN THE Circuit_Breaker SHALL operar em modo fail-open, permitindo o encaminhamento de requisições ao Provedor_LLM sem consultar o estado compartilhado, e SHALL registrar um evento de alerta na métrica de observabilidade.

### Requirement 6: Rate Limiting Distribuído

**User Story:** Como um operador de plataforma, eu quero limitar a taxa de requisições por cliente, para que eu possa proteger os provedores de LLM contra sobrecarga e garantir uso justo entre múltiplos clientes.

#### Acceptance Criteria

1. THE Rate_Limiter SHALL controlar a taxa de requisições por segundo por Cliente utilizando o algoritmo Token_Bucket com estado armazenado no Redis_Cluster, onde cada Cliente possui capacidade máxima de rajada (burst) e taxa de reposição (refill rate) configuráveis, e a verificação de consumo de tokens SHALL ser executada de forma atômica no Redis_Cluster.
2. WHEN um Cliente exceder o limite de requisições configurado, THE Rate_Limiter SHALL retornar erro HTTP 429 Too Many Requests com o header `Retry-After` contendo o tempo de espera em segundos até que pelo menos um token esteja disponível no bucket do Cliente.
3. WHEN um Cliente estiver dentro do limite configurado, THE Rate_Limiter SHALL permitir a passagem da requisição introduzindo no máximo 1ms de latência adicional para a verificação de rate limit.
4. THE Rate_Limiter SHALL suportar configuração de limites distintos por Cliente por meio de identificação via API key ou header customizado.
5. IF o Redis_Cluster estiver indisponível durante a verificação de rate limit, THEN THE Rate_Limiter SHALL permitir a passagem da requisição (fail-open) e registrar um evento de alerta indicando a falha de conectividade com o Redis_Cluster.

### Requirement 7: Autenticação e Roteamento

**User Story:** Como um operador de plataforma, eu quero que o gateway autentique clientes e roteie requisições de forma segura, para que apenas clientes autorizados acessem os provedores de LLM.

#### Acceptance Criteria

1. WHEN uma requisição for recebida sem credenciais (API key ou token) no header `Authorization`, THE Gateway SHALL retornar erro HTTP 401 Unauthorized com corpo contendo mensagem de erro indicando ausência de credenciais, e interromper o processamento.
2. IF as credenciais fornecidas no header `Authorization` forem inválidas, expiradas ou revogadas, THEN THE Gateway SHALL retornar erro HTTP 401 Unauthorized com corpo contendo mensagem de erro indicando credenciais inválidas, e interromper o processamento.
3. WHEN uma requisição for recebida com credenciais válidas no header `Authorization`, THE Gateway SHALL identificar o Cliente associado às credenciais e prosseguir com o pipeline de processamento.
4. WHEN uma requisição autenticada for recebida, THE Gateway SHALL rotear a requisição ao Provedor_LLM correspondente ao valor do campo `model` do payload, conforme o mapeamento modelo-provedor definido na Route_Config.
5. IF o campo `model` estiver ausente no payload ou contiver um valor não reconhecido no mapeamento definido na Route_Config, THEN THE Gateway SHALL retornar erro HTTP 400 Bad Request com mensagem de erro indicando modelo inválido ou ausente, e interromper o processamento.

### Requirement 8: Performance e Overhead Mínimo

**User Story:** Como um desenvolvedor de aplicação cliente, eu quero que o gateway introduza overhead desprezível nas minhas requisições, para que a latência percebida pelo usuário final permaneça aceitável.

#### Acceptance Criteria

1. THE Gateway SHALL adicionar no máximo 2ms de overhead no percentil 99 (p99) do processamento interno de uma requisição, excluindo tempo de rede e tempo do Provedor_LLM, quando submetido a uma carga sustentada de até 1000 requisições por segundo.
2. WHILE em estado de repouso (sem requisições ativas), THE Gateway SHALL consumir menos de 40MB de RAM RSS.
3. THE Gateway SHALL operar de forma totalmente stateless, delegando qualquer estado volátil de infraestrutura ao Redis_Cluster.
4. WHILE sob carga sustentada de 1000 requisições por segundo, THE Gateway SHALL consumir no máximo 128MB de RAM RSS.
5. THE Gateway SHALL processar pelo menos 5000 requisições concorrentes simultâneas sem rejeitar conexões ou exceder o limite de overhead definido no critério 1.

### Requirement 9: Observabilidade e Métricas (OpenTelemetry)

**User Story:** Como um operador de plataforma, eu quero ter visibilidade completa sobre o comportamento do gateway em produção, para que eu possa monitorar performance, diagnosticar problemas e otimizar custos.

#### Acceptance Criteria

1. THE Gateway SHALL expor um endpoint HTTP GET no caminho `/metrics` que retorne métricas no formato Prometheus text exposition (content-type `text/plain; version=0.0.4`).
2. WHEN uma requisição for processada pelo Gateway, THE Gateway SHALL incrementar o contador `melis_gateway_requests_total` com labels `route`, `client` e `status` (código HTTP retornado ao Cliente).
3. WHEN uma requisição for encaminhada ao Provedor_LLM, THE Gateway SHALL incrementar o contador `melis_llm_tokens_total` com labels `direction` (valor `input` ou `output`), `model` e `client_id`, registrando a quantidade de tokens consumidos.
4. WHEN o Context_Compactor for ativado para uma requisição, THE Gateway SHALL registrar no histograma `melis_context_compression_ratio` o valor da razão entre tokens após compressão e tokens antes da compressão (valor entre 0.0 e 1.0).
5. WHEN uma chamada ao Provedor_LLM for concluída, THE Gateway SHALL registrar a duração da chamada no histograma `melis_backend_latency_seconds` com labels `provider` e `status`, utilizando buckets de 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0 e 10.0 segundos.
6. WHEN uma requisição for processada pelo Gateway, THE Gateway SHALL registrar o tempo de processamento interno (excluindo tempo de rede e tempo do Provedor_LLM) no histograma `melis_gateway_overhead_seconds` utilizando buckets de 0.0005, 0.001, 0.002, 0.005, 0.01 e 0.05 segundos.
7. THE Gateway SHALL exportar traces distribuídos via protocolo OTLP (OpenTelemetry Protocol), propagando trace context entre requisições do Cliente e chamadas ao Provedor_LLM, utilizando as crates `tracing-opentelemetry` e `opentelemetry` do Rust.
8. WHEN uma requisição for processada pelo Gateway, THE Gateway SHALL criar um span raiz contendo os atributos `client_id`, `model`, `provider` e `status`, e spans filhos para cada etapa do pipeline (autenticação, compressão, encaminhamento ao provedor).
9. IF o endpoint `/metrics` não conseguir coletar métricas internas, THEN THE Gateway SHALL retornar HTTP 503 com mensagem de erro indicando indisponibilidade temporária do subsistema de métricas.

### Requirement 10: Compatibilidade com Containers e Orquestração

**User Story:** Como um engenheiro DevOps, eu quero que o gateway seja nativo para containers e compatível com Kubernetes, para que eu possa implantar, escalar e gerenciar instâncias de forma padronizada.

#### Acceptance Criteria

1. THE Gateway SHALL ser compilado como um binário estático nativo em Rust com runtime tokio, sem dependências de bibliotecas dinâmicas do sistema operacional.
2. THE Gateway SHALL fornecer uma imagem Docker otimizada baseada em imagem mínima (scratch ou distroless) com tamanho final inferior a 50MB.
3. THE Gateway SHALL expor endpoints de health check (`/healthz` para liveness e `/readyz` para readiness) compatíveis com probes do Kubernetes, retornando HTTP 200 quando saudável.
4. WHEN o endpoint `/healthz` for chamado e o processo do Gateway estiver respondendo, THE Gateway SHALL retornar HTTP 200 indicando que a instância está viva.
5. WHEN o endpoint `/readyz` for chamado e a conexão com o Redis_Cluster estiver indisponível, THE Gateway SHALL retornar HTTP 503 indicando que a instância não está pronta para receber tráfego.
6. WHEN o Gateway receber sinal SIGTERM, THE Gateway SHALL iniciar graceful shutdown, parando de aceitar novas conexões e aguardando até 30 segundos para conclusão das requisições em andamento antes de encerrar o processo.

### Requirement 11: Configuração de Rotas via YAML (Route Config)

**User Story:** Como um operador de plataforma, eu quero definir rotas com configurações individuais de provedor, modelo e otimização de tokens em um arquivo YAML, para que eu possa gerenciar o comportamento do gateway de forma declarativa e granular por rota, inspirado na abordagem do KrakenD.

#### Acceptance Criteria

1. THE Gateway SHALL carregar a Route_Config a partir de um arquivo YAML no caminho configurável via variável de ambiente `MELIS_ROUTES_CONFIG` (padrão: `./routes.yaml`), contendo uma lista de rotas onde cada rota define obrigatoriamente os campos `path` (string), `method` (string HTTP method) e `provider` (string identificando o Provedor_LLM).
2. THE Route_Config SHALL suportar os seguintes provedores no campo `provider`: `openai`, `anthropic`, `google_vertex_ai`, `oci_genai` e `ollama`, além de permitir registro de provedores adicionais via campo `custom_providers` no nível raiz do arquivo YAML.
3. WHEN uma rota definir o campo `model` na Route_Config, THE Gateway SHALL utilizar o modelo especificado para encaminhar requisições recebidas nessa rota ao Provedor_LLM correspondente, substituindo qualquer valor de `model` presente no payload da requisição.
4. WHEN uma rota definir a seção `token_optimization` na Route_Config, THE Gateway SHALL aplicar a estratégia de otimização de tokens especificada para requisições recebidas nessa rota, respeitando os parâmetros `strategy` (string, valores aceitos: `adaptive_trimming`, `sliding_window`, `none`), `max_history_messages` (inteiro positivo, padrão: 20), `compress_above_tokens` (inteiro positivo, padrão: 4096) e `local_tokenizer` (string identificando o tokenizador, padrão: `cl100k_base`).
5. WHEN a Route_Config não definir a seção `token_optimization` para uma rota, THE Gateway SHALL utilizar a configuração global de compressão do Context_Compactor para requisições recebidas nessa rota.
6. WHEN o arquivo Route_Config for modificado em disco, THE Gateway SHALL detectar a alteração e aplicar a nova configuração (Hot_Reload) em até 5 segundos sem necessidade de reinicialização do serviço e sem interromper requisições em andamento.
7. WHEN o Gateway carregar ou recarregar a Route_Config, THE Gateway SHALL validar a estrutura do arquivo YAML verificando: presença dos campos obrigatórios por rota (`path`, `method`, `provider`), valores aceitos para `provider`, valores aceitos para `strategy` em `token_optimization`, e ausência de rotas duplicadas (mesma combinação `path` + `method`).
8. IF a validação da Route_Config falhar durante o carregamento inicial (startup), THEN THE Gateway SHALL recusar iniciar e registrar mensagem de erro detalhada indicando a linha e o campo com problema no arquivo YAML.
9. IF a validação da Route_Config falhar durante Hot_Reload (arquivo modificado em runtime), THEN THE Gateway SHALL manter a configuração anterior ativa, registrar um evento de alerta na camada de observabilidade indicando o erro de validação, e continuar operando com a configuração válida anterior.
10. THE Route_Config SHALL suportar definição de múltiplos provedores por rota com pesos de distribuição no campo `providers` (array de objetos com `name`, `weight` e `model`), permitindo balanceamento de carga por rota conforme definido no Requirement 4.
11. WHEN uma requisição HTTP for recebida, THE Gateway SHALL resolver a rota correspondente por matching exato de `path` e `method` definidos na Route_Config, e aplicar as configurações específicas daquela rota ao pipeline de processamento.
