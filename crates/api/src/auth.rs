use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::http::{HeaderMap, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::oidc::{OidcAuthorization, OidcClient, OidcConfig, OidcError};

const SESSION_COOKIE: &str = "iptv_session";
const CSRF_COOKIE: &str = "iptv_csrf";
const CSRF_HEADER: &str = "x-csrf-token";
const SESSION_TTL: Duration = Duration::from_hours(12);
const LOGIN_CSRF_TTL: Duration = Duration::from_mins(30);
const MAX_LOGIN_CSRF_TOKENS: usize = 4_096;
const ADMIN_USERNAME: &str = "operator";
pub(crate) const OPERATOR_TOKEN_READ_SCOPE: &str = "read";
pub(crate) const OPERATOR_TOKEN_CONTROL_SCOPE: &str = "control";
pub(crate) const OPERATOR_TOKEN_OUTPUT_SCOPE: &str = "output";
pub(crate) const OPERATOR_TOKEN_ADMIN_SCOPE: &str = "admin";

#[derive(Clone)]
pub(crate) struct AuthManager {
    inner: Arc<AuthInner>,
}

struct AuthInner {
    password_hash: Arc<str>,
    bearer_hash: [u8; 32],
    bootstrap_bearer_enabled: AtomicBool,
    secure_cookies: bool,
    sessions: Mutex<HashMap<[u8; 32], SessionRecord>>,
    login_csrf_tokens: Mutex<HashMap<[u8; 32], Instant>>,
    operator_tokens: Mutex<HashMap<[u8; 32], OperatorTokenRecord>>,
    oidc: Option<OidcClient>,
}

impl fmt::Debug for AuthManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthManager")
            .field("password_hash", &"<redacted>")
            .field("bearer_hash", &"<redacted>")
            .field(
                "bootstrap_bearer_enabled",
                &self.inner.bootstrap_bearer_enabled.load(Ordering::Acquire),
            )
            .field("secure_cookies", &self.inner.secure_cookies)
            .field("operator_token_count", &self.operator_token_count())
            .field("active_sessions", &self.session_count())
            .finish()
    }
}

