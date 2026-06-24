# Implementation Plan: Melis AI Gateway

## Overview

Implementação incremental do Melis AI Gateway em Rust com Axum 0.7+, Tokio, Redis (fred), HuggingFace Tokenizer, reqwest com SSE streaming e observabilidade completa via OpenTelemetry + Prometheus. Cada tarefa constrói sobre as anteriores, integrando componentes ao pipeline progressivamente.

## Tasks

- [x] 1. Scaffolding do projeto e infraestrutura base
  - [x] 1.1 Criar estrutura do projeto Cargo e dependências
    - Criar `Cargo.toml` com workspace/dependencies: axum 0.7+, tokio, serde/serde_json, fred, tokenizers, reqwest, tracing, tracing-opentelemetry, opentelemetry, tower, tower-http, proptest (dev), wiremock (dev), testcontainers (dev)
    - Criar `src/main.rs` com entrypoint básico Tokio multi-thread
    - Criar módulos: `src/config.rs`, `src/router.rs`, `src/error.rs`
    - Definir estrutura de diretórios: `src/middleware/`, `src/transpiler/`, `src/`
    - _Requirements: 8.1, 10.1_

  - [x] 1.2 Implementar carregamento de configuração (`config.rs`)
    - Definir structs `GatewayConfig`, `ServerConfig`, `ProviderConfig`, `RedisConfig`, `CircuitBreakerConfig`, `CompactorConfig`, `RateLimitDefaults`, `OtelConfig`, `AuthConfig` com serde Deserialize
    - Carregar configuração de arquivo YAML e variáveis de ambiente
    - Incluir campo `routes_config_path: PathBuf` em `GatewayConfig` com leitura da variável de ambiente `MELIS_ROUTES_CONFIG` (padrão: `./routes.yaml`)
    - Validar valores obrigatórios e ranges (ex: token_threshold entre 512–128000)
    - _Requirements: 3.1, 4.4, 5.1, 6.1, 11.1_

  - [x] 1.3 Implementar modelos de dados core e tipos de erro
    - Criar `src/models.rs` com structs `OpenAiRequest<'a>`, `Message<'a>`, `OpenAiResponse<'a>`, `Choice<'a>`, `Usage`, `OpenAiChunk<'a>`, `ChunkChoice<'a>`, `Delta<'a>` usando `Cow<'a, str>` para zero-copy
    - Criar `src/error.rs` com `ErrorResponse`, `ErrorDetail` e enum de erros internos que implementa `IntoResponse` para Axum
    - _Requirements: 1.1, 2.4, 8.1_

