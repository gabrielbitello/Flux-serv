//! Apresentação do painel (L6.3): layout, páginas e SVG — a CONSULTA é
//! leitura.rs, o formato é formato.rs, a montagem de rotas é mod.rs.

use axum::http::StatusCode;
use rusqlite::params;

use super::formato::{
    badge_level, badge_sync, classe_status, dot, epoch_de, esc, like_esc, page_de, parse_access,
    quando, query_encode,
};
use super::leitura::{
    self, EVENTOS_DASHBOARD, EVENTOS_POR_PAGINA, EventoRow, PainelEnv, agora_epoch, ambiente,
    eventos_filtrados, eventos_recentes, projetos, row_evento,
};
use crate::AppState;
use flux_common::config::ProjectToml;
use flux_common::protocol::Env;

const CURVA_W: u32 = 720;
const CURVA_H: u32 = 120;

pub(super) const CSS_TAIL: &str = r#"/* Badges de estado da sessão de sync — mesmos tokens, um por classe. */
.sb{display:inline-block;border-radius:999px;padding:.05rem .55rem;
font-size:.72rem;font-weight:600}
.sb-ok{background:var(--okbg);color:var(--ok)}
.sb-warn{background:var(--warnbg);color:var(--warn)}
.sb-attn{background:var(--attnbg);color:var(--attn)}
.sb-info{background:var(--infobg);color:var(--info)}
.sb-idle{background:var(--idlebg);color:var(--idle)}
.sync{margin:.6rem 0 0;font-size:.82rem}
.sync table{margin-top:.3rem}
/* Filtros + tabela de eventos (26/10 — liberado JS mínimo pelo Gabriel). */
.filtros{display:flex;flex-wrap:wrap;gap:.35rem;align-items:center;margin:.4rem 0 .8rem}
.chip{display:inline-block;border:1px solid var(--line);border-radius:999px;
padding:.08rem .6rem;font-size:.78rem;color:var(--text);text-decoration:none}
.chip:hover{border-color:var(--accent)}
.chip-at{background:var(--accentbg);color:var(--accent);border-color:var(--accent)}
.filtros form{display:flex;gap:.35rem;margin-left:auto}
.filtros input{background:var(--bg);color:var(--text);border:1px solid var(--line);
border-radius:6px;padding:.2rem .5rem}
.filtros button{background:var(--accentbg);color:var(--accent);border:1px solid var(--accent);
border-radius:6px;padding:.2rem .6rem;cursor:pointer}
.st{display:inline-block;border-radius:4px;padding:0 .35rem;font-size:.76rem;font-weight:600}
.st-ok{background:var(--okbg);color:var(--ok)}
.st-3{background:var(--idlebg);color:var(--idle)}
.st-warn{background:var(--warnbg);color:var(--warn)}
.st-err{background:var(--errbg);color:var(--err)}
tr.lwarn td{background:var(--warnbg)}
tr.lerr td{background:var(--errbg)}
.cru{margin-top:.15rem}
.cru summary{color:var(--muted);font-size:.76rem;cursor:pointer}
.cru pre{white-space:pre-wrap;word-break:break-all;font-size:.74rem;margin:.2rem 0 0;
background:var(--bg);border-radius:6px;padding:.3rem .4rem}
.atualiza{display:flex;gap:.6rem;align-items:center;margin:.3rem 0 .6rem}
.atualiza button{background:var(--idlebg);color:var(--text);border:1px solid var(--line);
border-radius:6px;padding:.15rem .6rem;cursor:pointer;font-size:.78rem}
"#;

