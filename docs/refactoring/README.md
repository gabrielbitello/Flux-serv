# Refatoração modular do Flux — auditoria e plano de execução

Data: 2026-10-18 · Branch de partida: `audit/modularizacao` · Escopo: 3 crates do workspace (`flux-cli`, `fluxd`, `flux-common`) + manifestos e CI.

## Objetivo

Registrar a auditoria estrutural completa do repositório e fixar O PLANO CANÔNICO de refatoração modular: que arquivos mudam, em que ordem, com que evidência, com que rede de testes e com que critério de aceite. O plano é executável em lotes pequenos, independentes e revisáveis — cada lote deixa o repo compilando e testando.

Este diretório é documento de trabalho: mudanças de código só acontecem quando um passo do plano for executado; nada aqui altera o comportamento do produto.

## Escopo

- **44 arquivos `.rs`** de produção e teste nos 3 crates (inventário em [inventory.md](inventory.md), com linhas medidas por leitura direta).
- **Manifestos e infra de verificação:** `Cargo.toml` (raiz), os 3 `Cargo.toml` de crate, `.github/workflows/ci.yml`, `flux.toml.example`, `scripts/host/` (3 arquivos).
- **Total in-scope: 53 itens. 100% auditados** (cobertura em [coverage-manifest.md](coverage-manifest.md)).

Fora de escopo: comportamento de produto (nenhuma mudança funcional é proposta, exceto onde um passo declara explicitamente mudança de comportamento — ex.: propagar erro de listagem de containers), VM, ssh, flux, deploy.

## Metodologia

1. **Auditores paralelos por domínio**, cada um com lente e lote próprios, todos somente leitura:
   - `auditor-nucleo-fluxd` — núcleo do daemon: `lib.rs`, `main.rs`, `ciclo.rs`, `decision.rs`, `events.rs`, `sleep.rs`, `temporary.rs`, `beat.rs` (+ `state.rs` leve).
   - `auditor-rede-fluxd` — borda HTTP: `router.rs`, `proxy.rs`, `guard.rs`, `pages.rs`, `edge.rs`, `waf.rs`.
   - `auditor-painel-observe` — UI do daemon: `panel.rs`, `panel_auth.rs`, `api.rs`, `observe.rs`, `observe_page.rs`, `deploy.rs`, `sync.rs`.
   - `auditor-dados-fluxd` — persistência e Docker: `state.rs`, `db.rs`, `sqlite_source.rs`, `docker.rs`, `compose.rs`.
   - `auditor-cli` — cliente: `main.rs`, `api.rs`, `sync.rs`, `tunel.rs`, `wizard.rs`.
   - `auditor-common` — fronteiras públicas do crate base: `config.rs`, `host.rs`, `protocol.rs`, `framework.rs`, `error.rs`, `lib.rs`, `flux.toml.example`.
   - `auditor-testes` — suítes, CI e manifestos: 13 itens (5 testes de integração do fluxd, `sync_tests.rs`, `ci.yml`, 4 `Cargo.toml`, `scripts/host`).
2. **Foco dedicado nos arquivos >1000 linhas** (9 auditores "foco", um por arquivo): `flux-cli/main.rs` (2575), `panel.rs` (1986), `api.rs` (1674), `compose.rs` (1384), `flux-cli/sync.rs` (1287), `observe_page.rs` (1284), `flux-cli/api.rs` (1225), `observe.rs` (1106), `state.rs` (1038). Cada foco produziu mapa linha a linha, grafos de chamada e proposta estrutural (ou a decisão fundamentada de NÃO dividir).
3. **41 propostas** de mudança (`id` curto cada — ex. `nucleo-busy-morto`), com movimento, testes, risco e critério de aceite.
4. **Revisor independente** verificou as 41 propostas contra o código: **36 aprovadas, 5 "ajustar"** (com correção de fato ou de interface), nenhuma rejeitada por inteiro. Os ajustes do revisor são VINCULANTES e já estão incorporados no plano.
5. **Mapa de dependências** do workspace e interno do `fluxd`, com acoplamentos problemáticos e direção-alvo ([dependency-map.md](dependency-map.md)).