- [x] 2. Route Config Loader (YAML + Hot-Reload)
  - [x] 2.1 Implementar modelos de dados e parsing da Route Config (`route_config.rs`)
    - Criar structs `RouteConfigFile`, `RouteDefinition`, `WeightedProvider`, `TokenOptimizationConfig`, `CustomProviderDef` com serde Deserialize
    - Implementar enums `ProviderType` (openai, anthropic, google_vertex_ai, oci_genai, ollama, Custom(String)) e `TokenOptimizationStrategy` (adaptive_trimming, sliding_window, none)
    - Implementar parsing via `serde_yaml` do arquivo `routes.yaml`
    - Implementar valores default para `TokenOptimizationConfig` (max_history_messages: 20, compress_above_tokens: 4096, local_tokenizer: "cl100k_base")
    - Dependência: `serde_yaml = "0.9"`
    - _Requirements: 11.1, 11.2, 11.4, 11.10_

  - [x] 2.2 Implementar validação da Route Config
    - Criar enum `RouteConfigError` com variantes: `IoError`, `ParseError`, `MissingField`, `InvalidProvider`, `InvalidStrategy`, `DuplicateRoute`
    - Validar campos obrigatórios por rota (`path`, `method`, `provider` ou `providers`)
    - Validar valores aceitos para `provider` contra lista fixa + `custom_providers` registrados
    - Validar valores de `strategy` em `token_optimization` (adaptive_trimming, sliding_window, none)
    - Detectar rotas duplicadas (mesma combinação `path` + `method`)
    - Produzir mensagens de erro descritivas com índice da rota e campo problemático
    - _Requirements: 11.7, 11.8_

  - [x] 2.3 Implementar resolução de rota (matching exato path + method)
    - Implementar método `resolve_route(path: &str, method: &str) -> Option<&RouteDefinition>`
    - Matching exato por combinação `path` + `method` (case-sensitive para path, case-insensitive para method)
    - Implementar método `effective_token_config(route, global_config) -> CompactorConfig` que retorna config per-route se `token_optimization` definida, ou config global caso contrário
    - Usar HashMap interno para lookup O(1) por chave `(path, method)`
    - _Requirements: 11.3, 11.4, 11.5, 11.11_

  - [x] 2.4 Implementar hot-reload via notify + ArcSwap (`RouteConfigManager`)
    - Criar struct `RouteConfigManager` com `Arc<ArcSwap<RouteConfigFile>>` para configuração atual
    - Implementar carregamento inicial: se validação falhar, recusar iniciar (panic com mensagem descritiva)
    - Integrar `notify::RecommendedWatcher` (crate `notify 6.1`) para detectar modificações no arquivo
    - Spawnar task Tokio para processar eventos de file change do watcher
    - No callback de mudança: ler, parsear, validar novo YAML → se válido: `ArcSwap::store` atômico; se inválido: manter anterior e emitir alerta
    - Método `current()` retorna `arc_swap::Guard<Arc<RouteConfigFile>>` (lock-free, sem bloquear requisições)
    - Dependências: `notify = "6.1"`, `arc-swap = "1.7"`
    - _Requirements: 11.6, 11.8, 11.9_

  - [x] 2.5 Integrar Route Config no pipeline (substituir lookups estáticos)
    - Adicionar `RouteConfigManager` ao `AppState`
    - No handler de chat completions, resolver rota via `resolve_route(path, method)` antes de prosseguir no pipeline
    - Aplicar override de `model` se definido na rota (substituir valor do payload)
    - Passar `effective_token_config` ao Context Compactor ao invés de config global fixa
    - Utilizar lista de provedores/pesos da rota para Load Balancer (se `providers` multi-provedor definido)
    - _Requirements: 11.3, 11.4, 11.5, 11.11_

  - [x]* 2.6 Escrever property test para validação da Route Config (Property 16)
    - **Property 16: Validação Estrutural da Route Config**
    - Gerar YAML configs válidos e inválidos aleatoriamente usando proptest (campos ausentes, providers desconhecidos, strategies inválidas, rotas duplicadas)
    - Verificar que configs válidos produzem `RouteConfigFile` sem erro e configs inválidos retornam erros descritivos específicos
    - **Validates: Requirements 11.1, 11.2, 11.7, 11.10**

  - [x]* 2.7 Escrever property test para override de modelo (Property 17)
    - **Property 17: Override de Modelo pela Rota**
    - Gerar rotas com campo `model` definido e payloads com `model` diferente
    - Verificar que o modelo da rota substitui o modelo do payload antes do encaminhamento
    - **Validates: Requirements 11.3**

  - [x]* 2.8 Escrever property test para resolução de token optimization (Property 18)
    - **Property 18: Resolução de Token Optimization (Per-Route vs Global)**
    - Gerar rotas com/sem seção `token_optimization` e config global variada
    - Verificar: se `token_optimization` presente na rota → usa config da rota; se ausente → usa config global
    - **Validates: Requirements 11.4, 11.5**

  - [x]* 2.9 Escrever property test para hot-reload seguro (Property 19)
    - **Property 19: Hot-Reload Seguro (Config Inválida Preserva Anterior)**
    - Gerar pares (config válida ativa, config inválida para reload) usando `tempfile`
    - Verificar que após reload inválido a config ativa permanece inalterada
    - Dependência dev: `tempfile = "3.10"`
    - **Validates: Requirements 11.9**

  - [x]* 2.10 Escrever property test para matching de rota (Property 20)
    - **Property 20: Resolução de Rota por Matching Exato**
    - Gerar tabelas de rotas e requisições HTTP com path/method aleatórios
    - Verificar: matching exato resolve rota correta; paths/methods sem match retornam None
    - **Validates: Requirements 11.11**

