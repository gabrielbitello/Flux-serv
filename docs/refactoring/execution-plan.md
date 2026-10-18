# Plano de execução da refatoração modular — CANÔNICO — v2

**v2 (19/10/2026): revisão completa sob o modelo arquitetural capacidades × orquestradores × interfaces**, com releitura do código por 4 analistas independentes (só-leitura, evidência `path:linha`) + revisor independente do v2. O v1 (41 propostas, revisor independente) foi PONTO DE PARTIDA, não verdade absoluta: nada foi preservado por estar no Markdown — cada decisão foi re-verificada no código. Resultado da reavaliação: **34 passos do v1 confirmados intactos, 4 ampliados/ajustados (L3.1, L5.2, L6.3, L7.2), 3 novos (L3.4, L5.5, L5.6), 0 removidos — 41 passos no v2.** Contagens divergem ±1 por convenção de newline; os ranges abaixo seguem o revisor do v2.

Prioridades FINAIS incorporam os vereditos do revisor independente do v1 (onde divergiram do auditor proponente, marca-se "intenção → resultado"). Nenhum passo altera comportamento de produto, exceto onde o passo DECLARA explicitamente a mudança.

## Changelog v2 (o que mudou e por quê)

- **L5.2 ampliado**: `deploy.rs` tem TERCEIRA cópia de `erro` (`json_erro`, deploy.rs:110-112) e da checagem de projeto registrado inline (deploy.rs:146-160, mesmos 404/500) — o aceite do v1 (`grep "fn erro" == 1`) não fechava com deploy.rs vivo.
- **L3.1 aceite ampliado**: a resolução de container por label existe TAMBÉM no CLI (`container_do_servico`, main.rs:2248-2269) — o aceite v1 (`grep` só em fluxd) criava a ilusão de contrato único. A cópia do CLI vira consumidor registrado do contrato (menção, não movimento).
- **L6.3 ajuste vinculante**: `curva_eventos` (panel.rs:530-586) funde CONSULTA (buckets SQL, 532-565) com APRESENTAÇÃO (SVG, 569-585) — no mapa da árvore, consulta vai a `leitura.rs`, SVG a `paginas.rs`. Sem o corte, leitura.rs nasce impura.
- **L5.5 (resgatado)**: `rede-proxy-montagem-request` estava entre as 41 propostas aprovadas mas não virou passo na v1. Re-avaliado: capacidade real com 2 consumidores (`forward` proxy.rs:127-188 ≡ `forward_ws` :198-273, ~25 l. gêmeas de montagem) — entra como passo, com a ressalva de wire-format do ws.
- **L5.6 (novo, micro)**: wake+cooldown escrito 2× dentro do próprio `handle` (router.rs:156-163 ≡ 245-265) — extração de ~15 l. na mesma função/arquivo.
- **L3.4 (novo)**: os registros pós-up em compose.rs:522-527 são `let _ =` — erro de REGISTRO engolido em silêncio depois do EXECUTAR ter sucesso; contraria o padrão events.rs:19-21 ("nada silencioso"). Mudança de comportamento DECLARADA.
- **Registrados, não propostos** (gatilhos e decisões do dono): `set_deploy_token_hash` sem chamador de produção (a gravação real do token é o CLI via `ssh sqlite3`, main.rs:574/654 — fora da fachada, fora do crate); divergência semântica API×painel sobre "está de pé?" (regra F9 só na API: api.rs:458-498 vs painel cru panel.rs:406-417) — documentar, não unificar; rotas ssh→docker no CLI (build/live) vs o texto da regra "tudo passa por :7420" (main.rs:4-5); esqueleto `docker compose` spawn 2× (ciclo.rs:183-195 × temporary.rs:204-210); suspeita de worktree em `container_do_servico` (main.rs:2249 monta sem slug; ps documenta `flux-{name}-@{slug}-{env}` em :1047 — confirmar no fluxd antes de qualquer fix).

---

## 1. Visão arquitetural (como o Flux se organiza hoje e a onde queremos chegar)

O sistema tem **duas camadas de orquestração com fronteiras limpas** (re-avaliado, FATO):

- **flux-cli = orquestrador do lado Windows**: coordena a verdade do DISCO do desenvolvedor — sessão mutagen, overlays, coleta/hash de migrations, lease de sessão, polling de estado. Nunca toca containers direto pela regra declarada (main.rs:4-5); fala com o daemon pela API de controle :7420 (`ApiClient`, api.rs do CLI:325) e com a VM por ssh para o que é do disco/tar (env, overlays, build).
- **fluxd = orquestrador do lado VM**: coordena containers e borda — `subir_projeto` (compose.rs:258-561: guard→rede→services→render→override→túnel→up→estado→live), ciclo down/exec, deploy (gate + pipeline git), request flow (router), e o estado SQLite.

**Capacidades** (módulos de responsabilidade única, reutilizáveis) já existem e são o padrão dominante — a revisão confirmou que a arquitetura REAL já é próxima do modelo desejado; o plano fecha as exceções:

