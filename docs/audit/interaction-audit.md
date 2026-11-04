# Auditoria de interações (pré-experimento)

Auditoria focada nas interações do gateway antes do experimento, conforme
solicitado: isolamento do cache, normalização, guardrails em streaming,
consistência de configuração por requisição e operação degradada do Redis.

Base: branch `feat/phase-5-closure-and-audit`, a partir de `5cec0735`. Cada
correção de segurança/integridade entrou em commit próprio, com teste que falha
antes da correção.

Legenda: **Corrigido** = correção delimitada já aplicada. **Pendente (decisão)** =
exige aprovação antes de mexer. **OK** = verificado, sem ação necessária.

## 1. Isolamento do cache semântico — Corrigido

**Achado.** A chave do cache era `(route, method, modelo efetivo)` e o embedding
cobria apenas mensagens não-`system`. Logo, uma resposta podia ser reutilizada
entre requisições que diferiam em:
- instruções de `system` (persona, escopo, regras);
- parâmetros de geração (`temperature`, `top_p`, `max_tokens`, `stop`, `seed`,
  `response_format`, penalidades, etc.);
- definições de ferramentas (`tools`, `tool_choice`, `functions`).

Duas requisições com as mesmas mensagens de usuário mas, por exemplo, `temperature`
diferente ou `system` diferente colidiam e podiam devolver a resposta uma da outra.

**Correção (commit `fix(cache): isolate semantic cache ...`).**
`compatibility_salt(messages, payload)` gera um hash de: versão de namespace +
instruções `system` (sensível a maiúsculas) + parâmetros de geração + ferramentas.
Esse salt entra no namespace e na chave de índice. Diferença em qualquer um →
namespace diferente → sem reuso. `CACHE_NAMESPACE_VERSION` permite invalidar
entradas antigas no futuro sem limpeza destrutiva (expiram por TTL). A limpeza por
mudança de modelo passou a apagar todos os salts de um `(route, method, model)` por
glob.

**Testes.** `salt_isolates_system_params_and_tools` (unitário) e
`cache_does_not_reuse_across_incompatible_requests` (E2E HTTP). Ambos falham antes
da correção (verificado).

**Preferência aplicada:** um miss a mais causado pelo isolamento é aceitável; é
melhor perder um hit do que devolver contexto incompatível.

## 2. Identidade/tenant no cache — Pendente (decisão)

**Achado.** Nenhuma identidade (tenant, api-key, `ClientIdentity`) entra na chave
do cache. Com o cache habilitado, uma resposta é compartilhada entre todos os
chamadores autenticados da mesma `(rota, método, modelo, salt)`. Para muitos
usos isso é desejável (cache compartilhado), mas em cenário multi-tenant com dados
sensíveis é vazamento entre tenants.

**Por que não corrigi agora.** A identidade do autenticador (middleware Tower) não
é propagada até o handler hoje; incluí-la na chave exige encanar a identidade pela
pipeline e decidir a política (cache por-tenant vs compartilhado, e como derivar a
chave de tenant). Isso altera o fluxo de auth/identidade e a semântica de uma
configuração existente sem opt-in — entra nos itens que pedem sua aprovação.

**Proposta para aprovação.** Adicionar um componente de identidade opcional à chave
do cache (ex.: `cache_scope: shared | per_identity` por rota, default `shared`
para preservar o comportamento atual), derivando a chave de tenant do
`ClientIdentity`. Aguardando decisão.

## 3. Normalização — OK (com observação documentada)

**Achado.** Minúsculas + colapso de espaços são usados em dois lugares:
- `normalize_conversation` (texto do embedding do cache e `entry_id`);
- correspondência de guardrails `keyword`/`prompt_injection_heuristic` e a busca da
  redação.

Regras `regex` **não** são normalizadas (precisa de `(?i)` para case-insensitive).
A redação usa minúsculas só para localizar; substitui preservando o texto original.

**Conclusão.** A normalização é usada apenas para comparação/similaridade e para
localizar spans de redação. Ela **não** altera o payload encaminhado (exceto a
própria redação, que é o efeito desejado) nem decide compatibilidade de
provedor/modelo. Não corrompe conteúdo. Risco teórico: código/identificadores
colocados em minúsculas afetam só a *similaridade* do cache, nunca a resposta
entregue. Sem ação de código; comportamento documentado.

## 4. Guardrails em streaming — Corrigido + limites documentados

