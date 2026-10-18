# Manifesto de cobertura da auditoria

Status definidos:
- **Completo** — auditoria auto-declarada como leitura integral do arquivo (uma ou mais lentes), com mapa/citações de linha; se houve auditoria "foco" dedicada, conta como dupla cobertura.
- **Leve** — auditoria auto-declarada de profundidade menor ou lente única, mesmo com citações de linha; registra-se o que faltou e como obter.
- **Não auditado** — fora da varredura.

**Resultado: 53/53 itens in-scope com status explícito — 51 completos, 2 leves, 0 não auditados.**

Status honesto dos dois "leves": `edge.rs` e `waf.rs` foram lidos com citações de linha pela lente de rede (o auditor-rede auto-declarou "auditoria leve" quanto à profundidade das propostas), são ~910 linhas (abaixo do gatilho de foco >1000) e não receberam segunda lente dedicada. Consequência prática: suas propostas são só as de croma compartilhado (L4.2); se um passo futuro tocar esses arquivos além do CSS, re-avaliar com leitura dedicada. `state.rs` teve UMA passagem leve (auditor-nucleo) COMPENSADA por duas integrais (auditor-dados + foco-state) — net: completo.

Verificações re-executadas nesta sessão de edição (todas somente leitura, complementam a auditoria): leitura integral de `.github/workflows/ci.yml`; `git grep` de Busy/PROJECTS_ROOT_PADRAO/image_guard/sha2/mod sync;/branch_exists; `wc -l` de main.rs (2574), panel.rs (1985), lib.rs (197), compose.rs (1383) — consistentes com o inventário (±1, newline).

## crates/flux-cli

| Caminho | Linhas | Status | Revisor(es) | Achado |
|---|---:|---|---|---|
| `crates/flux-cli/src/main.rs` | 2575 | Completo (duplo) | auditor-cli (integral); foco-cli-main (integral); revisor | [FA-01](file-audit.md) |
| `crates/flux-cli/src/sync.rs` | 1287 | Completo (duplo) | auditor-cli; foco-cli-sync (integral, wc -l e git log medidos); revisor | [FA-02](file-audit.md) |
| `crates/flux-cli/src/api.rs` | 1225 | Completo (duplo) | auditor-cli; foco-cli-api (integral); revisor | [FA-03](file-audit.md) |
| `crates/flux-cli/tests/sync_tests.rs` | 670 | Completo | auditor-testes (+main.rs:13 conferido nesta sessão); revisor | [FA-04](file-audit.md) |
| `crates/flux-cli/src/tunel.rs` | 240 | Completo | auditor-cli; revisor | [FA-05](file-audit.md) |
| `crates/flux-cli/src/wizard.rs` | 190 | Completo | auditor-cli; revisor | [FA-06](file-audit.md) |

## crates/fluxd — src

