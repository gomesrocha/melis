# Fase 6A: modelos virtuais (aliases declarativos)

Spec para aprovação. **Nada implementado ainda.** Define aliases de modelo
declarativos, selecionáveis pelo campo `model` da requisição, com precedência,
compatibilidade, `/v1/models`, interação com fallback e cache, hot reload e
critérios de aceite.

Base: `feat/phase-5-closure-and-audit` (tip da cadeia Fases 1–5 + auditoria).
Escopo: apenas mapeamento declarativo e pesos configurados. **Sem** roteamento por
"menor custo" ou "melhor qualidade" — isso exigiria política explícita e dados, e
fica fora desta fase.

## Motivação

Hoje o campo `model` da requisição é usado assim (verificado no código):
- a rota resolve por `(path, method)`; se a rota define `model`, ele **substitui** o
  `model` do payload (`effective_model = route.model.unwrap_or(payload_model)`);
- o `effective_model` vira o label do provedor-alvo e entra na chave do cache;
- não existe endpoint `/v1/models`.

Um alias de modelo permite que o cliente peça `model: "fast"` ou
`model: "team-default"` e o gateway resolva para um provedor/modelo concreto (ou um
conjunto ponderado), sem o cliente conhecer o backend. É um indireção declarativa,
não uma decisão automática de qualidade/custo.

## Conceitos

- **Modelo concreto:** nome que um provedor entende (ex.: `llama3.2`, `gpt-4o`).
- **Alias (modelo virtual):** nome lógico declarado na configuração que mapeia para
  um ou mais destinos concretos `(provider, model, weight)`.
- **Rota:** entrada `(path, method)` existente em `routes.yaml`.

Um alias NÃO é uma rota. Aliases são resolvidos pelo valor de `model` dentro de uma
requisição que já casou com uma rota.

## Configuração

Novo bloco de topo em `routes.yaml`, opcional:

```yaml
model_aliases:
  - name: "fast"
    targets:
      - provider: "ollama"
        model: "llama3.2"
  - name: "team-default"
    targets:
      - provider: "openai"
        model: "gpt-4o-mini"
        weight: 3
      - provider: "ollama"
        model: "llama3.2"
        weight: 1
```

Regras de validação (rejeitam o arquivo; no hot reload mantêm a config anterior):
- `name` não-vazio, único entre aliases.
- `name` **não pode colidir** com um modelo concreto conhecido nem com outro alias
  (ver "Colisões").
- `targets` não-vazio; cada `provider` deve existir (built-in ou
  `custom_providers`); cada `model` não-vazio.
- `weight` opcional, default 1; finito, `>= 1` (inteiro); soma dos pesos `> 0`.
- Sem ciclos: um `targets[].model` não pode ser o nome de um alias (aliases não
  encadeiam nesta fase; um alias aponta só para modelos concretos).

## Precedência (requested → effective)

Quando uma requisição casa uma rota, o modelo efetivo é resolvido nesta ordem,
parando no primeiro que se aplica:

1. **Override de rota.** Se a rota define `model` (comportamento atual), ele vence e
   é usado como está. Isso preserva 100% do comportamento existente. (Decisão de
   design: o override de rota é o mais específico; um operador que fixou o modelo na
   rota não quer que o alias do cliente o altere. Alternativa rejeitada: alias do
   cliente sobrepor o override de rota — quebraria rotas fixas existentes.)
2. **Alias do payload.** Senão, se `payload.model` é o `name` de um alias, resolve
   para os `targets` do alias.
3. **Modelo concreto do payload.** Senão, se `payload.model` é um modelo concreto,
   usa como está (comportamento atual).
4. **Default.** Senão, o comportamento atual de "nenhuma rota/modelo" (o payload
   segue como está para o(s) provedor(es) da rota).

Observação: a resolução de alias (passo 2) só troca o(s) **destino(s)**
`(provider, model)`; ela não muda a rota nem as políticas da rota (cache, guardrails,
compactação continuam as da rota).

## Interação com provedores e fallback

