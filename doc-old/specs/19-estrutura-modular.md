# 19 — Estrutura de arquivos modular (anti-monolito)

**Status: ESCOPO FUTURO para refatoração — nada disto deve ser feito agora
sem fase planejada.** Preferência fixada pelo Gabriel em 18/10/2026: ele
ODEIA arquivo monolito com centenas/milhares de linhas. Quer projeto BEM
modular e fragmentado: diretório por domínio, arquivo por responsabilidade.

## Princípio

- **Arquivo grande = bandeira vermelha.** Acima de ~1000 linhas pede
  refatoração; acima de ~500 já merece justificativa.
- **Pasta por domínio, arquivo por função.** Lógica de banco → diretório
  `db/` com vários arquivos dentro (ex.: conexão, migrations, guard, seed —
  um por concern), nunca um `db.rs` de 1100 linhas.
- **Um tipo/módulo por responsabilidade.** Se o nome do arquivo precisa de
  "e" ("router e proxy e páginas"), já são arquivos demais nele.
- **Testes acompanham o fragmento** — arquivo de teste por módulo movido,
  sem testes-sumidouro.

## Referência de gosto (não de padrão obrigatório)

O CMS do vippers (Filament) tem a estrutura que o Gabriel FORÇOU e gosta:
Resource enxuto que só delega; `Tables/<Nome>Table.php` com a configuração
de tabela; colunas e filtros separados em `Support/<Nome>ListColumns.php` e
`Support/<Nome>ListFilters.php`; forms e infolist em `Schemas/`; páginas em
`Pages/`; relation managers em `RelationManagers/`; painéis organizados em
`Clusters/`. O padrão Filament em si NÃO é o mandato — é o EXEMPLO da
organização desejada: muita pasta, arquivos curtos com nome que diz o que
fazem, zero deus-objeto.

## Estado atual do Flux (evidência, `wc -l` em 18/10/2026)

Nove arquivos acima de 1000 linhas — candidatos à fragmentação:

| Arquivo | Linhas |
|---|---|
| `crates/flux-cli/src/main.rs` | 2574 |
| `crates/fluxd/src/panel.rs` | 1985 |
| `crates/fluxd/src/api.rs` | 1673 |
| `crates/fluxd/src/observe_page.rs` | 1283 |
| `crates/flux-cli/src/sync.rs` | 1268 |
| `crates/flux-cli/src/api.rs` | 1224 |
| `crates/fluxd/src/compose.rs` | 1181 |
| `crates/fluxd/src/observe.rs` | 1105 |
| `crates/fluxd/src/state.rs` | 1037 |

Direção de fragmentação (exemplos, decisão fina por fase): `main.rs` do CLI
→ um módulo por subcomando; `panel.rs` → rotas/handlers/render separados;
`state.rs` → schema, queries por domínio, migrações; `compose.rs` →
render/override/wrapper/live.

## Regras da refatoração (quando acontecer)

1. **Mover, nunca reescrever.** Refactor de estrutura não melhora lógica —
   melhora de lógica é commit separado, depois.
2. **Por fases curtas**, cada uma: suíte verde antes, movimento, suíte
   verde depois, commit próprio (`refactor(escopo): move X para Y`).
3. **Nada de "aproveitar que está aberto".** Escopo fiel: só estrutura.
4. **Visibilidade mínima reavaliada** ao mover (`pub(crate)` volta a ser
   `fn` quando o vizinho passa a ser módulo interno).
5. `cargo clippy -D warnings` e testes por fase — gate inegociável.

## O que NUNCA fazer

- Nunca criar arquivo novo com 800 linhas "porque ia ficar 40 arquivos".
- Nunca mover código e alterar comportamento no mesmo commit.
- Nunca inventar camada de abstração só para "parecer modular" — módulo
  existe porque o domínio pede, não por estética.
