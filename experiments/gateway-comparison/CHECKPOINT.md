# Checkpoint — coleta confirmatória ainda não autorizada

Este arquivo registra o desenho proposto depois dos pilotos técnicos de
2026-11-05. Não é resultado confirmatório. Os intervalos e estimativas dos
pilotos (incluindo qualquer bloco único ou de três blocos) não entram na
avaliação final. Nada aqui foi otimizado num conjunto final, porque esse
conjunto não foi coletado.

## O que os pilotos suportam

A infraestrutura dos três caminhos sobe, grava CSV 1.1.0 e valida. Isso foi
exercido com o backend determinístico (JSON e SSE, concorrência real) e com
um piloto mínimo no Ollama local (`llama3.2`, duas requisições JSON por
caminho). O host estava carregado: swap cheio e outros stacks em execução.
Não é ambiente para medição representativa.

## Desfecho primário

Família primária, uma célula só, para limitar a multiplicidade:

- População: backend determinístico, perfil mínimo (sem cache, compactação ou
  guardrails), JSON, concorrência configurada 1.
- Unidade independente: bloco. Requisições dentro do bloco são aninhadas.
- Contrastes: `melis − direct` e `litellm − direct` na mediana de latência de
  conclusão (`latency_ms`), pareados pelo bloco.
- Correção: Holm entre esses dois contrastes, alfa 0,05.
- Papel: confirmatório somente depois da aprovação e de uma coleta `phase=final`
  num host quieto. Tudo o que existe hoje é `evidence_role=exploratory`.

Secundários, sem reivindicação confirmatória: os mesmos contrastes em SSE e na
concorrência 8; throughput útil; taxa de erro; delta do contador de CPU;
working set. Pico observado no backend não é a concorrência configurada e não
substitui essa concorrência. `memory_rss_bytes` permanece vazio no cgroup v2.

## Tamanho amostral

Os pilotos têm um bloco por estrato. Com n = 1 não há desvio-padrão entre
blocos, então não há cálculo honesto de poder nem intervalo. A proposta é:

1. Num host quieto, um piloto de variância com 10 blocos independentes na
   célula primária. Esse piloto continua exploratório.
2. A partir do desvio-padrão desses 10 blocos, fixar N para um intervalo de
   95% da diferença pareada de medianas com meia-largura de 1 ms, ou poder
   80% para um efeito de 2 ms, o que exigir mais blocos. O número só é
   travado depois dessa estimativa.
3. O candidato de 30 blocos do protocolo não está aprovado e não deve ser
   executado neste host.

## Qualidade

Não há dados de qualidade. A margem 0,5 não é adotada: numa rubrica 0–3, 0,5
é um sexto da escala e pode esconder uma queda sistemática de meio ponto.

Margem proposta, ainda sujeita a aprovação e não estimada de dado algum:
δ = 0,25 na diferença pareada média `política − baseline sem compactação`.
Isso corresponde, na média, a perder um ponto inteiro em cerca de um caso a
cada quatro. O critério de não inferioridade segue o protocolo: limite
inferior unilateral de 95% acima de −δ. Equivalência, se for alegada, é um
desenho separado (intervalo bilateral de 90% dentro de [−δ, +δ]).

Política primária escolhida antes de qualquer avaliação: **cronológica**.
Motivo: é a política determinística mais simples já implementada. Não houve
resultado de qualidade que a selecionasse. Heurística, embedding e híbrida
ficam como ablações secundárias. Baseline de qualidade: provedor direto sem
compactação. Controle de mediação: Melis sem compactação.

Dataset proposto: conversas sintéticas geradas por família de template, com
fatos, posição, tamanho e rubrica 0–3 ancorada por exemplos. Split de
desenvolvimento e avaliação por família, não por reembaralhar o mesmo
template. Licença pretendida: CC0 para o material sintético gerado no
repositório. Nenhum corpus externo foi escolhido. Os três fixtures atuais por
split continuam smoke, não avaliação.

## Tempo e recursos da coleta final

Não executar a grade larga neste host. Uma conta só para planejamento, se a
célula primária for 30 blocos com 20 s de aquecimento e 60 s medidos, um
caminho por vez: cerca de 40 minutos de carga, mais a troca de gateway. A
grade completa do protocolo (30 blocos × JSON/SSE × duas concorrências × três
caminhos × 80 s) passa de 8 horas de carga e não está aprovada. Ollama local
não tem custo de API; não usar provedor pago sem uma decisão explícita.

## Pendências antes da coleta final

- Travar N com o piloto de variância num host sem swap saturado e sem stacks
  alheios.
- Aprovar δ = 0,25 e a política cronológica, ou substituí-los antes de coletar.
- Congelar pin do LiteLLM (`main-v1.83.10-stable`, digest no README). Não é a
  linha de desenvolvimento atual; a escolha foi anterior aos resultados.
- Não misturar os CSVs históricos 1.0.0 (`data/pilot/`, `data/pilot-conc/`)
  nem os pilotos 1.1.0 desta sessão com `phase=final`.
