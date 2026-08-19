use crate::{Diagnostic, DiagnosticCode, ParseError, ParseLimits, ParseStats};
use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use iptv_domain::{DatePattern, EventRule, EventRuleSet};
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::{BufRead, Read},
    str::FromStr,
};
use thiserror::Error;

#[derive(Clone, Debug, Default)]
pub struct EventRuleDocument {
    pub rules: EventRuleSet,
    pub diagnostics: Vec<Diagnostic>,
    pub stats: ParseStats,
}

#[derive(Clone, Debug)]
pub struct EventCandidate<'a> {
    pub name: &'a str,
    pub group: Option<&'a str>,
    pub epg_title: Option<&'a str>,
    pub categories: &'a [String],
    pub metadata: &'a BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct MatchedEvent<'a> {
    pub rule: &'a EventRule,
    pub starts_at: Option<DateTime<Utc>>,
    /// Named values extracted from the title, group, EPG title, and date regexes.
    pub captures: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeneratedProgrammeKind {
    Event,
    Filler,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedProgramme {
    pub starts_at: DateTime<Utc>,
    pub stops_at: DateTime<Utc>,
    pub title: String,
    pub kind: GeneratedProgrammeKind,
    /// Present only for event programmes so imports can update in place.
    pub stable_event_key: Option<String>,
    /// The unmodified provider title used to generate this interval.
    pub source_title: String,
}

#[derive(Debug)]
struct CompiledRule {
    rule: EventRule,
    name: Option<Regex>,
    group: Option<Regex>,
    epg_title: Option<Regex>,
    dates: Vec<CompiledDatePattern>,
}

#[derive(Debug)]
struct CompiledDatePattern {
    regex: Regex,
    definition: DatePattern,
}

#[derive(Debug)]
pub struct CompiledEventRules {
    rules: Vec<CompiledRule>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum EventDateError {
    #[error("event rule references unknown timezone {0:?}")]
    UnknownTimezone(String),
    #[error("captured event date {value:?} does not match format {format:?}")]
    InvalidDate { value: String, format: String },
    #[error("event date {value:?} is ambiguous in timezone {timezone}")]
    AmbiguousLocalTime { value: String, timezone: String },
    #[error("event date {value:?} does not exist in timezone {timezone}")]
    NonexistentLocalTime { value: String, timezone: String },
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum EventGuideError {
    #[error("event has no unambiguous start time")]
    MissingStart,
    #[error("guide window must end after it starts")]
    InvalidWindow,
    #[error("event duration must be greater than zero")]
    InvalidDuration,
    #[error("title template references unknown capture {0:?}")]
    UnknownTemplateField(String),
    #[error("event interval exceeds the supported date range")]
    DateOverflow,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EventRuleInput {
    Rules(Vec<EventRule>),
    Wrapped { rules: Vec<EventRule> },
}

/// Parses and validates an ordered JSON event-rule set without executing it.
///
/// # Errors
///
/// Returns [`ParseError`] for I/O failures, malformed JSON, invalid regex/timezone
/// definitions, or configured input, line, and record-limit violations.
#[allow(clippy::too_many_lines)]
pub fn parse_event_rules<R: BufRead>(
    mut reader: R,
    limits: ParseLimits,
) -> Result<EventRuleDocument, ParseError> {
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
    for (index, line) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if line.len() > limits.max_line_bytes {
            return Err(ParseError::LineTooLong {
                line: u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1),
                limit: limits.max_line_bytes,
            });
        }
    }
    let input: EventRuleInput = serde_json::from_slice(&bytes)
        .map_err(|error| ParseError::MalformedEventRules(error.to_string()))?;
    let rules = match input {
        EventRuleInput::Rules(rules) | EventRuleInput::Wrapped { rules } => rules,
    };
    if rules.len() > limits.max_records {
        return Err(ParseError::TooManyRecords {
            limit: limits.max_records,
        });
    }

    let mut document = EventRuleDocument {
        stats: ParseStats {
            bytes_read: byte_count,
            records_seen: u64::try_from(rules.len()).unwrap_or(u64::MAX),
            ..ParseStats::default()
        },
        ..EventRuleDocument::default()
    };
    let mut seen_names = std::collections::HashSet::new();
    let mut seen_ids = std::collections::HashSet::new();
    for rule in rules {
        let mut valid = true;
        if rule.name.trim().is_empty() {
            event_rule_diagnostic(&mut document, "event rule name must not be empty");
            valid = false;
        } else if !seen_names.insert(rule.name.clone()) {
            event_rule_diagnostic(
                &mut document,
                format!("duplicate event rule name {:?}", rule.name),
            );
            valid = false;
        }
        if !seen_ids.insert(rule.id) {
            event_rule_diagnostic(
                &mut document,
                format!("duplicate event rule id {}", rule.id),
            );
            valid = false;
        }
        if Tz::from_str(&rule.timezone).is_err() {
            event_rule_diagnostic(
                &mut document,
                format!(
                    "event rule {:?} has unknown timezone {:?}",
                    rule.name, rule.timezone
                ),
            );
            valid = false;
        }
        for (field, pattern) in [
            ("name_regex", rule.matcher.name_regex.as_deref()),
            ("group_regex", rule.matcher.group_regex.as_deref()),
            ("epg_title_regex", rule.matcher.epg_title_regex.as_deref()),
        ] {
            if let Some(pattern) = pattern
                && let Err(error) = Regex::new(pattern)
            {
                event_rule_diagnostic(
                    &mut document,
                    format!("event rule {:?} has invalid {field}: {error}", rule.name),
                );
                valid = false;
            }
        }
        for pattern in &rule.date_patterns {
            match Regex::new(&pattern.regex) {
                Ok(regex)
                    if regex
                        .capture_names()
                        .flatten()
                        .any(|name| name == "datetime")
                        || regex.captures_len() >= 2 => {}
                Ok(_) => {
                    event_rule_diagnostic(
                        &mut document,
                        format!(
                            "event rule {:?} date regex must contain a named 'datetime' or positional capture",
                            rule.name
                        ),
                    );
                    valid = false;
                }
                Err(error) => {
                    event_rule_diagnostic(
                        &mut document,
                        format!("event rule {:?} has invalid date regex: {error}", rule.name),
                    );
                    valid = false;
                }
            }
            if let Some(timezone) = &pattern.timezone
                && Tz::from_str(timezone).is_err()
            {
                event_rule_diagnostic(
                    &mut document,
                    format!(
                        "event rule {:?} has unknown date timezone {timezone:?}",
                        rule.name
                    ),
                );
                valid = false;
            }
        }
        if valid {
            document.rules.0.push(rule);
            document.stats.records_emitted += 1;
        } else {
            document.stats.records_skipped += 1;
        }
    }
    Ok(document)
}

fn event_rule_diagnostic(document: &mut EventRuleDocument, message: impl Into<String>) {
    let diagnostic = Diagnostic::error(DiagnosticCode::InvalidEventRule, None, None, message);
    document.stats.observe(&diagnostic);
    document.diagnostics.push(diagnostic);
}

impl CompiledEventRules {
    /// Compiles validated rules for repeated matching.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] if a caller supplies an unvalidated invalid regex.
    pub fn new(rules: EventRuleSet) -> Result<Self, ParseError> {
        let mut compiled = Vec::with_capacity(rules.len());
        for rule in rules.0 {
            let name = compile_optional(rule.matcher.name_regex.as_deref())?;
            let group = compile_optional(rule.matcher.group_regex.as_deref())?;
            let epg_title = compile_optional(rule.matcher.epg_title_regex.as_deref())?;
            let dates = rule
                .date_patterns
                .iter()
                .map(|definition| {
                    Regex::new(&definition.regex)
                        .map(|regex| CompiledDatePattern {
                            regex,
                            definition: definition.clone(),
                        })
                        .map_err(|error| ParseError::MalformedEventRules(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            compiled.push(CompiledRule {
                rule,
                name,
                group,
                epg_title,
                dates,
            });
        }
        Ok(Self { rules: compiled })
    }

    /// Returns the first enabled matching rule, preserving declaration order.
    ///
    /// # Errors
    ///
    /// Returns [`EventDateError`] when a matching rule extracts an invalid,
    /// nonexistent, or ambiguous local date/time. Such dates fail closed.
    pub fn first_match<'rules>(
        &'rules self,
        candidate: &EventCandidate<'_>,
    ) -> Result<Option<MatchedEvent<'rules>>, EventDateError> {
        for compiled in &self.rules {
            if !compiled.rule.enabled || !matches_rule(compiled, candidate) {
                continue;
            }
            let starts_at = parse_candidate_date(compiled, candidate)?;
            let captures = collect_captures(compiled, candidate);
            return Ok(Some(MatchedEvent {
                rule: &compiled.rule,
                starts_at,
                captures,
            }));
        }
        Ok(None)
    }
}

fn collect_captures(
    compiled: &CompiledRule,
    candidate: &EventCandidate<'_>,
) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    collect_regex_captures(compiled.name.as_ref(), candidate.name, &mut values);
    if let (Some(regex), Some(group)) = (compiled.group.as_ref(), candidate.group) {
        collect_regex_captures(Some(regex), group, &mut values);
    }
    if let (Some(regex), Some(title)) = (compiled.epg_title.as_ref(), candidate.epg_title) {
        collect_regex_captures(Some(regex), title, &mut values);
    }
    for date in &compiled.dates {
        for input in [Some(candidate.name), candidate.epg_title]
            .into_iter()
            .flatten()
        {
            collect_regex_captures(Some(&date.regex), input, &mut values);
        }
    }
    values
}

fn collect_regex_captures(
    regex: Option<&Regex>,
    input: &str,
    values: &mut BTreeMap<String, String>,
) {
    let Some(regex) = regex else {
        return;
    };
    let Some(captures) = regex.captures(input) else {
        return;
    };
    for name in regex.capture_names().flatten() {
        if let Some(value) = captures.name(name) {
            values
                .entry(name.to_owned())
                .or_insert_with(|| value.as_str().trim().to_owned());
        }
    }
}

/// Builds a gap-free guide window around a matched dynamic event.
///
/// The literal provider title is retained as provenance, while the rendered
/// title may reference `{source}` or any named regex capture. Intervals are
/// clipped to the requested window and are always non-overlapping.
///
/// # Errors
///
/// Returns [`EventGuideError`] when the match is unscheduled, configuration is
/// invalid, a template field is unknown, or date arithmetic overflows.
pub fn generate_event_guide(
    matched: &MatchedEvent<'_>,
    source_title: &str,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
) -> Result<Vec<GeneratedProgramme>, EventGuideError> {
    if window_end <= window_start {
        return Err(EventGuideError::InvalidWindow);
    }
    if matched.rule.guide.duration_seconds == 0 {
        return Err(EventGuideError::InvalidDuration);
    }
    let starts_at = matched.starts_at.ok_or(EventGuideError::MissingStart)?;
    let pre_roll = signed_seconds(matched.rule.lifecycle.pre_roll_seconds)?;
    let duration = signed_seconds(matched.rule.guide.duration_seconds)?;
    let post_roll = signed_seconds(matched.rule.lifecycle.post_roll_seconds)?;
    let event_start = starts_at
        .checked_sub_signed(pre_roll)
        .ok_or(EventGuideError::DateOverflow)?;
    let event_end = starts_at
        .checked_add_signed(duration)
        .and_then(|value| value.checked_add_signed(post_roll))
        .ok_or(EventGuideError::DateOverflow)?;
    let title = render_event_title(
        &matched.rule.guide.title_template,
        source_title,
        &matched.captures,
    )?;

    let mut programmes = Vec::with_capacity(3);
    if event_end <= window_start || event_start >= window_end {
        programmes.push(filler(
            window_start,
            window_end,
            &matched.rule.guide.filler_title,
            source_title,
        ));
        return Ok(programmes);
    }
    let clipped_start = event_start.max(window_start);
    let clipped_end = event_end.min(window_end);
    if clipped_start > window_start {
        programmes.push(filler(
            window_start,
            clipped_start,
            &matched.rule.guide.filler_title,
            source_title,
        ));
    }
    if clipped_end > clipped_start {
        programmes.push(GeneratedProgramme {
            starts_at: clipped_start,
            stops_at: clipped_end,
            title,
            kind: GeneratedProgrammeKind::Event,
            stable_event_key: Some(stable_event_key(matched, source_title, starts_at)),
            source_title: source_title.to_owned(),
        });
    }
    let filler_start = clipped_end.max(window_start);
    if filler_start < window_end {
        programmes.push(filler(
            filler_start,
            window_end,
            &matched.rule.guide.filler_title,
            source_title,
        ));
    }
    Ok(programmes)
}

fn signed_seconds(seconds: u64) -> Result<Duration, EventGuideError> {
    i64::try_from(seconds)
        .map(Duration::seconds)
        .map_err(|_| EventGuideError::DateOverflow)
}

fn filler(
    starts_at: DateTime<Utc>,
    stops_at: DateTime<Utc>,
    title: &str,
    source_title: &str,
) -> GeneratedProgramme {
    GeneratedProgramme {
        starts_at,
        stops_at,
        title: title.to_owned(),
        kind: GeneratedProgrammeKind::Filler,
        stable_event_key: None,
        source_title: source_title.to_owned(),
    }
}

fn render_event_title(
    template: &str,
    source_title: &str,
    captures: &BTreeMap<String, String>,
) -> Result<String, EventGuideError> {
    let mut rendered = String::with_capacity(template.len() + source_title.len());
    let mut remaining = template;
    while let Some(open) = remaining.find('{') {
        rendered.push_str(&remaining[..open]);
        let after_open = &remaining[open + 1..];
        let Some(close) = after_open.find('}') else {
            return Err(EventGuideError::UnknownTemplateField(after_open.to_owned()));
        };
        let field = &after_open[..close];
        let value = if matches!(field, "source" | "title") {
            source_title
        } else {
            captures
                .get(field)
                .map(String::as_str)
                .ok_or_else(|| EventGuideError::UnknownTemplateField(field.to_owned()))?
        };
        rendered.push_str(value);
        remaining = &after_open[close + 1..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

fn stable_event_key(
    matched: &MatchedEvent<'_>,
    source_title: &str,
    starts_at: DateTime<Utc>,
) -> String {
    let mut identity = ["league", "home", "away", "event"]
        .into_iter()
        .filter_map(|field| matched.captures.get(field))
        .map(|value| normalize_identity(value))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("|");
    if identity.is_empty() {
        identity = normalize_identity(source_title);
    }
    format!("{}|{}", identity, starts_at.to_rfc3339())
}

fn normalize_identity(value: &str) -> String {
    value
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn compile_optional(pattern: Option<&str>) -> Result<Option<Regex>, ParseError> {
    pattern
        .map(Regex::new)
        .transpose()
        .map_err(|error| ParseError::MalformedEventRules(error.to_string()))
}

fn matches_rule(compiled: &CompiledRule, candidate: &EventCandidate<'_>) -> bool {
    if compiled
        .name
        .as_ref()
        .is_some_and(|regex| !regex.is_match(candidate.name))
    {
        return false;
    }
    if compiled
        .group
        .as_ref()
        .is_some_and(|regex| !candidate.group.is_some_and(|value| regex.is_match(value)))
    {
        return false;
    }
    if compiled.epg_title.as_ref().is_some_and(|regex| {
        !candidate
            .epg_title
            .is_some_and(|value| regex.is_match(value))
    }) {
        return false;
    }
    if !compiled.rule.matcher.categories_any.is_empty()
        && !compiled.rule.matcher.categories_any.iter().any(|expected| {
            candidate
                .categories
                .iter()
                .any(|actual| actual.eq_ignore_ascii_case(expected))
        })
    {
        return false;
    }
    compiled
        .rule
        .matcher
        .metadata
        .iter()
        .all(|(key, expected)| candidate.metadata.get(key) == Some(expected))
}

fn parse_candidate_date(
    compiled: &CompiledRule,
    candidate: &EventCandidate<'_>,
) -> Result<Option<DateTime<Utc>>, EventDateError> {
    for pattern in &compiled.dates {
        for input in [Some(candidate.name), candidate.epg_title]
            .into_iter()
            .flatten()
        {
            let Some(captures) = pattern.regex.captures(input) else {
                continue;
            };
            let Some(value) = captures
                .name("datetime")
                .or_else(|| captures.get(1))
                .map(|capture| capture.as_str().trim())
            else {
                continue;
            };
            let timezone = pattern
                .definition
                .timezone
                .as_deref()
                .unwrap_or(&compiled.rule.timezone);
            return parse_event_local_datetime(value, &pattern.definition.format, timezone)
                .map(Some);
        }
    }
    Ok(None)
}

fn parse_event_local_datetime(
    value: &str,
    format: &str,
    timezone: &str,
) -> Result<DateTime<Utc>, EventDateError> {
    if let Ok(datetime) = DateTime::parse_from_str(value, format) {
        return Ok(datetime.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(value, format).or_else(|_| {
        NaiveDate::parse_from_str(value, format).map(|date| date.and_hms_opt(0, 0, 0).unwrap())
    });
    let naive = naive.map_err(|_| EventDateError::InvalidDate {
        value: value.to_owned(),
        format: format.to_owned(),
    })?;
    let timezone =
        Tz::from_str(timezone).map_err(|_| EventDateError::UnknownTimezone(timezone.to_owned()))?;
    match timezone.from_local_datetime(&naive) {
        LocalResult::Single(value) => Ok(value.with_timezone(&Utc)),
        LocalResult::Ambiguous(_, _) => Err(EventDateError::AmbiguousLocalTime {
            value: value.to_owned(),
            timezone: timezone.name().to_owned(),
        }),
        LocalResult::None => Err(EventDateError::NonexistentLocalTime {
            value: value.to_owned(),
            timezone: timezone.name().to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iptv_domain::builtin_sports_event_rules;
    use iptv_domain::{EventIdentityPolicy, EventRuleId};
    use std::{io::Cursor, sync::LazyLock};

    const FIXTURE: &str = include_str!("../../../tests/fixtures/events/sports-rules.json");
    static EMPTY_METADATA: LazyLock<BTreeMap<String, String>> = LazyLock::new(BTreeMap::new);

    #[test]
    fn parses_valid_rules_and_applies_conservative_defaults() {
        let document =
            parse_event_rules(Cursor::new(FIXTURE), ParseLimits::default()).expect("parse");
        assert_eq!(document.rules.len(), 2);
        assert_eq!(document.stats.records_emitted, 2);
        assert_eq!(
            document.rules.0[0].channel.identity,
            EventIdentityPolicy::ChannelUuid
        );
        assert_eq!(document.rules.0[0].lifecycle.archive_after_seconds, 86_400);
    }

    #[test]
    fn first_matching_rule_wins_and_extracts_timezone_safe_date() {
        let document =
            parse_event_rules(Cursor::new(FIXTURE), ParseLimits::default()).expect("parse");
        let compiled = CompiledEventRules::new(document.rules).expect("compile");
        let categories = vec!["Football".to_owned()];
        let metadata = BTreeMap::new();
        let candidate = EventCandidate {
            name: "NFL: Broncos vs Chiefs 2026-09-13 18:20",
            group: Some("NFL Sunday Ticket"),
            epg_title: None,
            categories: &categories,
            metadata: &metadata,
        };
        let matched = compiled
            .first_match(&candidate)
            .expect("unambiguous")
            .expect("match");
        assert_eq!(matched.rule.name, "NFL live events");
        assert_eq!(
            matched.starts_at.expect("date").to_rfc3339(),
            "2026-09-14T00:20:00+00:00"
        );
    }

    #[test]
    fn invalid_regex_is_reported_and_rule_is_skipped() {
        let input = r#"[{"name":"bad","matcher":{"name_regex":"("}}]"#;
        let document =
            parse_event_rules(Cursor::new(input), ParseLimits::default()).expect("parse document");
        assert!(document.rules.is_empty());
        assert_eq!(document.stats.records_skipped, 1);
        assert_eq!(
            document.diagnostics[0].code,
            DiagnosticCode::InvalidEventRule
        );
    }

    #[test]
    fn daylight_saving_ambiguity_fails_closed() {
        let error =
            parse_event_local_datetime("2026-11-01 01:30", "%Y-%m-%d %H:%M", "America/Denver")
                .expect_err("ambiguous");
        assert!(matches!(error, EventDateError::AmbiguousLocalTime { .. }));
    }

    #[test]
    fn enforces_rule_line_and_record_limits() {
        let one_line = r#"[{"name":"a"},{"name":"b"}]"#;
        let line_limits = ParseLimits {
            max_line_bytes: 12,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_event_rules(Cursor::new(one_line), line_limits),
            Err(ParseError::LineTooLong { line: 1, limit: 12 })
        ));

        let record_limits = ParseLimits {
            max_records: 1,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_event_rules(Cursor::new(one_line), record_limits),
            Err(ParseError::TooManyRecords { limit: 1 })
        ));
    }

    #[test]
    fn metadata_matchers_fail_closed_when_evidence_is_absent() {
        let input = r#"[{"name":"scoped","matcher":{"metadata":{"country":"US"}}}]"#;
        let document =
            parse_event_rules(Cursor::new(input), ParseLimits::default()).expect("parse");
        let compiled = CompiledEventRules::new(document.rules).expect("compile");
        let categories = Vec::new();
        let metadata = BTreeMap::new();
        let candidate = EventCandidate {
            name: "Some game",
            group: None,
            epg_title: None,
            categories: &categories,
            metadata: &metadata,
        };
        assert!(
            compiled
                .first_match(&candidate)
                .expect("evaluate")
                .is_none()
        );
    }

    #[test]
    fn rejects_all_invalid_rule_identity_timezone_and_date_definitions() {
        let duplicate_id = EventRuleId::new();
        let input = serde_json::json!({
            "rules": [
                {"id": duplicate_id, "name": "duplicate"},
                {"id": duplicate_id, "name": "duplicate"},
                {"name": "", "timezone": "Mars/Olympus"},
                {
                    "name": "no capture",
                    "date_patterns": [{"regex": "[0-9]+", "format": "%Y"}]
                },
                {
                    "name": "bad date regex",
                    "date_patterns": [{"regex": "(", "format": "%Y"}]
                },
                {
                    "name": "bad date timezone",
                    "date_patterns": [{
                        "regex": "(?P<datetime>[0-9]{4})",
                        "format": "%Y",
                        "timezone": "Invalid/Timezone"
                    }]
                }
            ]
        })
        .to_string();
        let document = parse_event_rules(Cursor::new(input), ParseLimits::default())
            .expect("invalid records are diagnosed");
        assert_eq!(document.stats.records_seen, 6);
        assert_eq!(document.stats.records_emitted, 1);
        assert_eq!(document.stats.records_skipped, 5);
        assert!(document.diagnostics.len() >= 7);
    }

    #[test]
    fn rejects_oversized_rule_document() {
        let limits = ParseLimits {
            max_input_bytes: 2,
            ..ParseLimits::default()
        };
        assert!(matches!(
            parse_event_rules(Cursor::new("[]\n"), limits),
            Err(ParseError::InputTooLarge { limit: 2 })
        ));
    }

    fn scheduled_match(title_template: &str) -> (CompiledEventRules, EventCandidate<'static>) {
        let input = serde_json::json!([{
            "name": "sports",
            "matcher": {
                "name_regex": "(?i)^(?P<league>NFL): (?P<home>.+) vs (?P<away>.+?) 20[0-9]{2}-"
            },
            "date_patterns": [{
                "regex": "(?P<datetime>20[0-9]{2}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2})",
                "format": "%Y-%m-%d %H:%M",
                "timezone": "UTC"
            }],
            "lifecycle": {"pre_roll_seconds": 300, "post_roll_seconds": 600},
            "guide": {
                "duration_seconds": 7200,
                "filler_title": "Stand by",
                "title_template": title_template
            }
        }])
        .to_string();
        let document = parse_event_rules(Cursor::new(input), ParseLimits::default()).unwrap();
        let compiled = CompiledEventRules::new(document.rules).unwrap();
        let candidate = EventCandidate {
            name: "NFL: Broncos vs Chiefs 2026-09-14 00:20",
            group: None,
            epg_title: None,
            categories: &[],
            metadata: &EMPTY_METADATA,
        };
        (compiled, candidate)
    }

    #[test]
    fn generated_event_guide_is_contiguous_stable_and_preserves_source_title() {
        let (compiled, candidate) = scheduled_match("{league}: {home} vs {away}");
        let matched = compiled.first_match(&candidate).unwrap().unwrap();
        assert_eq!(matched.captures.get("league").unwrap(), "NFL");
        assert_eq!(matched.captures.get("home").unwrap(), "Broncos");
        assert_eq!(matched.captures.get("away").unwrap(), "Chiefs");

        let window_start = "2026-09-14T00:00:00Z".parse().unwrap();
        let window_end = "2026-09-14T03:00:00Z".parse().unwrap();
        let programmes =
            generate_event_guide(&matched, candidate.name, window_start, window_end).unwrap();
        assert_eq!(programmes.len(), 3);
        assert_eq!(programmes[0].kind, GeneratedProgrammeKind::Filler);
        assert_eq!(programmes[0].title, "Stand by");
        assert_eq!(programmes[0].stops_at, programmes[1].starts_at);
        assert_eq!(programmes[1].title, "NFL: Broncos vs Chiefs");
        assert_eq!(
            programmes[1].starts_at.to_rfc3339(),
            "2026-09-14T00:15:00+00:00"
        );
        assert_eq!(
            programmes[1].stops_at.to_rfc3339(),
            "2026-09-14T02:30:00+00:00"
        );
        assert_eq!(programmes[1].stops_at, programmes[2].starts_at);
        assert_eq!(programmes[2].stops_at, window_end);
        assert_eq!(programmes[1].source_title, candidate.name);
        assert_eq!(
            programmes[1].stable_event_key.as_deref(),
            Some("nfl|broncos|chiefs|2026-09-14T00:20:00+00:00")
        );
    }

    #[test]
    fn builtin_sports_rules_strip_prefix_and_quality_and_render_both_separators() {
        let mut rules = builtin_sports_event_rules();
        for rule in &mut rules.0 {
            rule.timezone = "America/Denver".to_owned();
        }
        let compiled = CompiledEventRules::new(rules).expect("compile built-ins");
        let categories = Vec::new();
        let metadata = BTreeMap::new();

        for (source, expected_title, expected_start) in [
            (
                "EVENT 12: Broncos vs Chiefs 2026-09-13 18:20 FHD",
                "Broncos vs Chiefs",
                "2026-09-14T00:20:00+00:00",
            ),
            (
                "[VIP] Yankees @ Rockies 08/19/2026 07:10 PM HD",
                "Yankees @ Rockies",
                "2026-08-20T01:10:00+00:00",
            ),
        ] {
            let candidate = EventCandidate {
                name: source,
                group: None,
                epg_title: None,
                categories: &categories,
                metadata: &metadata,
            };
            let matched = compiled
                .first_match(&candidate)
                .expect("evaluate")
                .expect("built-in match");
            assert_eq!(
                matched.starts_at.expect("schedule").to_rfc3339(),
                expected_start
            );
            let start = matched.starts_at.expect("schedule") - Duration::minutes(30);
            let end = matched.starts_at.expect("schedule") + Duration::hours(4);
            let programmes =
                generate_event_guide(&matched, source, start, end).expect("render guide");
            let event = programmes
                .iter()
                .find(|item| item.kind == GeneratedProgrammeKind::Event)
                .expect("event programme");
            assert_eq!(event.title, expected_title);
        }
    }

    #[test]
    fn generated_guide_clips_event_and_fills_an_outside_window() {
        let (compiled, candidate) = scheduled_match("{source}");
        let matched = compiled.first_match(&candidate).unwrap().unwrap();
        let clipped = generate_event_guide(
            &matched,
            candidate.name,
            "2026-09-14T00:16:00Z".parse().unwrap(),
            "2026-09-14T00:30:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(clipped.len(), 1);
        assert_eq!(clipped[0].kind, GeneratedProgrammeKind::Event);

        let outside = generate_event_guide(
            &matched,
            candidate.name,
            "2026-09-15T00:00:00Z".parse().unwrap(),
            "2026-09-15T01:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(outside.len(), 1);
        assert_eq!(outside[0].kind, GeneratedProgrammeKind::Filler);
    }

    #[test]
    fn generated_guide_rejects_missing_schedule_invalid_window_and_unknown_template() {
        let (compiled, candidate) = scheduled_match("{unknown}");
        let mut matched = compiled.first_match(&candidate).unwrap().unwrap();
        let start = "2026-09-14T00:00:00Z".parse().unwrap();
        let end = "2026-09-14T01:00:00Z".parse().unwrap();
        assert!(matches!(
            generate_event_guide(&matched, candidate.name, end, start),
            Err(EventGuideError::InvalidWindow)
        ));
        assert!(matches!(
            generate_event_guide(&matched, candidate.name, start, end),
            Err(EventGuideError::UnknownTemplateField(field)) if field == "unknown"
        ));
        matched.starts_at = None;
        assert!(matches!(
            generate_event_guide(&matched, candidate.name, start, end),
            Err(EventGuideError::MissingStart)
        ));
    }
}
