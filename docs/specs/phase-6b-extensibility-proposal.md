# Fase 6B: extensibilidade — proposta (para aprovação)

**Nada implementado.** Este documento tem duas partes, conforme combinado:

1. Proposta de **interfaces internas estáveis** para políticas de contexto e
   guardrails, preservando as implementações atuais e seus testes.
2. **Checkpoint de design** da extensibilidade externa (carregamento de plugins /
   Wasm), que aguarda aprovação antes de qualquer implementação.

A extensibilidade externa não deve bloquear um experimento de governança de
contexto. Se ela não fizer parte das questões de pesquisa escolhidas, fica como
extensão não avaliada.

## Parte 1 — Interfaces internas estáveis (proposta)

Objetivo: dar um contrato interno claro às duas famílias de política já existentes,
sem mudar comportamento, para que futuras implementações (internas ou externas)
tenham um ponto de extensão único e testável.

### 1A. Política de contexto (compactação/seleção)

Hoje o compactor tem dois caminhos concretos: `SimpleCompactor` e
`SemanticGuardedCompactor` (via `ContextCompactor::compact_with_pins`), mais o
scoring opt-in da Fase 5 (`semantic_scoring`). Proposta de interface interna:

```
trait ContextPolicy {
    // Entrada: mensagens originais (Value) + config resolvida + scores opcionais.
    // Saída: JsonCompaction (objetos originais preservados, kept_indices).
    fn compact(&self, input: ContextInput) -> Result<JsonCompaction, ContextError>;
    fn name(&self) -> &'static str;
}
```

- `ContextInput` encapsula o que o handler já monta: mensagens, `CompactorConfig`,
  modo/limites de scoring, e as unidades (D1/D2).
- As implementações atuais passam a ser `ContextPolicy` registradas por nome; o
  handler seleciona por configuração, como hoje. Sem mudança de comportamento:
  os testes de paridade e preservação (D1/Fase 5) continuam valendo como contrato.
- **Preservação é invariante do contrato**, não de cada implementação: o adaptador
  que chama a política garante objetos originais, ordem e índices válidos antes de
  devolver ao caminho de envio. Uma política que devolva índices inválidos é
  rejeitada (erro → fallback para heurística), nunca encaminha payload inválido.

### 1B. Guardrails

Hoje `CompiledGuardrails::evaluate` decide `Allow|Redacted|Blocked` por direção.
Proposta: um trait `GuardrailPolicy` com a mesma assinatura semântica, para que o
conjunto de regras compiladas seja uma implementação entre outras:

```
trait GuardrailPolicy {
    fn evaluate(&self, direction: Direction, text: &str) -> GuardrailDecision;
    fn name(&self) -> &'static str;
}
```

- A implementação atual (regex/keyword/prompt-injection) vira a `BuiltinGuardrails`.
- O caminho de streaming continua com a janela deslizante e as garantias já
  documentadas (buffer de 4096, supressão do chunk a cavalo); o contrato não promete
  proteção geral contra qualquer padrão.

### Entregáveis da Parte 1 (se aprovada)

- Os dois traits + adaptadores que embrulham as implementações atuais.
- Zero mudança de comportamento observável; a suíte atual (445 testes) permanece o
  contrato. Nenhuma dependência nova.
- Commit isolado; sem runtime externo.

## Parte 2 — Extensibilidade externa (checkpoint de design)

A pedido, apresento o design para aprovação **antes** de qualquer implementação.

### Contrato de entrada/saída e versionamento

- Entrada: a mesma `ContextInput`/texto de guardrail, serializados por uma ABI
  versionada (`abi_version`), sem tipos internos do Rust vazando.
- Saída: `kept_indices` + conteúdo reescrito **apenas** quando permitido; o host
  revalida índices e preservação. Saída inválida → fallback documentado, nunca
  encaminha payload corrompido.
- Versionamento: a interface declara `abi_version`; o host recusa plugins de versão
  incompatível na carga (config inválida mantém a anterior).

### Preservação e segurança de dados

- O host, não o plugin, é a autoridade sobre preservação de mensagens originais,
  ordem e grupos de ferramenta. O plugin só pode propor remoções/scores; o host
  aplica as mesmas invariantes do D1/Fase 5.
- Índices ou resultados inválidos do plugin são rejeitados.

### Limites de recursos

- Orçamento de execução (tempo), memória máxima e tamanho máximo de saída por
  chamada. Estouro → aborta a chamada e faz fallback.
- Sem acesso a rede, filesystem ou segredos por padrão.

### Erro, fallback, carregamento e rollback

- Qualquer erro do plugin → fallback para a política interna correspondente.
- Carregamento na inicialização e no hot reload com validação; inválido mantém a
  configuração anterior (igual ao resto do gateway).
- Rollback = recarregar a configuração anterior; sem estado persistente no plugin.

### Impacto em dependências, imagem e operação

- Um runtime Wasm (ex.: wasmtime) adiciona uma dependência significativa e aumenta
  a imagem; a decisão de adotá-lo é parte deste checkpoint.
- Se adotado: sandbox **sem** rede, filesystem ou segredos por padrão, com orçamento
  de execução e memória. **Não** usar bibliotecas nativas dinâmicas como
  substituição silenciosa.

### Questões para aprovação

1. A extensibilidade externa faz parte das questões de pesquisa do experimento, ou
   fica declarada como extensão não avaliada?
2. Se avaliada: Wasm (sandbox, com as dependências/imagem que isso implica) ou um
   escopo menor (apenas as interfaces internas da Parte 1, sem runtime externo)?
3. Prioridade frente ao pacote experimental (que continua adiado).

**Recomendação:** aprovar a Parte 1 (interfaces internas, baixo risco, sem
dependências) quando fizer sentido, e manter a Parte 2 como extensão não avaliada
até que o experimento de governança de contexto esteja definido. Nenhuma das duas
é pré-requisito do experimento.
