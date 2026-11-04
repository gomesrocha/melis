# Fase 5: scoring por similaridade vetorial (opt-in)

Cópia versionada e sanitizada do spec da Fase 5 (o original fica em `.kiro/specs/`,
ignorado pelo git). Não contém configuração local nem segredos.

Status: **experimental**. A melhoria de seleção é uma **hipótese a avaliar**, não um
resultado. Nenhum peso foi calibrado empiricamente.

## Contexto

O `SemanticGuardedCompactor` (estratégia `semantic_guarded_trimming`) pontua as
mensagens com uma heurística estrutural: role, marcadores críticos, sinais técnicos
e termos da última pergunta. Depois remove as de menor score até caber no orçamento.
A Fase 5 adiciona, de forma opt-in, a similaridade de embedding com a última
mensagem do usuário como componente de score.

Pré-requisito: o bugfix D1 (`fix/compactor-message-preservation`), que preserva os
objetos originais das mensagens e fixa as mensagens não textuais e de ferramenta.

## Decisões aprovadas

- **D1: bugfix separado** (já entregue). Objetos originais preservados, incluindo
  campos desconhecidos. A representação textual serve só para pontuação e contagem.
  A ordem original é mantida. Mensagens com `tool_calls`, `tool_call_id`,
  `function_call`, role `tool`/`function`, `content` não-string ou sem `role` string
  ficam fixadas. Conversas só de texto mantêm o resultado legado.
- **D2: ferramentas.**
  - Escopo: apenas o caminho OpenAI-compatible, que repassa esses campos. Os
    transpilers Anthropic e Vertex não ganham suporte a tool calling. Limitação
    documentada.
  - Um `assistant` com `tool_calls` e todos os seus resultados (`role: tool`) formam
    um grupo indivisível.
  - Uma dependência incompleta (resultado sem chamada, chamada sem todos os
    resultados, ids duplicados, resultados fora de sequência) fixa todas as
    mensagens envolvidas. Nunca há remoção parcial.
  - `tool_group_policy: preserve | eligible_for_removal` (default `preserve`). Em
    `preserve`, um grupo completo é protegido (nunca removido). Em
    `eligible_for_removal`, pode ser removido, mas só inteiro. Em ambos, grupos
    incompletos/ambíguos e instruções protegidas ficam fixados. Preservar a
    dependência de protocolo (chamada↔resultado) **não** é garantia de preservação
    semântica da conversa.
  - **Identificadores opacos (limitação conhecida e correção delimitada):** os ids
    das chamadas (`call_1`, `tooluse_xyz`) não têm valor semântico mas perturbavam a
    heurística (dígitos lidos como sinal técnico) e o embedding. A representação
    usada só para scoring exclui os ids (mantém nome e argumentos da função). O
    payload enviado ao provedor e o algoritmo legado não mudam. Teste diagnóstico:
    trocar apenas os ids não altera a seleção.
- **D3: similaridade.**
  - Fórmula: `sim = clamp((cos + 1) / 2, 0, 1)`. É uma transformação de escala para
    `[0, 1]`, **não** uma probabilidade de relevância.
  - Os vetores são validados antes do cosseno.
  - O score estrutural é normalizado (min-max sobre os candidatos; empate total vira
    0,5).
  - Desempate determinístico: menor score primeiro e, em caso de empate, a unidade
    mais antiga (menor índice original).
- **D4: pesos.** `semantic_weight = 0.5`, `structural_weight = 0.5`. São
  provisórios e configuráveis. Validação: finitos, `>= 0` e soma `> 0`. São
  normalizados para somar 1. O modo `embedding` equivale a `w_sem = 1, w_struct = 0`.
- **D5: limite de candidatos.** Se o número de candidatos ultrapassar `max_messages`,
  há fallback **integral** para a heurística, sem processar subconjunto de
  primeiras ou últimas mensagens. O motivo vai para uma métrica de cardinalidade
  limitada. O fallback segue as regras do D1.
