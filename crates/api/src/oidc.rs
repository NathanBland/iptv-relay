//! `OpenID` Connect discovery, authorization-code, and token validation.
//!
//! The module keeps the OIDC protocol state in memory. The local administrator
//! login remains available when OIDC is disabled or when the provider is down.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::auth::cookie;

pub(crate) const OIDC_STATE_COOKIE: &str = "iptv_oidc_state";
const OIDC_STATE_TTL: Duration = Duration::from_mins(10);
const MAX_PENDING_LOGINS: usize = 4_096;
const MAX_STATE_BYTES: usize = 128;
const MAX_CODE_BYTES: usize = 8_192;

/// OIDC settings supplied by the operator.
///
/// The client secret and allowlist values never appear in the debug output.
#[derive(Clone)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_url: String,
    pub allowed_subjects: Vec<String>,
    pub allowed_emails: Vec<String>,
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for OidcConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OidcConfig")
            .field("issuer_url", &self.issuer_url)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("redirect_url", &self.redirect_url)
            .field("allowed_subject_count", &self.allowed_subjects.len())
            .field("allowed_email_count", &self.allowed_emails.len())
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl OidcConfig {
    /// Validate and create OIDC settings.
    ///
    /// The configuration requires at least one approved subject or email.
    ///
    /// # Errors
    ///
    /// Returns an error when a URL, client value, scope, or allowlist is invalid.
    pub fn new(
        issuer_url: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: Option<String>,
        redirect_url: impl Into<String>,
        allowed_subjects: Vec<String>,
        allowed_emails: Vec<String>,
        scopes: Vec<String>,
    ) -> Result<Self, String> {
        let issuer_url = issuer_url.into().trim().to_owned();
        let client_id = client_id.into().trim().to_owned();
        let redirect_url = redirect_url.into().trim().to_owned();
        let client_secret = client_secret
            .map(|secret| secret.trim().to_owned())
            .filter(|secret| !secret.is_empty());
        let allowed_subjects = normalize_entries(allowed_subjects, false);
        let allowed_emails = normalize_entries(allowed_emails, true);
        let scopes = normalize_scopes(scopes);

        validate_endpoint_url(&issuer_url, "issuer")?;
        validate_endpoint_url(&redirect_url, "redirect")?;
        if client_id.is_empty() || client_id.len() > 256 || contains_control(&client_id) {
            return Err("OIDC client ID must contain 1-256 non-control characters".to_owned());
        }
        if client_secret
            .as_deref()
            .is_some_and(|secret| secret.len() > 4_096 || contains_control(secret))
        {
            return Err(
                "OIDC client secret is too long or contains a control character".to_owned(),
            );
        }
        if allowed_subjects.is_empty() && allowed_emails.is_empty() {
            return Err("OIDC requires an approved subject or email allowlist".to_owned());
        }
        if scopes.is_empty() || scopes.iter().any(|scope| contains_control(scope)) {
            return Err("OIDC scope values must contain non-control characters".to_owned());
        }

        Ok(Self {
            issuer_url,
            client_id,
            client_secret,
            redirect_url,
            allowed_subjects,
            allowed_emails,
            scopes,
        })
    }

    pub(crate) fn allows(
        &self,
        subject: &str,
        email: Option<&str>,
        email_verified: Option<bool>,
    ) -> bool {
        if self.allowed_subjects.iter().any(|value| value == subject) {
            return true;
        }
        let Some(email) = email else {
            return false;
        };
        if email_verified != Some(true) {
            return false;
        }
        let email = email.trim().to_ascii_lowercase();
        self.allowed_emails.iter().any(|value| value == &email)
    }

    fn issuer(&self) -> Result<Url, OidcError> {
        Url::parse(&self.issuer_url).map_err(|_| OidcError::InvalidConfiguration)
    }
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OidcError {
    #[error("OIDC is not configured")]
    NotConfigured,
    #[error("OIDC configuration is invalid")]
    InvalidConfiguration,
    #[error("OIDC provider metadata is invalid")]
    InvalidProvider,
    #[error("OIDC provider is unavailable")]
    ProviderUnavailable,
    #[error("OIDC authorization state is invalid or expired")]
    InvalidState,
    #[error("OIDC callback is invalid")]
    InvalidCallback,
    #[error("OIDC identity is not approved")]
    IdentityNotAllowed,
    #[error("OIDC identity token is invalid")]
    InvalidIdentityToken,
}

#[derive(Clone, Debug)]
pub(crate) struct OidcAuthorization {
    pub location: String,
    pub state_cookie: String,
}

#[derive(Clone)]
pub(crate) struct OidcClient {
    inner: Arc<OidcInner>,
}

struct OidcInner {
    config: OidcConfig,
    http: Client,
    provider: Mutex<Option<OidcProvider>>,
    pending: Mutex<HashMap<[u8; 32], PendingLogin>>,
}

impl std::fmt::Debug for OidcClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OidcClient")
            .field("config", &self.inner.config)
            .field("provider_cached", &self.lock_provider().is_some())
            .field("pending_logins", &self.lock_pending().len())
            .finish()
    }
}

