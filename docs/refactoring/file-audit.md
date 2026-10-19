# Auditoria por arquivo

Formato: responsabilidade → avaliação de coesão/acoplamento → achados com evidência `path:linha` → decisão → propostas vinculadas (com veredito do revisor independente). Arquivos sãos têm avaliação curta; problemáticos têm detalhe. Prioridades mostradas são as FINAIS (pós-revisor); onde o revisor divergiu do auditor proponente, registra-se "intenção → resultado".

Todas as evidências vêm de auditoria somente leitura (nenhum `cargo build/test` executado — proibido no lote de análise). Verificações independentes re-executadas nesta sessão de edição (todas somente leitura, `git grep`/`wc -l`): `Busy` em sleep.rs (2 ocorrências: :37 declaração, :255 braço); `PROJECTS_ROOT_PADRAO` (temporary.rs:25 def + temporary.rs:40 interno + 4 consumidores: api.rs:878, ciclo.rs:172, compose.rs:260, deploy.rs:361); `image_guard` (config.rs:112 e :323 declaradas, config.rs:949 é TESTE, flux.toml.example:43); `sha2` no flux-common (só Cargo.toml:11); `mod sync;` em main.rs:13; `branch_exists` stubs em docker.rs:290 e sqlite_source.rs:59; `wc -l` de main.rs (2574), panel.rs (1985), lib.rs (197), compose.rs (1383) — consistentes com o inventário (±1, newline).

---

## crates/flux-cli

### FA-01 · `src/main.rs` — 2575 linhas — DIVIDIR (P2)

**Responsabilidade:** binário `flux`: árvore clap (Cli/Cmd + 9 sub-enums, main.rs:42-315), dispatch (317-445) e implementação de TODOS os domínios de comando — ciclo de dev via API, tokens via ssh/sqlite, sync, build, watch, .env, live — mais 16 testes inline (2328-2574).

**Avaliação: MÚLTIPLA de verdade.** Serviços de domínio inteiros moram no bin, fora do papel de handler: watch com reconstrução de spec de sessão (tick_watch/rescan_sessao/spec_da_sessao, 1828-2006), pipeline de overlays tar→ssh (897-948), transferência de .env (2014-2155), toggle do live com cirurgia no flux.toml (2166-2320), build ssh com teto e kill (1749-1797), gravação direta no SQLite da VM (token_generate 568-617, project_token 639-688), pipeline do up com polling de 600 s (697-882). O crate já adota módulo-por-domínio (api/sync/tunel/wizard); main.rs é o despejo do que sobrou.

**Achados:**
- Prefixo ssh `['-o','BatchMode=yes','-o','ConnectTimeout=8', alvo]` colado em 9 call sites (576, 656, 914, 1756, 2023, 2057, 2100, 2205, 2250); alvo montado inline com `tunel::VM_USER/VM_HOST` em 571, 649, 920 apesar de `env_ssh_alvo()` existir (2014-2016).
- `sha256_hex` (512-520) repete o laço hex de `hex_de` em flux-cli/src/sync.rs:879-885; expressão de soma de "problems" do SyncReportIn repetida (1856-1860 vs 1937-1941); `ps()` reconstrói o nome `flux-{name}-{env}` à mão (1046-1057, 1103-1107).
- Defeito de digestão: doc de `token_generate` dividida entre 548-554 (colada acima de vm_ssh) e fragmento truncado "dar certo — falha no meio nunca deixa CLI…" em 566-567 — verificado por leitura pelo revisor.
- Arm "ps" morto em `ciclo_api` (1224-1227): único call site é main.rs:347 com "restart" — inalcançável. Menção, não fix (escopo da auditoria é análise).
- `spec_da_sessao` (1957-2006) reconstrói o contrato `flux-{project}-{slug}` que é do sync.rs (session(), sync.rs:322-327) — lógica de domínio da sync fora do módulo dono.

**Decisão:** dividir. Fase 1 extrai `contexto.rs` (Contexto 452-458, contexto_atual 460-510 com process::exit em 4 pontos, sha256_hex 512-520, parse_env 522-530, falhar 534-537, LEASE_UP_SECS 32); fase 2 extrai `comandos/{up,ciclo,logs,sync_cmd,build,watch,env_cmd,live,tokens}.rs` — main.rs sobra ~460 linhas (Cli/Cmd + main() + stub). Movimento verbatim, um módulo por commit.
**Propostas:** `foco-cli-contexto` (aprovado, P2) · `foco-cli-comandos-por-dominio` (aprovado, P2) · `foco-cli-ssh-helper` (aprovado, P3, opcional).

### FA-02 · `src/sync.rs` — 1287 linhas — DIVIDIR PARCIALMENTE (P2)

**Responsabilidade:** orquestração da sync contínua Windows→VM com o mutagen (spec 02): sessão (Beta/SyncSpec 147-381), execução (run 408-426), ciclo de vida create/pause/resume/terminate/reset (455-498), parse de status (504-727) e estabilidade de árvore git (729-929).

**Avaliação: MÚLTIPLA.** Duas responsabilidades sem nenhuma dependência entre si: a ponte mutagen e a estabilidade de árvore (zero linhas da seção 729-929 tocam o mutagen; erro em `String` sync.rs:747 vs `SyncError` na ponte). Evidência de atrito: o segundo consumidor do matcher saiu do domínio (filtro de migrations, main.rs:959-976); o git log concentra na fronteira os últimos 4 fixes (9faf85c, ba83621, f29ebfc, 44f7373); doc derivou dentro do arquivo (429-430 alega consumidor no CLI que o grep não acha).

**Achados:**
- `VM_USER`/`VM_HOST` (62-63) duplicam tunel.rs:18-19 com valores idênticos — enquanto `flux-common` trata o IP como configurável (`VmSection.vm_ip`, config.rs:501) e ninguém lê.
- `hex_de` (879-885) repete o laço hex de `sha256_hex` (main.rs:512-520).
- Restrição estrutural: `tests/sync_tests.rs:7-8` compila o arquivo standalone via `#[path]` e o arquivo tem ZERO referências a `crate::` (grep sem hits) — qualquer reestruturação preserva essa propriedade; exige formato directory/mod.rs.
- Montagem de ignores em dois pontos do mesmo arquivo: SyncSpec::ignores (331-339, com dedup) e listar_arquivos_rastreaveis (772-778, sem dedup).

**Decisão:** dividir em `sync/mod.rs` (ponte mutagen inteira) + `spec.rs` + `estabilidade.rs`. Ajuste do revisor: **spec.rs NÃO redeclara VM_USER/VM_HOST — importa `crate::tunel::{VM_HOST, VM_USER}`** (compatível com `cli-vm-constantes-fonte-unica`).
**Propostas:** `foco-sync-divisao-submodulos` (ajustar→incorporado, P2) · `cli-vm-constantes-fonte-unica` (aprovado, P3) · `foco-sync-doc-veraz` (aprovado, P3).

### FA-03 · `src/api.rs` — 1225 linhas — COESO (P3)

**Responsabilidade:** cliente HTTP da API de controle :7420: ApiError com exit code (33-84), espelhos Deserialize dos DTOs (86-284), transporte com timeout por rota (382-434), stream SSE sem timeout de corpo (565-619), parsing do contrato de exec (290-305), LeaseGuard (694-726) e suíte de contrato com mock axum (728-1224, 41% do arquivo).

**Avaliação: COESA.** Cliente, taxonomia de erro, DTOs e lease são uma responsabilidade só (falar o protocolo da 07). Ponto de risco protegido pela co-localização: `abrir_stream` (561-619) replica a montagem de request de `call_com_timeout` de propósito, SEM timeout de corpo — unificar os dois caminhos mataria o `flux logs -f` (invariante documentada em 561-564).

**Achados:**
- `CorpoErro` (88-91) espelha `ApiErrorBody` do flux-common (protocol.rs:119-122, Serialize-only) — duplicação DELIBERADA e documentada (86-95); vira simples quando o common ganhar Deserialize.
- DTOs In/Out espelham o protocolo por design (crate bin não depende do fluxd).
- `FreshIn` (196-200) sem método correspondente no ApiClient — main.rs serializa direto com call_raw (1252-1265): inconsistência do chamador, não deste arquivo.

