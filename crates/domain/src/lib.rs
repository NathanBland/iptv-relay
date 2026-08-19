//! Shared, serialization-friendly domain types for the IPTV control and media planes.
//!
//! The central identity rule is deliberate: a [`ChannelId`] identifies a logical
//! service and must not be derived from a mutable stream URL, playlist position,
//! display name, or provider-specific identifier.

mod catalog;
mod config;
mod epg;
mod events;
mod ids;
mod reconcile;

pub use catalog::{
    Channel, ChannelGroup, ChannelNumber, ChannelNumberError, Provider, ProviderError, Source,
    SourceKind,
};
pub use config::{
    ApplyRequirement, EffectiveSetting, InheritanceSource, SettingDefinition, SettingOverrides,
    SettingRisk, SettingValidationError, SettingValueKind, resolve_settings, setting_catalog,
};
pub use epg::{
    Credit, Credits, EpgChannel, EpgIcon, EpisodeNumber, ExtensionElement, LocalizedText,
    Programme, ProgrammeRating,
};
pub use events::{
    DatePattern, EventChannelTemplate, EventGuidePolicy, EventIdentityPolicy, EventLifecyclePolicy,
    EventMatch, EventRule, EventRuleSet, builtin_sports_event_rules,
};
pub use ids::{ChannelId, EpgSourceId, EventRuleId, ProviderId, SourceId};
pub use reconcile::{
    MappingCandidate, MappingContext, MappingDecision, MappingMethod, StationEvidence,
    apply_non_destructive, reconcile_epg,
};
