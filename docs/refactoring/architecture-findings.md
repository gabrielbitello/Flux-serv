# Achados estruturais

Problemas transversais, ordenados por importância. Cada achado: evidência (path:linha, de auditoria somente leitura), consequência, correção recomendada com a(s) proposta(s) que a executam. Nomenclatura: FATO = observado no código; INFERÊNCIA = juízo rotulado; premissa externa ao código é dita como tal.

**Síntese do desenho atual:** o grafo de crates é um DAG sadio (`flux-cli` → `flux-common`; `fluxd` → `flux-common`; nunca CLI↔fluxd). Dentro do fluxd, `decision.rs` (núcleo puro) e `state.rs` (fronteira única de estado) têm fronteiras exemplares. O problema NÃO é arquitetura ausente — é que a mesma convenção/consulta/visual existe em N lugares com tratamento de erro que já divergiu, e três acoplamentos de direção errada. CICLO REAL: nenhum (verificado por grep por todos os auditores); ciclos potenciais: `deploy.rs → api.rs` por helpers `pub(crate)` e `proxy.rs → beat.rs`.

---

## AF-01 · Gates de verificação vazios: o coração da plataforma tem zero execução automatizada — P1

**Evidência (FATO):** `ci.yml:27` roda `cargo test --workspace` sem definir `FLUX_DOCKER_URL` nem `MUTAGEN_BIN` (grep em `.github/`: vazio). Os 3 testes de engine retornam cedo sem a env (docker_integration.rs:14-17, sleep_integration.rs:14-21, schema_integration.rs:12-19); os 3 gated de sync real exigem `MUTAGEN_BIN` (sync_tests.rs:32-41). docker_integration.rs:5 prevê exatamente este cenário ("CI pode injetar o engine quando existir") — e nada injeta.

**Consequência:** ciclo do DockerSource (wake/pause/unpause/sleep), sweep do SleepManager, `ensure_schema` via docker exec e o mecanismo one-way-replica do mutagen (que protege `.env`/segredos de replicação via GLOBAL_IGNORES, sync.rs:83-91) rodam ZERO linhas no CI. Regressão nessas rotas só apareceria na VM do Gabriel.

**Correção:** `testes-ci-gates-docker` (P1): job com dind + `FLUX_DOCKER_URL`, rodando `cargo test --workspace` completo (ajuste do revisor — pega também os gated embutidos em ciclo.rs:812-815 e panel.rs:1966-1972). `testes-ci-gates-mutagen` (P2): mutagen binário pinado + `MUTAGEN_BIN`. `testes-ci-toolchain-pin` (P3): rust-toolchain.toml. Antes de qualquer movimento estrutural (Lote 0 do plano).

## AF-02 · Seleção "Docker senão SQLite" reimplementada 4× — a porta existe e não é usada — P2

**Evidência (FATO):** a abstração certa JÁ existe (`decision::HostSource`, decision.rs:76-90), mas a escolha da fonte padrão é reescrita em: router.rs:142-145, beat.rs:46-50, panel.rs:406-417 (`verdade_ambiente`) e compose.rs:548-553.

**Consequência (INFERÊNCIA):** política de fallback duplicada em 4 pontos; mudança de critério (ex.: logar a escolha, inverter prioridade) exige 4 edits; assimetria já vista em beat.rs:50 (`host_custom` sempre via SqliteSource enquanto DockerSource::host_custom, docker.rs:297-303, já delega ao SQLite — comportamento idêntico hoje, seleção duplicada).

**Correção:** um único construtor/injetor que devolva a fonte padrão do AppState (usa o trait existente; sem camada nova). Não proposto como passo formal neste plano — é pequeno, depende do formato do AppState e melhor amarrado a um toque futuro de router/panel; registrado como dívida explícita. Relacionado: a 3ª implementação do trait (enum `Source`, router.rs:72-112) foi ANALISADA E MANTIDA — divergência resolvida pelo revisor: é adaptador de lifetime (DockerSource possui Arc, SqliteSource empresta &State), correto como está; trocar por `Box<dyn HostSource>` exigiria owned/borrow unificado sem ganho.