- [x] 3. HTTP Router e endpoint handlers
  - [x] 3.1 Implementar router Axum e handler de chat completions
    - Criar `src/router.rs` com `build_router(state: AppState) -> Router`
    - Registrar rota `POST /v1/chat/completions` com handler `chat_completions_handler`
    - Implementar validação de payload: campos `model` e `messages` obrigatórios, `messages` não-vazio, cada mensagem com `role` e `content`
    - Configurar limite de payload de 10MB via `DefaultBodyLimit`
    - Retornar HTTP 400 com detalhes para payload inválido, HTTP 413 para payload excedendo 10MB
    - _Requirements: 1.1, 1.5, 1.6_

  - [x] 3.2 Implementar detecção de modo streaming vs JSON
    - No handler de chat completions, inspecionar header `Accept: text/event-stream`
    - Se presente: retornar resposta SSE com content-type `text/event-stream`
    - Se ausente: retornar resposta JSON com content-type `application/json`
    - _Requirements: 1.2, 1.3_

  - [x]* 3.3 Escrever property test para validação de payload (Property 1)
    - **Property 1: Validação de Payload Rejeita Entradas Inválidas**
    - Gerar payloads com campos aleatoriamente ausentes/vazios usando proptest
    - Verificar que todo payload sem `model` ou `messages` válido retorna 400
    - **Validates: Requirements 1.5**

- [x] 4. Auth Middleware
  - [x] 4.1 Implementar middleware de autenticação (`middleware/auth.rs`)
    - Criar trait `AuthValidator` com método `validate(token: &str) -> Result<ClientIdentity, AuthError>`
    - Implementar extração do token do header `Authorization: Bearer <token>`
    - Retornar HTTP 401 se header ausente ou credenciais inválidas/expiradas/revogadas
    - Struct `ClientIdentity` com `client_id`, `rate_limit_config`, `allowed_models`
    - Integrar como Tower layer no router
    - _Requirements: 7.1, 7.2, 7.3_

  - [x] 4.2 Implementar roteamento modelo-para-provedor
    - Após autenticação, validar campo `model` contra mapeamento configurado
    - Retornar HTTP 400 se modelo não reconhecido no mapeamento
    - Associar provedor selecionado ao contexto da requisição
    - _Requirements: 7.4, 7.5_

  - [x]* 4.3 Escrever property test para autenticação (Property 14)
    - **Property 14: Corretude da Autenticação**
    - Gerar tokens válidos e inválidos aleatórios
    - Verificar que tokens válidos identificam cliente e inválidos retornam 401
    - **Validates: Requirements 7.2, 7.3**

  - [x]* 4.4 Escrever property test para roteamento (Property 15)
    - **Property 15: Roteamento Modelo-para-Provedor**
    - Gerar mapeamentos e modelos aleatórios
    - Verificar que modelo presente no mapeamento roteia ao provedor correto
    - **Validates: Requirements 7.4**

- [x] 5. Rate Limiter distribuído (Redis Token Bucket)
  - [x] 5.1 Implementar Rate Limiter com Lua script (`middleware/rate_limiter.rs`)
    - Criar trait `RateLimiter` com método `try_acquire(client_id: &str) -> Result<u64, RateLimitExceeded>`
    - Implementar `RedisTokenBucket` usando fred com Lua script atômico conforme design
    - Script Lua: verificar/recarregar tokens, consumir 1 token, retornar remaining ou wait time
    - Chaves Redis: `melis:rl:{client_id}` (HASH com tokens e last_refill, TTL 3600s)
    - _Requirements: 6.1_

  - [x] 5.2 Integrar Rate Limiter como Tower middleware
    - Criar Tower layer que extrai `client_id` do contexto e chama `try_acquire`
    - Retornar HTTP 429 com header `Retry-After` quando limite excedido
    - Implementar fail-open: se Redis indisponível, permitir passagem e registrar alerta
    - Suportar configuração distinta por cliente via `RateLimitConfig`
    - _Requirements: 6.2, 6.3, 6.4, 6.5_

  - [x]* 5.3 Escrever property test para Token Bucket (Property 13)
    - **Property 13: Corretude do Token Bucket**
    - Gerar configurações (capacidade, refill_rate) e sequências de requisições no tempo
    - Verificar: permite quando tokens > 0, rejeita com Retry-After correto quando 0, nunca tokens negativos, limites distintos por cliente
    - **Validates: Requirements 6.1, 6.2, 6.4**