| Capacidade | Dono atual | Consumidores (prova de reuso) |
|---|---|---|
| Consulta HostSource | `sqlite_source.rs` (+ `DockerSource` p/ estado vivo) | router, api, painel, sleep, temporary, deploy |
| Persistência/registro | `state.rs` (fachada `State`) | api, painel, ciclo, deploy, sleep, temporary, compose, router, observe_page |
| Execução em container | `docker.rs` (+ `db.rs` p/ migrations) | ciclo, db, api (L3.3 unifica o esqueleto) |
| Compose/estado de projeto | `compose.rs` | api (up/switch), ciclo, temporary, router (porta viva) |
| Blue-green | bloco em `compose.rs:872-1083` → `bluegreen.rs` (L6.1) | api (consulta F9 + switch) — 2 caminhos distintos |
| Migrations | `db.rs` → `migrations.rs` (L1.2) | api (hoje 1 fluxo; `flux db migrate` é consumidor futuro) |
| Sono | `sleep.rs` (decisão pura + manager) | router, sweep, beat, api, painel |
| Config/validação | `flux-common/src/config.rs` | os DOIS crates |
| Hostnames | `flux-common/src/host.rs` | fluxd inteiro, wizard |
| Protocolo de fio | `flux-common/src/protocol.rs` (`Env`, `ProjectSummary`, `ApiErrorBody`) | os DOIS crates |
| Roteamento puro | `decision.rs` (matriz + trait `HostSource`) | router |
| Eventos (registro de histórico) | `events.rs` (fachada única de escrita) | todos |

**Interfaces compostas sobre o mesmo estado** (o eixo consulta×apresentação está SÃO — FATO verificado, não há SQL duplicada entre interfaces): `api.rs` (JSON, para o CLI), `panel.rs` (HTML, para o humano) e `observe_page.rs` (HTML de métricas) leem o MESMO `State`; onde precisam de mais que a fachada dá, cada uma tem a SUA consulta local (panel.rs:447-456 — alcançável pelo L6.3). A divergência de SEMÂNTICA é decisão de produto (API aplica a regra F9 "RUNNING só é awake com health", painel mostra estado cru + fonte declarada) — documentada, não unificada.

**Direção das dependências (sem ciclos, verificado)**: `fluxd/ciclo,api,panel,… → state.rs; docker.rs → state.rs (só para ler active_color, entrada legítima da convenção de nome); router → decision (puro) → HostSource; flux-cli → flux-common (contratos) → nada; flux-cli → HTTP/ssh → fluxd. Nenhuma proposta cria dependência nova entre crates.

**Arquivos grandes que ficam grandes**: orquestradores com coordenação própria legítima (main.rs residual ~460 l. = superfície de comandos; serve() ~150 l. = composition root; handle ~424 l. = despacho da matriz) e persistência/consulta co-localizadas por razão física (state.rs — schema↔purge↔ladder). As justificativas demonstradas estão na seção 8.

## 2. Diagnóstico atual (problemas reais, com evidência)

1. **Contrato de fio `{"error": causa}` em 3 cópias**: api.rs:172, ciclo.rs:87-89 (comentário admite a cópia), deploy.rs:110-112; e "projeto registrado" em api.rs:613-625 ≡ ciclo.rs:129-141 ≡ deploy.rs:146-160. Mudar mensagem/status exige tocar 3 arquivos.
2. **Listagem de containers por label 4×** no fluxd (docker.rs:34-48, :54-62, compose.rs:1069-1073, temporary.rs:178-186) com erros divergentes (`unwrap_or_default` vs `.ok()?`) + **cópia de convenção no CLI** (main.rs:2248-2269).
3. **Esqueleto `docker exec` duplicado com contratos já divergidos**: db.rs:367-422 (wrap `sh -c`, SEM teto de saída) vs ciclo.rs:375-458 (args verbatim, teto 256 KB, exit -1-while-running).
4. **Registro pós-up silencioso**: compose.rs:522-527 grava estado/proxy_target/interação com `let _ =` — se o registro falha depois de um up bem-sucedido, ninguém sabe (e o modo degradado SQLite serve alvo velho).
5. **Regras de identidade partidas**: validador de nome de projeto em 5 cópias (decision.rs:197-204, temporary.rs:30-35, api.rs:201-205, main.rs:624-629, wizard.rs:38-48); regra de host `vite-`/dev_suffix partida entre config.rs:439-494 (valida) e host.rs:85-93 (interpreta); VM_HOST/VM_USER 2× no CLI (tunel.rs:18-19, sync.rs:62-63); convenção `flux-{name}[-{env}|-{slug}]` espalhada em 4 pontos do CLI.
6. **CSS/escape triplicado com drift real**: croma público byte-idêntico em 3 módulos (AF-07); CSS admin JÁ divergiu em silêncio entre panel.rs e observe_page.rs (declaração "cópia verbatim" é falsa); `esc`/`html_escape` 3×.
7. **Gates de CI vazios**: os 3 testes de engine e os 3 de mutagen passam sem executar (ci.yml sem FLUX_DOCKER_URL/MUTAGEN_BIN).
8. **Consultas fundidas com apresentação**: `curva_eventos` (panel.rs:530-586) = SQL + SVG na mesma fn; `render_monitor` (observe_page.rs:501-764) = 4 seções com contratos de honestidade próprios numa função de 264 l.
9. **Política de engine embutida em handler**: project_status (api.rs:458-498) embute a regra F9 do engine com cobertura ZERO de teste.
10. **Mortos e mentiras de doc**: `SleepDecision::Busy` inalcançável; `Causa` sem consumidor; `sha2` morta no flux-common; `[db].image_guard` aceita e ignorada (o guard real usa o default de fábrica — compose.rs:282); `set_deploy_token_hash` sem chamador de produção (gravação real via ssh sqlite3 no CLI — terceiro caminho de escrita fora da fachada); doc de `parse_created_id` alega consumidor inexistente; doc de temporary.rs:23-24 promete o que não acontece; doc da ponte `#[path]` diz "transitória" (é permanente).
11. **Orquestrador embutindo política de tick**: loop de sweep no composition root (lib.rs:130-158) com epoch inline duplicando `sleep::now_epoch`.
12. **Montagem de request upstream duplicada**: proxy.rs `forward` ≡ `forward_ws` (~25 l. gêmeas × 5 trechos); wake+cooldown duplicado dentro do `handle` do router.
13. **Dois escritores de `sleep_state.mode` com semântica diferente** (`set_env_state` state.rs:221 vs `record_mode` state.rs:449 — só um gerencia `sleeping_since`): convenção não documentada.
14. **Wizard grava TOML não validado**: interpolação crua de `health` (wizard.rs:91) — entrada hostil vira flux.toml inválido que explode longe da causa.