**Decisão:** produção fica; só o bloco `#[cfg(test)]` (728-1224) sai para `src/api/tests.rs` (layout `foo.rs + foo/tests.rs`, válido sem virar mod.rs; acesso a privado do ancestral preserva SchemaIn e o campo `parar`).
**Propostas:** `foco-api-tests` (aprovado, P3).

### FA-04 · `tests/sync_tests.rs` — 670 linhas — PERMANECE (P3)

**Responsabilidade:** 24 testes da sync: pureza, parse JSON Lines real, tabela de status, ignores globais verbatim, 3 gated de sync real com `MUTAGEN_BIN`.

**Avaliação: COESA.** Ponte `#[path]` (7-8) é PERMANENTE — main.rs:13 já declara `mod sync;` (confirmado nesta sessão) e ainda assim o teste compila a cópia, porque flux-cli é crate só de binário (Cargo.toml:7-9, sem [lib]) e teste de integração não enxerga módulos de bin. Constraint real: src/sync.rs não pode referenciar itens `crate::` do main.rs sem quebrar o target de teste (quebra seria pega pelo clippy `--all-targets` do ci.yml:24 — não silenciosa). O doc do cabeçalho (2-5) alega o contrário.

**Achados:** doc falsa em dois pontos (ver testes-sync-bridge-doc); sem duplicação relevante (SESSAO_REAL :301 é fixture).
**Decisão:** permanece; corrigir doc do cabeçalho.
**Propostas:** `testes-sync-bridge-doc` (aprovado, P3).

### FA-05 · `src/tunel.rs` — 240 linhas — SÃO (P3)

**Responsabilidade:** acesso do CLI ao fluxd: túnel SSH `ssh -L` sob demanda (garantir_forward, 106-155) + seção `[api]` do config global do usuário (34-97).

**Avaliação: COESA.** As duas metades (config TOML e túnel ssh) servem o mesmo consumidor (`api.rs::from_env` usa as duas juntas, api.rs:340/350). Sem dependência de nenhum módulo do crate.

**Achados:**
- `VM_HOST`/`VM_USER` (18-19) redeclarados em sync.rs:62-63 — e o `flux-common` trata o IP como configurável (config.rs:501) sem leitor: host da VM em 3 lugares com fontes de verdade diferentes.
- `config_de` engole config.toml ilegível virando default silencioso (41-47) — contrato documentado (24-25) e pinado por teste (186-191), mas em tensão com "nada silencioso"; registrado, sem proposta.

**Decisão:** permanece; vira fonte única das consts de topologia.
**Propostas:** `cli-vm-constantes-fonte-unica` (aprovado, P3).

### FA-06 · `src/wizard.rs` — 190 linhas — SÃO COM BURACO (P3)

**Responsabilidade:** wizard interativo `flux up -m` (22-123): perguntas no stdin que produzem o flux.toml, recusando sobrescrever (24-30) e abortando sem escrever em validação falha.

**Avaliação: COESA, com um buraco real.** `health` (68-78) é o único string livre que vai para o TOML sem validação: wizard.rs:91 interpola `path = "{health}"` cru e grava sem verificação (120) — entrada com `"` ou `\` grava flux.toml INVÁLIDO; o próximo `flux up` falha no parse (main.rs:700-703) com erro longe da causa. Nome (38-48), framework (58-64) e TTL (95-105) são validados.

**Achados:**
- Regra "node/rust exigem health path" (72-78) duplicada do registro no fluxd (api.rs:232) e do comentário em config.rs:64-65 — o servidor é a fonte, o wizard repete para feedback cedo.
- Lista de frameworks duplicada: wizard.rs:60-62 reimplementa o match de framework.rs:26-35 — 4ª cópia da lista.
- `ManifestoGerado.toml` é `#[allow(dead_code)]` documentado como consumidor futuro (15) — anotação deliberada, não símbolo perdido.

**Decisão:** permanece; extrair `montar_manifesto` pura (89-106 → função privada) com validação/round-trip do TOML; adotar `Framework::from_str` e `flux_common::nomes::projeto_valido` (L2).
**Propostas:** `cli-wizard-toml-robusto` (aprovado, P3) · `common-framework-fromstr-no-wizard` (aprovado, P3) · adoção de `nucleo-nome-projeto` no wizard (junto de cli-wizard-toml-robusto).

---

## crates/fluxd

### FA-07 · `src/panel.rs` — 1986 linhas — DIVIDIR (P2)

**Responsabilidade:** painel web server-side: monta o Router axum autenticado (`build_panel` 156-176) e contém 4 camadas: wiring HTTP + guard (129-399), leitura SQL (401-586), formatação pura (588-813), renderização HTML (815-1241), mais 743 linhas de testes (1243-1985, 38%).

**Avaliação: MÚLTIPLA.** As seams já existem como comentários de seção (panel.rs:129, 401, 588, 815, 893, 1027, 1192, 1243) — o autor já gerencia o arquivo como se fossem módulos. Trajetória de crescimento por acréscimo (git log: 6 commits, cada um atravessa as camadas). Superfície externa é UMA função: `panel::build_panel` (lib.rs:121, router.rs:370); `PanelState` pub sem uso externo (grep vazio). Precedente do crate: observe_page.rs e panel_auth.rs nasceram como irmãos extraídos.

**Achados:**
- Sete itens copiados entre panel.rs, observe_page.rs e pages.rs: `esc` (592-597 = observe_page.rs:421 = pages.rs:200, byte a byte); `fmt_utc` + teste idêntico (618,1404 = observe_page.rs:430,1019); `agora_epoch` (518 = observe_page.rs:304); `NOINDEX`+`x_robots_tag` (46-50 = observe_page.rs:34-38); builder `resposta` (307 = observe_page.rs:129); `CSS` verbatim (63-127 = observe_page.rs:53-116); `layout` (820 ≈ observe_page.rs:391). O próprio comentário em 591 admite a cópia.
- Matemática civil de calendário (fmt_utc 618-633, dias_de_civil 637-645, epoch_de 651-670 — algoritmo de Hinnant) abrigada num módulo web.
- Import morto `use crate::decision::HostSource;` (36) — grep: zero usos. Menção de 1 linha.
- `badge_sync`/`linha_sync` (735, 759) conhecem o vocabulário "watching"/"absent" do sync.rs por `contains` — contrato frágil, documentado (731-734).

**Divergência resolvida:** o foco-painel apontou que a convenção dominante do crate é "arquivo grande único com mod tests" e que o split criaria exceção estilística. O revisor resolveu com evidência: tamanho (2º maior do crate) + crescimento por acréscimo + seams reais + grafo acíclico justificam; convenção nova é custo aceito e documentado. Intenção (dividir) → resultado (dividir, com essa ressalva registrada).
**Propostas:** `foco-panel-arvore` (aprovado, P2) · `foco-panel-esc-compartilhado` (aprovado, P3) · `foco-css-nucleo-compartilhado` (ajustar→incorporado, P2).

### FA-08 · `src/api.rs` — 1674 linhas — QUASE COESA (P2→P3)

**Responsabilidade:** API de controle :7420: `build_api` (46-81), gate Bearer sha256 fail-closed (83-174), handlers de projetos/lease/schema/switch/fresh/up/containers/sync (176-983), suíte e2e com bind real (985-1673).

**Avaliação: MISTA.** Handlers uniformes e rasos que compartilham `erro`/`toml_gravado`/`garante_projeto` — a tabela única em `build_api` é a propriedade de segurança "rota nova nasce atrás do gate". Defeitos pontuais de fronteira, não de estrutura.

