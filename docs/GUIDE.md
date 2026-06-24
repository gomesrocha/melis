# Melis AI Gateway - Guia de Configuração e Deploy

## Índice

1. [Visão Geral](#visão-geral)
2. [Como Rodar](#como-rodar)
3. [Arquivos de Configuração](#arquivos-de-configuração)
4. [Como Adicionar Novos Provedores](#como-adicionar-novos-provedores)
5. [Referência de Parâmetros: config.yaml](#referência-configyaml)
6. [Referência de Parâmetros: routes.yaml](#referência-routesyaml)
7. [Deploy com Docker](#deploy-com-docker)
8. [Deploy com Kubernetes](#deploy-com-kubernetes)
9. [Exemplos de Uso (curl)](#exemplos-de-uso)

---

## Visão Geral

O Melis é um AI Gateway stateless escrito em Rust que unifica o acesso a múltiplos provedores de LLM sob uma API compatível com OpenAI. Ele atua como proxy reverso entre suas aplicações e os provedores (Ollama, OpenAI, Anthropic, Google Gemini, Grok, DeepSeek, etc.).

**Funcionalidades:**
- Endpoint unificado OpenAI-compatible (`POST /v1/chat/completions`)
- Rotas customizadas por provedor (`/v1/chat/grok`, `/v1/chat/claude`, etc.)
- Tradução automática de payload entre formatos (OpenAI ↔ Anthropic ↔ Vertex AI)
- Compressão adaptativa de contexto (redução de tokens)
- Rate limiting distribuído (Token Bucket via Redis)
- Circuit breaker com backoff exponencial
- Métricas Prometheus em `/metrics`
- Health checks Kubernetes (`/healthz`, `/readyz`)
- Hot-reload de rotas (edite `routes.yaml` sem reiniciar)
- Graceful shutdown (SIGTERM)

---

## Como Rodar

### Pré-requisitos

- Rust 1.83+ (para compilar)
- Redis (opcional, para rate limiting e circuit breaker distribuído)

### Compilar e Executar

```bash
# Compilar
cargo build --release

# Executar (usa config.yaml e routes.yaml do diretório atual)
./target/release/melis-gateway

# Com porta customizada via variável de ambiente
MELIS_SERVER_PORT=9090 ./target/release/melis-gateway
```

### Variáveis de Ambiente

| Variável | Descrição | Padrão |
|----------|-----------|--------|
| `MELIS_SERVER_PORT` | Porta HTTP do gateway | `9090` (ou o valor no config.yaml) |
| `MELIS_SERVER_HOST` | Host de bind | `0.0.0.0` |
| `MELIS_ROUTES_CONFIG` | Path do arquivo de rotas | `./routes.yaml` |
| `MELIS_OTLP_ENDPOINT` | Endpoint OTLP para tracing | `http://localhost:4317` |
| `RUST_LOG` | Nível de log | `info` |

---

## Arquivos de Configuração

O gateway usa dois arquivos:

| Arquivo | Propósito | Hot-reload? |
|---------|-----------|-------------|
| `config.yaml` | Configuração global (server, redis, providers, auth) | Não (requer reinício) |
| `routes.yaml` | Rotas por endpoint (path, provider, model, otimização) | Sim (~5 segundos) |

---

## Como Adicionar Novos Provedores

### Passo 1: Identifique o tipo do provedor

| Se a API do provedor é... | Use `provider_type` |
|---------------------------|---------------------|
| Compatível com OpenAI (ex: Grok, DeepSeek, Together, Groq, Fireworks) | `openai` |
| API oficial da OpenAI | `openai` |
| API oficial da Anthropic | `anthropic` |
| API do Google (Gemini via AI Studio) | `google_vertex_ai` |
| Oracle Cloud GenAI | `oci_genai` |
| Ollama local | `ollama` |

### Passo 2: Adicione o provedor no `config.yaml`

```yaml
providers:
  - id: "meu-provedor"          # Nome único (usado internamente)
    provider_type: "openai"      # Tipo (veja tabela acima)
    base_url: "https://api.provedor.com/v1"  # URL base da API
    api_key: "sk-sua-chave"      # API key
    weight: 1                    # Peso para load balancing
    timeout_secs: 60             # Timeout em segundos
    models:                      # Lista de modelos suportados
      - "modelo-1"
      - "modelo-2"
```

### Passo 3: Registre no `routes.yaml` (se for custom provider)

Se o nome do provedor não é um dos 5 built-in (`openai`, `anthropic`, `google_vertex_ai`, `oci_genai`, `ollama`), registre em `custom_providers`:

```yaml
custom_providers:
  - name: "meu-provedor"
    base_url: "https://api.provedor.com/v1"
    api_format: "openai_compatible"
```

### Passo 4: Crie uma rota no `routes.yaml`

```yaml
routes:
  - path: "/v1/chat/meu-provedor"   # URL que o cliente chama
    method: "POST"
    provider: "meu-provedor"         # Deve coincidir com o 'id' no config.yaml
    model: "modelo-1"                # Modelo padrão
    token_optimization:              # Opcional
      strategy: "adaptive_trimming"
      max_history_messages: 20
      compress_above_tokens: 4096
      local_tokenizer: "cl100k_base"
```

### Passo 5: Reinicie (ou aguarde hot-reload para routes.yaml)

```bash
pkill melis-gateway && ./target/release/melis-gateway
```

### Passo 6: Teste

```bash
curl -s http://localhost:9090/v1/chat/meu-provedor \
  -H "Content-Type: application/json" \
  -d '{"model":"modelo-1","messages":[{"role":"user","content":"Olá!"}]}'
```

---

## Referência: config.yaml

### `server`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `host` | string | `"0.0.0.0"` | Interface de rede para bind |
| `port` | int | `8080` | Porta HTTP |
| `max_payload_size` | int | `10485760` | Tamanho máximo de payload (bytes, 10MB) |
| `graceful_shutdown_timeout_secs` | int | `30` | Tempo para encerrar conexões ao receber SIGTERM |
| `max_concurrent_connections` | int | `5000` | Máximo de conexões simultâneas |

### `redis`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `cluster_urls` | list | — | URLs dos nós Redis (`redis://host:port`) |
| `pool_size` | int | `10` | Conexões por nó |
| `connect_timeout_secs` | int | `5` | Timeout de conexão |
| `command_timeout_secs` | int | `2` | Timeout por comando |

### `providers[]`

| Parâmetro | Tipo | Obrigatório | Descrição |
|-----------|------|-------------|-----------|
| `id` | string | Sim | Identificador único do provedor |
| `provider_type` | string | Sim | Tipo: `openai`, `anthropic`, `google_vertex_ai`, `oci_genai`, `ollama` |
| `base_url` | string | Sim | URL base da API do provedor |
| `api_key` | string | Sim | Chave de autenticação |
| `weight` | int | Não (1) | Peso para load balancing |
| `timeout_secs` | int | Não (30) | Timeout da chamada ao provedor |
| `models` | list | Não | Modelos disponíveis neste endpoint |

**URLs base por provedor:**

| Provedor | `base_url` |
|----------|-----------|
| OpenAI | `https://api.openai.com/v1` |
| Anthropic | `https://api.anthropic.com/v1` |
| Google Gemini | `https://generativelanguage.googleapis.com/v1beta/openai` |
| Grok (x.ai) | `https://api.x.ai/v1` |
| DeepSeek | `https://api.deepseek.com/v1` |
| Ollama | `http://localhost:11434` |
| Together AI | `https://api.together.xyz/v1` |
| Groq | `https://api.groq.com/openai/v1` |
| Fireworks | `https://api.fireworks.ai/inference/v1` |

### `rate_limit`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `burst_capacity` | int | `100` | Tokens máximos no bucket (rajada) |
| `refill_rate` | float | `10.0` | Tokens recarregados por segundo |

### `compactor`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `token_threshold` | int | `4096` | Limiar para ativar compressão (512–128000) |
| `stop_words` | list | `[]` | Palavras a remover do contexto |
| `tokenizer_name` | string | `"cl100k_base"` | Tokenizador para contagem |

### `circuit_breaker`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `failure_threshold_percent` | float | `50.0` | % de falhas para abrir circuito |
| `window_duration_secs` | int | `60` | Janela deslizante (segundos) |
| `min_requests_in_window` | int | `5` | Mínimo de requests antes de avaliar |
| `open_ttl_secs` | int | `30` | Tempo que o circuito fica aberto |
| `max_ttl_secs` | int | `300` | TTL máximo com backoff |
| `backoff_factor` | float | `2.0` | Fator de backoff exponencial |

### `observability`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `otlp_endpoint` | string | `"http://localhost:4317"` | Endpoint OTLP gRPC |
| `service_name` | string | `"melis-gateway"` | Nome do serviço nos traces |
| `enabled` | bool | `false` | Habilitar export OTLP |

### `auth`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `enabled` | bool | `false` | Habilitar autenticação de clientes |
| `api_keys[]` | list | `[]` | Lista de chaves autorizadas |
| `api_keys[].key` | string | — | A API key do cliente |
| `api_keys[].client_id` | string | — | Identificador do cliente |
| `api_keys[].allowed_models` | list | `[]` | Modelos permitidos (vazio = todos) |
| `api_keys[].rate_limit` | object | null | Override de rate limit por cliente |

---

## Referência: routes.yaml

### `custom_providers[]`

| Parâmetro | Tipo | Descrição |
|-----------|------|-----------|
| `name` | string | Nome do provedor customizado (referenciado nas rotas) |
| `base_url` | string | URL base (informativo) |
| `api_format` | string | Formato da API: `"openai_compatible"` ou `"custom"` |

### `routes[]`

| Parâmetro | Tipo | Obrigatório | Descrição |
|-----------|------|-------------|-----------|
| `path` | string | Sim | Path HTTP (ex: `/v1/chat/meu-bot`) |
| `method` | string | Sim | Método HTTP (`POST`) |
| `provider` | string | Sim* | Provedor para esta rota (id do config ou nome built-in) |
| `providers` | list | Sim* | Multi-provedor com pesos (alternativa a `provider`) |
| `model` | string | Não | Modelo padrão (override do payload) |
| `token_optimization` | object | Não | Configuração de compressão por rota |

*`provider` ou `providers` — pelo menos um é obrigatório.

### `routes[].providers[]` (multi-provedor)

| Parâmetro | Tipo | Descrição |
|-----------|------|-----------|
| `name` | string | Nome do provedor |
| `weight` | int | Peso para distribuição (maior = mais tráfego) |
| `model` | string | Modelo a usar neste provedor |

### `routes[].token_optimization`

| Parâmetro | Tipo | Padrão | Descrição |
|-----------|------|--------|-----------|
| `strategy` | string | — | `adaptive_trimming`, `sliding_window`, ou `none` |
| `max_history_messages` | int | `20` | Máximo de mensagens no histórico |
| `compress_above_tokens` | int | `4096` | Limiar de tokens para ativar compressão |
| `local_tokenizer` | string | `"cl100k_base"` | Tokenizador para contagem local |

---

## Deploy com Docker

### Build da imagem

```bash
docker build -t melis-gateway:latest .
```

### Executar standalone

```bash
docker run -d \
  --name melis-gateway \
  -p 9090:9090 \
  -v $(pwd)/config.yaml:/app/config.yaml:ro \
  -v $(pwd)/routes.yaml:/app/routes.yaml:ro \
  -e MELIS_SERVER_PORT=9090 \
  melis-gateway:latest
```

### Docker Compose (com Redis)

```bash
docker compose up -d
```

O `docker-compose.yml` já inclui:
- `melis-gateway` na porta 8080
- `redis:7-alpine` na porta 6379
- Health checks configurados
- Volume para `routes.yaml`

---

## Deploy com Kubernetes

### 1. ConfigMap para configuração

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: melis-gateway-config
data:
  config.yaml: |
    server:
      host: "0.0.0.0"
      port: 8080
    redis:
      cluster_urls:
        - "redis://redis-service:6379"
    auth:
      enabled: true
      api_keys:
        - key: "sk-melis-prod-key-001"
          client_id: "app-backend"
    routes_config_path: "/app/config/routes.yaml"

  routes.yaml: |
    custom_providers:
      - name: "grok"
        base_url: "https://api.x.ai/v1"
        api_format: "openai_compatible"
    routes:
      - path: "/v1/chat/completions"
        method: "POST"
        provider: "openai"
        model: "gpt-4o-mini"
```

### 2. Secret para API keys dos provedores

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: melis-gateway-secrets
type: Opaque
stringData:
  OPENAI_API_KEY: "sk-proj-..."
  ANTHROPIC_API_KEY: "sk-ant-..."
  GROK_API_KEY: "xai-..."
```

### 3. Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: melis-gateway
spec:
  replicas: 3
  selector:
    matchLabels:
      app: melis-gateway
  template:
    metadata:
      labels:
        app: melis-gateway
    spec:
      containers:
        - name: melis-gateway
          image: melis-gateway:latest
          ports:
            - containerPort: 8080
          env:
            - name: RUST_LOG
              value: "info"
          volumeMounts:
            - name: config
              mountPath: /app/config
              readOnly: true
          livenessProbe:
            httpGet:
              path: /healthz
              port: 8080
            initialDelaySeconds: 5
            periodSeconds: 10
          readinessProbe:
            httpGet:
              path: /readyz
              port: 8080
            initialDelaySeconds: 5
            periodSeconds: 5
          resources:
            requests:
              memory: "32Mi"
              cpu: "100m"
            limits:
              memory: "128Mi"
              cpu: "500m"
      volumes:
        - name: config
          configMap:
            name: melis-gateway-config
```

### 4. Service

```yaml
apiVersion: v1
kind: Service
metadata:
  name: melis-gateway
spec:
  selector:
    app: melis-gateway
  ports:
    - port: 80
      targetPort: 8080
  type: ClusterIP
```

### 5. HPA (Auto-scaling)

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: melis-gateway-hpa
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: melis-gateway
  minReplicas: 2
  maxReplicas: 10
  metrics:
    - type: Resource
      resource:
        name: cpu
        target:
          type: Utilization
          averageUtilization: 70
```

### Aplicar no cluster

```bash
kubectl apply -f k8s/configmap.yaml
kubectl apply -f k8s/secret.yaml
kubectl apply -f k8s/deployment.yaml
kubectl apply -f k8s/service.yaml
kubectl apply -f k8s/hpa.yaml
```

---

## Exemplos de Uso

### Ollama (local)
```bash
curl -s http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"llama3.2","messages":[{"role":"user","content":"Olá!"}]}'
```

### OpenAI
```bash
curl -s http://localhost:9090/v1/chat/openai \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"Hello!"}]}'
```

### Claude (Anthropic)
```bash
curl -s http://localhost:9090/v1/chat/claude \
  -H "Content-Type: application/json" \
  -d '{"model":"claude-sonnet-4-6","messages":[{"role":"user","content":"Hello!"}]}'
```

### Grok (x.ai)
```bash
curl -s http://localhost:9090/v1/chat/grok \
  -H "Content-Type: application/json" \
  -d '{"model":"grok-3-mini","messages":[{"role":"user","content":"Hello!"}]}'
```

### DeepSeek
```bash
curl -s http://localhost:9090/v1/chat/deepseek \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-chat","messages":[{"role":"user","content":"Hello!"}]}'
```

### Streaming (qualquer provedor)
```bash
curl -N http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"llama3.2","messages":[{"role":"user","content":"Conte uma piada"}],"stream":true}'
```

### Métricas
```bash
curl http://localhost:9090/metrics
```

### Health Checks
```bash
curl http://localhost:9090/healthz   # Liveness
curl http://localhost:9090/readyz    # Readiness
```

### Com Python (SDK OpenAI)
```python
from openai import OpenAI

client = OpenAI(
    base_url="http://localhost:9090/v1/chat/completions",
    api_key="não-necessário"
)

# Usa Ollama por padrão (rota /v1/chat/completions)
response = client.chat.completions.create(
    model="llama3.2",
    messages=[{"role": "user", "content": "Olá!"}]
)
print(response.choices[0].message.content)
```
