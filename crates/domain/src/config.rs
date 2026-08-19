//! Operator-facing configuration metadata and deterministic inheritance.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingValueKind {
    Boolean,
    Integer,
    String,
    Choice,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingRisk {
    Low,
    Capacity,
    Compatibility,
    ServiceDisruption,
    Security,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApplyRequirement {
    Immediate,
    Restart,
    Reimport,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingDefinition {
    pub key: String,
    pub label: String,
    pub description: String,
    pub value_kind: SettingValueKind,
    pub default_value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub operational_effect: String,
    pub risk: SettingRisk,
    pub apply_requirement: ApplyRequirement,
    pub provider_overridable: bool,
    pub group_overridable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case", tag = "scope", content = "id")]
pub enum InheritanceSource {
    SystemDefault,
    Global,
    Provider(String),
    ChannelGroup(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveSetting {
    pub definition: SettingDefinition,
    pub value: Value,
    pub inherited_from: InheritanceSource,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SettingValidationError {
    #[error("unknown setting {0:?}")]
    UnknownSetting(String),
    #[error("setting {key:?} expects {expected}")]
    WrongType { key: String, expected: &'static str },
    #[error("setting {key:?} must be between {minimum} and {maximum}")]
    OutOfRange {
        key: String,
        minimum: i64,
        maximum: i64,
    },
    #[error("setting {key:?} must be one of {choices:?}")]
    InvalidChoice { key: String, choices: Vec<String> },
    #[error("setting {0:?} cannot be overridden for a provider")]
    ProviderOverrideForbidden(String),
    #[error("setting {0:?} cannot be overridden for a channel group")]
    GroupOverrideForbidden(String),
}

impl SettingDefinition {
    /// Validates a JSON control-plane value without coercion.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the value has the wrong JSON type, violates
    /// an integer range, or is not one of the declared choices.
    pub fn validate(&self, value: &Value) -> Result<(), SettingValidationError> {
        match self.value_kind {
            SettingValueKind::Boolean if !value.is_boolean() => {
                return Err(self.wrong_type("a boolean"));
            }
            SettingValueKind::Integer => {
                let Some(value) = value.as_i64() else {
                    return Err(self.wrong_type("an integer"));
                };
                let minimum = self.minimum.unwrap_or(i64::MIN);
                let maximum = self.maximum.unwrap_or(i64::MAX);
                if !(minimum..=maximum).contains(&value) {
                    return Err(SettingValidationError::OutOfRange {
                        key: self.key.clone(),
                        minimum,
                        maximum,
                    });
                }
            }
            SettingValueKind::String if !value.is_string() => {
                return Err(self.wrong_type("a string"));
            }
            SettingValueKind::Choice => {
                let Some(value) = value.as_str() else {
                    return Err(self.wrong_type("a string choice"));
                };
                if !self.choices.iter().any(|choice| choice == value) {
                    return Err(SettingValidationError::InvalidChoice {
                        key: self.key.clone(),
                        choices: self.choices.clone(),
                    });
                }
            }
            SettingValueKind::Boolean | SettingValueKind::String => {}
        }
        Ok(())
    }

    fn wrong_type(&self, expected: &'static str) -> SettingValidationError {
        SettingValidationError::WrongType {
            key: self.key.clone(),
            expected,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SettingOverrides {
    pub global: BTreeMap<String, Value>,
    pub provider: BTreeMap<String, Value>,
    pub group: BTreeMap<String, Value>,
}

/// Returns backend-owned v1 defaults and the operator-facing explanation for
/// each setting. The keys are stable API identifiers, not display labels.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn setting_catalog() -> Vec<SettingDefinition> {
    vec![
        integer_setting(
            "ingest.max_download_bytes",
            "Maximum source download",
            "Stops a catalogue or guide download after this many bytes; the previous active generation remains in service.",
            2_147_483_648,
            1_048_576,
            8_589_934_592,
            "bytes",
            "Higher limits admit larger providers but require more temporary disk space.",
            SettingRisk::Capacity,
            ApplyRequirement::Reimport,
            true,
            false,
        ),
        integer_setting(
            "ingest.max_m3u_line_bytes",
            "Maximum M3U line",
            "Rejects an M3U logical line larger than this limit instead of allowing unbounded allocation.",
            1_048_576,
            4_096,
            16_777_216,
            "bytes",
            "Higher limits support unusual provider metadata at the cost of parser memory.",
            SettingRisk::Capacity,
            ApplyRequirement::Reimport,
            true,
            false,
        ),
        integer_setting(
            "media.ring.duration_seconds",
            "Live ring duration",
            "Keeps this many recent seconds for new or temporarily slow viewers.",
            8,
            1,
            120,
            "seconds",
            "Higher values improve jitter tolerance and increase memory and tune latency.",
            SettingRisk::Capacity,
            ApplyRequirement::Immediate,
            true,
            false,
        ),
        integer_setting(
            "media.ring.max_bytes",
            "Live ring byte ceiling",
            "Caps the memory held by one active channel; old packet-aligned chunks are recycled at the limit.",
            67_108_864,
            1_048_576,
            1_073_741_824,
            "bytes per channel",
            "The effective ring is whichever limit is reached first: duration or bytes.",
            SettingRisk::Capacity,
            ApplyRequirement::Immediate,
            true,
            false,
        ),
        integer_setting(
            "media.viewer.start_delay_seconds",
            "Viewer start delay",
            "Starts new viewers behind the live write head so short network stalls can be absorbed.",
            2,
            0,
            30,
            "seconds",
            "Higher values trade tune latency for smoother startup.",
            SettingRisk::Low,
            ApplyRequirement::Immediate,
            true,
            false,
        ),
        integer_setting(
            "media.recovery.window_seconds",
            "Recovery window",
            "Keeps downstream responses open while the channel actor reconnects or selects a fallback stream.",
            10,
            0,
            60,
            "seconds",
            "Long recovery windows mask provider faults but can delay a visible terminal error.",
            SettingRisk::Compatibility,
            ApplyRequirement::Immediate,
            true,
            false,
        ),
        choice_setting(
            "media.adapter",
            "Input adapter",
            "Selects native MPEG-TS ingestion or a fixed FFmpeg/VLC stream-copy adapter.",
            "auto",
            &["auto", "native-ts", "ffmpeg", "vlc"],
            "Changing adapters can alter compatibility, startup time, and reconnect behavior.",
            SettingRisk::Compatibility,
            ApplyRequirement::Immediate,
            true,
            false,
        ),
        integer_setting(
            "events.inferred_duration_seconds",
            "Inferred event duration",
            "Sets the programme length when a dynamic channel title contains a start time but no end time.",
            10_800,
            300,
            86_400,
            "seconds",
            "Generated filler is recalculated around the inferred programme interval.",
            SettingRisk::Low,
            ApplyRequirement::Reimport,
            true,
            true,
        ),
        string_setting(
            "events.filler_title",
            "Filler programme title",
            "Names the contiguous programme emitted before and after a dynamic event.",
            "No programs available",
            "The selected text is published in XMLTV for generated guide gaps.",
            SettingRisk::Low,
            ApplyRequirement::Reimport,
            true,
            true,
        ),
    ]
}

/// Resolves group, provider, global, and system-default values in descending
/// precedence while enforcing each definition's override policy.
///
/// # Errors
///
/// Returns an error for unknown keys, forbidden scopes, or invalid values.
pub fn resolve_settings(
    overrides: &SettingOverrides,
    provider_id: &str,
    group_id: &str,
) -> Result<Vec<EffectiveSetting>, SettingValidationError> {
    let definitions = setting_catalog();
    let known: BTreeMap<_, _> = definitions
        .iter()
        .map(|definition| (definition.key.as_str(), definition))
        .collect();
    for key in overrides
        .global
        .keys()
        .chain(overrides.provider.keys())
        .chain(overrides.group.keys())
    {
        if !known.contains_key(key.as_str()) {
            return Err(SettingValidationError::UnknownSetting(key.clone()));
        }
    }

    definitions
        .into_iter()
        .map(|definition| {
            let (value, inherited_from) = if let Some(value) = overrides.group.get(&definition.key)
            {
                if !definition.group_overridable {
                    return Err(SettingValidationError::GroupOverrideForbidden(
                        definition.key.clone(),
                    ));
                }
                (
                    value.clone(),
                    InheritanceSource::ChannelGroup(group_id.to_owned()),
                )
            } else if let Some(value) = overrides.provider.get(&definition.key) {
                if !definition.provider_overridable {
                    return Err(SettingValidationError::ProviderOverrideForbidden(
                        definition.key.clone(),
                    ));
                }
                (
                    value.clone(),
                    InheritanceSource::Provider(provider_id.to_owned()),
                )
            } else if let Some(value) = overrides.global.get(&definition.key) {
                (value.clone(), InheritanceSource::Global)
            } else {
                (
                    definition.default_value.clone(),
                    InheritanceSource::SystemDefault,
                )
            };
            definition.validate(&value)?;
            Ok(EffectiveSetting {
                definition,
                value,
                inherited_from,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn integer_setting(
    key: &str,
    label: &str,
    description: &str,
    default_value: i64,
    minimum: i64,
    maximum: i64,
    unit: &str,
    operational_effect: &str,
    risk: SettingRisk,
    apply_requirement: ApplyRequirement,
    provider_overridable: bool,
    group_overridable: bool,
) -> SettingDefinition {
    SettingDefinition {
        key: key.to_owned(),
        label: label.to_owned(),
        description: description.to_owned(),
        value_kind: SettingValueKind::Integer,
        default_value: Value::from(default_value),
        minimum: Some(minimum),
        maximum: Some(maximum),
        choices: Vec::new(),
        unit: Some(unit.to_owned()),
        operational_effect: operational_effect.to_owned(),
        risk,
        apply_requirement,
        provider_overridable,
        group_overridable,
    }
}

#[allow(clippy::too_many_arguments)]
fn choice_setting(
    key: &str,
    label: &str,
    description: &str,
    default_value: &str,
    choices: &[&str],
    operational_effect: &str,
    risk: SettingRisk,
    apply_requirement: ApplyRequirement,
    provider_overridable: bool,
    group_overridable: bool,
) -> SettingDefinition {
    SettingDefinition {
        key: key.to_owned(),
        label: label.to_owned(),
        description: description.to_owned(),
        value_kind: SettingValueKind::Choice,
        default_value: Value::from(default_value),
        minimum: None,
        maximum: None,
        choices: choices.iter().map(|choice| (*choice).to_owned()).collect(),
        unit: None,
        operational_effect: operational_effect.to_owned(),
        risk,
        apply_requirement,
        provider_overridable,
        group_overridable,
    }
}

#[allow(clippy::too_many_arguments)]
fn string_setting(
    key: &str,
    label: &str,
    description: &str,
    default_value: &str,
    operational_effect: &str,
    risk: SettingRisk,
    apply_requirement: ApplyRequirement,
    provider_overridable: bool,
    group_overridable: bool,
) -> SettingDefinition {
    SettingDefinition {
        key: key.to_owned(),
        label: label.to_owned(),
        description: description.to_owned(),
        value_kind: SettingValueKind::String,
        default_value: Value::from(default_value),
        minimum: None,
        maximum: None,
        choices: Vec::new(),
        unit: None,
        operational_effect: operational_effect.to_owned(),
        risk,
        apply_requirement,
        provider_overridable,
        group_overridable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn by_key<'a>(settings: &'a [EffectiveSetting], key: &str) -> &'a EffectiveSetting {
        settings
            .iter()
            .find(|setting| setting.definition.key == key)
            .expect("setting exists")
    }

    #[test]
    fn catalog_exposes_stable_media_and_ingest_defaults() {
        let settings = resolve_settings(&SettingOverrides::default(), "provider-a", "sports")
            .expect("default settings");
        assert_eq!(by_key(&settings, "media.ring.duration_seconds").value, 8);
        assert_eq!(by_key(&settings, "media.ring.max_bytes").value, 67_108_864);
        assert_eq!(by_key(&settings, "media.adapter").value, "auto");
        assert_eq!(
            by_key(&settings, "ingest.max_download_bytes").value,
            2_147_483_648_i64
        );
        assert!(settings.iter().all(|setting| {
            !setting.definition.description.is_empty()
                && !setting.definition.operational_effect.is_empty()
                && setting.inherited_from == InheritanceSource::SystemDefault
        }));
    }

    #[test]
    fn resolution_uses_group_provider_global_default_precedence() {
        let mut overrides = SettingOverrides::default();
        overrides
            .global
            .insert("media.ring.duration_seconds".to_owned(), json!(12));
        overrides
            .provider
            .insert("media.ring.duration_seconds".to_owned(), json!(16));
        overrides
            .global
            .insert("events.inferred_duration_seconds".to_owned(), json!(7_200));
        overrides
            .provider
            .insert("events.inferred_duration_seconds".to_owned(), json!(8_000));
        overrides
            .group
            .insert("events.inferred_duration_seconds".to_owned(), json!(9_000));
        let settings = resolve_settings(&overrides, "provider-a", "sports").expect("resolve");
        let ring = by_key(&settings, "media.ring.duration_seconds");
        assert_eq!(ring.value, 16);
        assert_eq!(
            ring.inherited_from,
            InheritanceSource::Provider("provider-a".to_owned())
        );
        let duration = by_key(&settings, "events.inferred_duration_seconds");
        assert_eq!(duration.value, 9_000);
        assert_eq!(
            duration.inherited_from,
            InheritanceSource::ChannelGroup("sports".to_owned())
        );
    }

    #[test]
    fn validation_rejects_type_range_and_choice_errors() {
        let catalog = setting_catalog();
        let ring = catalog
            .iter()
            .find(|setting| setting.key == "media.ring.duration_seconds")
            .expect("ring setting");
        assert!(matches!(
            ring.validate(&json!("8")),
            Err(SettingValidationError::WrongType { .. })
        ));
        assert!(matches!(
            ring.validate(&json!(0)),
            Err(SettingValidationError::OutOfRange { .. })
        ));
        let adapter = catalog
            .iter()
            .find(|setting| setting.key == "media.adapter")
            .expect("adapter setting");
        assert!(matches!(
            adapter.validate(&json!(5)),
            Err(SettingValidationError::WrongType { .. })
        ));
        assert!(matches!(
            adapter.validate(&json!("shell")),
            Err(SettingValidationError::InvalidChoice { .. })
        ));
        assert!(adapter.validate(&json!("vlc")).is_ok());
    }

    #[test]
    fn unknown_and_forbidden_overrides_are_explicit() {
        let mut unknown = SettingOverrides::default();
        unknown.global.insert("mystery".to_owned(), json!(true));
        assert_eq!(
            resolve_settings(&unknown, "provider-a", "sports"),
            Err(SettingValidationError::UnknownSetting("mystery".to_owned()))
        );

        let mut forbidden = SettingOverrides::default();
        forbidden
            .group
            .insert("media.ring.max_bytes".to_owned(), json!(1_048_576));
        assert_eq!(
            resolve_settings(&forbidden, "provider-a", "sports"),
            Err(SettingValidationError::GroupOverrideForbidden(
                "media.ring.max_bytes".to_owned()
            ))
        );

        let definition = SettingDefinition {
            key: "private".to_owned(),
            label: String::new(),
            description: String::new(),
            value_kind: SettingValueKind::Boolean,
            default_value: json!(false),
            minimum: None,
            maximum: None,
            choices: Vec::new(),
            unit: None,
            operational_effect: String::new(),
            risk: SettingRisk::Security,
            apply_requirement: ApplyRequirement::Restart,
            provider_overridable: false,
            group_overridable: false,
        };
        assert!(definition.validate(&json!(true)).is_ok());
        assert!(matches!(
            definition.validate(&json!("true")),
            Err(SettingValidationError::WrongType { .. })
        ));
        assert_eq!(
            SettingValidationError::ProviderOverrideForbidden("private".to_owned()).to_string(),
            "setting \"private\" cannot be overridden for a provider"
        );
    }

    #[test]
    fn metadata_serializes_for_generated_clients() {
        let catalog = setting_catalog();
        let encoded = serde_json::to_value(&catalog).expect("serialize catalog");
        let first = &encoded.as_array().expect("array")[0];
        assert!(first.get("defaultValue").is_some());
        assert!(first.get("operationalEffect").is_some());
        assert!(first.get("applyRequirement").is_some());
    }
}
