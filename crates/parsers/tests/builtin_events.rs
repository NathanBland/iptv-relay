use chrono::{TimeZone, Utc};
use iptv_domain::builtin_sports_event_rules;
use iptv_parsers::{
    CompiledEventRules, EventCandidate, GeneratedProgrammeKind, generate_event_guide,
};
use std::collections::BTreeMap;

#[test]
fn built_in_vs_rule_removes_prefix_and_quality_and_honors_group_timezone() {
    let mut rules = builtin_sports_event_rules();
    for rule in &mut rules.0 {
        rule.timezone = "America/Denver".to_owned();
    }
    let compiled = CompiledEventRules::new(rules).expect("built-ins compile");
    let categories = Vec::new();
    let metadata = BTreeMap::new();
    let candidate = EventCandidate {
        name: "VIP 12: Broncos vs Chiefs (2026-09-13 18:20) FHD",
        group: Some("Sports"),
        epg_title: None,
        categories: &categories,
        metadata: &metadata,
    };
    let matched = compiled
        .first_match(&candidate)
        .expect("unambiguous local time")
        .expect("built-in match");
    assert_eq!(
        matched.captures.get("home").map(String::as_str),
        Some("Broncos")
    );
    assert_eq!(
        matched.captures.get("away").map(String::as_str),
        Some("Chiefs")
    );
    assert_eq!(
        matched.starts_at,
        Some(Utc.with_ymd_and_hms(2026, 9, 14, 0, 20, 0).unwrap())
    );

    let window_start = Utc.with_ymd_and_hms(2026, 9, 14, 0, 0, 0).unwrap();
    let window_end = Utc.with_ymd_and_hms(2026, 9, 14, 4, 0, 0).unwrap();
    let guide = generate_event_guide(&matched, candidate.name, window_start, window_end)
        .expect("generated guide");
    assert_eq!(guide.len(), 3);
    assert_eq!(guide[0].kind, GeneratedProgrammeKind::Filler);
    assert_eq!(guide[0].title, "No programs available");
    assert_eq!(guide[1].kind, GeneratedProgrammeKind::Event);
    assert_eq!(guide[1].title, "Broncos vs Chiefs");
    assert_eq!(guide[1].source_title, candidate.name);
    assert_eq!(guide[2].kind, GeneratedProgrammeKind::Filler);
    assert_eq!(guide[0].stops_at, guide[1].starts_at);
    assert_eq!(guide[1].stops_at, guide[2].starts_at);
}

#[test]
fn built_in_at_rule_supports_us_twelve_hour_time() {
    let compiled =
        CompiledEventRules::new(builtin_sports_event_rules()).expect("built-ins compile");
    let categories = Vec::new();
    let metadata = BTreeMap::new();
    let candidate = EventCandidate {
        name: "Yankees @ Rockies 08/19/2026 07:10 PM HD",
        group: Some("Baseball"),
        epg_title: None,
        categories: &categories,
        metadata: &metadata,
    };
    let matched = compiled
        .first_match(&candidate)
        .expect("unambiguous")
        .expect("built-in match");
    assert_eq!(
        matched.captures.get("away").map(String::as_str),
        Some("Yankees")
    );
    assert_eq!(
        matched.captures.get("home").map(String::as_str),
        Some("Rockies")
    );
    assert_eq!(
        matched.starts_at,
        Some(Utc.with_ymd_and_hms(2026, 8, 19, 19, 10, 0).unwrap())
    );
    assert_eq!(matched.rule.guide.title_template, "{away} @ {home}");
}