#[derive(Clone, Debug)]
struct PendingLogin {
    verifier: String,
    nonce: String,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
struct OidcProvider {
    issuer: String,
    authorization_endpoint: Url,
    token_endpoint: Url,
    jwks_uri: Url,
}

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct JsonWebKeySet {
    keys: Vec<JsonWebKey>,
}

#[derive(Debug, Deserialize)]
struct JsonWebKey {
    kty: String,
    kid: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    aud: serde_json::Value,
    exp: i64,
    nonce: String,
    email: Option<String>,
    email_verified: Option<bool>,
    azp: Option<String>,
}

impl OidcClient {
    pub(crate) fn new(config: OidcConfig) -> Self {
        Self {
            inner: Arc::new(OidcInner {
                config,
                http: Client::new(),
                provider: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) async fn start(&self, secure_cookies: bool) -> Result<OidcAuthorization, OidcError> {
        let provider = self.provider().await?;
        let state = random_token().ok_or(OidcError::ProviderUnavailable)?;
        let verifier = random_token().ok_or(OidcError::ProviderUnavailable)?;
        let nonce = random_token().ok_or(OidcError::ProviderUnavailable)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

        let mut pending = self.lock_pending();
        let now = Instant::now();
        pending.retain(|_, value| value.expires_at > now);
        if pending.len() >= MAX_PENDING_LOGINS
            && let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, value)| value.expires_at)
                .map(|(key, _)| *key)
        {
            pending.remove(&oldest);
        }
        pending.insert(
            digest(&state),
            PendingLogin {
                verifier,
                nonce: nonce.clone(),
                expires_at: now + OIDC_STATE_TTL,
            },
        );
        drop(pending);

        let mut location = provider.authorization_endpoint.clone();
        {
            let mut query = location.query_pairs_mut();
            query
                .append_pair("client_id", &self.inner.config.client_id)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &self.inner.config.redirect_url)
                .append_pair("scope", &self.inner.config.scopes.join(" "))
                .append_pair("state", &state)
                .append_pair("nonce", &nonce)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
        }

        Ok(OidcAuthorization {
            location: location.to_string(),
            state_cookie: state_cookie(&state, secure_cookies),
        })
    }