## AF-03 · Listagem de containers por label escrita 4-6× com tratamento de erro divergente — P2

**Evidência (FATO):** filtro `com.docker.compose.project` montado em docker.rs:34-48 e :54-62, compose.rs:1069-1073, temporary.rs:178-186 — e ainda observe.rs:539-556 (coleta) e api.rs:787-814 (`listar_containers` inline). O tratamento já divergiu: docker.rs:47 `unwrap_or_default()` (erro do engine vira lista vazia e wake/sleep relatam sucesso sem agir), docker.rs:63 `.ok()?` (não distingue "sem container" de "engine falhou"), compose/temporary tratam de um jeito, observe/log de outro.

**Consequência:** viola "nada silencioso" no caminho de estado do sono e do dev temporário: o daemon pode reportar sucesso dormindo/acordando zero containers.

**Correção:** `dados-listagem-por-label` (P2): `listar_projeto` único em docker.rs, adotado por compose.rs e temporary.rs; **ajuste do revisor:** duas formas — match exato por projeto (containers_do_projeto/find_app_container/lista_do_projeto) e variante "qualquer container com label" (compose_projects_do casa prefixo `flux-{nome}-` através de projetos). Mudança de comportamento intencional e declarada: propagar erro de listagem em vez de `Ok` silencioso — compatível com os chamadores (sleep.rs:232-252 e router.rs:252-263 já tratam Err). Cópias em observe.rs e api.rs ficam registradas (contextos distintos), sem escopo neste lote.

## AF-04 · Consultas SQLite copiadas no adaptador Docker, engolindo erro sem log — P2

**Evidência (FATO):** docker.rs:321-341 reimplementa `project_exists`/`release_flags` do SqliteSource — o próprio comentário (:321) diz "mesmo código do SqliteSource" — e engole falha: `.is_ok()` (327), `unwrap_or((false,false))` (339). O SqliteSource cumpre "nada silencioso" com tracing::error! (sqlite_source.rs:33-36, 106-109). O revisor achou mais uma cópia: `host_custom` (docker.rs:297-305 ≡ sqlite_source.rs:67-75). `compose_project_cor` engole Err de active_color (docker.rs:172-174).

**Consequência:** numa falha de leitura o router finge "projeto inexistente"/"sem flags" sem nenhum traço.

**Correção:** `dados-sqlite-reuso-em-dockersource` (P1→P2): DockerSource delega as consultas (incl. host_custom) ao SqliteSource; `compose_project_cor` passa a logar warn. Sem ciclo: sqlite_source só importa decision+state.

## AF-05 · Stubs de F11 duplicados: `branch_exists = true` em duas fontes — P2 (barreira de F11)

**Evidência (FATO):** docker.rs:290-293 e sqlite_source.rs:59-64 (confirmado por `git grep` nesta sessão), ambos citando F11.

**Consequência:** quando o registro de branches chegar, dois lugares para atualizar; esquecer um cria roteamento inconsistente por fonte. Hoje o stub torna `Action::BranchMissing` inalcançável — o que também blinda o XSS latente (AF-21) até F11.

**Correção:** nenhum passo dedicado; a delegação de AF-04 deixa as duas implementações no mesmo arquivo-irmão com o contrato visível. Registrado como atenção no momento de implementar F11.

## AF-06 · Taxonomia de erro existe e é ignorada — P3 (dívida difusa)

