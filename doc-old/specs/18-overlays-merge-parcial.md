# 18 — Overlays com merge parcial (delta declarativo sobre o arquivo do repo)

**Status: ESCOPO FUTURO — nada disto está implementado.** Pedido do Gabriel
em 18/10/2026, depois do incidente real: o overlay do `vite.config.js` do
vippers era uma CÓPIA do vipa (`origin`/`allowedHosts`/`cors` de `vipa-*`
e `hmr: false` já sem verdade) — o arquivo evoluiu no repo, o overlay
congelou, e ninguém percebeu até o live quebrar.

## Objetivo

Fim do dilema "arquivo que precisa de delta de VM tem que sair da sync e
virar cópia manual congelada". Com merge parcial, o arquivo continua
SINCRONIZADO do repo (evolui no Windows, chega na VM pela sync normal) e a
overlay passa a declarar só o DELTA — o que substituir pelo quê — aplicado
por cima a cada `flux up`. Manter também, intacto, o modo atual de
arquivo inteiro.

## Dois modos por arquivo (coexistem)

### Modo INTEIRO (hoje, mantido)

`.flux/overlays/caminho/arquivo` existe → tar por cima da árvore da VM no
step `[4.5/6]` do `flux up`. Semântica de hoje, zero mudança.

### Modo MERGE (novo)

A overlay declara, por arquivo-alvo, uma LISTA de patches mínimos. O
Gabriel fala "o quê substituir pelo quê"; o mecanismo aplica o mínimo e o
resto do arquivo vem do repo pela sync. O `vite.config.js` para de ter
cópia: o repo evolui livre e a overlay prende só o bloco `server`.

Contrato do patch (o FORMATO exato é decisão da implementação; o
CONTRATO é este):

- **Declarativo e revisável** — texto plano no `.flux/`, legível e
  versionável. Candidatos: TOML com âncoras exatas (find/replace,
  insert-after-âncora, remove-bloco). Sem regex solto, sem fuzzy: âncora é
  texto EXATO, verbatim, do arquivo.
- **Fail-loud** — âncora não encontrada, arquivo-alvo ausente, patch
  duplicado: a overlay é RECUSADA naquele arquivo, o `flux up` avisa com o
  motivo (padrão "nada silencioso") e o passo segue para os demais
  arquivos — recusa nunca vira aplicação pela metade.
- **Idempotente** — aplicar o mesmo patch duas vezes (dois `flux up`
  seguidos) produz o mesmo arquivo. Patch que "acumula" é bug.
- **Otimizado** — só o delta é declarado/trafegado; o arquivo inteiro
  nunca precisa existir em duplicidade. A sync entrega o base, o patch
  transforma em memória, o resultado grava na VM.
- **Precedência explícita** — sync → patch → compose. Um arquivo não pode
  ter os dois modos ao mesmo tempo (INTEIRO + MERGE no mesmo alvo = ERRO
  de configuração, não aviso — ambiguidade é bug de config).
- **Auditoria** — o que o patch mudou aparece no log do `up` (arquivo +
  número de patches aplicados), como os overlays de hoje reportam
  "N arquivo(s)".

## Por que merge e não só INTEIRO

1. Arquivo de config do repo evolui no git normal; overlay para de apodrecer
   (o incidente de 18/10 é o contraexemplo canônico).
2. Delta de 10 linhas não exige manter cópia de 88 linhas em paralelo.
3. Merge bem feito torna o extra_ignore do arquivo DESNECESSÁRIO — o
   `vite.config.js` volta a sincronizar (o patch reescreve só o bloco
   `server` na VM), simplificando o `flux.toml` e matando o modo
   "cópia manual" da rotina.

## Aceites (quando implementar)

1. Projeto com overlay MERGE no `vite.config.js`: editar o arquivo no
   Windows → sync replica → `flux up` aplica o patch → o bloco `server` na
   VM reflete o patch e o RESTO reflete o repo (diff contra o repo mostra
   só o delta).
2. Quebrar a âncora no repo (renomear o bloco que o patch procura) →
   `flux up` avisa a recusa com o motivo, live segue com o arquivo do repo
   puro (comportamento honesto, não silencioso).
3. Aplicar `flux up` duas vezes seguidas → hash do arquivo na VM idêntico
   nas duas.
4. Modo INTEIRO em outro arquivo coexiste sem interação.
5. Patch + INTEIRO no mesmo alvo → erro de config explícito.

## O que NUNCA fazer

- Nunca aplicar patch parcial e calar sobre falha de âncora.
- Nunca usar regex/âncora fuzzy — âncora é texto exato.
- Nunca exigir overlay INTEIRA para mudar 3 linhas de um arquivo grande.
- Nunca congelar/copiar manualmente arquivo do repo para "caber na overlay" —
  se o merge não cobre, o arquivo INTEIRO sai do repo e a cópia vira
  responsabilidade explícita e documentada do projeto.
