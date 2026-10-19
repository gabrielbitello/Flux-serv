//! observe_page — página `/observe` do painel (spec 13): o monitor 24 h.
//! Mesma escola do `panel.rs` (08): HTML gerado server-side, SVG desenhado
//! aqui no servidor, CSS próprio embutido como `const`, ZERO JavaScript — o
//! formulário de filtros é GET puro (funciona com JS desligado) e os links de
//! drill só setam a query string. Toda resposta leva `no-store` +
//! `X-Robots-Tag: noindex, nofollow, noarchive` (R22), igual ao painel.
//!
//! Do GERAL ao drill fino: curva da série filtrada (projeto/container x
//! métrica x janela) → mini-curvas por container do projeto (link "focar"
//! seta `?container=`) → tabela dos PICOS da janela com `<details>` abrindo o
//! snapshot JSON da perícia (o que a imagem e as irmãs faziam no pico).
//!
//! Este módulo é CONSUMIDOR do `observe` (consultas prontas: `serie`,
//! `series_por_container`, `peaks`, `peak_snapshot`) — nada de SQL de métrica
//! aqui, uma só fonte de verdade para leitura. A montagem da rota dentro do
//! painel (auth herdada do guard de sessão) é feita em `panel::build_panel`.
//!
//! Honestidade dos dados (regra da casa): sem coleta ainda se diz "sem coleta
//! ainda — o sampler sobe com o fluxd"; container/projeto sem amostra na
//! janela não desenha curva mentirosa; erro de leitura aparece na página com
//! a mensagem real, nunca silencioso.

use std::sync::Arc;

use axum::extract::{RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde_json::Value;

use crate::AppState;
use crate::observe::{self, Metrica, PeakRow};
use crate::ui::{NOINDEX, x_robots_tag};

/// Geometria da curva grande (idêntica à do dashboard) e da mini-curva.
const CURVA_W: u32 = 720;
const CURVA_H: u32 = 120;
const MINI_W: u32 = 240;
const MINI_H: u32 = 80;
/// Janelas suportadas em minutos (`?janela=`, default 24 h).
const JANELAS: [(i64, &str); 3] = [(60, "1 h"), (360, "6 h"), (1440, "24 h")];
const JANELA_DEFAULT: i64 = 1440;

/// Design tokens — cópia VERBATIM da `CSS` do `panel.rs` (mesma fonte de
/// verdade visual; CADA cor do light tem par no dark) + um bloco mínimo no fim
/// para os controles do formulário de filtros e o `<pre>` do snapshot,
/// usando SÓ os tokens que já existem.
const CSS_TAIL: &str = r#"/* --- filtros e snapshot (só tokens existentes; painel não tem form GET) --- */
form.filtros{display:flex;gap:.8rem;flex-wrap:wrap;align-items:end;margin-top:.8rem}
form.filtros label{display:flex;flex-direction:column;gap:.25rem;
font-size:.78rem;color:var(--muted)}
form.filtros select,form.filtros input{background:var(--bg);color:var(--text);
border:1px solid var(--line);border-radius:6px;padding:.35rem .5rem;font:inherit}
form.filtros button{background:var(--accentbg);color:var(--accent);
border:1px solid var(--accent);border-radius:6px;padding:.35rem .8rem;
cursor:pointer;font-weight:600}
details pre{background:var(--idlebg);color:var(--text);border-radius:6px;
padding:.6rem;overflow:auto;font-size:.78rem;max-width:640px;margin:.4rem 0 0}"#;

// ------------------------------------------------------------------ handler

/// `GET /observe` — montada DENTRO do painel (`panel::build_panel`), o guard
/// de sessão roda ANTES daqui: auth é herdada, este handler não re-checa.
pub async fn pagina(State(app): State<Arc<AppState>>, RawQuery(q): RawQuery) -> Response {
    let filtro = filtro_da(q.as_deref());
    resposta(StatusCode::OK, render_monitor(&app, &filtro))
}

/// Toda resposta passa por aqui: no-store (estado vivo), noindex no header E
/// no HTML (R22) + no-referrer — mesmo trio do painel.
fn resposta(status: StatusCode, page: String) -> Response {
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (x_robots_tag(), NOINDEX),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(page),
    )
        .into_response()
}

// ------------------------------------------------------------------ filtros

/// Filtro da página, vindo da query string. Tudo clampado: valor inválido
/// cai no default honesto, nunca em erro 500 — a página sempre responde.
struct Filtro {
    projeto: Option<String>,
    container: Option<String>,
    metrica: Metrica,
    minutos: i64,
}

impl Filtro {
    fn default_() -> Self {
        Self {
            projeto: None,
            container: None,
            metrica: Metrica::Cpu,
            minutos: JANELA_DEFAULT,
        }
    }

    /// Escopo legível para títulos: o que esta curva representa.
    fn escopo(&self) -> String {
        match (&self.projeto, &self.container) {
            (_, Some(c)) => format!("container {c}"),
            (Some(p), None) => format!("projeto {p}"),
            (None, None) => "todos os projetos".into(),
        }
    }
}