/// último erro quando existe, badge de rescan pendente e o botão de rescan.
/// SVG da curva de eventos — APRESENTAÇÃO (a consulta é
/// `leitura::curva_contagem`, ajuste v2 do L6.3): leitura devolve conts/total/max.
fn svg_curva_eventos(conts: &[u64], total: u64, max: u64) -> String {
    // Desenho server-side: 1440 pontos (~17 kB) cabe folgado no orçamento de
    // 100 kB da spec. y=112 é o chão; pico vai a y=14.
    let mut pts = String::with_capacity(conts.len() * 12);
    for (i, c) in conts.iter().enumerate() {
        let x = i as f64 * (CURVA_W as f64 - 1.0) / (conts.len() as f64 - 1.0);
        let y = 112.0 - (*c as f64 / max as f64) * 98.0;
        pts.push_str(&format!("{x:.1},{y:.1} "));
    }
    format!(
        "<svg viewBox=\"0 0 {CURVA_W} {CURVA_H}\" role=\"img\" \
         aria-label=\"eventos por minuto nas últimas 24 horas\">\
         <line class=\"eixo\" x1=\"0\" y1=\"112\" x2=\"{CURVA_W}\" y2=\"112\"/>\
         <polyline points=\"{pts}\"/>\
         <text x=\"4\" y=\"10\">pico {max}/min · {total} eventos</text>\
         <text x=\"4\" y=\"117\">-24 h</text>\
         <text x=\"{}\" y=\"117\">agora</text>\
         </svg>",
        CURVA_W - 34
    )
}

/// Linha "Sync" do card do projeto (leitura de `crate::sync`): badge com
/// cor, texto CRU do mutagen à mostra (honestidade — o rótulo digere, o
/// dado continua lá), arquivos/pendências, `updated_at` em tempo relativo.
fn linha_sync(now: i64, row: Option<&crate::sync::SyncRow>, name: &str) -> String {
    let Some(r) = row else {
        // Sem relato ainda, o botão FICA: o rescan é fila por projeto no
        // fluxd — a flag nasce antes do primeiro report e o watch consome
        // depois (sync.rs, request_rescan).
        return format!(
            "<div class=\"sync\"><span class=\"sb sb-idle\">sem sessão</span> \
             <span class=\"muted\">o flux watch ainda não reportou este projeto</span> \
             <form method=\"post\" action=\"/projects/{name}/rescan\">\
             <button type=\"submit\" style=\"background:var(--accentbg);color:var(--accent);\
             border:1px solid var(--accent);border-radius:6px;padding:.1rem .55rem;cursor:pointer;\
             font-size:.76rem\">Rescan</button></form></div>",
            name = esc(name),
        );
    };
    let (classe, rotulo) = badge_sync(Some(r));
    let arquivos = match r.files {
        Some(n) => n.to_string(),
        None => "—".into(),
    };
    let pend = if r.problems > 0 {
        format!("<span class=\"err-t\">{}</span>", r.problems)
    } else {
        "0".into()
    };
    // Parseável vira tempo relativo com o absoluto no `title`; o que não
    // parseia aparece CRU — nunca hora inventada.
    let atualizado = match epoch_de(&r.updated_at) {
        Some(t) => quando(now, Some(t)),
        None => esc(&r.updated_at),
    };
    let pendente = if r.rescan_requested {
        "<span class=\"sb sb-warn\">rescan pendente</span>"
    } else {
        ""
    };
    let erro = match &r.last_error {
        Some(e) => format!("<p class=\"sync err-t\">último erro: {}</p>", esc(e)),
        None => String::new(),
    };
    format!(
        "<div class=\"sync\">\
         <span class=\"sb {classe}\">{rotulo}</span> <span class=\"muted\">{}</span> {pendente}\
         <table><tr><th>arquivos</th><td>{arquivos}</td>\
         <th>pendências</th><td>{pend}</td><th>atualizado</th><td>{atualizado}</td></tr></table>\
         {erro}\
         <form method=\"post\" action=\"/projects/{}/rescan\">\
         <button type=\"submit\" style=\"background:var(--accentbg);color:var(--accent);\
         border:1px solid var(--accent);border-radius:6px;padding:.1rem .55rem;cursor:pointer;\
         font-size:.76rem\">Rescan</button></form>\
         </div>",
        esc(&r.status),
        esc(name),
    )
}

// ------------------------------------------------------------------ layout

