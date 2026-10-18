# Plano de execução da refatoração modular — CANÔNICO

Este documento é a fonte única de verdade da execução. Prioridades FINAIS já incorporam os vereditos do revisor independente (onde divergiram do auditor proponente, marca-se "intenção → resultado"). Nenhum passo altera comportamento de produto, exceto onde o passo DECLARA explicitamente a mudança.

## Regras de execução (valem para todo passo)

1. **Um passo = um commit** (ou um commit por sub-unidade quando dito). Commit local com pathspec dos próprios arquivos; **push só por pedido do Gabriel**; nunca `--amend` em HEAD compartilhado.
2. **Baseline antes do movimento:** rodar o trio de verificação (abaixo) ANTES de editar e confirmar verde. Suíte não executada = movimento não começa.
3. **Movimento verbatim:** mover código sem "melhorar". Reordenar passos com efeito colateral é proibido (ver NUNCA fazer).
4. **Ambiente compartilhado:** re-ler o arquivo antes de editar por cima; o `flux` gerencia dev server e a VM — **nenhum passo toca VM, ssh, flux ou relinka binário em uso**; `cargo` roda no host de dev, e gates de CI verificam no push (quando o Gabriel pedir).
5. **`docs/` acompanha:** cada passo que muda mecanismo atualiza a doc do mecanismo (docs/ existentes) na mesma entrega.
6. **Escopo fiel:** achado incidental vira menção de 1 linha no relatório do passo, nunca fix extra.

## Comandos de verificação

O `ci.yml` (lido e confirmado nesta sessão; linhas: 21, 24, 27) executa exatamente três comandos — **o "trio"**:

```
cargo fmt --all -- --check                                  # ci.yml:21
cargo clippy --workspace --all-targets -- -D warnings       # ci.yml:24
cargo test --workspace                                      # ci.yml:27
```

Todo passo valida com o trio (local, antes de commitar). Comandos EXTRA são listados por passo. Convenções:

- **G** = `FLUX_DOCKER_URL=tcp://127.0.0.1:2375 cargo test --workspace` — requer engine real (flux/local; no CI após L0.2). **Requer verificação local** (não executado nesta análise).
- **M** = `MUTAGEN_BIN=<caminho> cargo test --workspace` — requer binário mutagen. **Requer verificação local.**
- Qualquer comando fora do trio/G/M (ex.: `cargo tree -p flux-common`) está marcado **requer verificação** (derivado da proposta, não confirmado por execução nesta sessão).
- Nenhum comando de build/teste foi executado no lote de análise (proibido); "verde" nos testes de regressão abaixo = proteção A RODAR, com baseline confirmado na hora da execução.

## Prioridades

- **P0: nenhum** — unanimidade dos auditores; não existe comportamento crítico sem caracterização utilizável hoje.
- **P1: 1 passo** (L0.2 — gate de CI do engine). Intenção dos auditores tinha mais P1s (pages-escape, docker-reuso, sweep); o revisor rebaixou com evidência (caminhos mortos hoje) — registradas as rebaixas.
- **P2: núcleo do plano** (contratos compartilhados, dedups com risco de drift, splits estruturais aprovados).
- **P3: higiene** (remoções mortas, docs, dedups locais).
- **defer: 2 decisões** com zero código (observe.rs, state.rs permanecem).

## Ordem dos lotes (dependência e risco)

```
L0 verificação ──► L1 higiene ──► L2 contratos ──► L3 docker/infra ──► L6 compose/panel
                        │              │                  │
                        │              │                  └── L6.1 depende de L3.1
                        │              └── L7.4 depende de L2.5
                        └── L4 páginas/visual ──► L5 núcleo/API ──► L6.3 panel-árvore (após L4)
                                 L7 cli estrutural (L7.1→L7.2→L7.3; L7.4 após L2.5; L7.5 livre)
```

---

## Lote 0 — Verificação (não toca código de produção)

### L0.1 · `testes-ci-toolchain-pin` — P3 · S (horas) · risco baixo

- **Arquivos-alvo:** `rust-toolchain.toml` (NOVO, raiz); nenhum código.
- **Problema (evidência):** dtolnay/rust-toolchain@stable (ci.yml:14-16) é canal móvel; edition 2024 (Cargo.toml:7) exige Rust ≥ 1.85; não existe rust-toolchain.toml no repo — bump de stable pode quebrar fmt/clippy/test sem mudança de código.
- **Módulos origem→destino:** nenhum símbolo; novo arquivo de config de toolchain com canal pinado (ex.: 1.85.0 ou stable corrente na data).
- **Imports/visibilidade/API:** nenhuma no código Rust; o action respeita o arquivo quando existir.
- **Pré-requisitos:** nenhum.
- **Testes de regressão:** os 3 comandos do ci.yml (a própria suíte).
- **Validação:** trio + **requer verificação**: run do CI verde na toolchain pinada (push é decisão do Gabriel).
- **Resultado esperado:** bumps de Rust passam a ser decisão explícita em PR que sobe o pin.
- **Aceite:** CI verde no pin; quebra futura de Rust só entra por PR que muda o pin.

### L0.2 · `testes-ci-gates-docker` — P1 · M (1-2 dias) · risco médio — ÚNICO P1 do plano

- **Arquivos-alvo:** `.github/workflows/ci.yml`; beneficiários: `crates/fluxd/tests/{docker,sleep,schema}_integration.rs` (intocados).
- **Problema (evidência):** AF-01 — ci.yml:27 roda `cargo test --workspace` sem FLUX_DOCKER_URL (grep em .github/: vazio); os 3 testes retornam cedo (docker_integration.rs:14-17, sleep_integration.rs:14-21, schema_integration.rs:12-19); docker_integration.rs:5 prevê a injeção e nada injeta. Wake/sweep/schema-exec: zero execução automatizada.
- **Módulos origem→destino:** nenhum símbolo; ci.yml ganha job `test-engine`: ubuntu-latest + service `docker:dind` expondo :2375 + step com env FLUX_DOCKER_URL=tcp://127.0.0.1:2375. **Ajuste do revisor (vinculante):** rodar `cargo test --workspace` COMPLETO no job com engine (não filtros `--test`) — pega de graça os gated embutidos (ciclo.rs:812-815 exec/logs/SSE/teto; panel.rs:1966-1972 fonte docker verdade).
- **Imports/visibilidade/API:** nenhuma; DockerSource::connect já fala HTTP com a env (docker.rs:152-165). Gates locais (pulado explícito sem engine) permanecem para dev no Windows.
- **Pré-requisitos:** nenhum.
- **Testes de regressão:** os próprios 3 arquivos passam a rodar de verdade; proteção = log do CI sem "pulado: FLUX_DOCKER_URL não definido".
- **Validação:** **requer verificação**: run do CI com job test-engine verde, 3 testes reportando passed; margem nos timings 300ms×N se runner flanquear.
- **Resultado esperado:** regressões em DockerSource/SleepManager/ensure_schema aparecem no CI, não na VM.
- **Aceite:** job verde com os testes passed (não "pulado").

### L0.3 · `testes-ci-gates-mutagen` — P2 · M (~1 dia) · risco médio

- **Arquivos-alvo:** `.github/workflows/ci.yml`; beneficiário: `crates/flux-cli/tests/sync_tests.rs` (intocado).
- **Problema (evidência):** gate_sync_real exige MUTAGEN_BIN (sync_tests.rs:32-41); ci.yml não define — os 3 gated de sync real (482-669: exclusões globais, pause/resume, reset) passam vazios; regressão de exclusão de segredo (.env) só apareceria com mutagen na mão.
- **Módulos origem→destino:** nenhum símbolo; step que baixa release PINADA do mutagen (mutagen-io/mutagen, open-source, sem tarifa) com checksum e exporta MUTAGEN_BIN; sync local↔local com tempdirs é neutra de plataforma.
- **Imports/visibilidade/API:** nenhuma.
- **Pré-requisitos:** nenhum (pode dividir runner com L0.2 no mesmo PR).
- **Testes de regressão:** os 3 gated verdes no CI.
- **Validação:** **requer verificação**: log do CI sem "pulado: MUTAGEN_BIN não definido".
- **Aceite:** 3 testes passed no CI.

### L0.4 · `testes-noindex-header-toda-resposta-dev` — P2 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/tests/integration.rs`.
- **Problema (evidência):** R22 ("TODA resposta de dev leva noindex", router.rs:315-318) só assertada no proxy (integration.rs:100-104); página de espera (131-146), 503 JSON (148-165) e robots.txt (167-178) não assertam — regressão que condicione o insert passa verde.
- **Movimentos:** nenhum símbolo; +1 assert por teste existente: `header(&headers, "x-robots-tag") == Some("noindex, nofollow, noarchive")` em dev_dormindo_recebe_pagina_de_espera_200 (:146), api_dormindo_recebe_503_json_com_retry_after (:164) e robots_em_dev_e_gerado_disallow_r22 (:177, opcional).
- **Pré-requisitos:** nenhum.
- **Testes de regressão:** os próprios asserts.
- **Validação:** trio + prova de mutação (uma vez, local): remover o header em router.rs:315-318, rodar `cargo test -p fluxd --test integration`, conferir FALHA, restaurar. **Requer verificação** (não executada nesta análise).
- **Aceite:** suíte falha ao remover o header e volta ao verde com ele.

---

## Lote 1 — Higiene e remoções seguras (um commit cada; nenhum depende de outro)

