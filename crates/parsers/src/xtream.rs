use crate::{Diagnostic, DiagnosticCode, ParseError, ParseLimits, ParseStats};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
};
use chrono::{DateTime, LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{Map, Value};
use std::{collections::BTreeMap, io::Read};
use url::Url;

#[derive(Clone, Debug)]
pub struct XtreamDocument<T> {
    pub records: Vec<T>,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: ParseStats,
}

impl<T> Default for XtreamDocument<T> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            diagnostics: Vec::new(),
            stats: ParseStats::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct XtreamAuth {
    pub authenticated: bool,
    pub status: Option<String>,
    pub expiration: Option<DateTime<Utc>>,
    pub active_connections: u32,
    pub max_connections: Option<u32>,
    pub allowed_output_formats: Vec<String>,
    pub server_url: Option<Url>,
    pub server_timezone: Option<String>,
    pub server_timestamp: Option<DateTime<Utc>>,
    /// Unknown non-secret scalar user/server fields.
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct XtreamCategory {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct XtreamLiveStream {
    pub stream_id: u64,
    pub name: String,
    pub category_id: Option<String>,
    pub epg_channel_id: Option<String>,
    pub icon: Option<Url>,
    pub channel_number: Option<f64>,
    pub stream_type: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
    pub is_adult: bool,
    pub has_archive: bool,
    pub archive_duration_days: u32,
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct XtreamShortEpgEntry {
    pub id: Option<String>,
    pub epg_id: Option<String>,
    pub title: String,
    pub description: String,
    pub language: Option<String>,
    pub start: DateTime<Utc>,
    pub stop: DateTime<Utc>,
    pub channel_id: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

/// Parses an Xtream `player_api.php` authentication response without retaining credentials.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed JSON, I/O failures, or input/text limits.
pub fn parse_xtream_auth<R: Read>(
    reader: R,
    limits: ParseLimits,
) -> Result<XtreamDocument<XtreamAuth>, ParseError> {
    let (root, byte_count) = read_json(reader, limits)?;
    let object = root
        .as_object()
        .ok_or_else(|| ParseError::MalformedXtream("auth response must be an object".into()))?;
    let user = object
        .get("user_info")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ParseError::MalformedXtream("auth response has no user_info object".into())
        })?;
    let server = object.get("server_info").and_then(Value::as_object);
    let mut document = XtreamDocument {
        stats: ParseStats {
            bytes_read: byte_count,
            records_seen: 1,
            ..ParseStats::default()
        },
        ..XtreamDocument::default()
    };
    let expiration = optional_unix_time(user.get("exp_date"), "exp_date", &mut document);
    let server_timestamp = server.and_then(|value| {
        optional_unix_time(
            value
                .get("timestamp_now")
                .or_else(|| value.get("server_timestamp")),
            "server timestamp",
            &mut document,
        )
    });
    let server_url = server.and_then(build_server_url);
    let mut metadata = BTreeMap::new();
    for (prefix, values) in [("user", Some(user)), ("server", server)] {
        if let Some(values) = values {
            for (key, value) in values {
                if is_sensitive_key(key) || known_auth_key(key) {
                    continue;
                }
                if let Some(value) = scalar_string(value)
                    && value.len() <= limits.max_text_bytes
                {
                    metadata.insert(format!("{prefix}.{key}"), value);
                }
            }
        }
    }
    let auth = XtreamAuth {
        authenticated: boolish(user.get("auth")).unwrap_or(false),
        status: optional_bounded_string(user.get("status"), limits.max_text_bytes)?,
        expiration,
        active_connections: u32ish(user.get("active_cons")).unwrap_or(0),
        max_connections: u32ish(user.get("max_connections")),
        allowed_output_formats: user
            .get("allowed_output_formats")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(scalar_string)
                    .filter(|value| value.len() <= limits.max_text_bytes)
                    .collect()
            })
            .unwrap_or_default(),
        server_url,
        server_timezone: server
            .and_then(|value| value.get("timezone"))
            .and_then(scalar_string),
        server_timestamp,
        metadata,
    };
    document.records.push(auth);
    document.stats.records_emitted = 1;
    Ok(document)
}

/// Parses the live-category array returned by `action=get_live_categories`.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed JSON, I/O failures, or configured limits.
pub fn parse_xtream_live_categories<R: Read>(
    reader: R,
    limits: ParseLimits,
) -> Result<XtreamDocument<XtreamCategory>, ParseError> {
    let (root, bytes) = read_json(reader, limits)?;
    let values = root.as_array().ok_or_else(|| {
        ParseError::MalformedXtream("live categories response must be an array".into())
    })?;
    ensure_record_limit(values.len(), limits.max_records)?;
    let mut document = new_document(bytes, values.len());
    for value in values {
        let Some(object) = value.as_object() else {
            invalid_record(&mut document, "live category is not an object");
            continue;
        };
        let Some(id) = object.get("category_id").and_then(scalar_string) else {
            invalid_record(&mut document, "live category is missing category_id");
            continue;
        };
        let Some(name) = object.get("category_name").and_then(scalar_string) else {
            invalid_record(&mut document, "live category is missing category_name");
            continue;
        };
        if id.len() > limits.max_text_bytes || name.len() > limits.max_text_bytes {
            return Err(ParseError::TextTooLarge {
                limit: limits.max_text_bytes,
            });
        }
        document.records.push(XtreamCategory {
            id,
            name,
            parent_id: object.get("parent_id").and_then(scalar_string),
            metadata: unknown_fields(object, &["category_id", "category_name", "parent_id"]),
        });
        document.stats.records_emitted += 1;
    }
    Ok(document)
}

/// Parses the live-stream array returned by `action=get_live_streams`.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed JSON, I/O failures, or configured limits.
pub fn parse_xtream_live_streams<R: Read>(
    reader: R,
    limits: ParseLimits,
) -> Result<XtreamDocument<XtreamLiveStream>, ParseError> {
    let (root, bytes) = read_json(reader, limits)?;
    let values = root.as_array().ok_or_else(|| {
        ParseError::MalformedXtream("live streams response must be an array".into())
    })?;
    ensure_record_limit(values.len(), limits.max_records)?;
    let mut document = new_document(bytes, values.len());
    for value in values {
        let Some(object) = value.as_object() else {
            invalid_record(&mut document, "live stream is not an object");
            continue;
        };
        let Some(stream_id) = u64ish(object.get("stream_id")) else {
            invalid_record(&mut document, "live stream is missing numeric stream_id");
            continue;
        };
        let Some(name) = object.get("name").and_then(scalar_string) else {
            invalid_record(&mut document, "live stream is missing name");
            continue;
        };
        if name.len() > limits.max_text_bytes {
            return Err(ParseError::TextTooLarge {
                limit: limits.max_text_bytes,
            });
        }
        let icon = object
            .get("stream_icon")
            .and_then(scalar_string)
            .filter(|value| !value.is_empty())
            .and_then(|value| match Url::parse(&value) {
                Ok(url) => Some(url),
                Err(error) => {
                    push_warning(
                        &mut document,
                        format!("stream {stream_id} has invalid icon URL: {error}"),
                    );
                    None
                }
            });
        let stream = XtreamLiveStream {
            stream_id,
            name,
            category_id: object.get("category_id").and_then(scalar_string),
            epg_channel_id: object.get("epg_channel_id").and_then(scalar_string),
            icon,
            channel_number: f64ish(object.get("num")),
            stream_type: object.get("stream_type").and_then(scalar_string),
            added_at: optional_unix_time(object.get("added"), "added", &mut document),
            is_adult: boolish(object.get("is_adult")).unwrap_or(false),
            has_archive: boolish(object.get("tv_archive")).unwrap_or(false),
            archive_duration_days: u32ish(object.get("tv_archive_duration")).unwrap_or(0),
            metadata: unknown_fields(
                object,
                &[
                    "stream_id",
                    "name",
                    "category_id",
                    "epg_channel_id",
                    "stream_icon",
                    "num",
                    "stream_type",
                    "added",
                    "is_adult",
                    "tv_archive",
                    "tv_archive_duration",
                ],
            ),
        };
        document.records.push(stream);
        document.stats.records_emitted += 1;
    }
    Ok(document)
}

/// Parses the object returned by `action=get_short_epg`, excluding VOD/series data.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed JSON, I/O failures, or configured limits.
pub fn parse_xtream_short_epg<R: Read>(
    reader: R,
    limits: ParseLimits,
) -> Result<XtreamDocument<XtreamShortEpgEntry>, ParseError> {
    parse_xtream_short_epg_in_timezone(reader, limits, "UTC")
}

/// Parses a short EPG response with a timezone for local provider timestamps.
///
/// Explicit timestamp offsets remain authoritative. The source timezone applies
/// only to timestamps that do not have an offset.
///
/// # Errors
///
/// Returns [`ParseError`] for malformed JSON, I/O failures, configured limits,
/// or an invalid source timezone.
pub fn parse_xtream_short_epg_in_timezone<R: Read>(
    reader: R,
    limits: ParseLimits,
    source_timezone: &str,
) -> Result<XtreamDocument<XtreamShortEpgEntry>, ParseError> {
    let source_timezone = source_timezone.parse::<Tz>().map_err(|_| {
        ParseError::MalformedXtream("short EPG source timezone is invalid".to_owned())
    })?;
    let (root, bytes) = read_json(reader, limits)?;
    let values = root
        .get("epg_listings")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ParseError::MalformedXtream("short EPG response has no epg_listings array".into())
        })?;
    ensure_record_limit(values.len(), limits.max_records)?;
    let mut document = new_document(bytes, values.len());
    for value in values {
        let Some(object) = value.as_object() else {
            invalid_record(&mut document, "short EPG listing is not an object");
            continue;
        };
        let Some(start) =
            parse_listing_time(object, &["start_timestamp", "start"], source_timezone)
        else {
            invalid_record(&mut document, "short EPG listing has no valid start");
            continue;
        };
        let Some(stop) = parse_listing_time(
            object,
            &["stop_timestamp", "end_timestamp", "end"],
            source_timezone,
        ) else {
            invalid_record(&mut document, "short EPG listing has no valid stop");
            continue;
        };
        if stop <= start {
            invalid_record(&mut document, "short EPG stop must be after start");
            continue;
        }
        let title = decoded_text(object.get("title"), limits.max_text_bytes)?;
        let description = decoded_text(object.get("description"), limits.max_text_bytes)?;
        document.records.push(XtreamShortEpgEntry {
            id: object.get("id").and_then(scalar_string),
            epg_id: object.get("epg_id").and_then(scalar_string),
            title,
            description,
            language: object.get("lang").and_then(scalar_string),
            start,
            stop,
            channel_id: object
                .get("channel_id")
                .or_else(|| object.get("stream_id"))
                .and_then(scalar_string),
            metadata: unknown_fields(
                object,
                &[
                    "id",
                    "epg_id",
                    "title",
                    "description",
                    "lang",
                    "start",
                    "end",
                    "start_timestamp",
                    "stop_timestamp",
                    "end_timestamp",
                    "channel_id",
                    "stream_id",
                ],
            ),
        });
        document.stats.records_emitted += 1;
    }
    Ok(document)
}

