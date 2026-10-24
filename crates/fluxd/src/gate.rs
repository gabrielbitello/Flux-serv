//! Gate do dev (24/10, pedido do Gabriel): o `-d` NÃO é público. Fonte de
//! fora da LAN enfrenta um desafio de SENHA em página HTML PURA servida pelo
//! fluxd (sem PHP no caminho): senha certa = IP confiável por 30 dias; 5
//! erros = IP banido, só sai com comando (`flux gate unban`); o form leva o
//! Turnstile do Cloudflare quando configurado. Este módulo é MECANISMO DE
//! SEGURANÇA — fail-closed em tudo: sem senha definida o desafio nunca
//! passa; captcha configurado é OBRIGATÓRIO no POST; erro de rede no
//! siteverify = recusa; hash da senha é Argon2id (mesma família do painel).
//!
//! Estado: tabela `gate_trust` (ip → expira), `gate_ban` (ip → eterno até
//! desbanir) e `gate_tentativa` (contador de falhas), DDL no `state.rs`.
//! Config: chaves na `settings` (`gate_senha_hash`, `gate_turnstile_site`,
//! `gate_turnstile_secret`). A verificação do Turnstile vai pelo cliente
//! HTTP puro do fluxd PARA A PONTE TLS do Caddy (127.0.0.1:2016 →
//! challenges.cloudflare.com) — o Rust não carrega stack TLS; a ponte é
//! parte do deploy (Caddyfile).

use std::sync::Arc;

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use rusqlite::OptionalExtension;

use crate::AppState;
use crate::events;
use crate::waf;

/// Chaves na tabela `settings`.
pub const CHAVE_SENHA: &str = "gate_senha_hash";
const CHAVE_TURNSTILE_SITE: &str = "gate_turnstile_site";
const CHAVE_TURNSTILE_SECRET: &str = "gate_turnstile_secret";

/// Falhas de senha/captcha antes do ban (pedido do Gabriel: "se errar 5x
/// bloqueia o ip"). Contador por IP, sem janela — só desban ou acerto zera.
pub const TETO_TENTATIVAS: i64 = 5;

/// Dias de confiança por IP após senha correta (pedido: 30).
pub const DIAS_CONFIANCA: i64 = 30;

/// Rota REAL do desafio (mesmo desenho do `/__flux/beat`: rota vence o
/// fallback — o POST do form nunca chega ao dispatch normal).
pub const PATH: &str = "/__flux/gate";

/// Ponte TLS do Caddy para o siteverify (deploy: listener 127.0.0.1:2016
/// proxyando `https://challenges.cloudflare.com`). HTTP puro no loopback —
/// o segredo do Turnstile não sai da VM fora de TLS terminado no Caddy.
const SITEVERIFY_PONTE: &str = "http://127.0.0.1:2016/turnstile/siteverify";

// ------------------------------------------------------------- estado do IP

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstadoIp {
    Banido,
    Confiavel,
    Livre,
}

/// Época em segundos pela MESMA fonte dos timestamps gravados. Erro de
/// relógio = `0` = qualquer confiança nasce já vencida (fail-closed).
fn agora(st: &crate::state::State) -> i64 {
    st.with(|c| {
        c.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |r| {
            r.get(0)
        })
    })
    .unwrap_or(0)
}

/// Estado do IP na régua do gate: ban vence confiança, confiança vencida é
/// apagada na leitura (poda preguiçosa do próprio IP consultado).
pub fn estado_ip(st: &crate::state::State, ip: &str) -> EstadoIp {
    if ip.is_empty() {
        return EstadoIp::Livre;
    }
    let banido: bool = st
        .with(|c| {
            Ok(
                c.query_row("SELECT 1 FROM gate_ban WHERE ip = ?1", [ip], |_| Ok(()))
                    .is_ok(),
            )
        })
        .unwrap_or(false);
    if banido {
        return EstadoIp::Banido;
    }
    let expira: Option<i64> = st
        .with(|c| {
            c.query_row(
                "SELECT expira_em FROM gate_trust WHERE ip = ?1",
                [ip],
                |r| r.get(0),
            )
            .optional()
        })
        .ok()
        .flatten();
    match expira {
        Some(e) if e > agora(st) => EstadoIp::Confiavel,
        Some(_) => {
            let _ = st.with(|c| c.execute("DELETE FROM gate_trust WHERE ip = ?1", [ip]));
            EstadoIp::Livre
        }
        None => EstadoIp::Livre,
    }
}

