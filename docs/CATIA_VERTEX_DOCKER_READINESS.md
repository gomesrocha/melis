# Melis — CatIA / Vertex Docker Readiness

`feature/catia-vertex-docker-readiness` — prepara o Melis para ser
consumido pelo CatIA Discovery (repo separado, não tocado por esta
feature exceto por config/env de runtime temporária de validação) como
imagem Docker dedicada, servindo os três aliases lógicos de produção
(`catia-bootstrap`, `catia-spec`, `catia-implementation`) roteados pra
Claude Sonnet 4.6 via Google Vertex AI, autenticado por ADC.

## Arquitetura (antes desta feature)

Melis é um gateway Rust (Axum) stateless: `router.rs` resolve rota por
(path, method) via `route_config.rs`/`routes.yaml`, seleciona um
provider configurado (`config.yaml`), traduz o payload OpenAI-shaped
pro formato nativo do provider via um `PayloadTranspiler`
(`transpiler/`), encaminha via `client.rs` (HTTP genérico), e traduz a
resposta de volta. Já suportava nativamente: OpenAI passthrough,
Anthropic direto (`x-api-key`), Ollama, e um transpiler pra Vertex/
Gemini (`transpiler/vertex.rs`) — mas **nunca um caminho pra Claude via
Vertex**, e **nenhuma autenticação Google ADC em lugar nenhum do
código** (confirmado: zero dependência `gcp_auth`/`yup-oauth2`/similar
no `Cargo.toml` antes desta feature).

## Achados reais desta auditoria (Fase 1-5)

1. **Alias resolution é por (path, method), não por `model`.** O
   campo `model` do payload só serve de fallback/override — o roteamento
   de verdade (`router.rs::resolve_route_config`) nunca olha pro alias
   pra ESCOLHER o provider. Como as três aliases CatIA mapeiam pro MESMO
   alvo físico inicial (Fase 6), isso é suficiente: uma única rota em
   `routes.yaml` cobre as três, com um `model:` override fixo — zero
   `if model == "catia-bootstrap"` espalhado em lógica de negócio.
2. **`validate_model_routing`/`find_provider_by_model` (routing.rs) são
   código morto** — nunca chamados do caminho real de request. Não
   tocado por esta feature (fora de escopo).
3. **`google_vertex_ai` (Gemini) está PARCIAL e quebrado no dispatch
   real**: o transpiler (`transpiler/vertex.rs`) existe e está correto
   isoladamente, mas `router.rs` só chama `to_native()`/`from_native()`
   pra `ptype == "anthropic"` — pra `google_vertex_ai`, o payload de
   saída NUNCA é traduzido (vai OpenAI-shaped cru pro endpoint Gemini,
   que rejeitaria) e a resposta não-streaming também não é traduzida de
   volta. **Não corrigido nesta feature** — fora de escopo (CatIA não
   precisa de Gemini via Melis; a exceção de geração de imagem já é
   direta ao Vertex do lado do CatIA, documentada e deliberada).
4. **`route_config.rs` tem sua PRÓPRIA whitelist de provider (`KNOWN_PROVIDERS`)**,
   separada da validação de `config.yaml` (`config.rs::valid_types`) —
   as duas precisam ficar em sincronia manualmente (achado real: minha
   primeira tentativa de subir o container falhou exatamente por só ter
   atualizado uma das duas). Ambas atualizadas nesta feature, comentário
   cruzado deixado em cada uma.