    pub(crate) async fn callback(
        &self,
        headers: &HeaderMap,
        state: &str,
        code: Option<&str>,
    ) -> Result<(), OidcError> {
        Self::verify_state_cookie(headers, state)?;
        let pending = self.take_pending(state)?;
        let code = code
            .filter(|value| !value.is_empty() && value.len() <= MAX_CODE_BYTES)
            .ok_or(OidcError::InvalidCallback)?;
        let provider = self.provider().await?;
        let response = self
            .inner
            .http
            .post(provider.token_endpoint.clone())
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.inner.config.redirect_url.as_str()),
                ("client_id", self.inner.config.client_id.as_str()),
                ("code_verifier", pending.verifier.as_str()),
            ])
            .basic_auth(
                &self.inner.config.client_id,
                self.inner.config.client_secret.as_deref(),
            )
            .send()
            .await
            .map_err(|_| OidcError::ProviderUnavailable)?;
        if !response.status().is_success() {
            return Err(OidcError::ProviderUnavailable);
        }
        let token = response
            .json::<TokenResponse>()
            .await
            .map_err(|_| OidcError::InvalidCallback)?;
        let id_token = token
            .id_token
            .filter(|value| !value.is_empty())
            .ok_or(OidcError::InvalidCallback)?;
        let claims = self.verify_id_token(&provider, &id_token).await?;
        if claims.nonce != pending.nonce || claims.sub.is_empty() || claims.sub.len() > 256 {
            return Err(OidcError::InvalidIdentityToken);
        }
        if !self
            .inner
            .config
            .allows(&claims.sub, claims.email.as_deref(), claims.email_verified)
        {
            return Err(OidcError::IdentityNotAllowed);
        }
        Ok(())
    }

    pub(crate) fn cancel(&self, headers: &HeaderMap, state: &str) -> Result<(), OidcError> {
        Self::verify_state_cookie(headers, state)?;
        self.take_pending(state).map(|_| ())
    }

    fn verify_state_cookie(headers: &HeaderMap, state: &str) -> Result<(), OidcError> {
        if state.is_empty()
            || state.len() > MAX_STATE_BYTES
            || !state
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || !cookie(headers, OIDC_STATE_COOKIE)
                .is_some_and(|value| constant_time_eq(&digest(value), &digest(state)))
        {
            return Err(OidcError::InvalidState);
        }
        Ok(())
    }

    pub(crate) fn clear_state_cookie(secure_cookies: bool) -> String {
        format!(
            "{OIDC_STATE_COOKIE}=; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age=0{}",
            secure_attribute(secure_cookies)
        )
    }

    fn take_pending(&self, state: &str) -> Result<PendingLogin, OidcError> {
        let mut pending = self.lock_pending();
        let value = pending
            .remove(&digest(state))
            .ok_or(OidcError::InvalidState)?;
        (value.expires_at > Instant::now())
            .then_some(value)
            .ok_or(OidcError::InvalidState)
    }

    async fn provider(&self) -> Result<OidcProvider, OidcError> {
        if let Some(provider) = self.lock_provider().clone() {
            return Ok(provider);
        }
        let issuer = self.inner.config.issuer()?;
        let metadata_url = discovery_url(&issuer)?;
        let response = self
            .inner
            .http
            .get(metadata_url)
            .send()
            .await
            .map_err(|_| OidcError::ProviderUnavailable)?;
        if !response.status().is_success() {
            return Err(OidcError::ProviderUnavailable);
        }
        let document = response
            .json::<DiscoveryDocument>()
            .await
            .map_err(|_| OidcError::InvalidProvider)?;
        if document.issuer != self.inner.config.issuer_url {
            return Err(OidcError::InvalidProvider);
        }
        let provider = OidcProvider {
            issuer: document.issuer,
            authorization_endpoint: parse_provider_endpoint(&document.authorization_endpoint)?,
            token_endpoint: parse_provider_endpoint(&document.token_endpoint)?,
            jwks_uri: parse_provider_endpoint(&document.jwks_uri)?,
        };
        *self.lock_provider() = Some(provider.clone());
        Ok(provider)
    }

    async fn verify_id_token(
        &self,
        provider: &OidcProvider,
        id_token: &str,
    ) -> Result<IdTokenClaims, OidcError> {
        let header = decode_header(id_token).map_err(|_| OidcError::InvalidIdentityToken)?;
        let kid = header
            .kid
            .as_deref()
            .ok_or(OidcError::InvalidIdentityToken)?;
        let algorithm = header.alg;
        if !matches!(
            algorithm,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::PS256
                | Algorithm::PS384
                | Algorithm::PS512
        ) {
            return Err(OidcError::InvalidIdentityToken);
        }
        let response = self
            .inner
            .http
            .get(provider.jwks_uri.clone())
            .send()
            .await
            .map_err(|_| OidcError::ProviderUnavailable)?;
        if !response.status().is_success() {
            return Err(OidcError::ProviderUnavailable);
        }
        let keys = response
            .json::<JsonWebKeySet>()
            .await
            .map_err(|_| OidcError::InvalidProvider)?;
        let key = keys
            .keys
            .into_iter()
            .find(|key| key.kty == "RSA" && key.kid.as_deref() == Some(kid))
            .ok_or(OidcError::InvalidIdentityToken)?;
        let n = key.n.ok_or(OidcError::InvalidIdentityToken)?;
        let e = key.e.ok_or(OidcError::InvalidIdentityToken)?;
        let decoding_key = DecodingKey::from_rsa_components(&n, &e)
            .map_err(|_| OidcError::InvalidIdentityToken)?;
        let mut validation = Validation::new(algorithm);
        validation.set_issuer(&[provider.issuer.as_str()]);
        validation.set_audience(&[self.inner.config.client_id.as_str()]);
        let token = decode::<IdTokenClaims>(id_token, &decoding_key, &validation)
            .map_err(|_| OidcError::InvalidIdentityToken)?;
        if token.claims.iss != provider.issuer
            || !audience_contains(&token.claims.aud, &self.inner.config.client_id)
            || !audience_party_is_valid(
                &token.claims.aud,
                token.claims.azp.as_deref(),
                &self.inner.config.client_id,
            )
            || token.claims.exp <= 0
        {
            return Err(OidcError::InvalidIdentityToken);
        }
        Ok(token.claims)
    }

    fn lock_provider(&self) -> std::sync::MutexGuard<'_, Option<OidcProvider>> {
        self.inner
            .provider
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], PendingLogin>> {
        self.inner
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn audience_contains(value: &serde_json::Value, client_id: &str) -> bool {
    match value {
        serde_json::Value::String(value) => value == client_id,
        serde_json::Value::Array(values) => {
            values.iter().any(|value| value.as_str() == Some(client_id))
        }
        _ => false,
    }
}

fn audience_party_is_valid(
    audience: &serde_json::Value,
    authorized_party: Option<&str>,
    client_id: &str,
) -> bool {
    match audience {
        serde_json::Value::Array(values) if values.len() > 1 => authorized_party == Some(client_id),
        _ => true,
    }
}

fn normalize_entries(values: Vec<String>, lowercase: bool) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && !contains_control(value))
        .map(|value| {
            if lowercase {
                value.to_ascii_lowercase()
            } else {
                value
            }
        })
        .collect()
}

