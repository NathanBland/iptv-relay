use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::http::{HeaderMap, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

const SESSION_COOKIE: &str = "iptv_session";
const CSRF_COOKIE: &str = "iptv_csrf";
const CSRF_HEADER: &str = "x-csrf-token";
const SESSION_TTL: Duration = Duration::from_hours(12);
const LOGIN_CSRF_TTL: Duration = Duration::from_mins(30);
const MAX_LOGIN_CSRF_TOKENS: usize = 4_096;
const ADMIN_USERNAME: &str = "operator";

#[derive(Clone)]
pub(crate) struct AuthManager {
    inner: Arc<AuthInner>,
}

struct AuthInner {
    password_hash: Arc<str>,
    bearer_hash: [u8; 32],
    secure_cookies: bool,
    sessions: Mutex<HashMap<[u8; 32], SessionRecord>>,
    login_csrf_tokens: Mutex<HashMap<[u8; 32], Instant>>,
}

impl fmt::Debug for AuthManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthManager")
            .field("password_hash", &"<redacted>")
            .field("bearer_hash", &"<redacted>")
            .field("secure_cookies", &self.inner.secure_cookies)
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
}

impl AuthManager {
    pub(crate) fn new(
        password_hash: impl Into<Arc<str>>,
        bearer_token: &str,
        secure_cookies: bool,
    ) -> Self {
        Self {
            inner: Arc::new(AuthInner {
                password_hash: password_hash.into(),
                bearer_hash: digest(bearer_token),
                secure_cookies,
                sessions: Mutex::new(HashMap::new()),
                login_csrf_tokens: Mutex::new(HashMap::new()),
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

    pub(crate) async fn login(
        &self,
        headers: &HeaderMap,
        username: String,
        password: String,
    ) -> Result<IssuedSession, LoginError> {
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

        let session_token = random_token().map_err(|_| LoginError::Unavailable)?;
        let csrf_token = random_token().map_err(|_| LoginError::Unavailable)?;
        let mut sessions = self.lock_sessions();
        if let CsrfBinding::Session(previous_session) = csrf_binding {
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
        if let CsrfBinding::Login(token) = csrf_binding {
            self.lock_login_csrf_tokens().remove(&token);
        }
        Ok(IssuedSession {
            session_cookie: self.session_cookie(&session_token),
            csrf_cookie: self.csrf_cookie(&csrf_token, SESSION_TTL),
        })
    }

    pub(crate) fn authorize(
        &self,
        headers: &HeaderMap,
        require_csrf: bool,
    ) -> Option<Authorization> {
        if bearer_token(headers)
            .is_some_and(|token| constant_time_eq(&digest(token), &self.inner.bearer_hash))
        {
            return Some(Authorization::Bearer);
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

    pub(crate) fn logout(&self, headers: &HeaderMap) {
        if let Some(token) = cookie(headers, SESSION_COOKIE) {
            self.lock_sessions().remove(&digest(token));
        }
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CsrfBinding {
    Login([u8; 32]),
    Session([u8; 32]),
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

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
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
            .login(
                &login_headers,
                "operator".to_owned(),
                "admin-password".to_owned(),
            )
            .await
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
            .login(
                &session_headers,
                "operator".to_owned(),
                "admin-password".to_owned(),
            )
            .await
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