/// Esqueleto de página: tokens CSS, botão Sair, `meta robots` (R22) e
/// `meta refresh` opcional (só dashboard — /events atualiza por JS mínimo
/// autorizado, detalhe é leitura sob demanda).
pub(super) fn layout(title: &str, body: &str, auto_refresh: bool) -> String {
    let refresh = if auto_refresh {
        "<meta http-equiv=\"refresh\" content=\"5\">"
    } else {
        ""
    };
    format!(
        r#"<!doctype html>
<html lang="pt-br">
<head>
<meta charset="utf-8">
{refresh}
<meta name="robots" content="noindex, nofollow">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{css_admin}{css_tail}</style>
</head>
<body>
<div class="wrap">
<header class="top">
<h1>Flux · painel</h1>
<nav><a href="/">Dashboard</a><a href="/events">Eventos</a><a href="/observe">Monitor</a><a href="http://192.168.140.2:8081/" title="Gerenciador de banco — LAN-only (05)">Adminer</a></nav>
<form method="post" action="/logout" style="margin-left:auto">
<button type="submit" style="background:var(--idlebg);color:var(--text);
border:1px solid var(--line);border-radius:6px;padding:.2rem .7rem;cursor:pointer">Sair</button>
</form>
</header>
{body}
<footer>fluxd — painel server-side · JS mínimo só no auto-refresh de /events · horários em UTC</footer>
</div>
</body>
</html>"#,
        css_admin = crate::ui::CSS_ADMIN,
        css_tail = CSS_TAIL,
    )
}

/// Tela de login (08): campo de senha só — usuário único (Gabriel), o nome
/// não participa da autenticação. Mensagem opcional em cima (senha errada,
/// rate limit, hash ausente). Sem nav — quem não logou não navega.
pub(super) fn render_login(mensagem: Option<&str>) -> String {
    let msg = mensagem
        .map(|m| format!("<div class=\"aviso\">{}</div>", esc(m)))
        .unwrap_or_default();
    let mut body = String::from("<h1>Entrar no painel</h1>");
    body.push_str(&msg);
    body.push_str(
        r#"<form method="post" action="/login" class="card" style="max-width:380px;
display:flex;flex-direction:column;gap:.7rem;margin-top:.8rem">
<label for="password">Senha</label>
<input type="password" id="password" name="password" required autofocus
style="background:var(--bg);color:var(--text);border:1px solid var(--line);
border-radius:6px;padding:.45rem .6rem">
<button type="submit" style="background:var(--accentbg);color:var(--accent);
border:1px solid var(--accent);border-radius:6px;padding:.45rem .6rem;
font-weight:600;cursor:pointer">Entrar</button>
</form>"#,
    );
    layout("Flux — entrar", &body, false)
}

fn linha_estado(e: &PainelEnv) -> String {
    let (classe, rotulo) = dot(e.estado);
    match &e.row {
        Ok(_) => format!(
            "<span class=\"dot {classe}\"></span>{rotulo} <span class=\"muted\">({})</span>",
            e.fonte
        ),
        Err(msg) => format!(
            "<span class=\"dot err\"></span><span class=\"err-t\">{}</span>",
            esc(msg)
        ),
    }
}

// --------------------------------------------------------------- dashboard

