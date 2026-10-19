//! Símbolos da família visual ADMIN (L4.4): painel + /observe + router.
//! NÃO é "design system": são os símbolos que existiam em cópias múltiplas
//! (NOINDEX/x_robots_tag 3×; CSS admin 2× JÁ divergidas em silêncio enquanto
//! observe_page declarava "cópia verbatim"). As DUAS famílias visuais do
//! fluxd continuam separadas: pública = pages.rs/edge.rs/waf.rs (croma
//! próprio, L4.2); admin = este módulo + tails exclusivos por página.
//!
//! Corte do CSS (decisão do passo): CSS_ADMIN é a UNIÃO dos tokens/regras
//! comuns — `--attn` entra também no /observe sem uso (inofensivo, custo de
//! bytes); os tails exclusivos (badges de sync no painel; form.filtros e
//! pre no /observe) ficam em cada arquivo. A ORDEM das regras muda com a
//! extração; seletores e valores são preservados — os testes de composição
//! aqui e os testes de conteúdo das duas páginas são a rede.

use axum::http::HeaderName;

/// R22 admin: painel e /observe são noindex SEMPRE (a família pública é
/// noindex só em dev — política diferente, por isso NÃO se unifica).
pub(crate) const NOINDEX: &str = "noindex, nofollow, noarchive";

pub(crate) fn x_robots_tag() -> HeaderName {
    HeaderName::from_static("x-robots-tag")
}

/// Tokens + regras base do admin. CADA cor do light tem par no dark (regra
/// de UI do Gabriel). `--attn` é do painel; no /observe entra sem uso.
pub(crate) const CSS_ADMIN: &str = r#"

:root{
--bg:#f1f5f9;--card:#ffffff;--text:#0f172a;--muted:#64748b;--line:#e2e8f0;
--accent:#2563eb;--accentbg:#dbeafe;--ok:#15803d;--okbg:#dcfce7;
--warn:#b45309;--warnbg:#fef3c7;--err:#b91c1c;--errbg:#fee2e2;
--info:#0e7490;--infobg:#cffafe;--idle:#475569;--idlebg:#e2e8f0;
--attn:#c2410c;--attnbg:#ffedd5}
@media (prefers-color-scheme:dark){:root{
--bg:#0b1220;--card:#121b2f;--text:#e2e8f0;--muted:#94a3b8;--line:#26344f;
--accent:#93c5fd;--accentbg:#1e3a8a;--ok:#4ade80;--okbg:#14532d;
--warn:#fbbf24;--warnbg:#451a03;--err:#fca5a5;--errbg:#450a0a;
--info:#67e8f9;--infobg:#164e63;--idle:#94a3b8;--idlebg:#1e293b;
--attn:#fb923c;--attnbg:#431407}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);
font:15px/1.55 system-ui,-apple-system,"Segoe UI",sans-serif;padding-bottom:3rem}
.wrap{max-width:1060px;margin:0 auto;padding:1.2rem}
header.top{display:flex;gap:1.2rem;align-items:baseline;flex-wrap:wrap;
border-bottom:1px solid var(--line);padding-bottom:.6rem;margin-bottom:1rem}
header.top h1{font-size:1.15rem;margin:0}
nav a{margin-right:.9rem;color:var(--accent);text-decoration:none}
nav a:hover{text-decoration:underline}
.aviso{background:var(--warnbg);color:var(--warn);border:1px solid var(--warn);
border-radius:8px;padding:.55rem .8rem;margin:0 0 1rem;font-size:.88rem}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(330px,1fr));gap:1rem}
.card{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:.9rem 1rem}
.card h3{margin:0 0 .5rem;font-size:1rem;display:flex;gap:.5rem;align-items:center;flex-wrap:wrap}
.card a{color:var(--accent);text-decoration:none}
.card a:hover{text-decoration:underline}
a{color:var(--accent)}
.badge{background:var(--accentbg);color:var(--accent);border-radius:999px;
padding:.05rem .55rem;font-size:.72rem;font-weight:600}
.dot{display:inline-block;width:.62rem;height:.62rem;border-radius:50%;margin-right:.35rem}
.dot.ok{background:var(--ok)}.dot.warn{background:var(--warn)}.dot.err{background:var(--err)}
.dot.info{background:var(--info)}.dot.accent{background:var(--accent)}.dot.idle{background:var(--idle)}
table{width:100%;border-collapse:collapse;font-size:.86rem}
th,td{text-align:left;padding:.3rem .5rem;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--muted);font-weight:600}
.urls{margin:.6rem 0 0;font-size:.82rem;color:var(--muted);word-break:break-all}
section{margin-top:1.6rem}
section h2{font-size:1rem;margin:0 0 .6rem}
.muted{color:var(--muted)}
.ok-t{color:var(--ok)}.warn-t{color:var(--warn)}.err-t{color:var(--err)}
.empty{background:var(--card);border:1px dashed var(--line);border-radius:10px;
padding:1.4rem;text-align:center;color:var(--muted)}
.curve{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:.8rem}
.curve svg{display:block;width:100%;height:auto}
.curve polyline{stroke:var(--accent);fill:none;stroke-width:1.5}
.curve .eixo{stroke:var(--line)}
.curve text{fill:var(--muted);font-size:10px}
.pager{display:flex;gap:1rem;margin-top:.8rem}
code{background:var(--idlebg);color:var(--text);border-radius:4px;padding:.05rem .3rem;font-size:.85em}
footer{margin-top:2rem;color:var(--muted);font-size:.78rem}
@media(max-width:640px){.wrap{padding:.7rem}table{font-size:.8rem}}

"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Composição: os tokens compartilhados NUNCA se perdem no corte.
    #[test]
    fn css_admin_tem_os_tokens_do_contrato() {
        assert!(CSS_ADMIN.contains("--accent:#2563eb"), "token accent");
        assert!(
            CSS_ADMIN.contains("--attnbg:#ffedd5}"),
            "fechamento do :root light"
        );
        assert!(
            CSS_ADMIN.contains("--attnbg:#431407}}"),
            "fechamento do :root dark"
        );
        assert!(
            CSS_ADMIN.contains("footer{margin-top:2rem;color:var(--muted)"),
            "footer base do admin"
        );
    }
}
