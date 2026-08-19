//! Bounded pull parsers for IPTV catalogue, guide, and dynamic event-rule inputs.

mod diagnostics;
mod events;
mod m3u;
mod xmltv;
mod xtream;

pub use diagnostics::{Diagnostic, DiagnosticCode, ParseError, ParseLimits, ParseStats, Severity};
pub use events::{
    CompiledEventRules, EventCandidate, EventDateError, EventGuideError, EventRuleDocument,
    GeneratedProgramme, GeneratedProgrammeKind, MatchedEvent, generate_event_guide,
    parse_event_rules,
};
pub use m3u::{M3uDirective, M3uEntry, M3uHeader, M3uPlaylist, parse_m3u, parse_m3u_visit};
pub use xmltv::{
    TimestampPrecision, XmltvDocument, XmltvParseOptions, XmltvTimestamp, XmltvTimestampError,
    parse_xmltv, parse_xmltv_timestamp, parse_xmltv_timestamp_in_timezone, parse_xmltv_visit,
    parse_xmltv_visit_with_options, parse_xmltv_with_options,
};
pub use xtream::{
    XtreamAuth, XtreamCategory, XtreamDocument, XtreamLiveStream, XtreamShortEpgEntry,
    parse_xtream_auth, parse_xtream_live_categories, parse_xtream_live_streams,
    parse_xtream_short_epg,
};
