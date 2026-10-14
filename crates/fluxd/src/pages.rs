//! Páginas geradas pelo fluxd (03/15): família visual Flux, `meta refresh` 3 s
//! (zero JS obrigatório), `Cache-Control: no-store` — 200 não prova que o app
//! subiu. Em dev, toda página leva `noindex` no HTML (R22).

use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Json, Response};
use flux_common::config::DomainConfig;
use flux_common::protocol::{ServiceFailedBody, ServiceWakingBody};

use crate::decision::{Robots, Sitemap, WaitPhase};

const CSS: &str = "body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
background:linear-gradient(160deg,#0f172a,#1e293b);color:#e2e8f0;\
font-family:system-ui,-apple-system,sans-serif;text-align:center}\
main{max-width:640px;padding:2rem}h1{font-size:1.4rem;font-weight:600;margin:0 0 .5rem}\
p{color:#94a3b8;margin:.4rem 0;line-height:1.5}code{color:#7dd3fc;font-size:.95em}\
footer{margin-top:2.5rem;font-size:.8rem;color:#64748b;letter-spacing:.08em}";

fn html_page(title: &str, heading: &str, body_html: &str, dev: bool) -> String {
    let robots = if dev {
        r#"<meta name="robots" content="noindex, nofollow">"#
    } else {
        ""
    };
    format!(
        r#"<!doctype html>
<html lang="pt-br">
<head>
<meta charset="utf-8">
<meta http-equiv="refresh" content="3">
{robots}
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{CSS}</style>
</head>
<body>
<main>
<h1>{heading}</h1>
{body_html}
<footer>FLUX</footer>
</main>
</body>
</html>"#
    )
}

fn html_response(status: StatusCode, page: String) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Html(page)).into_response()
}

/// Página de espera — sempre HTTP 200, auto-refresh a cada 3 s (03).
pub fn wait(name: &str, phase: WaitPhase, dev: bool) -> Response {
    let (heading, detail) = match phase {
        WaitPhase::Waking => (
            format!("Acordando {name}…"),
            "Subindo o container. A página atualiza sozinha em 3 segundos.",
        ),
        WaitPhase::Provisioning => (
            format!("Preparando {name}…"),
            "Dependências e build em andamento. A página atualiza sozinha.",
        ),
    };
    html_response(
        StatusCode::OK,
        html_page(
            &format!("Flux — {name}"),
            &heading,
            &format!("<p>{detail}</p>"),
            dev,
        ),
    )
}

/// Página de estado "falhou" — 200 honesta, com apontamento para o painel (03).
pub fn failed(name: &str, dev: bool) -> Response {
    let body = format!(
        "<p><code>{name}</code> não subiu após o timeout de health.</p>\
<p>As últimas linhas de log do container aparecem aqui quando o módulo de \
Docker estiver no ar; o diagnóstico completo é no painel.</p>"
    );
    html_response(
        StatusCode::OK,
        html_page(
            &format!("Flux — {name}"),
            &format!("{name} não subiu"),
            &body,
            dev,
        ),
    )
}

/// Branch de worktree não existe localmente (03): página brandiada com a saída.
pub fn branch_missing(name: &str, branch: &str, dev: bool) -> Response {
    let body = format!(
        "<p>A branch <code>{branch}</code> do projeto <code>{name}</code> não \
está baixada localmente no Windows.</p>\
<p>Crie o worktree e a sync entrega: \
<code>git worktree add ../{name}-{branch} {branch}</code></p>"
    );
    html_response(
        StatusCode::OK,
        html_page(
            &format!("Flux — {name}"),
            "Branch não encontrada localmente",
            &body,
            dev,
        ),
    )
}

/// 404 brandiado "host desconhecido" — o único 404 da matriz (03).
pub fn not_found(host: &str) -> Response {
    let body = format!(
        "<p><code>{}</code> não é um host gerenciado pela plataforma.</p>",
        html_escape(host)
    );
    html_response(
        StatusCode::NOT_FOUND,
        html_page(
            "Flux — host desconhecido",
            "Host desconhecido",
            &body,
            false,
        ),
    )
}

