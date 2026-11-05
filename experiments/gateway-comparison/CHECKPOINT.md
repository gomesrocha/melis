# Checkpoint — coleta confirmatória ainda não autorizada

Desenho registrado em 2026-11-05. Smoke, piloto e o piloto de variância são
exploratórios. Não entram na avaliação final.

## Commits de main que esta branch não contém

`origin/main` está em `c365c2dc`. A branch de revisão não contém:

- `d5cf93d4` feat: add Vertex Claude support for CatIA
- `c365c2dc` merge do PR #3

O diff adiciona `vertex_anthropic`, autenticação ADC e campos opcionais de
projeto/região. O `Dockerfile` só muda `EXPOSE 8080` para `EXPOSE 9090`, que
não altera o bind. O caminho Ollama/OpenAI usado por esta comparação não é
reescrito: a ramificação nova só corre quando `provider_type` é
`vertex_anthropic`.

Decisão: não mesclar nem rebasear antes do congelamento. Integrar esses
commits mudaria o binário e o `Cargo.lock` em relação ao candidato desta
branch, e o bloco não autoriza feature de produto. O artefato medido é a
ponta da branch de revisão.

## RQ1

Célula do piloto de variância, já autorizada, e candidata à coleta final:

- Backend determinístico de latência artificial zero. O backend de 100 ms é
  outro cenário e não se mistura.
- Perfil mínimo, JSON, concorrência configurada 1.
- Unidade independente: bloco. Requisições dentro do bloco são aninhadas.
- Estimador: média, entre blocos, das diferenças das medianas de conclusão
  por execução.
- Contrastes, na mesma célula: `melis − direct`, `litellm − direct`,
  `melis − litellm`.
- Multiplicidade planejada para a coleta final: Holm nessa família de três,
  alfa 0,05. O piloto não calcula p-valor.
- Falhas e throughput ficam nos CSVs. Um bloco sem mediana de sucesso ou
  truncado sai da estimativa e fica listado; não se descarta um bloco por a
  latência ser alta.

`duration_s` é a duração efetiva, com a drenagem dos pedidos em voo. Não é a
duração configurada. O denominador de `throughput_per_s` é essa parede. O
numerador são os pedidos terminados, incluindo falhas. Throughput útil, no
resumo, usa só os sucessos sobre a mesma parede.

Secundários, fora desta célula: SSE, concorrência maior, CPU, working set.
RSS continua vazio no cgroup v2.

### Piloto de variância

12 blocos. As seis permutações dos três caminhos, duas vezes, na ordem da
semente. Janela candidata: 20 s de aquecimento e 60 s medidos por caminho.
Carga configurada: 12 × 3 × 80 s = 2880 s, 48 minutos, mais trocas. Não é a
coleta final.

### Tamanho final

Os alvos de meia-largura de 1 ms e efeito de 2 ms não estão aprovados. São a
escala do overhead de mediação neste mock, não a latência do modelo. A conta
de 30 blocos × 3 caminhos × 80 s é 120 minutos de carga, não 40. A grade com
JSON e SSE e duas concorrências continua na ordem de oito horas e não está
aprovada. O piloto abaixo não adota nenhum desses números como N.

### Piloto de variância executado

Coleta local, não confirmatória:
`experiments/gateway-comparison/data/pilot-variance-20261105T1750`
no checkout limpo `melis-exp-var`, commit medido
`86dc5ce763ce5a6580c4f967a3e413191a7a92d9`. Identidade
`col-33439a62b8ab`, nonce `acc52bf35d1048a19878c9ceca339e1a`, definição
`exp-rq1-comparative-4e19a406934a`. Fase `pilot`. Backend
`MOCK_LATENCY_MS=0`. As seis permutações ocorreram duas vezes.

