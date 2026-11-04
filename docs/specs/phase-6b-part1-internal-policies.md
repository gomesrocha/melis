# Fase 6B — Parte 1: interfaces internas de política (implementado)

**Status: IMPLEMENTADO** na branch `feat/phase-6b-internal-policies`. Apenas a
Parte 1 (interfaces internas). A Parte 2 (runtime externo / Wasm) permanece proposta
não implementada e não avaliada (ver `phase-6b-extensibility-proposal.md`).

Objetivo: dar contratos internos claros às duas famílias de política já existentes
(contexto e guardrails), sem mudança intencional de comportamento, para permitir
evolução futura. **Não** é uma API de plugins, uma ABI estável nem suporte a
terceiros.

## Princípios aplicados

- Sem abstração maior que a necessidade. Os dois tipos de política têm formatos
  diferentes e essa diferença foi **preservada**, não forçada em uma interface única.
- O **host** (handler) continua responsável por validar as invariantes:
  preservação de mensagens/campos, ordem, grupos de ferramenta, orçamento
  (legacy/strict), contagem de tokens, métricas e o erro 400 de orçamento. As
  garantias não foram transferidas para implementações intercambiáveis.
- Sem novos campos obrigatórios, sem mudança de defaults, sem registro global
  mutável que misture políticas entre requisições, sem dependências novas.

## ContextPolicy (`src/context_policy.rs`)

```
trait ContextPolicy {
    async fn compact(&self, messages, config, embeddings)
        -> Result<ContextOutcome, ContextError>;
    fn name(&self) -> &'static str;
}
```

- `ContextOutcome { compaction: JsonCompaction, scoring: Option<ScoringOutcome> }`.
  O resultado referencia as mensagens originais por índice (`kept_indices`) e carrega
  os objetos JSON originais — nunca reconstrói payloads (sem regressão do D1).
- `ContextError::BudgetInsufficient` → o host mapeia para 400 (modo `strict`).
- Implementações internas (a diferença de forma é intencional):
  - `LegacyCompactionPolicy`: síncrona, sem embeddings. Envolve
    `compact_json_messages` com o compactor da estratégia.
  - `ScoredCompactionPolicy`: embeddings assíncronos no handler (Fase 5), seleção
    síncrona determinística. Envolve `compact_with_scoring`.
- `select_policy(semantic_scoring)` escolhe entre as duas exatamente como o handler
  fazia inline (scored quando opt-in, senão legacy).
- Preservado: seleção/ordem, objetos e campos extras, protegidas e grupos de
  ferramenta, orçamento legacy/strict, fallbacks, contagem de tokens, resultados de
  compactação, e **ausência de chamadas de embedding** quando o recurso não está
  habilitado. O cálculo assíncrono de embeddings continua fora da seleção síncrona.

## GuardrailPolicy (`src/guardrails.rs`)

```
trait GuardrailPolicy {
    fn evaluate(&self, direction, text, metrics) -> GuardrailDecision;
    fn fail_closed(&self) -> bool;
    fn name(&self) -> &'static str;
}
```

- Implementado para `CompiledGuardrails` (delega ao `evaluate` inerente; precedência
  `block > redact > warn`, ações, `fail_closed`, status HTTP e formato de erro
  inalterados).
- `apply_outbound_to_response` passou a receber `&dyn GuardrailPolicy`, então o
  caminho outbound (resposta do provedor e hit de cache) flui pela trait.
- **Streaming não é equivalente a avaliar uma resposta completa.** A trait
  deliberadamente **não** modela SSE incremental. A janela deslizante vive em
  `transform_sse_stream_guarded`, criada a cada requisição e **nunca compartilhada
  entre clientes**; ela chama `evaluate` sobre a janela acumulada. Teste novo
  confirma isolamento do buffer entre streams concorrentes.
- Preservado: inbound antes de cache/compactação; outbound em resposta do provedor e
  em hit de cache; comportamento e limites documentados do streaming (buffer de 4096,
  supressão do chunk a cavalo; sem promessa de proteção geral contra qualquer padrão).

## Responsabilidades: host × política

| Responsabilidade | Quem |
|------------------|------|
| Decidir seleção/ranking de mensagens | ContextPolicy |
| Decidir allow/redact/block por direção | GuardrailPolicy |
| Preservar objetos originais, ordem, grupos de ferramenta | Host (valida) |
| Semântica de orçamento legacy/strict e erro 400 | Host |
| Contagem de tokens e métricas | Host |
| Buffer de streaming isolado por requisição | Streaming (host), não a trait |
| Autorização (`allowed_models`), cache, snapshot de config | Host |

## Garantias de preservação e limites

- As invariantes do D1/Fase 5 continuam sendo contrato do host e são verificadas
  pelos testes existentes (paridade antes/depois) mais os testes de contrato das
  traits, que casam com chamadas diretas às implementações.
- Uma política que devolvesse índices inválidos não encaminha payload inválido: o
  host aplica/valida o resultado; em erro, há fallback documentado. Falhas de
  seleção/resolução não viram Noop silencioso — em guardrails, `fail_closed`
  decide block vs allow explicitamente.

## O que um runtime externo futuro exigiria (não implementado)

Para uma eventual Parte 2 (fora deste escopo): ABI versionada, serialização sem
tipos internos, revalidação de índices/preservação pelo host, limites de execução/
memória/tamanho de saída, sandbox sem rede/filesystem/segredos, política de
erro/fallback/rollback, e avaliação do impacto em dependências e imagem. Nada disso
é introduzido aqui; as traits internas **não** são apresentadas como suporte a
plugins externos nem como ABI estável.