**Evidência (FATO):** `FluxError` (flux-common/error.rs:17-51) só é usado por config.rs e state.rs; todo o resto do fluxd devolve `Result<_, String>` com prefixo manual de origem (db.rs:119-127, 136-144; sync.rs do fluxd: 56, 169; compose.rs; deploy.rs::json_erro 110-112), e a tradução para HTTP se repete: `erro` em api.rs:172 e sua CÓPIA em ciclo.rs:87-89 (o comentário admite: "o helper de lá não é pub"); `projeto_registrado` (ciclo.rs:129-141) espelha `garante_projeto` (api.rs:613-625); tradução artesanal "sync: " por strings em sync.rs:69-74, 110-115, 199.

**Consequência:** a causa (mutada? exec falho? toml inválido?) só existe como texto na borda da API; parsers do CLI dependem do formato `{"error": ...}` — duas cópias vivas do contrato de fio divergem em silêncio.

**Correção (mínima, neste plano):** `nucleo-helper-erro` (P3): `pub(crate)` nos helpers de api.rs e cópias de ciclo.rs apagadas. A migração completa para FluxError/tipo estruturado NÃO entra no plano (mudança de contrato difusa, alto churn); registrada como direção de longo prazo.

## AF-07 · Identidade visual e utilidades de página duplicadas em DUAS famílias — P2

**Evidência (FATO):** família pública — CSS byte-idêntico em pages.rs:12-17, edge.rs:558-563, waf.rs:443-448 (md5 idêntico nos três, verificado por execução na auditoria). Família admin — CSS base comum entre panel.rs:63-127 e observe_page.rs:53-116 JÁ DIVERGIU nos dois sentidos (panel: --attn/--attnbg, .sb*/.sync; observe: form.filtros, details pre), enquanto observe_page.rs:49-52 ainda declara "cópia VERBATIM... mesma fonte de verdade visual" — fato falso. `esc`/`html_escape` 3× (panel.rs:592 = observe_page.rs:421 = pages.rs:200); `NOINDEX`/`x_robots_tag` 3× (panel.rs:46-50, observe_page.rs:34-38, router.rs:54-56/68 — terceira cópia achada pelo revisor); `fmt_utc`+`agora_epoch` 2× com teste duplicado; percent decode 2× com semântica levemente distinta (waf.rs:214-231, observe_page.rs:196-221); escape de LIKE 2× (observe.rs:167-176, panel.rs:600-604).

**Consequência:** mudar a identidade visual exige editar 3-5 arquivos; o drift JÁ aconteceu em silêncio na família admin. Escape de HTML é função de segurança com duas cópias independentes no mesmo daemon.

**Correção:** manter as duas famílias SEPARADAS (pública vs admin têm políticas diferentes: pages noindex só em dev, painel SEMPRE noindex): `rede-paginas-croma-compartilhado` (P3) — pages.rs dono único do croma público, edge/waf importam; `foco-css-nucleo-compartilhado` (P2, ajustada) — `ui.rs` com CSS_BASE/NOINDEX/x_robots_tag para **3 consumidores** (panel, observe_page, router); `foco-panel-esc-compartilhado` (P3) — dono único do escape. A unificação família-pública+admin NÃO é desejada (seria refatoração conceitual não pedida).

## AF-08 · Validador de nome de projeto escrito 5-6× — P2

**Evidência (FATO):** predicado `[a-z0-9-]` em decision.rs:197-204, temporary.rs:30-35, api.rs:201-205, flux-cli/main.rs:624-629, flux-cli/wizard.rs:38-48 (+ variante host com '.' em config.rs:479-485, que é OUTRO contrato e fica). Confirmado pelo revisor.

**Consequência:** relaxar/endurecer a regra exige achar 6 pontos; esquecer um cria projeto registrável cujo host nunca roteia (ou vice-versa) — falha silenciosa exatamente do tipo que a plataforma quer evitar.

**Correção:** `nucleo-nome-projeto` (P2): `flux_common::nomes::projeto_valido` (flux-common é o lar certo; protocol.rs já é o padrão de contrato compartilhado); adoção no fluxd no mesmo passo, no flux-cli junto do wizard (L2).