### L1.1 · `nucleo-busy-morto` — P3 · S (horas) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/sleep.rs`.
- **Problema (evidência):** `SleepDecision::Busy` (sleep.rs:37) nunca construída — should_sleep (42-71) só produz KeepAwake/Held/Sleep; braço inalcançável em :255 (confirmado nesta sessão: 2 únicas ocorrências). A guarda real de CPU roda fora da decisão pura, em sweep_one (226-231). O enum mente sobre o desenho.
- **Movimento:** SAI variante `Busy` (37) e braço do match (255). FICA todo o resto, inclusive a guarda real em sweep_one (comportamento idêntico); doc do enum lista só as 3 variantes produzíveis.
- **Visibilidade/API:** SleepDecision é pub — remoção de variante é breaking para consumidor externo, MAS fluxd lib não tem consumidor fora do crate (workspace = 3 crates; grep `fluxd::` em flux-cli/flux-common vazio). Segura.
- **Pré-requisitos:** nenhum.
- **Testes de regressão:** should_sleep (280-331) e limits_for (334-351) sem edição; suíte completa do fluxd.
- **Validação:** trio + `grep Busy crates/fluxd/src/sleep.rs` vazio.
- **Resultado:** nenhum evento "CPU alta — acordado" existia (inalcançável) — zero mudança observável.
- **Aceite:** trio verde; grep vazio.

### L1.2 · `dados-renome-db-migrations` — P3 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/db.rs` → `crates/fluxd/src/migrations.rs`; `crates/fluxd/src/lib.rs` (declaração); `crates/fluxd/src/api.rs:32` (`use crate::db::{self, SchemaOutcome}`).
- **Problema (evidência):** db.rs (1-13) é pipeline de migrations/seed DENTRO de containers; quem fala com o SQLite de estado é state.rs — num crate com state.rs, sqlite_source.rs e observe.rs (schema próprio), "db" sugere o módulo errado.
- **Movimento:** renomeação de arquivo + `pub mod db` → `pub mod migrations` (lib.rs:8) + 1 `use`. Nada sai nem entra.
- **Visibilidade/API:** muda caminho público do crate, MAS nenhum crate consome `fluxd::` (grep em flux-cli/flux-common vazio; flux-cli fala HTTP).
- **Pré-requisitos:** nenhum.
- **Testes de regressão:** cargo check/test existente (renomeação mecânica).
- **Validação:** trio + `grep -rn "crate::db\|fluxd::db" crates/` vazio.
- **Aceite:** trio verde; grep vazio.

### L1.3 · `common-sha2-dep-morta` — P3 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/flux-common/Cargo.toml`.
- **Problema (evidência):** sha2 declarado (Cargo.toml:11) sem NENHUM uso em src/ (grep sha2|Sha256|Digest vazio — confirmado nesta sessão). Dep morta em crate base de 2 dependentes.
- **Movimento:** sai a linha; nenhum código muda.
- **Pré-requisitos:** nenhum. **Testes:** cargo check/test do workspace.
- **Validação:** trio + **requer verificação**: `cargo tree -p flux-common` sem sha2 na árvore (comando fora do ci.yml).
- **Nota:** sha2 É usado por fluxd/api.rs:28 e flux-cli — os workspace deps ficam.
- **Aceite:** workspace compila; árvore sem sha2.

### L1.4 · `painel-auth-hash-path-privado` — P3 · S (minutos + suíte) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/panel_auth.rs`.
- **Problema (evidência):** `hash_path` pub (panel_auth.rs:126) sem leitor externo — grep `.hash_path` no workspace só acha o uso interno em `verificar` (:147); consumidores usam novo()/with_hash_path. Superfície que permite ler/trocar o caminho do segredo por fora do fluxo fail-closed.
- **Movimento:** `pub hash_path` → `hash_path` (:126); `sessions`/`limiter` seguem pub (usados por panel.rs:252,270,295).
- **Pré-requisitos:** nenhum.
- **Testes:** suíte do módulo (212-305, 7 testes) + harness painel_com_auth (panel.rs:1269).
- **Validação:** trio + `grep -rn "\.hash_path" crates/` devolve só panel_auth.rs:147 antes e depois.
- **Aceite:** trio verde; nenhum leitor externo quebrado.

### L1.5 · `common-causa-orfao` — P3 · S (meio dia) · risco baixo — **decisão do dono**

- **Arquivos-alvo:** `crates/flux-common/src/error.rs`.
- **Problema (evidência):** `Causa` (5-14) é tipo público sem NENHUM consumidor — grep "Causa" no workspace só acha a definição; órfão da spec 17. FluxError fica (consumido por state.rs:124 e config.rs).
- **Movimento:** sair com a enum; OU mantê-la com comentário de marco da spec 17 — **decisão do dono** (default da proposta: remover; git preserva).
- **Pré-requisitos:** nenhum. **Testes:** teste atual de error.rs (mensagens_de_erro_carregam_origem_e_causa) verde — não toca Causa.
- **Validação:** trio.
- **Aceite:** grep Causa vazio (ou mantida com nota explícita por decisão do dono); 3 crates compilam.

### L1.6 · `foco-sync-doc-veraz` — P3 · S (<1h) · risco baixo

- **Arquivos-alvo:** `crates/flux-cli/src/sync.rs` (só doc de `parse_created_id`, 429-430).
- **Problema (evidência):** doc alega "o CLI também usa para exibir o id em modo verboso" — grep: zero uso em main.rs/api.rs/tunel.rs/wizard.rs/fluxd. Consumidores reais: sync.rs:469 (interno) e tests/sync_tests.rs:418-429. Crate só-binário: pub não muda linkage; a doc é a única visibilidade "real".
- **Movimento:** nenhum símbolo; doc reescrita para o fato verificável ("público para os testes de integração via #[path]; consumo interno em `create`"). Se L7.4 landar primeiro, viaja junto para sync/mod.rs.
- **Validação:** trio; `grep -n "verboso" crates/flux-cli/src/sync.rs` sem a alegação falsa.
- **Aceite:** doc veraz; compilação idêntica.

### L1.7 · `testes-sync-bridge-doc` — P3 · S (horas) · risco baixo

- **Arquivos-alvo:** `crates/flux-cli/tests/sync_tests.rs` (cabeçalho).
- **Problema (evidência):** doc (2-5) diz que a ponte `#[path]` é transitória ("quando o integrador declarar mod sync...") — FATO: main.rs:13 JÁ declara (confirmado nesta sessão) e o teste ainda compila a cópia; flux-cli é só-binário (Cargo.toml:7-9) e teste de integração não importa módulo de bin. Ponte PERMANENTE, com constraint real: src/sync.rs não pode referenciar `crate::` do main.rs (quebra seria pega pelo clippy --all-targets do ci.yml:24 — não silenciosa, mas a regra não está documentada).
- **Movimento:** só comentário — doc reescrita com mecanismo real + constraint + quem pega a quebra. A alternativa estrutural (criar [lib]) NÃO está no escopo.
- **Validação:** trio (doc não muda comportamento).
- **Aceite:** doc descreve a constraint verdadeira; nenhum teste muda de resultado.

### L1.8 · `nucleo-raiz-projetos` (REDUZIDO pelo revisor) — P3 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/temporary.rs` (+ `crates/fluxd/src/paths.rs` NOVO, opcional; 4 imports).
- **Problema (evidência):** PROJECTS_ROOT_PADRAO (temporary.rs:25) consumida por ciclo.rs:172, api.rs:878, compose.rs:260, deploy.rs:361 (confirmado por git grep nesta sessão); doc de temporary.rs:23-24 promete "o serve passa o default" mas serve usa a const direto (lib.rs:155; temporary.rs:40) — doc drift. Segunda fonte: compose.rs:601-608 / router.rs:117-122 derivam a raiz de state_dir.
- **Resolução da divergência (vinculante):** fase 2 da proposta (helper de derivação unificado) **DESCARTADA** — a derivação é gate DELIBERADO de "estou na VM" (router.rs:114-119); unificar muda semântica; divergência com --db custom é latente e sem impacto.
- **Movimento:** (1) corrigir a doc de temporary.rs:23-24; (2) opcional: mover a const para paths.rs (`crate::temporary::PROJECTS_ROOT_PADRAO` → `crate::paths::PROJECTS_ROOT_PADRAO`; 4 imports mudam) e documentar a divergência com --db custom na doc. temporary.rs fica dono de toda a lógica de expiração/teardown.
- **Testes de regressão (existem, sem edição):** temporary.rs:264-382 (expire_check_em com tempdir) e ciclo.rs:686-704 (down_sem_pasta_vira_409 — passa porque /srv/flux/projects não existe no host de teste).
- **Validação:** trio + `grep -rn "temporary::PROJECTS_ROOT_PADRAO" crates/` vazio (se movida).
- **Aceite:** doc correta; suíte verde; zero uso residual do caminho antigo (se movida).

---

## Lote 2 — Contratos compartilhados (flux-common + borda CLI)

### L2.1 · `nucleo-nome-projeto` — P2 · S (meio dia, incluindo api.rs; flux-cli no L2.6) · risco baixo

- **Arquivos-alvo:** `crates/flux-common/src/nomes.rs` (NOVO) + `crates/flux-common/src/lib.rs` (pub mod nomes;) · `crates/fluxd/src/decision.rs` · `crates/fluxd/src/temporary.rs` · `crates/fluxd/src/api.rs`.
- **Problema (evidência):** predicado `[a-z0-9-]` em 5 lugares (decision.rs:197-204, temporary.rs:30-35, api.rs:201-205, main.rs:624-629, wizard.rs:38-48 — confirmado pelo revisor) + variante host com '.' (config.rs:479-485, OUTRO contrato, fica). Relaxar/endurecer exige achar 6 pontos; esquecer um cria projeto registrável que não roteia.
- **Movimento:** ENTRA `flux_common::nomes::projeto_valido(name: &str) -> bool` (adição, sem quebra). SAI: inline de decision.rs:197-204; corpo de nome_seguro (temporary.rs:30-35 vira chamada direta); api.rs:204 passa a chamar. **A variante de host com '.' FICA distinta.**
- **Visibilidade/API:** nova função pub no flux-common; nenhuma assinatura do fluxd muda.
- **Pré-requisitos:** nenhum. **Coordenação:** adoção no flux-cli (main.rs:628, wizard.rs:40) junto do L2.6.
- **Testes:** unitários novos em nomes: '', 'Vipers', 'a.b', '../escapou', 'vipers', 'vipers-ok-1', 'viç', 'a b'. Caracterização que DEVE permanecer verde SEM edição: decision.rs:651-691 e temporary.rs:356-382.
- **Validação:** trio.
- **Aceite:** greps confirmam validador único no fluxd; suítes de flux-common e fluxd verdes.

