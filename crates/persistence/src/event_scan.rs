//! Pure parts of the dynamic event scan.
//!
//! The functions in this module do not use the database. `catalog.rs` calls
//! them from the scan engine, and the unit tests below examine them directly.

use std::{collections::BTreeMap, sync::LazyLock};

use chrono::{DateTime, Duration, DurationRound, Utc};
use chrono_tz::Tz;
use iptv_parsers::{GeneratedProgramme, GeneratedProgrammeKind};
use regex::Regex;
use serde::Serialize;
use uuid::Uuid;

/// ID of the built-in event template that migration `0077` inserts.
///
/// The built-in template applies the built-in sports rules to all enabled
/// channels. The operator can disable it, or change its settings.
pub const BUILTIN_EVENT_TEMPLATE_ID: Uuid =
    Uuid::from_u128(0x8b2d_571e_91de_7000_8000_0000_0000_e000);

/// Maximum number of skip records that one scan report keeps.
pub const MAX_EVENT_SCAN_SKIPS: usize = 200;

/// Text settings for the event filler programmes.
///
/// [`EventScanSettings::from_settings`] reads them from the operator settings.
#[derive(Clone, Debug)]
pub struct EventScanSettings {
    /// Prefix of the filler title before an event, for example `Upcoming`.
    pub upcoming_prefix: String,
    /// Filler title after an event.
    pub ended_title: String,
    /// Programme length in seconds when a template has no duration.
    pub inferred_duration_seconds: u64,
    /// ESPN client. `None` turns the sports enrichment off.
    pub sports: Option<iptv_sports::EspnClient>,
}

impl Default for EventScanSettings {
    fn default() -> Self {
        Self {
            upcoming_prefix: "Upcoming".to_owned(),
            ended_title: "Event ended".to_owned(),
            inferred_duration_seconds: DEFAULT_INFERRED_DURATION_SECONDS,
            sports: None,
        }
    }
}

/// Default of `events.inferred_duration_seconds` (three hours).
pub const DEFAULT_INFERRED_DURATION_SECONDS: u64 = 10_800;

impl EventScanSettings {
    /// Builds the scan settings from resolved operator settings.
    ///
    /// `events.inferred_duration_seconds` is the duration of a template that
    /// has none. An operator value of `events.filler_title` is the title of
    /// the filler programme after an event. When the operator did not set
    /// `events.filler_title`, the title stays `Event ended`.
    #[must_use]
    pub fn from_settings(settings: &[iptv_domain::EffectiveSetting]) -> Self {
        let defaults = Self::default();
        let find = |key: &str| {
            settings
                .iter()
                .find(|setting| setting.definition.key == key)
        };
        Self {
            ended_title: find("events.filler_title")
                .filter(|setting| {
                    setting.inherited_from != iptv_domain::InheritanceSource::SystemDefault
                })
                .and_then(|setting| setting.value.as_str())
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map_or(defaults.ended_title, str::to_owned),
            inferred_duration_seconds: find("events.inferred_duration_seconds")
                .and_then(|setting| setting.value.as_u64())
                .unwrap_or(defaults.inferred_duration_seconds),
            ..defaults
        }
    }
}

/// One channel that matched an event template but did not get a guide.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventScanSkip {
    /// Template that matched the stream name. `None` when no template applies.
    pub template_id: Option<Uuid>,
    /// Channel of the stream. `None` for a template-level reason.
    pub channel_id: Option<Uuid>,
    pub stream_name: String,
    pub reason: String,
}

/// Result of one event scan.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventScanReport {
    /// Enabled channels that the scan examined.
    pub channels_scanned: u64,
    /// Channels with a stream name that a template and a sports rule matched.
    pub matched: u64,
    /// Channels that received event programmes.
    pub scheduled: u64,
    /// Matched channels that received no programmes.
    pub skipped: u64,
    /// Event channels that the prune step changed to `hidden`.
    pub pruned: u64,
    /// Skip reasons. The list keeps at most [`MAX_EVENT_SCAN_SKIPS`] records.
    pub skips: Vec<EventScanSkip>,
}

