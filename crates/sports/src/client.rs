//! Async ESPN client with an in-memory TTL cache and a simple circuit breaker.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use parking_lot::Mutex;

use crate::league::League;
use crate::matcher::{EventMatch, MatchQuery, match_event};
use crate::model::{EspnEvent, EspnTeam, parse_scoreboard, parse_teams};

/// The public ESPN API base URL.
pub const DEFAULT_BASE_URL: &str = "https://site.api.espn.com/apis/site/v2/sports";
/// A generic client name. Do not add contact data.
pub const USER_AGENT: &str = "Mozilla/5.0 (compatible; iptv-relay)";

/// Errors from the ESPN client.
#[derive(Debug, thiserror::Error)]
pub enum EspnError {
    #[error("ESPN request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("ESPN response was not valid JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("ESPN client is paused after repeated failures")]
    CircuitOpen,
}

/// Client settings. The defaults are the production values.
#[derive(Debug, Clone)]
pub struct EspnClientConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub scoreboard_ttl: Duration,
    pub teams_ttl: Duration,
    /// Consecutive failures before the circuit opens.
    pub failure_threshold: u32,
    /// How long the circuit stays open.
    pub open_for: Duration,
}

impl Default for EspnClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(5),
            scoreboard_ttl: Duration::from_mins(10),
            teams_ttl: Duration::from_hours(24),
            failure_threshold: 3,
            open_for: Duration::from_mins(1),
        }
    }
}

struct Entry<T> {
    at: Instant,
    value: Arc<T>,
}

#[derive(Default)]
struct State {
    scoreboards: HashMap<(League, NaiveDate), Entry<Vec<EspnEvent>>>,
    teams: HashMap<League, Entry<Vec<EspnTeam>>>,
    failures: u32,
    open_until: Option<Instant>,
}

/// Cached ESPN client. Clone it to share the cache.
#[derive(Clone)]
pub struct EspnClient {
    http: reqwest::Client,
    config: Arc<EspnClientConfig>,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for EspnClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EspnClient")
            .field("base_url", &self.config.base_url)
            .finish_non_exhaustive()
    }
}

impl EspnClient {
    /// Create a client with the production settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot start.
    pub fn new() -> Result<Self, EspnError> {
        Self::with_config(EspnClientConfig::default())
    }