- Um alias com um único target resolve para aquele `(provider, model)`.
- Um alias com múltiplos targets alimenta o **balanceador** já existente
  (round-robin ponderado), exatamente como a lista `providers` de uma rota. Cada
  target é um candidato com seu peso.
- O **fallback** (failover) reusa o mecanismo atual: em falha de um target, o
  próximo target do alias é tentado (até o limite atual de tentativas). O circuito
  por provedor continua valendo.
- Precedência alias vs `providers` da rota: se a rota define `providers` (multi-
  provider) **e** o payload usa um alias, o alias do payload vence para a escolha do
  modelo/destino, porque o passo 1 (override de rota) não disparou. Se isso for
  indesejado para alguma rota, o operador fixa `model` na rota (passo 1). Documentar
  claramente. (Ponto a confirmar na revisão — ver "Questões em aberto".)

## Modelo solicitado vs efetivamente utilizado

- O corpo da resposta mantém o `model` **solicitado** pelo cliente quando era um
  alias? Decisão proposta: a resposta reporta o **modelo concreto efetivamente
  utilizado** no campo `model` (como os provedores fazem), e o alias solicitado é
  registrado apenas em logs/métricas. Racional: não enganar o cliente sobre o que
  gerou a resposta. Alternativa (ecoar o alias) fica como questão em aberto.
- Observabilidade (sem conteúdo sensível, cardinalidade limitada):
  - métrica nova `melis_model_alias_resolution_total{alias, effective_model, outcome}`
    onde `outcome ∈ {resolved, fallback, unknown_alias}`;
  - log por requisição: `requested_model`, `effective_model`, `provider`,
    `via_alias` (bool), e em fallback o destino final.
  - O modelo efetivo e o alias solicitado são registrados **separadamente**,
    inclusive quando o fallback muda o destino.

## Interação com o cache semântico

O cache já isola por `effective_model` e pelo `compatibility_salt` (auditoria). Para
aliases:
- A chave do cache usa o **modelo concreto efetivamente resolvido** (não o alias),
  para que duas requisições que resolvem ao mesmo concreto compartilhem cache e uma
  mudança no mapeamento do alias **não** reuse respostas antigas.
- Como o mapeamento do alias pode mudar no hot reload, o `compatibility_salt` (ou um
  componente equivalente da chave) deve incluir um identificador do mapeamento
  efetivo do alias (ex.: o par `(provider, model)` resolvido, que já é o modelo
  concreto na chave). Assim, se `fast` deixa de apontar para `llama3.2` e passa a
  apontar para `gpt-4o-mini`, as requisições caem em um namespace diferente
  automaticamente — sem reuso indevido e sem limpeza destrutiva.
- Para alias multi-target, cada destino concreto tem seu próprio namespace de cache
  (a chave usa o concreto escolhido), então não há mistura entre targets.

## `/v1/models`

Novo endpoint `GET /v1/models`, compatível com OpenAI:
- Lista: os aliases declarados **mais** os modelos concretos explicitamente
  configurados nas rotas e em `providers` (união, deduplicada).
- Formato OpenAI: `{ "object": "list", "data": [ { "id": <name>, "object": "model",
  "created": <ts>, "owned_by": "melis" }, ... ] }`.
- **Não** expõe: `base_url`, `api_key`, pesos, mapeamento interno do alias, ou
  qualquer detalhe de provedor. Apenas o `id` (nome do alias/modelo) e campos
  OpenAI padrão. Racional: evitar vazar topologia/credenciais.
- Aliases e concretos aparecem com o mesmo formato; um campo opcional
  `melis:kind: alias | concrete` pode ser adicionado atrás de um flag, mas por
  padrão a saída é OpenAI-pura (decisão proposta: não adicionar campos não-padrão).

## Hot reload

- A config de aliases entra no mesmo `routes.yaml` e segue o mesmo caminho de
  validação + `ArcSwap` das rotas: config inválida (nome duplicado, colisão com
  concreto, provider inexistente, ciclo) **mantém a configuração anterior** e loga
  warning; nunca derruba o serviço.