pub(super) fn render_dashboard(app: &AppState, aviso: Option<&str>) -> String {
    let mut body = String::new();
    // Aviso de ação (confirmação de rescan, erro real de rescan) no topo.
    if let Some(msg) = aviso {
        body.push_str(&format!("<div class=\"aviso\">{}</div>", esc(msg)));
    }

    match projetos(app) {
        Ok(lista) if lista.is_empty() => {
            body.push_str(
                "<div class=\"empty\">Nenhum projeto registrado ainda.\
                 <br><span class=\"muted\">Projetos aparecem aqui quando o fluxd conhece \
                 o <code>flux.toml</code> (spec 06).</span></div>",
            );
        }
        Ok(lista) => {
            // Sem `unwrap_or(0)`: now=0 faria `quando` renderizar "em 20 mil
            // dias" como se fosse verdade — a falha real aparece na página,
            // como em toda leitura do estado (promessa do módulo).
            let now = match agora_epoch(app) {
                Ok(n) => n,
                Err(e) => {
                    body.push_str(&format!(
                        "<div class=\"empty err-t\">Falha lendo o relógio do estado: {}</div>",
                        esc(&e)
                    ));
                    return layout("Flux — painel", &body, true);
                }
            };
            // Estados de sync (mutagen) — UMA leitura para todos os cards;
            // projeto sem linha na lista = "sem sessão" no card.
            let syncs = crate::sync::sync_states(app);
            body.push_str("<div class=\"cards\">");
            for (name, toml_src) in lista {
                let dev = ambiente(app, &name, Env::Dev);
                let prod = ambiente(app, &name, Env::Prod);
                let framework = match ProjectToml::parse_str(&toml_src) {
                    Ok(t) => format!("<span class=\"badge\">{}</span>", t.framework()),
                    Err(e) => format!(
                        "<span class=\"badge err-t\" title=\"{}\">flux.toml inválido</span>",
                        esc(&e.to_string())
                    ),
                };
                // [domains] (09): override gravado, senão derivação.
                let (ov_prod, ov_dev) = app.state.project_hosts(&name).unwrap_or((None, None));
                let dev_host = ov_dev.unwrap_or_else(|| app.domain.dev_host(&name));
                let prod_host = ov_prod.unwrap_or_else(|| app.domain.prod_host(&name));
                body.push_str(&format!(
                    "<article class=\"card\"><h3><a href=\"/projects/{}\">{}</a>{framework}</h3>\
                     <table>\
                     <tr><th>dev</th><td>{}</td><td>migrations aplicadas: {}</td></tr>\
                     <tr><th>prod</th><td>{}</td><td>migrations aplicadas: {}</td></tr>\
                     </table>\
                     <p class=\"urls\"><a href=\"https://{dev_host}\">{dev_host}</a> · \
                     <a href=\"https://{prod_host}\">{prod_host}</a></p>",
                    esc(&name),
                    esc(&name),
                    linha_estado(&dev),
                    dev.aplicadas.len(),
                    linha_estado(&prod),
                    prod.aplicadas.len(),
                ));
                // Linha de sync do projeto (badge + contagens + rescan).
                body.push_str(&linha_sync(
                    now,
                    syncs.iter().find(|r| r.project == name),
                    &name,
                ));
                // Sinais de sono (04) quando existem — ausente vira "—".
                if let (Ok(drow), Ok(prow)) = (&dev.row, &prod.row) {
                    body.push_str(&format!(
                        "<table><tr><th></th><th>interação</th><th>lease</th><th>cooldown</th></tr>\
                         <tr><th>dev</th><td>{}</td><td>{}</td><td>{}</td></tr>\
                         <tr><th>prod</th><td>{}</td><td>{}</td><td>{}</td></tr></table>",
                        quando(now, drow.0),
                        quando(now, drow.1),
                        quando(now, drow.2),
                        quando(now, prow.0),
                        quando(now, prow.1),
                        quando(now, prow.2),
                    ));
                }
                body.push_str("</article>");
            }
            body.push_str("</div>");
        }
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha lendo projetos do estado: {}</div>",
            esc(&e)
        )),
    }

    // Curva 24 h — sem dado, a frase exata, sem gráfico vazio mentindo vida.
    body.push_str("<section><h2>Eventos por minuto — últimas 24 h</h2>");
    match leitura::curva_contagem(app) {
        Ok(Some((conts, total, max))) => body.push_str(&format!(
            "<div class=\"curve\">{}</div>",
            svg_curva_eventos(&conts, total, max)
        )),
        Ok(None) => body.push_str("<div class=\"empty\">sem dados ainda</div>"),
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha lendo eventos: {}</div>",
            esc(&e)
        )),
    }
    body.push_str("</section>");

    body.push_str("<section><h2>Últimos eventos</h2>");
    match eventos_recentes(app, EVENTOS_DASHBOARD) {
        Ok(rows) if rows.is_empty() => {
            body.push_str("<div class=\"empty\">sem eventos registrados ainda</div>")
        }
        Ok(rows) => {
            body.push_str("<table><tr><th>quando (UTC)</th><th>nível</th><th>escopo</th><th>mensagem</th></tr>");
            for ev in rows {
                body.push_str(&format!(
                    "<tr><td class=\"muted\">{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                    esc(&ev.created_at),
                    badge_level(&ev.level),
                    esc(&ev.scope),
                    esc(&ev.message)
                ));
            }
            body.push_str("</table><p><a href=\"/events\">todos os eventos →</a></p>");
        }
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha lendo eventos: {}</div>",
            esc(&e)
        )),
    }
    body.push_str("</section>");

    // Vigia (24/10): anomalias detectadas + prova de vida do detector —
    // silêncio SEM marca de ciclo = vigia morto, e isso é problema.
    body.push_str("<section><h2>Vigia</h2>");
    match leitura::vigia_ultimo_ciclo(app) {
        Ok(Some(marca)) => body.push_str(&format!(
            "<p class=\"muted\">último ciclo: {} UTC — silêncio sem marca de ciclo = vigia morto</p>",
            esc(&marca)
        )),
        Ok(None) => body.push_str("<p class=\"muted\">vigia ainda não rodou nenhum ciclo</p>"),
        Err(e) => body.push_str(&format!("<div class=\"empty err-t\">{}</div>", esc(&e))),
    }
    match leitura::anomalias_recentes(app, 5) {
        Ok(rows) if rows.is_empty() => {
            body.push_str("<div class=\"empty\">sem anomalias — silêncio saudável</div>")
        }
        Ok(rows) => {
            body.push_str("<table><tr><th>quando (UTC)</th><th>mensagem</th></tr>");
            for ev in rows {
                body.push_str(&format!(
                    "<tr><td class=\"muted\">{}</td><td>{}</td></tr>",
                    esc(&ev.created_at),
                    esc(&ev.message)
                ));
            }
            body.push_str("</table>");
        }
        Err(e) => body.push_str(&format!("<div class=\"empty err-t\">{}</div>", esc(&e))),
    }
    body.push_str("</section>");

    layout("Flux — painel", &body, true)
}

