//! Apresentação do painel (L6.3): layout, páginas e SVG — a CONSULTA é
//! leitura.rs, o formato é formato.rs, a montagem de rotas é mod.rs.

use axum::http::StatusCode;
use rusqlite::params;

use super::formato::{badge_level, dot, esc, like_esc, quando};
use super::leitura::{EventoRow, agora_epoch, ambiente, projetos, row_evento};
use crate::AppState;
use flux_common::config::ProjectToml;
use flux_common::protocol::Env;

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

// --------------------------------------------------------------- dashboard

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
