//! Tests that lock the time behavior of event parsing.
//!
//! These tests compare RFC 3339 strings and error messages, not library types.
//! The chrono to jiff migration must keep each expected value unchanged.

#![allow(
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unnecessary_wraps
)]

use iptv_domain::{DatePattern, builtin_sports_event_rules};
use iptv_parsers::{
    CompiledEventRules, EventCandidate, resolve_spelled_date_start, resolve_time_only_start,
};
use std::collections::BTreeMap;

/// Matches `name` with one rule that parses the whole name with `format`.
fn parse_with_format(
    name: &str,
    format: &str,
    timezone: &str,
    pattern_timezone: Option<&str>,
) -> Result<Option<String>, String> {
    let mut rules = builtin_sports_event_rules();
    rules.0.truncate(1);
    let rule = &mut rules.0[0];
    timezone.clone_into(&mut rule.timezone);
    rule.matcher.name_regex = Some("^.+$".to_owned());
    rule.date_patterns = vec![DatePattern {
        regex: "^(?P<datetime>.+)$".to_owned(),
        format: format.to_owned(),
        timezone: pattern_timezone.map(str::to_owned),
    }];
    let compiled = CompiledEventRules::new(rules).expect("rule compiles");
    let categories = Vec::new();
    let metadata = BTreeMap::new();
    let candidate = EventCandidate {
        name,
        group: None,
        epg_title: None,
        categories: &categories,
        metadata: &metadata,
    };
    match compiled.first_match(&candidate) {
        Ok(matched) => Ok(matched
            .and_then(|matched| matched.starts_at)
            .map(|starts_at| starts_at.to_rfc3339())),
        Err(error) => Err(error.to_string()),
    }
}

fn ok(value: &str) -> Result<Option<String>, String> {
    Ok(Some(value.to_owned()))
}

#[test]
fn each_operator_date_format_parses_to_the_same_instant() {
    let cases: &[(&str, &str, &str, Result<Option<String>, String>)] = &[
        // The built-in rule formats.
        (
            "2026-09-13 18:20",
            "%Y-%m-%d %H:%M",
            "America/Denver",
            ok("2026-09-14T00:20:00+00:00"),
        ),
        (
            "2026-09-13T18:20",
            "%Y-%m-%dT%H:%M",
            "America/Denver",
            ok("2026-09-14T00:20:00+00:00"),
        ),
        (
            "08/19/2026 07:10 PM",
            "%m/%d/%Y %I:%M %p",
            "America/New_York",
            ok("2026-08-19T23:10:00+00:00"),
        ),
        (
            "8/9/2026 7:10 pm",
            "%m/%d/%Y %I:%M %p",
            "America/New_York",
            ok("2026-08-09T23:10:00+00:00"),
        ),
        (
            "2026-09-13 18:20:45",
            "%Y-%m-%d %H:%M:%S",
            "America/Denver",
            ok("2026-09-14T00:20:45+00:00"),
        ),
        // Other operator formats.
        (
            "2026-09-13",
            "%Y-%m-%d",
            "America/Denver",
            ok("2026-09-13T06:00:00+00:00"),
        ),
        (
            "13/09/2026 18:20",
            "%d/%m/%Y %H:%M",
            "Europe/London",
            ok("2026-09-13T17:20:00+00:00"),
        ),
        (
            "13.09.2026 18:20",
            "%d.%m.%Y %H:%M",
            "Europe/Berlin",
            ok("2026-09-13T16:20:00+00:00"),
        ),
        (
            "202609131820",
            "%Y%m%d%H%M",
            "UTC",
            ok("2026-09-13T18:20:00+00:00"),
        ),
        (
            "Sep 13 2026 06:20 PM",
            "%b %d %Y %I:%M %p",
            "America/Denver",
            ok("2026-09-14T00:20:00+00:00"),
        ),
        (
            "September 13, 2026 18:20",
            "%B %d, %Y %H:%M",
            "America/Denver",
            ok("2026-09-14T00:20:00+00:00"),
        ),
        (
            "Sun Sep 13 2026 18:20",
            "%a %b %d %Y %H:%M",
            "America/Denver",
            ok("2026-09-14T00:20:00+00:00"),
        ),
        (
            "18:20 13/09/2026",
            "%H:%M %d/%m/%Y",
            "America/Denver",
            ok("2026-09-14T00:20:00+00:00"),
        ),
        // A format with an offset ignores the rule timezone.
        (
            "2026-09-13T18:20:00-0400",
            "%Y-%m-%dT%H:%M:%S%z",
            "America/Denver",
            ok("2026-09-13T22:20:00+00:00"),
        ),
        (
            "2026-09-13 18:20 +0200",
            "%Y-%m-%d %H:%M %z",
            "America/Denver",
            ok("2026-09-13T16:20:00+00:00"),
        ),
        (
            "2026-09-13T18:20:00+05:30",
            "%Y-%m-%dT%H:%M:%S%:z",
            "America/Denver",
            ok("2026-09-13T12:50:00+00:00"),
        ),
        (
            "20260913182000 -0600",
            "%Y%m%d%H%M%S %z",
            "UTC",
            ok("2026-09-14T00:20:00+00:00"),
        ),
        // Text that does not match the format fails closed.
        (
            "2026-09-13",
            "%Y-%m-%d %H:%M",
            "America/Denver",
            Err(
                "captured event date \"2026-09-13\" does not match format \"%Y-%m-%d %H:%M\""
                    .to_owned(),
            ),
        ),
        (
            "13/09/2026 18:20",
            "%m/%d/%Y %H:%M",
            "America/Denver",
            Err(
                "captured event date \"13/09/2026 18:20\" does not match format \"%m/%d/%Y %H:%M\""
                    .to_owned(),
            ),
        ),
    ];
    for (name, format, timezone, expected) in cases {
        assert_eq!(
            &parse_with_format(name, format, timezone, None),
            expected,
            "{name:?} with {format:?} in {timezone}"
        );
    }
}