/// Falha a mais para o IP: `(banido_agora, falhas_acumuladas)`. Atingiu o
/// teto → contador zerado + linha em `gate_ban` (eterno até `desbanir`).
/// IP vazio não registra — banir "" não protege nada.
pub fn registrar_falha(
    st: &crate::state::State,
    ip: &str,
) -> Result<(bool, i64), crate::state::StateError> {
    if ip.is_empty() {
        return Ok((false, 0));
    }
    let t = agora(st);
    st.with(move |c| {
        c.execute(
            "INSERT INTO gate_tentativa(ip, falhas, atualizada_em) VALUES (?1, 1, ?2)
             ON CONFLICT(ip) DO UPDATE SET falhas = falhas + 1, atualizada_em = ?2",
            rusqlite::params![ip, t],
        )?;
        let falhas: i64 = c.query_row(
            "SELECT falhas FROM gate_tentativa WHERE ip = ?1",
            [ip],
            |r| r.get(0),
        )?;
        if falhas < TETO_TENTATIVAS {
            return Ok((false, falhas));
        }
        c.execute("DELETE FROM gate_tentativa WHERE ip = ?1", [ip])?;
        c.execute(
            "INSERT INTO gate_ban(ip, criado_em) VALUES (?1, ?2)
             ON CONFLICT(ip) DO UPDATE SET criado_em = ?2",
            rusqlite::params![ip, t],
        )?;
        Ok((true, falhas))
    })
}

/// Concede confiança ao IP por `dias` dias (upsert — reconfiança estende) e
/// zera o contador de falhas. Poda as confianças vencidas na mesma transação
/// (amortizada: quem ganha confiança limpa o passado).
pub fn confiar(
    st: &crate::state::State,
    ip: &str,
    dias: i64,
) -> Result<(), crate::state::StateError> {
    let t = agora(st);
    st.with(move |c| {
        c.execute(
            "INSERT INTO gate_trust(ip, expira_em, criado_em) VALUES (?1, ?2, ?3)
             ON CONFLICT(ip) DO UPDATE SET expira_em = ?2",
            rusqlite::params![ip, t + dias * 86_400, t],
        )?;
        c.execute("DELETE FROM gate_tentativa WHERE ip = ?1", [ip])?;
        c.execute("DELETE FROM gate_trust WHERE expira_em <= ?1", [t])?;
        Ok(())
    })
}

/// Remove o ban (comando do operador). `false` = o IP não estava banido.
pub fn desbanir(st: &crate::state::State, ip: &str) -> Result<bool, crate::state::StateError> {
    let n = st.with(|c| c.execute("DELETE FROM gate_ban WHERE ip = ?1", [ip]))?;
    if n > 0 {
        events::event(
            st,
            "info",
            "gate",
            &format!("gate: ban do IP {ip} removido por comando"),
        );
    }
    Ok(n > 0)
}

/// Revoga confiança (higiene: expulsar um IP antes do prazo). `false` = não
/// tinha confiança.
pub fn revogar(st: &crate::state::State, ip: &str) -> Result<bool, crate::state::StateError> {
    let n = st.with(|c| c.execute("DELETE FROM gate_trust WHERE ip = ?1", [ip]))?;
    if n > 0 {
        events::event(
            st,
            "info",
            "gate",
            &format!("gate: confiança do IP {ip} revogada por comando"),
        );
    }
    Ok(n > 0)
}

// ------------------------------------------------------------------- senha

