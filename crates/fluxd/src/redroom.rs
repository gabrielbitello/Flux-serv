//! Redroom (21/10): modo de contenção GLOBAL do fluxd — um interruptor, a
//! plataforma INTEIRA atrás de uma página estática. Ligado, o router devolve
//! a página para TODO request (vindo do túnel Cloudflare OU da rede local,
//! host gerenciado ou desconhecido, painel público incluso) ANTES da decisão
//! de host: nenhum container é acordado ou proxyado — a pilha toda (PHP
//! incluso) fica intocável enquanto o modo durar. O corte é GENERALISTA de
//! propósito (pedido do Gabriel): não existe exceção por projeto, ambiente
//! ou origem.
//!
//! O controle NÃO cai junto: a API de controle :7420 e o painel :7421 são
//! listeners próprios no 127.0.0.1 (`serve`, lib.rs), fora do router — é por
//! eles (túnel SSH) que o modo liga (`flux redroom on`) e desliga, inclusive
//! com a contenção no ar. `POST /__flux/beat` é rota real e não passa pelo
//! fallback de propósito: 204 sem tocar container, inofensivo sob contenção.
//!
//! Estado: chave `redroom` na tabela `settings` (k/v desde o schema v1) —
//! sobrevive a restart do fluxd, sem migration nova. A leitura por request é
//! um SELECT por PK; erro de leitura NÃO alterna a contenção sozinho: loga
//! `error` e segue SEM redroom (a plataforma não vira refém de falha
//! transitória de banco — e a falha aparece, nada silencioso).
//!
//! A página é HTML estático CONST: sem entrada do usuário, sem JS, sem
//! asset externo — não há o que escapar nem o que carregar. `503` honesto
//! (o serviço público está suspenso), `no-store` e noindex SEMPRE (página
//! de estado, nunca indexada — dev ou prod não muda nada aqui).

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use rusqlite::params;

use crate::state::{State, StateError};

/// Chave da flag na tabela `settings`. Valor `"1"` = ligado; qualquer outra
/// coisa (ausente incluída) = desligado — o default é a plataforma aberta.
const CHAVE: &str = "redroom";

/// Redroom ligado? `false` sem a chave, com valor estranho ou com o banco
/// falhando (nesse caso com `error` no log — ver doc do módulo).
pub fn ativo(state: &State) -> bool {
    match state.with(|c| {
        c.query_row("SELECT v FROM settings WHERE k = ?1", params![CHAVE], |r| {
            r.get::<_, String>(0)
        })
    }) {
        Ok(v) => v == "1",
        Err(StateError::Sql(rusqlite::Error::QueryReturnedNoRows)) => false,
        Err(e) => {
            tracing::error!(
                target: "redroom",
                "leitura da flag do redroom falhou — seguindo SEM contenção: {e}"
            );
            false
        }
    }
}

/// Liga (`true`) ou desliga (`false`) a contenção — upsert idempotente.
pub fn definir(state: &State, ligar: bool) -> Result<(), StateError> {
    state.with(|c| {
        c.execute(
            "INSERT INTO settings(k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            params![CHAVE, if ligar { "1" } else { "0" }],
        )?;
        Ok(())
    })
}

/// A página de contenção: `503` + HTML estático, `no-store`. Corpo fixo —
/// ver doc do módulo.
pub fn resposta() -> Response {
    let mut r = Response::new(Body::from(PAGINA));
    *r.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

const PAGINA: &str = r#"<!doctype html>
<html lang="pt-br">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow">
<title>modo redroom</title>
<style>
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;
background:radial-gradient(1100px 620px at 50% -12%,#2c070d,#0a0407 62%);color:#f3d9d9;
font-family:system-ui,-apple-system,"Segoe UI",sans-serif;text-align:center}
main{max-width:560px;padding:2.5rem 1.4rem}
.dot{width:14px;height:14px;border-radius:50%;background:#ef4444;margin:0 auto 1.5rem;
box-shadow:0 0 22px 5px rgba(239,68,68,.5);animation:pulso 1.6s ease-in-out infinite}
@keyframes pulso{0%,100%{opacity:1;transform:scale(1)}50%{opacity:.4;transform:scale(.8)}}
h1{font-size:1.35rem;font-weight:700;letter-spacing:.34em;margin:0 0 .9rem;color:#f87171}
p{margin:.45rem 0;line-height:1.6;font-size:.95rem;color:#e5c2c2}
footer{margin-top:2.4rem;font-size:.78rem;letter-spacing:.08em;color:#9c7575}
</style>
</head>
<body>
<main>
<div class="dot"></div>
<h1>MODO REDROOM</h1>
<p>O acesso a todos os sites e serviços desta plataforma está suspenso<br>em modo de contenção.</p>
<p>Volte mais tarde.</p>
<footer>FLUX</footer>
</main>
</body>
</html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    use crate::state::State;

    /// Default é aberto: sem chave, valor estranho e banco vazio = OFF —
    /// a flag nunca liga sozinha.
    #[test]
    fn default_e_off_e_so_o_1_liga() {
        let st = State::in_memory().unwrap();
        assert!(!ativo(&st), "sem chave = OFF");

        st.with(|c| c.execute("INSERT INTO settings(k, v) VALUES ('redroom', 'lixo')", []))
            .unwrap();
        assert!(!ativo(&st), "valor estranho = OFF (só \"1\" liga)");

        definir(&st, true).unwrap();
        assert!(ativo(&st), "definir(true) liga");

        definir(&st, false).unwrap();
        assert!(!ativo(&st), "definir(false) desliga");
    }

    /// O upsert é idempotente (re-rodar não duplica nem corrompe).
    #[test]
    fn definir_e_idempotente() {
        let st = State::in_memory().unwrap();
        for _ in 0..3 {
            definir(&st, true).unwrap();
        }
        let n: i64 = st
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM settings WHERE k = 'redroom'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(n, 1, "uma linha só");
        assert!(ativo(&st));
    }

    /// Forma da resposta: 503 honesto, no-store, HTML com noindex SEMPRE
    /// (estado temporário não é conteúdo) e a marca da página.
    #[tokio::test]
    async fn resposta_e_503_no_store_noindex_e_estatica() {
        let r = resposta();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            r.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let corpo = String::from_utf8(
            r.into_body()
                .collect()
                .await
                .expect("body coletável")
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(corpo.contains("MODO REDROOM"), "corpo: {corpo}");
        assert!(
            corpo.contains(r#"content="noindex, nofollow""#),
            "noindex sempre: {corpo}"
        );
        assert!(corpo.contains("FLUX"), "família: {corpo}");
        // Estática de verdade: sem JS e sem asset externo (o CSS é inline).
        assert!(!corpo.contains("<script"), "sem JS: {corpo}");
        assert!(
            !corpo.contains("http://") && !corpo.contains("https://"),
            "sem asset externo: {corpo}"
        );
    }
}