#[test]
fn pattern_timezone_overrides_the_rule_timezone() {
    assert_eq!(
        parse_with_format(
            "2026-09-13 18:20",
            "%Y-%m-%d %H:%M",
            "UTC",
            Some("America/New_York")
        ),
        ok("2026-09-13T22:20:00+00:00")
    );
}

#[test]
fn rule_dates_at_daylight_saving_changes_fail_closed() {
    let cases: &[(&str, &str, Result<Option<String>, String>)] = &[
        // Spring change: 02:00 to 02:59 does not exist.
        (
            "2026-03-08 01:59",
            "America/Denver",
            ok("2026-03-08T08:59:00+00:00"),
        ),
        (
            "2026-03-08 02:30",
            "America/Denver",
            Err(
                "event date \"2026-03-08 02:30\" does not exist in timezone America/Denver"
                    .to_owned(),
            ),
        ),
        (
            "2026-03-08 03:00",
            "America/Denver",
            ok("2026-03-08T09:00:00+00:00"),
        ),
        (
            "2026-03-08 01:59",
            "America/New_York",
            ok("2026-03-08T06:59:00+00:00"),
        ),
        (
            "2026-03-08 02:30",
            "America/New_York",
            Err(
                "event date \"2026-03-08 02:30\" does not exist in timezone America/New_York"
                    .to_owned(),
            ),
        ),
        (
            "2026-03-08 03:00",
            "America/New_York",
            ok("2026-03-08T07:00:00+00:00"),
        ),
        // Fall change: 01:00 to 01:59 occurs two times.
        (
            "2026-11-01 00:59",
            "America/Denver",
            ok("2026-11-01T06:59:00+00:00"),
        ),
        (
            "2026-11-01 01:30",
            "America/Denver",
            Err(
                "event date \"2026-11-01 01:30\" is ambiguous in timezone America/Denver"
                    .to_owned(),
            ),
        ),
        (
            "2026-11-01 02:00",
            "America/Denver",
            ok("2026-11-01T09:00:00+00:00"),
        ),
        (
            "2026-11-01 00:59",
            "America/New_York",
            ok("2026-11-01T04:59:00+00:00"),
        ),
        (
            "2026-11-01 01:30",
            "America/New_York",
            Err(
                "event date \"2026-11-01 01:30\" is ambiguous in timezone America/New_York"
                    .to_owned(),
            ),
        ),
        (
            "2026-11-01 02:00",
            "America/New_York",
            ok("2026-11-01T07:00:00+00:00"),
        ),
    ];
    for (name, timezone, expected) in cases {
        assert_eq!(
            &parse_with_format(name, "%Y-%m-%d %H:%M", timezone, None),
            expected,
            "{name:?} in {timezone}"
        );
    }
}