## 3. Estrutura proposta (árvore-alvo por crate)

```
crates/flux-common/src/
├── config.rs          # FICA único: schema+defaults dos 2 TOML (seção 8)
├── host.rs            # + validar_hosts_custom (L2.3) — regra única de host
├── nomes.rs           # NOVO (L2.1): projeto_valido — validador único
├── protocol.rs        # FICA: contratos de fio usados pelos 2 crates
├── framework.rs       # FICA (L2.4 aponta o wizard para cá)
└── error.rs           # FICA (L1.5 decide Causa)

crates/fluxd/src/
├── lib.rs             # composition root ~150 l. (L5.1 tira a política de tick)
├── sweep.rs           # NOVO (L5.1): política do tick 60 s, tick testável
├── router.rs          # FICA orquestrador (~424 l. prod); L5.6 dedupa wake/cooldown
├── decision.rs        # FICA: matriz pura + HostSource
├── proxy.rs           # FICA; L5.5 extrai montagem única p/ forward e forward_ws
├── edge.rs, waf.rs    # FICAM; croma importado de pages.rs (L4.2)
├── pages.rs           # dono do croma público + html_escape (L4.2/L4.3)
├── guard.rs, panel_auth.rs
├── api.rs → api/{mod.rs,auth.rs}   # L5.3: gate de auth com dono próprio
├── state.rs           # FICA ÚNICO (seção 8 — demonstrado)
├── sqlite_source.rs   # FICA: consulta pura, exemplo de interface limpa
├── docker.rs          # + listar_projeto (L3.1), exec_no_app (L3.3), estado_dev_rotulado (L5.4)
├── migrations.rs      # renomeado de db.rs (L1.2); exec sai no L3.3
├── compose.rs         # FICA: compose/capacidades de subida; L6.2 extrai classificar_de pura
├── bluegreen.rs       # NOVO (L6.1): trocar_cor/saude_ok/esperar_saude/drain
├── deploy.rs          # FICA orquestrador de deploy (gate + pipeline git)
├── ciclo.rs           # FICA (maioria teste); helpers dedup no L5.2
├── sleep.rs, temporary.rs, beat.rs, events.rs  # FICAM (capacidades limpas)
├── observe.rs         # FICA ÚNICO (seção 8; gatilhos de divisão registrados)
├── observe_page.rs    # FICA; L4.5 extrai 4 seções do render_monitor
├── panel.rs → panel/{mod.rs,leitura.rs,formato.rs,paginas.rs,tests.rs}  # L6.3
└── ui.rs              # NOVO (L4.4): NOINDEX/x_robots_tag/CSS_BASE admin

crates/flux-cli/src/
├── main.rs            # ~460 l.: Cli/enums + main() + stub (L7.2)
├── contexto.rs        # NOVO (L7.1): contexto do projeto + primitivas
├── api.rs             # FICA (cliente HTTP único); testes → api/tests.rs (L7.5)
├── sync.rs → sync/{mod.rs,spec.rs,estabilidade.rs}                  # L7.4
├── tunel.rs, wizard.rs # FICAM (L2.6 endurece o wizard)
└── comandos/{up,ciclo,logs,sync_cmd,build,watch,env_cmd,live,tokens}.rs  # L7.2
```

## 4. Mapeamento de responsabilidades (manter / extrair / dividir / reorganizar)

