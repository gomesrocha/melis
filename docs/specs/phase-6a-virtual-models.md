# Fase 6A: modelos virtuais (aliases declarativos) — spec revisado

Spec para aprovação. **Nada implementado.** Rev. 2 incorpora os pontos da revisão:
distinção override obrigatório vs default, tabela de precedência com casos de teste,
elegibilidade de cache pelo destino efetivo, separação solicitado/resolvido/efetivo,
consistência de configuração por requisição, colisões sem descoberta remota, e
`/v1/models` respeitando permissões. As quatro questões abertas estão resolvidas
abaixo com alternativas, recomendação e impacto de compatibilidade.

Base: `feat/phase-5-closure-and-audit`. Escopo: mapeamento declarativo e pesos
configurados. **Sem** roteamento por "menor custo"/"melhor qualidade".

## Conceitos

- **Modelo concreto:** nome que um provedor entende (`llama3.2`, `gpt-4o`).
- **Alias (modelo virtual):** nome lógico que mapeia para um ou mais destinos
  concretos `(provider, model, weight)`.
- **Destino efetivo:** o `(provider/deployment, model)` realmente usado para gerar a
  resposta — pode mudar por fallback.

Um alias NÃO é uma rota; é resolvido pelo valor de `model` dentro de uma requisição
que já casou com uma rota.

## Configuração

```yaml
model_aliases:
  - name: "team-default"
    targets:
      - provider: "openai"
        model: "gpt-4o-mini"
        weight: 3
      - provider: "ollama"
        model: "llama3.2"
        weight: 1
```

Validação (rejeita o arquivo; hot reload inválido mantém a config anterior):
`name` não-vazio e único; sem colisão com concreto declarado (ver Colisões);
`targets` não-vazio; `provider` existente; `model` não-vazio; `weight >= 1`, soma
`> 0`; sem encadeamento (target não pode nomear um alias).

## Precedência: override obrigatório vs default

Hoje `route.model` sempre substitui o `model` do payload. Isso impede um alias do
cliente de funcionar em qualquer rota que fixe `model`. A revisão pede distinguir:

- **`model` da rota = override obrigatório** (`model: "x"`): a rota força o modelo;
  o alias do cliente é ignorado **mas** isso é intencional e explícito.
- **`default_model` da rota = default** (novo campo, opcional): usado apenas quando
  o cliente não informou um modelo (ou informou um desconhecido); um alias informado
  pelo cliente **não** é silenciosamente anulado.

Regra: uma rota declara no máximo um entre `model` (obrigatório) e `default_model`
(default). Rotas atuais com `model` preservam o comportamento exato de hoje.

### Tabela de precedência

Seja `payload.model` o campo enviado pelo cliente.

| # | Rota | `payload.model` | Modelo resolvido | Observação |
|---|------|-----------------|------------------|------------|
| 1 | `model: "concreto"` | qualquer ou ausente | `concreto` (override) | comportamento atual preservado |
| 2 | `default_model: "d"` | alias conhecido | resolve o alias | default não anula o alias |
| 3 | `default_model: "d"` | concreto | o concreto do payload | |
| 4 | `default_model: "d"` | ausente/vazio | `d` | default aplicado |
| 5 | `default_model: "d"` | alias desconhecido | `d` + warning `unknown_alias` | trata como "não informado" |
| 6 | sem `model`/`default_model` | alias conhecido | resolve o alias | |
| 7 | sem `model`/`default_model` | concreto | o concreto | comportamento atual |
| 8 | sem `model`/`default_model` | ausente/vazio | 400 (já hoje: `model` é obrigatório) | validação atual |
| 9 | sem `model`/`default_model` | alias desconhecido | o valor como está (concreto "desconhecido") + warning | mantém semântica atual: o gateway repassa o nome |

Casos de teste (um por linha da tabela), todos determinísticos:
`route_concrete_override_ignores_alias` (1), `default_does_not_shadow_known_alias`
(2), `default_keeps_concrete` (3), `default_applied_when_absent` (4),
`unknown_alias_falls_back_to_default` (5), `no_route_model_resolves_alias` (6),
`no_route_model_keeps_concrete` (7), `missing_model_is_400` (8),
`unknown_alias_without_default_passes_through` (9).

## Modelo solicitado, resolvido e efetivo

Três grandezas distintas, registradas separadamente:

1. **Solicitado:** `payload.model` (alias ou concreto) como veio do cliente.
2. **Resolvido:** o modelo/destino escolhido pela precedência antes de chamar o
   provedor (pode ser um target ponderado de um alias).
3. **Efetivo:** o `(provider, model)` que de fato produziu a resposta — pode diferir
   do resolvido por fallback.

**Contrato da resposta preservado.** Nenhum campo público novo e o significado do
campo `model` na resposta **não muda** nesta fase: continua o que o provedor retorna
(hoje o gateway repassa a resposta do provedor). As três grandezas vão para
observabilidade, não para o corpo. Qualquer mudança no `model` da resposta seria
proposta aqui antes — e não é proposta.

