# Melis AI Gateway

Gateway de IA stateless, de ultra-alta performance, escrito em Rust. Unifica múltiplos provedores de LLM sob uma API compatível com OpenAI.

## Features

- **API unificada** — endpoint compatível com OpenAI para todos os provedores
- **Multi-provedor** — OpenAI, Anthropic, Google Gemini, Grok (x.ai), DeepSeek, Ollama, OCI GenAI
- **Streaming SSE** — respostas token-a-token em tempo real
- **Compressão de contexto** — reduz tokens antes de enviar ao provedor
- **Load balancing** — distribuição ponderada entre provedores
- **Circuit breaker** — proteção contra provedores instáveis
- **Rate limiting** — Token Bucket por cliente (distribuído via Redis)
- **Métricas Prometheus** — observabilidade completa em `/metrics`
- **Hot-reload** — edite `routes.yaml` sem reiniciar
- **Graceful shutdown** — SIGTERM encerra conexões em andamento

## Quick Start

### Binário

```bash
# Baixar o release para sua plataforma em github.com/.../releases
tar xzf melis-gateway-*.tar.gz
cd melis-gateway-*/

# Configurar
cp config.yaml.example config.yaml
cp routes.yaml.example routes.yaml
# Edite config.yaml com suas API keys

# Rodar
./melis-gateway
```

### Docker

```bash
# Rodar com configuração customizada
docker run -d -p 9090:9090 \
  -v $(pwd)/config.yaml:/app/config.yaml:ro \
  -v $(pwd)/routes.yaml:/app/routes.yaml:ro \
  -e MELIS_SERVER_PORT=9090 \
  ghcr.io/seu-usuario/melis:latest
```

A imagem Docker inclui `config.yaml.example` e `routes.yaml.example` como defaults. Para customizar, monte seus próprios arquivos via volume (`-v`).

### Docker Compose (com Redis)

```bash
docker compose up -d
```

## Configuração

O gateway usa dois arquivos:

| Arquivo | Propósito | Hot-reload |
|---------|-----------|------------|
| `config.yaml` | Provedores, Redis, auth, server | Não (requer reinício) |
| `routes.yaml` | Rotas por endpoint, modelos, compressão | Sim (~5s) |

### Exemplo config.yaml

```yaml
server:
  port: 9090

providers:
  - id: "ollama"
    provider_type: "ollama"
    base_url: "http://localhost:11434"
    api_key: "ollama"
    models: ["llama3.2"]

  - id: "openai"
    provider_type: "openai"
    base_url: "https://api.openai.com/v1"
    api_key: "sk-proj-SUA_CHAVE"
    models: ["gpt-4o-mini"]

redis:
  cluster_urls: ["redis://localhost:6379"]
```

### Exemplo routes.yaml

```yaml
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "ollama"
    model: "llama3.2"
    token_optimization:
      strategy: "adaptive_trimming"
      compress_above_tokens: 4096

  - path: "/v1/chat/openai"
    method: "POST"
    provider: "openai"
    model: "gpt-4o-mini"
```

## Uso

```bash
# Chat JSON
curl -s http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"llama3.2","messages":[{"role":"user","content":"Olá!"}]}'

# Streaming
curl -N http://localhost:9090/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"model":"llama3.2","messages":[{"role":"user","content":"Olá!"}],"stream":true}'

# Métricas
curl http://localhost:9090/metrics

# Health
curl http://localhost:9090/healthz
curl http://localhost:9090/readyz
```

## Variáveis de Ambiente

| Variável | Padrão | Descrição |
|----------|--------|-----------|
| `MELIS_SERVER_PORT` | `9090` | Porta HTTP |
| `MELIS_SERVER_HOST` | `0.0.0.0` | Interface de bind |
| `MELIS_ROUTES_CONFIG` | `./routes.yaml` | Path do arquivo de rotas |
| `RUST_LOG` | `info` | Nível de log |

## Adicionar Novo Provedor

1. Adicione no `config.yaml` (seção `providers`)
2. Adicione uma rota no `routes.yaml`
3. Reinicie (ou aguarde hot-reload para `routes.yaml`)

Veja [docs/GUIDE.md](docs/GUIDE.md) para o guia completo.

## Build

```bash
# Compilar release
cargo build --release

# Testes
cargo test -- --test-threads=1

# Docker
docker build -t melis-gateway .

# Cross-compile (Linux)
make build-linux
make build-linux-arm
```

## Releases

Releases multiplataforma são gerados automaticamente via GitHub Actions quando uma tag `v*` é criada:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Ou manualmente: Actions → Release → Run workflow.

Plataformas geradas: Linux x86_64, Linux ARM64, macOS Intel, macOS Apple Silicon, Windows x86_64, Docker.

## Arquitetura

```
Client → Melis Gateway → LLM Provider
         │
         ├── Auth (Bearer token)
         ├── Rate Limiter (Token Bucket / Redis)
         ├── Context Compactor (pruning + stop-words)
         ├── Load Balancer (Weighted Round-Robin)
         ├── Circuit Breaker (distributed / Redis)
         ├── Payload Transpiler (OpenAI ↔ Anthropic ↔ Vertex)
         └── Metrics (Prometheus) + Tracing (OTLP)
```

## Licença

MIT