// ----------------------------------------------------------------- detalhe

pub(super) fn detalhe_status(app: &AppState, name: &str) -> StatusCode {
    match projetos(app) {
        Ok(lista) if lista.iter().any(|(n, _)| n == name) => StatusCode::OK,
        Ok(_) => StatusCode::NOT_FOUND,
        // Estado ilegível: 503 é a verdade (painel não consegue provar que o
        // projeto existe) — com a mensagem real na página, nunca silêncio.
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub(super) fn render_detalhe(app: &AppState, name: &str) -> String {
    let Ok(lista) = projetos(app) else {
        return layout(
            "Flux — painel",
            "<h1>Estado indisponível</h1><p class=\"err-t\">Não consegui ler a lista de \
             projetos — o erro real está no log do fluxd.</p><p><a href=\"/\">← dashboard</a></p>",
            false,
        );
    };
    let Some((_, toml_src)) = lista.iter().find(|(n, _)| n == name) else {
        return layout(
            "Flux — painel",
            &format!(
                "<h1>Projeto não registrado</h1><p><code>{}</code> não está no estado do \
                 fluxd. Projetos entram pelo <code>flux.toml</code> (spec 06).</p>\
                 <p><a href=\"/\">← dashboard</a></p>",
                esc(name)
            ),
            false,
        );
    };

    let mut body = String::new();
    let framework = match ProjectToml::parse_str(toml_src) {
        Ok(t) => format!("<span class=\"badge\">{}</span>", t.framework()),
        Err(e) => format!(
            "<span class=\"badge err-t\" title=\"{}\">flux.toml inválido</span>",
            esc(&e.to_string())
        ),
    };
    // [domains] (09): override gravado, senão derivação.
    let (ov_prod, ov_dev) = app.state.project_hosts(name).unwrap_or((None, None));
    let dev_host = ov_dev.unwrap_or_else(|| app.domain.dev_host(name));
    let prod_host = ov_prod.unwrap_or_else(|| app.domain.prod_host(name));
    body.push_str(&format!(
        "<h1>{} {framework}</h1>\
         <p class=\"urls\">dev <a href=\"https://{dev_host}\">{dev_host}</a> · \
         prod <a href=\"https://{prod_host}\">{prod_host}</a></p>",
        esc(name)
    ));

    let dev = ambiente(app, name, Env::Dev);
    let prod = ambiente(app, name, Env::Prod);
    // Sem `unwrap_or(0)` pelo mesmo motivo do dashboard: relógio ilegível não
    // vira "em 20 mil dias" — a causa real sobe na página.
    let now = match agora_epoch(app) {
        Ok(n) => n,
        Err(e) => {
            return layout(
                "Flux — painel",
                &format!(
                    "<h1>Estado indisponível</h1><p class=\"err-t\">Falha lendo o relógio do \
                     estado: {}</p><p><a href=\"/\">← dashboard</a></p>",
                    esc(&e)
                ),
                false,
            );
        }
    };

    body.push_str("<section><h2>Ambientes</h2><table>\
                   <tr><th></th><th>estado</th><th>última interação</th><th>lease até</th><th>cooldown até</th></tr>");
    for e in [&dev, &prod] {
        let (sinais, _) = match &e.row {
            Ok(row) => (
                format!(
                    "<td>{}</td><td>{}</td><td>{}</td>",
                    quando(now, row.0),
                    quando(now, row.1),
                    quando(now, row.2)
                ),
                "",
            ),
            Err(msg) => (
                format!("<td colspan=\"3\" class=\"err-t\">{}</td>", esc(msg)),
                "",
            ),
        };
        let (dclasse, drotulo) = dot(e.estado);
        body.push_str(&format!(
            "<tr><th>{}</th><td><span class=\"dot {dclasse}\"></span>{drotulo} \
             <span class=\"muted\">({})</span></td>{sinais}</tr>",
            e.env, e.fonte
        ));
    }
    body.push_str("</table></section>");

    for e in [&dev, &prod] {
        body.push_str(&format!("<section><h2>Migrations — {}</h2>", e.env));
        if e.aplicadas.is_empty() {
            body.push_str(&format!(
                "<div class=\"empty\">nenhuma migration aplicada em {} ainda</div>",
                e.env
            ));
        } else {
            body.push_str("<ul>");
            for (file, sha) in &e.aplicadas {
                body.push_str(&format!(
                    "<li><code>{}</code> <span class=\"muted\">{}</span></li>",
                    esc(file),
                    esc(sha.get(..8).unwrap_or(sha))
                ));
            }
            body.push_str("</ul>");
        }
        // O painel desta fase lê SÓ o estado: o diff contra o disco é do ciclo
        // de schema (05), que roda no sync/deploy. Dizer "não sei" é a saída
        // honesta — inventar "0 pendentes" não é.
        body.push_str(
            "<p class=\"muted\">pendentes: sem verdade de disco nesta fase \
                       (o diff roda no ciclo de schema, spec 05)</p></section>",
        );
    }

    body.push_str(&format!(
        "<section><h2>Eventos que mencionam \"{}</h2>",
        esc(name)
    ));
    match app.state.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, level, scope, message, created_at FROM events
             WHERE message LIKE '%' || ?1 || '%' ESCAPE '\\'
             ORDER BY id DESC LIMIT 20",
        )?;
        let rows = stmt.query_map(params![like_esc(name)], row_evento)?;
        rows.collect::<rusqlite::Result<Vec<EventoRow>>>()
    }) {
        Ok(rows) if rows.is_empty() => {
            body.push_str("<div class=\"empty\">nenhum evento menciona este projeto ainda</div>")
        }
        Ok(rows) => {
            body.push_str("<table><tr><th>quando (UTC)</th><th>nível</th><th>escopo</th><th>mensagem</th></tr>");
            for ev in rows {
                body.push_str(&format!(
                    "<tr><td class=\"muted\">{}</td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                    esc(&ev.created_at),
                    badge_level(&ev.level),
                    esc(&ev.scope),
                    esc(&ev.message)
                ));
            }
            body.push_str("</table>");
        }
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha na busca de eventos: {}</div>",
            esc(&e.to_string())
        )),
    }
    body.push_str("</section><p><a href=\"/\">← dashboard</a></p>");

    layout(&format!("Flux — {}", esc(name)), &body, false)
}

