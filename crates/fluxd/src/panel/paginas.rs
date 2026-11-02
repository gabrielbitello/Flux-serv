//! Apresentação do painel (L6.3): o que sobrou de server-side — layout,
//! login e o CSS tail dos badges de sync (as páginas viraram shells nas
//! fatias 4/5/6a; isto morre quando o login for o único render restante).

use super::formato::esc;

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
/// Login (fatia 8): `turnstile_site` = widget do Cloudflare quando
/// configurado (`flux gate turnstile` — MESMAS chaves do gate de dev);
/// "manter conectado" = sessão de 30 dias.
pub(super) fn render_login(mensagem: Option<&str>, turnstile_site: Option<&str>) -> String {
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
<label style="display:flex;gap:.45rem;align-items:center;font-size:.85rem">
<input type="checkbox" name="manter" value="1" style="width:auto">
manter conectado (30 dias)
</label>"#,
    );
    if let Some(site) = turnstile_site {
        body.push_str(&format!(
            r#"<div class="cf-turnstile" data-sitekey="{site}" data-theme="auto"></div>
<script src="https://challenges.cloudflare.com/turnstile/v0/api.js" async defer></script>"#
        ));
    }
    body.push_str(
        r#"<button type="submit" style="background:var(--accentbg);color:var(--accent);
border:1px solid var(--accent);border-radius:6px;padding:.45rem .6rem;
font-weight:600;cursor:pointer">Entrar</button>
</form>"#,
    );
    layout("Flux — entrar", &body, false)
}

// --------------------------------------------------------------- dashboard

// ----------------------------------------------------------------- detalhe