## AF-09 · Normalização de Host com 3 semânticas — IPv6 divergente entre borda e decisão — P3 (latente)

**Evidência (FATO):** flux-common/host.rs:60-61 (trim+lowercase+split ':'), edge.rs:394-417 (`host_key` com tratamento IPv6 próprio, documentado em 387-393 — o split cego erra), router.rs:131-137 e beat.rs:39-42 inline SEM o tratamento IPv6.

**Consequência (INFERÊNCIA):** a borda isola IPv6 num bucket; a decisão/beat podem ver chave diferente para o MESMO request — divergência latente documentada só de um lado. Hoje não há superfície IPv6 exposta (dados não encontrados indicando clientes IPv6 no layout); risco baixo e registrado.

**Correção:** nenhum passo formal — a extração de host em router/beat é micro e as normalizações divergem de propósito (o decide passa o host cru; ManagedHost::parse normaliza). Atenção registrada para qualquer toque em router/beat: herdar o padrão de edge.rs.

## AF-10 · Porta de domínio síncrona escondendo blocking — P3 (fragilidade documentada)

**Evidência (FATO):** `HostSource` (decision.rs:76-90) é trait sync; a implementação Docker resolve async com `tokio::task::block_in_place` + `block_on` dentro dos métodos (docker.rs:281-287 env_state, 308-314 proxy_target) e o mesmo truque em compose.rs::tunel_classificar (843-853). Exige runtime multi_thread (main.rs:56 garante hoje; nenhum guard impede uso em current_thread).

**Consequência:** invisível na assinatura; maior obstáculo a testar DockerSource isolado; quebra obscura se algo rodar em runtime de thread única.

**Correção (direção-alvo, sem passo neste plano):** a decisão (ou o router) resolve o estado async ANTES de chamar `decide()`, e o trait recebe valores prontos. É mudança de desenho do request path — fora do escopo desta rodada de modularização; registrada. Nota relacionada: handlers async do CLI usam `std::thread::sleep` bloqueante (main.rs:756, 801, 1603) — num CLI monocomando não trava ninguém; viraria problema só com concorrência interna.

## AF-11 · Acoplamentos de direção invertida (infra→feature, núcleo→feature) — P3 (registrados; um resolvido)

**Evidência (FATO):**
- `proxy.rs → beat.rs`: o forward L7 injeta `crate::beat::SCRIPT` (proxy.rs:106; toggle 27) — o proxy genérico conhece o overlay do sono. Inverter criaria acoplamento pior; hoje documentado (spec 04).
- `deploy.rs → api.rs`: helpers `pub(crate)` `{bearer_token, hash_confere, sha256_hex}` importados de outra superfície HTTP (deploy.rs:56; api.rs:139-170 admite o recorte "SÓ para deploy") — potencial ciclo de módulo se api um dia montar rotas de deploy.
- `lib.rs → deploy.rs`: AppState monta `deploy_limiter` com constantes do feature (lib.rs:65-69) — todo consumidor de AppState (testes incluídos) carrega o acoplamento mesmo sem tocar deploy.

**Correção:** o par deploy→api é resolvido por `foco-api-auth` (P3): primitivas de auth para `api/auth.rs` com re-export `pub(crate)` — deploy.rs intocado. Os outros dois ficam REGISTRADOS (corrigir proxy→beat exigiria injeção de script no ProxyClient; lib→deploy exigiria construir limiters fora do AppState) — custo maior que o ganho nesta rodada.

## AF-12 · Helpers de plataforma duplicados ENTRE crates + dep morta — P3

**Evidência (FATO):** token CSPRNG→hex idêntico em panel_auth.rs:81-85 (fluxd) e flux-cli/main.rs:542-546; `sha256_hex` idêntico em fluxd/api.rs:148-156 e flux-cli/main.rs:512-520; `hex_de` em flux-cli/sync.rs:879-885. Enquanto isso `sha2` é dependência MORTA do flux-common (Cargo.toml:11, zero usos — confirmado por `git grep` nesta sessão).