#[derive(Clone, Debug)]
struct SessionRecord {
    csrf_hash: [u8; 32],
    expires_at: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct IssuedSession {
    pub session_cookie: String,
    pub csrf_cookie: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Authorization {
    Bearer,
    Session,
    OperatorToken,
}

#[derive(Clone, Debug)]
pub(crate) struct OperatorTokenRecord {
    pub id: Uuid,
    pub token_hash: [u8; 32],
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl AuthManager {
    /// Create an authentication manager with OIDC disabled.
    #[allow(dead_code)]
    pub(crate) fn new(
        password_hash: impl Into<Arc<str>>,
        bearer_token: &str,
        secure_cookies: bool,
    ) -> Self {
        Self::new_with_oidc(password_hash, bearer_token, secure_cookies, None)
    }

    /// Create an authentication manager with optional OIDC configuration.
    pub(crate) fn new_with_oidc(
        password_hash: impl Into<Arc<str>>,
        bearer_token: &str,
        secure_cookies: bool,
        oidc_config: Option<OidcConfig>,
    ) -> Self {
        Self {
            inner: Arc::new(AuthInner {
                password_hash: password_hash.into(),
                bearer_hash: digest(bearer_token),
                bootstrap_bearer_enabled: AtomicBool::new(true),
                secure_cookies,
                sessions: Mutex::new(HashMap::new()),
                login_csrf_tokens: Mutex::new(HashMap::new()),
                operator_tokens: Mutex::new(HashMap::new()),
                oidc: oidc_config.map(OidcClient::new),
            }),
        }
    }

    pub(crate) fn ensure_csrf_cookie(&self, headers: &HeaderMap) -> Option<String> {
        let now = Instant::now();
        let cookie_csrf = cookie(headers, CSRF_COOKIE);
        let mut sessions = self.lock_sessions();
        sessions.retain(|_, session| session.expires_at > now);

        if let Some(session_token) = cookie(headers, SESSION_COOKIE)
            && let Some(session) = sessions.get_mut(&digest(session_token))
        {
            if cookie_csrf.is_some_and(|value| constant_time_eq(&digest(value), &session.csrf_hash))
            {
                return None;
            }
            let csrf_token = random_token().ok()?;
            session.csrf_hash = digest(&csrf_token);
            return Some(self.csrf_cookie(&csrf_token, SESSION_TTL));
        }
        drop(sessions);

        let mut login_tokens = self.lock_login_csrf_tokens();
        login_tokens.retain(|_, expires_at| *expires_at > now);
        if cookie_csrf.is_some_and(|value| login_tokens.contains_key(&digest(value))) {
            return None;
        }
        if login_tokens.len() >= MAX_LOGIN_CSRF_TOKENS
            && let Some(oldest) = login_tokens
                .iter()
                .min_by_key(|(_, expires_at)| **expires_at)
                .map(|(token, _)| *token)
        {
            login_tokens.remove(&oldest);
        }
        let csrf_token = random_token().ok()?;
        login_tokens.insert(digest(&csrf_token), now + LOGIN_CSRF_TTL);
        Some(self.csrf_cookie(&csrf_token, LOGIN_CSRF_TTL))
    }

    pub(crate) async fn verify_login(
        &self,
        headers: &HeaderMap,
        username: String,
        password: String,
    ) -> Result<VerifiedLogin, LoginError> {
        let csrf_binding = self
            .csrf_binding(headers)
            .ok_or(LoginError::CsrfValidationFailed)?;
        if password.is_empty() || password.len() > 1024 {
            return Err(LoginError::InvalidCredentials);
        }
        let username_valid = username.len() <= 128
            && constant_time_eq(&digest(username.as_str()), &digest(ADMIN_USERNAME));
        let expected = Arc::clone(&self.inner.password_hash);
        let verified = tokio::task::spawn_blocking(move || {
            PasswordHash::new(&expected).ok().is_some_and(|hash| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
            })
        })
        .await
        .map_err(|_| LoginError::Unavailable)?;
        if !username_valid || !verified {
            return Err(LoginError::InvalidCredentials);
        }

        Ok(VerifiedLogin { csrf_binding })
    }

    pub(crate) fn complete_login(
        &self,
        verified: VerifiedLogin,
    ) -> Result<IssuedSession, LoginError> {
        let session_token = random_token().map_err(|_| LoginError::Unavailable)?;
        let csrf_token = random_token().map_err(|_| LoginError::Unavailable)?;
        let mut sessions = self.lock_sessions();
        if let CsrfBinding::Session(previous_session) = verified.csrf_binding {
            sessions.remove(&previous_session);
        }
        sessions.insert(
            digest(&session_token),
            SessionRecord {
                csrf_hash: digest(&csrf_token),
                expires_at: Instant::now() + SESSION_TTL,
            },
        );
        drop(sessions);
        if let CsrfBinding::Login(token) = verified.csrf_binding {
            self.lock_login_csrf_tokens().remove(&token);
        }
        Ok(IssuedSession {
            session_cookie: self.session_cookie(&session_token),
            csrf_cookie: self.csrf_cookie(&csrf_token, SESSION_TTL),
        })
    }

    pub(crate) async fn start_oidc(&self) -> Result<OidcAuthorization, OidcError> {
        let client = self.inner.oidc.as_ref().ok_or(OidcError::NotConfigured)?;
        client.start(self.inner.secure_cookies).await
    }

    pub(crate) async fn complete_oidc_login(
        &self,
        headers: &HeaderMap,
        state: &str,
        code: Option<&str>,
    ) -> Result<IssuedSession, OidcError> {
        let client = self.inner.oidc.as_ref().ok_or(OidcError::NotConfigured)?;
        client.callback(headers, state, code).await?;
        let session_token = random_token().map_err(|_| OidcError::ProviderUnavailable)?;
        let csrf_token = random_token().map_err(|_| OidcError::ProviderUnavailable)?;
        let mut sessions = self.lock_sessions();
        sessions.insert(
            digest(&session_token),
            SessionRecord {
                csrf_hash: digest(&csrf_token),
                expires_at: Instant::now() + SESSION_TTL,
            },
        );
        drop(sessions);
        Ok(IssuedSession {
            session_cookie: self.session_cookie(&session_token),
            csrf_cookie: self.csrf_cookie(&csrf_token, SESSION_TTL),
        })
    }

    /// Revoke an OIDC authorization attempt after the provider reports an error.
    ///
    /// The state cookie and the server-side state entry remain single-use.
    pub(crate) fn cancel_oidc(&self, headers: &HeaderMap, state: &str) -> Result<(), OidcError> {
        let client = self.inner.oidc.as_ref().ok_or(OidcError::NotConfigured)?;
        client.cancel(headers, state)
    }

    pub(crate) fn clear_oidc_state_cookie(&self) -> String {
        self.inner
            .oidc
            .as_ref()
            .map(|_| crate::oidc::OidcClient::clear_state_cookie(self.inner.secure_cookies))
            .unwrap_or_default()
    }

    pub(crate) fn authorize(
        &self,
        headers: &HeaderMap,
        require_csrf: bool,
    ) -> Option<Authorization> {
        self.authorize_with_scope(headers, require_csrf, None)
    }

    pub(crate) fn authorize_scope(
        &self,
        headers: &HeaderMap,
        require_csrf: bool,
        scope: &str,
    ) -> Option<Authorization> {
        self.authorize_with_scope(headers, require_csrf, Some(scope))
    }

    fn authorize_with_scope(
        &self,
        headers: &HeaderMap,
        require_csrf: bool,
        required_scope: Option<&str>,
    ) -> Option<Authorization> {
        if self.inner.bootstrap_bearer_enabled.load(Ordering::Acquire)
            && bearer_token(headers)
                .is_some_and(|token| constant_time_eq(&digest(token), &self.inner.bearer_hash))
        {
            return Some(Authorization::Bearer);
        }
        if let Some(token) = bearer_token(headers) {
            let token_hash = digest(token);
            let now = Utc::now();
            let mut operator_tokens = self.lock_operator_tokens();
            operator_tokens
                .retain(|_, record| record.expires_at.is_none_or(|expires_at| expires_at > now));
            if let Some(record) = operator_tokens.get(&token_hash)
                && constant_time_eq(&token_hash, &record.token_hash)
                && required_scope.is_none_or(|scope| scope_satisfied(&record.scopes, scope))
            {
                return Some(Authorization::OperatorToken);
            }
        }
        let token = cookie(headers, SESSION_COOKIE)?;
        let token_hash = digest(token);
        let now = Instant::now();
        let mut sessions = self.lock_sessions();
        sessions.retain(|_, session| session.expires_at > now);
        let session = sessions.get(&token_hash)?;
        if require_csrf {
            let header_csrf = headers
                .get(CSRF_HEADER)
                .and_then(|value| value.to_str().ok())?;
            let cookie_csrf = cookie(headers, CSRF_COOKIE)?;
            let header_hash = digest(header_csrf);
            if !constant_time_eq(&header_hash, &digest(cookie_csrf))
                || !constant_time_eq(&header_hash, &session.csrf_hash)
            {
                return None;
            }
        }
        Some(Authorization::Session)
    }

    pub(crate) fn load_operator_tokens(
        &self,
        records: impl IntoIterator<Item = OperatorTokenRecord>,
    ) {
        let mut operator_tokens = self.lock_operator_tokens();
        operator_tokens.clear();
        for record in records {
            operator_tokens.insert(record.token_hash, record);
        }
    }

    pub(crate) fn add_operator_token(&self, record: OperatorTokenRecord) {
        self.lock_operator_tokens()
            .insert(record.token_hash, record);
    }

    pub(crate) fn revoke_operator_token(&self, id: Uuid) {
        self.lock_operator_tokens()
            .retain(|_, record| record.id != id);
    }

    pub(crate) fn operator_token_actor(&self, headers: &HeaderMap) -> String {
        if let Some(token) = bearer_token(headers) {
            let token_hash = digest(token);
            if let Some(record) = self.lock_operator_tokens().get(&token_hash) {
                return format!("operator-api-token:{}", record.id);
            }
        }
        ADMIN_USERNAME.to_owned()
    }

    pub(crate) fn generate_operator_token() -> Result<String, getrandom::Error> {
        random_token()
    }

    pub(crate) fn logout(&self, headers: &HeaderMap) {
        if let Some(token) = cookie(headers, SESSION_COOKIE) {
            self.lock_sessions().remove(&digest(token));
        }
    }

    pub(crate) fn disable_bootstrap_bearer(&self) {
        self.inner
            .bootstrap_bearer_enabled
            .store(false, Ordering::Release);
    }

    pub(crate) fn clear_cookies(&self) -> [String; 2] {
        let secure = if self.inner.secure_cookies {
            "; Secure"
        } else {
            ""
        };
        [
            format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}"),
            format!("{CSRF_COOKIE}=; Path=/; SameSite=Strict; Max-Age=0{secure}"),
        ]
    }

    fn session_count(&self) -> usize {
        self.lock_sessions().len()
    }

    fn operator_token_count(&self) -> usize {
        self.lock_operator_tokens().len()
    }

    fn csrf_binding(&self, headers: &HeaderMap) -> Option<CsrfBinding> {
        let cookie_token = cookie(headers, CSRF_COOKIE)?;
        let header_token = headers.get(CSRF_HEADER)?.to_str().ok()?;
        let csrf_hash = digest(header_token);
        if !constant_time_eq(&csrf_hash, &digest(cookie_token)) {
            return None;
        }

        let now = Instant::now();
        let mut sessions = self.lock_sessions();
        sessions.retain(|_, session| session.expires_at > now);
        if let Some(session_token) = cookie(headers, SESSION_COOKIE) {
            let session_hash = digest(session_token);
            if let Some(session) = sessions.get(&session_hash) {
                return constant_time_eq(&csrf_hash, &session.csrf_hash)
                    .then_some(CsrfBinding::Session(session_hash));
            }
        }
        drop(sessions);

        let mut login_tokens = self.lock_login_csrf_tokens();
        login_tokens.retain(|_, expires_at| *expires_at > now);
        login_tokens
            .contains_key(&csrf_hash)
            .then_some(CsrfBinding::Login(csrf_hash))
    }

    fn session_cookie(&self, token: &str) -> String {
        format!(
            "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            SESSION_TTL.as_secs(),
            self.secure_attribute()
        )
    }

    fn csrf_cookie(&self, token: &str, ttl: Duration) -> String {
        format!(
            "{CSRF_COOKIE}={token}; Path=/; SameSite=Strict; Max-Age={}{}",
            ttl.as_secs(),
            self.secure_attribute()
        )
    }

    fn secure_attribute(&self) -> &'static str {
        if self.inner.secure_cookies {
            "; Secure"
        } else {
            ""
        }
    }