**Observabilidade (cardinalidade controlada).**
- Métrica `melis_model_alias_resolution_total{alias, effective_model, outcome}` com
  `outcome ∈ {resolved, fallback, unknown_alias, no_alias}`.
- **Cardinalidade:** os labels `alias` e `effective_model` são restritos aos nomes
  **declarados na configuração** (aliases + modelos concretos das rotas/providers).
  Um `payload.model` arbitrário que não seja um alias/concreto conhecido é
  rotulado como `other` — nunca a string crua do cliente. Isso impede explosão de
  cardinalidade por entrada arbitrária.
- Log por requisição (sem conteúdo): `requested_model`, `resolved_model`,
  `effective_provider`, `effective_model`, `via_alias`, e em fallback o destino
  final.

## Elegibilidade de cache (destino efetivo, não só o nome)

A identidade de execução para o cache **não** pode ser apenas o nome concreto do
modelo. O `compatibility_salt` (auditoria) passa a incluir o **destino efetivo**:

- `provider` (e `deployment`/`base_url` quando distingue instâncias do mesmo tipo),
- `model` concreto,
- parâmetros de geração relevantes (já incluídos),
- identidade do chamador (já incluída, Fase de isolamento),
- uma **revisão de configuração** do mapeamento efetivo quando necessário
  (ex.: hash do target resolvido), **sem** expor segredos (o `base_url` entra como
  hash, não em claro; `api_key` nunca entra).

Propriedades exigidas:

1. **Alias remapeado não reusa entrada antiga incompatível.** Como o destino efetivo
   (provider+model) entra no salt, se `fast` deixa de apontar para `llama3.2@ollama`
   e passa a `gpt-4o-mini@openai`, o namespace muda — sem reuso. Nenhuma limpeza
   destrutiva; entradas antigas expiram por TTL.
2. **Resposta de fallback é registrada com o destino que a produziu.** A chave de
   **store** usa o destino **efetivo** (o que respondeu), não o resolvido
   inicialmente. Assim uma resposta gerada por `llama3.2@ollama` (após fallback de
   `gpt-4o-mini@openai`) é cacheada sob o namespace de `llama3.2@ollama`.
3. **Cache consultado antes da seleção do destino.** Hoje o lookup do cache ocorre
   **antes** do balanceador escolher o destino (o lookup está no início do handler;
   a seleção vem depois). Para um alias multi-target, o destino do lookup é ambíguo.
   Tratamento proposto, em ordem de preferência:
   - **(a)** Para alias de **target único**, o destino é determinístico: usa-se esse
     destino no lookup e no store. Sem ambiguidade.
   - **(b)** Para alias **multi-target**, nesta versão: **desabilitar o cache** para
     a requisição (bypass documentado) OU exigir `cache: shared_across_targets:
     false` — como não há garantia de que dois targets produzam respostas
     intercambiáveis, misturá-los seria incorreto. **Recomendação: desabilitar o
     cache para aliases multi-target nesta fase**, com métrica
     `melis_cache_skipped_total{reason="multi_target_alias"}` e documentação. Uma
     versão futura pode mover o lookup para depois da seleção do destino.
   - Essa limitação é explícita: preferir não cachear a arriscar misturar destinos
     incompatíveis.

## Fallback

Reusa o mecanismo atual: targets de um alias viram candidatos do balanceador
(round-robin ponderado); em falha, o próximo target é tentado até o limite atual. O
circuito por provedor continua valendo. O destino efetivo final é o que alimenta
store de cache e observabilidade.

## Consistência de configuração por requisição

A correção da auditoria já garante um snapshot único do `RouteResolver` por
requisição e um snapshot único dos guardrails. Os aliases vivem no **mesmo**
`routes.yaml` e no **mesmo** `RouteResolver`, então rota + alias + token/scoring +
cache-config vêm do **mesmo snapshot**. Requisito explícito: a resolução de alias usa
o mesmo `resolver` já capturado uma vez por requisição — nunca relê o `ArcSwap`. Um
hot reload no meio da requisição não mistura versões (rota de uma versão, alias de
outra). Teste: `request_uses_single_config_snapshot_for_route_and_alias` (injeta um
resolver fixo e verifica que alias e rota vêm dele).

## Colisões sem descoberta remota

Não é viável enumerar todos os modelos de todos os provedores no boot. Detecção
conservadora, apenas com o que está na configuração:

- **Erro de validação** se um `alias.name` for igual a um `model` concreto declarado
  em qualquer rota (`route.model`, `route.default_model`, `route.providers[].model`)
  ou em `providers[].models`.
- Um alias que colida com um concreto que o cliente referencia implicitamente (não
  declarado) **não** é detectável na carga. Regra documentada: aliases têm
  precedência sobre o nome concreto do payload (passo 2/6 da tabela). Para eliminar
  a ambiguidade, **questão aberta 3** propõe um prefixo reservado.