- **D6: orçamento.** São três grandezas separadas:
  1. tokens de conteúdo, contados pelo contador ativo (Fase 1: `estimated` ou `real`);
  2. overhead de formatação, contado **só** com perfil explícito
     (`formatting_overhead: openai_chat_estimate`, que é uma estimativa e não
     faturamento);
  3. `usage` do provedor, que nunca é sobrescrito quando válido.

  Uma contagem BPE local não é garantia de reproduzir o faturamento de nenhum modelo.
  A semântica de orçamento das configurações antigas não muda.

  No modo opt-in `budget.mode: strict`, quando conteúdo obrigatório + overhead +
  `output_reserve_tokens` excedem `max_context_tokens`, a resposta é **400**
  (`invalid_request_error`) com mensagem clara de orçamento insuficiente. Nesse
  caso, instruções protegidas e grupos de ferramentas não são removidos.

## Configuração

Apenas em `token_optimization` de uma rota com `strategy: semantic_guarded_trimming`.
Duas formas são aceitas:

```yaml
token_optimization:
  strategy: semantic_guarded_trimming
  semantic_scoring: hybrid            # forma curta: heuristic | embedding | hybrid
```

```yaml
token_optimization:
  strategy: semantic_guarded_trimming
  semantic_scoring:
    mode: hybrid                      # heuristic (default) | embedding | hybrid
    semantic_weight: 0.5              # provisório, não calibrado
    structural_weight: 0.5            # provisório, não calibrado
    embedding_model: nomic-embed-text
    timeout_ms: 1500                  # tempo total do scoring por requisição
    max_concurrency: 4                # lotes simultâneos
    batch_size: 32                    # textos por chamada /api/embed
    max_messages: 64                  # candidatos; acima disso → fallback integral
    max_chars: 2000                   # truncamento do texto enviado (só para scoring)
    budget:
      mode: legacy                    # legacy (default) | strict
      max_context_tokens: 8192        # obrigatório em strict
      output_reserve_tokens: 1024     # strict
      formatting_overhead: none       # none (default) | openai_chat_estimate
```

- Sem `semantic_scoring`, ou com `heuristic`: caminho legado do D1, sem nenhuma
  chamada de embedding.
- `semantic_scoring` em outra estratégia: warning na carga e o campo é ignorado.
- Valores inválidos (pesos, limites `0`, `strict` sem `max_context_tokens`)
  rejeitam o arquivo de rotas, como já acontece com outros erros de validação. No
  hot reload, a configuração anterior é mantida.

## Fluxo

1. O handler (assíncrono) monta as **unidades**:
   - mensagem de texto;
   - grupo de ferramenta completo;
   - mensagem fixada.
2. Conta os tokens de conteúdo com o contador ativo. Se a contagem ficar abaixo de
   `token_threshold`, nada acontece e **não há embedding**.
3. Unidades protegidas: `system`, última mensagem do usuário, marcadores críticos
   (com `preserve_critical_markers`) e fixadas. As demais são candidatas.
4. Em `strict`, verifica obrigatório + overhead + reserva contra
   `max_context_tokens`. Se exceder, devolve o erro antes de qualquer embedding.
5. Quando não há candidatos, ou quando o obrigatório já atinge o alvo (só em
   `legacy`), a seleção não depende de score. Usa o caminho legado, sem embedding.
6. Com mais de `max_messages` candidatos, fallback `limit_exceeded`, sem embedding.
7. Embeda `[consulta] + candidatos` em lotes (`batch_size`, `max_concurrency`),
   dentro de `timeout_ms` total. Valida quantidade, dimensão, finitude e norma.
8. O compactor (síncrono, determinístico) recebe as similaridades prontas,
   normaliza, combina, ordena e remove candidatos até caber. Depois restaura a ordem
   original. A saída reemite os objetos originais **sem alteração**: sem remoção de
   stop-words, nem em protegidas nem em ferramentas.
9. Qualquer falha nos passos 6 e 7 leva ao **fallback integral** para o caminho
   legado (D1). A requisição nunca falha por causa do scoring. A única exceção é o
   erro explícito de `strict`, que também se aplica ao resultado do fallback.

Orçamento (igual para todos os modos):
`target = max(original × target_token_ratio, min_final_tokens)`.
Em `strict`, o alvo efetivo é `min(target, max_context_tokens − output_reserve_tokens − overhead)`.

## Observabilidade e privacidade

