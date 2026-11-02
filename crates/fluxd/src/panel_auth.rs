//! Auth do painel web (08/09): usuário único com senha em hash Argon2id em
//! `/srv/flux/secrets/panel.argon2`, sessão por cookie `httpOnly` +
//! `SameSite=Lax` expirando em 12 h, rate limit de login (5/min por IP) e
//! fail-closed — sem hash em disco não existe login (mesma postura da API de
//! controle sem token). A regra da 03 vale: **nunca** painel sem auth atrás do
//! wildcard público — este módulo é o que autoriza a montagem no router.
//!
//! Sessões vivem em memória: reinício do `fluxd` desloga todo mundo (aceito —
//! o painel é de uma pessoa e reinício é raro). Falha de leitura do hash é
//! erro VISÍVEL na tela de login (regra "nada silencioso"), nunca 500 cru.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHash, PasswordVerifier};

/// Cookie de sessão do painel (08): httpOnly + SameSite=Lax + Path=/.
pub const COOKIE_NAME: &str = "flux_panel_session";
/// Sessão expira em 12 h (08).
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// "Manter conectado" (escopo 31/10, fatia 8): 30 dias.
pub const SESSION_TTL_LONGA: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Janela e teto do rate limit de login (08): 5 tentativas por minuto por IP.
pub const LOGIN_WINDOW: Duration = Duration::from_secs(60);
pub const LOGIN_MAX_TENTATIVAS: u32 = 5;
/// Hash da senha do painel (09) — caminho padrão na VM; o `serve` pode
/// apontar outro via `PanelAuth::with_hash_path` (testes usam tempdir).
pub const HASH_PATH_PADRAO: &str = "/srv/flux/secrets/panel.argon2";

// ------------------------------------------------------------------ sessão

/// Sessões ativas: token (256 bits hex) → instante de expiração.
pub struct SessionStore {
    inner: Mutex<HashMap<String, Instant>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Cria sessão nova (12 h), podando as vencidas de quebra.
    pub fn create(&self) -> String {
        self.create_com_ttl(SESSION_TTL)
    }

    /// Sessão "manter conectado" (30 dias).
    pub fn create_longa(&self) -> String {
        self.create_com_ttl(SESSION_TTL_LONGA)
    }

    fn create_com_ttl(&self, ttl: Duration) -> String {
        let token = token_novo();
        let expira = Instant::now() + ttl;
        let mut inner = self.inner.lock().expect("mutex de sessões");
        inner.retain(|_, exp| *exp > Instant::now());
        inner.insert(token.clone(), expira);
        token
    }

    /// Sessão vale? Token desconhecido ou vencido = não.
    pub fn validate(&self, token: &str) -> bool {
        let inner = self.inner.lock().expect("mutex de sessões");
        inner.get(token).is_some_and(|exp| *exp > Instant::now())
    }

    pub fn revoke(&self, token: &str) {
        self.inner.lock().expect("mutex de sessões").remove(token);
    }

    /// "Sair de TODOS os dispositivos" (fatia 8): zera tudo.
    pub fn revogar_todas(&self) {
        self.inner.lock().expect("mutex de sessões").clear();
    }