A imagem local continua
`sha256:f87640f8e88636fcec54ebf3773f4d696037dd5481cc2b069d7a1193a991883e`,
criada em 2026-11-05T20:45:29Z, amd64. `RepoDigests` está vazio. A imagem não
tem `Config.Labels`, e `docker history` não contém um commit. O manifesto
original não foi reescrito. O arquivo
`provenance-supplement.json`, no mesmo diretório, liga
`col-33439a62b8ab` ao SHA-256 completo do manifesto
`6f5533c6c93afbfc6c58a9f5c443970256346f095dbe18d6cacdb84dad3c1f45`.
O commit de origem do binário fica nulo: não há label, e esta auditoria não
reconstruiu a imagem. O `Created` da imagem é anterior ao commit de execução
`86dc5ce7` (20:47:50Z) e posterior ao pai `61557ffb` (20:39:19Z). O
`git diff --numstat` de `Dockerfile`, `Cargo.toml`, `Cargo.lock` e `src`
entre esses dois commits é vazio. Isso é equivalência dos insumos, não um
rótulo da imagem.

Janelas: 72/72 terminaram por `deadline`, nenhuma truncada. A duração
efetiva medida ficou entre 60,000 s e 60,008 s. Oito das 36 ficaram
arredondadas em 60,0; isso não é a duração configurada apresentada como
medida. Pedidos medidos: 1.401.117, com zero falhas. Nenhum bloco excluído.
Throughput útil sobre a parede efetiva: direto 1154 req/s, Melis 684 req/s,
LiteLLM 107 req/s.

Contrastes, média entre blocos das diferenças de medianas, n=12, desvio
amostral:

- Melis − direto: 0,520 ms, s=0,088 ms. Bootstrap percentil exportado:
  0,478 a 0,571 ms.
- LiteLLM − direto: 7,887 ms, s=0,490 ms. Bootstrap: 7,634 a 8,157 ms.
- Melis − LiteLLM: −7,367 ms, s=0,507 ms. Bootstrap: −7,645 a −7,110 ms.

A releitura confirma o estimador sem ajuste. As fatias abaixo são
descritivas, com n=6 ou n=4, e não substituem a média dos 12 blocos.

Melis − direto: 0,502 ms na primeira metade e 0,538 ms na segunda.
Correlação descritiva com o índice do bloco: 0,17. Por posição do Melis,
n=4: 0,482, 0,559 e 0,519 ms.

LiteLLM − direto: 8,143 ms na primeira metade e 7,631 ms na segunda.
Correlação descritiva com o índice do bloco: −0,47. Por posição do LiteLLM,
n=4: 7,411 ms (s=0,097), 8,016 ms (s=0,409) e 8,234 ms (s=0,467). A posição
e o tempo estão misturados neste desenho; 12 blocos não separam os dois.
A média sem ajuste permanece 7,887 ms. Não há estimativa corrigida por
ordem. A correlação descritiva desse contraste com o load1 médio do bloco
é 0,59, e com as páginas de swap entrando no intervalo do bloco é 0,47.
São 12 pares, no mesmo eixo temporal, e não são um teste.

Melis − LiteLLM acompanha o contraste do LiteLLM, com sinal oposto:
−7,641 ms e −7,093 ms nas duas metades. p-valores não foram calculados.

O host não estava quieto. MemAvailable ficou entre 11,4 e 14,9 GiB. O swap
de 8 GiB permaneceu quase cheio e todos os 104 intervalos de 30 s moveram
páginas: 9165 para dentro (~36 MiB) e 8015 para fora (~31 MiB) ao longo da
execução. Às 21:05:21Z um intervalo moveu 4559 páginas para dentro e 3682
para fora, com 12,4 GiB livres e load 11,8. O load1 foi de 4,0 a 15,1 em
16 CPUs. Não é thrashing. Também não é um resultado de host em repouso.

Parede observada: 53 min 11 s para 48 min de carga configurada. As trocas
acrescentaram cerca de cinco minutos.

### Releitura

A agregação foi reexecutada para arquivos novos, sem sobrescrever a coleta.
O código de análise é `agg-1.2.0-1185a4fe8930`, SHA-256 de `aggregate.py`
`1185a4fe8930d8853b724230f275d6d1253ab0259248ef27877c83871f57f903`.
O validador releu o diretório original e terminou sem erros.