impl EventScanReport {
    pub(crate) fn skip(
        &mut self,
        template_id: Option<Uuid>,
        channel_id: Option<Uuid>,
        stream_name: &str,
        reason: String,
    ) {
        if self.skips.len() < MAX_EVENT_SCAN_SKIPS {
            self.skips.push(EventScanSkip {
                template_id,
                channel_id,
                stream_name: stream_name.to_owned(),
                reason,
            });
        }
    }

    /// Returns the legacy count that `scan_event_channels` returns.
    #[must_use]
    pub fn legacy_count(&self) -> u64 {
        self.scheduled
    }
}

/// Lifecycle state of an event channel at an instant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventChannelState {
    Scheduled,
    Live,
    Ended,
    Hidden,
}

impl EventChannelState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Live => "live",
            Self::Ended => "ended",
            Self::Hidden => "hidden",
        }
    }

    /// Computes the state from the event interval, the grace, and `now`.
    #[must_use]
    pub fn at(
        starts_at: DateTime<Utc>,
        stops_at: DateTime<Utc>,
        grace: Duration,
        now: DateTime<Utc>,
    ) -> Self {
        if now < starts_at {
            Self::Scheduled
        } else if now < stops_at {
            Self::Live
        } else if stops_at
            .checked_add_signed(grace)
            .is_some_and(|hidden_at| now < hidden_at)
        {
            Self::Ended
        } else {
            Self::Hidden
        }
    }
}

/// Compiles a template `matchRegex` with the Rust `regex` crate.
///
/// `matchRegex` uses the Rust regex syntax. The function also accepts the
/// `PostgreSQL` word-boundary escapes `\y`, `\m`, and `\M`, and changes each one
/// to `\b`. Migration `0077` makes the same change to stored templates.
///
/// # Errors
///
/// Returns the regex error when the pattern is not valid.
pub fn compile_template_regex(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&postgres_word_boundaries_to_rust(pattern))
}

fn postgres_word_boundaries_to_rust(pattern: &str) -> String {
    let mut output = String::with_capacity(pattern.len());
    let mut characters = pattern.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next() {
            Some('y' | 'm' | 'M') => output.push_str(r"\b"),
            Some(next) => {
                output.push('\\');
                output.push(next);
            }
            None => output.push('\\'),
        }
    }
    output
}

static PROVIDER_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[A-Z]{2,3}\s+)?\(\s*(?P<label>[^()]{1,32}?)\s*\)\s*:")
        .expect("provider label regex is valid")
});