### L2.2 · `common-db-image-guard-chave-morta` — P2 · S (meio dia) · risco MÉDIO (quebra de parse deliberada)

- **Arquivos-alvo:** `crates/flux-common/src/config.rs` · `flux.toml.example`.
- **Problema (evidência):** DbSection.image_guard (config.rs:112) + bloco do template (41-43, "Default: true") sem NENHUM leitor (grep zero). **Correção de fato do revisor (maior que a proposta):** o guard real NEM lê a global — compose.rs:282 usa `ImageGuardSection::default().block_list()`; única leitura da global é teste (config.rs:949). Ambas são "aceitas e ignoradas" — viola o princípio do próprio arquivo (config.rs:3-4).
- **Movimento:** SAI campo DbSection::image_guard e bloco do template. FICA ImageGuardSection/GlobalConfig/image_guard GLOBAL... **decisão do dono no mesmo escopo:** (a) ligar GlobalConfig.image_guard no compose.rs:282 (substituindo o default de fábrica) OU (b) removê-la em proposta própria. Default sugerido: (b) — ninguém nunca leu nenhuma das duas.
- **Visibilidade/API:** quebra de parse DELIBERADA — flux.toml existente com image_guard falha com erro citando a chave (deny_unknown_fields, barulhento por desenho). Nenhuma API de código muda.
- **Pré-requisitos:** nenhum. **Testes:** novo em config.rs: parse_str com "[db]\nimage_guard = true" deve FALHAR citando a chave; nenhum teste atual usa a chave (grep confirmado); suíte de 15 testes de config.rs verde.
- **Validação:** trio.
- **Aceite:** grep image_guard em flux-common só acha o que sobrou da decisão; parse da chave recusa com erro claro.

### L2.3 · `common-hosts-regras-concentradas` — P3 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/flux-common/src/{config.rs,host.rs}`.
- **Problema (evidência):** regra do prefixo `vite-`/dev_suffix partida: validação na escrita (config.rs:467-478) e interpretação na leitura (host.rs:85, 92-93) — mudar um lado sem o outro cria host que valida no registro mas não roteia na borda (ou vice-versa).
- **Movimento:** SAI de config.rs: corpo de ProjectToml::hosts_custom (439-494) e o teste hosts_custom_valida_zona_reservas_e_charset (689-727) → função livre em host.rs (ex.: `pub fn validar_hosts_custom(...) -> Result<(Option<String>, Option<String>), String>`). FICA: ProjectToml::hosts_custom como delegação de 1 linha (chamador api.rs:239 intocado). host.rs passa a importar config::{DomainsSection, DomainConfig} — sem ciclo (config não importa host).
- **Visibilidade/API:** assinatura pública de hosts_custom PRESERVADA.
- **Testes:** o teste 689-727 move inteiro; suítes de host.rs (12) e config.rs verdes.
- **Validação:** trio + `grep 'vite-' crates/flux-common/src` só em host.rs.
- **Aceite:** regras de host num único módulo; chamadores compilam sem mudança.

### L2.4 · `common-framework-fromstr-no-wizard` — P3 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/flux-cli/src/wizard.rs` (framework.rs intocado).
- **Problema (evidência):** wizard.rs:60-62 reimplementa o match de framework.rs:26-35 com mensagem própria — 4ª cópia da lista; doc de framework.rs:3 prevê exatamente esse drift.
- **Movimento:** SAI o match manual; FICA `parse::<Framework>()` como única validação, com a mensagem do FromStr (framework.rs:31-34).
- **Pré-requisitos:** nenhum. **Testes:** caso "python" rejeitado com a mensagem do FromStr; testes de detecção (wizard.rs:162-166) verdes.
- **Validação:** trio.
- **Aceite:** valor inválido rejeitado com a mensagem de framework.rs; lista existe em 1 cópia executável.

### L2.5 · `cli-vm-constantes-fonte-unica` — P3 · S (meio dia) · risco baixo — **antes de L7.4**

- **Arquivos-alvo:** `crates/flux-cli/src/{tunel.rs,sync.rs}`.
- **Problema (evidência):** VM_HOST="192.168.140.2"/VM_USER="flux" declarados 2× com valores idênticos (tunel.rs:18-19, sync.rs:62-63); `VmSection.vm_ip` (config.rs:501) é configurável sem leitor — trocar o host exige editar 2 arquivos que não leem config; drift silencioso nasce aqui (sync aponta VM, túnel do token aponta outra).
- **Movimento:** SAI de sync.rs as 2 consts pub; FICAM em tunel.rs (fonte já tratada por main.rs: 557, 571, 649, 920, 2015). vm_beta (sync.rs:173-183) usa as importadas; assinaturas intactas. Dependência sync→tunel nova, dentro do crate, sem ciclo (tunel não importa sync).
- **Coordenação:** L7.4 (spec.rs) herda o import — ajuste do revisor: spec.rs NÃO redeclara.
- **Testes:** teste de sync.rs que cobre vm_beta (endpoint_ssh_recombina_user_e_host_do_json_do_mutagen, sync.rs:1078-1107) verde sem edição.
- **Validação:** trio + `grep -rn "192.168.140.2" crates/flux-cli/src/` só em tunel.rs:18.
- **Aceite:** mesmas URLs/comandos ssh; segunda declaração do IP zerada.

### L2.6 · `cli-wizard-toml-robusto` — P3 · S (meio dia) · risco baixo — **absorve a adoção do validador no CLI**

- **Arquivos-alvo:** `crates/flux-cli/src/wizard.rs`.
- **Problema (evidência):** wizard.rs:91 interpola `health` cru (`path = "{health}"`) e grava sem verificação (120) — entrada com `"`/`\` grava flux.toml INVÁLIDO; o próximo `flux up` falha no parse (main.rs:700-703) longe da causa. Nome/framework/TTL são validados; health é o único string livre.
- **Movimento:** SAI de gerar(): o bloco de montagem (89-106) vira `fn montar_manifesto(nome, fw, health, temporario, ttl) -> Result<String, String>` pura e privada, com validação nova (recusa aspa/barra invertida, ou round-trip). FICAM: I/O stdin, guarda de arquivo existente (24-30), escrita (120), validações existentes. **Neste passo também:** wizard.rs:40 adota `flux_common::nomes::projeto_valido` (fecha o L2.1 no CLI).
- **Visibilidade/API:** `gerar(...)` e ManifestoGerado mantêm assinatura (consumidor único main.rs:335).
- **Testes:** caracterização da montagem pura: health com `"` e com `\` → Err claro e nada escrito; caso de ouro do TOML exato; TTL 0 → Err (regra atual de 99-102); testes existentes (158-188) verdes.
- **Validação:** trio.
- **Aceite:** nenhum caminho do wizard grava TOML que ProjectToml::parse_str recuse; health hostil coberto por teste.

---

## Lote 3 — Infra Docker única (fluxd)

**Ordem no lote:** L3.1 antes de L6.1 (a lista_do_projeto movida ao bluegreen já nasce consumindo o helper único).

### L3.1 · `dados-listagem-por-label` — P2 · S (meio dia) · risco baixo — **mudança de comportamento DECLARADA**

- **Arquivos-alvo:** `crates/fluxd/src/{docker.rs,compose.rs,temporary.rs}`.
- **Problema (evidência):** filtro `com.docker.compose.project` montado 4× (docker.rs:34-48, :54-62; compose.rs:1069-1073; temporary.rs:178-186) com erros divergentes: `unwrap_or_default()` (47 — engine falhou, wake/sleep agem como lista vazia), `.ok()?` (63 — não distingue "sem container" de "engine falhou").
- **Movimento:** FICA em docker.rs: `listar_projeto` (novo, único ponto com os filtros), containers_do_projeto/find_app_container refeitos sobre ele; compose_project/map_state/proxy_target_from/alvo_app intocados. SAEM: filtros duplicados, lista_do_projeto (compose.rs:1064-1083), bloco de temporary.rs:174-202 (mantém só o filtro de prefixo `flux-{nome}-` por cima da lista). **Ajuste do revisor:** o helper tem DUAS formas — match exato por projeto e variante "qualquer container com label" (compose_projects_do casa prefixo através de projetos — não forçar a forma exata ali).
- **Comportamento (declarado):** wake/sleep passam a PROPAGAR erro de listagem em vez de Ok silencioso — compatível: sleep.rs:232-252 já trata Err com evento, router.rs:252-263 trata Err do wake com warn. find_app_container continua Option (None + warn).
- **Visibilidade/API:** nova fn `pub(crate)` em docker.rs; contratos preservados; sem ciclos.
- **Testes:** unitário puro que monta os filtros (sem engine); compose_project (docker.rs:389-393) verde; gated de ciclo.rs cobrem o caminho real.
- **Validação:** trio + **G** (após L0.2, também no CI).
- **Aceite:** `grep -rn "com.docker.compose.project=" crates/fluxd/src/` só docker.rs; suíte verde.