**Correção:** `common-sha2-dep-morta` (P3) remove a dependência morta agora. A consolidação dos helpers NO flux-common (que já declara sha2 sem usar) fica como opção futura registrada — exige decidir política de primitivas de cripto no crate base; não bloqueia nada.

## AF-13 · O schema SQLite do fluxd tem dois donos — P3 (registrado)

**Evidência (FATO):** state.rs é a fachada com schema versionado v1→v4 e ladder de migração; observe.rs mantém `SCHEMA_OBSERVE` próprio (observe.rs:67-95) com init próprio (lib.rs:160-163), fora das migrations.

**Consequência:** qualquer política futura de schema (backup, migração, purge) precisa conhecer os dois donos.

**Correção:** nenhuma neste plano — `foco-state-manter` (defer) manteve state.rs único e o observe é feature coesa com schema deliberadamente próprio; registrado para o dono decidir se um dia unifica a política.

## AF-14 · SQL de leitura fora da camada de dados (funil parcial) — P3 (direção, não passo)

**Evidência (FATO):** panel.rs tem queries inline (447-456, 477-487, 491-514, 530-586, 1157-1165); api.rs idem (stored_token_hash 122-137, toml_gravado 337-352, project_status 401-427); sync.rs do fluxd com SQL próprio (56-207). O padrão CERTO já existe no crate: observe_page.rs consome só a API de observe.rs. Adicionalmente a chave `proxy_target/{name}/{env}` vive por convenção em dois arquivos (escrita state.rs:244, leitura sqlite_source.rs:81) — dado não encontrado: nenhuma fonte declarando o formato além dos dois usos.

**Consequência:** schema muda em N lugares; tratamento de toml quebrado decidido ponto a ponto (re-parse do TOML gravado no SQLite em 6+ pontos por chamada: sleep.rs:84-99, router.rs:337-344, temporary.rs:57, compose.rs:401-409/704-715/990-996, api.rs:327-333, db.rs:145 — hotspot aceitável, parse barato).

**Correção (direção-alvo):** accessors de leitura no dono (`State::project(name) -> Option<ProjectToml>`; métodos de eventos do painel ao lado do escritor). Fora do escopo desta rodada — os movimentos do plano não aumentam o problema e o painel, ao virar árvore (L6.3), isola o SQL na camada `leitura.rs`, que é o passo intermediário honesto.

## AF-15 · Ciclo de vida sem dono único: loop inline no composition root — P2

**Evidência (FATO):** o loop de sweep (intervalo 60 s, primeiro tick pulado lib.rs:136, SleepManager::new(...).sweep() 140-146, expire_check sempre 155, epoch inline 151-154 duplicando sleep::now_epoch 22-27) vive embutido em `serve()` (lib.rs:130-158), enquanto observe.rs usa o padrão encapsulado `iniciar` com spawn próprio (493, 510). lib.rs não tem mod tests — "primeiro tick é pulado" e "sem Docker o teardown ainda roda" (126-129) estão sem rede.

**Consequência:** qualquer refactor de serve() quebra esses comportamentos em silêncio; o composition root mistura orquestração com política de tick.

**Correção:** `nucleo-sweep-lifecycle` (P2, ajustada pelo revisor: `iniciar(shared: Arc<AppState>, global: GlobalConfig)` — AppState não carrega GlobalConfig): `sweep.rs` novo com tick testável; sleep.rs/temporary.rs intocados.

## AF-16 · Constante de layout da VM em módulo de feature + segunda fonte de verdade — P3 (divergência resolvida: documentar, não unificar)