/// Define (ou redefine) a senha do gate: hash Argon2id na `settings` — a
/// senha em claro NUNCA é gravada nem logada.
pub fn definir_senha(st: &crate::state::State, senha: &str) -> Result<(), String> {
    let salt = SaltString::generate(&mut rand_core::OsRng);
    let hash = Argon2::default()
        .hash_password(senha.as_bytes(), &salt)
        .map_err(|e| format!("gerando o hash argon2 da senha do gate: {e}"))?
        .to_string();
    st.with(|c| {
        c.execute(
            "INSERT INTO settings(k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = ?2",
            rusqlite::params![CHAVE_SENHA, hash],
        )?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    events::event(
        st,
        "warn",
        "gate",
        "gate: senha do desafio (re)definida por comando",
    );
    Ok(())
}

pub fn senha_definida(st: &crate::state::State) -> bool {
    settings_get(st, CHAVE_SENHA).is_some()
}

/// Verificação Argon2id. Hash ausente ou corrompido = SEMPRE inválido
/// (fail-closed) com erro no log — nunca pânico, nunca permitir.
pub fn verificar_senha(st: &crate::state::State, senha: &str) -> bool {
    let Some(hash) = settings_get(st, CHAVE_SENHA) else {
        tracing::warn!("gate: desafio respondido SEM senha definida — recusado (fail-closed)");
        return false;
    };
    let Ok(parsed) = PasswordHash::new(&hash) else {
        tracing::error!("gate: hash da senha ilegível na settings — recusado (fail-closed)");
        return false;
    };
    Argon2::default()
        .verify_password(senha.as_bytes(), &parsed)
        .is_ok()
}

fn settings_get(st: &crate::state::State, chave: &str) -> Option<String> {
    st.with(|c| {
        c.query_row("SELECT v FROM settings WHERE k = ?1", [chave], |r| r.get(0))
            .optional()
    })
    .ok()
    .flatten()
}

// --------------------------------------------------------------- turnstile

pub struct TurnstileConfig {
    pub site: Option<String>,
    pub secret: Option<String>,
}

pub fn turnstile_config(st: &crate::state::State) -> TurnstileConfig {
    TurnstileConfig {
        site: settings_get(st, CHAVE_TURNSTILE_SITE),
        secret: settings_get(st, CHAVE_TURNSTILE_SECRET),
    }
}

/// Grava o par do widget (a SECRET nunca vira log — só o fato de estar setada).
pub fn definir_turnstile(
    st: &crate::state::State,
    site: &str,
    secret: &str,
) -> Result<(), crate::state::StateError> {
    st.with(|c| {
        for (k, v) in [
            (CHAVE_TURNSTILE_SITE, site),
            (CHAVE_TURNSTILE_SECRET, secret),
        ] {
            c.execute(
                "INSERT INTO settings(k, v) VALUES (?1, ?2)
                 ON CONFLICT(k) DO UPDATE SET v = ?2",
                rusqlite::params![k, v],
            )?;
        }
        Ok(())
    })?;
    events::event(
        st,
        "info",
        "gate",
        "gate: Turnstile do Cloudflare configurado (captcha ativo no desafio)",
    );
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum VereditoTurnstile {
    /// Sem secret configurada — o form roda sem captcha (com warn no log).
    Desativado,
    Valido,
    Invalido(&'static str),
}

/// Verificação server-side do token do widget: POST à ponte TLS do Caddy →
/// `challenges.cloudflare.com/turnstile/v0/siteverify`. Erro de rede/timeout
/// = `Invalido` (fail-closed — mecanismo de segurança não tem modo aberto).
pub async fn turnstile_verificar(
    client: &crate::proxy::ProxyClient,
    secret: &str,
    token: &str,
    ip_remoto: &str,
) -> VereditoTurnstile {
    let corpo = format!(
        "secret={}&response={}&remoteip={}",
        urlencode(secret),
        urlencode(token),
        urlencode(ip_remoto)
    );
    let req = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri(SITEVERIFY_PONTE)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(corpo));
    let req = match req {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("gate: montando request do siteverify: {e}");
            return VereditoTurnstile::Invalido("captcha indisponível");
        }
    };
    let resp =
        match tokio::time::timeout(std::time::Duration::from_secs(10), client.request(req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::error!("gate: siteverify inalcançável (ponte do Caddy no ar?): {e}");
                return VereditoTurnstile::Invalido("captcha indisponível");
            }
            Err(_) => {
                tracing::error!("gate: siteverify excedeu 10s — recusado (fail-closed)");
                return VereditoTurnstile::Invalido("captcha indisponível");
            }
        };
    let bytes = match resp.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            tracing::error!("gate: corpo do siteverify ilegível: {e}");
            return VereditoTurnstile::Invalido("captcha indisponível");
        }
    };
    veredito_do_json(&bytes)
}