### Status dos fatos neste relatório

- **Toda a auditoria foi somente leitura** (leitura integral de arquivos, `grep`, `wc -l`, `git log`, diff de blocos). **Nenhum `cargo build/test` foi executado** — proibido no lote de análise; VM e flux intocados. Toda menção a "suíte verde" é proteção A RODAR quando o passo for executado, não verificação feita nesta sessão.
- Cada afirmação forte tem evidência `path:linha`. Onde há divergência entre auditores, ela foi resolvida com evidência e está registrada em [architecture-findings.md](architecture-findings.md) e na seção de propostas ajustadas do [execution-plan.md](execution-plan.md).
- **Nenhum achado P0.** Unanimidade dos sete auditores: todo módulo crítico tem rede de caracterização utilizável hoje; a refatoração pode começar. O buraco real é de EXECUÇÃO no CI (testes gated passam vazios), não de instrumentação — por isso o Lote 0 do plano é de verificação.
- Contagens de linha divergem ±1 entre medidores (convenção de newline final: este relatório mediu `wc -l`, o inventário consolidado usou leitura direta). Usa-se a coluna do [inventory.md](inventory.md) como referência.

## Navegação

| Documento | Conteúdo |
|---|---|
| [inventory.md](inventory.md) | Os 53 itens in-scope: caminho, linhas, responsabilidade, status de auditoria, decisão. |
| [file-audit.md](file-audit.md) | Achados por arquivo — avaliação curta dos sãos, detalhada dos problemáticos, com propostas vinculadas. |
| [architecture-findings.md](architecture-findings.md) | Problemas estruturais transversais (AF-01…AF-23): evidência, consequência, correção recomendada. |
| [dependency-map.md](dependency-map.md) | Grafo de dependências entre crates e camadas internas do `fluxd`; acoplamentos problemáticos; direção-alvo. |
| [execution-plan.md](execution-plan.md) | **O PLANO CANÔNICO — v2 (19/10/2026).** Visão arquitetural (capacidades × orquestradores × interfaces), diagnóstico com as emendas da revisão, estrutura-alvo, mapeamento por arquivo, fluxos de execução, passos L0–L7 (emendas v2: L3.4, L5.5, L5.6; L3.1/L5.2/L6.3/L7.2 ampliados), justificativas demonstradas dos arquivos grandes e validação de reuso. |
| [execution-plan-v1.md](execution-plan-v1.md) | Plano v1 (18/10) PRESERVADO: texto integral dos passos L0–L7 que o v2 referencia — os passos não emendados no v2 valem pelo v1. |
| [coverage-manifest.md](coverage-manifest.md) | Cobertura da auditoria: caminho × linhas × status × revisor × referência ao achado. 100% com status explícito. |

## Resumo executivo (30 segundos)

- **P0: nenhum.** **P1: um** — os gates de Docker/Mutagen do CI rodam vazios (`ci.yml` não define `FLUX_DOCKER_URL` nem `MUTAGEN_BIN`), então wake/sweep/schema-exec e a sync real têm zero execução automatizada. É o Lote 0.
- O código está melhor do que o tamanho dos arquivos sugere: `decision.rs` e `state.rs` têm fronteiras exemplares; os arquivos >1000 linhas são em geral grandes-mas-coesos. Os splits reais justificados são: `flux-cli/main.rs` (9 domínios de comando), `panel.rs` (4 camadas com seams existentes), `compose.rs` (blue-green deslocado) e `flux-cli/sync.rs` (estabilidade × ponte mutagen).
- O restante do ganho está em **duplicação entre módulos e crates**: 4–6 cópias de listagem de containers por label, 5–6 cópias do validador de nome, CSS/escape duplicados em duas famílias visuais, helpers de token/sha256 duplicados, chaves e tipos mortos (`[db].image_guard`, `Causa`, `SleepDecision::Busy`, dep `sha2`).
- Regra de ouro do plano: **mover código sem teste de caracterização é proibido; dividir só para baixar contagem de linhas é proibido; quebrar API pública dos crates é proibido.** Detalhes em "O que NUNCA fazer" no [execution-plan.md](execution-plan.md).