`requests.csv` tem 1.866.300 linhas de dados. O aquecimento tem 465.183 e a
medição 1.401.117. A soma é 1.866.300. A linha de cabeçalho explica a
contagem de 1.866.301 linhas no arquivo. Todas as linhas têm `status=ok`.
Há uma única coleta, `col-33439a62b8ab`, e um único `experiment_id`.
As 36 execuções medidas pareiam os três caminhos nos 12 blocos. As seis
permutações aparecem duas vezes. Cenário de medição `rq1_fixed_load` e
aquecimento `rq1_warmup` ficam separados. Backend determinístico, streaming
falso, concorrência 1, perfil mínimo, modelo `mock-model`.

As 72 janelas terminam por `deadline`, com `truncated=false`. A duração
efetiva fica entre 60,000 s e 60,008 s; a pedida é 60 s. Alguns registros
efetivos aparecem como 60,0 pelo número de casas. O campo pedido continua
ao lado. `drained_n` é 1 em Melis e LiteLLM e 0 ou 1 no direto: o pedido
em voo entra na parede e no numerador. `throughput_per_s` usa essa parede.
Não há bloco excluído e não há falha medida.

### Planejamento de N, sem aprovação

A largura exportada é a meia-largura marginal do intervalo t bilateral de
95%, df = N−1, `t × s / √N`. Ela não é simultânea. Holm, na família de
três contrastes e alfa de família 0,05, é o plano para os testes da coleta
final. Holm não transforma esses intervalos em intervalos simultâneos.
Nenhum intervalo simultâneo é proposto. Poder não foi calculado: não há
efeito relevante aprovado nem uma região de rejeição usada para dimensionar
N. O desvio do piloto não é tratado como conhecido.

A coluna de sensibilidade usa o limite superior de 95% de uma qui-quadrado
para sigma, df = 11, sob diferenças de bloco iid normais:
`s × √(df / χ²_{df, 0,05})`. Para estes dados o fator é cerca de 1,55.
Não é um multiplicador escolhido à mão. As metas de 0,50 ms e 0,25 ms não
estão aprovadas.

0,50 ms é da ordem do próprio overhead Melis − direto observado (0,52 ms).
Uma meia-largura dessa ordem, se a estimativa se mantiver, mal separa
“cerca de 0,5 ms” de “cerca de 1 ms” nesse contraste, e é uma fração pequena
do contraste com o LiteLLM, perto de 8 ms. 0,25 ms é cerca de metade do
overhead Melis − direto. Continua sendo overhead de mediação num mock
submilissegundo, não latência visível de modelo.

Meia-largura plug-in / limite superior de sigma, em ms:

| Contraste | s | N=12 | N=18 | N=24 | N=30 |
|---|---:|---:|---:|---:|---:|
| Melis − direto | 0,088 | 0,056 / 0,086 | 0,044 / 0,068 | 0,037 / 0,057 | 0,033 / 0,051 |
| LiteLLM − direto | 0,490 | 0,311 / 0,483 | 0,244 / 0,378 | 0,207 / 0,321 | 0,183 / 0,284 |
| Melis − LiteLLM | 0,507 | 0,322 / 0,500 | 0,252 / 0,391 | 0,214 / 0,332 | 0,189 / 0,294 |

Carga configurada: N × 3 × 80 s. Overhead de troca observado neste piloto:
25,9 s por bloco, parede de 3191 s para N=12. Extrapolação, não medida nova:
N=18 cerca de 80 min, N=24 cerca de 106 min, N=30 cerca de 133 min, somando
essa troca. N=12, 18, 24 e 30 repetem as seis permutações 2, 3, 4 e 5 vezes.

Sob o plug-in, 0,50 ms cabe em N=12 nos três contrastes. Sob o limite de
sigma, Melis − LiteLLM em N=12 fica em 0,500 ms, na borda da meta. N=18
fica abaixo de 0,50 ms nos três, também sob esse limite. A meta de 0,25 ms,
sob o limite de sigma, não cabe nem em N=30 no contraste mais largo (0,294
ms). O mesmo método, fora da grade pedida, só passa de 0,25 ms nesse limite
por volta de N=48, oito repetições de cada permutação.