/// Igualdade campo a campo — `Metrica` é da frente do `observe`; aqui o
/// comparador vive só nos testes (o render nunca precisa de ==).
#[cfg(test)]
fn mesmo_filtro(a: &Filtro, b: &Filtro) -> bool {
    let mesma_metrica = matches!(
        (&a.metrica, &b.metrica),
        (Metrica::Cpu, Metrica::Cpu)
            | (Metrica::Mem, Metrica::Mem)
            | (Metrica::NetRx, Metrica::NetRx)
            | (Metrica::NetTx, Metrica::NetTx)
    );
    a.projeto == b.projeto && a.container == b.container && mesma_metrica && a.minutos == b.minutos
}

/// Primeiro valor de `?chave=` na query CRU (split ANTES do decode — `&` e
/// `=` codificados dentro do valor não quebram o parse). Valor vazio vira
/// `Some("")` — quem decide se vira `None` é o filtro (select "todos").
fn param<'a>(query: &'a str, chave: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| kv.strip_prefix(chave))
}

/// `%XX` e `+` → texto (filtros são GET de formulário; o browser codifica).
/// `%` solto ou hexadecimal inválido volta literal — nunca pânico.
fn url_decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let hex = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Query string → filtro com defaults (janela 24 h, métrica cpu, sem recorte).
fn filtro_da(query: Option<&str>) -> Filtro {
    let Some(q) = query else {
        return Filtro::default_();
    };
    let texto = |chave: &str| param(q, chave).map(url_decode);
    let projeto = texto("projeto=")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let container = texto("container=")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let metrica = texto("metrica=")
        .and_then(|s| metrica_de(&s))
        .unwrap_or(Metrica::Cpu);
    let minutos = texto("janela=")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|m| JANELAS.iter().any(|(min, _)| min == m))
        .unwrap_or(JANELA_DEFAULT);
    Filtro {
        projeto,
        container,
        metrica,
        minutos,
    }
}

/// String do select → `Metrica` (valores aceitos: cpu/mem/net_rx/net_tx).
fn metrica_de(s: &str) -> Option<Metrica> {
    match s {
        "cpu" => Some(Metrica::Cpu),
        "mem" => Some(Metrica::Mem),
        "net_rx" => Some(Metrica::NetRx),
        "net_tx" => Some(Metrica::NetTx),
        _ => None,
    }
}

/// Rótulo humano da métrica (títulos, tabela de picos, aria-label).
fn rotulo_metrica(m: Metrica) -> &'static str {
    match m {
        Metrica::Cpu => "CPU",
        Metrica::Mem => "memória",
        Metrica::NetRx => "rede rx",
        Metrica::NetTx => "rede tx",
    }
}

/// Unidade da métrica conforme o rollup da spec 13 (cpu_pct, mem_mb, *_kb).
fn unidade(m: Metrica) -> &'static str {
    match m {
        Metrica::Cpu => "%",
        Metrica::Mem => "MB",
        Metrica::NetRx | Metrica::NetTx => "kB",
    }
}

/// Valor legível: 1 casa até 999, inteiro acima (100 % e 12 345 kB não
/// precisam de decimal mentindo precisão).
fn fmt_valor(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// `60/360/1440` → rótulo da janela (rótulo de janela fora da lista é
/// montado honestamente em minutos).
fn janela_label(minutos: i64) -> String {
    JANELAS
        .iter()
        .find(|(min, _)| *min == minutos)
        .map(|(_, rot)| (*rot).to_owned())
        .unwrap_or_else(|| format!("{minutos} min"))
}

// ------------------------------------------------------------------ leitura

/// Época atual em segundos, pela MESMA fonte dos timestamps gravados
/// (relógio único com o painel — `strftime('now')` é UTC no SQLite).
fn agora_epoch(app: &AppState) -> Result<i64, String> {
    app.state
        .with(|c| {
            c.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)", [], |r| {
                r.get(0)
            })
        })
        .map_err(|e| e.to_string())
}

/// Projetos registrados para o `<select>` do filtro (nomes em ordem).
fn projetos(app: &AppState) -> Result<Vec<String>, String> {
    app.state.list_projects().map_err(|e| e.to_string())
}

// --------------------------------------------------------------- desenho SVG

