# AGENTS.md — Flux (padrão de código obrigatório)

Vale para QUALQUER agente/sessão que mexa neste repo. O objetivo é simples:
o repo passou por uma refatoração modular completa (41/41 passos, 2026-10-19,
`docs/refactoring/execution-plan.md`) e NÃO volta a virar monólito. O plano
canônico é [`docs/refactoring/execution-plan.md`](docs/refactoring/execution-plan.md) —
este arquivo é o resumo executável e vence preguiça.

## Estrutura (a lei)

- **Arquivo > ~1000 linhas é suspeita.** Padrão atual: pasta por domínio,
  arquivo por função (`crates/flux-cli/src/comandos/`, `crates/fluxd/src/panel/`,
  `crates/flux-cli/src/sync/`). Antes de crescer um arquivo, pergunte: tem um
  segundo domínio aqui dentro? Separe.
- **Arquivos grandes que FICAM únicos de propósito** (não "consertar"):
  `observe.rs`, `state.rs`, `observe_page.rs` — coesos, com gatilhos de
  reavaliação documentados no plano (§"Decisões em defer"). Mexer neles sem
  ler esses gatilhos = erro.
- **Separação consulta × apresentação** (padrão `panel/`): toda SQL num
  módulo de leitura (`leitura.rs`), formatação pura em `formato.rs`, render
  em `paginas.rs`. SQL não cresce dentro de render.
- **Fonte única**: constante ou regra que existe 2× já está errada. Contratos
  têm um dono — nomes de projeto (`flux-common/src/nomes.rs`), hosts
  (`host.rs`), CSS/noindex de UI (`fluxd/src/ui.rs`), paths (`fluxd/src/paths.rs`),
  ssh batch (`flux-cli/src/ssh_util.rs`), constantes da VM (`sync/mod.rs`,
  reexportadas pelo `tunel.rs`). Duplicar = drift silencioso.
- **Visibilidade**: `pub(crate)` é o teto. Nada de `pub` em crate binário
  além do que o compilador exigir.
- **Movimento é VERBATIM**: extrair/mover código sem "melhorar" nada no mesmo
  commit. Melhoria é outro commit, com justificativa.

## Processo (obrigatório)

- **TRIO verde antes de qualquer commit**: `cargo fmt --check` +
  `cargo clippy --workspace -- -D warnings` + `cargo test --workspace`.
  Commit vermelho não existe.
- **1 commit por escopo** (um domínio/módulo por commit em refatorações;
  nunca amend em HEAD compartilhado; commitar com pathspec dos próprios
  arquivos — o Gabriel edita no mesmo working tree).
- **Refatoração NÃO implementa funcionalidade** (regra do Gabriel, 19/10/2026):
  estrutura + consertar problema JÁ diagnosticado (engolimento de erro etc.),
  declarado no commit. Feature = pedido separado.
- **Push só por ordem explícita do Gabriel.** Fluxo de landing atual: branch
  → PR → CI verde (`test` + `test-engine`) → ele mergeia.
- `docs/STATUS.md` (matriz SIM/PARCIAL/NÃO) e `PROGRESSO.md` (diário)
  atualizam em TODA sessão que mexe em código. Sem exceção.

## Armadilhas que já morderam (não morder de novo)

- **Pipe engole exit code**: `cargo ... | tail` retorna o exit do `tail`.
  Medir sem pipe, com `PIPESTATUS` ou `set -o pipefail`. (Mordeu 3×.)
- **`cargo fix` come o que não vê**: imports usados só em `cfg(test)`;
  `pub use` sem consumidor interno em crate binário (os re-exports da ponte
  `#[path]` do `sync` precisam `#[allow(unused_imports)]` — é contrato, não
  sujeira).
- **Ponte `#[path]`**: `tests/sync_tests.rs` recompila `src/sync/mod.rs`
  standalone — os filhos (`sync/spec.rs`, `sync/estabilidade.rs`) NÃO podem
  referenciar `crate::` do binário; usam `super::`. Formato obrigatório:
  directory `mod.rs`, nunca `sync.rs` + pasta.
- **Regex de `pub`**: `^(pub )?fn` NÃO casa com `pub(crate) fn`.
- **Painel e API bindam 127.0.0.1 na VM**: curl direto no 192.168.140.2:7421
  dá 000 e NÃO é bug — acesso por túnel ssh (é o que o CLI faz).
- **Toolchain pinada**: `rust-toolchain.toml` (1.92.0) é o que impede o
  fmt/clippy de quebrar sozinho; os steps do `.github/workflows/ci.yml`
  usam `dtolnay/rust-toolchain@1.92.0` — bumpar um sem o outro quebra o CI.
- **zcode-api (`Projetos/zcode-api`) é a API que alimenta o Claude Code do
  Gabriel** — não derrubar, não pausar, não brincar de sleep com ele sem
  ordem.

## O que NUNCA fazer (destilado do plano; texto integral em
[`docs/refactoring/execution-plan.md`](docs/refactoring/execution-plan.md) §10)

1. Dividir arquivo só para baixar contagem de linhas.
2. Mover código sem teste de caracterização rodando antes.
3. Quebrar API pública dos crates (preservar por re-export/delegação).
4. Misturar limpeza não relacionada com extração arquitetural.
5. "Melhorar" enquanto move (movimento verbatim; ordens estritas preservadas).
6. Mudar comportamento silenciosamente (mudança existe? DECLARA no commit).
7. Engolir erro (padrão do repo: `tracing::error!` + UI com a mensagem real).
8. Rodar build/assets na mão nem relincar o binário que o Gabriel está usando.
9. Push sem pedido; `--amend` em HEAD compartilhado.
10. Criar abstração onde uma função resolve.