fn read_json<R: Read>(mut reader: R, limits: ParseLimits) -> Result<(Value, u64), ParseError> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limits.max_input_bytes + 1)
        .read_to_end(&mut bytes)?;
    let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if byte_count > limits.max_input_bytes {
        return Err(ParseError::InputTooLarge {
            limit: limits.max_input_bytes,
        });
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| ParseError::MalformedXtream(error.to_string()))?;
    Ok((value, byte_count))
}

fn ensure_record_limit(count: usize, maximum: usize) -> Result<(), ParseError> {
    if count > maximum {
        Err(ParseError::TooManyRecords { limit: maximum })
    } else {
        Ok(())
    }
}

fn new_document<T>(bytes: u64, records: usize) -> XtreamDocument<T> {
    XtreamDocument {
        stats: ParseStats {
            bytes_read: bytes,
            records_seen: u64::try_from(records).unwrap_or(u64::MAX),
            ..ParseStats::default()
        },
        ..XtreamDocument::default()
    }
}

fn invalid_record<T>(document: &mut XtreamDocument<T>, message: impl Into<String>) {
    document.stats.records_skipped += 1;
    let diagnostic = Diagnostic::error(DiagnosticCode::InvalidXtreamRecord, None, None, message);
    document.stats.observe(&diagnostic);
    document.diagnostics.push(diagnostic);
}