    #[cfg(test)]
    fn insert_com_expiracao(&self, token: &str, expira: Instant) {
        self.inner
            .lock()
            .expect("mutex de sessões")
            .insert(token.to_owned(), expira);
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Token de sessão: 32 bytes do CSPRNG do SO em hex (64 chars).
fn token_novo() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("CSPRNG do SO indisponível — sem isso não há sessão segura");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------- rate limit

/// Contador de tentativas de login por IP, janela deslizante de 60 s.
pub struct LoginLimiter {
    tentativas: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self {
            tentativas: Mutex::new(HashMap::new()),
        }
    }

    /// Registra a tentativa e responde se ainda cabe no teto (5/min).
    /// Conta tentativa FRACASSADA ou não — o 6º POST do mesmo IP na janela
    /// é recusado antes de tocar no hash.
    pub fn permitir(&self, ip: IpAddr) -> bool {
        let mut t = self.tentativas.lock().expect("mutex de login");
        let agora = Instant::now();
        t.retain(|_, (inicio, _)| agora.duration_since(*inicio) < LOGIN_WINDOW);
        let e = t.entry(ip).or_insert((agora, 0));
        e.1 += 1;
        e.1 <= LOGIN_MAX_TENTATIVAS
    }
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ lockout

/// Erros que, dentro da janela, armar o lock global.
pub const LOCKOUT_ERROS: usize = 3;
/// Janela deslizante de erros: 3 em ≤ 90 s (espec do Gabriel).
pub const LOCKOUT_JANELA: Duration = Duration::from_secs(90);
/// Quem erra espera: 5 min GLOBAL (menos origem LAN, que é imune).
pub const LOCKOUT_DURACAO: Duration = Duration::from_secs(5 * 60);

/// Lockout GLOBAL do login (fatia 8): 3 erros em ≤ 90 s — de QUALQUER IP
/// fora da LAN, pois erros vindos da LAN nem contam — fecha o login pra
/// TODO MUNDO exceto LAN por 5 min. Estado em memória de propósito: um
/// reinício do fluxd limpa o lock (o rate limit por IP segue em pé) — o
/// atacante tem que reiniciar o daemon dele... que não é o nosso.
pub struct Lockout {
    falhas: Mutex<Vec<Instant>>,
    ate: Mutex<Option<Instant>>,
}

impl Lockout {
    pub fn new() -> Self {
        Self {
            falhas: Mutex::new(Vec::new()),
            ate: Mutex::new(None),
        }
    }

    /// Login fechado? LAN nunca.
    pub fn bloqueado(&self, lan: bool) -> bool {
        if lan {
            return false;
        }
        let ate = self.ate.lock().expect("mutex do lockout");
        matches!(*ate, Some(t) if t > Instant::now())
    }

    /// Falha de senha vinda de fora da LAN. LAN não conta (imune, decisão
    /// do Gabriel — em casa você nunca fica preso).
    pub fn registrar_falha(&self, lan: bool) {
        if lan {
            return;
        }
        self.registrar_falha_em(Instant::now());
    }

    fn registrar_falha_em(&self, agora: Instant) {
        let mut f = self.falhas.lock().expect("mutex do lockout");
        f.retain(|t| agora.duration_since(*t) <= LOCKOUT_JANELA);
        f.push(agora);
        if f.len() >= LOCKOUT_ERROS {
            *f = Vec::new(); // o lock fala por si; contador zera
            *self.ate.lock().expect("mutex do lockout") = Some(agora + LOCKOUT_DURACAO);
        }
    }

    /// Sucesso autêntico limpa as falhas em aberto (2 erros antigos não
    /// contaminam o próximo ciclo).
    pub fn limpar(&self) {
        self.falhas.lock().expect("mutex do lockout").clear();
    }
}

impl Default for Lockout {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ senha

/// Estado de auth compartilhado entre login, logout e o middleware de guarda.
pub struct PanelAuth {
    pub sessions: SessionStore,
    pub limiter: LoginLimiter,
    pub lockout: Lockout,
    hash_path: PathBuf,
}

impl PanelAuth {
    pub fn novo() -> Self {
        Self::with_hash_path(PathBuf::from(HASH_PATH_PADRAO))
    }

    pub fn with_hash_path(path: PathBuf) -> Self {
        Self {
            sessions: SessionStore::new(),
            limiter: LoginLimiter::new(),
            lockout: Lockout::new(),
            hash_path: path,
        }
    }

    /// Lê o hash de `/srv/flux/secrets/panel.argon2` A CADA login (arquivo de
    /// poucos bytes; rotação de senha não pede reinício do fluxd) e verifica a
    /// senha com Argon2id. `Err` = problema de PLATAFORMA (hash ausente/ilegível
    /// ou formato inválido) — mensagem volta para a tela, fail-closed.
    pub fn verificar(&self, senha: &str) -> Result<bool, String> {
        let hash = std::fs::read_to_string(&self.hash_path).map_err(|e| {
            format!(
                "hash do painel ausente ou ilegível em {}: {e}. Gere com \
                 `printf '%s' 'SUA-SENHA' | argon2 \"$(openssl rand -hex 16)\" -id -e \
                 > {}` (dentro da VM).",
                self.hash_path.display(),
                self.hash_path.display()
            )
        })?;
        let hash = hash.trim();
        let parsed = PasswordHash::new(hash).map_err(|e| {
            format!(
                "hash do painel em {} é inválido: {e}",
                self.hash_path.display()
            )
        })?;
        Ok(Argon2::default()
            .verify_password(senha.as_bytes(), &parsed)
            .is_ok())
    }
}

impl Default for PanelAuth {
    fn default() -> Self {
        Self::novo()
    }
}

// ----------------------------------------------------------------- cookie

/// Valor do header `Set-Cookie` para a sessão recém-criada (08: httpOnly,
/// SameSite=Lax, Path=/, Max-Age do TTL). Sem `Secure`: o painel também
/// responde por HTTP na rede interna (192.168.140.x) — o tráfego público
/// sempre passa pelo TLS do Caddy/tunnel (03).
pub fn set_cookie_sessao(token: &str) -> String {
    set_cookie_sessao_com(token, SESSION_TTL)
}

/// Cookie com o TTL escolhido no login ("manter conectado" = 30 dias).
pub fn set_cookie_sessao_com(token: &str, ttl: Duration) -> String {
    format!(
        "{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}",
        ttl.as_secs()
    )
}

/// `Set-Cookie` de expiração (logout).
pub fn set_cookie_limpo() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

/// Extrai o token de sessão do header `Cookie` da request (se houver).
pub fn token_do_header(headers: &axum::http::HeaderMap) -> Option<String> {
    let bruto = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for par in bruto.split(';') {
        let par = par.trim();
        if let Some(valor) = par.strip_prefix(COOKIE_NAME)
            && let Some(valor) = valor.strip_prefix('=')
        {
            let valor = valor.trim();
            if !valor.is_empty() {
                return Some(valor.to_owned());
            }
        }
    }
    None
}

// ------------------------------------------------------------------ testes

#[cfg(test)]
mod testes {
    use super::*;
    use argon2::PasswordHasher;
    use argon2::password_hash::SaltString;

    fn hash_de(senha: &str) -> String {
        let salt = SaltString::generate(&mut rand_core::OsRng);
        Argon2::default()
            .hash_password(senha.as_bytes(), &salt)
            .expect("hash de teste")
            .to_string()
    }

    #[test]
    fn sessao_criada_valida_e_revogavel() {
        let st = SessionStore::new();
        let token = st.create();
        assert_eq!(token.len(), 64, "token = 32 bytes em hex");
        assert!(st.validate(&token));
        assert!(!st.validate("outro-token"));
        st.revoke(&token);
        assert!(!st.validate(&token), "revogada não valida mais");
    }

    #[test]
    fn sessao_vencida_nao_valida() {
        let st = SessionStore::new();
        st.insert_com_expiracao("velho", Instant::now() - Duration::from_secs(1));
        assert!(!st.validate("velho"), "expiração passou = inválida");
    }

    #[test]
    fn rate_limit_solta_cinco_e_segura_o_sexta() {
        let lim = LoginLimiter::new();
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        for i in 0..5 {
            assert!(lim.permitir(ip), "tentativa {i} cabe no teto de 5/min");
        }
        assert!(!lim.permitir(ip), "6ª na mesma janela é recusada");
        // IP diferente não herda a conta.
        let outro: IpAddr = "10.0.0.10".parse().unwrap();
        assert!(lim.permitir(outro));
    }

    #[test]
    fn argon2_confere_senha_certa_e_recusa_errada() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let caminho = tmp.path().join("panel.argon2");
        // \n no FIM do arquivo (como o `tee`/redirect deixa) é cortado no trim.
        std::fs::write(&caminho, format!("{}\n", hash_de("senha-certa"))).expect("escreve hash");
        let auth = PanelAuth::with_hash_path(caminho);
        assert_eq!(auth.verificar("senha-certa"), Ok(true));
        assert_eq!(auth.verificar("senha-errada"), Ok(false));
    }

    #[test]
    fn hash_ausente_e_erro_visivel_nunca_login() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth = PanelAuth::with_hash_path(tmp.path().join("nao-existe"));
        let r = auth.verificar("qualquer");
        assert!(r.is_err(), "sem hash = Err (fail-closed)");
        let msg = r.unwrap_err();
        assert!(msg.contains("argon2"), "mensagem ensina a gerar: {msg}");
    }

    #[test]
    fn cookie_parse_pela_assinatura_exata() {
        use axum::http::HeaderMap;
        let mut h = HeaderMap::new();
        assert_eq!(token_do_header(&h), None);
        h.insert(
            axum::http::header::COOKIE,
            "outro=1; flux_panel_session=abc123".parse().unwrap(),
        );
        assert_eq!(token_do_header(&h).as_deref(), Some("abc123"));
        h.insert(
            axum::http::header::COOKIE,
            "flux_panel_session=".parse().unwrap(),
        );
        assert_eq!(token_do_header(&h), None, "valor vazio não é sessão");
    }

    #[test]
    fn set_cookie_tem_os_atributos_da_spec() {
        let c = set_cookie_sessao("tok");
        assert!(c.starts_with("flux_panel_session=tok;"));
        assert!(c.contains("HttpOnly"));
        assert!(c.contains("SameSite=Lax"));
        assert!(c.contains("Path=/"));
        assert!(c.contains("Max-Age=43200"), "12 h: {c}");
        assert!(set_cookie_limpo().contains("Max-Age=0"));
    }
}

#[cfg(test)]
mod lockout_tests {
    use super::*;