**Evidência (FATO):** `PROJECTS_ROOT_PADRAO` nasce em temporary.rs:25 e é consumida por ciclo.rs:172, api.rs:878, compose.rs:260, deploy.rs:361 (confirmado por `git grep` nesta sessão). Em paralelo, compose.rs:601-608 (`raiz_arvore`) e router.rs:117-122 (`live_ativo`) derivam a MESMA raiz de state_dir — as duas fontes só coincidem com o db default (main.rs:19); com `--db` custom divergem. Doc de temporary.rs:23-24 promete "o serve passa o default", mas serve usa a constante direto (lib.rs:155; temporary.rs:40).

**Resolução da divergência entre auditores:** o auditor-nucleo propôs unificar a derivação (fase 2); o revisor REJEITOU com evidência — a derivação por state_dir é gate DELIBERADO de "estou na VM" (router.rs:114-119: sem state_dir/testes o live fica desligado); unificar mudaria semântica, não removeria duplicação; a divergência com --db custom é latente e sem impacto no layout real da VM.

**Correção (reduzida):** `nucleo-raiz-projetos` (P3): corrigir a doc de temporary.rs:23-24; opcionalmente mover a const para um `paths.rs` neutro (4 imports mudam); documentar a divergência com --db custom. Fase 2 descartada.

## AF-17 · compose.rs mistura 5 responsabilidades; a regra de túnel é testada contra uma CÓPIA — P2

**Evidência (FATO):** override YAML (86-119), guard no disco (287-305, 1087-1116), up em background (258-561), live server (563-831), blue-green de prod (872-1083) — o doc do módulo (1-21) não menciona o último; nenhum símbolo do bloco é usado pelo resto do arquivo; consumidores externos (api.rs:484, 717, 724) e contrato documentado em terceiro arquivo (deploy.rs:39). `tunel_classificar` (836-870) funde I/O com a regra pura e o teste usa uma cópia (`classificar_de` no mod tunel 1346-1357; comentário 1302-1304 admite) — bug na regra real não seria pego.

**Correção:** `foco-compose-bluegreen` (P2): blue-green → `bluegreen.rs` (irmão de deploy.rs; NÃO fundir em deploy.rs, que já tem 917 l.). `foco-compose-arvore` (ajustada): OBRIGATÓRIO extrair `classificar_de` pura no próprio arquivo e apontar o teste para a real (a cópia 1346-1357 é apagada); o split de diretório em 5 arquivos foi REBAIXADO pelo revisor (fragmentação fina — pós-bluegreen sobram ~1150 l., faixa do observe.rs mantido único) — split opcional reduzido a up.rs/live.rs. Registrar no movimento: preservar a ordem de efeito colateral de subir_projeto (guard → rede → services → render → override em disco → túnel → up → estado → live, compose.rs:263-559) — escrever override antes do guard deixaria arquivo órfão.

## AF-18 · O bin CLI é despejo de 9 domínios; lógica de domínio da sync fora do módulo dono — P2

**Evidência (FATO):** ver FA-01. Destaque: `spec_da_sessao` (main.rs:1957-2006) reconstrói o contrato `flux-{project}-{slug}` que é de sync.rs (session(), sync.rs:322-327); doc de token_generate dividida (548-554 + fragmento 566-567); 9 call sites do prefixo ssh.

**Correção:** `foco-cli-contexto` (P2) → `foco-cli-comandos-por-dominio` (P2, um módulo por commit) → `foco-cli-ssh-helper` (P3, opcional). `spec_da_sessao`/`rescan_sessao` migram para `comandos/watch.rs` neste plano (movimento verbatim); a descida definitiva para sync.rs fica registrada como direção-alvo (AF-18 do dependency-map), não passo.

## AF-19 · Contratos de fio duplicados entre crates e fontes de verdade tripladas — P3