| Arquivo (linhas) | Veredito v2 | Responsabilidades identificadas | Base |
|---|---|---|---|
| main.rs CLI (2574) | **EXTRAIR por domínio** (L7.1→L7.2→L7.3); residual ~460 l. FICA | árvore clap+despacho (fica) × contexto (sai) × 9 domínios de comando (saem) × coletores do up (ficam em up.rs: consumidor único) | leitura integral; mapa L7.2 conferido fn a fn |
| state.rs (1037) | **MANTER ÚNICO** | schema+purge+ladder (físico: um não vive sem o outro) × grupos de fns por domínio cuja LÓGICA mora nos chamadores | grupos NÃO compartilham chamador; split criaria arquivos de 30-120 l. sem unidade; purge enumera 7 tabelas |
| observe.rs (1105) | **MANTER ÚNICO** | pipeline coleta→grava→poda→perícia→consulta; consulta já isolada da apresentação via observe_page.rs | gatilhos do defer NÃO dispararam (1 fonte de coleta, 1 consumidor da consulta, pico não configurável) |
| panel.rs (1985) | **DIVIDIR** (L6.3) em leitura (consulta) × formato × páginas (apresentação) × mod × tests | 4 eixos com seams desenhados pelo autor; `curva_eventos` precisa do corte consulta/SVG (v2) | build_panel única fachada; 23 testes como caracterização |
| api.rs fluxd (1673) | **EXTRAIR auth** (L5.3); resto FICA | gate/token (sai; consumidor deploy.rs) × handlers/DTOs (ficam: tabela única = propriedade da rota) | rejeição do fatiamento por endpoint re-confirmada |
| compose.rs (1383) | **EXTRAIR blue-green** (L6.1) + `classificar_de` pura (L6.2) | compose/capacidades de subida (fica) × domínio blue-green (sai; 2 consumidores por caminhos diferentes) × regra de túnel (pura, teste usa cópia — bug) | L6.2 obrigatório resolve o teste-cópia; split em 5 arquivos rebaixado (fragmentação) |
| router.rs (~424 prod) | **MANTER orquestrador**; L5.5/L5.6 tiram as duplicações | despacho da matriz (fica) × montagem (vai p/ proxy.rs) × wake+cooldown (fn única) | handle é coordenação; nada mais embutido com 2º consumidor |
| ciclo.rs (996, ~420 prod) | **MANTER**; L5.2 dedupa helpers | handlers down/logs/exec + parsers ciclo-locais (ficam) | cópias de erro/projeto vão no L5.2 (ampliado a deploy.rs) |
| db.rs (554) | **RENOMEAR** (L1.2) + **EXTRAIR exec** (L3.3) | política pura de migrations × orquestração do fluxo × infra exec | pós-L3.3: execução (docker.rs) e registro (State) já separados entre arquivos |
| config.rs flux-common (964) | **MANTER ÚNICO**; L2.2/L2.3 pontuais | schema+validação+defaults (inseparáveis dos campos Option) | consumidores cruzam as metades project/global no mesmo uso (sleep.rs:84-99) — dividir = fragmentação |
| edge.rs (909), waf.rs (904), deploy.rs (917) | **MANTER**; croma importado (L4.2) | borda/WAF/gate — coesos, cada um 1 eixo | dedup só do visual compartilhado |
| temporary.rs (390), sleep.rs (352), sqlite_source.rs (112), events.rs (22), beat.rs (177) | **MANTER** | capacidades limpas, decisão pura separada do executor | padrão do crate já correto |

## 5. Fluxos de execução (quem combina o quê)

**`flux up` (CLI, main.rs:697-882)** — orquestrador Windows: contexto/toml → health/register (API) → lease (`LeaseGuard`, api.rs:694-726 — capacidade com 4 consumidores) → sessão mutagen (sync.rs:455) → **overlays por ssh** (capacidade embutida, 1 consumidor — fica em up.rs) → **POST /projects/{name}/up** → poll "acordado" (loop com teto — fica; gatilho: restart/db fresh com backend) → **coleta/hash de migrations** (capacidade embutida, consumidor futuro `flux db migrate`) → janela de silêncio (`esperar_estabilidade`, 2 consumidores) → imprime_status (apresentação).

**`POST /up` → `subir_projeto` (daemon, api.rs:843-895 → compose.rs:258-561)** — orquestrador VM: guard de imagens → rede → services → render/strip → override EM DISCO (antes do guard: arquivo órfão proibido) → túnel → `compose up -d --build` → **registra estado/proxy_target/interação** (L3.4 tira o silêncio daqui) → live. Fronteira CLI×daemon: sem passos sobrepostos (CLI=disco Windows, daemon=containers) — confirmado, nenhum passo atravessa.

**Request flow (Caddy→router)**: `handle` monta fonte (Docker senão SQLite) → `decision::decide` (puro) → Proxy (ensure_awake+cooldown → forward), Wait (wake em spawn + página), Panel, Deploy, ApiWaking/hold → rastro noindex+access. `down`: ciclo.rs:159-209 = gates → `docker compose stop` inline → evento. **Deploy**: gate sequencial (deploy.rs:134-273) → 202 + spawn → pipeline git (`preparar_release_em`) → troca de cor quando F9/F10 ligar.

**Migrations (o fluxo-exemplo do dono)**: EXECUTAR = db.rs (policy→docker exec→registro delegado à State `record_migration`); REGISTRAR = state.rs tabela `migration_state` (state.rs:37-45); CONSULTAR = api.rs:505 e panel.rs:439 via `migration_rows`. Execução e registro já moram em arquivos distintos; consulta via fachada — o desenho alvo já existe, L1.2/L3.3 só arrumam nomes e infra.