| Caminho | Linhas | Status | Revisor(es) | Achado |
|---|---:|---|---|---|
| `crates/fluxd/src/panel.rs` | 1986 | Completo (duplo) | auditor-painel-observe (integral, 2 passadas); foco-painel (integral + git log); revisor | [FA-07](file-audit.md) |
| `crates/fluxd/src/api.rs` | 1674 | Completo (duplo) | auditor-painel-observe; foco-api-fluxd (integral + wc/grep); revisor | [FA-08](file-audit.md) |
| `crates/fluxd/src/compose.rs` | 1384 | Completo (duplo) | auditor-dados; foco-compose (integral, 4 suites de teste lidas); revisor | [FA-09](file-audit.md) |
| `crates/fluxd/src/observe_page.rs` | 1284 | Completo (duplo) | auditor-painel-observe; foco-observe-page (integral + diff dos blocos CSS executado); revisor | [FA-10](file-audit.md) |
| `crates/fluxd/src/observe.rs` | 1106 | Completo (duplo) | auditor-painel-observe; foco-observe (integral, mapa símbolo a símbolo); revisor | [FA-11](file-audit.md) |
| `crates/fluxd/src/state.rs` | 1038 | Completo (triplo; 1ª passagem leve) | auditor-nucleo (leve); auditor-dados (lente persistência); foco-state (integral); revisor | [FA-12](file-audit.md) |
| `crates/fluxd/src/ciclo.rs` | 997 | Completo | auditor-nucleo; revisor (comparou corpos dos helpers) | [FA-13](file-audit.md) |
| `crates/fluxd/src/deploy.rs` | 918 | Completo | auditor-painel-observe; revisor | [FA-14](file-audit.md) |
| `crates/fluxd/src/edge.rs` | 910 | **Leve** | auditor-rede (auto-declarada leve; leitura com citações; lente única) | [FA-15](file-audit.md) |
| `crates/fluxd/src/waf.rs` | 905 | **Leve** | auditor-rede (auto-declarada leve; nota de segurança registrada p/ futuro toque) | [FA-16](file-audit.md) |
| `crates/fluxd/src/decision.rs` | 761 | Completo | auditor-nucleo (should_sleep lido integralmente); revisor | [FA-17](file-audit.md) |
| `crates/fluxd/src/db.rs` | 555 | Completo | auditor-dados; revisor | [FA-18](file-audit.md) |
| `crates/fluxd/src/proxy.rs` | 537 | Completo | auditor-rede; revisor | [FA-19](file-audit.md) |
| `crates/fluxd/src/router.rs` | 528 | Completo | auditor-rede (trechos 40-120 e 280-380 citados); revisor (divergência do enum Source resolvida) | [FA-20](file-audit.md) |
| `crates/fluxd/src/docker.rs` | 395 | Completo | auditor-dados; revisor (achou cópia extra host_custom) | [FA-21](file-audit.md) |
| `crates/fluxd/src/temporary.rs` | 391 | Completo | auditor-nucleo (trechos 160-210); revisor (rejeitou fase 2 com evidência) | [FA-22](file-audit.md) |
| `crates/fluxd/src/sleep.rs` | 353 | Completo | auditor-nucleo (42-71 integral); revisor; `git grep Busy` nesta sessão | [FA-23](file-audit.md) |
| `crates/fluxd/src/pages.rs` | 338 | Completo | auditor-rede; revisor (rebaixou P1→P2 com evidência de caminho morto) | [FA-24](file-audit.md) |
| `crates/fluxd/src/panel_auth.rs` | 306 | Completo | auditor-painel-observe; revisor (grep `.hash_path` workspace) | [FA-25](file-audit.md) |
| `crates/fluxd/src/sync.rs` | 306 | Completo | auditor-painel-observe | [FA-26](file-audit.md) |
| `crates/fluxd/src/guard.rs` | 262 | Completo | auditor-rede (138-261 citados) | [FA-27](file-audit.md) |
| `crates/fluxd/src/lib.rs` | 198 | Completo | auditor-nucleo; revisor (arquivo inteiro lido, 197 l.); `wc -l` nesta sessão | [FA-28](file-audit.md) |
| `crates/fluxd/src/beat.rs` | 178 | Completo | auditor-nucleo | [FA-29](file-audit.md) |
| `crates/fluxd/src/sqlite_source.rs` | 113 | Completo | auditor-dados | [FA-30](file-audit.md) |
| `crates/fluxd/src/main.rs` | 88 | Completo | auditor-nucleo | [FA-31](file-audit.md) |
| `crates/fluxd/src/events.rs` | 23 | Completo | auditor-nucleo | [FA-32](file-audit.md) |

## crates/fluxd — tests

| Caminho | Linhas | Status | Revisor | Achado |
|---|---:|---|---|---|
| `crates/fluxd/tests/integration.rs` | 227 | Completo | auditor-testes (+leitura de router/decision/edge/waf que exercem) | [FA-33](file-audit.md) |
| `crates/fluxd/tests/deploy_integration.rs` | 313 | Completo | auditor-testes (verificou duplicação verbatim com deploy.rs:831-900) | [FA-34](file-audit.md) |
| `crates/fluxd/tests/state_migration_integration.rs` | 179 | Completo | auditor-testes (comparou fixture com SCHEMA_V1 real verbatim) | [FA-35](file-audit.md) |
| `crates/fluxd/tests/schema_integration.rs` | 115 | Completo | auditor-testes | [FA-36](file-audit.md) |
| `crates/fluxd/tests/sleep_integration.rs` | 83 | Completo | auditor-testes | [FA-37](file-audit.md) |
| `crates/fluxd/tests/docker_integration.rs` | 78 | Completo | auditor-testes | [FA-38](file-audit.md) |

## crates/flux-common