| Métrica | Labels | Significado |
|---|---|---|
| `melis_compaction_scoring_total` | `requested`, `effective` | modo pedido × modo usado (`heuristic`/`embedding`/`hybrid`) |
| `melis_compaction_scoring_seconds` | — | duração do scoring, incluindo o embedding |
| `melis_compaction_scoring_fallback_total` | `reason` | `limit_exceeded`, `timeout`, `provider_error`, `count_mismatch`, `invalid_dimension`, `non_finite`, `zero_norm`, `no_query` |
| `melis_compaction_scoring_skipped_total` | `reason` | `no_candidates`, `mandatory_exceeds_target` (seleção não depende de score) |
| `melis_compaction_budget_insufficient_total` | — | erros 400 do modo `strict` |

As métricas só são registradas para rotas com o modo habilitado e compactação
necessária. Logs e labels **não** contêm prompts, respostas, vetores nem
identificadores de requisição. Registram apenas modos, contagens e motivos.

**Destino dos dados:**
- O texto dos candidatos e da última mensagem do usuário (truncado a `max_chars`) é
  enviado ao provedor de embedding do `AppState`. Em produção, é o Ollama
  configurado em `providers` (`provider_type: ollama`), via `/api/embed`, com
  fallback para `/api/embeddings`.
- Os guardrails inbound (Fase 3) rodam antes, então o texto enviado é o já redigido.

**Custo com o cache semântico:**
- Com o cache semântico (Fase 2) ativo na mesma rota, a requisição compactada faz
  chamadas de embedding **adicionais**: o cache embeda a conversa normalizada inteira
  e o scoring embeda consulta + candidatos.
- Os textos e, possivelmente, os modelos são diferentes, então **não** há
  reutilização entre as etapas.

## Testes

- **Unitários com `ScriptedEmbeddingProvider` (stub):** cobrem estes casos.
  - Paridade legada e zero chamadas no modo heuristic e abaixo do limiar.
  - Relevância que não depende só de recência.
  - Fallback para cada motivo: timeout, erro, contagem, dimensão, NaN/∞, norma zero,
    limite.
  - Limites de lote e concorrência.
  - Agrupamento de ferramentas (completo, incompleto, órfão, múltiplos resultados).
  - Orçamento insuficiente em `strict`.
  - Ordem e desempate determinístico.
- **E2E HTTP:** wiremock como provedor OpenAI-compatible, capturando o payload
  enviado, com o stub de embedding.
- **Ollama real:** `#[ignore]`, verificação G1.

## Avaliação offline

- Harness: `semantic_scoring::eval_harness` (`#[ignore]`).
- Compara `heuristic`, `embedding` e `hybrid` com o mesmo orçamento.
- Registra no relatório: modelo de embedding, provedor (stub/ollama), pesos,
  configuração completa, split e revisão do código (`git rev-parse HEAD`).
- Dados em `eval/compaction/`:
  - `dev.jsonl`: desenvolvimento e ajuste.
  - `eval.jsonl`: somente avaliação final.

  Não ajuste pesos no `eval`. Os fixtures atuais são **sintéticos e pequenos**:
  servem para exercitar o harness e não sustentam conclusões.

## Limitações explícitas

- **GAP-OAI-1:** a validação HTTP rejeita `content: null` e `content` array. Esses
  formatos só são cobertos internamente (adaptador e unidades), não pelo HTTP.
- Grupos de ferramentas: só no payload OpenAI-compatible repassado (D2).
- `openai_chat_estimate` (3 tokens por mensagem + 3 de priming) é uma estimativa.

## Critérios de aceite

- Sem modo habilitado: nenhuma chamada de embedding nova; paridade legada e
  preservação do D1 intactas; endpoints, formatos de erro e configurações antigas
  compatíveis.
- Embedding apenas com modo habilitado e compactação necessária; lote, concorrência
  e tempo total limitados.
- Fallback integral para a heurística em erro, timeout, vetor inválido ou excesso de
  candidatos, com motivo em métrica.
- Instruções protegidas, mensagens fixadas e grupos de ferramentas preservados;
  ordem original restaurada; objetos originais sem alteração.
- `strict`: erro claro quando o obrigatório não cabe; nenhuma remoção silenciosa.
- `usage` válido do provedor nunca sobrescrito.
- Métricas de cardinalidade limitada; nenhum conteúdo em logs ou labels.
- Harness com 3 modos, mesmo orçamento, metadados reproduzíveis e splits separados.
