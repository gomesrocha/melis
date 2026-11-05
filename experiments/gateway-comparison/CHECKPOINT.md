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
escala do overhead de mediação neste mock, não a latência do modelo. A
proposta de N sai do desvio entre blocos deste piloto, com o estimador acima
e Holm na família de três. Não se escolhe N para obter significância. A conta
de 30 blocos × 3 caminhos × 80 s é 120 minutos de carga, não 40. A grade com
JSON e SSE e duas concorrências continua na ordem de oito horas e não está
aprovada.

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

- Executar o piloto de variância num checkout limpo, com imagem construída
  desse commit, ou declará-lo bloqueado.
- Propor N com o desvio observado. Não tratar 1 ms ou 2 ms como aprovados.
- Não aprovar δ nem a heurística como decisão final até revisão deste texto.
- Não misturar CSVs 1.0.0 ou 1.1.0 com `phase=final`.