## 6. Dependências e interfaces

- **flux-common é o contrato dos 2 crates**: `Env`, `ProjectSummary`, `ProjectToml`/`GlobalConfig`, `Framework` (grep dos 2 lados). `ContainerState`/corpos 503 são vocabulário do fluxd que por acaso mora no common — higienização sem ganho, NÃO proposta (AF-19 registra os DTOs `*Out` espelhados do CLI por desenho de crate bin-only).
- **`HostSource` trait** = interface de consulta do router; SQLite como fallback, Docker como verdade. `State::with` expõe conexão crua — a fronteira única de estado é DISCIPLINAR (events.rs/deploy.rs/sync.rs escrevem SQL inline; CLI escreve via ssh) — limitação consciente registrada (AF-14), não problema a resolver neste plano.
- **events.rs** = fachada única de REGISTRO de histórico (tracing+UI) — todo novo tratamento de erro segue esse padrão (L3.4, L3.1).
- **Regras de dependência que os passos preservam**: sem [lib] no CLI (testes via ponte `#[path]`, constraint documentada em L1.7); sem novos `pub` além dos `pub(crate)`/`pub(super)` declarados; sem ciclos (cada passo lista a direção: sync→tunel; comandos→contexto; leitura/paginas folhas do panel; bluegreen irmão de deploy).

## 7. Plano de implementação (v2 — ordem por dependência e risco)

**Regras (valem para todo passo)**: um passo = um commit (pathspec próprio; push só por pedido; nunca `--amend` em HEAD compartilhado); **baseline verde ANTES do movimento** (trio abaixo); movimento VERBATIM (nunca "melhorar" ao mover); re-ler arquivo antes de editar (ambiente compartilhado); **nenhum passo toca VM/ssh/flux/docker** — `cargo` no host; `docs/` do mecanismo acompanha o passo; achado incidental = menção de 1 linha.

**Comandos de verificação — o "trio"** (ci.yml:21,24,27): `cargo fmt --all -- --check` · `cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`. **G** = idem com `FLUX_DOCKER_URL=tcp://127.0.0.1:2375` (engine real; CI após L0.2 — requer verificação). **M** = idem com `MUTAGEN_BIN` (requer verificação).

**Ordem**: `L0 verificação → L1 higiene → L2 contratos → L3 docker/infra → L6 compose/panel` · `L4 páginas → L5 núcleo/API → L6.3` · `L7 cli (L7.1→L7.2→L7.3; L7.4 após L2.5; L7.5 livre)` · L3.1 antes de L6.1; L5.2 antes de L5.3; L6.1 depois de L3.1.

> Os passos L0.1–L0.4, L1.1–L1.8, L2.1–L2.6, L3.2–L3.3, L4.1–L4.5, L5.1, L5.3–L5.4, L6.1–L6.2 e L7.1–L7.5 mantêm INTEGRAMENTE o texto do v1 (problema com evidência path:linha, movimento origem→destino, visibilidade, pré-requisitos, testes de regressão, validação, resultado esperado, aceite) — nenhuma evidência foi refutada pela releitura; consultá-los em [execution-plan-v1.md](execution-plan-v1.md). **L3.1, L5.2, L6.3 e L7.2 foram EMENDADOS nesta revisão** (texto abaixo vence o v1). Abaixo, SÓ o que a revisão v2 mudou ou acrescentou.

### L3.1 (ampliado) — aceite v2: além do grep em `crates/fluxd/src/`, o passo REGISTRA a cópia do CLI (`container_do_servico`, main.rs:2248-2269) como consumidor do contrato de labels/nome de compose project — menção no corpo do commit, não movimento. Confirmação registrada como pendência: `container_do_servico` monta `flux-{name}-{env}` SEM slug (main.rs:2249) enquanto `ps` documenta worktree com `@{slug}` (main.rs:1047) — confirmar no fluxd como compose project de worktree é nomeado antes de qualquer fix (dado não encontrado nesta revisão).

### L3.4 · `dados-registro-pos-up-visivel` — NOVO v2 · P2 · S (horas) · risco baixo — mudança de comportamento DECLARADA

- **Arquivos-alvo:** `crates/fluxd/src/compose.rs` (3 linhas).
- **Problema (evidência):** os três registros pós-up são `let _ =` (compose.rs:522 `set_env_state`, :524 `set_proxy_target`, :527 `touch_interaction`) — falha de REGISTRO após EXECUTAR com sucesso some em silêncio; o router em modo degradado consultaria alvo velho sem nenhum evento. Padrão do repo para falha visível: events.rs:19-21.
- **Movimento:** `let _ =` → log `tracing::warn!` com projeto/qual registro falhou (sem mudar fluxo nem status HTTP — o up já aconteceu). Evento `events::event` opcional: NÃO — warn basta, registro falho não é evento de projeto.
- **Testes:** suíte compose.rs verde; novo teste não obrigatório (3 linhas de log).
- **Validação:** trio.
- **Aceite (corrigido pelo revisor v2):** os registros usam a referência local `st` (`let st = &app.state;`, compose.rs:261) — grep `let _ = st\.` em compose.rs VAZIO depois do passo (hoje bate 3×: :522, :524, :527); o `let _ =` legítimo de compose.rs:117 (variável de env) fica, como exceção declarada.