- [x] 6. Checkpoint - Validar infraestrutura base
  - Ensure all tests pass, ask the user if questions arise.

- [x] 7. Context Compactor (HF Tokenizer)
  - [x] 7.1 Implementar Context Compactor (`compactor.rs`)
    - Criar trait `ContextCompactor` com método `compact(messages, config) -> CompactionResult`
    - Integrar HuggingFace Tokenizer SDK para contagem precisa de tokens
    - Implementar algoritmo: contar tokens → se abaixo do limiar retornar sem modificação → identificar mensagens elegíveis (excluir system e última user) → remover stop-words → podar mais antigas até atingir 25% de redução
    - Preservar intactas todas mensagens system e última mensagem user
    - Manter ordem cronológica das mensagens restantes
    - Se redução de 25% não atingida, encaminhar com melhor compressão obtida
    - _Requirements: 3.1, 3.2, 3.3, 3.4, 3.5, 3.6, 3.7_

  - [x]* 7.2 Escrever property test para preservação de invariantes do Compactor (Property 5)
    - **Property 5: Context Compactor Preserva Mensagens Invariantes e Poda Corretamente**
    - Gerar históricos com mix de roles (system, user, assistant) e tamanhos variados
    - Verificar: system intactas, última user intacta, ordem preservada, poda apenas elegíveis começando pelas mais antigas
    - **Validates: Requirements 3.1, 3.4**

  - [x]* 7.3 Escrever property test para redução mínima (Property 6)
    - **Property 6: Context Compactor Atinge Redução Mínima de 25%**
    - Gerar históricos grandes com mensagens prunáveis suficientes
    - Verificar que tokens_finais ≤ 75% × tokens_originais
    - **Validates: Requirements 3.3**

  - [x]* 7.4 Escrever property test para remoção de stop-words (Property 7)
    - **Property 7: Context Compactor Remove Stop-Words**
    - Gerar mensagens com stop-words inseridas aleatoriamente
    - Verificar que nenhuma stop-word permanece na saída e conteúdo não-stop-word preservado
    - **Validates: Requirements 3.2**

  - [x]* 7.5 Escrever property test para identidade sub-limiar (Property 8)
    - **Property 8: Context Compactor é Identidade Abaixo do Limiar**
    - Gerar históricos pequenos (< limiar tokens)
    - Verificar que output == input sem modificação
    - **Validates: Requirements 3.6**