**Achados:**
- **Cluster de auth deslocado:** TOKEN_KEY (40), auth (85-118), stored_token_hash (122-137), bearer_token (142-144), sha256_hex (148-156), hash_confere (164-170) — deploy.rs:56 importa as três últimas helpers `pub(crate)` de OUTRA superfície HTTP; os comentários de api.rs:139-170 admitem o recorte "SÓ para o handler de deploy".
- **Política do engine em handler:** derivação awake/waking do container dev (463-498) embute a regra F9 ("RUNNING só é awake com [health] respondendo") usando DockerSource/alvo_app/saude_ok/HostSource — usados SÓ nesse bloco (grep do foco-api-fluxd).
- Mesma causa 404 "não registrado" em três pontos (349, 417-418, 618) via três leituras distintas — mensagem repetida, não lógica pesada.
- Cobertura do ramo awake/waking: NENHUMA (testes sobem AppState sem Docker, api.rs:1013) — dado não encontrado; caracterização obrigatória antes do movimento (ver plano L5.4).

**Rejeições explícitas do auditor:** fatiar handlers por endpoint e mover mod tests para arquivo próprio — só contagem de linhas, não melhora manutenção.
**Propostas:** `foco-api-auth` (aprovado, P2→**P3** pelo revisor) · `foco-api-estado-dev` (aprovado, P3) · `nucleo-helper-erro` (aprovado, P3).

### FA-09 · `src/compose.rs` — 1384 linhas — DIVIDIR (P2)

**Responsabilidade:** hoje concentra 4-5 domínios: helpers do engine Docker Compose + override YAML (06), orquestração do `up` em background (`subir_projeto` 258-561), live server de dev Vite/hot (563-831), classificação de túnel (836-870) e **blue-green de prod com health probe** (872-1083).

**Avaliação: MÚLTIPLA — único candidato real a divisão no fluxd.** Evidência de domínio estranho: o doc do módulo (1-21) fala só de override/up/live e NÃO menciona o blue-green; nenhum símbolo do bloco 872-1083 é usado pelo resto do arquivo (trocar_cor usa só state/lista_do_projeto/esperar_saude/events/ProjectToml; subir_projeto não chama nada do bloco); consumidores são externos (api.rs:484, 717, 724); o contrato do trocar_cor está documentado num terceiro arquivo (deploy.rs:39).

**Achados:**
- `tunel_classificar` (836-870) roda `docker compose config --format json` uma SEGUNDA vez no mesmo up (o render já existe em 341) e funde I/O com a lógica pura de match (843-859, com block_in_place+block_on aninhado); o teste usa uma CÓPIA da regra (`classificar_de` no mod tunel 1346-1357; o teste admite em 1302-1304) — bug na regra real não seria pego.
- Remoção de hot órfão duplicada: compose.rs:753-777 vs router.rs:210-217 — dois pontos que mudam juntos.
- Gravações de estado pós-up engolidas com `let _ =` SEM log — set_env_state/set_proxy_target/touch_interaction (522-527), contra o padrão events.rs:19-21.
- Override YAML escrito com fs::write direto (420-431), sem escrita atômica — crash no meio deixa truncado; falha visível no compose up seguinte (aceitável, registrado).
- `lista_do_projeto` (1064-1083) reimplementa a listagem por label de docker.rs (ver FA-21).
- Regra "app ou primeiro service" duplicada: 540-544 vs docker.rs:50/70 (comentário 536-538 admite).

**Veredito do revisor (vinculante):** extração do blue-green APROVADA (`foco-compose-bluegreen`, P2). O split de diretório em 5 arquivos foi REBAIXADO — fragmentação fina: pós-bluegreen sobram ~1150 l., mesma faixa do observe.rs mantido único. Obrigatório e suficiente: extrair `classificar_de` pura no próprio arquivo e apontar o teste para a real; split up.rs/live.rs fica OPCIONAL e reduzido.
**Propostas:** `foco-compose-bluegreen` (aprovado, P2) · `foco-compose-arvore` (ajustar→incorporado, P2) · `dados-listagem-por-label` (aprovado, P2).

### FA-10 · `src/observe_page.rs` — 1284 linhas — PERMANECE (P2)

**Responsabilidade:** página /observe (monitor 24 h): parse de filtros GET, curvas SVG server-side, tabela de picos com snapshot, layout HTML — consumidor puro de `observe` (607-738 chama serie/series_por_container/peaks/peak_snapshot; zero SQL de métrica).

**Avaliação: COESA.** Um único símbolo público (`pagina`, 122); 764 linhas de produção (~120 CSS/doc, ~100 helpers de formatação). Padrão do crate é flat — dividir em diretório criaria convenção à parte sem fronteira nova.

**Achados:**
- A maior duplicação do lote, toda contra panel.rs: `esc` (421), `fmt_utc` + teste (430, 1019), `agora_epoch` (304), `NOINDEX`/`x_robots_tag` (34-38), `resposta` (129), `CSS` "verbatim" (53-116) e `layout` (391). O comentário 49-52 declara "cópia VERBATIM... mesma fonte de verdade visual" — **fato observado: falso**; diff executado mostra bloco comum idêntico (~49 l.) e divergência nos dois sentidos (panel-only: --attn/--attnbg, .sb*/.sync; observe-only: form.filtros, details pre).
- `render_monitor` (501-764, 264 linhas) mistura 4 seções com contratos próprios de honestidade (filtros 527-599, curva 601-634, drill 636-692, picos 694-761).
- Menção de 1 linha: `mesmo_filtro` (175-185) é dispensável — Metrica deriva PartialEq/Eq (observe.rs:100-104).
- Helpers de teste duplicados com panel.rs são isolamento deliberado documentado (768-773) — não propor extração.

**Propostas:** `foco-css-nucleo-compartilhado` (ajustar→incorporado, P2) · `foco-render-monitor-secoes` (aprovado, P3).

### FA-11 · `src/observe.rs` — 1106 linhas — PERMANECE (defer)

**Responsabilidade:** observabilidade de recursos (spec 13): sampler bollard 30 s → rollup 1-min (`metrics_1min`) → perícia de pico com snapshot de irmãos → retenção 24 h → consultas do painel.

**Avaliação: COESA — não há problema a corrigir.** Um domínio; tipos centrais (Amostra/Metrica/PeakRow) compartilhados por todas as fases e por observe_page.rs:31; partição alternativa seria por fase técnica, não por responsabilidade; 36% é teste embutido (706-1105); src/ do fluxd é flat (nenhum diretório de módulo no crate). Reuso correto: `now_epoch`/`cpu_percent` de sleep.rs (43, 597) — não reimplementa CPU. Schema de métricas PRÓPRIO (SCHEMA_OBSERVE 67-95) fora do state.rs — o schema do fluxd tem dois donos hoje (ver AF-13); registrado para a lente estrutural, sem proposta.

**Gatilhos de reavaliação documentados:** segunda fonte de coleta; face de leitura consumida por >1 página/API; regra de pico configurável por projeto.
**Propostas:** `foco-observe-manter` (aprovado, **defer**).

### FA-12 · `src/state.rs` — 1038 linhas — PERMANECE (defer)

**Responsabilidade:** estado persistente do fluxd: SQLite em WAL com integrity check, corrupção mapeada (StateError + backup), schema versionado v1..v4 com ladder de migração e métodos das tabelas núcleo (projetos, sono, hosts custom, cor blue-green, migrations de usuário, tokens de deploy).

**Avaliação: COESA — divisão rejeitada com evidência.** O inventário schema (19-119) ↔ `purge_project` (286-305) ↔ ladder (597-673) é um conjunto fechado auditável só no mesmo arquivo — comentários 292-298 provam que v3 e v4 exigiram editar purge junto (co-localização é load-bearing). Repartir métodos por domínio criaria módulos de 22-120 linhas sem unidade nova; a posse real do SQL de feature já está nos chamadores via `with` (sync.rs:63-190, deploy.rs:116-119, events.rs:15, observe.rs:153-306).