### L5.2 (ampliado) — escopo v2: além de ciclo.rs, SAEM de deploy.rs as duplicatas gêmeas: `json_erro` (deploy.rs:110-112, mesmo corpo `{"error": ...}`) e a checagem inline de projeto registrado (deploy.rs:146-160, mesmos 404/500 e mensagem). deploy.rs passa a usar `crate::api::{erro, garante_projeto}` (pub(crate) conforme o passo). **Aceite v2:** `grep -rn "fn erro\|fn json_erro" crates/fluxd/src/` exatamente 1 definição; asserts de causa dos testes de ciclo.rs e deploy.rs passam SEM edição.

### L5.5 · `rede-proxy-montagem-request` — resgatado v2 (aprovado na auditoria, tinha ficado sem passo) · P3 · S (meio dia) · risco médio (fio de ws)

- **Arquivos-alvo:** `crates/fluxd/src/proxy.rs`.
- **Problema (evidência):** montagem do request upstream duplicada em `forward` (proxy.rs:127-188) e `forward_ws` (:198-273): path_and_query (133-137 ≡ 205-209), URI (138-143 ≡ 210-215), rewrite HOST (146-150 ≡ 217-221), builder+headers+body (152-158 ≡ 223-229), `client.request` (160-163 ≡ 231-234).
- **Movimento:** ENTRA `fn montar_request(parts: &Parts, target: &str) -> (Uri, Builder)` (ou assinatura equivalente) usada pelos 2. **RESALVA DE WIRE-FORMAT (vinculante, corrigida pelo revisor v2):** a extração cobre uri+host+headers do PEDIDO; `forward_ws` não stripa o PEDIDO (Connection/Upgrade SÃO o pedido, proxy.rs:190-195) e NÃO injeta beat — mas stripa a RESPOSTA de downgrade não-101 (proxy.rs:240): esse strip permanece. X-Forwarded-* fica no chamador. Alterar qualquer um desses pontos muda o fio do HMR em silêncio.
- **Testes:** os existentes de proxy (sem engine) verdes; caracterização: mesmo request → mesma URI/headers antes e depois (teste novo barato com Parts sintéticas).
- **Validação:** trio + smoke de HMR (requer verificação, fora do plano: projeto dev com live ligado).
- **Aceite:** `grep -n "Request::builder" crates/fluxd/src/proxy.rs` em 1 lugar; difs de comportamento zero nos testes.

### L5.6 · `rede-wake-cooldown-fn-unica` — NOVO v2 · P3 · S (horas) · risco baixo

- **Arquivos-alvo:** `crates/fluxd/src/router.rs`.
- **Problema (evidência):** política "acordou → cooldown anti-flapping" escrita 2× no mesmo handler — braço Proxy (router.rs:156-163, após `ensure_awake` inline) e braço Wait (router.rs:245-265, após `wake` dentro de `tokio::spawn`).
- **Movimento (corrigido pelo revisor v2 — os 2 braços usam verbos DIFERENTES e isso é intencional):** Proxy usa `ensure_awake` (docker.rs:184-210 — SÓ o container app, unpause de pausado, erro engolido com warn) e Wait usa `wake` (docker.rs:214-230 — start em TODOS os containers parados do projeto, `Result` propagado com warn :262). A dedup é SÓ da política de cooldown: ENTRA `fn cooldown_se_acordou(app: &AppState, nome: &str, env: Env, acordou: bool)` (chama `set_cooldown` quando `acordou`); Proxy chama com o bool de `ensure_awake`, o spawn do Wait chama com o `Ok(acordou)` de `wake` (o `Err` continua no warn de :262). **NÃO trocar o verbo de wake de nenhum braço** — trocar `wake` por `ensure_awake` no Wait para os containers de apoio (ex.: banco) pararem de acordar = mudança silenciosa.
- **Testes:** suíte do router (mock HostSource) verde sem edição.
- **Validação:** trio.
- **Aceite (corrigido v2):** `set_cooldown` aparece em router.rs EXATAMENTE 1× (dentro da nova fn — hoje são 2 chamadas: :161 e :255); chamadas de `ensure_awake` e `wake` nos braços inalteradas.

### L6.3 (ajuste vinculante v2) — no mapa da árvore panel/: `curva_eventos` divide-se — buckets SQL (panel.rs:532-565) vão para `leitura.rs`; geração de SVG (panel.rs:569-585) vai para `paginas.rs`. **Contrato intermediário (corrigido pelo revisor v2):** o SVG consome `max` e `total` computados no lado consulta (panel.rs:561, :565, :580) — a fn de leitura.rs devolve `(contagens: Vec<(u64,u64)>, total: u64, max: u64)` (ou struct equivalente), NÃO os buckets crus; tipo exato fixado no PR. Motivo: leitura.rs nasce FOLHA de consulta pura (a lente execução×consulta×apresentação exige consulta sem render). Restante do mapa v1 intocado.

