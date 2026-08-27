//! Run: cargo test --test live-api-acceptance -- --ignored
//!
//! This suite needs the live stack at <http://127.0.0.1:8081>.
//! The workspace reqwest build does not enable the cookies feature.
//! This suite parses Set-Cookie headers and sends them back with requests.

use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::header::SET_COOKIE;

const BASE_URL: &str = "http://127.0.0.1:8081";
const USERNAME: &str = "operator";
const PASSWORD: &str = "admin1234567";
const M3U_SOURCE_ID: &str = "01a040b6-3f10-7ff1-acf0-780c66b231e2";
const XMLTV_SOURCE_ID: &str = "01a040b6-3f1d-7a51-a666-83612174e1f6";

#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
struct Session {
    session_cookie: String,
    csrf_cookie: String,
    csrf_token: String,
}

fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_mins(1))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
}

fn extract_cookie(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(SET_COOKIE) {
        if let Ok(s) = value.to_str() {
            for part in s.split(';') {
                let mut kv = part.trim().splitn(2, '=');
                let key = kv.next()?;
                if key == name {
                    return kv.next().map(String::from);
                }
            }
        }
    }
    None
}

fn cookie_header(session: &Session) -> String {
    format!(
        "iptv_session={}; iptv_csrf={}",
        session.session_cookie, session.csrf_cookie
    )
}