    /// Create a client with custom settings.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot start.
    pub fn with_config(config: EspnClientConfig) -> Result<Self, EspnError> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(config.timeout)
            .build()?;
        Ok(Self {
            http,
            config: Arc::new(config),
            state: Arc::new(Mutex::new(State::default())),
        })
    }

    fn check_circuit(&self) -> Result<(), EspnError> {
        let mut st = self.state.lock();
        if let Some(until) = st.open_until {
            if Instant::now() < until {
                return Err(EspnError::CircuitOpen);
            }
            st.open_until = None;
            st.failures = 0;
        }
        Ok(())
    }

    fn record(&self, ok: bool) {
        let mut st = self.state.lock();
        if ok {
            st.failures = 0;
        } else {
            st.failures += 1;
            if st.failures >= self.config.failure_threshold {
                st.open_until = Some(Instant::now() + self.config.open_for);
            }
        }
    }

    async fn get(&self, url: &str) -> Result<Vec<u8>, EspnError> {
        self.check_circuit()?;
        let result = async {
            let resp = self.http.get(url).send().await?.error_for_status()?;
            Ok::<_, reqwest::Error>(resp.bytes().await?.to_vec())
        }
        .await;
        self.record(result.is_ok());
        Ok(result?)
    }

    /// Fetch the events of one league on one local date. Cached for the scoreboard TTL.
    ///
    /// # Errors
    ///
    /// Returns an error on a network failure, a bad status, a bad body, or an open circuit.
    pub async fn scoreboard(
        &self,
        league: League,
        date: NaiveDate,
    ) -> Result<Arc<Vec<EspnEvent>>, EspnError> {
        if let Some(e) = self.state.lock().scoreboards.get(&(league, date))
            && e.at.elapsed() < self.config.scoreboard_ttl
        {
            return Ok(e.value.clone());
        }
        let url = format!(
            "{}/{}/scoreboard?dates={}",
            self.config.base_url,
            league.espn_path(),
            date.format("%Y%m%d")
        );
        let body = self.get(&url).await?;
        let value = Arc::new(parse_scoreboard(&body)?);
        self.state.lock().scoreboards.insert(
            (league, date),
            Entry {
                at: Instant::now(),
                value: value.clone(),
            },
        );
        Ok(value)
    }

    /// Fetch the team list of one league. Cached for the teams TTL.
    ///
    /// # Errors
    ///
    /// Returns an error on a network failure, a bad status, a bad body, or an open circuit.
    pub async fn teams(&self, league: League) -> Result<Arc<Vec<EspnTeam>>, EspnError> {
        if let Some(e) = self.state.lock().teams.get(&league)
            && e.at.elapsed() < self.config.teams_ttl
        {
            return Ok(e.value.clone());
        }
        let url = format!(
            "{}/{}/teams?limit=1000",
            self.config.base_url,
            league.espn_path()
        );
        let body = self.get(&url).await?;
        let value = Arc::new(parse_teams(&body)?);
        self.state.lock().teams.insert(
            league,
            Entry {
                at: Instant::now(),
                value: value.clone(),
            },
        );
        Ok(value)
    }

    /// Search the local date and the next date, then match.
    ///
    /// A failure on the next date does not hide a match on the local date.
    ///
    /// # Errors
    ///
    /// Returns an error when the local date cannot be fetched and no match exists.
    pub async fn lookup(
        &self,
        league: League,
        local_date: NaiveDate,
        query: &MatchQuery,
    ) -> Result<Option<EventMatch>, EspnError> {
        let mut events: Vec<EspnEvent> = Vec::new();
        let first = self.scoreboard(league, local_date).await;
        let mut first_err = None;
        match first {
            Ok(v) => events.extend(v.iter().cloned()),
            Err(e) => first_err = Some(e),
        }
        if let Some(next) = local_date.succ_opt()
            && let Ok(v) = self.scoreboard(league, next).await
        {
            events.extend(v.iter().cloned());
        }
        match (match_event(query, &events), first_err) {
            (Some(m), _) => Ok(Some(m)),
            (None, Some(e)) if events.is_empty() => Err(e),
            (None, _) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serve fixtures from memory. `/scoreboard?dates=20261104` and `20261105` and `/teams`.
    async fn serve(fail: bool) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let h = h.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    h.fetch_add(1, Ordering::SeqCst);
                    let (status, body): (&str, &[u8]) = if fail {
                        ("500 Internal Server Error", b"{}")
                    } else if req.contains("dates=20261104") {
                        (
                            "200 OK",
                            include_bytes!("../tests/fixtures/nhl_scoreboard_20261104.json"),
                        )
                    } else if req.contains("dates=20261105") {
                        (
                            "200 OK",
                            include_bytes!("../tests/fixtures/nhl_scoreboard_20261105.json"),
                        )
                    } else if req.contains("/teams") {
                        ("200 OK", include_bytes!("../tests/fixtures/nhl_teams.json"))
                    } else {
                        ("404 Not Found", b"{}")
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(body).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://{addr}"), hits)
    }

    fn config(base: String) -> EspnClientConfig {
        EspnClientConfig {
            base_url: base,
            ..EspnClientConfig::default()
        }
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()
    }

    #[tokio::test]
    async fn lookup_matches_and_caches() {
        let (base, hits) = serve(false).await;
        let client = EspnClient::with_config(config(base)).unwrap();
        let query = MatchQuery {
            first_team: "Senators".into(),
            second_team: "Sabres".into(),
            separator: crate::Separator::At,
            parsed_start: None,
        };
        let m = client
            .lookup(League::Nhl, date(), &query)
            .await
            .unwrap()
            .expect("match");
        assert_eq!(m.home.abbreviation, "BUF");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "local date and next date");
        client.lookup(League::Nhl, date(), &query).await.unwrap();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "second lookup uses the cache"
        );
        let teams = client.teams(League::Nhl).await.unwrap();
        assert!(teams.iter().any(|t| t.abbreviation == "BUF"));
        client.teams(League::Nhl).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 3, "teams are cached");
    }

    #[tokio::test]
    async fn cache_expires() {
        let (base, hits) = serve(false).await;
        let client = EspnClient::with_config(EspnClientConfig {
            scoreboard_ttl: Duration::ZERO,
            ..config(base)
        })
        .unwrap();
        client.scoreboard(League::Nhl, date()).await.unwrap();
        client.scoreboard(League::Nhl, date()).await.unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn circuit_opens_after_failures() {
        let (base, hits) = serve(true).await;
        let client = EspnClient::with_config(config(base)).unwrap();
        for _ in 0..3 {
            assert!(matches!(
                client.scoreboard(League::Nhl, date()).await,
                Err(EspnError::Http(_))
            ));
        }
        assert!(matches!(
            client.scoreboard(League::Nhl, date()).await,
            Err(EspnError::CircuitOpen)
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    /// Live smoke test. Run with `cargo test -p iptv-sports -- --ignored`.
    #[tokio::test]
    #[ignore = "uses the public ESPN API"]
    async fn live_scoreboard_smoke() {
        let client = EspnClient::new().unwrap();
        let events = client.scoreboard(League::Nhl, date()).await.unwrap();
        assert!(!events.is_empty());
        assert!(!client.teams(League::Nhl).await.unwrap().is_empty());
    }
}
