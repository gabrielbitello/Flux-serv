---
description: Cria o flux.toml do projeto atual (repo onde o comando roda), valida o parse e garante o aviso da plataforma Flux no .zcode/AGENTS.md do projeto. Argumento opcional: o que você já sabe do projeto (framework, banco, domínio).
argument-hint: [contexto do projeto, se houver]
---

# /flux:init — registrar este projeto na plataforma Flux

Objetivo: deixar o repo atual operável pelo flux — `flux.toml` válido na raiz e aviso obrigatório no `.zcode/AGENTS.md`. Responda em pt-BR.

## Passo 1 — Reconhecer o terreno

1. Raiz do repo = diretório atual. Se já existir `flux.toml`, PARE: mostre o conteúdo e pergunte se o objetivo é revisar/ajustar (nunca sobrescrever sem decisão explícita — o wizard do flux também recusa).
2. Detecte o framework com evidência: `composer.json` (laravel?), `package.json` (node?), `Cargo.toml` (rust?), nenhum → static/custom. Anote a versão de PHP/node se visível.
3. Banco: procure `DB_*` no `.env.example`/`config` — mysql/mariadb? O projeto quer banco próprio (`[db] own = true`, um container dedicado `flux-db-{grupo}`, com TTL e dump automático) ou o banco único compartilhado? Se o argumento do comando não responder, PERGUNTE — uma pergunta, sem interrogatório.
4. Domínios: default é `{pasta}.bitello.xyz` (prod) e `{pasta}-d.bitello.xyz` (dev). Só toque se o usuário pedir diferente.

## Passo 2 — Escrever o flux.toml

Regras duras:

- **Chave desconhecida = parse error** (validação estrita do flux). Use SÓ chaves que existem; quando em dúvida, consulte o template `C:\Users\ieubi\Documents\Projetos\Flux\flux.toml.example` e a doc `C:\Users\ieubi\Documents\Projetos\Flux\docs\configuration.md`.
- Mínimo útil: `name`, `framework`. Adicione apenas o que o projeto realmente usa (`[db] own`, `[build] command` se não for o default do framework, `[sync] extra_ignore` se houver diretório gerado que a sync não pode levar — `vendor/`, `node_modules/`, `storage/` JÁ são ignorados globalmente).
- NUNCA segredo no flux.toml (versionado). Segredo é coisa de `.env` na VM (`flux env push`).
- Nome: default = nome da pasta (`name` explícito só se a pasta tiver nome estranho).

## Passo 3 — Validar ANTES de dizer que está pronto

1. Rode `flux status` na raiz do projeto. Saída esperada: aviso de projeto não registrado (normal — registra no primeiro `flux up`) OU status normal. **Parse error do flux.toml = consertar e repetir.** Não declare pronto com parse error.
2. Se a ferramenta MCP `flux` estiver disponível, `flux_projetos` serve pra conferir se o nome escolhido colide com projeto registrado.
3. NÃO rode `flux up` por conta própria — subir projeto é decisão do Gabriel.

## Passo 4 — Aviso no .zcode/AGENTS.md

Se `.zcode/AGENTS.md` não existir, crie. Se existir, leia ANTES de editar (ambiente compartilhado) e acrescente a seção abaixo SÓ se ainda não houver aviso de flux (procure por "Flux" ou "flux.toml" antes de duplicar):

```markdown
## Flux (obrigatório)

Este projeto roda na plataforma Flux (Docker na VM 192.168.140.2; nada de
build/docker/npm dev no host). Todo ciclo passa pelo CLI `flux` ou pelas tools
MCP `flux_*` (skill `flux` do plugin flux): up, logs, exec, banco, env, sync.
Regras de ouro na skill — violar = incidente. `flux.toml` é a config do
projeto; `.env` vive SÓ na VM.
```

## Passo 5 — Fechamento

Reporte: o que foi criado/editado, resultado do `flux status` (parse ok), e o próximo passo de verdade: `flux up` na raiz (com `-m` se quiser revisar via wizard). Não prometa uptime nem registre o projeto por conta própria.