- [x] 8. Payload Transpiler (Zero-Copy)
  - [x] 8.1 Implementar trait e estrutura base do Transpiler (`transpiler/mod.rs`)
    - Criar trait `PayloadTranspiler` com métodos `to_native`, `from_native`, `translate_chunk`
    - Definir structs `NativeRequest<'a>`, `NativeResponse<'a>`, `NativeChunk<'a>` com lifetimes para zero-copy
    - Implementar factory para selecionar transpiler por `ProviderType`
    - _Requirements: 2.4_

  - [x] 8.2 Implementar Transpiler Anthropic (`transpiler/anthropic.rs`)
    - Conversão OpenAI → Anthropic Messages API: extrair system como campo separado, mapear roles, `max_tokens` obrigatório
    - Conversão Anthropic → OpenAI para resposta completa e chunks SSE individuais
    - Utilizar `Cow<'a, str>` para zero-copy em campos não transformados
    - Omitir campos sem equivalente e registrar warning
    - _Requirements: 2.1, 2.3, 2.4, 2.6_

  - [x] 8.3 Implementar Transpiler Vertex AI (`transpiler/vertex.rs`)
    - Conversão OpenAI → Google Generative AI API: mapear messages→contents/parts, params→generationConfig
    - Conversão Vertex AI → OpenAI para resposta completa e chunks SSE
    - Zero-copy via `Cow<'a, str>` para campos transferidos sem modificação
    - Omitir campos sem equivalente e registrar warning
    - _Requirements: 2.2, 2.3, 2.4, 2.6_

  - [x] 8.4 Implementar Transpiler OpenAI passthrough (`transpiler/openai.rs`)
    - Passthrough sem conversão para requisições direcionadas a provedores OpenAI
    - _Requirements: 1.4_

  - [x]* 8.5 Escrever property test para round-trip do Transpiler (Property 2)
    - **Property 2: Round-Trip do Payload Transpiler**
    - Gerar payloads OpenAI válidos com model, messages, temperature, max_tokens, stop
    - Verificar: to_native → from_native preserva valores dos campos testados
    - **Validates: Requirements 2.5**

  - [x]* 8.6 Escrever property test para validade estrutural nativa (Property 3)
    - **Property 3: Validade Estrutural da Saída Nativa do Transpiler**
    - Gerar payloads OpenAI com todas combinações de campos
    - Verificar estrutura válida do output Anthropic e Vertex AI
    - **Validates: Requirements 2.1, 2.2**

  - [x]* 8.7 Escrever property test para campos não-suportados (Property 4)
    - **Property 4: Campos Não-Suportados São Omitidos Graciosamente**
    - Gerar payloads com campos extras aleatórios
    - Verificar que campos extras são omitidos sem interromper processamento
    - **Validates: Requirements 2.6**

- [x] 9. Load Balancer (Weighted Round-Robin)
  - [x] 9.1 Implementar Load Balancer (`balancer.rs`)
    - Criar trait `LoadBalancer` com métodos `select_provider(model)` e `update_weights`
    - Implementar `WeightedRoundRobin` com `ArcSwap<Vec<ProviderEndpoint>>` para hot-reload
    - Distribuir requisições proporcionalmente aos pesos com tolerância ≤ 5% em 1000 requisições
    - Integrar com Circuit Breaker para excluir provedores indisponíveis
    - Retornar HTTP 503 quando todos provedores indisponíveis
    - _Requirements: 4.1, 4.3, 4.4_

  - [x] 9.2 Implementar failover com retry
    - Quando provedor retornar 5xx ou timeout, redirecionar para próximo disponível
    - Máximo 2 tentativas de failover por requisição
    - Timeout configurável por provedor (padrão: 30s)
    - _Requirements: 4.2_

  - [x]* 9.3 Escrever property test para distribuição ponderada (Property 9)
    - **Property 9: Distribuição Ponderada Dentro da Tolerância**
    - Gerar configurações de peso aleatórias e simular 1000 seleções
    - Verificar desvio ≤ 5% da proporção ideal
    - **Validates: Requirements 4.1**

  - [x]* 9.4 Escrever property test para failover (Property 10)
    - **Property 10: Failover Seleciona Próximo Provedor Disponível**
    - Gerar conjuntos de provedores com falhas aleatórias
    - Verificar: redireciona para próximo disponível, max 2 retries
    - **Validates: Requirements 4.2**