### L3.2 · `dados-sqlite-reuso-em-dockersource` — P2 (intenção P1; revisor rebaixou) · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/{docker.rs,sqlite_source.rs}`.
- **Problema (evidência):** docker.rs:321-341 reimplementa project_exists/release_flags do SqliteSource (comentário :321 "mesmo código do SqliteSource") e ENGOLE erro SEM log: `.is_ok()` (327), `unwrap_or((false,false))` (339) — numa falha de leitura o router finge "projeto inexistente"/"sem flags". Revisor achou mais uma cópia: host_custom (docker.rs:297-305 ≡ sqlite_source.rs:67-75) — **incluir**. compose_project_cor engole Err (172-174).
- **Movimento:** SAEM de docker.rs: sqlite_project_exists (324-328) e sqlite_release_flags (330-340). project_exists/release_flags/**host_custom** do DockerSource passam a delegar a SqliteSource::new(self.sqlite) — sem ciclo (sqlite_source só importa decision+state). compose_project_cor passa a logar warn em Err.
- **Visibilidade/API:** nenhuma mudança pública; 2 helpers privados somem.
- **Testes:** unitário do SqliteSource contra State::in_memory (projeto existe/não existe, flags 0/1, host_custom); suíte do router (mock de HostSource) verde.
- **Validação:** trio.
- **Aceite:** `grep -rn "SELECT 1 FROM projects\|has_robots" crates/fluxd/src/docker.rs` vazio; tracing::error! revisados nos caminhos de erro.

### L3.3 · `dados-exec-container-unificado` — P2 · M (1-2 dias) · risco MÉDIO

- **Arquivos-alvo:** `crates/fluxd/src/{db.rs,docker.rs,ciclo.rs}`.
- **Problema (evidência):** esqueleto bollard create_exec→start_exec→stream→inspect duplicado (~50 l. cada): db.rs:367-422 embrulha em `sh -c` (386), sem workdir, SEM TETO de saída (395-410 acumula em memória); ciclo.rs:375-458 usa args verbatim, workdir do inspect, teto 256 KB e regra exit -1-while-running (423-456). Contratos JÁ divergiram; próxima mudança de exec (timeout, usuário, env) terá de ser feita 2×.
- **Movimento:** SAI exec_in_container (db.rs) e corpo de rodar_exec (ciclo.rs; chamadores preservados por wrapper fino). ENTRA em docker.rs: `exec_no_app` pub(crate) + tipos ExecSpec/ExecOutput. FICAM: db.rs mapeia exit code → erro de migrate/seed; ciclo.rs mapeia → resposta HTTP. **Ajuste do revisor:** ExecSpec preserva as diferenças deliberadas como campos — db.rs mantém o wrap `sh -c` e GANHA teto (mudança de comportamento: saída truncada com marker — usar None ou teto generoso, decidir no passo); ciclo.rs mantém workdir do inspect e a regra exit -1 (db.rs adota o contrato mais estrito: hoje unwrap_or(-1), db.rs:420).
- **Visibilidade/API:** nenhuma API pública muda; sem ciclos (db.rs:21 e ciclo.rs:48 já importam docker.rs); bollard/futures_util já são deps.
- **Pré-requisitos:** L0.2 (o caminho de engine precisa de rede no CI).
- **Testes:** unitário puro que monta CreateExecOptions a partir de ExecSpec (shell vs args, workdir, teto) — sem engine; gated reais ciclo.rs:812-864 verdes; caracterização dos contratos atuais (db.rs:365-366, ciclo.rs:447-456).
- **Validação:** trio + **G**.
- **Aceite:** `grep -rn "create_exec" crates/fluxd/src/` só docker.rs; gated verde com engine.

---

## Lote 4 — Páginas e identidade visual

### L4.1 · `rede-pages-escape-interpolacao` — P2 (intenção P1; revisor rebaixou: caminho morto hoje) · S (meio dia) · risco baixo — **OBRIGATÓRIO ANTES DE F11**

- **Arquivos-alvo:** `crates/fluxd/src/pages.rs` (sem árvore nova).
- **Problema (evidência):** interpolação crua de dado do Host: branch_missing injeta branch+name (108-113); failed (88-98) e wait (64-84) injetam name em body/heading/title; branch vem do Host sem validação de charset (host.rs:107-115); not_found JÁ escapa (130) — contrato inconsistente no mesmo módulo. Caminho morto hoje (branch_exists=true sempre — docker.rs:290-293, sqlite_source.rs:59-64; name gateado — api.rs:201-205, host.rs:98,119); F11 liga branch_exists real e a página renderiza string do atacante; Caddy repassa Host arbitrário.
- **Movimento:** todos os símbolos ficam; mudam corpos: branch_missing passa a interpolar html_escape(branch) e html_escape(name); failed/wait interpolam html_escape(name). Camada única de escape: escapar nos VALORES, mantendo html_page como formato confiável (padrão body_html).
- **Visibilidade/API:** nenhuma assinatura muda (todas `(...) -> Response`, consumidas por router.rs:266-312).
- **Testes:** caracterização espelhando not_found_escapa_html_do_host (pages.rs:287-292): branch_missing com `<img src=x onerror=alert(1)>` não contém o literal `<img` e contém a versão escapada; wait/failed idem; caso positivo com branch legítima legível no `<code>`.
- **Validação:** trio + `cargo test -p fluxd pages::` (subset do trio; **requer verificação** como comando isolado).
- **Aceite:** host/branch hostis renderizados como entidades nos 3 caminhos; suíte de pages verde.

### L4.2 · `rede-paginas-croma-compartilhado` — P3 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/{pages.rs,edge.rs,waf.rs}`.
- **Problema (evidência):** CSS byte-idêntico em 3 módulos (pages.rs:12-17, edge.rs:558-563, waf.rs:443-448 — md5 igual verificado por execução); waf.rs:441-442 documenta a cópia como consequência da privacidade das consts.
- **Movimento:** pages.rs dono único: `pub(crate) const CSS` e `pub(crate) fn html_escape` (hoje privados — pages.rs:12, 200). edge.rs remove html_error_page/CSS locais (521-563) e monta as próprias páginas sobre o croma importado; waf.rs remove CSS (443-448), idem. panel.rs fica para L4.3.
- **Visibilidade/API:** nenhuma API `pub` externa muda.
- **Testes de forma visual:** edge.rs:769-775, waf.rs:836-844, pages.rs:207-337 inteiros. Opcional: teste de consistência (footer `<footer>FLUX</footer>` + substring do CSS nas páginas de edge/waf).
- **Validação:** trio + `grep -rn "linear-gradient(160deg,#0f172a" crates/fluxd/src` devolve UMA definição.
- **Aceite:** edge/waf sem const própria do croma; suíte verde.

### L4.3 · `foco-panel-esc-compartilhado` — P3 · S (meio dia) · risco baixo — **após L4.2**

- **Arquivos-alvo:** `crates/fluxd/src/{panel.rs,pages.rs}`.
- **Problema (evidência):** esc (panel.rs:592-597) e html_escape (pages.rs:200-205) idênticos byte a byte (mesmos 4 replaces); comentário de panel.rs:591 admite a cópia. Escape é função de segurança com teste dos dois lados (pages.rs:287-292; panel.rs:1386) — duas cópias podem divergir silenciosamente numa correção futura.
- **Movimento:** html_escape vira pub(crate) em pages.rs (se L4.2 já landou, já está); panel.rs mantém `fn esc(s: &str) -> String { pages::html_escape(s) }` (delegação, call sites e testes intocados). Dono = pages.rs (módulo mais antigo da família). Se L6.3 landar antes: formato.rs::esc delega.
- **Testes:** esc_nao_deixa_html_passar (panel.rs:1386-1391) e testes de pages verdes pelo caminho da delegação. Opcional: asserção de idempotência no teste do dono.
- **Validação:** trio + `grep -c "replace('" crates/fluxd/src` retorna UMA definição de escape no crate.
- **Aceite:** uma definição de escape; testes dos dois lados verdes.

### L4.4 · `foco-css-nucleo-compartilhado` (AJUSTADA) — P2 · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/{ui.rs(NOVO),panel.rs,observe_page.rs,router.rs}` + `lib.rs` (mod ui;).
- **Problema (evidência):** duas cópias do CSS admin JÁ divergiram em silêncio (panel.rs:63-140 vs observe_page.rs:53-116; diff executado na auditoria: ~49 l. comuns; panel-only --attn/.sb*/.sync; observe-only form.filtros/details pre) enquanto observe_page.rs:49-52 declara "cópia VERBATIM" — falso. NOINDEX/x_robots_tag em panel.rs:46,48-50 e observe_page.rs:34,36-38. **Ajuste do revisor (vinculante):** existe uma TERCEIRA cópia em router.rs (x_robots_tag 54-56, NOINDEX 68 — caminho R22 de todo host dev): ui.rs absorve 3 consumidores, não 2.
- **Movimento:** ENTRA ui.rs: `pub(crate) const NOINDEX`, `pub(crate) fn x_robots_tag() -> HeaderName`, `pub(crate) const CSS_BASE`. SAEM de panel.rs e observe_page.rs os 3 símbolos + núcleo comum do CSS. FICAM os tails exclusivos (**corte do CSS_BASE definido no PR**: tokens comuns + regras base; painel mantém --attn/.sb/.sync; observe mantém form.filtros/details pre — :root diverge entre os arquivos). router.rs passa a importar de ui.rs. As DUAS famílias visuais continuam separadas (pages/edge/waf = pública, L4.2; ui.rs = admin).
- **Mecânica:** concat! não aceita const — compor o `<style>` em runtime com `format!("{CSS_BASE}{}")` dentro de layout (observe_page.rs:400) e equivalente do panel; custo: 1 alloc por request que já faz dezenas. observe_page/panel podem largar o import de HeaderName (observe_page.rs:26) se x_robots_tag local sair e nada mais usar.
- **Visibilidade/API:** nenhuma API externa muda; pagina segue único pub do módulo; ui.rs é módulo privado com itens pub(crate).
- **Pré-requisitos:** nenhum (mas L6.3 prefere landar depois deste).
- **Testes:** caracterização ANTES da troca: 1 asserção nos testes existentes de cada página de que o `<style>` contém tokens compartilhados (ex.: "--accent:#2563eb", ".curve polyline"); depois, os 6 testes de observe_page (1026-1282) e a suíte do panel passam SEM edição de asserção de corpo.
- **Validação:** trio + `grep -n "const CSS" crates/fluxd/src/*.rs` mostra composição em exatamente 2 lugares (panel, observe_page) e NOINDEX/x_robots_tag definidos 1× em ui.rs.
- **Aceite:** `<style>` de /observe byte-a-byte igual ao de hoje (capturar antes/depois); sem --attn em observe_page.

### L4.5 · `foco-render-monitor-secoes` — P3 · S (meio dia) · risco baixo — **após L4.4**