/// Curva SVG server-side (padrão do panel.rs: polyline + eixo + rótulos).
/// A série chega esparsa (só os minutos coletados): o eixo x é NORMALIZADO no
/// tempo da janela `[t0, t1]` — buraco de coleta vira buraco no desenho, não
/// distorção. `None` = zero amostra na janela → o chamador mostra o estado
/// vazio honesto. Amostra com timestamp no futuro (relógio adiantado) é
/// descartada — dado inventado não.
fn svg_curva(
    serie: &[(i64, f64)],
    t0: i64,
    t1: i64,
    metrica: Metrica,
    w: u32,
    h: u32,
    aria: &str,
) -> Option<String> {
    let span = (t1 - t0).max(1);
    let pontos: Vec<(f64, f64)> = serie
        .iter()
        .filter(|(ts, _)| *ts >= t0 && *ts <= t1)
        .map(|(ts, v)| {
            let x = ((ts - t0) as f64 / span as f64) * (w as f64 - 1.0);
            (x, *v)
        })
        .collect();
    if pontos.is_empty() {
        return None;
    }
    let chao = h as f64 - 8.0;
    let topo = 14.0;
    let amplitude = chao - topo;
    let max = pontos.iter().map(|(_, v)| *v).fold(f64::MIN, f64::max);
    let escala = if max > 0.0 { max } else { 1.0 };
    let pts = pontos
        .iter()
        .map(|(x, v)| format!("{x:.1},{:.1} ", chao - (v / escala) * amplitude))
        .collect::<String>();
    // Um ponto só: polyline não desenha nada visível — marca com círculo.
    let ponto = if pontos.len() == 1 {
        let (x, v) = pontos[0];
        format!(
            "<circle cx=\"{x:.1}\" cy=\"{:.1}\" r=\"2.5\" fill=\"var(--accent)\"/>",
            chao - (v / escala) * amplitude
        )
    } else {
        String::new()
    };
    let n = pontos.len();
    // O rótulo da janela sai do PRÓPRIO intervalo desenhado (em minutos) —
    // a curva nunca diz uma janela diferente da que de fato desenhou.
    Some(format!(
        "<svg viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"{aria}\">\
         <line class=\"eixo\" x1=\"0\" y1=\"{chao:.0}\" x2=\"{w}\" y2=\"{chao:.0}\"/>\
         <polyline points=\"{pts}\"/>{ponto}\
         <text x=\"4\" y=\"10\">pico {} {} · {n} min</text>\
         <text x=\"4\" y=\"{}\">-{}</text>\
         <text x=\"{}\" y=\"{}\">agora</text>\
         </svg>",
        fmt_valor(max),
        unidade(metrica),
        h - 3,
        janela_label((t1 - t0) / 60),
        w.saturating_sub(34),
        h - 3,
    ))
}

// ------------------------------------------------------------------ layout

/// Esqueleto da página — mesma estrutura do painel (tokens, Sair, `meta
/// robots`, zero JS). Sem `meta refresh`: leitura sob demanda, como /events.
fn layout(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="pt-br">
<head>
<meta charset="utf-8">
<meta name="robots" content="noindex, nofollow">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>{css_admin}{css_tail}</style>
</head>
<body>
<div class="wrap">
<header class="top">
<h1>Flux · painel</h1>
<nav><a href="/">Dashboard</a><a href="/events">Eventos</a><a href="/observe">Monitor 24 h</a></nav>
<form method="post" action="/logout" style="margin-left:auto">
<button type="submit" style="background:var(--idlebg);color:var(--text);
border:1px solid var(--line);border-radius:6px;padding:.2rem .7rem;cursor:pointer">Sair</button>
</form>
</header>
{body}
<footer>fluxd — painel server-side, zero JS (spec 13) · horários em UTC</footer>
</div>
</body>
</html>"#,
        css_admin = crate::ui::CSS_ADMIN,
        css_tail = CSS_TAIL,
    )
}

/// Escape HTML para TODA interpolação de dado (mesmo conjunto do panel.rs).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Época → "YYYY-MM-DD HH:MM:SS" UTC (algoritmo civil de Howard Hinnant —
/// std Rust não tem tz database; a página exibe UTC e DIZ isso).
fn fmt_utc(secs: i64) -> String {
    let dias = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Snapshot cru (JSON) → texto formatado para o `<details>` da perícia.
/// JSON válido: pretty-print. JSON inválido: exibido CRU com a observação —
/// nunca descartado nem "consertado" por cima.
fn snapshot_formatado(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_owned()),
        Err(e) => format!("{raw}\n(snapshot não é JSON válido: {e})"),
    }
}

/// `metric` cru do peak (vocabulário das colunas do rollup: `cpu_percent`,
/// `mem_used_mb`) → rótulo legível. Valor desconhecido aparece CRU —
/// honesto, nunca "???". Usado na célula com o cru no `title`.
fn rotulo_metric_str(m: &str) -> String {
    match m {
        "cpu_percent" => "CPU".into(),
        "mem_used_mb" => "memória".into(),
        other => other.to_owned(),
    }
}

/// Unidade do valor/threshold do peak, pelo MESMO vocabulário do `metric`.
fn unidade_metric_str(m: &str) -> &'static str {
    match m {
        "cpu_percent" => "%",
        "mem_used_mb" => "MB",
        _ => "",
    }
}

/// Monta `?projeto=..&container=..&metrica=..&janela=..` para drill/link.
fn url_filtro(projeto: Option<&str>, container: Option<&str>, f: &Filtro) -> String {
    let mut partes: Vec<String> = Vec::new();
    if let Some(p) = projeto {
        partes.push(format!("projeto={}", esc(p)));
    }
    if let Some(c) = container {
        partes.push(format!("container={}", esc(c)));
    }
    partes.push(format!(
        "metrica={}",
        match f.metrica {
            Metrica::Cpu => "cpu",
            Metrica::Mem => "mem",
            Metrica::NetRx => "net_rx",
            Metrica::NetTx => "net_tx",
        }
    ));
    partes.push(format!("janela={}", f.minutos));
    format!("/observe?{}", partes.join("&"))
}

// ------------------------------------------------------------------ render