- A resolução de alias por requisição usa o **mesmo snapshot** do resolver já
  capturado uma vez por requisição (consistência por requisição garantida pela
  correção da auditoria). Aliases e rota vêm do mesmo `RouteResolver`.

## Colisões (alias vs concreto)

- Um alias cujo `name` é igual a um modelo concreto conhecido é **erro de validação**
  (rejeita o arquivo / mantém anterior no reload). Não há precedência silenciosa.
- "Modelo concreto conhecido" = qualquer `model` que apareça em uma rota
  (`route.model` ou `route.providers[].model`) ou um nome reservado. Como o espaço de
  modelos de um provedor é aberto (o cliente pode pedir qualquer string), a detecção
  de colisão é **conservadora**: cobre os concretos declarados na config. Um alias
  que colida com um concreto só referenciado implicitamente pelo cliente não é
  detectável na carga; nesse caso o passo 2 (alias) tem precedência sobre o passo 3
  e isso fica **documentado**. (Questão em aberto: oferecer um prefixo reservado para
  aliases, ex.: `@fast`, para eliminar ambiguidade.)

## Testes e critérios de aceite

Determinísticos (sem serviço real), com `#[ignore]` para os que exijam Ollama:

1. **Compatibilidade:** rotas e modelos concretos existentes continuam idênticos;
   sem `model_aliases`, nenhuma mudança de comportamento (paridade).
2. **Precedência:** override de rota vence alias; alias vence concreto do payload;
   concreto usado quando não é alias; default preservado. Um teste por nível.
3. **Resolução single-target:** `model: "fast"` → `(provider, model)` corretos
   enviados ao provedor (captura de payload via wiremock).
4. **Resolução multi-target:** distribuição conforme pesos (determinística com
   balanceador semeado) e fallback para o próximo target em falha.
5. **Colisão:** alias com nome de concreto declarado → erro de validação; hot reload
   inválido mantém a config anterior.
6. **Ciclo/baixa validação:** target apontando para um alias → erro; `weight` 0 →
   erro; provider inexistente → erro.
7. **`/v1/models`:** lista aliases + concretos; **não** contém `base_url`/`api_key`/
   pesos/mapeamento; formato OpenAI válido.
8. **Requested vs effective:** resposta reporta o modelo concreto; métrica/log
   registram alias solicitado e efetivo, inclusive em fallback.
9. **Cache:** duas requisições ao mesmo alias→concreto compartilham namespace; após
   mudança do mapeamento do alias, caem em namespace diferente (sem reuso); dois
   targets distintos não compartilham cache.
10. **Hot reload consistente:** uma requisição vê um único snapshot de aliases+rota.

Critérios de aceite:
- Nenhuma regressão nos 405 testes atuais; paridade sem `model_aliases`.
- Endpoints, formatos públicos e configs antigas permanecem válidos.
- `/v1/models` não vaza credenciais nem topologia interna.
- Alias solicitado e modelo efetivo sempre distinguíveis em observabilidade.
- Config inválida nunca é aplicada (boot falha; hot reload mantém anterior).
- Sem roteamento por custo/qualidade.

## Fora de escopo (Fase 6A)

- Encadeamento de aliases (alias → alias).
- Roteamento por "menor custo" / "melhor qualidade" / políticas automáticas.
- Descoberta dinâmica de modelos do provedor para `/v1/models`.
- Plugins/extensibilidade externa (Fase 6B, spec e checkpoint separados).

## Questões em aberto (para a revisão)

1. **Resposta:** ecoar o alias solicitado no `model` da resposta, ou o concreto
   efetivo? Proposta: concreto efetivo.
2. **Alias vs `providers` da rota:** quando ambos existem, confirmar que o alias do
   payload vence (via passo 2) ou se `providers` da rota deve ter precedência.
3. **Prefixo reservado para aliases** (ex.: `@nome`) para eliminar a ambiguidade de
   colisão com concretos não declarados? Proposta: não nesta fase; documentar a
   regra de precedência.
4. **`/v1/models`:** incluir um campo não-padrão `melis:kind`? Proposta: não,
   manter saída OpenAI-pura.