/// 503 JSON + Retry-After para API parada/acordando (matriz 03/12).
pub fn api_waking(retry_after: u64) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, retry_after.to_string())],
        Json(ServiceWakingBody::waking(retry_after)),
    )
        .into_response()
}

/// 503 JSON sem Retry-After: container falhando de verdade.
pub fn api_failed() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ServiceFailedBody::default()),
    )
        .into_response()
}

/// robots.txt gerado (R22/R23) — `Cache-Control: public, max-age=3600`.
pub fn robots(kind: &Robots, domain: &DomainConfig) -> Response {
    let body = match kind {
        Robots::DisallowAll => "User-agent: *\nDisallow: /\n".to_owned(),
        Robots::ProdAllow { name } => format!(
            "User-agent: *\nAllow: /\nSitemap: https://{}/sitemap.xml\n",
            domain.prod_host(name)
        ),
    };
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "public, max-age=3600")],
        ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body),
    )
        .into_response()
}

/// sitemap.xml gerado (R23). Sem `lastmod` quando não há release conhecido —
/// dado ausente não vira dado inventado.
pub fn sitemap(kind: &Sitemap, domain: &DomainConfig) -> Response {
    let urlset_open = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#;
    let body = match kind {
        Sitemap::Empty => format!("{urlset_open}\n</urlset>\n"),
        Sitemap::Prod { name } => format!(
            "{urlset_open}\n  <url><loc>https://{}/</loc></url>\n</urlset>\n",
            domain.prod_host(name)
        ),
    };
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "public, max-age=3600")],
        ([(header::CONTENT_TYPE, "application/xml")], body),
    )
        .into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn domain() -> DomainConfig {
        DomainConfig::default()
    }

    #[tokio::test]
    async fn espera_tem_refresh_3s_no_store_e_noindex_em_dev() {
        let r = wait("vipers", WaitPhase::Waking, true);
        assert_eq!(r.status(), StatusCode::OK);
        let body =
            String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(
            body.contains(r#"http-equiv="refresh" content="3""#),
            "body: {body}"
        );
        assert!(body.contains("noindex"), "dev precisa de noindex: {body}");
        assert!(body.contains("Acordando vipers"), "body: {body}");
    }

    #[tokio::test]
    async fn espera_fora_de_dev_nao_leva_noindex() {
        let r = wait("vipers", WaitPhase::Waking, false);
        let body =
            String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(!body.contains("noindex"), "body: {body}");
    }

    #[tokio::test]
    async fn api_waking_tem_forma_exata_do_contrato() {
        let r = api_waking(30);
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let retry = r
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert_eq!(retry, "30");
        let body =
            String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert_eq!(body, r#"{"error":"service_waking","retry_after":30}"#);
    }

    #[tokio::test]
    async fn not_found_e_404_brandiado() {
        let r = not_found("zzz.bitello.xyz");
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let body =
            String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(body.contains("zzz.bitello.xyz"), "body: {body}");
        assert!(body.contains("Host desconhecido"));
    }

    #[tokio::test]
    async fn not_found_escapa_html_do_host() {
        let r = not_found("<script>alert(1)</script>.bitello.xyz");
        let body =
            String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(!body.contains("<script>alert"), "body: {body}");
    }

    #[tokio::test]
    async fn robots_dev_disallow_e_prod_allow_com_sitemap() {
        let dev = robots(&Robots::DisallowAll, &domain());
        let body = String::from_utf8(dev.into_body().collect().await.unwrap().to_bytes().to_vec())
            .unwrap();
        assert_eq!(body, "User-agent: *\nDisallow: /\n");

        let prod = robots(
            &Robots::ProdAllow {
                name: "vipers".into(),
            },
            &domain(),
        );
        let body = String::from_utf8(
            prod.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert_eq!(
            body,
            "User-agent: *\nAllow: /\nSitemap: https://vipers.bitello.xyz/sitemap.xml\n"
        );
    }

    #[tokio::test]
    async fn sitemap_prod_aponta_a_raiz() {
        let r = sitemap(
            &Sitemap::Prod {
                name: "vipers".into(),
            },
            &domain(),
        );
        let body =
            String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(
            body.contains("<loc>https://vipers.bitello.xyz/</loc>"),
            "body: {body}"
        );
    }
}