/// Parse do JSON do siteverify (`{"success": bool, ...}`). Campo ausente ou
/// JSON podre = `Invalido` — nunca é válido por acidente.
fn veredito_do_json(bytes: &[u8]) -> VereditoTurnstile {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) if v["success"].as_bool() == Some(true) => VereditoTurnstile::Valido,
        _ => VereditoTurnstile::Invalido("captcha inválido"),
    }
}

/// %XX só nos bytes que exigem — form-urlencoded mínimo para os 3 campos.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- handlers

/// Form do desafio. `cf-turnstile-response` é o nome que o widget do
/// Cloudflare publica; `retorno` é o caminho de origem (validado — ver
/// `retorno_validado`).
#[derive(serde::Deserialize)]
pub struct GateForm {
    #[serde(default)]
    pub senha: String,
    #[serde(rename = "cf-turnstile-response", default)]
    pub turnstile_token: Option<String>,
    #[serde(default)]
    pub retorno: Option<String>,
}

/// GET do desafio: página limpa (ou 403 se banido / 303 se já confiável) —
/// auditado como todo request externo.
pub async fn pagina(State(app): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let started = std::time::Instant::now();
    let host = host_de(&headers);
    let ip = waf::ip_real(&headers, None)
        .map(|i| i.to_string())
        .unwrap_or_default();
    let origem = events::origem_de(&headers, true);
    let mut status = StatusCode::UNAUTHORIZED;
    let r = match estado_ip(&app.state, &ip) {
        EstadoIp::Banido => {
            status = StatusCode::FORBIDDEN;
            resposta_bloqueado()
        }
        EstadoIp::Confiavel => {
            status = StatusCode::SEE_OTHER;
            redirecionar("/")
        }
        EstadoIp::Livre => resposta_desafio("/", &turnstile_config(&app.state), None),
    };
    events::access(
        &app.state,
        &host,
        "GET",
        PATH,
        status.as_u16(),
        started.elapsed().as_millis(),
        &origem,
    );
    r
}

/// POST do form: a cadeia inteira é fail-closed e TODA tentativa fica no log
/// com dump de headers (request externo = 100% auditável).
pub async fn processar(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<GateForm>,
) -> Response {
    let started = std::time::Instant::now();
    let host = host_de(&headers);
    let ip = waf::ip_real(&headers, None)
        .map(|i| i.to_string())
        .unwrap_or_default();
    let origem = events::origem_de(&headers, true);

    if estado_ip(&app.state, &ip) == EstadoIp::Banido {
        events::access(
            &app.state,
            &host,
            "POST",
            PATH,
            403,
            started.elapsed().as_millis(),
            &origem,
        );
        return resposta_bloqueado();
    }

    // 1) Turnstile: configurado = OBRIGATÓRIO; falha de captcha conta no
    //    mesmo contador de falhas (bot repetitivo também vira ban).
    let cfg = turnstile_config(&app.state);
    if let Some(secret) = &cfg.secret {
        let token = form.turnstile_token.as_deref().unwrap_or_default();
        let veredito = if token.is_empty() {
            VereditoTurnstile::Invalido("captcha ausente")
        } else {
            turnstile_verificar(&app.http, secret, token, &ip).await
        };
        if veredito != VereditoTurnstile::Valido {
            return falha(&app, &host, &ip, &origem, "captcha");
        }
    } else {
        tracing::warn!(
            "gate: desafio respondido SEM Turnstile configurado — defina com `flux gate turnstile <site> <secret>`"
        );
    }

    // 2) Senha (Argon2id). Sem senha definida = inválido (fail-closed).
    if !verificar_senha(&app.state, &form.senha) {
        return falha(&app, &host, &ip, &origem, "senha");
    }

    // 3) Acerto: confiança por 30 dias, contador zerado, redirect para o
    //    caminho de origem (validado, mesmo host — nada de open redirect).
    if let Err(e) = confiar(&app.state, &ip, DIAS_CONFIANCA) {
        tracing::error!("gate: gravando confiança de {ip}: {e}");
        events::access(
            &app.state,
            &host,
            "POST",
            PATH,
            500,
            started.elapsed().as_millis(),
            &origem,
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "falha gravando a confiança — erro no log do fluxd",
        )
            .into_response();
    }
    events::event(
        &app.state,
        "info",
        "gate",
        &format!("gate: IP {ip} confiável por {DIAS_CONFIANCA} dias"),
    );
    events::access(
        &app.state,
        &host,
        "POST",
        PATH,
        303,
        started.elapsed().as_millis(),
        &origem,
    );
    redirecionar(&retorno_validado(form.retorno.as_deref()))
}