// ------------------------------------------------------------------ eventos

/// Query string com os filtros + página — os chips e o pager nunca perdem o
/// filtro de onde vieram.
fn qs_eventos(f: &leitura::FiltroEventos, page: i64) -> String {
    let mut pares = vec![format!("page={page}")];
    if let Some(e) = &f.scope {
        pares.push(format!("escopo={}", query_encode(e)));
    }
    if let Some(n) = &f.nivel {
        pares.push(format!("nivel={}", query_encode(n)));
    }
    if let Some(q) = &f.busca {
        pares.push(format!("q={}", query_encode(q)));
    }
    pares.join("&")
}

/// Chip de filtro ativo ganha `.chip-at` — o operador vê o que está aplicado.
fn chip(rotulo: &str, href: &str, ativo: bool) -> String {
    let classe = if ativo { "chip chip-at" } else { "chip" };
    format!("<a class=\"{classe}\" href=\"{href}\">{}</a>", esc(rotulo))
}

/// Uma célula de conteúdo da tabela: access vira linha estruturada (método +
/// caminho + badge de status + duração + host) com o texto CRU num
/// `<details>` — a mensagem integral segue à mostra, um clique de distância.
/// O que não é access fica CRU, esc passado.
fn celula_conteudo(ev: &EventoRow) -> String {
    match parse_access(&ev.message) {
        Some(a) => format!(
            "<code>{} {}</code> <span class=\"st {classe}\">{status}</span> \
             <span class=\"muted\">{dur} ms · {host}</span>\
             <details class=\"cru\"><summary>cru</summary><pre>{cru}</pre></details>",
            esc(&a.metodo),
            esc(&a.caminho),
            classe = classe_status(a.status),
            status = a.status,
            dur = a.dur_ms,
            host = esc(&a.host),
            cru = esc(&ev.message),
        ),
        None => esc(&ev.message),
    }
}