fn normalize_scopes(values: Vec<String>) -> Vec<String> {
    let mut values = values
        .into_iter()
        .flat_map(|value| {
            value
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        values = ["openid", "profile", "email"]
            .into_iter()
            .map(str::to_owned)
            .collect();
    }
    values
}

fn validate_endpoint_url(value: &str, name: &str) -> Result<(), String> {
    let url = Url::parse(value).map_err(|_| format!("OIDC {name} URL is invalid"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "OIDC {name} URL must use HTTPS and have no credentials or fragment"
        ));
    }
    if url.scheme() == "http" && !is_loopback_host(url.host_str().unwrap_or_default()) {
        return Err(format!(
            "OIDC {name} URL must use HTTPS outside loopback development hosts"
        ));
    }
    Ok(())
}

fn parse_provider_endpoint(value: &str) -> Result<Url, OidcError> {
    let url = Url::parse(value).map_err(|_| OidcError::InvalidProvider)?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || (url.scheme() == "http" && !is_loopback_host(url.host_str().unwrap_or_default()))
    {
        return Err(OidcError::InvalidProvider);
    }
    Ok(url)
}

fn discovery_url(issuer: &Url) -> Result<Url, OidcError> {
    let mut value = issuer.as_str().trim_end_matches('/').to_owned();
    value.push_str("/.well-known/openid-configuration");
    Url::parse(&value).map_err(|_| OidcError::InvalidConfiguration)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "[::1]"
        || host == "::1"
}

fn contains_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn random_token() -> Option<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
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

fn state_cookie(state: &str, secure: bool) -> String {
    format!(
        "{OIDC_STATE_COOKIE}={state}; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age={}{}",
        OIDC_STATE_TTL.as_secs(),
        secure_attribute(secure)
    )
}