| Caminho | Linhas | Status | Revisor(es) | Achado |
|---|---:|---|---|---|
| `crates/flux-common/src/config.rs` | 965 | Completo | auditor-common (integral; 15 testes lidos); revisor (corrigiu fato do guard global); `git grep image_guard` nesta sessão | [FA-39](file-audit.md) |
| `crates/flux-common/src/host.rs` | 313 | Completo | auditor-common (12 testes) | [FA-40](file-audit.md) |
| `crates/flux-common/src/protocol.rs` | 152 | Completo | auditor-common (Serialize-only verificado) | [FA-41](file-audit.md) |
| `crates/flux-common/src/framework.rs` | 72 | Completo | auditor-common | [FA-42](file-audit.md) |
| `crates/flux-common/src/error.rs` | 68 | Completo | auditor-common (grep "Causa" workspace) | [FA-43](file-audit.md) |
| `crates/flux-common/src/lib.rs` | 10 | Completo | auditor-common; auditor-testes; `git grep sha2` nesta sessão | [FA-44](file-audit.md) |

## Manifestos, CI e scripts (não-.rs)

| Caminho | Linhas | Status | Revisor(es) | Achado |
|---|---:|---|---|---|
| `.github/workflows/ci.yml` | 28 | Completo | auditor-testes (greps de env) + **leitura integral nesta sessão** (comandos :21/:24/:27 confirmados verbatim) | [FA-45](file-audit.md) |
| `Cargo.toml` (raiz) | — | Completo | auditor-testes (sem [alias]/[profile]) | [FA-46](file-audit.md) |
| `crates/fluxd/Cargo.toml` | — | Completo | auditor-testes (redundâncias mapeadas linha a linha) | [FA-47](file-audit.md) |
| `crates/flux-cli/Cargo.toml` | — | Completo | auditor-testes ([[bin]] sem [lib]; axum dev-dep validada como usada) | [FA-48](file-audit.md) |
| `crates/flux-common/Cargo.toml` | — | Completo | auditor-testes + auditor-common; `git grep sha2` nesta sessão | [FA-49](file-audit.md) |
| `flux.toml.example` | — | Completo | auditor-common (amarração com struct por teste mapeada) | [FA-50](file-audit.md) |
| `scripts/host/README.md` | — | Completo | auditor-testes (doc WSL histórica mapeada :11-12, 29, 33, 80-81) | [FA-51](file-audit.md) |
| `scripts/host/install-cloudflared.ps1` | — | Completo | auditor-testes (token como arg :52 registrado) | [FA-51](file-audit.md) |
| `scripts/host/install-fluxd.sh` | — | Completo | auditor-testes (solidão verificada: set -euo :5, idempotência :17, contrato de binds :36) | [FA-51](file-audit.md) |

## O que ficou sem cobertura e por quê

**Nada ficou sem cobertura** — 100% dos itens in-scope foram auditados. Limitações parciais registradas:

1. **`edge.rs` e `waf.rs` (leve):** lente única (rede), auto-declarada de profundidade menor em propostas; nenhuma auditoria focada (arquivos <1000 l.). Como obter: leitura dedicada caso algum passo futuro vá além do croma compartilhado (L4.2); a nota de segurança do waf (AF-20) reavaliar sempre que a topologia de rede mudar.
2. **Nenhum `cargo build/test` executado** por qualquer auditor (proibido no lote de análise; VM/flux intocados) — todas as suítes citadas como rede de proteção são proteção A RODAR; baseline verde é confirmado na execução de cada passo do plano ([execution-plan.md](execution-plan.md), regra 2).
3. **Nenhuma medição de cobertura percentual** (tarpaulin/llvm-cov: dado não encontrado no repo) — o gap real mapeado é de EXECUÇÃO no CI (AF-01), não de instrumentação; nenhuma ferramenta de cobertura foi proposta.
4. **Dados não encontrados durante a auditoria** (com como obter): chamadores de `purge_project` em temporary.rs não lidos no lote de state (AF-22 — intencionalidade do purge parcial); cobertura de docker.rs no ramo awake/waking (fora do escopo do foco-api — resolvido pela caracterização obrigatória do L5.4); template user-facing do config global (não existe — criar é decisão do dono); fonte do mutagen upstream citada no cabeçalho de sync.rs:19-40 (tratada como documento do repo, não fato conferido).