- **Arquivos-alvo:** `crates/fluxd/src/observe_page.rs` (MESMO arquivo, sem árvore nova).
- **Problema (evidência):** render_monitor (501-764, 264 l.) mistura 4 seções com contratos próprios de honestidade: filtros (527-599, com erro de projetos na página 551-557), curva geral (601-634, estado vazio 629-632), drill por container (636-692, 3 estados), tabela de picos (694-761, snapshot indisponível 744-745). Localizar onde se diz "sem coleta ainda" exige ler a função toda.
- **Movimento:** SAI do corpo de render_monitor 4 blocos → 4 fn privadas no mesmo arquivo: filtros_html, serie_html, drill_html, picos_html (assinaturas `(&AppState, &Filtro[, t0, now])` — os blocos só leem app/f/t0/now, verificado). FICA: relógio + erro honesto (505-517), t0 (518), título (520-525), composição e layout final (763).
- **Visibilidade/API:** zero — nada pub muda (pagina segue único pub), nenhum import novo.
- **Testes:** os 6 testes de integração existentes (1026-1282) são a proteção e cobrem cada seção — devem passar SEM edição; se um estado vazio honesto ficar para trás, o teste quebra na asserção certa.
- **Validação:** trio.
- **Aceite:** render_monitor ≤ ~50 l.; grep ^pub continua mostrando só pagina; suíte observe_page verde.

---

## Lote 5 — fluxd: núcleo e API

### L5.1 · `nucleo-sweep-lifecycle` (AJUSTADA) — P2 (intenção P1; revisor rebaixou) · S (meio dia) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/lib.rs` (+ `crates/fluxd/src/sweep.rs` NOVO).
- **Problema (evidência):** loop de sweep embutido no composition root (lib.rs:130-158: interval 60 s, primeiro tick pulado :136, SleepManager por tick 140-146, expire_check sempre :155) enquanto observe.rs usa o padrão encapsulado iniciar (observe.rs:493, spawn :510); epoch inline (151-154) duplica sleep::now_epoch (22-27, que api.rs:606,1207 e observe.rs:43 já importam). lib.rs não tem mod tests — "primeiro tick é pulado" e "sem Docker o teardown ainda roda" (126-129) sem rede.
- **Ajuste do revisor (vinculante):** `iniciar(shared)` NÃO compila como escrito — o loop consome GlobalConfig (clonado em lib.rs:133; SleepManager::new 140-144) e AppState não o carrega. Assinatura: **`pub async fn iniciar(shared: Arc<AppState>, global: GlobalConfig)`**; serve() passa `global.clone()`.
- **Movimento:** SAI de lib.rs o bloco do tick (130-158) e o epoch inline. ENTRA em sweep.rs: iniciar com o loop (chamando `crate::sleep::now_epoch()`) + corpo do tick extraído em fn testável (`async fn tick(shared: &AppState, docker: Option<&Docker>)`). FICAM: sleep.rs/temporary.rs intocados; observe::iniciar paralelo; lib.rs serve() vira `tokio::spawn(sweep::iniciar(Arc::clone(&shared), global.clone()))` ao lado do spawn de observe (lib.rs:164-167). lib.rs para de importar SleepManager/expire_check no corpo.
- **Visibilidade/API:** novo `pub mod sweep;` com `pub async fn iniciar`; NENHUMA assinatura existente muda (serve(), SleepManager::sweep, expire_check iguais).
- **Pré-requisitos:** nenhum.
- **Testes:** caracterização ANTES do movimento: teste do tick com docker=None — projeto temporário expirado some do estado (mesmo cenário de temporary.rs:264-284) e nenhum sweep de sono roda; com docker=Some (gated FLUX_DOCKER_URL, convenção do repo): sweep_one roda sem pânico. Intervalo/primeiro tick documentados no módulo (não unit-testáveis sem engessar tokio::time).
- **Validação:** trio + **G** + `grep 'interval' crates/fluxd/src/lib.rs` vazio.
- **Aceite:** lib.rs = AppState + serve() com 2 spawns; suíte do fluxd verde.

### L5.2 · `nucleo-helper-erro` — P3 · S (horas) · risco baixo — **antes de L5.3 (mesmo arquivo)**

- **Arquivos-alvo:** `crates/fluxd/src/{api.rs,ciclo.rs}`.
- **Problema (evidência):** ciclo.rs:87-89 copia `erro` da api.rs:172 (comentário 85-86 ADMITE: "o helper de lá não é pub") e projeto_registrado (129-141) espelha garante_projeto (api.rs:613-625 — revisor comparou: mesmos status e mensagens). Parsers do CLI dependem do formato `{"error": causa}` (ciclo.rs:86) — duas cópias vivas de contrato de fio.
- **Movimento:** SAI de ciclo.rs as 2 fns privadas; ENTRA `pub(crate)` em api.rs::erro e api.rs::garante_projeto (ou reply.rs novo — decidir com L5.3; recomendação: pub(crate) agora, sem módulo novo). ciclo.rs importa `crate::api::{erro, garante_projeto}`; comentários "copiado da api.rs" (85-86, 127-128) somem.
- **Visibilidade/API:** api.rs ganha pub(crate) em 2 itens privados (sem API externa nova); nenhum formato de resposta muda.
- **Coordenação:** exige sincronia com o auditor de api.rs (mesmos helpers) — resolvido: este passo vem primeiro; L5.3 preserva os dois em api/mod.rs.
- **Testes:** a suíte de ciclo.rs É a caracterização, sem edição de cenário: 630-647 (422), 667-683 (404), 686-704 (409+evento), 707-721 (503); testes de api.rs que cobrem garante_projeto/erro.
- **Validação:** trio + `grep -n "fn erro" crates/fluxd/src` exatamente 1 definição.
- **Aceite:** asserts de causa dos testes de ciclo.rs passam sem edição.

### L5.3 · `foco-api-auth` — P3 (intenção P2; revisor rebaixou: ~90 l., sem defeito ativo) · S (meio dia) · risco baixo — **após L5.2**

- **Arquivos-alvo:** `crates/fluxd/src/api.rs` → diretório `crates/fluxd/src/api/{mod.rs,auth.rs}`.
- **Problema (evidência):** primitivas de auth vivem no módulo da API :7420 mas servem outra superfície: deploy.rs:56 importa {bearer_token, hash_confere, sha256_hex}. Regras do gate (fail-closed 503, 401 com evento, comparação em tempo constante — api.rs:1-12, 103-116, 158-170) sem lar próprio; retrabalho em api.rs (1.673 l.) arrisca o gate de deploy por proximidade.
- **Movimento:** SAEM para api/auth.rs: TOKEN_KEY (40), auth (85-118), stored_token_hash (122-137), bearer_token (142-144), sha256_hex (148-156), hash_confere (164-170). FICAM em api/mod.rs: build_api (46-81), erro (172-174), handlers/DTOs (176-983), mod tests (985-1673).
- **Visibilidade/API:** `auth` e `TOKEN_KEY` viram pub(super); bearer_token/sha256_hex/hash_confere continuam pub(crate); mod.rs reexporta `pub(crate) use auth::{bearer_token, hash_confere, sha256_hex};` — **deploy.rs:56 NÃO muda**. `crate::api::build_api` preservado (lib.rs:123, router.rs:510 intocados). auth.rs usa `super::erro` (filho vê privado do ancestral — sem mudança de visibilidade). Teste usa `super::auth::TOKEN_KEY` (api.rs:1008 migra referência).
- **Pré-requisitos:** nenhum (independente de L5.4; sequência com L5.2 é por higiene de arquivo).
- **Testes:** existentes, sem edição: healthz_responde_sem_token (api.rs:1070), fail-closed (1079), 401+evento (1099 — conta eventos no banco); deploy.rs 10 tokio tests cobrem o consumo.
- **Validação:** trio.
- **Aceite:** deploy.rs:56 sem edição (reexport ativo); cargo check sem warnings; 23 testes (13 api + 10 deploy) verdes.

### L5.4 · `foco-api-estado-dev` — P3 · S (meio dia; M se saude_ok precisar seam) · risco baixo — **com GATE de caracterização obrigatória**

- **Arquivos-alvo:** `crates/fluxd/src/{api.rs,docker.rs}`.
- **Problema (evidência):** project_status (api.rs:400-542) embute 35 l. de política do engine (463-498): DockerSource::env_state + alvo_app + saude_ok + regra F9 ("RUNNING só é awake com [health] respondendo; senão waking"). Verdade do engine em handler que também consulta 4 tabelas. **Cobertura atual do ramo: NENHUMA** (testes sobem AppState sem Docker, api.rs:1013; dado não encontrado em docker.rs — fora do escopo do auditor).
- **Movimento:** SAI de api.rs o bloco 458-498. ENTRA em docker.rs: fn `pub(crate) async` (ex.: estado_dev_rotulado) com parâmetros EXPLÍCITOS (handle Docker, estado do banco, cliente HTTP, nome) — docker.rs NÃO passa a depender de AppState. FICA: leitura de sono (432-456), migrations (500-519), hosts efetivos (521-526), DTOs e resto do handler. api.rs perde os usos de HostSource/DockerSource/alvo_app/saude_ok deste bloco. project_status vira mapeamento HTTP puro; JSON de saída inalterado.
- **Pré-requisitos:** NENHUM movimento sem antes criar o teste de caracterização em docker.rs dos 3 ramos (não-Running → estado cru; Running + health ok → awake; Running + health falha → waking) com saude_ok injetável/fake. **Sem essa rede, não executar.**
- **Testes:** a caracterização nova + register_lista_e_status_ciclo_completo (api.rs:1125) sem alteração (JSON igual nos cenários sem Docker).
- **Validação:** trio + **G**.
- **Aceite:** regra F9 em um só lugar (grep); caracterização dos 3 ramos verde.

---

## Lote 6 — fluxd estrutural: compose e painel

### L6.1 · `foco-compose-bluegreen` — P2 · S (meio dia) · risco baixo — **após L3.1**