**Achados (registrados, sem proposta neste plano):**
- Doc de `set_project_hosts` (111) afirma "transação", mas NÃO há BEGIN/COMMIT — cada execute autocomita (315-327); atomicidade vem só do mutex (133-135, 190-196); crash no meio de set_project_hosts ou purge_project (7 DELETEs) deixa estado parcial. FATO; correção (unchecked_transaction ou correção do comentário) é proposta futura do dono.
- `migrate` (597-673) não wrapa em transação: queda no meio do ladder deixa `schema_version` unset e a próxima abertura reexecuta o ladder idempotente — self-healing por design (inferência rotulada); qualquer refactor futuro preserva.
- `purge_project` apaga 7 tabelas mas NÃO apaga `deploy_tokens` nem as linhas `settings proxy_target/{name}/*`, apesar do doc dizer "TODA a trilha" (285) — intencionalidade: dado não encontrado (chamadores em temporary.rs não lidos neste ask).
- Doc velha: `set_proxy_target` (237-238) diz "testes; F3 troca" — compose.rs:524 usa em produção.
- FK não é usada apesar de `foreign_keys=ON` (170-171).
- Chave `proxy_target/{name}/{env}` com formato implícito em dois arquivos (escrita state.rs:244, leitura sqlite_source.rs:81) — acoplamento por convenção.

**Propostas:** `foco-state-manter` (aprovado, **defer**).

### FA-13 · `src/ciclo.rs` — 997 linhas — PERMANECE (P3)

**Responsabilidade:** handlers do ciclo de dev na API de controle: down (compose stop), logs (tail/SSE), exec (docker exec com teto 256 KB e timeout), com contrato CLI↔fluxd documentado (6-29) e suíte extensa.

**Avaliação: COESA.** ~420 linhas são teste; domínio único; arquivo grande porém coeso permanece.

**Achados:**
- `fn erro` (87-89) copiada da api.rs:172 — o comentário 85-86 ADMITE ("o helper de lá não é pub") e avisa que parsers do CLI dependem do formato `{"error": causa}`: duas cópias vivas de um contrato de fio.
- `projeto_registrado` (129-141) espelha `garante_projeto` (api.rs:613-625) — revisor comparou os corpos: mesmos status 404/500 e mensagens.
- `rodar_exec` (375-458) duplica o esqueleto bollard de exec com `exec_in_container` (db.rs:367-422) — contratos já divergiram (teto/exit -1 aqui; sem teto lá). Ver FA-18/FA-21.

**Propostas:** `nucleo-helper-erro` (aprovado, P3) · `dados-exec-container-unificado` (aprovado, P2, risco médio).

### FA-14 · `src/deploy.rs` — 918 linhas — PERMANECE (defer)

**Responsabilidade:** rota pública de deploy: gate sequencial (método → rate limit por projeto → registro → auth Bearer por projeto fail-closed → corpo → concorrência) e pipeline de fundo (git fetch + guard de descendência + release-dir imutável via worktree) com trilha deploy_log/events.

**Avaliação: COESA.** Gate e pipeline são fases do mesmo fluxo; recorte cenário B documentado no topo (34-42); testes cobrem os dois lados com git real em tempdir (852-916).

**Achados:**
- `deploy.rs:56` importa `{bearer_token, hash_confere, sha256_hex}` de api.rs — módulo dono errado para primitivas compartilhadas (ver FA-08; resolvido por `foco-api-auth` com re-export, deploy.rs intocado).
- `EM_VOO` é estado global de processo (`static OnceLock<Mutex<HashSet>>`, 91-95); testes reservam nomes únicos (713-714; deploy_integration.rs:127-129). Pertenceria ao AppState como os outros limiters — registrado, sem proposta.
- Fluxo de gate similar ao da api.rs (162-237 vs 85-118): similaridade semântica com semânticas distintas (tokens por projeto), não cópia literal.

### FA-15 · `src/edge.rs` — 910 linhas — PERMANECE (defer)

**Responsabilidade:** camada de admissão tower: teto de body 413, token bucket de rate por host 429 + Retry-After, semáforo de concorrência com fila curta, respostas HTML/JSON brandiadas.

**Avaliação: COESA.** Três camadas de admissão + respostas = uma preocupação (a borda). Self-contained de propósito (41-43): não lê AppState/SQLite/Docker. `EdgeState` reutilizado pelo deploy como limiter próprio via AppState.deploy_limiter (lib.rs:53, 65-69) — reuso saudável.

**Achados:**
- CSS + `html_error_page` duplicam o croma de pages.rs byte a byte (537-563 vs pages.rs:12-17, 19-56 — md5 igual verificado por execução); documentado como cópia local porque as consts de pages são privadas (533-536). Unificação: `rede-paginas-croma-compartilhado`.
- `host_key` (394-417) tem tratamento IPv6 próprio, documentado (387-393), que o router/beat NÃO têm — normalização de Host com 3 semânticas (ver AF-09).
- Reusa `decision::wants_json` (329) — acoplamento intencional: borda e decisão nunca discordam sobre quem é API.

**Avaliação declarada "leve" pelo auditor-rede** (lente única; leitura com citações de linha). Propostas mínimas — cobertura registrada como tal no coverage-manifest.

### FA-16 · `src/waf.rs` — 905 linhas — PERMANECE (defer, com nota de segurança)

**Responsabilidade:** WAF local layer tower: detecção substring (SQLi + path scan) sobre path+query decodificados, resolução de IP real (cf-connecting-ip → XFF → ConnectInfo), allowlist de rede fail-closed, 403 brandiado JSON/HTML.

**Avaliação: COESA.** Detecção pura (202-266), ip/allowlist (148-195) e layer/service = mesma preocupação.

**Achados:**
- **Nota de segurança (FATO no código, registrado sem proposta):** `cf-connecting-ip` vence TUDO quando parseia (174-179); um cliente com HTTP direto ao Caddy pode forjar `cf-connecting-ip: 192.168.140.x` e entrar na allowlist (152-161). A exploração é INFERÊNCIA condicionada à topologia — binds 127.0.0.1 (lib.rs:172-174) e cloudflared sobrescreve o header no túnel (170-178). Premissa de rede mora fora do código; ver AF-20.
- CSS + página 403 espelham o croma de pages.rs/edge.rs (419-448, md5 igual verificado); o JSON de erro manual espelha o padrão do edge (394-415 vs edge.rs:509-519) — mesmo desenho documentado (440-442).
- Percent decode (214-231) duplica observe_page.rs:196-221 com semântica levemente distinta — registrado (AF-07 do mapa), sem proposta.

**Avaliação declarada "leve" pelo auditor-rede** (idem edge.rs).

### FA-17 · `src/decision.rs` — 761 linhas — PERMANECE (P2, higiene)

**Responsabilidade:** máquina de decisão pura do router: Host + path + estado do container → Action, via trait `HostSource` injetável (76-90), sem rede nem Docker.

**Avaliação: COESA — fronteira EXEMPLAR** (veredito do auditor-nucleo). ~310 de implementação + ~450 de teste; um domínio só (a matriz da spec 03). VITE_PORT/VITE_PATH_PREFIXES/eh_path_vite (168-185) moram aqui de propósito: mover para compose/infra INVERTERIA a dependência (núcleo puro dependendo de infra) — rejeitado.

**Achados:** validação de nome `[a-z0-9-]` inline em `rota_deploy` (197-204), repetida em temporary.rs:30-35 e api.rs:201-205 (+ main.rs:624-629 e wizard.rs:38-48 no CLI) — unificar em flux-common (`nucleo-nome-projeto`, aprovado, P2; contrato de fio compartilhado entre registro e roteamento; variante de host com '.' em config.rs:479-485 é OUTRO contrato e fica).

### FA-18 · `src/db.rs` — 555 linhas — PERMANECE + RENOMEAR (P2/P3)

**Responsabilidade:** pipeline de migrations/seed do projeto: diff por hash, decisão dev (Reconstruir/Migrar/Nada/Pulado) vs prod (mutada bloqueia), execução via docker exec no container app, fresh forçado de dev.

**Avaliação: COESA.** ~85 de teste; uma preocupação (ciclo de schema). O nome é o problema: db.rs NÃO é acesso a banco — quem fala com o SQLite de estado é state.rs; num crate com state.rs, sqlite_source.rs e observe.rs (schema próprio), "db" sugere o módulo errado.