5. **Bug real e severo, pré-existente, encontrado via E2E de verdade**:
   `AnthropicTranspiler::to_native()` **nunca encaminhava `tools`/
   `tool_choice`** (caía no bucket "campo não suportado, log e
   descarta"). Isso quebra QUALQUER request de tool-calling — exatamente
   o que a geração de código do Implementation Service (`catia-implementation`)
   exige. Só foi descoberto porque este E2E chegou a disparar uma
   implementação real via Melis (não um smoke de texto simples) — nenhum
   teste existente (mockado) exercitava isso. **Corrigido nesta feature**
   (ver Fase 9 abaixo) — beneficia tanto `"anthropic"` (direto, já
   quebrado antes desta feature) quanto o novo `"vertex_anthropic"`.
6. **Redis é configurado mas nunca conectado em runtime** (`main.rs`
   sempre usa `LocalTokenBucket`, `/readyz` usa uma flag estática
   `true`, `MELIS_REDIS_URL` do `docker-compose.yml` de dev é uma env
   var nunca lida em código). Pré-existente, não relacionado a esta
   feature, não corrigido (fora de escopo) — documentado aqui só pra
   quem for depurar `/readyz` não refletir o Redis de verdade.
7. **`Dockerfile`'s `EXPOSE 8080` está desatualizado** — a porta real
   (`config.yaml.example`'s `server.port`) é 9090. `EXPOSE` é só
   metadado (não afeta o bind real), não corrigido (fora de escopo,
   achado cosmético).

## O que foi implementado

- **`src/vertex_auth.rs`** (novo): `VertexTokenCache`, wrapper fino e
  preguiçoso sobre `gcp_auth::provider()` — nunca faz parsing manual de
  JSON de service account nem geração manual de JWT/OAuth (Fase 7).
  Cadeia de descoberta real do `gcp_auth` 0.12.7: env var
  `GOOGLE_APPLICATION_CREDENTIALS` -> arquivo ADC do `gcloud` ->
  metadata server GCE/GKE -> CLI `gcloud`. Construção é instantânea/
  infalível; a descoberta de ADC só acontece na primeira chamada real
  (deployments sem provider Vertex nunca tentam ADC).
  **Ressalva verificada em código-fonte (não assumida)**: `gcp_auth`
  0.12.7 NÃO reconhece JSON de credencial `external_account` (Workload
  Identity Federation) — só `service_account`, arquivo ADC do gcloud,
  metadata server, ou `gcloud` CLI. A arquitetura-alvo real (Fase 36)
  precisa de WIF; **isso não foi validado por esta feature** — antes de
  depender disto pro bootstrap AWS/EKS, reverifique suporte a WIF na
  versão do `gcp_auth` em uso então (ou troque de biblioteca).
- **`src/transpiler/vertex_anthropic.rs`** (novo): `VertexAnthropicTranspiler`
  — delega `from_native`/`translate_chunk` pro `AnthropicTranspiler`
  inalterado (formato de resposta é idêntico ao da API direta da
  Anthropic — confirmado, não assumido); `to_native` chama o
  `AnthropicTranspiler` e depois remove `model` (Vertex codifica o
  modelo na URL) e insere `anthropic_version: "vertex-2023-10-16"`
  (valor fixo do Vertex, distinto do header `anthropic-version` da API
  direta).
- **`src/transpiler/anthropic.rs`** (achado + corrigido): tools/tool_choice
  agora traduzidos de verdade (OpenAI `tools[].function` -> Anthropic
  `tools[].input_schema`; `tool_choice` string/objeto -> `{type:
  "auto"|"tool", name}`; mensagens `assistant` com `tool_calls` viram
  blocos `tool_use`; mensagens `role: "tool"` viram `role: "user"` com
  bloco `tool_result`; resposta com blocos `tool_use` vira
  `message.tool_calls` no formato OpenAI; `stop_reason: "tool_use"` ->
  `finish_reason: "tool_calls"`). Streaming de tool-calling (SSE
  incremental `input_json_delta`) **não implementado** — CatIA e
  Implementation Service sempre mandam `stream: false` na prática, e
  replicar a reconstrução incremental de JSON parcial seria escopo
  especulativo não exercitado por nenhum caller real hoje.
- **`src/config.rs`**: `ProviderConfig` ganhou `project_id`/`region`
  (opcionais, só diagnóstico/log — a URL real vem inteira de
  `base_url`); `api_key` virou opcional (`vertex_anthropic` autentica
  via ADC, nunca chave estática); `"vertex_anthropic"` adicionado à
  lista de `provider_type` válidos.
- **`src/route_config.rs`**: `"vertex_anthropic"` adicionado a
  `KNOWN_PROVIDERS` (achado 4 acima).
- **`src/router.rs`**: novo branch de dispatch pra `ptype ==
  "vertex_anthropic"` — URL (`{base_url}/{model}:rawPredict` ou
  `:streamRawPredict`), header (`Authorization: Bearer <token ADC>`,
  nunca `x-api-key`/chave estática), tradução de payload/resposta via
  `get_transpiler("vertex_anthropic")`. Fail-closed real: erro de ADC
  nunca cai pra outro provider nem vaza o erro cru (que pode referenciar
  um path de credencial local) pro cliente HTTP — loga detalhe completo
  server-side, devolve `503 "Vertex AI authentication unavailable"`
  genérico (Fase 11/12).
- **`src/state.rs`/`src/main.rs`**: `AppState.vertex_token_cache`
  (novo, `Arc<VertexTokenCache>`), construído uma vez no startup.

## Roteamento das 3 aliases (config, não código)

Ver `deploy/catia-vertex-e2e/{config.yaml,routes.yaml}` (gitignorados,
mesma convenção do `config.yaml`/`routes.yaml` reais do repo — nunca
sobrescrevem o `config.yaml` real do desenvolvedor, que já tem outros
providers/secrets próprios). Um único provider `vertex_anthropic`
com `models: [catia-bootstrap, catia-spec, catia-implementation]`
(documentacional), e uma única rota `/v1/chat/completions` com
`model: "claude-sonnet-4-6"` override — as três aliases chegam no mesmo
path, saem todas com o mesmo modelo físico real. Otimização de custo
por alias fica pra depois (Fase 6, deliberadamente fora de escopo).

## Contrato de consumo — CatIA (não modificado)

```
MELIS_URL=http://<host-do-melis>:9090        # nome real: CATIA_MELIS_BASE_URL
MELIS_API_KEY=                                # vazio — auth.enabled=false nesta validação;
                                               # produção real deve ligar auth.enabled + api_keys
                                               # no config.yaml do Melis, e configurar
                                               # CATIA_MELIS_API_KEY do lado do CatIA
```

Validação local Docker: `CATIA_MELIS_BASE_URL=http://melis-catia-e2e:9090`
(nome do contêiner Melis numa rede Docker compartilhada — ver
"Integração local" abaixo), `CATIA_AI_PROVIDER=melis` (default real de
produção — nunca precisou virar `vertex` pra este teste passar).

## Contrato de consumo — Implementation Service (não modificado)

```
CATIA_MELIS_BASE_URL=http://<host-do-melis>:9090
CATIA_MELIS_API_KEY=
CATIA_MELIS_DEFAULT_MODEL=catia-implementation
```

Mesmo contrato HTTP (`POST /v1/chat/completions`, OpenAI-shaped) — já
confirmado compatível na integração real (ver evidência abaixo).

## Integração local real (Fase 25-27)

O stack Docker E2E do CatIA Discovery (feature anterior, repo
separado, `~/organizacao/repository/cateno/CatIA_Discovery/deploy/
docker-local-e2e/`) já estava rodando — conectado a este Melis via:

```bash
docker network connect melis-catia-e2e catia-e2e-web
docker network connect melis-catia-e2e catia-e2e-implementation-service
```

(ação manual/temporária, não parte do `up` normal de nenhum dos dois
stacks — documentada, não persistida como dependência rígida em
nenhum dos dois `compose.yml`, pra não quebrar quem rodar cada stack
sozinho). `CATIA_MELIS_BASE_URL`/`CATIA_AI_PROVIDER=melis` ajustados
no `.env.docker.local`/`compose.yml` do OUTRO repo (nunca no código
CatIA) — ver o commit desta feature nesse repo pra detalhe. Fonte da
verdade sobre CatIA continua o repo dele; aqui só o resultado:

**Prova de produção real (não bypass pra `provider=vertex`)**:
`CATIA_AI_PROVIDER=melis` (default real) → `MelisInterviewAgent` →
Melis (este contêiner) → alias `catia-bootstrap` → Vertex → Claude
Sonnet 4.6 → resposta coerente real, `usage` real (3106 tokens de
entrada, 138 de saída).

**Prova de implementação completa** (`catia-implementation`, via
Implementation Service real, `strategy: modelo_direto`, profile
`custom`, spec mínima deliberada — servidor HTTP Python puro com 2
rotas): `queued` → `generating` → `testing` (2 tentativas com teste
falho, corrigidas pelo loop real de fix) → `running` na 3ª tentativa.
Contêiner gerado real (`impl-svc-90002`) respondeu `GET /` → `hello-e2e`
(200) e `GET /healthz` → `ok` (200). Preview resolvido via
`GET /runtime/endpoint` (API real, nunca URL sintetizada pelo CatIA).
`destroy` via `POST /destroy` removeu contêiner+rede (confirmado via
`docker ps -a`/`docker network ls`), segunda chamada idempotente.

Isso fecha o achado "MELIS LOCAL E2E BLOCKED" da feature
`feature/docker-local-e2e` do repo CatIA_Discovery.

## Testes

Suite completa Melis (`cargo test`): **299 passed, 4 failed** — as 4
falhas são PRÉ-EXISTENTES e não-relacionadas (confirmado via `git
stash`/`cargo test` na `dev` original antes desta feature): os testes
de `integration_tests.rs` usam `RouteConfigManager::new_for_test()`
(rotas/providers vazios de propósito), então QUALQUER request de chat
completions ali sempre recebe `503 "All providers exhausted"` — não é
flakiness de rede nem regressão desta feature.

Novos testes desta feature (todos mockados, sem rede/ADC real, exceto
um best-effort): `vertex_auth::tests` (2), `transpiler::vertex_anthropic::tests`
(5), `transpiler::anthropic::tests` (+6 cobrindo tools/tool_choice nos
dois sentidos). `transpiler::proptest_tests::property_unsupported_fields_omitted::anthropic_omits_unsupported_fields`
corrigido — tinha `"tools"` hardcoded na lista de campos que "devem ser
descartados", codificando o próprio bug corrigido nesta feature como
comportamento esperado.

Smoke real (Fase 21/22, custo mínimo, nunca repetido desnecessariamente):
`catia-bootstrap`/`catia-spec`/`catia-implementation` — HTTP 200, todos
com `response.model == "claude-sonnet-4-6"` (evidência de roteamento
real, não só nome de alias), `usage` real presente. Streaming (SSE)
testado uma vez, funciona. Fail-closed sem ADC testado num contêiner
descartável separado (nunca o principal) — `503`, mensagem genérica,
zero vazamento de path/credencial (confirmado lendo os logs do
contêiner: `gcp_auth` tenta e falha no metadata server, 5 retries,
erro final "no available authentication method found", nada além
disso).

## Docker

Imagem: `melis:catia-e2e` (tag de validação, não SHA — working tree tem
mudanças não commitadas; após o commit desta feature, rebuilde com
`melis:$(git rev-parse --short HEAD)`). Build multi-stage já existente
reaproveitado sem alteração (musl estático -> `distroless/static-debian12:nonroot`,
~14.7MB, usuário `nonroot:nonroot`, sem credencial em nenhuma camada —
confirmado via scan de filesystem da imagem, zero match). Host port
7004 -> interno 9090 (porta real do binário, nunca alterada). Uso de
recursos: ~2.5MB RAM em idle (binário Rust estático). Restart limpo
confirmado (stateless — config vem de arquivos montados, recarregado
do zero, nenhum estado perdido porque não existe estado).

## ECR / EKS (preparação, nada publicado)

```
melis:<git-sha>
->
<account>.dkr.ecr.<region>.amazonaws.com/melis:<git-sha>
```

Nunca `:latest` no manifest final. Requisitos Kubernetes-friendly já
satisfeitos sem mudança: config via env/arquivo montado, logs
stdout/stderr, `/healthz`+`/readyz`, non-root, sem dependência de
filesystem do host, sem socket Docker, sem chave Google estática no
Dockerfile/imagem.

## Futuro: AWS EKS + Google Workload Identity Federation

```
Pod Melis no EKS
    -> AWS workload identity (IRSA/Pod Identity)
    -> Google Workload Identity Federation
    -> ADC (via gcp_auth::provider())
    -> Vertex AI
```

Nenhuma chave privada Google estática no EKS. **Nenhuma mudança de
código deveria ser necessária** — MAS ver a ressalva verificada acima
sobre `gcp_auth` 0.12.7 não reconhecer JSON `external_account`: essa
afirmação precisa ser reverificada contra a versão real do `gcp_auth`
em uso quando o bootstrap AWS acontecer, não assumida como já provada
por esta feature (que só validou o caminho local via
`GOOGLE_APPLICATION_CREDENTIALS`).

## Segurança — credenciais

Nunca: JSON de credencial impresso, `private_key`/`private_key_id`
logados, JSON copiado pro repositório, `COPY`/`ADD` de credencial no
Dockerfile, JSON embutido no Compose, chave privada em variável de
ambiente, base64 de credencial em código/config, path absoluto do
desenvolvedor commitado. Mount read-only confirmado em runtime (escrita
no arquivo montado falha com "Read-only file system"). Path real do
host só existe em `deploy/catia-vertex-e2e/.env.catia-vertex-e2e`
(gitignorado).