### L7.2 (ampliado v2) — no movimento de `build.rs`/`live.rs`, corrigir 1 linha de doc: a regra declarada em main.rs:4-5 ("o CLI NUNCA toca em Docker direto; tudo passa por :7420") convive com 3 rotas ssh→docker de longa data (`build` flock+exec :1753, `live OFF` kill :2201, `container_do_servico` docker ps :2258) — a doc passa a declarar a exceção real (decisão de contrato: doc; unificação exigiria endpoint novo no fluxd, FORA do escopo). Registrar também no mesmo commit: convenção `flux-{name}[-{env}|-@{slug}-{env}]` como contrato a respeitar nos 4 pontos do CLI (ps :1053/:1102, watch :1839/:1878, sync.rs:322-327, :2249).

## 8. Justificativas arquiteturais (arquivos grandes que FICAM — demonstrações)

- **state.rs (1037) fica ÚNICO.** Os grupos de fns (projetos, sono, hosts, blue-green, migrations, tokens, proxy_target) NÃO compartilham chamador — separar criaria módulos de 30-120 l. cuja única unidade é "a tabela". O que amarra o arquivo é físico e demonstrável: o ladder `migrate` (597-673) precisa de TODOS os DDL; `purge_project` (286-305) enumera as 7 tabelas; os comentários 292-298 provam que v3 e v4 exigiram editar schema+purge JUNTOS. Os métodos são a metade de armazenamento de capacidades cuja lógica mora nos chamadores (db.rs orquestra migration; sleep.rs orquestra sono) — infraestrutura single-writer é o papel certo. Hotspot interno registrado (não proposto): ladder O(n²) (597-673).
- **observe.rs (1105) fica ÚNICO.** Mistura execução (coleta bollard), persistência (SCHEMA_OBSERVE próprio) e consulta — mas a consulta já tem API pub limpa consumida só por observe_page.rs (consulta×apresentação RESOLVIDA pela fronteira existente); os gatilhos de divisão não dispararam (uma fonte de coleta; api.rs não expõe métricas; pico não configurável). Invariantes para qualquer divisão futura: ordem do ciclo (523-534), snapshot de irmãos do mesmo tick (684-688), BTreeMap+sort (560, 650-656), 1 transação por ciclo (220-227).
- **config.rs flux-common (964) fica ÚNICO.** Única responsabilidade: schema+validação estrita+defaults efetivos dos dois TOML; os acessores são a metade política inseparável dos campos `Option`. Dividir project/global fragmentaria: sleep.rs:84-99 consome `ProjectToml` E `GlobalConfig` juntos; o contrato é o mais consumido do repo (16 arquivos fluxd + 3 CLI). L2.2/L2.3 fecham as únicas regras partidas.
- **Orquestradores grandes com coordenação legítima (ficam):** `main.rs` residual ~460 l. (superfície de comandos = design de API; cresce por comando novo, não por implementação embutida); `serve()` ~150 l. pós-L5.1 (único ponto que conhece a totalidade; sem política interna restante); `handle` ~424 l. (despacho da matriz; duplicações saem em L5.5/L5.6); `deploy.rs` (gate sequencial + pipeline git com capacidade já extraída e testável `preparar_release_em`); `ciclo.rs` (~420 prod, maioria teste).
- **`foco-observe-manter` e `foco-state-manter` (defer) RE-AVALIADOS e SUSTENTADOS** com demonstração (acima) — não por "serem coesos", mas por: fronteira de consulta já existir, gatilhos não disparados, e o custo físico do acoplamento schema↔purge ser maior que o ganho de fragmentar.
- **DUAS famílias visuais permanecem** (pública pages/edge/waf × admin ui.rs): políticas diferentes de verdade (noindex só-em-dev × sempre-em-tudo; meta refresh × layout admin) — ui.rs é portador de símbolos compartilhados, não fusão.

## 9. Validação da proposta (reuso sem duplicação nem dependência de fluxo maior)

- **Consulta** reutilizável sem fluxo: `sqlite_source.rs` e métodos `State` são chamáveis soltos (consumidores listados na seção 1); pós-L6.3 a leitura do painel é folha pura; pós-L6.1 `saude_ok`/`trocar_cor` servem consulta e execução por caminhos diferentes (api.rs:484 vs api.rs:717/724).
- **Execução** reutilizável: pós-L3.3 o esqueleto de exec serve migrations (db.rs) e CLI-exec (ciclo.rs) com contratos preservados como campos de `ExecSpec`; pós-L5.5 a montagem serve forward e forward_ws.
- **Nenhuma interface depende de fluxo maior**: api.rs nunca importa panel; observe_page nunca importa api.rs; CLI nunca importa fluxd (HTTP/contratos do common).
- **O `up` continua orquestrador sem virar despejo**: pós-L7.2 toda capacidade com 2º consumidor (lease, estabilidade, ssh, container_do_servico) vive fora; o que fica embutido tem 1 consumidor e gatilho registrado para virar módulo quando o 2º nascer (poll "acordado", coletores).
- **Localização por pergunta**: "quem cria o recurso?" → compose.rs (subir_projeto); "quem registra?" → state.rs (fachada); "quem executa migrations?" → migrations.rs; "quem coordena o up?" → dois orquestradores explícitos (CLI disco / daemon containers) — cada responsabilidade em 1 lugar nomeável, sem mistura.