**Achados:**
- `exec_in_container` (367-422): infra genérica de docker exec que `rodar_exec` (ciclo.rs:375-458) reimplementa com workdir e teto — contratos JÁ divergiram: db.rs embrulha em `sh -c` (386), sem workdir e SEM TETO (395-410 acumula em memória); ciclo.rs usa args verbatim, workdir do inspect, teto 256 KB e regra exit -1-while-running (423-456).
- Erros como `Result<_, String>` (126, 435) — sem tipo estruturado; coerente com ciclo.rs, mas a causa só existe como texto na borda da API (ver AF-06).

**Propostas:** `dados-renome-db-migrations` (aprovado, P3 — `pub mod db` → `pub mod migrations`; nenhum crate consome `fluxd::`, verificado por grep) · `dados-exec-container-unificado` (aprovado, P2, risco médio — ajuste do revisor: ExecSpec preserva as diferenças deliberadas: `sh -c`, workdir, exit -1, teto).

### FA-19 · `src/proxy.rs` — 537 linhas — PERMANECE (P3)

**Responsabilidade:** forward L7 streaming sem buffering, túnel websocket via upgrade (copy_bidirectional), e a única exceção de buffering: injeção do overlay de sono em resposta elegível (200 text/html sem encoding, < 256 KB).

**Avaliação: COESA.** Streaming + túnel ws + exceção de buffering = mesma preocupação L7.

**Achados:**
- Montagem do request upstream duplicada entre `forward` (133-158) e `forward_ws` (205-229): ~20 linhas gêmeas; única divergência deliberada é o strip_hop_by_hop (145 presente; 190-193 explica a ausência no ws — Connection/Upgrade SÃO o pedido). Hoje a proteção é disciplina, não estrutura: `rede-proxy-montagem-request` (aprovado, P3) extrai helper privado.
- Infra→feature: o forward injeta `crate::beat::SCRIPT` (106; toggle INJECT_BEAT 27) — o proxy L7 conhece o overlay do sono. Inverter (beat injetando no proxy) criaria acoplamento pior; registrado como AF-11, sem proposta neste plano.

### FA-20 · `src/router.rs` — 528 linhas — PERMANECE (defer)

**Responsabilidade:** monta o router catch-all :8080 (beat + fallback + camadas Edge/WAF) e despacha cada `decision::Action` para proxy, páginas, painel, deploy, robots/sitemap, com rastro comum (noindex em dev 315-318; access log 320-327).

**Avaliação: COESA.** 527 linhas com ~172 de teste; o núcleo é a montagem (33-52) + um único despacho `handle` (127-329) que materializa a matriz da 03 — quebrá-lo separaria um fluxo que se lê de cima a baixo.

**Achados:**
- Enum `Source { Docker, Sqlite }` (72-112) implementa HostSource por delegação — o mapa de dependências chamou de "3ª implementação redundante"; o auditor-rede divergiu e o revisor acatou: é adaptador de lifetime (DockerSource possui Arc, SqliteSource empresta &State) — **mantido como está**, registrado como divergência resolvida (intenção do mapa vs resultado).
- `logo_do_projeto` (337-344) é branding, não roteamento; posição atual é o menor mal (mover para pages.rs sujaria a pureza da página com State). Borderline mantido.
- `live_ativo` + limpeza de hot órfão (117-125, 207-215) é política do live dentro do router, documentada como intencional (gate por Option<state_dir>) — mantida (ver FA-22).
- Extração de host (131-137) sem o tratamento IPv6 do edge.rs:394-401 — normalizações divergem de propósito (o decide passa o host cru; ManagedHost::parse normaliza em host.rs:60-61); divergência latente IPv6 registrada no AF-09.
- Micro: host_env 2× (241, 248); DockerSource construído 2× (142-145, 157) — não pagam mudança.
- `x_robots_tag`/`NOINDEX` (54-56, 68) — TERCEIRA cópia, achada pelo revisor no ajuste de `foco-css-nucleo-compartilhado` (o caminho R22 de todo host dev).

### FA-21 · `src/docker.rs` — 395 linhas — REFORMAR POR DENTRO (P2)

**Responsabilidade:** adaptador Docker: convenção de compose project, descoberta por label, tradução inspect→ContainerState, alvo de proxy, ações wake/pause/unpause/sleep, DockerSource (HostSource híbrido).

**Avaliação: COESA em domínio, porosa em implementação** — P1 na intenção do auditor-dados, P2 no resultado final do revisor. O que precisa é consolidar listagem/exec DENTRO dele, não quebrar o arquivo.

**Achados:**
- Listagem por label `com.docker.compose.project` escrita 4× (aqui :34-48 e :54-62; compose.rs:1069-1073; temporary.rs:178-186) e mais cópias em observe.rs:539-556 e api.rs:787-814 — tratamento de erro já divergiu para pior: `unwrap_or_default()` (47) faz wake/sleep agirem como lista vazia com sucesso; `.ok()?` (63) não distingue "sem container" de "engine falhou".
- SQL de projects/active_color copiado do sqlite_source com comentário "mesmo código do SqliteSource" (321) e ENGOLE erro SEM log: `.is_ok()` (327), `unwrap_or((false,false))` (339) — contra os tracing::error! de sqlite_source.rs:33-36, 106-109. Revisor achou MAIS uma cópia não listada: `host_custom` (297-305 ≡ sqlite_source.rs:67-75) — incluir na delegação.
- `compose_project_cor` engole Err de active_color (172-174 `let Ok(Some(cor))`).
- `create_dummy_container` (346-381): helper de TESTE (único chamador: teste gated ciclo.rs:841) em código de produção sem `#[cfg(test)]` — puxa imagem alpine.
- `block_in_place`+`block_on` dentro dos métodos síncronos do trait (281-287, 308-314) — exige runtime multi_thread (main.rs:56 garante hoje; nenhum guard impede current_thread). Ver AF-10.

**Propostas:** `dados-listagem-por-label` (aprovado, P2 — ajuste do revisor: duas formas: match exato e variante "qualquer label" para o temporary) · `dados-sqlite-reuso-em-dockersource` (aprovado, P1→**P2**, inclui host_custom) · `dados-exec-container-unificado` (aprovado, P2) · `foco-api-estado-dev` (aprovado, P3 — recebe a política awake/waking).

### FA-22 · `src/temporary.rs` — 391 linhas — PERMANECE (P3)

**Responsabilidade:** dev temporário (14): expire_check de TTL por ociosidade e teardown ordenado (containers compose → diretórios → SQLite por último), defesa de nome seguro, falhas visíveis.

**Avaliação: COESA.** Módulo coeso (expiração + teardown com ordem documentada e testada).

**Achados:**
- `PROJECTS_ROOT_PADRAO` (25) — constante de layout da VM morando no módulo de dev temporário, consumida por ciclo.rs:172, api.rs:878, compose.rs:260, deploy.rs:361 (confirmado por `git grep` nesta sessão).
- Doc drift: temporary.rs:23-24 promete "o serve passa o default", mas serve chama `expire_check` com a constante direto (lib.rs:155; temporary.rs:40) — só testes injetam raiz via `expire_check_em`.
- Segunda fonte de verdade: compose.rs:601-608 (raiz_arvore) e router.rs:117-122 (live_ativo) derivam a MESMA raiz de state_dir. **Divergência resolvida pelo revisor:** a derivação é gate DELIBERADO de "estou na VM" (router.rs:114-119) — unificar mudaria semântica; a divergência com `--db` custom é latente e sem impacto no layout real. Fase 2 da proposta (helper de derivação) DESCARTADA.
- `nome_seguro` (30-35) repete o predicado `[a-z0-9-]` — resolvido por `nucleo-nome-projeto`.

**Propostas:** `nucleo-raiz-projetos` (ajustar→reduzido, P2→**P3**) · `nucleo-nome-projeto` (aprovado, P2).

### FA-23 · `src/sleep.rs` — 353 linhas — PERMANECE (P3)

**Responsabilidade:** política de sono: decisão pura `should_sleep` (42-71), limites do flux.toml vs global (`limits_for` 84-99), SleepManager com sweep de 60 s (re-sync do Docker, cooldown, lease, guarda de CPU 226-231) e `cpu_percent`.

**Avaliação: COESA.** Decisões puras separadas do manager I/O.

