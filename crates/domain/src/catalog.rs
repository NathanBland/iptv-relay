use crate::{ChannelId, ProviderId, SourceId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, str::FromStr};
use thiserror::Error;
use url::Url;
use utoipa::ToSchema;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct Provider {
    pub id: ProviderId,
    pub name: String,
    pub max_connections: u32,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl Provider {
    /// Creates a provider with a finite, positive upstream connection limit.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when the normalized name is empty or the limit is zero.
    pub fn new(name: impl Into<String>, max_connections: u32) -> Result<Self, ProviderError> {
        let name = name.into().trim().to_owned();
        if name.is_empty() {
            return Err(ProviderError::EmptyName);
        }
        if max_connections == 0 {
            return Err(ProviderError::ZeroConnections);
        }
        Ok(Self {
            id: ProviderId::new(),
            name,
            max_connections,
            metadata: BTreeMap::new(),
        })
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProviderError {
    #[error("provider name must not be empty")]
    EmptyName,
    #[error("provider max_connections must be at least one")]
    ZeroConnections,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    #[default]
    Auto,
    HttpTransportStream,
    Hls,
    Udp,
    Rtp,
    Rtsp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct Source {
    pub id: SourceId,
    pub provider_id: ProviderId,
    #[schema(value_type = String)]
    pub url: Url,
    #[serde(default)]
    pub kind: SourceKind,
    #[serde(default)]
    pub priority: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_stream_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_id: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ChannelGroup {
    pub name: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<ChannelNumber>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<ChannelGroup>,
    /// Source-scoped alias normally joined to XMLTV `channel@id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvg_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub logo: Option<Url>,
    /// Ordered source identities; lower indexes have higher preference.
    #[serde(default)]
    pub sources: Vec<SourceId>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl Channel {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: ChannelId::new(),
            name: name.into(),
            number: None,
            group: None,
            tvg_id: None,
            logo: None,
            sources: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}

/// Decimal channel number represented canonically as text to avoid floating-point identity bugs.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, ToSchema)]
#[serde(try_from = "String", into = "String")]
#[schema(value_type = String, example = "7.1")]
pub struct ChannelNumber(String);

impl ChannelNumber {
    /// Parses and canonicalizes a non-negative decimal channel number.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelNumberError`] for empty, overlong, signed, non-decimal, or malformed input.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ChannelNumberError> {
        let value = value.as_ref().trim();
        if value.is_empty() || value.len() > 32 {
            return Err(ChannelNumberError::Invalid);
        }

        let mut dot_seen = false;
        for (index, character) in value.char_indices() {
            match character {
                '0'..='9' => {}
                '.' if !dot_seen && index > 0 && index + 1 < value.len() => dot_seen = true,
                _ => return Err(ChannelNumberError::Invalid),
            }
        }

        let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
        let whole = whole.trim_start_matches('0');
        let whole = if whole.is_empty() { "0" } else { whole };
        let fractional = fractional.trim_end_matches('0');
        let canonical = if fractional.is_empty() {
            whole.to_owned()
        } else {
            format!("{whole}.{fractional}")
        };
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ChannelNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ChannelNumber {
    type Err = ChannelNumberError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ChannelNumber {
    type Error = ChannelNumberError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ChannelNumber> for String {
    fn from(value: ChannelNumber) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ChannelNumberError {
    #[error("channel number must be a decimal number of at most 32 characters")]
    Invalid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_validation_is_fail_closed() {
        assert_eq!(Provider::new("", 1), Err(ProviderError::EmptyName));
        assert_eq!(
            Provider::new("provider", 0),
            Err(ProviderError::ZeroConnections)
        );
        assert_eq!(
            Provider::new(" provider ", 2).expect("valid").name,
            "provider"
        );
    }

    #[test]
    fn channel_number_is_canonical_and_exact() {
        assert_eq!(
            ChannelNumber::new("007.100").expect("valid").as_str(),
            "7.1"
        );
        assert_eq!(ChannelNumber::new("000").expect("valid").as_str(), "0");
        assert!(ChannelNumber::new("7..1").is_err());
        assert!(ChannelNumber::new("-1").is_err());
        assert!(ChannelNumber::new("NaN").is_err());
    }

    #[test]
    fn channel_constructor_and_number_conversion_traits_are_consistent() {
        let channel = Channel::new("Denver Local");
        assert_eq!(channel.name, "Denver Local");
        assert!(channel.sources.is_empty());
        assert!(channel.metadata.is_empty());
        assert!(channel.number.is_none());
        assert!(channel.group.is_none());
        assert!(channel.tvg_id.is_none());
        assert!(channel.logo.is_none());

        let parsed: ChannelNumber = "007.100".parse().expect("parse channel number");
        assert_eq!(parsed.to_string(), "7.1");
        let via_try_from = ChannelNumber::try_from("0009.0200".to_owned()).expect("try from");
        let encoded: String = via_try_from.into();
        assert_eq!(encoded, "9.02");

        assert!(ChannelNumber::new("").is_err());
        assert!(ChannelNumber::new("1.").is_err());
        assert!(ChannelNumber::new(".1").is_err());
        assert!(ChannelNumber::new("123456789012345678901234567890123").is_err());
    }
}