fn push_warning<T>(document: &mut XtreamDocument<T>, message: impl Into<String>) {
    let diagnostic = Diagnostic::warning(DiagnosticCode::InvalidXtreamRecord, None, None, message);
    document.stats.observe(&diagnostic);
    document.diagnostics.push(diagnostic);
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn optional_bounded_string(
    value: Option<&Value>,
    maximum: usize,
) -> Result<Option<String>, ParseError> {
    let value = value.and_then(scalar_string);
    if value.as_ref().is_some_and(|value| value.len() > maximum) {
        return Err(ParseError::TextTooLarge { limit: maximum });
    }
    Ok(value)
}

fn u64ish(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    })
}

fn u32ish(value: Option<&Value>) -> Option<u32> {
    u64ish(value).and_then(|value| value.try_into().ok())
}

fn f64ish(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|value| match value {
            Value::Number(value) => value.as_f64(),
            Value::String(value) => value.parse().ok(),
            _ => None,
        })
        .filter(|value| value.is_finite())
}

fn boolish(value: Option<&Value>) -> Option<bool> {
    value.and_then(|value| match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_i64().map(|value| value != 0),
        Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "active" => Some(true),
            "0" | "false" | "no" | "disabled" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

fn optional_unix_time<T>(
    value: Option<&Value>,
    field: &str,
    document: &mut XtreamDocument<T>,
) -> Option<DateTime<Utc>> {
    let seconds = value.and_then(|value| match value {
        Value::Number(value) => value.as_i64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    })?;
    if let Some(value) = Utc.timestamp_opt(seconds, 0).single() {
        Some(value)
    } else {
        push_warning(document, format!("invalid {field} Unix timestamp"));
        None
    }
}

fn build_server_url(server: &Map<String, Value>) -> Option<Url> {
    let host = server.get("url").and_then(scalar_string)?;
    if let Ok(url) = Url::parse(&host) {
        return Some(url);
    }
    let scheme = server
        .get("server_protocol")
        .and_then(scalar_string)
        .unwrap_or_else(|| "http".to_owned());
    let port_key = if scheme.eq_ignore_ascii_case("https") {
        "https_port"
    } else {
        "port"
    };
    let port = server.get(port_key).and_then(scalar_string);
    Url::parse(&port.map_or_else(
        || format!("{scheme}://{host}"),
        |port| format!("{scheme}://{host}:{port}"),
    ))
    .ok()
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["username", "password", "token", "secret", "credential"]
        .iter()
        .any(|needle| key.contains(needle))
}

fn known_auth_key(key: &str) -> bool {
    matches!(
        key,
        "auth"
            | "status"
            | "exp_date"
            | "active_cons"
            | "max_connections"
            | "allowed_output_formats"
            | "url"
            | "port"
            | "https_port"
            | "server_protocol"
            | "timezone"
            | "timestamp_now"
            | "server_timestamp"
    )
}

fn unknown_fields(object: &Map<String, Value>, known: &[&str]) -> BTreeMap<String, Value> {
    object
        .iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn parse_listing_time(
    object: &Map<String, Value>,
    keys: &[&str],
    source_timezone: Tz,
) -> Option<DateTime<Utc>> {
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if let Some(seconds) = value.as_i64().or_else(|| value.as_str()?.parse().ok())
            && let Some(datetime) = Utc.timestamp_opt(seconds, 0).single()
        {
            return Some(datetime);
        }
        let Some(value) = value.as_str() else {
            continue;
        };
        if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
            return Some(datetime.with_timezone(&Utc));
        }
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S") {
            return match source_timezone.from_local_datetime(&datetime) {
                LocalResult::Single(value) => Some(value.with_timezone(&Utc)),
                LocalResult::Ambiguous(_, _) | LocalResult::None => None,
            };
        }
    }
    None
}