/// Returns the stable output name of a channel with a parsed event.
///
/// The name comes from the `league` and `slot` captures, for example
/// `NBA 01`. When the title has no league and slot, the name comes from a
/// provider label such as `(ESPN+ 069):`. Otherwise the result is `None`, and
/// the channel keeps its provider name.
#[must_use]
pub fn stable_event_channel_name(
    captures: &BTreeMap<String, String>,
    stream_name: &str,
) -> Option<String> {
    let league = captures
        .get("league")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let slot = captures
        .get("slot")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if let (Some(league), Some(slot)) = (league, slot) {
        return Some(format!("{} {slot}", display_league(league)));
    }
    PROVIDER_LABEL
        .captures(stream_name)
        .and_then(|captures| captures.name("label"))
        .map(|label| {
            label
                .as_str()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|label| !label.is_empty())
}

/// Writes a short league abbreviation in capitals and keeps longer names.
fn display_league(league: &str) -> String {
    let collapsed = league.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= 4 && collapsed.chars().all(|c| c.is_ascii_alphabetic()) {
        collapsed.to_ascii_uppercase()
    } else {
        collapsed
    }
}

/// Resolves the timezone of provider event times.
///
/// The order is: the template timezone if it is not `UTC`, then the source
/// timezone if it is not `UTC`, then the region timezone. The fallback is
/// `UTC`.
#[must_use]
pub fn resolve_event_timezone(template: &str, source: Option<&str>, region: Option<&str>) -> Tz {
    [
        Some(template).filter(|value| value.trim() != "UTC"),
        source.filter(|value| value.trim() != "UTC"),
        region,
    ]
    .into_iter()
    .flatten()
    .find_map(|value| value.trim().parse::<Tz>().ok())
    .unwrap_or(Tz::UTC)
}

/// Parses the `slot` capture as the legacy `event_channels.slot_number`.
#[must_use]
pub fn slot_number(captures: &BTreeMap<String, String>) -> i32 {
    captures
        .get("slot")
        .and_then(|slot| slot.trim().parse::<i32>().ok())
        .unwrap_or(0)
}

/// Builds the guide of one event channel: upcoming filler, event, and ended
/// filler.
///
/// The upcoming filler starts at the hour of `now` and stops at the event
/// start. The ended filler starts at the event stop and lasts for `grace`.
/// The intervals do not overlap and have no gaps between them.
#[must_use]
pub fn plan_event_channel_guide(
    event: &GeneratedProgramme,
    timezone: Tz,
    now: DateTime<Utc>,
    grace: Duration,
    settings: &EventScanSettings,
) -> Vec<GeneratedProgramme> {
    let mut programmes = Vec::with_capacity(3);
    let window_start = now.duration_trunc(Duration::hours(1)).unwrap_or(now);
    if window_start < event.starts_at {
        programmes.push(GeneratedProgramme {
            starts_at: window_start,
            stops_at: event.starts_at,
            title: upcoming_title(
                &settings.upcoming_prefix,
                &event.title,
                event.starts_at,
                timezone,
            ),
            kind: GeneratedProgrammeKind::Filler,
            stable_event_key: None,
            source_title: event.source_title.clone(),
        });
    }
    programmes.push(event.clone());
    if grace > Duration::zero()
        && let Some(ended_stop) = event.stops_at.checked_add_signed(grace)
    {
        programmes.push(GeneratedProgramme {
            starts_at: event.stops_at,
            stops_at: ended_stop,
            title: settings.ended_title.clone(),
            kind: GeneratedProgrammeKind::Filler,
            stable_event_key: None,
            source_title: event.source_title.clone(),
        });
    }
    programmes
}

/// Formats `Upcoming: <title>, <local start time>`.
#[must_use]
pub fn upcoming_title(prefix: &str, title: &str, starts_at: DateTime<Utc>, timezone: Tz) -> String {
    let local = starts_at.with_timezone(&timezone);
    format!("{prefix}: {title}, {}", local.format("%b %-d %-I:%M %p %Z"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn captures(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn stable_name_uses_league_and_slot() {
        assert_eq!(
            stable_event_channel_name(
                &captures(&[("league", "NHL"), ("slot", "03")]),
                "NHL: 03 - 10pm Sharks @ Kings [1080p]"
            )
            .as_deref(),
            Some("NHL 03")
        );
        assert_eq!(
            stable_event_channel_name(&captures(&[("league", "nba"), ("slot", "01")]), "x")
                .as_deref(),
            Some("NBA 01")
        );
        assert_eq!(
            stable_event_channel_name(
                &captures(&[("league", "NCAA  Football"), ("slot", "2")]),
                "x"
            )
            .as_deref(),
            Some("NCAA Football 2")
        );
    }

    #[test]
    fn stable_name_falls_back_to_provider_label() {
        assert_eq!(
            stable_event_channel_name(
                &captures(&[("league", "NCAA Football")]),
                "CA (TSN+ 007): NCAA Football: Ball State vs. Massachusetts (2026-11-04 19:00:00) [1080p]"
            )
            .as_deref(),
            Some("TSN+ 007")
        );
        assert_eq!(
            stable_event_channel_name(
                &captures(&[]),
                "(FLSP 004): hockey:  Kingston Frontenacs vs Ottawa 67`s (Home) (2026-11-04 10:30:00) [1080p]"
            )
            .as_deref(),
            Some("FLSP 004")
        );
        assert_eq!(
            stable_event_channel_name(&captures(&[]), "Broncos vs Chiefs 2026-09-14 00:20 HD"),
            None
        );
    }

    #[test]
    fn timezone_order_is_template_source_region() {
        assert_eq!(
            resolve_event_timezone("America/Denver", Some("America/New_York"), Some("UTC")),
            chrono_tz::America::Denver
        );
        assert_eq!(
            resolve_event_timezone("UTC", Some("America/New_York"), Some("America/Denver")),
            chrono_tz::America::New_York
        );
        assert_eq!(
            resolve_event_timezone("UTC", Some("UTC"), Some("America/Denver")),
            chrono_tz::America::Denver
        );
        assert_eq!(resolve_event_timezone("UTC", None, None), Tz::UTC);
        assert_eq!(
            resolve_event_timezone("UTC", Some("bad"), Some("also bad")),
            Tz::UTC
        );
    }

    #[test]
    fn slot_number_parses_capture() {
        assert_eq!(slot_number(&captures(&[("slot", "03")])), 3);
        assert_eq!(slot_number(&captures(&[])), 0);
    }

    #[test]
    fn template_regex_accepts_rust_and_postgres_word_boundaries() {
        let rust = compile_template_regex(r"(?i)\bNHL\b.*\b(?:vs\.?|@|at)\b").unwrap();
        assert!(rust.is_match("NHL: 01 - 7:30pm Senators at Sabres"));
        let postgres = compile_template_regex(r"(?i)\yNHL\y.*\y(?:vs|at)\y").unwrap();
        assert!(postgres.is_match("NHL: 02 - Senators vs Sabres"));
        assert!(!postgres.is_match("NHLX vs"));
        assert_eq!(
            postgres_word_boundaries_to_rust(r"a\\b\d\m\M"),
            r"a\\b\d\b\b"
        );
    }

    #[test]
    fn state_follows_time() {
        let start = at("2026-11-05T00:30:00Z");
        let stop = at("2026-11-05T03:30:00Z");
        let grace = Duration::hours(4);
        let state = |now| EventChannelState::at(start, stop, grace, at(now));
        assert_eq!(state("2026-11-04T15:00:00Z"), EventChannelState::Scheduled);
        assert_eq!(state("2026-11-05T00:30:00Z"), EventChannelState::Live);
        assert_eq!(state("2026-11-05T03:30:00Z"), EventChannelState::Ended);
        assert_eq!(state("2026-11-05T07:30:00Z"), EventChannelState::Hidden);
    }

    #[test]
    fn guide_has_upcoming_event_and_ended_without_gaps() {
        let event = GeneratedProgramme {
            starts_at: at("2026-11-05T00:30:00Z"),
            stops_at: at("2026-11-05T03:30:00Z"),
            title: "Senators @ Sabres".to_owned(),
            kind: GeneratedProgrammeKind::Event,
            stable_event_key: Some("key".to_owned()),
            source_title: "NHL: 01 - 7:30pm Senators @ Sabres [1080p]".to_owned(),
        };
        let guide = plan_event_channel_guide(
            &event,
            chrono_tz::America::New_York,
            at("2026-11-04T15:17:00Z"),
            Duration::hours(4),
            &EventScanSettings::default(),
        );
        assert_eq!(guide.len(), 3);
        assert_eq!(guide[0].starts_at, at("2026-11-04T15:00:00Z"));
        assert_eq!(
            guide[0].title,
            "Upcoming: Senators @ Sabres, Nov 4 7:30 PM EST"
        );
        assert_eq!(guide[1], event);
        assert_eq!(guide[2].title, "Event ended");
        assert_eq!(guide[2].stops_at, at("2026-11-05T07:30:00Z"));
        assert!(
            guide
                .windows(2)
                .all(|pair| pair[0].stops_at == pair[1].starts_at)
        );

        let live = plan_event_channel_guide(
            &event,
            chrono_tz::America::New_York,
            at("2026-11-05T01:00:00Z"),
            Duration::hours(4),
            &EventScanSettings::default(),
        );
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].kind, GeneratedProgrammeKind::Event);
    }
}