- Sem descoberta remota; sem chamada a provedor no boot.

## `/v1/models`

`GET /v1/models`, compatível com OpenAI:

- Lista aliases declarados + modelos concretos declarados (união deduplicada).
- **Permissões:** respeita o mesmo auth das demais rotas. Quando `auth.enabled`, o
  endpoint exige credencial válida. Se o `ClientIdentity` tiver `allowed_models` não
  vazio, a lista é **filtrada** para esses modelos/aliases permitidos (reusa o campo
  de autorização já existente). Sem identidade confiável e auth habilitado → 401.
- **Não** expõe `base_url`, `api_key`, pesos, nem o mapeamento interno do alias.
  Apenas `id` + campos OpenAI padrão.
- Formato: `{ "object": "list", "data": [ { "id", "object":"model", "created",
  "owned_by":"melis" } ] }`.

## Testes e critérios de aceite

Determinísticos (sem serviço real); `#[ignore]` para os que exijam Ollama.

1. Precedência: os 9 casos da tabela.
2. Resolução single-target (captura de payload via wiremock) e multi-target
   (distribuição por peso determinística + fallback).
3. Colisão alias×concreto declarado → erro; hot reload inválido mantém anterior.
4. Validação: ciclo, `weight` 0, provider inexistente, dois de `model`/`default_model`.
5. `/v1/models`: lista correta; sem credenciais/topologia; auth exigido; filtro por
   `allowed_models`.
6. Solicitado/resolvido/efetivo distinguíveis em métricas/log, inclusive em fallback;
   cardinalidade controlada (string arbitrária vira `other`).
7. Cache: single-target compartilha namespace por destino efetivo; alias remapeado
   não reusa; fallback cacheia sob o destino efetivo; multi-target não consulta o
   cache (bypass com métrica).
8. Consistência: uma requisição usa um único snapshot para rota e alias.
9. Paridade: sem `model_aliases` e sem `default_model`, nenhuma mudança (os 411
   testes atuais seguem válidos).

Aceite: paridade preservada; endpoints/schemas/configs antigas válidos; `/v1/models`
não vaza; solicitado/efetivo sempre distinguíveis; config inválida nunca aplicada;
sem roteamento por custo/qualidade; cache nunca mistura destinos incompatíveis.

## Fora de escopo

Encadeamento de aliases; roteamento por custo/qualidade; descoberta remota de
modelos; mover o lookup do cache para depois da seleção de destino (futuro);
plugins/Fase 6B.

## Questões abertas — resolvidas (alternativas, recomendação, impacto)

### Q1 — O que a resposta reporta no campo `model`?
- **Alternativas:** (a) ecoar o alias solicitado; (b) o modelo concreto efetivo;
  (c) manter exatamente o que o provedor retorna.
- **Recomendação:** (c) manter o contrato atual — o gateway repassa o `model` do
  provedor; o alias/efetivo ficam em observabilidade. Não alterar o significado do
  campo nesta fase.
- **Impacto de compatibilidade:** nenhum; clientes atuais não veem diferença. (a)/(b)
  mudariam um campo público e exigiriam proposta própria.

### Q2 — Alias do payload vs `providers` multi-provider da rota.
- **Alternativas:** (a) alias do payload vence; (b) `providers` da rota vence;
  (c) erro se ambos definidos.
- **Recomendação:** (a) com a distinção override/default: se a rota quer forçar,
  usa `model` (override) ou não oferece alias; `providers` define o pool padrão
  quando não há alias. Documentar que um alias informado escolhe o destino.
- **Impacto:** rotas atuais não usam alias, então nenhum efeito hoje. (c) seria mais
  estrito mas rejeitaria combinações válidas; (b) anularia o alias silenciosamente,
  o que a revisão pede para evitar.

### Q3 — Prefixo reservado para aliases (ex.: `@fast`)?
- **Alternativas:** (a) sem prefixo, precedência documentada; (b) prefixo opcional
  reservado que marca explicitamente um alias e elimina colisão com concretos não
  declarados.
- **Recomendação:** (b) como **opcional** — aceitar tanto `fast` quanto `@fast`;
  quando usado o prefixo, a resolução é inequívoca (nunca tratada como concreto).
  Sem prefixo, a regra de precedência vale. Mantém compatibilidade e dá uma saída
  determinística a quem precisa.
- **Impacto:** aditivo; nomes atuais continuam válidos. Custo: documentar o prefixo.

### Q4 — `/v1/models` inclui um campo não-padrão (`melis:kind`)?
- **Alternativas:** (a) saída OpenAI-pura; (b) incluir `melis:kind: alias|concrete`.
- **Recomendação:** (a) por padrão, para máxima compatibilidade com clientes OpenAI;
  (b) só atrás de um parâmetro/flag explícito se um operador pedir.
- **Impacto:** (a) nenhum; (b) adiciona campo não-padrão que alguns clientes podem
  rejeitar — por isso fica opt-in.
