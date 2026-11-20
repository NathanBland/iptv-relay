//! Tests that lock the time behavior of ESPN date parsing.
//!
//! The chrono to jiff migration must keep each expected value unchanged.

use iptv_sports::model::parse_espn_date;

#[test]
fn espn_dates_parse_to_the_same_instant() {
    let cases = [
        ("2026-11-05T00:30Z", Some("2026-11-05T00:30:00+00:00")),
        ("2026-11-05T00:30:00Z", Some("2026-11-05T00:30:00+00:00")),
        (
            "2026-11-05T00:30:00.5Z",
            Some("2026-11-05T00:30:00.500+00:00"),
        ),
        (
            "2026-11-04T19:30:00-05:00",
            Some("2026-11-05T00:30:00+00:00"),
        ),
        ("2026-03-08T02:30Z", Some("2026-03-08T02:30:00+00:00")),
        ("2026-11-05 00:30", None),
        ("2026-11-05T00:30", None),
        ("tomorrow", None),
    ];
    for (text, expected) in cases {
        assert_eq!(
            parse_espn_date(text).map(|value| value.to_rfc3339()),
            expected.map(str::to_owned),
            "{text}"
        );
    }
}