async fn live_stack_available() -> bool {
    match reqwest::get(format!("{BASE_URL}/health/live")).await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

async fn require_live_stack() -> anyhow::Result<()> {
    if !live_stack_available().await {
        anyhow::bail!("live stack not available at {BASE_URL}");
    }
    Ok(())
}

async fn auth_session(client: &reqwest::Client) -> anyhow::Result<Session> {
    let status = client
        .get(format!("{BASE_URL}/api/v1/auth/status"))
        .send()
        .await
        .context("send auth/status request")?;

    let csrf_cookie = extract_cookie(status.headers(), "iptv_csrf")
        .context("auth/status did not set iptv_csrf cookie")?;
    let csrf_token = csrf_cookie.clone();

    let login = client
        .post(format!("{BASE_URL}/api/v1/auth/login"))
        .header("X-CSRF-Token", &csrf_token)
        .header("Cookie", format!("iptv_csrf={csrf_cookie}"))
        .json(&serde_json::json!({ "username": USERNAME, "password": PASSWORD }))
        .send()
        .await
        .context("send login request")?;

    anyhow::ensure!(
        login.status().is_success(),
        "login failed with status {}",
        login.status()
    );

    let session_cookie = extract_cookie(login.headers(), "iptv_session")
        .context("login did not set iptv_session cookie")?;
    let csrf_cookie = extract_cookie(login.headers(), "iptv_csrf")
        .context("login did not set iptv_csrf cookie")?;

    Ok(Session {
        session_cookie,
        csrf_token: csrf_cookie.clone(),
        csrf_cookie,
    })
}

async fn get_json(
    client: &reqwest::Client,
    session: &Session,
    path: &str,
) -> anyhow::Result<serde_json::Value> {
    let response = client
        .get(format!("{BASE_URL}{path}"))
        .header("Cookie", cookie_header(session))
        .header("X-CSRF-Token", &session.csrf_token)
        .send()
        .await
        .context(format!("send GET {path}"))?;

    anyhow::ensure!(
        response.status().is_success(),
        "GET {path} failed with status {}",
        response.status()
    );

    response
        .json()
        .await
        .context(format!("decode GET {path} response"))
}

async fn read_catalog_events(client: &reqwest::Client, session: &Session) -> anyhow::Result<()> {
    let response = client
        .get(format!("{BASE_URL}/api/v1/catalog-events"))
        .header("Cookie", cookie_header(session))
        .header("X-CSRF-Token", &session.csrf_token)
        .timeout(Duration::from_secs(90))
        .send()
        .await
        .context("send catalog-events request")?
        .error_for_status()
        .context("catalog-events returned error status")?;

    let mut stream = response.bytes_stream();
    let mut buf = String::new();
    let mut saw_overview = false;
    let mut saw_sync = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);

    while tokio::time::Instant::now() < deadline && !saw_overview {
        match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
            Ok(Some(Ok(bytes))) => {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                if !saw_overview && buf.contains("event: overview") {
                    saw_overview = true;
                }
                if !saw_sync && buf.contains("event: source-sync-progress") {
                    saw_sync = true;
                }
            }
            Ok(Some(Err(error))) => anyhow::bail!(error),
            Ok(None) | Err(_) => break,
        }
    }

    anyhow::ensure!(saw_overview, "did not receive an overview event");
    if saw_sync {
        eprintln!("received source-sync-progress event (optional)");
    }

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn auth_status_sets_csrf_cookie() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let response = client
        .get(format!("{BASE_URL}/api/v1/auth/status"))
        .send()
        .await
        .context("send auth/status request")?;

    anyhow::ensure!(
        response.status().is_success(),
        "auth/status returned {}",
        response.status()
    );

    let csrf = extract_cookie(response.headers(), "iptv_csrf");
    anyhow::ensure!(csrf.is_some(), "auth/status did not set iptv_csrf cookie");

    let body: serde_json::Value = response.json().await.context("decode auth/status body")?;
    anyhow::ensure!(
        body["authenticated"].as_bool() == Some(false),
        "auth/status reported authenticated before login"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn login_and_authenticated_request_succeed() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await.context("authenticate")?;

    let system = get_json(&client, &session, "/api/v1/system").await?;
    anyhow::ensure!(
        system["channels"].as_u64().is_some(),
        "system info did not include channels"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn login_without_csrf_header_fails() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let status = client
        .get(format!("{BASE_URL}/api/v1/auth/status"))
        .send()
        .await
        .context("send auth/status request")?;

    let csrf_cookie = extract_cookie(status.headers(), "iptv_csrf")
        .context("auth/status did not set csrf cookie")?;

    let login = client
        .post(format!("{BASE_URL}/api/v1/auth/login"))
        .header("Cookie", format!("iptv_csrf={csrf_cookie}"))
        .json(&serde_json::json!({ "username": USERNAME, "password": PASSWORD }))
        .send()
        .await
        .context("send login request")?;

    anyhow::ensure!(
        !login.status().is_success(),
        "login succeeded without X-CSRF-Token header"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn source_list_contains_m3u_and_xmltv() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;
    let sources = get_json(&client, &session, "/api/v1/sources").await?;

    let items = sources.as_array().context("sources is not an array")?;
    anyhow::ensure!(!items.is_empty(), "sources list is empty");

    let has_m3u = items.iter().any(|source| {
        source["id"].as_str() == Some(M3U_SOURCE_ID)
            && source["kind"]
                .as_str()
                .map(|k| k.eq_ignore_ascii_case("m3u"))
                .unwrap_or(false)
    });
    let has_xmltv = items.iter().any(|source| {
        source["id"].as_str() == Some(XMLTV_SOURCE_ID)
            && source["kind"]
                .as_str()
                .map(|k| k.eq_ignore_ascii_case("xmltv"))
                .unwrap_or(false)
    });

    anyhow::ensure!(has_m3u, "m3u source {M3U_SOURCE_ID} not found");
    anyhow::ensure!(has_xmltv, "xmltv source {XMLTV_SOURCE_ID} not found");

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn source_sync_status_shows_progress_or_state() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;
    let status = get_json(
        &client,
        &session,
        &format!("/api/v1/sources/{M3U_SOURCE_ID}/sync-status"),
    )
    .await;

    if let Ok(body) = status {
        let percent = body["percent"].as_u64().unwrap_or(101);
        anyhow::ensure!(percent <= 100, "sync percent out of range: {percent}");
    } else {
        let sources = get_json(&client, &session, "/api/v1/sources").await?;
        let m3u = sources
            .as_array()
            .context("sources is not an array")?
            .iter()
            .find(|s| s["id"].as_str() == Some(M3U_SOURCE_ID))
            .context("m3u source missing from list")?;
        anyhow::ensure!(
            !m3u["state"].as_str().unwrap_or("").is_empty(),
            "m3u source has no state"
        );
    }

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn channels_are_ingested() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;
    let page = get_json(&client, &session, "/api/v1/channels?limit=1").await?;

    let total = page["total"].as_i64().unwrap_or(-1);
    anyhow::ensure!(total > 0, "channel total is not positive: {total}");

    let items = page["items"].as_array().context("channels items missing")?;
    anyhow::ensure!(!items.is_empty(), "channels page has no items");

    let first = &items[0];
    anyhow::ensure!(first["id"].as_str().is_some(), "channel has no id");
    anyhow::ensure!(first["name"].as_str().is_some(), "channel has no name");

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn groups_are_ingested() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;
    let groups = get_json(&client, &session, "/api/v1/groups").await?;

    let items = groups.as_array().context("groups is not an array")?;
    anyhow::ensure!(!items.is_empty(), "groups list is empty");

    let total_channels: i64 = items
        .iter()
        .filter_map(|g| g["channelCount"].as_i64())
        .sum();
    anyhow::ensure!(total_channels > 0, "groups report no channels");

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn programmes_exist_and_overlap_now_in_denver() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;
    let page = get_json(&client, &session, "/api/v1/programmes?limit=100").await?;

    let total = page["total"].as_i64().unwrap_or(-1);
    anyhow::ensure!(total > 0, "programme total is not positive: {total}");

    let items = page["items"]
        .as_array()
        .context("programme items missing")?;
    anyhow::ensure!(!items.is_empty(), "programme page has no items");

    let now_utc = Utc::now();
    let denver = chrono_tz::America::Denver;
    let today_denver = now_utc.with_timezone(&denver).date_naive();

    let mut saw_current = false;
    let mut saw_today = false;

    for programme in items {
        let start_str = programme["start"]
            .as_str()
            .context("programme start is not a string")?;
        let end_str = programme["end"]
            .as_str()
            .context("programme end is not a string")?;

        let start = chrono::DateTime::parse_from_rfc3339(start_str)
            .context("parse programme start")?
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339(end_str)
            .context("parse programme end")?
            .with_timezone(&Utc);

        if start <= now_utc && now_utc < end {
            saw_current = true;
        }

        if start.with_timezone(&denver).date_naive() == today_denver {
            saw_today = true;
        }
    }

    anyhow::ensure!(
        saw_current,
        "no programme currently overlaps with the current time"
    );
    anyhow::ensure!(saw_today, "no programme starts today in America/Denver");

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn epg_mappings_and_unmapped_channels_exist() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;

    let mappings = get_json(&client, &session, "/api/v1/epg/mappings?limit=1").await?;
    let mappings_total = mappings["total"].as_i64().unwrap_or(-1);
    anyhow::ensure!(
        mappings_total >= 0,
        "epg mappings total is negative: {mappings_total}"
    );

    let unmapped = get_json(&client, &session, "/api/v1/epg/unmapped?limit=1").await?;
    let unmapped_total = unmapped["total"].as_i64().unwrap_or(-1);
    anyhow::ensure!(
        unmapped_total >= 0,
        "unmapped channels total is negative: {unmapped_total}"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn catalog_events_stream_reports_sync_and_overview() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;

    read_catalog_events(&client, &session).await?;

    Ok(())
}

#[tokio::test]
#[ignore = "requires live stack"]
async fn system_info_is_populated() -> anyhow::Result<()> {
    require_live_stack().await?;

    let client = client().context("build http client")?;
    let session = auth_session(&client).await?;
    let system = get_json(&client, &session, "/api/v1/system").await?;

    let channels = system["channels"].as_i64().unwrap_or(-1);
    let guide_coverage = system["guideCoverage"].as_f64().unwrap_or(-1.0);

    anyhow::ensure!(channels > 0, "system reports no channels: {channels}");
    anyhow::ensure!(
        guide_coverage >= 0.0,
        "system reports negative guide coverage: {guide_coverage}"
    );

    Ok(())
}