fn render_monitor(app: &AppState, f: &Filtro) -> String {
    let mut body = String::new();

    // Sem relógio do estado, não há janela possível: erro real na página.
    let now = match agora_epoch(app) {
        Ok(n) => n,
        Err(e) => {
            return layout(
                "Flux — monitor",
                &format!(
                    "<h1>Monitor 24 h</h1><div class=\"empty err-t\">Falha lendo o relógio do \
                     estado: {}</div><p><a href=\"/\">← dashboard</a></p>",
                    esc(&e)
                ),
            );
        }
    };
    let t0 = now - f.minutos * 60;

    body.push_str(&format!(
        "<h1>Monitor 24 h</h1><p class=\"muted\">{} · janela {} · \
         coleta do sampler do fluxd (spec 13, rollup por minuto)</p>",
        esc(&f.escopo()),
        esc(&janela_label(f.minutos)),
    ));

    body.push_str(&filtros_html(app, f));
    body.push_str(&serie_html(app, f, t0, now));
    body.push_str(&drill_html(app, f, t0, now));
    body.push_str(&picos_html(app, f));

    layout("Flux — monitor", &body)
}

/// Filtros do monitor (L4.5). Erro de leitura aparece na página — nunca
/// select fingindo vazio.
fn filtros_html(app: &AppState, f: &Filtro) -> String {
    let mut body = String::new();
    // ------------------------------------------------------------- filtros
    body.push_str(
        "<section><h2>Filtros</h2><form method=\"get\" action=\"/observe\" class=\"filtros\">",
    );
    // `<select>` de projeto: registrado no estado + "todos". Erro de leitura
    // aparece na página (o select sai vazio) — nunca select fingindo vazio.
    let mut select_projeto =
        String::from("<label>projeto<select name=\"projeto\"><option value=\"\">todos</option>");
    let mut erro_projetos = String::new();
    match projetos(app) {
        Ok(lista) => {
            for p in lista {
                let sel = if f.projeto.as_deref() == Some(p.as_str()) {
                    " selected"
                } else {
                    ""
                };
                select_projeto.push_str(&format!(
                    "<option value=\"{}\"{sel}>{}</option>",
                    esc(&p),
                    esc(&p)
                ));
            }
        }
        Err(e) => {
            erro_projetos = format!(
                "<p class=\"err-t\">Falha lendo projetos do estado: {}</p>",
                esc(&e)
            );
        }
    }
    select_projeto.push_str("</select></label>");
    body.push_str(&select_projeto);
    let container_val = f.container.as_deref().unwrap_or("");
    body.push_str(&format!(
        "<label>container<input type=\"text\" name=\"container\" value=\"{}\" \
         placeholder=\"ex.: web-1\"></label>",
        esc(container_val)
    ));
    body.push_str("<label>métrica<select name=\"metrica\">");
    for (valor, rot) in [
        ("cpu", "CPU %"),
        ("mem", "memória (MB)"),
        ("net_rx", "rede rx (kB)"),
        ("net_tx", "rede tx (kB)"),
    ] {
        let sel = match (metrica_de(valor), f.metrica) {
            (Some(Metrica::Cpu), Metrica::Cpu)
            | (Some(Metrica::Mem), Metrica::Mem)
            | (Some(Metrica::NetRx), Metrica::NetRx)
            | (Some(Metrica::NetTx), Metrica::NetTx) => " selected",
            _ => "",
        };
        body.push_str(&format!("<option value=\"{valor}\"{sel}>{rot}</option>"));
    }
    body.push_str("</select></label><label>janela<select name=\"janela\">");
    for (min, rot) in JANELAS {
        let sel = if min == f.minutos { " selected" } else { "" };
        body.push_str(&format!(
            "<option value=\"{min}\"{sel}>{}</option>",
            esc(rot)
        ));
    }
    body.push_str(
        "</select></label>\
         <button type=\"submit\">Aplicar</button>\
         <label>&nbsp;<a href=\"/observe\">limpar</a></label>\
         </form>",
    );
    if !erro_projetos.is_empty() {
        body.push_str(&erro_projetos);
    }
    body.push_str("</section>");
    body
}

/// Curva geral da janela (L4.5): estado vazio dito, nunca silencioso.
fn serie_html(app: &AppState, f: &Filtro, t0: i64, now: i64) -> String {
    let mut body = String::new();
    // -------------------------------------------------------- curva geral
    body.push_str(&format!(
        "<section><h2>Série — {} · {}</h2>",
        rotulo_metrica(f.metrica),
        esc(&f.escopo())
    ));
    let serie = observe::serie(
        &app.state,
        f.projeto.as_deref(),
        f.container.as_deref(),
        f.metrica,
        f.minutos,
    );
    match svg_curva(
        &serie,
        t0,
        now,
        f.metrica,
        CURVA_W,
        CURVA_H,
        &format!(
            "{} de {} nas últimas {}",
            rotulo_metrica(f.metrica),
            esc(&f.escopo()),
            janela_label(f.minutos)
        ),
    ) {
        Some(svg) => body.push_str(&format!("<div class=\"curve\">{svg}</div>")),
        None => body.push_str(
            "<div class=\"empty\">sem coleta ainda nesta janela — o sampler sobe com o fluxd \
             (spec 13) e os minutos vão aparecendo aqui</div>",
        ),
    }
    body.push_str("</section>");
    body
}