- **Arquivos-alvo:** `crates/fluxd/src/compose.rs` → `crates/fluxd/src/bluegreen.rs` (NOVO, irmão de deploy.rs) + lib.rs (pub mod bluegreen;) + api.rs (3 call sites) + deploy.rs (comentário).
- **Problema (evidência):** domínio blue-green de prod (10/R12) mora em módulo cujo doc (1-21) não o menciona; nenhum símbolo do bloco 872-1083 é usado pelo resto do arquivo; consumidores externos (api.rs:484, 717, 724); contrato documentado em terceiro arquivo (deploy.rs:39).
- **Movimento:** SAEM para bluegreen.rs: DRAIN_PADRAO_SECS (876), HEALTH_* (880-882), saude_ok (888-906), esperar_saude (911-935), trocar_cor (943-1061), lista_do_projeto (1064-1083, permanece privada) + mod troca com teste cor_invalida_recusa_sem_tocar_nada (1360-1383). FICAM em compose.rs: escolher_compose, override, subir_projeto, live, tunel_classificar, coletar_guard e os 12 testes restantes. Banner 872 some.
- **Visibilidade/API:** saude_ok/esperar_saude/trocar_cor/consts seguem pub; api.rs troca `crate::compose::{...}` → `crate::bluegreen::*` (3 linhas); deploy.rs:39 só comentário. **Sem re-export de compat** (o path mentiroso é o problema). NÃO fundir em deploy.rs (917 l.).
- **Nota de L3.1:** a lista_do_projeto movida já deve consumir docker::listar_projeto.
- **Testes:** teste movido junto (caracterização do gate de entrada); suíte completa do fluxd. Teste novo barato no mesmo PR: caminho_override cria {state}/compose/{name}-{env}.override.yml (compose.rs:221-225, sem teste hoje).
- **Validação:** trio + `grep -rn "crate::compose::trocar_cor\|crate::compose::saude_ok\|crate::compose::DRAIN_PADRAO_SECS" crates/fluxd/src` vazio.
- **Aceite:** revisão do diff mostra só movimentação — nenhuma string de evento (events::event) nem ordem de passo alteradas.

### L6.2 · `foco-compose-arvore` (REDUZIDA) — P2 · S o obrigatório; M se o opcional · risco baixo — **após L6.1**

- **Arquivos-alvo:** `crates/fluxd/src/compose.rs`.
- **Problema (evidência):** `tunel_classificar` (836-870) roda `docker compose config --format json` uma SEGUNDA vez no mesmo up (render já existe em :341) e funde I/O com a regra pura (843-859, block_in_place+block_on aninhado); o teste usa uma CÓPIA da regra (mod tunel 1346-1357; comentário 1302-1304 admite) — **bug na regra real não seria pego**.
- **Veredito do revisor (vinculante):** OBRIGATÓRIO e suficiente: extrair `fn classificar_de(v: &serde_json::Value, services: &[String]) -> Vec<(String,String)>` PURA no MESMO arquivo (~15 l.); tunel_classificar vira fetch do JSON + chamada; o teste usa a REAL e a cópia 1346-1357 é APAGADA. O split de diretório em 5 arquivos REBAIXADO (fragmentação fina — pós-bluegreen sobram ~1150 l., faixa do observe.rs único). OPCIONAL e reduzido: split up.rs/live.rs (seams reais: tunel_classificar/coletar_guard só servem subir_projeto — :436/:286); engine/override_yaml ficam no mod.rs sem perda — **decisão do dono** (default: não executar agora).
- **Se o opcional for executado:** compose/mod.rs reexporta toda a superfície atual (`pub use`) — `crate::compose::X` continua compilando em api.rs:895 e router.rs:199/207 SEM tocar esses arquivos (git diff de api.rs e router.rs VAZIO é aceite). subir_live_se_configurado vira pub(crate) em live.rs. tunel_classificar e coletar_guard privados em up.rs. Sem ciclos (up → live/engine; live não importa up).
- **Movimento obrigatório (preservar ordem):** subir_projeto é sequência com efeito colateral (guard → rede → services → render/strip → override ESCRITO EM DISCO → túnel → up → estado → live, compose.rs:263-559) — **mover sem reordenar**; escrever override antes do guard deixaria arquivo órfão no state_dir de projetos bloqueados.
- **Testes:** 12 testes existentes movem com seus símbolos (override ×2, parse_render, strip_list, decidir_live, comando_live ×2, escolher_compose ×2, túnel ×3). Dois novos no mesmo PR: (a) mod tunel apontando para a classificar_de REAL; (b) caminho_override (formato/diretório).
- **Validação:** trio.
- **Aceite:** wc -l de cada novo arquivo < 500 (se o opcional rodar); grep prova que o teste usa a função real; suíte verde.

### L6.3 · `foco-panel-arvore` — P2 · M (1-2 dias) · risco baixo — **após L4.3/L4.4 (dedups já feitos movem menos código)**

- **Arquivos-alvo:** `crates/fluxd/src/panel.rs` → `crates/fluxd/src/panel/{mod.rs,formato.rs,leitura.rs,paginas.rs,tests.rs}`.
- **Problema (evidência):** 1985/1986 l. (maior do crate) empilham 4 camadas com eixos de mudança independentes sobre seams que o autor já desenha como comentários de seção (panel.rs:129, 401, 588, 815, 893, 1027, 1192, 1243); trajetória de crescimento por acréscimo (6 commits, cada um atravessa as camadas); precedente do crate (observe_page.rs e panel_auth.rs nasceram como irmãos extraídos). Superfície externa é UMA fn: panel::build_panel (lib.rs:121, router.rs:370); PanelState pub sem uso externo (grep vazio).
- **Movimento (mapa completo lido linha a linha pelo foco-painel):**
  - **mod.rs (~400):** doc 1-20; imports 22-43 (import morto HostSource :36 vira MENÇÃO, não fix); NOINDEX 46 + x_robots_tag 48-50 (pub(super) para tests.rs — e já delegando a ui.rs pós-L4.4); PanelState 134-138 + FromRef 140-150 (pode rebaixar para pub(crate)/pub(super) — zero uso externo); build_panel 156-176; exige_sessao 181-189; CredenciaisLogin 191-194; login_form 196-208; IpDoCliente 213-232; login_submit 238-291; logout 293-303; resposta 307-318; dashboard 320-332; detalhe_projeto 334-342; pagina_eventos 344-346; rescan_projeto 353-386; rota_desconhecida 388-399; `#[cfg(test)] mod tests;`.
  - **formato.rs (~200, folha):** esc 592-597 (delegando a pages::html_escape pós-L4.3), like_esc 600-604, page_de 608-614, fmt_utc 618-633, dias_de_civil 637-645, epoch_de 651-670, quando 675-703, dot 706-715, badge_level 717-725, badge_sync 735-753 + 6 testes unitários (1385-1501). Depende de std + flux_common::protocol + crate::sync::SyncRow.
  - **leitura.rs (~260, folha):** verdade_ambiente 406-417; PainelEnv 421-427 (pub(super), campos pub(super)); ambiente 429-444; projetos 447-456; EventoRow 458-463 + COLUNAS_EVENTO 465 + row_evento 467-474; eventos_recentes 477-487; eventos_pagina 491-514; agora_epoch 518-526; curva_eventos 530-586; consts 56-58. Zero dep de formato/paginas (sem ciclo).
  - **paginas.rs (~500):** CSS 63-127 (composto de CSS_BASE pós-L4.4), layout 820-853, render_login 858-877, linha_estado 879-891, linha_sync 759-813, EVENTOS_* 53-54, render_dashboard 895-1025, detalhe_status 1029-1037, render_detalhe 1039-1190, render_eventos 1194-1241. Depende de formato + leitura.
  - **tests.rs (~640):** app_sem_docker 1261-1264; painel_com_auth 1269-1281; pedir 1283-1324; get 1328-1346; post_logado 1350-1373; noindex_de 1375-1381; testes 1505-1961 + com_engine (1966-1984, gated).
- **Grafo resultante (acíclico, verificado):** formato e leitura folhas; paginas depende das duas; mod.rs no topo; tests.rs de mod.rs+formato.
- **Visibilidade/API:** `crate::panel::build_panel` mantém caminho e assinatura — lib.rs:121 e router.rs:370 NÃO mudam. Nada novo fica pub no crate. `use super::*` dos testes (panel.rs:1252) vira imports explícitos — pego pelo compilador se faltar.
- **Nota de convenção (divergência registrada e resolvida):** primeiro diretório-de-módulo do crate (src/ é flat) — revisor aprovou: tamanho + crescimento + seams justificam; convenção nova é custo aceito e documentado.
- **Testes:** os 23 existentes SÃO a caracterização (6 unitários 1385-1501 + 17 e2e 1505-1984) — mover SEM editar corpos. Baseline a confirmar antes do move.
- **Validação:** trio + `grep -rn "panel::" crates/fluxd/src` fora do diretório panel/ retorna apenas build_panel (lib.rs:121, router.rs:370) + nenhum arquivo fora de panel.rs é editado.
- **Aceite:** cargo check sem warnings novos de visibilidade/import; mesmos 23 testes verdes.

---

## Lote 7 — flux-cli estrutural

### L7.1 · `foco-cli-contexto` — P2 · S (meio dia) · risco baixo — **destrava L7.2**