Recomendação condicional, ainda não aprovada, restrita a esta célula:
se a meta adotada for 0,50 ms e o planejamento usar o limite de sigma, N=18.
N=12 permanece o piso balanceado e atende 0,50 ms no plug-in. N=30 não é
recomendado para obter significância e não cumpre 0,25 ms sob o limite de
sigma. A decisão de N, da meta e do host continua em aberto. Isto não se
transfere para SSE, concorrência maior ou o backend de 100 ms.

## RQ2

A proposta abaixo é informada pelo estudo exploratório e pelo rascunho
anteriores. Não é uma escolha feita sem conhecimento prévio. Não será
ajustada com o conjunto de avaliação, que não foi gerado nem lido.

Políticas, no commit `5769ff0e`:

- Primária proposta: `semantic_guarded_trimming` com `semantic_scoring`
  ausente, isto é, a heurística de `SemanticGuardedCompactor::calculate_score`.
  Defaults congelados em `CompactorConfig`: `token_threshold` 4096,
  `max_history_messages` 20, `target_token_ratio` 0,45, `min_final_tokens`
  200, `critical_markers` vazio, `preserve_critical_markers` verdadeiro,
  contagem `estimated`, tokenizer `cl100k_base`. Pesos do escore: sistema e
  última pergunta +100, marcador +80, número ou porcentagem +30, rota +30,
  data +20, termo da pergunta +20, palavra técnica +15, “contexto auxiliar”
  −30, preenchimento −20, resposta curta de assistente −20.
- Baseline de descarte por idade: política cronológica.
- Referência de qualidade: direto, sem compactação. Controle de mediação:
  Melis sem compactação (`strategy: none`, como nas rotas determinísticas).
- Secundárias: `semantic_scoring` embedding e híbrida. Não entram na
  decisão primária.

Rubrica 0–3. Dimensões: fatos críticos marcados no caso, fatos exigidos não
críticos, contradição que muda a decisão, e se a resposta endereça a
pergunta. A nota é a mais baixa exigida por essas dimensões, não uma média
delas.

- 3: todos os fatos exigidos, sem contradição. Em `dev-port-late`, a resposta
  diz 7071.
- 2: todos os críticos presentes e falta um fato não crítico. Em
  `dev-budget-early`, o orçamento 128000 está certo e a estratégia “nenhuma”
  não aparece.
- 1: responde, mas falta um fato exigido não crítico ou há contradição que
  não muda a decisão. Em `dev-distractor-color`, o modelo `mock-model` está
  certo e a cor azul é afirmada como se fosse exigida.
- 0: fato crítico ausente ou contradito, ou um fato inventado muda a
  decisão. Em `dev-port-late`, a resposta diz 8080 ou omite a porta.

Falha crítica: um item de `critical_facts` ausente ou contradito. A nota
desse caso é 0. Adjudicação: dois leitores; o autor do caso não é o único.
O desempate de um fato crítico usa a lista marcada no caso, não a média das
notas. Um par em que um leitor vê falha crítica e o outro não é um par
discordante e fica na tabela, sem ser apagado.

δ = 0,25 continua sem aprovação. Uma queda média de 0,25 na escala 0–3 pode
ser um ponto a menos em um quarto das respostas, ou uma falha maior em uma
fração menor. A média sozinha tolera essa mistura. O que a proposta tolera,
se uma margem vier a ser aprovada, são diferenças de formulação que não
acrescentam falha crítica. O que não se tolera é falha crítica adicional em
relação ao baseline sem compactação. A retenção de fatos críticos e a de
fatos não críticos são desfechos separados. A média da rubrica fica
descritiva.