/// Caminho comum de recusa: conta a falha, bane ao atingir o teto, devolve o
/// desafio com erro genérico — ou a página de bloqueio quando o teto fechou.
/// A auditoria do access é daqui (401 recusa / 403 ban).
fn falha(
    app: &Arc<AppState>,
    host: &str,
    ip: &str,
    origem: &events::Origem,
    motivo: &str,
) -> Response {
    let (banido, falhas) = registrar_falha(&app.state, ip).unwrap_or_else(|e| {
        tracing::error!("gate: registrando falha de {ip}: {e}");
        (false, 0)
    });
    events::event(
        &app.state,
        "warn",
        "gate",
        &format!("gate: {motivo} incorreta do IP {ip} ({falhas}/{TETO_TENTATIVAS}) host={host}"),
    );
    let (status, r) = if banido {
        events::event(
            &app.state,
            "warn",
            "gate",
            &format!(
                "gate: IP {ip} BANIDO após {TETO_TENTATIVAS} falhas — desbloqueia com `flux gate unban {ip}`"
            ),
        );
        (403, resposta_bloqueado())
    } else {
        let cfg = turnstile_config(&app.state);
        (
            401,
            resposta_desafio("/", &cfg, Some("senha ou captcha incorretos")),
        )
    };
    events::access(&app.state, host, "POST", PATH, status, 0, origem);
    r
}

// ---------------------------------------------------------------- respostas

/// Validador do retorno pós-senha: caminho RELATIVO do mesmo host, do `/`
/// pra frente, sem `//`, `\` ou `:` — open redirect não existe aqui.
fn retorno_validado(r: Option<&str>) -> String {
    let Some(r) = r else {
        return "/".to_owned();
    };
    let ok = r.starts_with('/')
        && !r.starts_with("//")
        && !r.contains('\\')
        && !r.contains(':')
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-._~?=&%@+;!,*()'[]".contains(&b));
    if ok { r.to_owned() } else { "/".to_owned() }
}

fn redirecionar(para: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = header::HeaderValue::from_str(para) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    r
}