**Achado.** O caminho de streaming aplica guardrails outbound sobre uma janela
deslizante (4096 bytes), então padrões divididos entre chunks são detectados
(há teste para `block`). Mas, no `redact`, só o delta do chunk atual era
reavaliado: uma palavra dividida entre dois chunks era detectada pela janela, mas
nenhuma metade era redigida — vazando as metades.

**Correção (commit `fix(streaming): do not leak redacted keywords ...`).** Quando a
janela casa um `redact` mas o delta atual não casa sozinho (padrão a cavalo do
limite), o conteúdo do delta a cavalo é **suprimido** (emitido vazio) em vez de
vazar. `block` não muda.

**Limites documentados (README).** São garantias honestas de um design de buffer
limitado e baixa latência:
- um padrão cujo início já foi descartado pelo corte da janela de 4096 bytes pode
  escapar;
- só o `delta.content` do assistant é inspecionado (não tool-call deltas);
- regex não sofre case-folding (use `(?i)`);
- chunks já emitidos antes de o padrão completar não podem ser retirados.

Garantias cross-chunk completas para regras arbitrárias exigiriam bufferizar a
resposta inteira (anula o streaming) ou limites de tamanho por regra. Fica como
opt-in futuro; **não** é prometido hoje.

**Teste.** `streaming_redact_does_not_leak_keyword_split_across_chunks` (falha antes
da correção, verificado).

## 5. Consistência de configuração por requisição — Corrigido

**Achado.** A config de rota/modelo/cache/scoring é resolvida uma vez por
requisição (`ResolvedRoute`, snapshot consistente via um único `current()` do
`ArcSwap`). Mas os guardrails compilados ficam em um `ArcSwap` separado
(`GuardrailEngine`) lido até quatro vezes por requisição (inbound, outbound de hit
de cache, setup de streaming, outbound da resposta do provedor). Um hot reload no
meio da requisição podia fazer o inbound usar uma versão de guardrails e o outbound
outra.

**Correção (commit `fix(router): snapshot route guardrails once per request`).** Os
guardrails da rota são resolvidos uma vez no início da requisição e reusados em
todas as verificações inbound/outbound. Comportamento público inalterado.

**Testabilidade.** A garantia é estrutural (uma única leitura por requisição);
verificada por leitura de código. Um teste de corrida de hot reload no meio da
requisição seria não determinístico e não foi adicionado.

## 6. Redis degradado, recuperação e readiness — OK

Verificado na Fase 4 e nos testes com Redis real (G1). Resumo:
- Operações do breaker e do cache são limitadas (timeout de 100ms) e suprimidas por
  1s após falha; nunca derrubam o tráfego por não conseguir ler o próprio estado.
- Proteção local conservadora e limitada (`max_ttl + lease`) durante indisponi-
  bilidade, com reconciliação automática quando o Redis volta.
- `/readyz` é opcional por padrão (`redis.required_for_readiness=false`): uma queda
  do Redis não retira todas as réplicas; responde 200 com `degraded:["redis"]`.
  `/healthz` nunca depende do Redis.
- Degradação distinguível de circuito aberto por falha do provedor
  (`Admission::DenyUnverified`, `reason=breaker_state_unavailable`, métricas
  `melis_circuit_breaker_degraded_decisions_total`).

Cobertura: os 20 testes `circuit_breaker::redis_it` (reais) incluindo queda no
boot/runtime/recuperação e expiração do horizonte. Sem ação adicional.

## Observação adicional (não bloqueante)

`melis_requests_total` usa label de provedor fixo `"unknown"` no caminho de sucesso
não-streaming, enquanto `melis_request_duration` carrega o `provider_type` real.
É uma inconsistência de rótulo de observabilidade (não afeta roteamento, cache ou
segurança). Não corrigida; registrada para uma limpeza futura de métricas, fora do
escopo desta auditoria.

## Resumo

| Área | Resultado | Commit |
|------|-----------|--------|
| Isolamento do cache (system/params/tools) | Corrigido | `fix(cache): isolate ...` |
| Identidade/tenant no cache | Pendente (decisão) | — |
| Normalização | OK (documentado) | — |
| Guardrails em streaming (redact split) | Corrigido + limites | `fix(streaming): ...` |
| Consistência por requisição (guardrails) | Corrigido | `fix(router): snapshot ...` |
| Redis degradado / readiness | OK (Fase 4 + G1) | — |