- [x] 10. Circuit Breaker Distribuído (Redis)
  - [x] 10.1 Implementar Circuit Breaker (`circuit_breaker.rs`)
    - Criar trait `CircuitBreaker` com métodos `is_available`, `record_result`, `try_half_open`
    - Implementar estados Closed → Open → HalfOpen com Redis
    - Abrir circuito quando taxa de falhas > threshold na janela deslizante (ou HTTP 429 recebido)
    - Chaves Redis: `melis:cb:{provider_id}:state` (TTL), `melis:cb:{provider_id}:failures` (ZSET)
    - Mínimo de requisições na janela antes de avaliar threshold
    - Fail-open quando Redis indisponível
    - _Requirements: 5.1, 5.2, 5.6_

  - [x] 10.2 Implementar half-open e backoff exponencial
    - Após TTL expirar, enviar 1 requisição de teste (half-open)
    - Se sucesso (2xx): remover flag, restaurar provedor
    - Se falha (5xx/429/timeout): renovar flag com TTL × backoff_factor, até max_ttl
    - _Requirements: 5.3, 5.4, 5.5_

  - [x]* 10.3 Escrever property test para abertura do Circuit Breaker (Property 11)
    - **Property 11: Circuit Breaker Abre Quando Limiar de Falhas é Excedido**
    - Gerar sequências de sucesso/falha aleatórias com diferentes thresholds
    - Verificar que circuito abre quando taxa de falhas excede threshold
    - **Validates: Requirements 5.1, 5.2**

  - [x]* 10.4 Escrever property test para backoff do Circuit Breaker (Property 12)
    - **Property 12: Cálculo de TTL com Backoff Exponencial**
    - Gerar TTLs e fatores de backoff aleatórios
    - Verificar: novo_ttl = min(ttl_atual × fator, max_ttl)
    - **Validates: Requirements 5.5**

- [x] 11. Checkpoint - Validar componentes core
  - Ensure all tests pass, ask the user if questions arise.

- [x] 12. HTTP Client com SSE Streaming
  - [x] 12.1 Implementar cliente HTTP para provedores (`client.rs`)
    - Criar trait `LlmHttpClient` com métodos `send` (não-streaming) e `send_stream` (SSE)
    - Implementar usando reqwest com connection pooling e TLS
    - `send`: envia POST, aguarda resposta completa, retorna Bytes
    - `send_stream`: envia POST, retorna `Pin<Box<dyn Stream<Item = Result<Bytes, ClientError>> + Send>>`
    - Configurar timeouts por provedor
    - _Requirements: 1.2, 1.3, 1.4, 2.3_

  - [x] 12.2 Integrar streaming SSE no handler de resposta
    - Converter chunks SSE do provedor para formato OpenAI via Transpiler
    - Cada chunk convertido individualmente e encaminhado sem aguardar resposta completa
    - Implementar encerramento limpo do stream em caso de erro
    - _Requirements: 1.2, 2.3_

- [x] 13. Observabilidade (Métricas + Tracing)
  - [x] 13.1 Implementar métricas Prometheus (`observability.rs`)
    - Registrar métricas: `melis_gateway_requests_total` (counter com labels route, client, status), `melis_llm_tokens_total` (counter com labels direction, model, client_id), `melis_context_compression_ratio` (histogram), `melis_backend_latency_seconds` (histogram com buckets específicos), `melis_gateway_overhead_seconds` (histogram com buckets específicos)
    - Implementar endpoint `GET /metrics` com format Prometheus text exposition
    - Retornar HTTP 503 se subsistema de métricas falhar
    - _Requirements: 9.1, 9.2, 9.3, 9.4, 9.5, 9.6, 9.9_

  - [x] 13.2 Implementar tracing distribuído com OTLP
    - Configurar pipeline tracing-opentelemetry com exporter OTLP
    - Criar span raiz por requisição com atributos: client_id, model, provider, status
    - Criar spans filhos para cada etapa do pipeline (auth, rate_limit, compressor, transpiler, provider_call)
    - Propagar trace context entre requisições do cliente e chamadas ao provedor
    - _Requirements: 9.7, 9.8_

  - [x] 13.3 Instrumentar componentes com métricas e spans
    - Adicionar instrumentação de métricas em: router (requests_total, overhead), compactor (compression_ratio), client (backend_latency, tokens_total)
    - Adicionar spans em cada middleware/handler do pipeline
    - _Requirements: 9.2, 9.3, 9.4, 9.5, 9.6, 9.8_