**Achados:**
- `SleepDecision::Busy` (37) é variante MORTA: `should_sleep` só constrói KeepAwake/Held/Sleep; o braço `Busy => ...CPU alta — acordado` (255) é inalcançável — confirmado nesta sessão: as 2 únicas ocorrências no arquivo são a declaração e o match. A guarda real de CPU acontece FORA da decisão pura, em sweep_one (226-231). O enum mente sobre o desenho.
- `now_epoch` (22-27) é util genérico morando no domínio sono (usado por api.rs:606,1207 e observe.rs:43) — aceitável; mover geraria churn (lib.rs:151-154 que o reimplementa inline É corrigido por `nucleo-sweep-lifecycle`).

**Propostas:** `nucleo-busy-morto` (aprovado, P2→**P3**) — seguro no workspace: fluxd lib não tem consumidor fora do crate (grep `fluxd::` vazio em flux-cli/flux-common).

### FA-24 · `src/pages.rs` — 338 linhas — PERMANECE + CONSERTO DE ESCAPE (P2)

**Responsabilidade:** geração das respostas brandiadas: páginas de espera/falha/branch/404, 503 JSON de API, robots.txt e sitemap.xml, família visual Flux com Cache-Control no-store (58-60, 174, 194).

**Avaliação: COESA** — o defeito real é de escape, não de estrutura.

**Achados:**
- **Reflected XSS latente por interpolação sem escape de dado do Host:** `branch_missing` injeta `branch` e `name` crus (108-113); `failed` (88-98) e `wait` (64-84) injetam `name` cru em body/heading/title. `branch` vem do prefixo do Host SEM validação de charset (host.rs:107-115 aceita bytes arbitrários); `not_found` JÁ escapa o host (130) — contrato inconsistente no mesmo módulo. Caminho morto hoje: `branch_exists` devolve true sempre (docker.rs:290-293, sqlite_source.rs:59-59 — stubs confirmados nesta sessão) e `name` é gateado pelo registro (api.rs:201-205, host.rs:98,119); F11 (`flux watch`) liga branch_exists real e a página passa a renderizar string do atacante. Revisor: P1→**P2** (defesa, não incidente) — mas pré-requisito ANTES de F11.
- Dono do croma: CSS (12-17) e `html_escape` (200-205) viram `pub(crate)` e edge/waf/panel importam (`rede-paginas-croma-compartilhado`, aprovado, P3).

**Propostas:** `rede-pages-escape-interpolacao` (aprovado, P1→**P2**) · `rede-paginas-croma-compartilhado` (aprovado, P3) · `foco-panel-esc-compartilhado` (aprovado, P3).

### FA-25 · `src/panel_auth.rs` — 306 linhas — PERMANECE (P3)

**Responsabilidade:** primitivas de auth do painel: sessões em memória (SessionStore), rate limit de login por IP (LoginLimiter), verificação Argon2id fail-closed do hash lido de disco a cada login, montagem/parsing do cookie — sem handlers HTTP (que ficam em panel.rs).

**Avaliação: COESA.** Testes de caracterização fortes embutidos (212-305: sessão/TTL, rate 5/min por IP, fail-closed de hash ausente, parse de cookie, atributos do Set-Cookie). A separação handler/auth que a lente pede já existe.

**Achados:** `hash_path` é `pub` (126) sem nenhum leitor externo — grep `.hash_path` no workspace só acha o uso interno em `verificar` (147); superfície pública sem usuário permite consumidor futuro ler/trocar o caminho do segredo por fora do fluxo fail-closed. `painel-auth-hash-path-privado` (aprovado, P3): uma palavra, zero risco.

### FA-26 · `src/sync.rs` — 306 linhas — PERMANECE (defer)

**Responsabilidade:** estado de sync (mutagen) no fluxd: upsert do relato do watch, leitura para painel/CLI, fila de rescan por projeto na tabela sync_status.

**Avaliação: COESA.** Uma tabela, três operações; tipos SyncRow/SyncReport (18, 35) são os contratos de fio, no lugar certo. Acoplamento por design e saudável (consumidores: api.rs:915-968, panel.rs:360,735,759,926).

**Achados (menores, sem proposta):** `request_rescan` checa registro com `SELECT 1 FROM projects` (170-180) enquanto api.rs `garante_projeto` usa list_projects (613-625) — mesma intenção por caminhos diferentes; tradução de erro artesanal com prefixo "sync: " em strings (69-74, 110-115, 199) em vez de tipo (ver AF-06).

### FA-27 · `src/guard.rs` — 262 linhas — PERMANECE (defer)

**Responsabilidade:** guard de imagens antes de pull/build: varredura de `image:` em compose e `FROM` em Dockerfile contra a lista bloqueada, produzindo Violation com arquivo/linha/imagem/padrão (79-136).

**Avaliação: COESA.** Funções puras com testes de caracterização embutidos (138-261); limitações documentadas honestamente (5-9). Único consumidor: compose.rs (183, 289-292, 1108, 1113). Nota do auditor: o módulo NÃO é rede — é política de build/deploy; posicionamento no fluxd correto.

### FA-28 · `src/lib.rs` — 198 linhas — EXTRAIR LOOP DE SWEEP (P2)

**Responsabilidade:** AppState compartilhado (36-72), const PANEL_PORT e composition root `serve()` (abre SQLite, conecta Docker, monta painel/router/api, spawna tasks, bind dos 3 listeners, try_join).

**Avaliação: MÚLTIPLA (parcial).** AppState na raiz é o hub de todos os módulos — aceitável (idiom axum `crate::AppState`), sem ciclo. O defeito é o loop de sweep embutido no composition root (130-158): intervalo 60 s, primeiro tick pulado (136), SleepManager por tick (140-146), expire_check sempre (155), epoch inline (151-154) duplicando sleep::now_epoch (22-27, que api.rs e observe.rs já importam) — enquanto observe.rs usa o padrão encapsulado `iniciar` com spawn próprio (493, 510). lib.rs NÃO tem mod tests — "primeiro tick é pulado" e "sem Docker o teardown ainda roda" estão sem rede.

**Achados adicionais:** AppState conhece constantes do FEATURE deploy (`deploy_limiter` com DEPLOY_RATE_PER_MIN/DEPLOY_BURST, 65-69) — núcleo acoplado a feature folha; registrado como AF-11, sem proposta neste plano.

**Proposta:** `nucleo-sweep-lifecycle` (ajustar→incorporado, P1→**P2**). Ajuste do revisor: `iniciar(shared)` não compila como escrito — o loop consome GlobalConfig (clonado em lib.rs:133; SleepManager::new 140-144) e AppState não o carrega. Assinatura correta: `pub async fn iniciar(shared: Arc<AppState>, global: GlobalConfig)`; serve passa `global.clone()`.

### FA-29 · `src/beat.rs` — 178 linhas — PERMANECE (defer)

**Responsabilidade:** handler POST /__flux/beat (zera timer de ociosidade por host gerenciado) + SCRIPT JS do overlay sendBeacon.

**Avaliação: COESA.** Handler + SCRIPT + testes, um propósito.

**Achados (menores):** assimetria de seleção: `project_exists` escolhe Docker/Sqlite (46-49) mas `host_custom` vai sempre de SqliteSource (50) — quando DockerSource::host_custom (docker.rs:297-303) já delega ao SQLite; comportamento idêntico, seleção duplicada. Mesma seleção "engine senão SQLite" do router e do painel (AF-08 do mapa). Testes usam AppState com Docker None (80-83).

### FA-30 · `src/sqlite_source.rs` — 113 linhas — PERMANECE (P2)

**Responsabilidade:** implementação de `decision::HostSource` que lê o SQLite de estado para o request path do router, degradando com log em erro (tracing::error! 33-36, 106-109).

**Avaliação: COESA.** Um adaptador só, com degradação logada por consulta — o padrão que docker.rs deveria seguir.

**Achados:** acoplamento por convenção de chave: `proxy_target/{name}/{env}` (81) é um formato que só o escritor state.rs:244 conhece — sem símbolo compartilhado (ver FA-12). `branch_exists` stub = true (59-64) duplicado com docker.rs:290-293 (ambos citam F11) — dois lugares para atualizar quando o registro de branches chegar (AF-05). Queries copiadas em docker.rs:321-341 voltam a ter um único dono com `dados-sqlite-reuso-em-dockersource`.