fn secure_attribute(secure: bool) -> &'static str {
    if secure { "; Secure" } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OidcConfig {
        OidcConfig::new(
            "http://localhost:43127/issuer",
            "gateway-client",
            Some("client-secret".to_owned()),
            "http://localhost:8080/api/v1/auth/oidc/callback",
            vec!["subject-1".to_owned()],
            vec!["Admin@Example.Test".to_owned()],
            vec![],
        )
        .expect("valid OIDC config")
    }

    #[test]
    fn config_requires_allowlist_and_https_for_non_loopback() {
        assert!(
            OidcConfig::new(
                "http://idp.example.test",
                "client",
                None,
                "https://gateway.example.test/callback",
                vec!["subject".to_owned()],
                vec![],
                vec![],
            )
            .is_err()
        );
        assert!(
            OidcConfig::new(
                "https://idp.example.test",
                "client",
                None,
                "https://gateway.example.test/callback",
                vec![],
                vec![],
                vec![],
            )
            .is_err()
        );
    }

    #[test]
    fn config_normalizes_email_and_default_scopes() {
        let value = config();
        assert_eq!(value.allowed_emails, ["admin@example.test"]);
        assert_eq!(value.scopes, ["openid", "profile", "email"]);
        let debug = format!("{value:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("client-secret"));
    }

    #[test]
    fn allowlist_requires_verified_email_when_email_is_used() {
        let value = config();
        assert!(value.allows("other", Some("ADMIN@example.test"), Some(true)));
        assert!(value.allows("subject-1", None, Some(false)));
        assert!(!value.allows("other", Some("ADMIN@example.test"), Some(false)));
        assert!(!value.allows("other", Some("unknown@example.test"), Some(true)));
    }

    #[test]
    fn discovery_url_preserves_issuer_path() {
        let issuer = Url::parse("https://idp.example.test/tenant").unwrap();
        assert_eq!(
            discovery_url(&issuer).unwrap().as_str(),
            "https://idp.example.test/tenant/.well-known/openid-configuration"
        );
    }

    #[test]
    fn state_cookie_has_lax_same_site_and_secure_attributes() {
        let cookie = state_cookie("state", true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("Max-Age=600"));
    }

    #[test]
    fn audience_accepts_string_or_array() {
        assert!(audience_contains(&serde_json::json!("client"), "client"));
        assert!(audience_contains(
            &serde_json::json!(["other", "client"]),
            "client"
        ));
        assert!(!audience_contains(&serde_json::json!(null), "client"));
    }

    #[test]
    fn callback_cookie_comparison_is_exact() {
        let state = "state-value";
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{OIDC_STATE_COOKIE}={state}").parse().unwrap(),
        );
        assert!(
            cookie(&headers, OIDC_STATE_COOKIE)
                .is_some_and(|value| { constant_time_eq(&digest(value), &digest(state)) })
        );
        assert!(
            !cookie(&headers, OIDC_STATE_COOKIE)
                .is_some_and(|value| { constant_time_eq(&digest(value), &digest("other")) })
        );
    }

    #[tokio::test]
    async fn start_creates_single_use_state_and_s256_pkce_challenge() {
        let client = OidcClient::new(config());
        *client.lock_provider() = Some(OidcProvider {
            issuer: "http://localhost:43127/issuer".to_owned(),
            authorization_endpoint: Url::parse("http://localhost:43127/authorize").unwrap(),
            token_endpoint: Url::parse("http://localhost:43127/token").unwrap(),
            jwks_uri: Url::parse("http://localhost:43127/keys").unwrap(),
        });

        let authorization = client.start(false).await.expect("start OIDC");
        let location = Url::parse(&authorization.location).expect("authorization URL");
        let state = location
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("state query value");
        let challenge = location
            .query_pairs()
            .find_map(|(key, value)| (key == "code_challenge").then(|| value.into_owned()))
            .expect("PKCE challenge");
        let pending = client
            .lock_pending()
            .get(&digest(&state))
            .cloned()
            .expect("pending state");
        assert_eq!(
            challenge,
            URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes()))
        );
        assert_eq!(
            location
                .query_pairs()
                .find_map(|(key, value)| (key == "nonce").then(|| value.into_owned()))
                .as_deref(),
            Some(pending.nonce.as_str())
        );
        assert!(authorization.state_cookie.contains("HttpOnly"));
        assert!(authorization.state_cookie.contains("SameSite=Lax"));
    }

    #[tokio::test]
    async fn callback_consumes_state_before_code_exchange_and_rejects_replay() {
        let client = OidcClient::new(config());
        let state = "state-value";
        client.lock_pending().insert(
            digest(state),
            PendingLogin {
                verifier: "verifier".to_owned(),
                nonce: "nonce".to_owned(),
                expires_at: Instant::now() + OIDC_STATE_TTL,
            },
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{OIDC_STATE_COOKIE}={state}").parse().unwrap(),
        );

        assert!(matches!(
            client.callback(&headers, state, None).await,
            Err(OidcError::InvalidCallback)
        ));
        assert!(matches!(
            client.callback(&headers, state, Some("code")).await,
            Err(OidcError::InvalidState)
        ));
    }

    #[test]
    fn cancel_requires_matching_cookie_and_consumes_state() {
        let client = OidcClient::new(config());
        let state = "state-value";
        client.lock_pending().insert(
            digest(state),
            PendingLogin {
                verifier: "verifier".to_owned(),
                nonce: "nonce".to_owned(),
                expires_at: Instant::now() + OIDC_STATE_TTL,
            },
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{OIDC_STATE_COOKIE}={state}").parse().unwrap(),
        );
        assert_eq!(client.cancel(&headers, state), Ok(()));
        assert_eq!(client.cancel(&headers, state), Err(OidcError::InvalidState));
    }

    #[test]
    fn multiple_audiences_require_authorized_party() {
        let audience = serde_json::json!(["gateway-client", "other-client"]);
        assert!(!audience_party_is_valid(&audience, None, "gateway-client"));
        assert!(!audience_party_is_valid(
            &audience,
            Some("other-client"),
            "gateway-client"
        ));
        assert!(audience_party_is_valid(
            &audience,
            Some("gateway-client"),
            "gateway-client"
        ));
        assert!(audience_party_is_valid(
            &serde_json::json!("gateway-client"),
            None,
            "gateway-client"
        ));
    }
}