---

## O que NUNCA fazer (anti-padrões concretos deste repo — v1 mantido integralmente)

1. **NUNCA dividir só para baixar contagem de linhas.** Este repo tem arquivos >1000 l. COESOS que ficam COM JUSTIFICATIVA DEMONSTRADA (seção 8): observe.rs, state.rs, ciclo.rs, flux-cli/api.rs (produção), deploy.rs, edge.rs, waf.rs, router.rs, config.rs. Fatiar handlers de api.rs por endpoint e dividir produção do api.rs do CLI foram REJEITADOS com evidência. Se a motivação de um passo for o número, o passo está errado.
2. **NUNCA mover código sem teste de caracterização rodando antes.** Caso-limite: L5.4 é BLOQUEADO até a caracterização dos 3 ramos existir. Os 23 testes do panel movem SEM editar corpos; os 22 do sync_tests são a prova dos re-exports.
3. **NUNCA quebrar API pública dos crates.** `panel::build_panel`, `crate::compose::*` (até L6.1), `crate::sync::*` (33 usos), deploy.rs:56 e `ProjectToml::hosts_custom` preservados por re-export/delegação — aceites que EXIGEM git diff vazio nos consumidores. flux-common é consumido pelos DOIS crates.
4. **NUNCA misturar limpeza não relacionada com extração arquitetural.** Arm morto e import morto viram MENÇÃO; um commit por escopo, sempre.
5. **NUNCA "melhorar" enquanto move.** Ordem de subir_projeto intocada (override antes do guard); `abrir_stream` NUNCA unificado com `call_com_timeout`; **forward_ws não stripa o PEDIDO (Connection/Upgrade SÃO o pedido) e mantém o strip da RESPOSTA de downgrade não-101 (proxy.rs:240)**; contexto_atual mantém os 4 process::exit; vm_ssh fora do helper ssh.
6. **NUNCA mudar comportamento silenciosamente.** Mudanças DECLARADAS: propagar erro de listagem (L3.1), teto de saída no migrate (L3.3), quebra de parse de image_guard (L2.2), warn em compose_project_cor (L3.2), **warn nos registros pós-up (L3.4)**.
7. **NUNCA engolir erro ao "consertar" engolimento** — padrão events.rs:19-21 / tracing::error!; nada de unwrap novo em caminho de request.
8. **NUNCA rodar build/assets na mão nem tocar VM/ssh/flux** — `cargo` no host; gates de CI verificam no longo prazo.
9. **NUNCA push sem pedido, nunca `--amend` em HEAD compartilhado.**
10. **NUNCA criar abstração onde uma função resolve** — `classificar_de` pura no mesmo arquivo; ui.rs não é "design system"; as duas famílias visuais não se unificam; **poll "acordado" e coletores do up NÃO ganham módulo enquanto tiverem 1 consumidor** (gatilho registrado).
11. **NUNCA reduzir visibilidade além do declarado no passo.**

## Perguntas abertas (decisão do dono; não bloqueiam L0/L1)

> **Regra de escopo (19/10, ordem do Gabriel): esta refatoração NÃO implementa funcionalidade** — define estrutura, separação e arruma problemas que JÁ existem (movimento, dedup, morto, doc, gate de CI, teste de caracterização). Opções abaixo que implementariam algo novo (ligar image_guard, endpoint de token) estão FORA do escopo desta refatoração — viram pedido separado se um dia fizerem sentido.

1. **[image_guard] GLOBAL** (L2.2): remover também? Default: remover. ("ligar" a global no compose.rs:282 implementaria comportamento novo — FORA do escopo.)
2. **`Causa`** (L1.5): remover ou marcar reserva da spec 17? Default: remover.
3. **Split opcional de compose.rs** (L6.2): up.rs/live.rs após o obrigatório? Default: não.
4. **Helper ssh** (L7.3): executar? Default: sim.
5. **NOVO v2 — `set_deploy_token_hash`** (state.rs): sem chamador de produção — a gravação real do token é o CLI via `ssh sqlite3` (main.rs:574, 654), terceiro caminho de escrita que bypassa fachada, gate e schema-check. Opção no escopo: (a) remover a fn morta e DOCUMENTAR o canal CLI→sqlite como contrato (status quo formalizado). (Endpoint novo na API :7420 = funcionalidade nova — FORA do escopo; candidato a pedido separado quando o contrato CLI↔daemon evoluir.)
6. **NOVO v2 — invocador `docker compose` duplicado** (ciclo.rs:183-195 stop × temporary.rs:204-210 down): esqueleto spawn+exit 2× — candidata P3 num futuro lote higiene; não bloqueia nada.
7. **NOVO v2 — divergência API×painel sobre "está de pé?"** (regra F9 só na API — api.rs:458-498 vs painel cru panel.rs:406-417): documentar a semântica por interface em docs/ (1 linha) — unificar NÃO recomendado (produto decidiu diferente por superfície).