- [x] 14. Health Checks e Graceful Shutdown
  - [x] 14.1 Implementar endpoints de health check
    - `GET /healthz` (liveness): retorna HTTP 200 se processo respondendo
    - `GET /readyz` (readiness): retorna HTTP 200 se Redis conectado, HTTP 503 se Redis indisponível
    - _Requirements: 10.3, 10.4, 10.5_

  - [x] 14.2 Implementar graceful shutdown
    - Capturar sinal SIGTERM
    - Parar de aceitar novas conexões
    - Aguardar até 30 segundos para conclusão de requisições em andamento
    - Encerrar processo após timeout ou conclusão
    - _Requirements: 10.6_

- [x] 15. Wiring completo do pipeline
  - [x] 15.1 Integrar todos os componentes no pipeline
    - Montar pipeline completo no `main.rs`: Router → Auth → RateLimiter → Compactor → LoadBalancer → CircuitBreaker → Transpiler → HttpClient
    - Configurar `AppState` com todas as dependências injetadas
    - Aplicar hot-reload de configuração (pesos de provedores) em até 5s
    - Configurar max 5000 conexões concorrentes
    - _Requirements: 1.4, 4.4, 8.3, 8.5_

  - [x]* 15.2 Escrever testes de integração end-to-end
    - Testar fluxo completo: request → response (JSON e SSE) via wiremock como mock provider
    - Testar failover: provider 1 falha → provider 2 responde
    - Testar graceful shutdown: SIGTERM → conexões existentes completam
    - Usar testcontainers para Redis nos testes de integração
    - _Requirements: 1.2, 1.3, 1.4, 4.2, 10.6_

- [x] 16. Imagem Docker otimizada
  - [x] 16.1 Criar Dockerfile multi-stage
    - Stage 1: build com imagem Rust oficial, compilação estática com `musl` target
    - Stage 2: imagem final baseada em `scratch` ou `distroless`
    - Tamanho final < 50MB
    - Configurar ENTRYPOINT para o binário
    - Expor porta 8080
    - _Requirements: 10.1, 10.2_

- [x] 17. Checkpoint final - Validação completa
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marcadas com `*` são opcionais e podem ser ignoradas para um MVP mais rápido
- Cada task referencia requirements específicos para rastreabilidade
- Checkpoints garantem validação incremental do progresso
- Property tests validam propriedades universais de corretude com proptest (100+ iterações)
- Unit tests validam exemplos específicos e edge cases
- Integration tests utilizam wiremock (mock HTTP) e testcontainers (Redis)
- O projeto usa `Cow<'a, str>` extensivamente para minimizar alocações no hot path
- fred é o Redis client escolhido por suporte a cluster e multiplexing
- Route Config usa `serde_yaml` para parsing, `notify 6.1` para file watching e `arc-swap 1.7` para swap atômico lock-free
- `tempfile` (dev) é utilizado para testes de hot-reload de configuração

## Task Dependency Graph

```json
{
  "waves": [
    { "id": 0, "tasks": ["1.1"] },
    { "id": 1, "tasks": ["1.2", "1.3"] },
    { "id": 2, "tasks": ["2.1"] },
    { "id": 3, "tasks": ["2.2", "2.3"] },
    { "id": 4, "tasks": ["2.4", "2.6", "2.7", "2.8", "2.10"] },
    { "id": 5, "tasks": ["2.5", "2.9", "3.1", "4.1", "5.1"] },
    { "id": 6, "tasks": ["3.2", "3.3", "4.2", "5.2"] },
    { "id": 7, "tasks": ["4.3", "4.4", "5.3", "7.1", "8.1"] },
    { "id": 8, "tasks": ["7.2", "7.3", "7.4", "7.5", "8.2", "8.3", "8.4"] },
    { "id": 9, "tasks": ["8.5", "8.6", "8.7", "9.1"] },
    { "id": 10, "tasks": ["9.2", "9.3", "9.4", "10.1"] },
    { "id": 11, "tasks": ["10.2", "10.3", "10.4"] },
    { "id": 12, "tasks": ["12.1"] },
    { "id": 13, "tasks": ["12.2", "13.1"] },
    { "id": 14, "tasks": ["13.2", "13.3", "14.1", "14.2"] },
    { "id": 15, "tasks": ["15.1", "16.1"] },
    { "id": 16, "tasks": ["15.2"] }
  ]
}
```