**Evidência (FATO):** `CorpoErro` (flux-cli/api.rs:88-91) espelha `ApiErrorBody` (protocol.rs:119-122, Serialize-only) — duplicação DELIBERADA e documentada; DTOs In/Out espelham por design (crate bin não depende do fluxd). Regra "node/rust exigem health path" em 3 textos (wizard.rs:72-78, api.rs:232, config.rs:64-65). Lista de frameworks em 4 cópias (serde, framework.rs:26-35, wizard.rs:60-62, doc). `VM_HOST`/`VM_USER` duplicados (tunel.rs:18-19 = sync.rs:62-63) enquanto `VmSection.vm_ip` (config.rs:501) é configurável sem leitor.

**Correção:** `common-framework-fromstr-no-wizard` (P3) e adoção de FromStr/validador no wizard; `cli-vm-constantes-fonte-unica` (P3) — tunel.rs vira fonte única (ajuste do revisor: spec.rs herda o import, não redeclara). O espelho de DTOs fica (mudar exige Deserialize no common — decisão de contrato, fora do plano). A regra de health path em 3 textos fica registrada (consolidar em flux-common seria passo próprio, ganho moderado).

## AF-20 · Segurança: confiança em `cf-connecting-ip` (FATO no código; exploração condicionada à topologia) — registrado, sem proposta

**Evidência (FATO):** waf.rs:174-179 — `cf-connecting-ip` vence TUDO quando parseia, antes de XFF (180-193) e ConnectInfo; `na_allowlist` (152-161) usa o IP resolvido. **INFERÊNCIA:** um cliente com HTTP direto ao Caddy pode forjar `cf-connecting-ip: 192.168.140.x` e entrar na allowlist. **Premissa fora do código:** a exploração depende de reachability direta ao Caddy de fora — a topologia atual nega (binds 127.0.0.1, lib.rs:172-174; cloudflared sobrescreve o header no túnel, waf.rs:170-178).

**Decisão:** registrado como risco dependente de topologia de rede (não vive no código). Se a exposição da borda mudar (ex.: binds 0.0.0.0, túnel novo), rever ANTES de qualquer refatoração aqui. Nenhum passo no plano — mudança de política de segurança não foi pedida.

## AF-21 · Segurança: XSS latente em pages.rs por interpolação sem escape — P2 (pré-requisito de F11)

**Evidência (FATO):** pages.rs:108-113 (`branch_missing` injeta `branch` e `name` crus), 88-98 e 64-84 (`name` cru em body/heading/title); `branch` vem do prefixo do Host sem validação de charset (host.rs:107-115); `not_found` JÁ escapa (130) — contrato inconsistente no mesmo módulo. **Caminho morto hoje:** `branch_exists` devolve true sempre (docker.rs:290-293, sqlite_source.rs:59-64) e `name` é gateado (api.rs:201-205, host.rs:98,119) — o revisor rebaixou de P1 para P2 (defesa, não incidente). F11 liga branch_exists real e a página passa a renderizar string do atacante; Caddy repassa Host arbitrário.

**Correção:** `rede-pages-escape-interpolacao` (P2): interpolar via `html_escape` em wait/failed/branch_missing; caracterização espelhando `not_found_escapa_html_do_host` (pages.rs:287-292). **Executar ANTES de F11.**

## AF-22 · Estado global de processo e estado parcial em crash — registrados, sem proposta

**Evidência (FATO):** `EM_VOO` é `static OnceLock<Mutex<HashSet>>` (deploy.rs:91-95) — testes reservam nomes únicos (713-714; deploy_integration.rs:127-129); pertenceria ao AppState como os outros limiters (edge já prova o padrão em lib.rs:65). Em state.rs, doc de `set_project_hosts` (111) afirma transação sem haver BEGIN/COMMIT (315-327) — atomicidade vem só do mutex; crash no meio deixa estado parcial; `migrate` idem (597-673) com self-healing por DDL idempotente (inferência rotulada); `purge_project` (286-305) apaga 7 tabelas mas não `deploy_tokens` nem `settings proxy_target/*` apesar do doc dizer "TODA a trilha" (285) — intencionalidade: dado não encontrado.