- **Arquivos-alvo:** `crates/flux-cli/src/main.rs` (+ `crates/flux-cli/src/contexto.rs` NOVO).
- **Problema (evidência):** contexto_atual/falhar/parse_env/sha256_hex (main.rs:452-537) e LEASE_UP_SECS (32) são usados por TODOS os 9 domínios de comando (call sites: 699, 704, 726, 1128, 1141, 1194, 1221, 1250, 1288, 1432, 1635, 1717, 2168) mas moram no meio do monolito; qualquer extração de domínio os arrasta junto. Extraí-los primeiro torna cada extração seguinte isolada e reversível.
- **Movimento:** SAEM Contexto (452-458), contexto_atual (460-510, process::exit em 465/477/487/493 — semântica preservada byte a byte), sha256_hex (512-520), parse_env (522-530), falhar (534-537), LEASE_UP_SECS (32). FICA: Cli/Cmd + sub-enums, main(), stub (2323). UP_TIMEOUT_SECS/TAIL_PADRAO seguem para comandos/ na fase 2.
- **Visibilidade/API:** nenhuma API externa (crate bin-only); movidos viram pub(crate); main.rs ganha `mod contexto;` + `use contexto::{...}`. Sem ciclo: contexto.rs depende de flux-common e api::ApiError (só exit_code). Sem lazy-static nem init-order — consts continuam const.
- **Testes:** os 16 testes inline (2328-2574) são a proteção — sha256_hex exercitado por token_novo_e_sha256_hex_tem_formato_do_contrato (2498-2516, vetor NIST).
- **Validação:** trio + **requer verificação**: smoke local `flux status` e `flux up` num projeto dev (aviso de projeto sem flux.toml herdando nome da pasta, 498-501, deve continuar).
- **Aceite:** rg 'fn contexto_atual' = 1 ocorrência em src/contexto.rs; `flux status` idêntico; nenhum pub além de pub(crate).

### L7.2 · `foco-cli-comandos-por-dominio` — P2 · M (1-2 dias) · risco MÉDIO — **após L7.1; um commit por módulo**

- **Arquivos-alvo:** `crates/flux-cli/src/main.rs` → `src/comandos/{mod.rs,up.rs,ciclo.rs,logs.rs,sync_cmd.rs,build.rs,watch.rs,env_cmd.rs,live.rs,tokens.rs}`.
- **Problema (evidência):** 9 domínios de comando independentes que compartilham só os 4 helpers de contexto.rs; main.rs 2575 l. é o despejo; doc de token_generate dividida (548-554 + fragmento 566-567 — confirmado por leitura); crate já adota módulo-por-domínio (api/sync/tunel/wizard).
- **Movimento (mapa verificado linha a linha pelo revisor):**
  - **up.rs (~330):** up (697-882), seed_escolhida (1178-1186), overlays_habilitados (886-892), aplicar_overlays (897-934), coletar_overlays (937-948), coletar_migrations (958-965), filtra_migrations_ignoradas (969-979), coletar_migrations_em (982-1002), UP_TIMEOUT_SECS (36) + testes 2332-2377, 2415-2431. **up orquestra ordem estrita (lease 734-739 → sync 746-768 → sleep 2s :756 → overlays 774-785 → subida/teto 788-827 → estabilidade 838-854) — mover verbatim, sem "melhorias".**
  - **ciclo.rs (~310):** down (1192-1212), ciclo_api (1219-1238; arm "ps" morto em 1225 vira MENÇÃO, não fix), status (1126-1149), imprime_status (1154-1173), ps (1008-1121), exec (1424-1462), db_fresh (1248-1277).
  - **logs.rs (~260):** logs (1279-1338), caminho_logs (1342-1344), drenar_linhas (1352-1362), frame_de_erro (1367-1369), consumir_sse (1376-1418), TAIL_PADRAO (40) + testes 2401-2492.
  - **sync_cmd.rs (~150):** TETO_RESET_SECS (1548), fmt_files (1468-1470), sync_spec_do_contexto (1475-1482), sync_status/pause/resume/reset (1487-1606) + teste 2569-2573.
  - **build.rs (~195):** BUILD_TIMEOUT_SECS (1612), default_build_command (1616-1621), shell_aspas_simples (1626-1628), build_assets (1634-1743), rodar_build_na_vm (1749-1797) + testes 2536-2567.
  - **watch.rs (~210):** watch (1808-1824), tick_watch (1828-1913), rescan_sessao (1919-1949), spec_da_sessao (1957-2006). *(Nota: descida definitiva de spec_da_sessao/rescan_sessao para sync.rs é direção-alvo registrada — AF-18 — não neste movimento.)*
  - **env_cmd.rs (~150):** env_ssh_alvo (2014-2016), env_remoto (2018-2020), env_pull (2022-2050), env_push (2052-2096), env_edit (2098-2147), gitignored_env (2151-2155).
  - **live.rs (~170):** live_toggle (2166-2237), container_do_projeto (2240-2242), container_do_servico (2248-2269), set_live_no_toml (2274-2320) + teste 2379-2399.
  - **tokens.rs (~160):** token_novo (542-546), vm_ssh (555-564), token_generate (568-617), nome_de_projeto_ok (624-629), project_token (639-688) + testes 2498-2532; **doc deslocada 548-567 se recompõe no movimento** (doc inteiro volta para cima de token_generate).
  - **FICA em main.rs:** Cli/Cmd + sub-enums (42-315), main() (317-445), stub (2322-2326) → ~460 l.