/// Drill por container (L4.5): só sem filtro de container e com projeto
/// escolhido — senão o estado é dito.
fn drill_html(app: &AppState, f: &Filtro, t0: i64, now: i64) -> String {
    let mut body = String::new();
    // ------------------------------------------------- drill por container
    // Só faz sentido SEM filtro de container (já está no drill) e COM projeto
    // escolhido — a consulta é por projeto. Sem um dos dois, o estado é dito.
    if f.container.is_none() {
        body.push_str("<section><h2>Por container</h2>");
        match &f.projeto {
            None => body.push_str(
                "<div class=\"empty\">escolha um projeto no filtro para ver a curva por \
                 container (drill)</div>",
            ),
            Some(projeto) => {
                let series =
                    observe::series_por_container(&app.state, projeto, f.metrica, f.minutos);
                if series.is_empty() {
                    body.push_str(&format!(
                        "<div class=\"empty\">sem coleta por container em \
                         <code>{}</code> nesta janela ainda</div>",
                        esc(projeto)
                    ));
                } else {
                    body.push_str("<div class=\"cards\">");
                    for (container, mini) in series {
                        body.push_str(&format!(
                            "<article class=\"card\"><h3><code>{}</code>\
                             <a href=\"{}\">focar →</a></h3>",
                            esc(&container),
                            url_filtro(Some(projeto), Some(&container), f)
                        ));
                        match svg_curva(
                            &mini,
                            t0,
                            now,
                            f.metrica,
                            MINI_W,
                            MINI_H,
                            &format!(
                                "{} do container {} nas últimas {}",
                                rotulo_metrica(f.metrica),
                                esc(&container),
                                janela_label(f.minutos)
                            ),
                        ) {
                            Some(svg) => {
                                body.push_str(&format!("<div class=\"curve\">{svg}</div>"))
                            }
                            None => body.push_str(
                                "<div class=\"empty\">sem amostra deste container na janela</div>",
                            ),
                        }
                        body.push_str("</article>");
                    }
                    body.push_str("</div>");
                }
            }
        }
        body.push_str("</section>");
    }
    body
}

/// Tabela de picos (L4.5): snapshot indisponível é dito com a causa.
fn picos_html(app: &AppState, f: &Filtro) -> String {
    let mut body = String::new();
    // ------------------------------------------------------------- picos
    body.push_str(&format!(
        "<section><h2>Picos — {} · {}</h2>",
        rotulo_metrica(f.metrica),
        esc(&janela_label(f.minutos))
    ));
    let picos = observe::peaks(&app.state, f.minutos, f.projeto.as_deref());
    if picos.is_empty() {
        body.push_str(
            "<div class=\"empty\">nenhum pico registrado nesta janela — picos aparecem quando \
             CPU passa de 80% ou memória de 85% do limite (spec 13)</div>",
        );
    } else {
        body.push_str(
            "<table><tr><th>quando (UTC)</th><th>projeto compose</th><th>container</th>\
             <th>imagem</th><th>métrica</th><th>valor vs threshold</th><th></th></tr>",
        );
        for p in picos {
            // `service` existe na linha mas não é coluna pedida — fica no
            // snapshot da perícia, onde faz parte do contexto.
            let PeakRow {
                id,
                ts,
                container,
                compose_project,
                image,
                metric,
                valor,
                threshold,
                ..
            } = p;
            // Valor no/above do threshold é a condição do alerta (spec 13):
            // acima pinta de err, encostando pinta de warn.
            let classe = if valor >= threshold {
                "err-t"
            } else {
                "warn-t"
            };
            let unid = unidade_metric_str(&metric);
            let celula_valor = format!(
                "<span class=\"{classe}\">{} {unid}</span> <span class=\"muted\">/ limiar {} {unid}</span>",
                fmt_valor(valor),
                fmt_valor(threshold)
            );
            let detalhe = match observe::peak_snapshot(&app.state, id) {
                Some(raw) => format!(
                    "<details><summary class=\"muted\">snapshot da perícia</summary>\
                     <pre>{}</pre></details>",
                    esc(&snapshot_formatado(&raw))
                ),
                // Sem snapshot gravado: dito — não é "—" sem explicação.
                None => "<span class=\"muted\">snapshot indisponível</span>".into(),
            };
            body.push_str(&format!(
                "<tr><td class=\"muted\">{}</td>\
                 <td class=\"muted\">{}</td><td><code>{}</code></td><td>{}</td>\
                 <td title=\"{}\">{}</td><td>{celula_valor}</td><td>{detalhe}</td></tr>",
                esc(&fmt_utc(ts)),
                esc(&compose_project),
                esc(&container),
                esc(&image),
                esc(&metric),
                esc(&rotulo_metric_str(&metric)),
            ));
        }
        body.push_str("</table>");
    }
    body.push_str("</section><p><a href=\"/\">← dashboard</a></p>");
    body
}

