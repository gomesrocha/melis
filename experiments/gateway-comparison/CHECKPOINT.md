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

A imagem `gateway-comparison-melis:latest` é
`sha256:f87640f8e88636fcec54ebf3773f4d696037dd5481cc2b069d7a1193a991883e`,
criada em 2026-11-05T20:45:29Z, amd64, sem digest externo. Foi construída no
commit pai `61557ffb`. O diff até `86dc5ce7` só altera
`harness/host_pressure.py` e o teste correspondente; `src`, `Dockerfile` e
`Cargo.lock` são iguais. O manifesto diz que a imagem saiu do checkout da
execução; essa frase é do runner e não deve ser lida como se o binário
tivesse sido reconstruído depois do commit do gate de swap.

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

Melis − direto quase não anda no tempo: 0,502 ms na primeira metade e
0,538 ms na segunda. LiteLLM − direto cai de 8,143 ms para 7,632 ms
(correlação −0,47 com o índice do bloco). Na posição 0 o LiteLLM fica
7,411 ms acima do direto; na posição 2 fica 8,234 ms. A diferença cabe no
desvio de 0,49 ms e não troca o sinal. p-valores não foram usados.

O host não estava quieto. MemAvailable ficou entre 11,4 e 14,9 GiB. O swap
de 8 GiB permaneceu quase cheio e todos os 104 intervalos de 30 s moveram
páginas: 9165 para dentro (~36 MiB) e 8015 para fora (~31 MiB) ao longo da
execução. Às 21:05:21Z um intervalo moveu 4559 páginas para dentro e 3682
para fora, com 12,4 GiB livres e load 11,8. O load1 foi de 4,0 a 15,1 em
16 CPUs. Não é thrashing. Também não é um resultado de host em repouso.

Parede observada: 53 min 11 s para 48 min de carga configurada. As trocas
acrescentaram cerca de cinco minutos.

### Proposta de N, ainda sem aprovação da coleta

Método de planejamento: meia-largura do intervalo t de Student bilateral de
95%, df = N−1, `t × s / √N`, com o s deste piloto. Holm não alarga esse
intervalo. Holm fica para a decisão da coleta final, na família de três,
alfa 0,05. Não se escolhe N para obter significância, e o poder não é o
vínculo aqui: os efeitos observados são muitas vezes o erro padrão de N=12.

Piso do desenho: 12 blocos, o menor N em que cada permutação aparece duas
vezes. Em N=12 a meia-largura t é 0,056 ms, 0,311 ms e 0,322 ms nos três
contrastes. Uma meia-largura de 1 ms, se viesse a ser aceita, já caberia em
N=4 no contraste mais largo. Um efeito mínimo de 2 ms é maior do que o
overhead Melis − direto inteiro, então não separa essa mediação.

Proposta para a coleta confirmatória desta mesma célula, se for aprovada:
N=12, aquecimento 20 s, medição 60 s, carga de 48 minutos mais as trocas.
Não proponho 30 blocos. Se a aprovação fixar meia-largura de 0,25 ms nos
contrastes com LiteLLM, o N passa a 19 no contraste Melis − LiteLLM. Esse
alvo também não está aprovado.

A proposta vale para mock de latência zero, JSON, concorrência 1 e perfil
mínimo, no mesmo tipo de host. Não transfere para SSE, concorrência maior
nem para o backend de 100 ms.

## RQ2

A política cronológica é baseline de economia agressiva, não a contribuição
de relevância. A primária proposta é a heurística já implementada em
`src/compactor.rs` (termos da última pergunta, marcadores, números, rotas,
penalidade de preenchimento), configuração congelada no commit medido. A
escolha é informada pelo estudo exploratório e pelo rascunho anteriores. Não
é uma escolha sem conhecimento prévio, e não será ajustada no conjunto final.
Embedding e híbrida ficam secundárias.

δ = 0,25 não está aprovado. A conta “um ponto em um quarto dos casos” explica
uma média e esconde uma falha crítica concentrada.

Rubrica 0–3, ancorada:

- 3: todos os fatos exigidos presentes, sem contradição, responde à pergunta.
- 2: todos os fatos críticos presentes; falta um fato não crítico.
- 1: responde, mas falta um fato exigido não crítico ou há contradição que
  não inverte a decisão.
- 0: falta ou contradição de um fato crítico, ou a resposta inventa um fato
  que muda a decisão.

Falha crítica: um fato marcado como crítico está ausente ou contradito. Esse
caso não passa de 0. Exemplo de 3: a conversa diz que a porta é 7071 e a
resposta diz 7071. Exemplo de 0: a resposta diz 8080, ou omite a porta quando
ela é o fato crítico.

Desfechos complementares, não substituídos pela média:

- Retenção de fatos críticos: `retidos / exigidos` só no subconjunto crítico.
  Limite proposto, ainda sem aprovação: nenhuma falha crítica a mais do que
  o baseline sem compactação. Uma média pode subir enquanto um fato crítico
  cai; esse caso não passa.
- Retenção de fatos não críticos: reportada à parte, sem δ.
- Média da rubrica: descritiva. Sem δ numérico até haver uma margem que não
  possa ser satisfeita quando o limite crítico falha.

Baseline de qualidade: direto sem compactação. Controle de mediação: Melis
sem compactação. Cronológica é o baseline de descarte por idade.

Casos de desenvolvimento estão em `quality/dev_cases.json`. São texto
original escrito para este experimento, não um corpus externo, e não estão
em CC0 por atribuição a material de terceiros. O repositório não tem arquivo
LICENSE; a licença destes casos é a do repositório quando ela existir, e até
lá ficam como material original não publicado à parte. O conjunto de
avaliação não foi gerado e não foi inspecionado. Famílias inteiras vão para
desenvolvimento ou avaliação; não se parte o mesmo template. A atribuição
das famílias de avaliação fica congelada antes de gerar esses casos.

## Pendências

- A coleta confirmatória continua bloqueada até aprovação explícita.
- Não tratar 1 ms ou 2 ms como aprovados. A proposta acima é N=12 para esta
  célula, pelo piso das permutações e pela precisão já observada.
- Não aprovar δ nem a heurística como decisão final até revisão deste texto.
- Não misturar CSVs 1.0.0 ou 1.1.0 com `phase=final`.
- O host desta execução teve swap em movimento o tempo todo. Uma coleta
  seguinte precisa registrar a mesma pressão e não ser descrita como host
  quieto se o movimento se repetir.
- `src/bedrock.rs` continua fora dos commits. Os dois commits de `main` não
  foram mesclados.