#[test]
fn time_only_starts_at_daylight_saving_changes() {
    let cases: &[(&str, &str, &str, Result<String, String>)] = &[
        // The reference is the morning of the change day in each zone.
        (
            "7pm",
            "2026-03-08T15:00:00Z",
            "America/Denver",
            Ok("2026-03-09T01:00:00+00:00".to_owned()),
        ),
        (
            "1am",
            "2026-03-08T07:00:00Z",
            "America/Denver",
            Ok("2026-03-08T08:00:00+00:00".to_owned()),
        ),
        (
            "2:30am",
            "2026-03-08T07:00:00Z",
            "America/Denver",
            Err(
                "event date \"2026-03-08 02:30:00\" does not exist in timezone America/Denver"
                    .to_owned(),
            ),
        ),
        (
            "2:30am",
            "2026-03-08T06:00:00Z",
            "America/New_York",
            Err(
                "event date \"2026-03-08 02:30:00\" does not exist in timezone America/New_York"
                    .to_owned(),
            ),
        ),
        (
            "7pm",
            "2026-03-08T15:00:00Z",
            "America/New_York",
            Ok("2026-03-08T23:00:00+00:00".to_owned()),
        ),
        (
            "1:30am",
            "2026-11-01T06:00:00Z",
            "America/Denver",
            Err(
                "event date \"2026-11-01 01:30:00\" is ambiguous in timezone America/Denver"
                    .to_owned(),
            ),
        ),
        (
            "1:30 AM",
            "2026-11-01T04:00:00Z",
            "America/New_York",
            Err(
                "event date \"2026-11-01 01:30:00\" is ambiguous in timezone America/New_York"
                    .to_owned(),
            ),
        ),
        (
            "7pm",
            "2026-11-01T15:00:00Z",
            "America/Denver",
            Ok("2026-11-02T02:00:00+00:00".to_owned()),
        ),
        (
            "19:30",
            "2026-11-01T15:00:00Z",
            "America/New_York",
            Ok("2026-11-02T00:30:00+00:00".to_owned()),
        ),
        // A start more than 12 hours in the past rolls to the next local day.
        (
            "6am",
            "2026-11-02T03:30:00Z",
            "America/Denver",
            Ok("2026-11-02T13:00:00+00:00".to_owned()),
        ),
        (
            "7pm",
            "2026-11-01T15:00:00Z",
            "Nowhere/City",
            Err("event rule references unknown timezone \"Nowhere/City\"".to_owned()),
        ),
        (
            "25pm",
            "2026-11-01T15:00:00Z",
            "America/Denver",
            Err("event time \"25pm\" is not a valid clock time".to_owned()),
        ),
    ];
    for (time, reference, timezone, expected) in cases {
        let actual = resolve_time_only_start(time, reference.parse().unwrap(), timezone)
            .map(|starts_at| starts_at.to_rfc3339())
            .map_err(|error| error.to_string());
        assert_eq!(&actual, expected, "{time:?} at {reference} in {timezone}");
    }
}

#[test]
fn spelled_date_starts_at_daylight_saving_changes() {
    let cases: &[(&str, &str, &str, &str, &str, Result<String, String>)] = &[
        (
            "Mar",
            "8",
            "2:30am",
            "2026-03-01T12:00:00Z",
            "America/Denver",
            Err(
                "event date \"2026-03-08 02:30:00\" does not exist in timezone America/Denver"
                    .to_owned(),
            ),
        ),
        (
            "march",
            "8",
            "3:30am",
            "2026-03-01T12:00:00Z",
            "America/New_York",
            Ok("2026-03-08T07:30:00+00:00".to_owned()),
        ),
        (
            "Nov",
            "1",
            "1:30am",
            "2026-10-30T12:00:00Z",
            "America/New_York",
            Err(
                "event date \"2026-11-01 01:30:00\" is ambiguous in timezone America/New_York"
                    .to_owned(),
            ),
        ),
        (
            "November",
            "1",
            "7:30pm",
            "2026-10-30T12:00:00Z",
            "America/Denver",
            Ok("2026-11-02T02:30:00+00:00".to_owned()),
        ),
        // The nearest year wins across the new year.
        (
            "Jan",
            "2",
            "7pm",
            "2026-12-30T12:00:00Z",
            "America/Denver",
            Ok("2027-01-03T02:00:00+00:00".to_owned()),
        ),
        (
            "Dec",
            "30",
            "7pm",
            "2027-01-02T12:00:00Z",
            "America/Denver",
            Ok("2026-12-31T02:00:00+00:00".to_owned()),
        ),
        (
            "Feb",
            "30",
            "7pm",
            "2026-02-01T12:00:00Z",
            "America/Denver",
            Err(
                "captured event date \"Feb 30\" does not match format \"month day time\""
                    .to_owned(),
            ),
        ),
    ];
    for (month, day, time, reference, timezone, expected) in cases {
        let actual =
            resolve_spelled_date_start(month, day, time, reference.parse().unwrap(), timezone)
                .map(|starts_at| starts_at.to_rfc3339())
                .map_err(|error| error.to_string());
        assert_eq!(
            &actual, expected,
            "{month} {day} {time} at {reference} in {timezone}"
        );
    }
}