    fn lock_sessions(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], SessionRecord>> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_login_csrf_tokens(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Instant>> {
        self.inner
            .login_csrf_tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_operator_tokens(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], OperatorTokenRecord>> {
        self.inner
            .operator_tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CsrfBinding {
    Login([u8; 32]),
    Session([u8; 32]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedLogin {
    csrf_binding: CsrfBinding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoginError {
    CsrfValidationFailed,
    InvalidCredentials,
    Unavailable,
}

/// Creates a PHC-formatted Argon2id password hash with a random salt.
///
/// # Errors
///
/// Returns an error when the password length is unsafe, operating-system
/// randomness is unavailable, or Argon2 rejects the parameters.
pub fn hash_admin_password(password: &str) -> Result<String, String> {
    if password.is_empty() || password.len() > 1024 {
        return Err("administrator password must contain 1-1024 bytes".to_owned());
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| error.to_string())?;
    let salt = SaltString::encode_b64(&random).map_err(|error| error.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| error.to_string())
}

fn random_token() -> Result<String, getrandom::Error> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

pub(crate) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn scope_satisfied(scopes: &[String], required: &str) -> bool {
    scopes.iter().any(|scope| {
        scope == OPERATOR_TOKEN_ADMIN_SCOPE
            || scope == required
            || (required == OPERATOR_TOKEN_READ_SCOPE && scope == OPERATOR_TOKEN_CONTROL_SCOPE)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parser_handles_multiple_values() {
        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, "one=1; iptv_session=abc".parse().unwrap());
        headers.append(header::COOKIE, "two=2".parse().unwrap());
        assert_eq!(cookie(&headers, SESSION_COOKIE), Some("abc"));
        assert_eq!(cookie(&headers, "missing"), None);
    }

    #[test]
    fn password_hash_is_argon2id_and_verifiable() {
        assert!(hash_admin_password("").is_err());
        assert!(hash_admin_password(&"x".repeat(1_025)).is_err());
        let hash = hash_admin_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$v=19$"));
        let parsed = PasswordHash::new(&hash).unwrap();
        assert!(
            Argon2::default()
                .verify_password(b"correct horse battery staple", &parsed)
                .is_ok()
        );
        assert!(
            Argon2::default()
                .verify_password(b"wrong", &parsed)
                .is_err()
        );
    }

    #[test]
    fn bootstrap_bearer_authorization_can_disable() {
        let manager = AuthManager::new(
            hash_admin_password("admin-password").unwrap(),
            "bootstrap-token",
            false,
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer bootstrap-token".parse().unwrap(),
        );
        assert_eq!(
            manager.authorize(&headers, false),
            Some(Authorization::Bearer)
        );

        manager.disable_bootstrap_bearer();

        assert_eq!(manager.authorize(&headers, false), None);
    }

    #[test]
    fn operator_tokens_enforce_scopes_and_expiration() {
        let manager = AuthManager::new(
            hash_admin_password("admin-password").unwrap(),
            "bootstrap-token",
            false,
        );
        let token = "operator-api-secret";
        manager.add_operator_token(OperatorTokenRecord {
            id: Uuid::now_v7(),
            token_hash: digest(token),
            scopes: vec![OPERATOR_TOKEN_READ_SCOPE.to_owned()],
            expires_at: None,
        });
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer operator-api-secret".parse().unwrap(),
        );
        assert_eq!(
            manager.authorize(&headers, false),
            Some(Authorization::OperatorToken)
        );
        assert_eq!(
            manager.authorize_scope(&headers, false, OPERATOR_TOKEN_READ_SCOPE),
            Some(Authorization::OperatorToken)
        );
        assert_eq!(
            manager.authorize_scope(&headers, false, OPERATOR_TOKEN_CONTROL_SCOPE),
            None
        );

        manager.add_operator_token(OperatorTokenRecord {
            id: Uuid::now_v7(),
            token_hash: digest(token),
            scopes: vec![OPERATOR_TOKEN_ADMIN_SCOPE.to_owned()],
            expires_at: Some(Utc::now() - chrono::Duration::seconds(1)),
        });
        assert_eq!(manager.authorize(&headers, false), None);
    }

    #[tokio::test]
    async fn secure_login_rotates_an_existing_session() {
        let manager = AuthManager::new(
            hash_admin_password("admin-password").unwrap(),
            "bootstrap-token",
            true,
        );
        let initial_csrf_cookie = manager.ensure_csrf_cookie(&HeaderMap::new()).unwrap();
        assert!(initial_csrf_cookie.contains("; Secure"));
        assert!(!initial_csrf_cookie.contains("HttpOnly"));
        let initial_csrf = initial_csrf_cookie
            .strip_prefix("iptv_csrf=")
            .and_then(|value| value.split(';').next())
            .unwrap();
        let mut login_headers = HeaderMap::new();
        login_headers.insert(
            header::COOKIE,
            format!("iptv_csrf={initial_csrf}").parse().unwrap(),
        );
        login_headers.insert(CSRF_HEADER, initial_csrf.parse().unwrap());
        assert!(manager.ensure_csrf_cookie(&login_headers).is_none());
        let debug = format!("{manager:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("admin-password"));

        let first = manager
            .complete_login(
                manager
                    .verify_login(
                        &login_headers,
                        "operator".to_owned(),
                        "admin-password".to_owned(),
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
        assert!(first.session_cookie.contains("HttpOnly"));
        assert!(first.session_cookie.contains("; Secure"));
        assert!(first.csrf_cookie.contains("; Secure"));

        let first_cookie_header = format!(
            "{}; {}",
            first.session_cookie.split(';').next().unwrap(),
            first.csrf_cookie.split(';').next().unwrap()
        );
        let first_csrf = first
            .csrf_cookie
            .strip_prefix("iptv_csrf=")
            .and_then(|value| value.split(';').next())
            .unwrap();
        let mut session_headers = HeaderMap::new();
        session_headers.insert(header::COOKIE, first_cookie_header.parse().unwrap());
        session_headers.insert(CSRF_HEADER, first_csrf.parse().unwrap());
        let mut mismatched_headers = session_headers.clone();
        mismatched_headers.insert(CSRF_HEADER, "different-token".parse().unwrap());
        assert!(manager.authorize(&mismatched_headers, true).is_none());
        let second = manager
            .complete_login(
                manager
                    .verify_login(
                        &session_headers,
                        "operator".to_owned(),
                        "admin-password".to_owned(),
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
        assert_ne!(first.session_cookie, second.session_cookie);
        assert_ne!(first.csrf_cookie, second.csrf_cookie);
        assert!(manager.authorize(&session_headers, false).is_none());
        let mut session_without_csrf = HeaderMap::new();
        session_without_csrf.insert(
            header::COOKIE,
            second
                .session_cookie
                .split(';')
                .next()
                .unwrap()
                .parse()
                .unwrap(),
        );
        assert!(manager.ensure_csrf_cookie(&session_without_csrf).is_some());
        assert!(
            manager
                .clear_cookies()
                .iter()
                .all(|cookie| cookie.contains("; Secure"))
        );
    }
}