    /// Regra do Gabriel: 3 erros em ≤ 90 s = lock global de 5 min; 2 erros
    /// não travam; sucesso limpa o contador.
    #[test]
    fn tres_erros_na_janela_travam_e_sucesso_limpa() {
        let l = Lockout::new();
        let t = Instant::now();
        l.registrar_falha_em(t);
        l.registrar_falha_em(t + Duration::from_secs(30));
        assert!(!l.bloqueado(false), "2 erros não travam");
        l.registrar_falha_em(t + Duration::from_secs(60));
        assert!(l.bloqueado(false), "3 erros em 60 s travam");
        assert!(l.bloqueado(false), "lock dura 5 min");
        l.limpar();
        assert!(l.bloqueado(false), "limpar não levanta o lock já armado");
    }

    /// Erros FORA da janela de 90 s não se acumulam.
    #[test]
    fn erros_fora_da_janela_nao_somam() {
        let l = Lockout::new();
        let t = Instant::now();
        l.registrar_falha_em(t - Duration::from_secs(200));
        l.registrar_falha_em(t - Duration::from_secs(100));
        l.registrar_falha_em(t);
        assert!(!l.bloqueado(false), "só 1 erro dentro da janela de 90 s");
    }

    /// LAN é imune dos dois lados: erros não contam e lock não pega.
    #[test]
    fn lan_imune() {
        let l = Lockout::new();
        l.registrar_falha(true);
        l.registrar_falha(true);
        l.registrar_falha(true);
        assert!(!l.bloqueado(true), "LAN passa mesmo com erros dela");
        // lock armado por OUTRA origem: LAN passa, fora não.
        let t = Instant::now();
        l.registrar_falha_em(t);
        l.registrar_falha_em(t);
        l.registrar_falha_em(t);
        assert!(l.bloqueado(false));
        assert!(!l.bloqueado(true), "LAN imune ao lock global");
    }

    /// Sessão longa: create_longa vai pro store (valida como qualquer uma).
    #[test]
    fn sessao_longa_valida() {
        let st = SessionStore::new();
        let t = st.create_longa();
        assert!(st.validate(&t));
        st.revogar_todas();
        assert!(!st.validate(&t), "logout global mata a longa também");
    }
}