“Nenhuma falha crítica adicional nesta amostra” descreve a amostra. Não é
garantia de que o risco não aumenta na população, nem prova de equivalência.
O relatório de cada contraste de política traz a contagem de pares em que só
o braço compactado falha no crítico, só o baseline falha, os dois falham, e
nenhum falha, mais o número de pares discordantes entre leitores. Um zero
observado permanece um zero nesta amostra.

Famílias de `quality/dev_cases.json`: `critical_number_late`,
`critical_number_early` e `distractor`. Uma família inteira é desenvolvimento
ou avaliação. Quase duplicata: mesmo fato crítico e o mesmo esqueleto de
pergunta dentro da família; esse caso não entra. O texto é original deste
experimento. O repositório não tem `LICENSE`. Nenhuma licença foi escolhida
aqui. Opções, para decisão posterior: manter os casos como material original
não licenciado dentro do repositório privado; adotar depois uma licença de
repositório que cubra o código e estes casos; ou um aviso separado, só para
os casos originais, com todos os direitos reservados até essa escolha. O
escopo possível são os casos escritos para o experimento. Não cobre pesos
de modelo, a imagem do LiteLLM nem texto de terceiros. CC0 não se aplica.

## Plano de host, não aplicado

Esta máquina, em 2026-11-05, não passa no critério de admissão abaixo: o
swap de 8 GiB está quase cheio, houve movimento de páginas durante o piloto
e há dezenas de contêineres alheios. Eles não foram parados. A coleta
confirmatória fica bloqueada neste host até uma janela que passe o critério
ou até outro host. O limiar de 1024 páginas num preflight de 5 s não prova
estabilidade da hora seguinte.

Admissão, numa amostra de 60 s antes de começar, com a coleta ainda parada:

- MemAvailable no mínimo 8 GiB, e nenhuma leitura da amostra abaixo de 6 GiB.
- Páginas de swap, entrada mais saída, abaixo de 256 na amostra inteira.
- load1 abaixo de 2 nesta máquina de 16 CPUs, antes da carga da própria
  coleta.
- Pelo menos 20 GiB livres no disco que receberá os CSVs.
- Redis, quando a célula precisar dele, dedicado e identificado; o Redis de
  outros stacks não é o da coleta.

Durante a coleta, a cada 30 s, o mesmo registro de `host_pressure.csv`:

- Violação se MemAvailable cair abaixo de 6 GiB, ou se um intervalo de 30 s
  mover 1024 páginas ou mais, ou se o disco livre cair abaixo de 10 GiB.
- load1 alto produzido pela própria coleta não é, sozinho, violação. O
  piloto chegou a 15 com a carga ligada.
- Ao violar: preservar CSVs e logs, marcar o bloco inteiro e não interromper
  no meio de um caminho. Os três caminhos daquele bloco terminam. O bloco
  sai da estimativa e permanece listado. Uma repetição, se a violação tiver
  cessado, refaz a permutação completa no fim, uma vez. Não se repete só o
  caminho de pior latência. Se a violação continuar, a coleta para e os
  blocos já gravados ficam marcados.

Opções enquanto não houver host que passe: esperar e amostrar de novo; usar
outra máquina com o mesmo protocolo de proveniência; não baixar o limiar
para esta máquina passar.

## Pendências

- A coleta confirmatória continua bloqueada até aprovação explícita de
  desenho, N, ambiente e critérios.
- N e as metas de 0,50 ms e 0,25 ms não estão aprovados. A recomendação
  condicional é N=18 se a meta for 0,50 ms com o limite de sigma.
- δ e a heurística continuam propostas. Nenhuma licença foi escolhida.
- Não misturar CSVs 1.0.0 ou 1.1.0 com `phase=final`.
- `src/bedrock.rs` continua fora dos commits. Os dois commits de `main` não
  foram mesclados.
- G1 ignorada, neste checkout limpo, em 2026-11-05T22:05Z: 34 aprovados,
  0 falhas, 55,28 s, incluindo os dois testes de probe e os smokes com
  Ollama 0.11.4 e Redis 7.4.11. O host estava compartilhado; o resultado
  não é um atestado de host quieto.