// -------------------------------------------------------------------- testes
//
// End-to-end no padrão do panel.rs: server efêmero no loopback + o MESMO
// cliente hyper do `AppState.http`. Diferença deliberada: a rota `/observe`
// entra no painel real via `panel::build_panel` (montagem do integrador, auth
// herdada do guard) — aqui ela roda num router mínimo com o MESMO handler,
// para o teste isolar o comportamento DESTA página (status, headers, corpo).
// Estado sempre `State::in_memory` (nunca toca disco).

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use flux_common::config::GlobalConfig;
    use http_body_util::BodyExt;

    fn app_sem_docker() -> Arc<AppState> {
        let st = crate::state::State::in_memory().unwrap();
        Arc::new(AppState::new(st, &GlobalConfig::default(), None))
    }

    fn router_observe(app: &Arc<AppState>) -> Router {
        Router::new()
            .route("/observe", get(pagina))
            .with_state(Arc::clone(app))
    }

    async fn pedir(
        app: &Arc<AppState>,
        router: Router,
        uri: &str,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let url: hyper::Uri = format!("http://{addr}{uri}").parse().unwrap();
        let req = axum::http::Request::builder()
            .method("GET")
            .uri(url)
            .header(header::HOST, "flux.bitello.xyz")
            .body(Body::empty())
            .unwrap();
        let resp = app.http.request(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = String::from_utf8(
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, headers, body)
    }

    async fn get_observe(
        app: &Arc<AppState>,
        query: &str,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let uri = if query.is_empty() {
            "/observe".to_owned()
        } else {
            format!("/observe?{query}")
        };
        pedir(app, router_observe(app), &uri).await
    }

    fn noindex_de(headers: &axum::http::HeaderMap) -> String {
        headers
            .get("x-robots-tag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    /// Época "agora" pelo MESMO relógio que a página usa (relógio do SQLite).
    /// Amostras de teste são semeadas relativas a ESTE relógio: os buckets
    /// caem sempre DENTRO (ou fora, quando o teste quer) da janela da página.
    fn agora(app: &AppState) -> i64 {
        agora_epoch(app).unwrap()
    }

    /// Schema de métricas/perícia do observe — o schema base do state.rs não
    /// cria `metrics_1min`/`peaks`; sem isso a consulta devolveria vazio por
    /// erro (logado) e o teste estaria verde por motivo errado.
    fn com_schema(app: &AppState) {
        observe::garantir_schema(&app.state).unwrap();
    }

    /// Semeia um bucket de rollup pela API PÚBLICA do observe (`upsert`) —
    /// zero acoplamento com o DDL: se a tabela mudar, o teste quebra na
    /// mesma linha que a página.
    #[allow(clippy::too_many_arguments)]
    fn semear(
        app: &AppState,
        ts: i64,
        compose_project: &str,
        container: &str,
        image: &str,
        cpu: f64,
        mem: f64,
        limite: f64,
        rx: f64,
        tx: f64,
    ) {
        observe::upsert(
            &app.state,
            &observe::Amostra {
                ts,
                compose_project: compose_project.to_owned(),
                service: "app".to_owned(),
                container: container.to_owned(),
                image: image.to_owned(),
                cpu_percent: cpu,
                mem_used_mb: mem,
                mem_limit_mb: limite,
                net_rx_kb: rx,
                net_tx_kb: tx,
                status: "running".to_owned(),
            },
        )
        .unwrap();
    }

    /// Semeia um pico pela API pública (`registrar_peak`), com snapshot real
    /// (`snapshot_json`). Devolve o id criado.
    // Helper de teste: os 8 parâmetros espelham os campos do peak real —
    // agrupar em struct só para calar o clippy esconderia o formato.
    #[allow(clippy::too_many_arguments)]
    fn semear_pico(
        app: &AppState,
        ts: i64,
        compose_project: &str,
        container: &str,
        image: &str,
        metric: &str,
        valor: f64,
        threshold: f64,
    ) -> i64 {
        let amostra = observe::Amostra {
            ts,
            compose_project: compose_project.to_owned(),
            service: "app".to_owned(),
            container: container.to_owned(),
            image: image.to_owned(),
            cpu_percent: 0.0,
            mem_used_mb: 0.0,
            mem_limit_mb: 0.0,
            net_rx_kb: 0.0,
            net_tx_kb: 0.0,
            status: "running".to_owned(),
        };
        let snapshot = observe::snapshot_json(&amostra, std::slice::from_ref(&amostra));
        observe::registrar_peak(&app.state, &amostra, metric, valor, threshold, &snapshot).unwrap()
    }

    // ------------------------------------------------------- unitários puros

    #[test]
    fn filtro_da_tem_defaults_e_clampa_lixo() {
        let base = Filtro::default_();
        assert!(mesmo_filtro(&filtro_da(None), &base));
        assert!(mesmo_filtro(&filtro_da(Some("")), &base));
        assert!(
            mesmo_filtro(&filtro_da(Some("janela=999&metrica=banana")), &base),
            "inválido → default"
        );
        assert_eq!(filtro_da(Some("janela=360")).minutos, 360);
        let cheio = filtro_da(Some(
            "projeto=vipers&container=web-1&metrica=net_rx&janela=60&extranho=1",
        ));
        assert_eq!(cheio.projeto.as_deref(), Some("vipers"));
        assert_eq!(cheio.container.as_deref(), Some("web-1"));
        assert!(
            matches!(cheio.metrica, Metrica::NetRx),
            "net_rx parseia para NetRx"
        );
        assert_eq!(cheio.minutos, 60);
        // Branco/nulo no select vira "todos" (None), não "" quebrando o SQL.
        assert!(mesmo_filtro(&filtro_da(Some("projeto=&container=")), &base));
    }

    #[test]
    fn url_decode_resolve_percent_e_mais() {
        assert_eq!(url_decode("web%201"), "web 1");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("vipers"), "vipers");
        assert_eq!(url_decode("100%25"), "100%");
        // % solto não pânico: volta literal.
        assert_eq!(url_decode("100%"), "100%");
    }

    #[test]
    fn metrica_de_e_rotulos_cobrem_os_quatro() {
        for (texto, esperado) in [
            ("cpu", Metrica::Cpu),
            ("mem", Metrica::Mem),
            ("net_rx", Metrica::NetRx),
            ("net_tx", Metrica::NetTx),
        ] {
            assert!(
                matches!(
                    metrica_de(texto),
                    Some(ref m) if matches!(
                        (m, &esperado),
                        (Metrica::Cpu, Metrica::Cpu)
                            | (Metrica::Mem, Metrica::Mem)
                            | (Metrica::NetRx, Metrica::NetRx)
                            | (Metrica::NetTx, Metrica::NetTx)
                    )
                ),
                "{texto} parseia para a variante certa"
            );
            assert!(!rotulo_metrica(esperado).is_empty());
            assert!(!unidade(esperado).is_empty());
        }
        assert_eq!(metrica_de("disc"), None);
    }

    #[test]
    fn fmt_valor_e_janela_label() {
        assert_eq!(fmt_valor(92.5), "92.5");
        assert_eq!(fmt_valor(1234.56), "1235");
        assert_eq!(janela_label(60), "1 h");
        assert_eq!(janela_label(360), "6 h");
        assert_eq!(janela_label(1440), "24 h");
        assert_eq!(janela_label(42), "42 min");
    }

    #[test]
    fn snapshot_formatado_pretty_e_lixo_cru_com_nota() {
        let bonito = snapshot_formatado(r#"{"b":1,"a":2}"#);
        assert!(bonito.contains("\"a\": 2"), "ordenado/pretty: {bonito}");
        let cru = snapshot_formatado("isto não é json");
        assert!(cru.contains("não é JSON válido"), "nota honesta: {cru}");
        assert!(
            cru.contains("isto não é json"),
            "conteúdo cru preservado: {cru}"
        );
    }

    #[test]
    fn esc_nao_deixa_html_passar() {
        assert_eq!(esc(r#"<img src=x>"#), "&lt;img src=x&gt;");
    }

    #[test]
    fn fmt_utc_bate_com_epocas_conhecidas() {
        assert_eq!(fmt_utc(0), "1970-01-01 00:00:00");
        assert_eq!(fmt_utc(1_000_000_000), "2001-09-09 01:46:40");
    }

    // ---------------------------------------------------------- integração

    #[tokio::test]
    async fn observe_vazio_e_honesto_com_form_e_headers() {
        let app = app_sem_docker();
        com_schema(&app); // schema existe, tabela vazia: vazio é falta de coleta
        let (status, headers, body) = get_observe(&app, "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("sem coleta ainda nesta janela"),
            "estado vazio é dito, não inventado: {body}"
        );
        // Form GET (funciona com JS desligado) com os 4 filtros + Aplicar + limpar.
        assert!(
            body.contains("method=\"get\" action=\"/observe\""),
            "form GET para a própria rota: {body}"
        );
        for nome in ["projeto", "container", "metrica", "janela"] {
            assert!(
                body.contains(&format!("name=\"{nome}\"")),
                "campo {nome}: {body}"
            );
        }
        assert!(body.contains("Aplicar"), "botão aplicar: {body}");
        assert!(
            body.contains("<a href=\"/observe\">limpar</a>"),
            "link limpar: {body}"
        );
        assert_eq!(noindex_de(&headers), NOINDEX);
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default(),
            "no-store"
        );
    }

    #[tokio::test]
    async fn observe_com_dados_desenha_curva_e_lista_pico_com_snapshot() {
        let app = app_sem_docker();
        com_schema(&app);
        let now = agora(&app);
        app.state
            .register_project("vipers", "name = \"vipers\"", "h")
            .unwrap();
        semear(
            &app,
            now - 120,
            "flux-vipers-dev",
            "vipers-web-1",
            "nginx:1.27",
            12.5,
            180.0,
            512.0,
            40.0,
            20.0,
        );
        semear(
            &app,
            now - 60,
            "flux-vipers-dev",
            "vipers-web-1",
            "nginx:1.27",
            30.0,
            190.0,
            512.0,
            60.0,
            25.0,
        );
        semear_pico(
            &app,
            now - 60,
            "flux-vipers-dev",
            "vipers-web-1",
            "nginx:1.27",
            "cpu_percent",
            92.5,
            80.0,
        );

        let (_, _, body) = get_observe(&app, "").await;
        assert!(
            body.contains("<svg") && body.contains("<polyline"),
            "série com dados desenha a curva: {body}"
        );
        assert!(
            body.contains("pico 30.0 %"),
            "rótulo do máximo da série: {body}"
        );
        assert!(
            body.contains("vipers-web-1"),
            "container na tabela de picos: {body}"
        );
        assert!(body.contains("nginx:1.27"), "imagem do pico: {body}");
        assert!(body.contains("92.5"), "valor do pico: {body}");
        assert!(body.contains("80.0"), "threshold do pico: {body}");
        assert!(
            body.contains("<details>") && body.contains("<pre>"),
            "snapshot expansível: {body}"
        );
        // Perícia formatada (pretty) e escapada: aspas viram &quot; no <pre>.
        assert!(
            body.contains("&quot;image&quot;: &quot;nginx:1.27&quot;"),
            "snapshot traz a imagem em JSON formatado: {body}"
        );
        assert!(
            body.contains("&quot;container&quot;: &quot;vipers-web-1&quot;"),
            "snapshot traz os irmãos do momento: {body}"
        );
    }

    #[tokio::test]
    async fn observe_filtros_mudam_a_serie_e_a_janela() {
        let app = app_sem_docker();
        com_schema(&app);
        let now = agora(&app);
        app.state.register_project("a", "x", "h").unwrap();
        app.state.register_project("b", "x", "h").unwrap();
        // Compose projects no formato real (prefixo `flux-`, 06): o LIKE do
        // observe é `flux-{projeto}-%`.
        semear(
            &app,
            now - 60,
            "flux-a-dev",
            "a-web-1",
            "img-a",
            90.0,
            10.0,
            512.0,
            1.0,
            1.0,
        );
        semear(
            &app,
            now - 60,
            "flux-b-dev",
            "b-web-1",
            "img-b",
            10.0,
            10.0,
            512.0,
            1.0,
            1.0,
        );
        // Pico FORA da janela de 1 h (2 h atrás) e um DENTRO.
        semear_pico(
            &app,
            now - 7200,
            "flux-a-dev",
            "a-web-1",
            "img-a",
            "cpu_percent",
            95.0,
            80.0,
        );
        semear_pico(
            &app,
            now - 60,
            "flux-a-dev",
            "a-web-1",
            "img-a",
            "mem_used_mb",
            500.0,
            400.0,
        );

        // Filtro por projeto: cada página mostra o pico DA sua série.
        let (_, _, pa) = get_observe(&app, "projeto=a").await;
        assert!(pa.contains("pico 90.0 %"), "curva do projeto a: {pa}");
        let (_, _, pb) = get_observe(&app, "projeto=b").await;
        assert!(pb.contains("pico 10.0 %"), "curva do projeto b: {pb}");
        assert!(!pb.contains("pico 90.0 %"), "filtro isola a série: {pb}");

        // Janela 1 h esconde o pico de 2 h atrás; 24 h mostra os dois.
        let (_, _, j1) = get_observe(&app, "projeto=a&janela=60").await;
        assert!(
            !j1.contains("95.0") && j1.contains("500.0"),
            "janela curta exclui pico antigo: {j1}"
        );
        let (_, _, j24) = get_observe(&app, "projeto=a&janela=1440").await;
        assert!(
            j24.contains("95.0"),
            "janela 24 h inclui pico antigo: {j24}"
        );

        // Drill: seção por container do projeto com link que seta ?container=.
        let (_, _, drill) = get_observe(&app, "projeto=a").await;
        assert!(
            drill.contains("focar →") && drill.contains("container=a-web-1"),
            "drill por container linka o foco: {drill}"
        );
        // Com container focado, a seção "Por container" some (já está no foco).
        let (_, _, foco) = get_observe(&app, "projeto=a&container=a-web-1").await;
        assert!(
            foco.contains("pico 90.0 %") && !foco.contains("focar →"),
            "focado desenha a série do container e não repete o drill: {foco}"
        );
        // Container SEM dados → vazio honesto, não curva mentirosa.
        let (_, _, fantasma) = get_observe(&app, "projeto=a&container=nada").await;
        assert!(
            fantasma.contains("sem coleta ainda nesta janela"),
            "container sem amostra é honesto: {fantasma}"
        );
    }

    #[tokio::test]
    async fn observe_sem_projeto_pe_drill_e_dito() {
        let app = app_sem_docker();
        let (_, _, body) = get_observe(&app, "").await;
        assert!(
            body.contains("escolha um projeto no filtro"),
            "drill sem projeto é explicado: {body}"
        );
    }

    #[tokio::test]
    async fn observe_pico_sem_snapshot_e_dito() {
        let app = app_sem_docker();
        com_schema(&app);
        let now = agora(&app);
        semear_pico(
            &app,
            now - 60,
            "flux-a-dev",
            "a-web-1",
            "img-a",
            "cpu_percent",
            90.0,
            80.0,
        );
        // Remove o snapshot para simular pico sem perícia gravada.
        app.state
            .with(|c| c.execute("UPDATE peaks SET snapshot = NULL", []))
            .unwrap();
        let (_, _, body) = get_observe(&app, "").await;
        assert!(
            body.contains("snapshot indisponível"),
            "falta de snapshot é declarada: {body}"
        );
    }

    #[tokio::test]
    async fn noindex_e_no_store_em_toda_rota_observe() {
        let app = app_sem_docker();
        com_schema(&app);
        for q in ["", "projeto=x", "metrica=bob&janela=abc", "container=%20"] {
            let (_, headers, _) = get_observe(&app, q).await;
            assert_eq!(noindex_de(&headers), NOINDEX, "query {q:?} sem noindex");
            assert_eq!(
                headers
                    .get(header::CACHE_CONTROL)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default(),
                "no-store",
                "query {q:?} sem no-store"
            );
        }
    }
}

/// L4.4: o tail exclusivo + a base compartilhada cobrem o contrato visual
/// do /observe (a base vem de ui.rs e é testada lá; aqui o tail).
#[test]
fn css_tail_tem_os_controles_do_monitor() {
    assert!(CSS_TAIL.contains("form.filtros"), "filtros do monitor");
    assert!(CSS_TAIL.contains("details pre"), "snapshot");
    assert!(
        crate::ui::CSS_ADMIN.contains("--accent:#2563eb"),
        "token base"
    );
}
