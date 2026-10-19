//! Formato do painel (L6.3): escape, datas, badges e rótulos — funções
//! puras de transformação, testadas uma a uma.

use flux_common::protocol::ContainerState;

// ---------------------------------------------------------------- formato

/// Escape HTML para TODA interpolação de dado (nome de projeto, mensagem de
/// evento, host...). L4.3: UMA definição no crate — delega ao dono
/// (`pages::html_escape`); a cópia local podia divergir silenciosamente numa
/// correção futura. Call sites e testes intocados.
pub(super) fn esc(s: &str) -> String {
    crate::pages::html_escape(s)
}

/// Escape do valor usado em `LIKE '%nome%'` (detalhe de projeto).
pub(super) fn like_esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// `?page=N` da query string — parse manual (extrator com serde não é
/// dependência desta fase). Página inválida/ausente/0 → 1 (clamp honesto).
pub(super) fn page_de(query: Option<&str>) -> i64 {
    query
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("page=")))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|p| *p >= 1)
        .unwrap_or(1)
}

/// Época → "YYYY-MM-DD HH:MM:SS" UTC (algoritmo civil de Howard Hinnant —
/// std Rust não tem tz database; painel exibe tudo em UTC e DIZ isso).
pub(super) fn fmt_utc(secs: i64) -> String {
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

/// Dias desde 1970-01-01 a partir do civil — a INVERSA exata do algoritmo
/// civil de Hinnant usado em `fmt_utc` (mesma época, mesmo deslocamento).
fn dias_de_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "YYYY-MM-DD HH:MM:SS" (formato do SQLite `datetime('now')`, fonte dos
/// timestamps do estado) e "YYYY-MM-DDTHH:MM:SSZ" (RFC3339 do `updated_at`
/// do sync) → epoch UTC. O que não parseia (ou está fora do relógio civil)
/// vira `None` — o chamador mostra o texto cru em vez de inventar hora.
pub(super) fn epoch_de(s: &str) -> Option<i64> {
    let (dia, hora) = s.trim().split_once([' ', 'T'])?;
    let mut p = dia.split('-');
    let y: i64 = p.next()?.parse().ok()?;
    let m: i64 = p.next()?.parse().ok()?;
    let d: i64 = p.next()?.parse().ok()?;
    let mut h = hora.trim_end_matches('Z').split(':');
    let hh: i64 = h.next()?.parse().ok()?;
    let mi: i64 = h.next()?.parse().ok()?;
    let ss: i64 = h.next().unwrap_or("0").parse().ok()?;
    if !((1..=12).contains(&m)
        && (1..=31).contains(&d)
        && (0..=23).contains(&hh)
        && (0..=59).contains(&mi)
        && (0..=60).contains(&ss))
    {
        return None;
    }
    Some(dias_de_civil(y, m, d) * 86_400 + hh * 3_600 + mi * 60 + ss)
}

/// Epoch opcional → "há 5 min" / "em 12 min" com o absoluto UTC no `title`.
/// `None` → "—" (nunca hora inventada). Passado conta com "há", futuro com
/// "em"; abaixo de 1 min, segundos.
pub(super) fn quando(now: i64, v: Option<i64>) -> String {
    let Some(v) = v else {
        return "—".into();
    };
    let diff = v - now;
    let rotulo = diff.abs();
    let rel = if rotulo < 60 {
        format!("{}{rotulo} s", if diff < 0 { "há " } else { "em " })
    } else if rotulo < 3600 {
        format!(
            "{}{} min",
            if diff < 0 { "há " } else { "em " },
            rotulo / 60
        )
    } else if rotulo < 86_400 {
        format!(
            "{}{} h",
            if diff < 0 { "há " } else { "em " },
            rotulo / 3600
        )
    } else {
        format!(
            "{}{} d",
            if diff < 0 { "há " } else { "em " },
            rotulo / 86_400
        )
    };
    format!("<span title=\"{} UTC\">{rel}</span>", fmt_utc(v))
}

/// Estado → (classe do dot, rótulo pt-BR). Cores espelham a matriz da 03.
pub(super) fn dot(estado: ContainerState) -> (&'static str, &'static str) {
    match estado {
        ContainerState::Awake => ("ok", "acordado"),
        ContainerState::Paused => ("info", "pausado"),
        ContainerState::Sleeping => ("warn", "dormindo"),
        ContainerState::Starting => ("accent", "subindo"),
        ContainerState::Failed => ("err", "falhou"),
        ContainerState::Unknown => ("idle", "desconhecido"),
    }
}

pub(super) fn badge_level(level: &str) -> String {
    let classe = match level {
        "info" => "muted",
        "warn" => "warn-t",
        "error" | "err" => "err-t",
        _ => "muted",
    };
    format!("<span class=\"{classe}\">{}</span>", esc(level))
}

/// Estado da sessão de sync → (classe do badge, rótulo). Regra sobre o texto
/// digerido do mutagen: contém "watching" = saudável (verde); `paused` =
/// pausada (azul); resto é classe de trabalho (amarelo) — e último erro ou
/// pendências sobem para atenção (laranja), que sobrepõe as outras. "absent"
/// é palavra do contrato (CLI api.rs: relato do watch para projeto conhecido
/// do fluxd SEM sessão local viva) e fica no cinza de "sem sessão" — não é
/// trabalho. Sem linha no estado = "sem sessão" (cinza): o painel não inventa
/// sessão.
pub(super) fn badge_sync(row: Option<&crate::sync::SyncRow>) -> (&'static str, &'static str) {
    let Some(r) = row else {
        return ("sb-idle", "sem sessão");
    };
    let base = if r.status.contains("watching") {
        ("sb-ok", "saudável")
    } else if r.status.contains("absent") {
        ("sb-idle", "sem sessão")
    } else if r.paused {
        ("sb-info", "pausada")
    } else {
        ("sb-warn", "trabalhando")
    };
    if r.problems > 0 || r.last_error.is_some() {
        ("sb-attn", "atenção")
    } else {
        base
    }
}
