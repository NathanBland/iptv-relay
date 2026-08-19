use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;
use utoipa::ToSchema;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct LocalizedText {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl LocalizedText {
    pub fn new(value: impl Into<String>, language: Option<String>) -> Self {
        Self {
            value: value.into(),
            language,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EpgIcon {
    #[schema(value_type = String)]
    pub source: Url,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ExtensionElement {
    pub name: String,
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EpgChannel {
    pub id: String,
    #[serde(default)]
    pub display_names: Vec<LocalizedText>,
    #[serde(default)]
    pub icons: Vec<EpgIcon>,
    #[serde(default)]
    #[schema(value_type = Vec<String>)]
    pub urls: Vec<Url>,
    #[serde(default)]
    pub extensions: Vec<ExtensionElement>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct Credits {
    #[serde(default)]
    pub entries: Vec<Credit>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct Credit {
    pub role: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct EpisodeNumber {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ProgrammeRating {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default)]
    pub icons: Vec<EpgIcon>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct Programme {
    pub channel_id: String,
    pub start: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<DateTime<Utc>>,
    #[serde(default)]
    pub titles: Vec<LocalizedText>,
    #[serde(default)]
    pub sub_titles: Vec<LocalizedText>,
    #[serde(default)]
    pub descriptions: Vec<LocalizedText>,
    #[serde(default)]
    pub categories: Vec<LocalizedText>,
    #[serde(default)]
    pub episode_numbers: Vec<EpisodeNumber>,
    #[serde(default)]
    pub icons: Vec<EpgIcon>,
    #[serde(default)]
    pub ratings: Vec<ProgrammeRating>,
    #[serde(default)]
    pub credits: Credits,
    #[serde(default)]
    pub previously_shown: bool,
    #[serde(default)]
    pub new: bool,
    #[serde(default)]
    pub extensions: Vec<ExtensionElement>,
}

impl Programme {
    pub fn has_valid_interval(&self) -> bool {
        self.stop.is_none_or(|stop| self.start < stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn programme_intervals_are_half_open_and_forward() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let mut programme = Programme {
            channel_id: "example".into(),
            start,
            stop: Some(start + chrono::Duration::hours(1)),
            titles: vec![],
            sub_titles: vec![],
            descriptions: vec![],
            categories: vec![],
            episode_numbers: vec![],
            icons: vec![],
            ratings: vec![],
            credits: Credits::default(),
            previously_shown: false,
            new: false,
            extensions: vec![],
        };
        assert!(programme.has_valid_interval());
        programme.stop = Some(start);
        assert!(!programme.has_valid_interval());
    }
}