pub(super) fn render_eventos(app: &AppState, query: Option<&str>) -> String {
    let filtro = leitura::filtro_de(query);
    let page_pedida = page_de(query);
    let escopos = leitura::escopos_distintos(app).unwrap_or_default();

    let mut body = String::from("<h1>Eventos</h1>");

    // Barra de filtros: escopos existentes + níveis fixos + busca. Todo link
    // volta com page=1 (trocar de filtro sem cair no meio de uma página velha).
    body.push_str("<div class=\"filtros\">");
    body.push_str(&chip(
        "todos",
        &format!(
            "/events?{}",
            qs_eventos(&leitura::FiltroEventos::default(), 1)
        ),
        filtro.scope.is_none() && filtro.nivel.is_none() && filtro.busca.is_none(),
    ));
    for e in &escopos {
        let mut f2 = filtro.clone();
        f2.scope = Some(e.clone());
        let ativo = filtro.scope.as_deref() == Some(e);
        body.push_str(&chip(e, &format!("/events?{}", qs_eventos(&f2, 1)), ativo));
    }
    body.push_str("</div>");
    body.push_str("<div class=\"filtros\">");
    for n in ["info", "warn", "error"] {
        let mut f2 = filtro.clone();
        f2.nivel = Some(n.to_owned());
        let ativo = filtro.nivel.as_deref() == Some(n);
        body.push_str(&chip(n, &format!("/events?{}", qs_eventos(&f2, 1)), ativo));
    }
    body.push_str(&format!(
        "<form method=\"get\" action=\"/events\"><input type=\"text\" name=\"q\" \
         value=\"{}\" placeholder=\"buscar na mensagem…\" autofocus> \
         <button type=\"submit\">buscar</button></form>",
        esc(filtro.busca.as_deref().unwrap_or(""))
    ));
    body.push_str("</div>");

    match eventos_filtrados(app, page_pedida, EVENTOS_POR_PAGINA, &filtro) {
        Ok((page, rows, total)) => {
            let paginas = ((total + EVENTOS_POR_PAGINA - 1) / EVENTOS_POR_PAGINA).max(1);
            body.push_str(&format!(
                "<p class=\"muted\" id=\"cabecalho-eventos\">página {page} de {paginas} · \
                 {total} eventos no total</p>"
            ));
            if rows.is_empty() {
                body.push_str("<div class=\"empty\">nenhum evento com esse filtro</div>");
            } else {
                body.push_str(
                    "<table id=\"tab-eventos\"><thead><tr><th>quando (UTC)</th><th>nível</th>\
                     <th>escopo</th><th>conteúdo</th></tr></thead><tbody id=\"corpo-eventos\">",
                );
                for ev in rows {
                    let destaque = match ev.level.as_str() {
                        "warn" => " class=\"lwarn\"",
                        "error" | "err" => " class=\"lerr\"",
                        _ => "",
                    };
                    body.push_str(&format!(
                        "<tr{destaque}><td class=\"muted\">{}</td><td>{}</td>\
                         <td><code>{}</code></td><td>{}</td></tr>",
                        esc(&ev.created_at),
                        badge_level(&ev.level),
                        esc(&ev.scope),
                        celula_conteudo(&ev)
                    ));
                }
                body.push_str("</tbody></table>");
            }
            body.push_str("<div class=\"pager\">");
            if page > 1 {
                body.push_str(&format!(
                    "<a href=\"/events?{}\">← anterior</a>",
                    qs_eventos(&filtro, page - 1)
                ));
            }
            if page < paginas {
                body.push_str(&format!(
                    "<a href=\"/events?{}\">próxima →</a>",
                    qs_eventos(&filtro, page + 1)
                ));
            }
            body.push_str("</div>");
        }
        Err(e) => body.push_str(&format!(
            "<div class=\"empty err-t\">Falha lendo eventos: {}</div>",
            esc(&e)
        )),
    }

    // Auto-refresh SEM reload de página (26/10 — liberado JS mínimo pelo
    // Gabriel): refetch da própria URL a cada 5 s e troca só o corpo da
    // tabela + o cabeçalho de contagem. Pausável; aba oculta não refetch;
    // falha de rede fica visível no rótulo (nada engolido).
    // Detalhe `<details>` aberto congela a troca (31/10 — evento expandido
    // não pode fechar sozinho nem deslizar com eventos novos; a leitura
    // retoma no ciclo seguinte ao fechar). Checa no fetch E na chegada da
    // resposta, que pode voltar depois de um clique.
    body.push_str(
        r#"<div class="atualiza"><button id="btn-pausa" type="button">⏸ pausar</button>\
<span id="st-atualiza" class="muted">atualiza a cada 5 s</span></div>
<script>
(function () {
  var vivo = true;
  var btn = document.getElementById('btn-pausa');
  var st = document.getElementById('st-atualiza');
  btn.addEventListener('click', function () {
    vivo = !vivo;
    btn.textContent = vivo ? '⏸ pausar' : '▶ retomar';
    st.textContent = vivo ? 'atualiza a cada 5 s' : 'atualização pausada';
  });
  function detalheAberto() {
    return document.querySelector('#corpo-eventos details[open]');
  }
  setInterval(function () {
    if (!vivo || document.hidden) return;
    if (detalheAberto()) {
      st.textContent = 'pausado com detalhe aberto — fecha para retomar';
      return;
    }
    fetch(location.href).then(function (r) { return r.ok ? r.text() : null; })
      .then(function (txt) {
        if (!txt) return;
        if (detalheAberto()) {
          st.textContent = 'pausado com detalhe aberto — fecha para retomar';
          return;
        }
        var novo = new DOMParser().parseFromString(txt, 'text/html');
        var corpo = novo.getElementById('corpo-eventos');
        var cab = novo.getElementById('cabecalho-eventos');
        var atual = document.getElementById('corpo-eventos');
        var cabAtual = document.getElementById('cabecalho-eventos');
        if (corpo && atual) atual.innerHTML = corpo.innerHTML;
        if (cab && cabAtual) cabAtual.innerHTML = cab.innerHTML;
        st.textContent = 'atualiza a cada 5 s';
      })
      .catch(function (e) {
        st.textContent = 'falhou em atualizar (' + e + ') — tenta de novo no próximo ciclo';
      });
  }, 5000);
})();
</script>"#,
    );
    body.push_str("<p><a href=\"/\">← dashboard</a></p>");
    layout("Flux — eventos", &body, false)
}
