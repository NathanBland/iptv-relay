use crate::EventRuleId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;
use uuid::Uuid;

fn default_true() -> bool {
    true
}

fn default_timezone() -> String {
    "UTC".to_owned()
}

fn default_event_group() -> String {
    "Sports".to_owned()
}

fn default_archive_seconds() -> u64 {
    86_400
}

fn default_duration_seconds() -> u64 {
    10_800
}

fn default_filler_title() -> String {
    "No programs available".to_owned()
}

fn default_title_template() -> String {
    "{source}".to_owned()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventIdentityPolicy {
    /// Generate managed guide IDs from the canonical channel UUID.
    #[default]
    ChannelUuid,
    /// Use the source `tvg-id` when the source contract guarantees stability.
    SourceTvgId,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EventMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_title_regex: Option<String>,
    #[serde(default)]
    pub categories_any: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DatePattern {
    /// Regex with a named `datetime` capture (or one positional capture).
    pub regex: String,
    /// Chrono format string used to parse the captured local date/time.
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EventLifecyclePolicy {
    #[serde(default)]
    pub pre_roll_seconds: u64,
    #[serde(default)]
    pub post_roll_seconds: u64,
    #[serde(default = "default_archive_seconds")]
    pub archive_after_seconds: u64,
}

impl Default for EventLifecyclePolicy {
    fn default() -> Self {
        Self {
            pre_roll_seconds: 0,
            post_roll_seconds: 0,
            archive_after_seconds: default_archive_seconds(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EventGuidePolicy {
    /// Duration used when the provider title supplies a start time but no end.
    #[serde(default = "default_duration_seconds")]
    pub duration_seconds: u64,
    /// Programme title emitted outside the event interval.
    #[serde(default = "default_filler_title")]
    pub filler_title: String,
    /// Event title template. `{source}` and named regex captures are supported.
    #[serde(default = "default_title_template")]
    pub title_template: String,
}

impl Default for EventGuidePolicy {
    fn default() -> Self {
        Self {
            duration_seconds: default_duration_seconds(),
            filler_title: default_filler_title(),
            title_template: default_title_template(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EventChannelTemplate {
    #[serde(default = "default_event_group")]
    pub group: String,
    #[serde(default = "default_name_template")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_start: Option<u32>,
    #[serde(default)]
    pub identity: EventIdentityPolicy,
}

fn default_name_template() -> String {
    "{title}".to_owned()
}

impl Default for EventChannelTemplate {
    fn default() -> Self {
        Self {
            group: default_event_group(),
            name: default_name_template(),
            number_start: None,
            identity: EventIdentityPolicy::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EventRule {
    #[serde(default)]
    pub id: EventRuleId,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub matcher: EventMatch,
    #[serde(default)]
    pub date_patterns: Vec<DatePattern>,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub lifecycle: EventLifecyclePolicy,
    #[serde(default)]
    pub guide: EventGuidePolicy,
    #[serde(default)]
    pub channel: EventChannelTemplate,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(transparent)]
pub struct EventRuleSet(pub Vec<EventRule>);

impl EventRuleSet {
    pub fn iter(&self) -> impl Iterator<Item = &EventRule> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Returns conservative built-in rules for provider titles such as
/// `Broncos vs Chiefs (2026-09-13 18:20) FHD` and
/// `Yankees @ Rockies 08/19/2026 07:10 PM HD`.
///
/// The rules intentionally require a recognizable date/time. Unscheduled
/// channels therefore remain ordinary channels instead of being guessed into
/// generated guide slots. Operators can clone and scope these defaults per
/// channel group, then override timezone, duration, title, and filler policy.
#[must_use]
pub fn builtin_sports_event_rules() -> EventRuleSet {
    const PREFIX: &str = r"(?:\[[^\]]{1,32}\]\s*|(?:event|vip|sports|slot)\s*\d*\s*[:|]\s*)*";
    const QUALITY: &str = r"(?:\s*[-|]?\s*(?:UHD|4K|FHD|HD|SD|2160p|1080p|720p))?";
    let formats = [
        (
            "ISO local time",
            r"20[0-9]{2}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}",
            "%Y-%m-%d %H:%M",
        ),
        (
            "ISO T local time",
            r"20[0-9]{2}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}",
            "%Y-%m-%dT%H:%M",
        ),
        (
            "US local time",
            r"[0-9]{1,2}/[0-9]{1,2}/20[0-9]{2} [0-9]{1,2}:[0-9]{2} [AP]M",
            "%m/%d/%Y %I:%M %p",
        ),
    ];
    let mut rules = Vec::with_capacity(formats.len() * 2);
    for (format_index, (label, date_regex, date_format)) in formats.into_iter().enumerate() {
        rules.push(builtin_sports_rule(
            format_index * 2,
            format!("Built-in sports: vs ({label})"),
            format!(
                r"(?i)^{PREFIX}(?P<home>.+?)\s+(?:v|vs|versus)\.?\s+(?P<away>.+?)\s+\(?(?:{date_regex})\)?{QUALITY}$"
            ),
            date_regex,
            date_format,
            "{home} vs {away}",
        ));
        rules.push(builtin_sports_rule(
            format_index * 2 + 1,
            format!("Built-in sports: at ({label})"),
            format!(
                r"(?i)^{PREFIX}(?P<away>.+?)\s+@\s+(?P<home>.+?)\s+\(?(?:{date_regex})\)?{QUALITY}$"
            ),
            date_regex,
            date_format,
            "{away} @ {home}",
        ));
    }
    EventRuleSet(rules)
}

fn builtin_sports_rule(
    index: usize,
    name: String,
    name_regex: String,
    date_regex: &str,
    date_format: &str,
    title_template: &str,
) -> EventRule {
    EventRule {
        id: EventRuleId::from_uuid(Uuid::from_u128(
            0x8b2d_571e_91de_7000_8000_0000_0000_0000_u128
                + u128::try_from(index).expect("built-in rule index fits u128"),
        )),
        name,
        enabled: true,
        matcher: EventMatch {
            name_regex: Some(name_regex),
            ..EventMatch::default()
        },
        date_patterns: vec![DatePattern {
            regex: format!(r"(?i)(?P<datetime>{date_regex})"),
            format: date_format.to_owned(),
            timezone: None,
        }],
        timezone: default_timezone(),
        lifecycle: EventLifecyclePolicy::default(),
        guide: EventGuidePolicy {
            title_template: title_template.to_owned(),
            ..EventGuidePolicy::default()
        },
        channel: EventChannelTemplate::default(),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_event_defaults_are_stable() {
        let rule: EventRule = serde_json::from_value(serde_json::json!({
            "name": "all sports"
        }))
        .expect("deserialize rule");
        assert!(rule.enabled);
        assert_eq!(rule.timezone, "UTC");
        assert_eq!(rule.lifecycle.archive_after_seconds, 86_400);
        assert_eq!(rule.guide.duration_seconds, 10_800);
        assert_eq!(rule.guide.filler_title, "No programs available");
        assert_eq!(rule.guide.title_template, "{source}");
        assert_eq!(rule.channel.group, "Sports");
        assert_eq!(rule.channel.identity, EventIdentityPolicy::ChannelUuid);
    }

    #[test]
    fn rule_set_exposes_ordered_iteration() {
        let first: EventRule =
            serde_json::from_value(serde_json::json!({"name": "first"})).expect("first rule");
        let second: EventRule =
            serde_json::from_value(serde_json::json!({"name": "second"})).expect("second rule");
        let rules = EventRuleSet(vec![first, second]);
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(!rules.is_empty());
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn built_in_sports_rules_are_stable_ordered_and_conservative() {
        let first = builtin_sports_event_rules();
        let second = builtin_sports_event_rules();
        assert_eq!(first, second);
        assert_eq!(first.len(), 6);
        assert_eq!(first.0[0].name, "Built-in sports: vs (ISO local time)");
        assert_eq!(first.0[1].guide.title_template, "{away} @ {home}");
        assert!(first.0.iter().all(|rule| {
            rule.enabled
                && rule.timezone == "UTC"
                && rule.guide.duration_seconds == 10_800
                && rule.guide.filler_title == "No programs available"
                && rule.matcher.name_regex.is_some()
                && rule.date_patterns.len() == 1
        }));
        assert_eq!(
            first.0[0].id.as_uuid(),
            Uuid::from_u128(0x8b2d_571e_91de_7000_8000_0000_0000_0000)
        );
    }
}