**Decisão:** nenhum passo — são correções de comportamento (transação, purge) que precisam de decisão do dono sobre semântica, não de modularização. Registrados como fila de propostas futuras candidatas.

## AF-23 · Superfície morta e nomes mentirosos — P3 (higiene barata)

**Evidência (FATO):** `SleepDecision::Busy` declarada e casada, nunca construída (sleep.rs:37, 255 — únicas ocorrências, confirmado nesta sessão); `Causa` sem consumidor (error.rs:5-14, grep workspace); `[db].image_guard` aceita-e-ignora (config.rs:112; e a global lida só em teste, config.rs:949) documentada no template (flux.toml.example:41-43); dep `sha2` morta (flux-common/Cargo.toml:11); `hash_path` pub sem leitor externo (panel_auth.rs:126; único uso :147); `db.rs` não é acesso a banco (db.rs:1-13); arm "ps" morto (main.rs:1224-1227); `create_dummy_container` helper de teste em produção (docker.rs:346-381); doc falsa da ponte #[path] (sync_tests.rs:2-5 vs main.rs:13) e de `parse_created_id` (sync.rs:429-430); comentário de "v2" velho (state_migration_integration.rs:2-4, 17-77).

**Correção:** cada item tem passo próprio no L1/L2 do plano (`nucleo-busy-morto`, `common-causa-orfao`, `common-db-image-guard-chave-morta`, `common-sha2-dep-morta`, `painel-auth-hash-path-privado`, `dados-renome-db-migrations`, `testes-sync-bridge-doc`, `foco-sync-doc-veraz`, `nucleo-raiz-projetos`-doc). O arm "ps" e o helper de teste em produção ficam como menções de 1 linha (correção não pedida).

---

## Propostas rejeitadas ou com defer (veredito com motivo)

| Item | Veredito | Motivo do revisor |
|---|---|---|
| `nucleo-raiz-projetos` fase 2 (unificar derivação de raiz) | **REJEITADA** | A derivação por state_dir é gate DELIBERADO de "estou na VM" (router.rs:114-119); unificar muda semântica; divergência com --db custom é latente e sem impacto. Restou doc + movida opcional (P3). |
| `foco-compose-arvore` split completo em 5 arquivos | **REBAIXADA** | Fragmentação fina: pós-bluegreen sobram ~1150 l., mesma faixa do observe.rs mantido único. Obrigatório: `classificar_de` pura + teste na real. Opcional: split reduzido up.rs/live.rs. |
| Fatiar handlers de api.rs por endpoint; mover mod tests | **REJEITADA** (pelo auditor) | Handlers uniformes e rasos; tabela única em build_api é a propriedade "rota nova nasce atrás do gate"; só contagem de linhas. |
| Dividir api.rs (CLI) em error/types/endpoints | **REJEITADA** (pelo auditor) | Maquinário privado compartilhado; divisão só alargaria visibilidade sem ganho. |
| `foco-observe-manter` / `foco-state-manter` | **DEFER** | Coesos; divisão não melhora manutenibilidade. Gatilhos de reavaliação documentados em FA-11/FA-12. |
| Mover `now_epoch` para fora do domínio sono | **NÃO PROPOR** | Aceitável onde está; mover geraria churn sem ganho (auditor-nucleo). |
| Unificar as duas famílias visuais (pública+admin) numa CSS só | **NÃO PROPOR** | Políticas diferentes (noindex dev vs sempre); seria refatoração conceitual não pedida. |
| Rebaixar `upsert` de observe.rs para pub(crate) | **NÃO RECOMENDADO AGORA** | Ganho marginal sem motivo real de manutenibilidade (foco-observe). |
| Refactor do ladder O(n²) do `migrate` | **FORA DO ESCOPO** | Refactor in-file de comportamento (self-healing preservado), não modularização — registrado em FA-12. |