### FA-31 · `src/main.rs` — 88 linhas — PERMANECE (defer)

**Responsabilidade:** binário fino: parse CLI com clap, init de tracing, runtime tokio, delegação para fluxd::serve.

**Avaliação: COESA — fino como o doc-comment declara (1-3).** O enum CtlCmd com 4 variantes não implementadas (39-48) é superfície declarada com stub explícito e exit 2 (80-85) — intencional ("nada silencioso"), agendado F5-F6, não código deslocado.

### FA-32 · `src/events.rs` — 23 linhas — PERMANECE (defer)

**Responsabilidade:** gravação de eventos no SQLite (trilha geral + access log), com falha visível em tracing::error (19-21) — o padrão "nada silencioso" que vários módulos citam como referência e outros violam.

### FA-33 · `tests/integration.rs` — 227 linhas — PERMANECE (P2)

**Responsabilidade:** 9 testes da cadeia decisão→proxy→páginas do router sobre HTTP real (upstream dummy + build_router completo), sem VM nem Docker.

**Avaliação: COESA.** Usa só a superfície pública; sockets efêmeros (34-48).

**Achados:** R22 ("toda resposta de dev leva noindex", aplicada em router.rs:315-318) só é caracterizada no caminho de proxy (100-104); página de espera (131-146), 503 JSON (148-165) e robots.txt (167-178) NÃO assertam o header — regressão que condicione o insert passa verde. Helpers duplicam ~40 linhas de deploy_integration.rs:63-123 — dedup opcional, não bloqueia.
**Proposta:** `testes-noindex-header-toda-resposta-dev` (aprovado, P2).

### FA-34 · `tests/deploy_integration.rs` — 313 linhas — PERMANECE (defer)

**Responsabilidade:** 4 testes da rota pública de deploy: auth fail-closed (401/403), rate próprio (429 no 11º), 409 concorrente, guard de descendência com git real em tempdir.

**Avaliação: COESA.** Guard EM_VOO estático de processo contornado com nomes únicos (:129, :203) — conduta correta em testes paralelos. Cenário 4 (:269-312) e helper git() (:244-260) duplicam verbatim unit tests de deploy.rs:831-900 — defesa em profundidade barata: manter, não extrair.

### FA-35 · `tests/state_migration_integration.rs` — 179 linhas — PERMANECE (P3)

**Responsabilidade:** migrador de schema: banco v1 real em ARQUIVO sobe para a versão do binário sem perder dado; schema_version adulterada é recusada.

**Avaliação: COESA.** Fixture SCHEMA_V1_FIXTURE (:19-73) é cópia MANUAL do const privado (state.rs:19-73) — conferida verbatim hoje; se o DDL histórico for editado, a cópia diverge em silêncio (o risco é do arquivo de integração; a caracterização autoritativa fica nos unit tests de state.rs:815+ que usam o const REAL). Valor real: WAL/integrity_check só rodam com path. Doc velha: 2-4 e 17-77 ainda falam "v2"/"tabela NOVA da v2" — SCHEMA_VERSION é 4 (state.rs:14); 2 linhas de comentário num cleanup qualquer.

### FA-36 · `tests/schema_integration.rs` — 115 linhas — PERMANECE (P1→via CI)

**Responsabilidade:** `ensure_schema` contra engine real: migrate executando DENTRO do container (prova por arquivo) e bloqueio de migration mutada em prod.

**Avaliação: COESA.** Exec direto via bollard no dbg_probe (:80-114) é recorte documentado (:77-79). **Passa vazio no CI** (retorna cedo sem FLUX_DOCKER_URL, 12-19) — resolvido pelo Lote 0 (`testes-ci-gates-docker`).

### FA-37 · `tests/sleep_integration.rs` — 83 linhas — PERMANECE (P1→via CI)

**Responsabilidade:** sweep do SleepManager contra engine real: cooldown pós-wake, cooldown expirado + idle dorme com stop, re-sync do Docker.

**Avaliação: COESA.** Usa WAKE_COOLDOWN_SECS público (sleep.rs:29). **Passa vazio no CI** (14-21) — resolvido pelo Lote 0.

### FA-38 · `tests/docker_integration.rs` — 78 linhas — PERMANECE (P1→via CI)

**Responsabilidade:** ciclo completo do DockerSource contra engine real: Unknown→Starting→Awake com proxy_target, pause/unpause, sleep, limpeza defensiva (30-35 — correto para engine compartilhado).

**Avaliação: COESA.** O próprio doc (:5) prevê "CI pode injetar o engine quando existir" — e nada injeta (14-17). Resolvido pelo Lote 0.

---

## crates/flux-common

### FA-39 · `src/config.rs` — 965 linhas — PERMANECE (P2)

**Responsabilidade:** schemas TOML de projeto (ProjectToml, 16 seções) e global (GlobalConfig, 12) com defaults da plataforma, validação estrita deny_unknown_fields, validação de hosts custom.

**Avaliação: COESA.** Tema único (schemas + defaults + validação); ~340 de teste co-localizado; consumidores estáveis via `flux_common::config::*`. Divisão project/global não se justifica.

**Achados:**
- **`[db].image_guard` chave morta:** DbSection.image_guard (112) declarada e documentada no template (flux.toml.example:41-43, "Default: true"), ZERO leitores (grep em fluxd/flux-cli: vazio — confirmado nesta sessão). Viola o princípio do próprio arquivo (3-4, "nada de typo silencioso"): usuário declara `image_guard = false` esperando desligar o guard e nada muda. **Correção de fato do revisor, maior que a proposta:** o guard real NEM lê a global — compose.rs:282 usa `ImageGuardSection::default().block_list()`; a única leitura da seção global é um teste (config.rs:949). Hoje a global `[image_guard]` também é "aceita e ignorada" em produção. Decisão do dono: ligar a global no compose.rs:282 OU removê-la — registrar no mesmo escopo.
- `hosts_custom` (439-494): regras de host gerenciado dentro do arquivo de schemas — o MESMO prefixo `vite-` é interpretado na leitura em host.rs:85 e o sufixo de dev em host.rs:92; regra partida em dois módulos (ver FA-40).
- Duplicação interna menor, não proposta: parse_str/load idênticos em ProjectToml (41-57) e GlobalConfig (335-351).

**Propostas:** `common-db-image-guard-chave-morta` (aprovado, P2 — quebra de parse DELIBERADA e barulhenta, coerente com deny_unknown_fields) · `common-hosts-regras-concentradas` (aprovado, P3).

### FA-40 · `src/host.rs` — 313 linhas — PERMANECE (defer/P3)

**Responsabilidade:** modelo de hosts gerenciados (ManagedHost) e decodificação pura do header Host em painel/prod/dev/vite/custom/unknown, com resolução por sufixo mais longo.

**Avaliação: COESA.** Modelo + parse puro + 12 testes num único assunto. Depende só de config::DomainConfig (5); fluxd injeta is_project/host_custom como closures (beat.rs:51, decision.rs:108) — fronteira limpa.

**Achados:** regra do prefixo `vite-`/dev_suffix validada na escrita em config.rs:467-478 e interpretada na leitura aqui (85, 92-93) — mudar um lado sem o outro cria host que valida no registro mas não roteia na borda. `common-hosts-regras-concentradas` (aprovado, P3) move a validação para cá com ProjectToml::hosts_custom como delegação de 1 linha (chamador api.rs:239 intocado). Menção: `ManagedHost::parse` sem consumidor externo (só recursão interna 87 e testes) — manter (doc 44-46), ganho marginal.

### FA-41 · `src/protocol.rs` — 152 linhas — PERMANECE (defer)

**Responsabilidade:** tipos do protocolo CLI↔API :7420: Env, ContainerState, corpos fixos de 503/health/erro.