fn host_de(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

/// Headers de segurança do desafio (página de MECANISMO DE SEGURANÇA): CSP
/// sem inline e sem nada além do widget da Cloudflare, sem cache, sem frame,
/// sem sniff, sem index, sem referrer.
fn cabec_seguras(r: &mut Response) {
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(
            "default-src 'none'; frame-src https://challenges.cloudflare.com; \
             script-src https://challenges.cloudflare.com; style-src 'none'; \
             img-src 'none'; connect-src 'none'; form-action 'self'; \
             base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    h.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    h.insert(
        header::HeaderName::from_static("x-robots-tag"),
        header::HeaderValue::from_static("noindex, nofollow"),
    );
    h.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
}

/// Página do desafio: HTML puro, form POSTando na rota real do gate. O erro
/// é frase FIXA (nunca ecoa entrada). Widget do Turnstile só quando há site
/// key configurada.
pub fn resposta_desafio(retorno: &str, cfg: &TurnstileConfig, erro: Option<&str>) -> Response {
    let widget = match &cfg.site {
        Some(site) => format!(
            "<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\" async defer></script>\
             <div class=\"cf-turnstile\" data-sitekey=\"{}\" data-theme=\"dark\"></div>",
            esc(site)
        ),
        None => String::new(),
    };
    let aviso_erro = erro
        .map(|_| "<p class=\"err\">senha ou captcha incorretos — tente de novo</p>")
        .unwrap_or_default();
    let corpo = format!(
        "<!doctype html><html lang=\"pt-BR\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Acesso restrito</title>\
         <style>body{{font-family:system-ui,sans-serif;background:#111;color:#eee;\
         display:grid;place-items:center;min-height:100vh;margin:0}}\
         form{{background:#1c1c1c;padding:2rem;border-radius:8px;max-width:20rem}}\
         input{{width:100%;padding:.5rem;margin:.4rem 0 1rem;box-sizing:border-box;\
         background:#000;color:#eee;border:1px solid #444;border-radius:4px}}\
         button{{width:100%;padding:.6rem;background:#2d6cdf;color:#fff;border:0;\
         border-radius:4px;cursor:pointer}}\
         .err{{color:#ff7b72;font-size:.9rem}}</style></head><body>\
         <form method=\"POST\" action=\"{PATH}\">\
         <h1>Acesso restrito</h1>\
         <p>Este ambiente de desenvolvimento é privado. Informe a senha de acesso.</p>\
         {aviso_erro}{widget}\
         <input type=\"password\" name=\"senha\" autocomplete=\"off\" required autofocus>\
         <input type=\"hidden\" name=\"retorno\" value=\"{}\">\
         <button type=\"submit\">Entrar</button></form></body></html>",
        esc(retorno),
    );
    let mut r = (StatusCode::UNAUTHORIZED, corpo).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    cabec_seguras(&mut r);
    r
}

/// Página de IP banido: frase fixa, sem hint do mecanismo.
pub fn resposta_bloqueado() -> Response {
    let corpo = "<!doctype html><html lang=\"pt-BR\"><head><meta charset=\"utf-8\">\
         <title>Bloqueado</title></head><body>\
         <h1>Acesso bloqueado</h1>\
         <p>Este endereço excedeu as tentativas e foi bloqueado.</p>\
         </body></html>"
        .to_string();
    let mut r = (StatusCode::FORBIDDEN, corpo).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    cabec_seguras(&mut r);
    r
}

/// Esc HTML mínimo (mesma semântica das outras páginas da casa).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use flux_common::config::GlobalConfig;

    fn app() -> Arc<AppState> {
        Arc::new(AppState::new(
            crate::state::State::in_memory().unwrap(),
            &GlobalConfig::default(),
            None,
        ))
    }

    fn state(app: &Arc<AppState>) -> &crate::state::State {
        &app.state
    }

    fn headers_host_ip(host: &str, ip: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        h.insert("cf-connecting-ip", HeaderValue::from_str(ip).unwrap());
        h
    }

    #[test]
    fn senha_define_verifica_e_falha_com_hash_ausente() {
        let app = app();
        assert!(!senha_definida(state(&app)), "sem definir = false");
        assert!(
            !verificar_senha(state(&app), "qualquer"),
            "sem hash = fail-closed"
        );
        definir_senha(state(&app), "segredo-forte-123").unwrap();
        assert!(senha_definida(state(&app)));
        assert!(verificar_senha(state(&app), "segredo-forte-123"));
        assert!(!verificar_senha(state(&app), "errada"));
    }

    #[test]
    fn cinco_falhas_banem_e_desbanir_livra() {
        let app = app();
        for i in 1..=4 {
            let (banido, falhas) = registrar_falha(state(&app), "203.0.113.9").unwrap();
            assert!(!banido, "falha {i} não bane");
            assert_eq!(falhas, i);
            assert_eq!(estado_ip(state(&app), "203.0.113.9"), EstadoIp::Livre);
        }
        let (banido, _) = registrar_falha(state(&app), "203.0.113.9").unwrap();
        assert!(banido, "a 5ª falha bane");
        assert_eq!(estado_ip(state(&app), "203.0.113.9"), EstadoIp::Banido);
        // Falha num IP já banido: contador foi zerado no ban — se um dia for
        // desbanido, recomeça do zero, sem infla fantasma.
        assert!(!registrar_falha(state(&app), "203.0.113.9").unwrap().0);
        assert!(desbanir(state(&app), "203.0.113.9").unwrap());
        assert!(
            !desbanir(state(&app), "203.0.113.9").unwrap(),
            "2º unban = false"
        );
        assert_eq!(estado_ip(state(&app), "203.0.113.9"), EstadoIp::Livre);
    }

    #[test]
    fn confianca_expira_e_revoga() {
        let app = app();
        confiar(state(&app), "198.51.100.7", DIAS_CONFIANCA).unwrap();
        assert_eq!(estado_ip(state(&app), "198.51.100.7"), EstadoIp::Confiavel);
        assert!(revogar(state(&app), "198.51.100.7").unwrap());
        assert_eq!(estado_ip(state(&app), "198.51.100.7"), EstadoIp::Livre);

        // Expirada: linha fica no banco, mas a leitura devolve Livre e apaga.
        let passado = agora(state(&app)) - 10;
        app.state
            .with(|c| {
                c.execute(
                    "INSERT INTO gate_trust(ip, expira_em, criado_em) VALUES ('198.51.100.8', ?1, ?1)",
                    [passado],
                )
            })
            .unwrap();
        assert_eq!(estado_ip(state(&app), "198.51.100.8"), EstadoIp::Livre);
        let sobra: i64 = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM gate_trust WHERE ip = '198.51.100.8'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert_eq!(sobra, 0, "confiança vencida é apagada na leitura");
    }

    #[test]
    fn acerto_zera_contador_de_falhas() {
        let app = app();
        for _ in 0..3 {
            registrar_falha(state(&app), "198.51.100.9").unwrap();
        }
        confiar(state(&app), "198.51.100.9", DIAS_CONFIANCA).unwrap();
        let falhas: Option<i64> = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT falhas FROM gate_tentativa WHERE ip = '198.51.100.9'",
                    [],
                    |r| r.get(0),
                )
                .optional()
            })
            .unwrap();
        assert_eq!(falhas, None, "confiança zera o contador");
    }

    #[test]
    fn ip_vazio_nunca_bane_nem_confia() {
        let app = app();
        for _ in 0..(TETO_TENTATIVAS + 1) {
            assert!(!registrar_falha(state(&app), "").unwrap().0);
        }
        assert_eq!(estado_ip(state(&app), ""), EstadoIp::Livre);
    }

    #[test]
    fn retorno_validado_s_aceita_caminho_relativo_seguro() {
        assert_eq!(retorno_validado(Some("/cms")), "/cms");
        assert_eq!(retorno_validado(Some("/a/b?x=1&y=2")), "/a/b?x=1&y=2");
        assert_eq!(retorno_validado(Some("//evil.com")), "/");
        assert_eq!(retorno_validado(Some("https://evil.com")), "/");
        assert_eq!(retorno_validado(Some("/a:b")), "/");
        assert_eq!(retorno_validado(Some("/a\\b")), "/");
        assert_eq!(retorno_validado(None), "/");
    }

    #[test]
    fn veredito_do_json_do_siteverify() {
        assert_eq!(
            veredito_do_json(b"{\"success\":true}"),
            VereditoTurnstile::Valido
        );
        assert_eq!(
            veredito_do_json(b"{\"success\":false,\"error-codes\":[\"invalid-input-response\"]}"),
            VereditoTurnstile::Invalido("captcha inválido")
        );
        assert_eq!(
            veredito_do_json(b"{}"),
            VereditoTurnstile::Invalido("captcha inválido"),
            "campo ausente nunca é válido"
        );
        assert_eq!(
            veredito_do_json(b"html podre"),
            VereditoTurnstile::Invalido("captcha inválido")
        );
    }

    #[tokio::test]
    async fn desafio_tem_csp_form_hidden_sem_eco_de_host() {
        let cfg = TurnstileConfig {
            site: Some("0xSITE".into()),
            secret: None,
        };
        let r = resposta_desafio("/cms", &cfg, None);
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let csp = r
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            csp.contains("frame-src https://challenges.cloudflare.com"),
            "CSP: {csp}"
        );
        assert_eq!(r.headers().get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
        let corpo = String::from_utf8(
            axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(corpo.contains("data-sitekey=\"0xSITE\""), "widget: {corpo}");
        assert!(corpo.contains("name=\"senha\""), "campo senha: {corpo}");
        assert!(corpo.contains("value=\"/cms\""), "retorno: {corpo}");
    }

    #[test]
    fn bloqueado_e_403_com_noindex() {
        let r = resposta_bloqueado();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            r.headers().get("x-robots-tag").unwrap(),
            "noindex, nofollow"
        );
    }

    // ------------------------------------------------- fluxo do POST (e2e)

    #[tokio::test]
    async fn post_errado_conta_post_correto_confia() {
        let app = app();
        definir_senha(state(&app), "senha-do-gabriel").unwrap();
        let h = headers_host_ip("vipers-d.bitello.xyz", "203.0.113.5");

        // Errada: 401, sem confiança, com access auditado.
        let r = processar(
            State(Arc::clone(&app)),
            h.clone(),
            Form(GateForm {
                senha: "errada".into(),
                turnstile_token: None,
                retorno: Some("/cms".into()),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(estado_ip(state(&app), "203.0.113.5"), EstadoIp::Livre);

        // Correta: 303 para /cms e confiança concedida.
        let r = processar(
            State(Arc::clone(&app)),
            h.clone(),
            Form(GateForm {
                senha: "senha-do-gabriel".into(),
                turnstile_token: None,
                retorno: Some("/cms".into()),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(estado_ip(state(&app), "203.0.113.5"), EstadoIp::Confiavel);

        // Auditoria: o access do acerto carrega o dump (request externo).
        let msg: String = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT message FROM events WHERE scope='access' ORDER BY id DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert!(msg.contains("POST /__flux/gate 303"), "audit: {msg}");
        assert!(msg.contains("hdrs["), "externo carrega dump: {msg}");
        assert!(
            msg.contains("cf-connecting-ip=203.0.113.5"),
            "ip no dump: {msg}"
        );
    }

    #[tokio::test]
    async fn cinco_erros_no_form_banem_o_ip() {
        let app = app();
        definir_senha(state(&app), "s").unwrap();
        let h = headers_host_ip("vipers-d.bitello.xyz", "203.0.113.77");
        for i in 0..5 {
            let r = processar(
                State(Arc::clone(&app)),
                h.clone(),
                Form(GateForm {
                    senha: "chute".into(),
                    turnstile_token: None,
                    retorno: None,
                }),
            )
            .await;
            let esperado = if i == 4 { 403 } else { 401 };
            assert_eq!(r.status().as_u16(), esperado, "tentativa {}", i + 1);
        }
        assert_eq!(estado_ip(state(&app), "203.0.113.77"), EstadoIp::Banido);
        let warn: String = app
            .state
            .with(|c| {
                c.query_row(
                    "SELECT message FROM events WHERE scope='gate' AND message LIKE '%BANIDO%'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert!(warn.contains("203.0.113.77"), "evento do ban: {warn}");
    }

    #[tokio::test]
    async fn sem_senha_definida_o_desafio_nunca_passa() {
        let app = app();
        let h = headers_host_ip("vipers-d.bitello.xyz", "203.0.113.5");
        let r = processar(
            State(Arc::clone(&app)),
            h,
            Form(GateForm {
                senha: "chute".into(),
                turnstile_token: None,
                retorno: None,
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "fail-closed");
        assert_eq!(estado_ip(state(&app), "203.0.113.5"), EstadoIp::Livre);
    }

    #[tokio::test]
    async fn banido_recebe_403_ate_antes_de_validar_senha() {
        let app = app();
        definir_senha(state(&app), "certa").unwrap();
        registrar_falha(state(&app), "203.0.113.9");
        registrar_falha(state(&app), "203.0.113.9");
        registrar_falha(state(&app), "203.0.113.9");
        registrar_falha(state(&app), "203.0.113.9");
        registrar_falha(state(&app), "203.0.113.9");
        let h = headers_host_ip("vipers-d.bitello.xyz", "203.0.113.9");
        let r = processar(
            State(Arc::clone(&app)),
            h,
            Form(GateForm {
                senha: "certa".into(),
                turnstile_token: None,
                retorno: None,
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN, "ban vence senha certa");
        assert_eq!(estado_ip(state(&app), "203.0.113.9"), EstadoIp::Banido);
    }
}