fn decoded_text(value: Option<&Value>, maximum: usize) -> Result<String, ParseError> {
    let Some(raw) = value.and_then(scalar_string) else {
        return Ok(String::new());
    };
    if raw.len() > maximum.saturating_mul(2) {
        return Err(ParseError::TextTooLarge { limit: maximum });
    }
    let decoded = STANDARD
        .decode(&raw)
        .or_else(|_| STANDARD_NO_PAD.decode(&raw))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or(raw);
    if decoded.len() > maximum {
        return Err(ParseError::TextTooLarge { limit: maximum });
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const AUTH: &str = include_str!("../../../tests/fixtures/events/xtream-auth.json");
    const CATEGORIES: &str =
        include_str!("../../../tests/fixtures/events/xtream-live-categories.json");
    const STREAMS: &str = include_str!("../../../tests/fixtures/events/xtream-live-streams.json");
    const EPG: &str = include_str!("../../../tests/fixtures/events/xtream-short-epg.json");

    #[test]
    fn parses_auth_without_retaining_credentials() {
        let document = parse_xtream_auth(Cursor::new(AUTH), ParseLimits::default()).expect("auth");
        let auth = &document.records[0];
        assert!(auth.authenticated);
        assert_eq!(auth.max_connections, Some(3));
        assert_eq!(
            auth.server_url.as_ref().map(Url::as_str),
            Some("https://tv.example/")
        );
        assert!(
            !auth
                .metadata
                .keys()
                .any(|key| key.contains("password") || key.contains("username"))
        );
    }

    #[test]
    fn parses_unicode_categories_and_preserves_unknown_fields() {
        let document =
            parse_xtream_live_categories(Cursor::new(CATEGORIES), ParseLimits::default())
                .expect("categories");
        assert_eq!(document.records.len(), 2);
        assert_eq!(document.records[1].name, "Fútbol ⚽");
        assert_eq!(
            document.records[0].metadata.get("provider_tag"),
            Some(&Value::String("local".into()))
        );
    }

    #[test]
    fn parses_coerced_live_fields_and_skips_hostile_records() {
        let document = parse_xtream_live_streams(Cursor::new(STREAMS), ParseLimits::default())
            .expect("streams");
        assert_eq!(document.records.len(), 2);
        assert_eq!(document.stats.records_skipped, 2);
        assert_eq!(document.records[0].stream_id, 101);
        assert!(document.records[0].has_archive);
        assert_eq!(document.records[0].archive_duration_days, 7);
        assert_eq!(
            document.records[0].metadata.get("custom_sid"),
            Some(&Value::String("abc".into()))
        );
    }

    #[test]
    fn decodes_short_epg_base64_and_normalizes_time() {
        let document =
            parse_xtream_short_epg(Cursor::new(EPG), ParseLimits::default()).expect("epg");
        assert_eq!(document.records.len(), 2);
        assert_eq!(document.records[0].title, "Broncos vs Chiefs");
        assert_eq!(
            document.records[0].start.to_rfc3339(),
            "2026-09-14T00:20:00+00:00"
        );
        assert_eq!(document.records[1].title, "Plain title");
    }

    #[test]
    fn short_epg_uses_source_timezone_only_without_an_explicit_offset() {
        let payload = r#"{
            "epg_listings": [
                {
                    "title": "TG9jYWw=",
                    "start": "2026-01-15 12:00:00",
                    "end": "2026-01-15 13:00:00",
                    "channel_id": "one"
                },
                {
                    "title": "T2Zmc2V0",
                    "start": "2026-01-15T12:00:00+02:00",
                    "end": "2026-01-15T13:00:00+02:00",
                    "channel_id": "one"
                }
            ]
        }"#;
        let document = parse_xtream_short_epg_in_timezone(
            Cursor::new(payload),
            ParseLimits::default(),
            "America/Denver",
        )
        .expect("short EPG");

        assert_eq!(
            document.records[0].start.to_rfc3339(),
            "2026-01-15T19:00:00+00:00"
        );
        assert_eq!(
            document.records[1].start.to_rfc3339(),
            "2026-01-15T10:00:00+00:00"
        );
    }

    #[test]
    fn short_epg_skips_ambiguous_or_nonexistent_local_timestamps() {
        let payload = r#"{
            "epg_listings": [
                {
                    "title": "U2tpcA==",
                    "start": "2026-11-01 01:30:00",
                    "end": "2026-11-01 02:30:00",
                    "channel_id": "one"
                }
            ]
        }"#;
        let document = parse_xtream_short_epg_in_timezone(
            Cursor::new(payload),
            ParseLimits::default(),
            "America/Denver",
        )
        .expect("short EPG");

        assert!(document.records.is_empty());
        assert_eq!(document.stats.records_skipped, 1);
    }

    #[test]
    fn enforces_xtream_byte_record_and_text_limits() {
        let bytes = ParseLimits {
            max_input_bytes: 8,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_xtream_live_categories(Cursor::new(CATEGORIES), bytes),
            Err(ParseError::InputTooLarge { limit: 8 })
        ));
        let records = ParseLimits {
            max_records: 1,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_xtream_live_categories(Cursor::new(CATEGORIES), records),
            Err(ParseError::TooManyRecords { limit: 1 })
        ));
        let text = ParseLimits {
            max_text_bytes: 3,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_xtream_live_categories(Cursor::new(CATEGORIES), text),
            Err(ParseError::TextTooLarge { limit: 3 })
        ));
    }

    #[test]
    fn rejects_wrong_top_level_shapes_and_malformed_json() {
        assert!(matches!(
            parse_xtream_live_streams(Cursor::new("{}"), ParseLimits::default()),
            Err(ParseError::MalformedXtream(_))
        ));
        assert!(matches!(
            parse_xtream_auth(Cursor::new("{"), ParseLimits::default()),
            Err(ParseError::MalformedXtream(_))
        ));
    }
}