**Avaliação: COESA.** Env/ContainerState SEM serde é deliberado (string no fio, FromStr na borda — api.rs:548, 570, 669); ProjectSummary é o único Serialize+Deserialize (desserializado pelo flux-cli). Nota para o futuro: o CLI mantém espelho Deserialize de ApiErrorBody (flux-cli/api.rs:88-91) enquanto o tipo daqui é Serialize-only — ganhar Deserialize no common simplificaria (mudança de contrato, fora do plano).

### FA-42 · `src/framework.rs` — 72 linhas — PERMANECE (P3)

**Responsabilidade:** enum Framework (stack do projeto) com FromStr/Display e default laravel.

**Avaliação: COESA.** Um enum com FromStr/Display. **Achado:** a lista de valores válidos existe em 4 cópias (serde, FromStr 26-35, wizard.rs:60-62, doc do template) — o próprio doc (3) prevê "adapter novo = variante nova + 3 implementações" e o wizard prova o drift. `common-framework-fromstr-no-wizard` (aprovado, P3) faz o wizard usar `Framework::from_str` e herdar a mensagem.

### FA-43 · `src/error.rs` — 68 linhas — PERMANECE (P3)

**Responsabilidade:** taxonomia FluxError compartilhada (config/estado/schema) com mensagens user-facing; `Causa` classifica causa de indisponibilidade.

**Avaliação: COESA.** FluxError é consumido por state.rs (#[from] em state.rs:124) e config.rs; o CLI não o usa (parse de toml vira e.to_string()). **Achado:** `Causa` (5-14) é tipo público sem NENHUM consumidor — grep "Causa" no workspace inteiro só acha a definição; órfão da spec 17. `common-causa-orfao` (aprovado, P3): remover (git preserva) ou marcar como reserva — decisão do dono.

### FA-44 · `src/lib.rs` — 10 linhas — PERMANECE (P3)

**Responsabilidade:** raiz do crate compartilhado: 5 módulos + fronteira documentada ("nada de rede/Docker/SQLite").

**Avaliação: COESA.** O código cumpre a fronteira (deps: serde, toml, thiserror). **Achado confirmado nesta sessão:** `sha2` declarado no Cargo.toml:11 sem NENHUM uso em src/ (grep sha2|Sha256|Digest vazio) — dep morta em crate base de 2 dependentes. `common-sha2-dep-morta` (aprovado, P3). Nota do mapa: os helpers sha256_hex/CSPRNG duplicados entre fluxd (api.rs:148-156, panel_auth.rs:81-85) e flux-cli (main.rs:512-520, 542-546) teriam no flux-common o lar natural — consolidados só dentro de cada crate neste plano; migração para o common fica como opção futura registrada (AF-12).

---

## Testes, CI, manifestos, scripts

### FA-33–FA-38 (testes de integração) — ver seção crates/fluxd acima.

### FA-45 · `.github/workflows/ci.yml` — 28 linhas — ATIVAR GATES (P1)

**Responsabilidade:** pipeline de verificação: `cargo fmt --all -- --check` (ci.yml:21), `cargo clippy --workspace --all-targets -- -D warnings` (ci.yml:24), `cargo test --workspace` (ci.yml:27); job único ubuntu-latest (:10), dtolnay/rust-toolchain@stable com clippy+rustfmt (:14-16), cache rust-cache@v2 (:18), triggers push [main,master] + pull_request (:3-6). Confirmado por leitura direta nesta sessão.

**Achado central:** os testes gated PASSAM VAZIOS — `FLUX_DOCKER_URL` e `MUTAGEN_BIN` nunca são definidos em `.github/` (grep), então docker/sleep/schema_integration (retornam cedo: docker_integration.rs:14-17, sleep_integration.rs:14-21, schema_integration.rs:12-19) e os 3 gated de sync real (gate_sync_real, sync_tests.rs:32-41) rodam zero linhas. Wake/sweep/schema-exec — o coração da plataforma — e o mecanismo one-way-replica (que protege .env de replicação) têm ZERO execução automatizada; regressão só apareceria na VM. Sem rust-toolchain.toml (ls inexistente): canal stable móvel torna a verificação não reprodutível.

**Propostas:** `testes-ci-gates-docker` (aprovado, P1 — ajuste do revisor: no job com engine, rodar `cargo test --workspace` COM FLUX_DOCKER_URL em vez de filtros --test, pegando de graça os gated embutidos: ciclo.rs:812-815, panel.rs:1966-1972) · `testes-ci-gates-mutagen` (aprovado, P2) · `testes-ci-toolchain-pin` (aprovado, P3 — rust-toolchain.toml pinado; bumps de Rust viram PR explícito) · `testes-noindex-header-toda-resposta-dev` (aprovado, P2, em integration.rs).

### FA-46 · `Cargo.toml` (raiz) — PERMANECE (defer)

Workspace: 3 members, versão/edition compartilhadas (edition 2024, Cargo.toml:7 — exige Rust ≥ 1.85), todas as dependências centralizadas em `[workspace.dependencies]`. Sem `[alias]`, sem `[profile.*]` — os 3 comandos do ci.yml são TODA a verificação automatizada. Nada a mudar.

### FA-47 · `crates/fluxd/Cargo.toml` — PERMANECE (P3, cosmético)

serde_json (:34, :44), tower (:38, :47) e http-body-util (:41, :45) em [dependencies] E [dev-dependencies] — targets de teste já enxergam [dependencies]: redundância inofensiva de 3 linhas, registrada, não justifica mudança estrutural. tempfile (:46) e rand_core (:50) são as dev-deps exclusivas reais.

### FA-48 · `crates/flux-cli/Cargo.toml` — PERMANECE (defer)

Só `[[bin]]` (:7-9, sem [lib]) — torna permanente a ponte `#[path]` do sync_tests.rs (decisão estrutural maior, não proposta). `axum` em dev-deps NÃO é dependência morta: consumida pelo mock do fluxd nos unit tests do bin (src/api.rs:731-736) — correção de hipótese do auditor-testes durante a auditoria.

### FA-49 · `crates/flux-common/Cargo.toml` — REMOVER sha2 (P3)

4 dependências (serde, toml, thiserror, sha2), sem dev-deps. `sha2` morta (ver FA-44). `common-sha2-dep-morta` (aprovado, P3) — aceite: `cargo tree -p flux-common` sem sha2 (comando fora do ci.yml: **requer verificação** por quem executar).

### FA-50 · `flux.toml.example` — REMOVER BLOCO image_guard (P2)

Template user-facing das 16 seções. **Achado:** bloco image_guard (41-43) documenta chave com "Default: true" que nenhum código lê — doc prometendo comportamento inexistente (ver FA-39). Remoção junto com `common-db-image-guard-chave-morta` (quebra de parse deliberada: flux.toml existente com a chave passa a falhar citando a chave). Divergência de amarração registrada: o template é testado contra o struct (config.rs:803-852 descomenta e parseia) — pega divergência de CHAVE, não de SEMÂNTICA (foi assim que image_guard escapou). Dado não encontrado: não existe template user-facing para o config GLOBAL (GlobalConfig documentado só em código) — assimetria registrada.

### FA-51 · `scripts/host/` (3 arquivos) — PERMANECE (P3, doc)

Provisionamento fora do Rust: README.md (referência manual da VM), install-cloudflared.ps1 (serviço cloudflared no host Windows), install-fluxd.sh (fluxd + unit systemd na VM). **Achados:** README.md:11-12, 29, 33, 80-81 dependem de WSL (qemu-img/genisoimage, build no WSL) — WSL desinstalado em 14/10/2026, pipeline descrito é histórico (o README se rotula como referência de fundação, 3-5). install-cloudflared.ps1:52 passa o token do tunnel como ARG de CLI (visível em process listing) — método oficial do cloudflared, token já vive fora do repo (:8-15); registrado, sem proposta. install-fluxd.sh sólido (set -euo pipefail :5, checks :7-8, idempotente :17); `After=network-online.target` (:32) sem `Wants=` é inerte — benigno aqui (fluxd binda 127.0.0.1, lib.rs:172-174; Requires=docker.service ordena o essencial). install-fluxd.sh:36 casa com os binds de lib.rs:172-174 (contrato respeitado).