- **Visibilidade/API:** movidos viram pub(crate). Cross-uses declarados: build.rs usa live.rs::container_do_servico (1727); live.rs usa env_cmd.rs::env_ssh_alvo (2211) — pub(crate) cobre. Imports que migram: http_body_util/hyper::Response → logs.rs; Sha256 → contexto/up/tokens; getrandom → tokens (544). Sem [lib]: nenhuma superfície pública nova; sem ciclo (comandos/* → contexto/api/sync/tunel; main → comandos).
- **Testes:** 15 testes inline movem junto (`use super::*` válido): 4→up, 5→logs, 2→tokens, 2→build, 1→live, 1→sync_cmd — cargo test -p flux-cli verde com os MESMOS nomes.
- **Validação:** trio por commit + **requer verificação**: smoke manual em localhost/VM de dev — flux status; flux ps fora/dentro de projeto; flux sync status; flux logs dev --tail 5 e -f (SSE; frame [fluxd: erro...] sai com exit ≠ 0, 1387-1392); flux exec dev -- echo ok (exit repassado, 1458); flux live OFF→ON; flux build; flux env pull/push; flux token generate.
- **Aceite:** rg por nome de cada símbolo → exatamente 1 arquivo; nenhum pub além de pub(crate); smoke idêntico.

### L7.3 · `foco-cli-ssh-helper` — P3 · S (meio dia) · risco baixo — **opcional, após L7.2; decisão do dono**

- **Arquivos-alvo:** helper em `src/contexto.rs` ou `src/ssh_util.rs` (decidir com os domínios existentes) + 9 call sites.
- **Problema (evidência):** prefixo `['-o','BatchMode=yes','-o','ConnectTimeout=8', alvo]` em 9 call sites (576, 656, 914, 1756, 2023, 2057, 2100, 2205, 2250 — revisor confirmou em :579, :659, :917, :1759, :2026, :2060, :2103, :2208, :2253): mudar timeout/auth exige tocar 9 pontos; um esquecido diverge em silêncio.
- **Movimento:** `fn ssh_alvo() -> String` + `fn ssh_vm(args: &[&str]) -> Command` pub(crate) com os flags fixos; sites só passam comando remoto e stdio. **Exclusões legítimas:** vm_ssh (555-564) NÃO usa o helper — ssh interativo sem BatchMode de propósito (doc 551-554). Variações por site ficam: stdout pipado + stdin herdado do tar (aplicar_overlays 910-927), stdout/stderr HERDADOS (rodar_build_na_vm 1765-1766), stdin pipado (env_push 2070-2072), .output() vs .status().
- **Testes:** nenhum unitário cobre ssh real — proteção é smoke: flux env pull (.output), env push (stdin), live OFF (.status), build (stdio herdado), conferindo ordem (-o antes do alvo, alvo antes do comando).
- **Validação:** trio + smoke acima (requer verificação).
- **Aceite:** rg 'BatchMode=yes' e 'ConnectTimeout=8' → 1 ocorrência cada (o helper); comportamento idêntico.

### L7.4 · `foco-sync-divisao-submodulos` (AJUSTADA) — P2 · S (meio dia) · risco baixo — **após L2.5**

- **Arquivos-alvo:** `crates/flux-cli/src/sync.rs` → `crates/flux-cli/src/sync/{mod.rs,spec.rs,estabilidade.rs}` (+1 linha em `tests/sync_tests.rs:7`).
- **Problema (evidência):** 1286 l. (929 código + 356 teste) com duas responsabilidades sem nenhuma dependência entre si: ponte mutagen (spawn de processo 408-498) e estabilidade de árvore git (729-929 — zero linhas tocam mutagen; erro em String :747 vs SyncError na ponte; único ponto comum é extra_ignore). Evidência de atrito: segundo consumidor do matcher fora do domínio (main.rs:959-976); git log concentra na fronteira os últimos 4 fixes; doc derivou (429-430).
- **Movimento:** SAEM para spec.rs: LABEL (59), VM_PROJECTS_ROOT (64), GLOBAL_IGNORES (83-91), Beta+to_url (147-167), vm_beta (173-183), branch_slug (188-209), ignores_do_projeto (220-250), SyncSpec+7 métodos (256-381), testes 938-1046, helpers git_disponivel/repo_temp (1050-1070). SAEM para estabilidade.rs: TETO_RODADAS (734), listar_arquivos_rastreaveis (743-786), caminho_ignorado (793-796), padrao_casa (798-817), glob_casa (822-846), hash_estabilidade (852-877), hex_de (879-885), esperar_estabilidade (894-929), testes 1109-1285. FICA em mod.rs: doc, SyncError (103-141), ENV_BIN (55), LIST_TEMPLATE (97), mutagen_bin (390-404), run (408-426), parse_created_id (431-449), ciclo create/pause/resume/terminate/reset (455-498), StatusState/parse (504-727), teste 1077-1107. **Ajuste do revisor (vinculante):** spec.rs NÃO redeclara VM_USER/VM_HOST — importa `crate::tunel::{VM_HOST, VM_USER}` (compatível com L2.5); LABEL/VM_PROJECTS_ROOT/GLOBAL_IGNORES ficam locais. Unificar convenção de erro (String vs SyncError) NÃO faz parte do movimento. Doc de parse_created_id corrigida junto (fecha L1.6 se lá não rodou).
- **Formato OBRIGATÓRIO:** directory/mod.rs (não sync.rs + sync/) — com `#[path]`, os mod filhos resolvem relativos ao DIRETÓRIO do arquivo incluído; o compile standalone do sync_tests.rs só acha os filhos em src/sync/.
- **Visibilidade/API:** main.rs NÃO muda: `mod sync;` (main.rs:13) resolve para sync/mod.rs e `crate::sync::*` continua válido via pub use (33 ocorrências — invariante medida). run/problemas/endpoint_de/padrao_casa/glob_casa/hex_de seguem privados; caminho_ignorado segue pub(crate) (main.rs:976); resto segue pub. Dependência entre irmãos sem ciclo: spec usa super::SyncError; estabilidade usa super::spec::GLOBAL_IGNORES; mod usa spec.
- **Testes:** nenhum novo — tests/sync_tests.rs (22 testes) cobre a superfície inteira por caminhos sync::* e é exatamente o teste que valida os re-exports; 12 testes internos movem com seus grupos sem editar asserções.
- **Validação:** trio (o target de teste recompila o módulo — roda 2×, como hoje) + **requer verificação**: (1) `git diff crates/flux-cli/src/main.rs` VAZIO; (2) única diff de sync_tests.rs é a linha do #[path]; (3) `grep -c "sync::" crates/flux-cli/src/main.rs` inalterado (33).
- **Aceite:** mesmo número de testes (22 externos + 12 internos + main.rs); compile standalone do sync_tests.rs continua funcionando.

### L7.5 · `foco-api-tests` — P3 · S (1h) · risco baixo — independente

- **Arquivos-alvo:** `crates/flux-cli/src/api.rs` (+ `crates/flux-cli/src/api/tests.rs` NOVO).
- **Problema (evidência):** 497 l. (41%) de bloco #[cfg(test)] (728-1224) incluindo mock axum do fluxd de ~240 l. (742-981); o contrato CLI↔fluxd ainda evolui (mensagens citam F5-F17, api.rs:829) — o bloco que mais muda está embutido no arquivo de produção.
- **Movimento:** SAI o bloco inteiro (mock_fluxd + handlers 742-981, cliente() 983-985, 14 testes 987-1223). FICA 1-726 sem alteração. Em api.rs, o bloco vira `#[cfg(test)]\nmod tests;` — layout foo.rs + foo/tests.rs válido (Rust 2018+), não exige mod.rs.
- **Visibilidade/API:** módulo só sob cfg(test); acesso privado preservado pela regra de descendência (SchemaIn privada :225 usada no teste :1202-1223; campo `parar` :695 acessado em :1067 continuam compilando). Mover para tests/ de integração é IMPOSSÍVEL (sem [lib]).
- **Testes:** a suíte movida é a caracterização — 14 testes passam idênticos no novo local (gate: MESMA contagem antes/depois).
- **Validação:** trio + **requer verificação**: `cargo build -p flux-cli` sem warning novo (cfg(test) não afeta produção).
- **Aceite:** api.rs reduzido a ~730 l. de produção; nenhum diff fora do bloco de testes e da linha `mod tests`.

---

## Decisões em defer (zero código; gatilhos de reavaliação)

### `foco-observe-manter` — observe.rs (1106 l.) permanece arquivo único

Coeso (pipeline único coleta→rollup→perícia→consulta compartilhando Amostra/Metrica/PeakRow/SCHEMA_OBSERVE; 36% teste; src/ flat — criar observe/ romperia o padrão local sem fronteira nova). **Gatilhos de reavaliação:** (1) segunda fonte de coleta além de bollard; (2) face de leitura consumida por >1 página ou exposta via api.rs; (3) regra de pico configurável por projeto. Invariantes para qualquer divisão futura: ordem coleta→grava→poda→perícia em ciclo (523-534); snapshot de irmãos usa amostras do MESMO ciclo em memória (684-688); determinismo via BTreeMap+sort (560, 650-656); uma transação por ciclo (220-227).

### `foco-state-manter` — state.rs (1038 l.) permanece arquivo único

Inventário schema↔purge↔ladder auditável só no mesmo arquivo (comentários 292-298: v3 e v4 exigiram editar purge junto — co-localização é load-bearing); repartir por domínio criaria módulos de 22-120 l. sem unidade nova (a posse do SQL de feature já está nos chamadores via `with`). Alternativa rejeitada com evidência: mover schema para state/schema.rs separaria o DELETE de cada tabela do DDL que a criou. Hotspot interno registrado (não proposto): ladder O(n²) do migrate (597-673) — refactor in-file futuro que PRESERVA o self-healing (DDL idempotente sem transação única). Comando de proteção para qualquer toque futuro: `cargo test -p fluxd state::`.

---

## O que NUNCA fazer (anti-padrões concretos deste repo)

1. **NUNCA dividir só para baixar contagem de linhas.** Este repo tem arquivos >1000 l. COESOS que ficam: observe.rs (1106), state.rs (1038), ciclo.rs (997), flux-cli/api.rs (produção 1-726), deploy.rs, edge.rs, waf.rs, router.rs. Fatiar handlers de api.rs por endpoint e dividir produção do api.rs do CLI em error/types/endpoints foram REJEITADOS com evidência (tabela única do build_api é a propriedade "rota nova nasce atrás do gate"). Se a motivação de um passo for o número, o passo está errado.
2. **NUNCA mover código sem teste de caracterização rodando antes.** Baseline verde ANTES do movimento é pré-requisito de todo passo. Caso-limite: L5.4 (awake/waking) tem cobertura ZERO hoje — o passo é BLOQUEADO até a caracterização dos 3 ramos existir. Os 23 testes do panel movem SEM editar corpos; os 22 do sync_tests são a prova dos re-exports.
3. **NUNCA quebrar API pública dos crates.** `panel::build_panel` (lib.rs:121, router.rs:370), `crate::compose::*` nos consumidores, `crate::sync::*` (33 usos em main.rs), deploy.rs:56 e ProjectToml::hosts_custom (api.rs:239) são preservados por re-export/delegação — aceites que EXIGEM git diff vazio nos consumidores. flux-common é consumido pelos DOIS crates: nenhuma remoção/renome de item público do common sem checar os 2 consumidores.
4. **NUNCA misturar limpeza não relacionada com extração arquitetural.** Arm "ps" morto (main.rs:1225) vira MENÇÃO, não fix; import morto (panel.rs:36) idem; comentário de "v2" velho (state_migration_integration.rs:2-4) vira cleanup próprio; doc de parse_created_id viaja junto do símbolo em L7.4, não como commit solto "de passagem" — um commit por escopo, sempre.
5. **NUNCA "melhorar" enquanto move.** Movimento é verbatim: subir_projeto mantém a ordem estrita (escrever override antes do guard deixa arquivo órfão no state_dir); `abrir_stream` NUNCA é unificado com `call_com_timeout` (mataria o timeout-zero do `flux logs -f`, invariante api.rs:561-564); strip_hop_by_hop continua AUSENTE no forward_ws (Connection/Upgrade SÃO o pedido); contexto_atual mantém os 4 process::exit no lugar; vm_ssh continua fora do helper ssh (BatchMode quebraria o login interativo).
6. **NUNCA mudar comportamento silenciosamente.** As únicas mudanças de comportamento do plano são DECLARADAS no passo: propagar erro de listagem (L3.1), teto de saída no migrate (L3.3, decidir None/teto generoso), quebra de parse de image_guard (L2.2, deliberada e barulhenta), warn em compose_project_cor (L3.2). Todo o resto preserva comportamento byte a byte — strings de evento e ordem de passos conferidas no diff.
7. **NUNCA engolir erro ao "consertar" engolimento.** O padrão do repo é events.rs:19-21 (tracing::error! + UI): L3.2/L3.1 seguem-no. Não introduzir unwrap/expect novo no caminho de request.
8. **NUNCA rodar build/assets na mão nem tocar VM/ssh/flux.** O flux gerencia dev server e executa docker internamente; o repo no Windows nunca é tocado pela VM; gates do CI são a verificação de longo prazo. `cargo` roda no host de dev.
9. **NUNCA push sem pedido, nunca --amend em HEAD compartilhado.** Commit local com pathspec dos próprios arquivos; múltiplos agentes operam no mesmo working tree — re-ler arquivo antes de editar.
10. **NUNCA criar abstração onde uma função resolve.** `classificar_de` é fn pura no MESMO arquivo (não módulo novo); ui.rs é um módulo único, não "framework de design system"; paths.rs só se a const sair de temporary.rs; as DUAS famílias visuais (pública pages/edge/waf × admin ui.rs) não são unificadas — políticas diferentes (noindex dev vs sempre).
11. **NUNCA reduzir visibilidade além do declarado no passo.** `sessions`/`limiter` permanecem pub (L1.4);`PanelState` só rebaixa porque o grep provou zero uso externo (L6.3); nada ganha `pub` novo além do `pub(crate)`/`pub(super)` especificado.

---

## Perguntas abertas (decisão do dono; não bloqueiam os lotes L0/L1)

1. **[image_guard] GLOBAL** (L2.2): ligar `GlobalConfig.image_guard` no compose.rs:282 (substituindo o default de fábrica) ou remover a seção também? Default sugerido: remover — ninguém nunca leu.
2. **`Causa`** (L1.5): remover ou marcar como reserva da spec 17? Default: remover (git preserva).
3. **Split opcional de compose.rs** (L6.2): executar up.rs/live.rs após o classificar_de obrigatório? Default: não.
4. **Helper ssh** (L7.3): executar a fase 3? Default: sim, é barato e fecha o drift de timeout/auth.
5. **Propostas futuras candidatas** (registradas em AF-22, fora deste plano): transação real em `set_project_hosts`/`purge_project` (ou corrigir o comentário de state.rs:111); purge de `deploy_tokens`/`settings proxy_target/*` em purge_project; construtor único da fonte "Docker senão SQLite"; resolução async antes de `decide()`; template user-facing do config global (não existe — dado não encontrado).
