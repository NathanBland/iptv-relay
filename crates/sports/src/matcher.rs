//! Pure matcher from two team strings to an ESPN event. No I/O.

use chrono::{DateTime, Duration, Utc};

use crate::model::{EspnEvent, EspnTeam};

/// Maximum difference between the parsed time and the ESPN time before a conflict is set.
pub const CONFLICT_THRESHOLD: Duration = Duration::hours(2);

/// Minimum score for one team to count as a match.
const MIN_TEAM_SCORE: f32 = 0.7;

/// The word between the two team strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    /// `@` or `at`: the first team is away and the second team is home.
    At,
    /// `vs`: the order has no fixed meaning.
    Vs,
}

impl Separator {
    /// Parse `@`, `at`, `vs`, `vs.`, or `v` (case-insensitive).
    #[must_use]
    pub fn parse(text: &str) -> Option<Separator> {
        match text.trim().to_ascii_lowercase().as_str() {
            "@" | "at" => Some(Separator::At),
            "vs" | "vs." | "v" | "v." => Some(Separator::Vs),
            _ => None,
        }
    }
}

/// Input for [`match_event`].
#[derive(Debug, Clone)]
pub struct MatchQuery {
    pub first_team: String,
    pub second_team: String,
    pub separator: Separator,
    /// Start time from the channel name, already converted to UTC.
    pub parsed_start: Option<DateTime<Utc>>,
}

/// A team in a match result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedTeam {
    pub name: String,
    pub short_name: String,
    pub abbreviation: String,
    pub logo_url: Option<String>,
}

impl From<&EspnTeam> for MatchedTeam {
    fn from(t: &EspnTeam) -> Self {
        MatchedTeam {
            name: t.display_name.clone(),
            short_name: t.short_display_name.clone(),
            abbreviation: t.abbreviation.clone(),
            logo_url: t.logo_url.clone(),
        }
    }
}

/// Output of [`match_event`].
#[derive(Debug, Clone, PartialEq)]
pub struct EventMatch {
    pub event_id: String,
    /// The ESPN start time. It always wins over the parsed time.
    pub start_utc: DateTime<Utc>,
    pub home: MatchedTeam,
    pub away: MatchedTeam,
    pub venue: Option<String>,
    pub broadcasters: Vec<String>,
    /// From 0.7 to 1.0.
    pub confidence: f32,
    /// True when the parsed time and the ESPN time differ by more than 2 hours.
    pub time_conflict: bool,
    /// True when the first team in the query is the ESPN home team.
    /// For `At` this means the source order disagrees with ESPN.
    pub first_team_is_home: bool,
}

/// Normalize a team string: lower case, ASCII letters and digits, single spaces.
#[must_use]
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for ch in text.chars() {
        if ch == '&' {
            if !space {
                out.push(' ');
            }
            out.push_str("and ");
            space = true;
        } else if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
            space = false;
        } else if ch != '\'' && ch != '\u{2019}' && ch != '.' && !space {
            out.push(' ');
            space = true;
        }
    }
    out.trim().to_string()
}

fn contains_words(haystack: &str, needle: &str) -> bool {
    !needle.is_empty() && format!(" {haystack} ").contains(&format!(" {needle} "))
}

/// Score how well `text` names `team`. Returns 0.0 for no match.
#[must_use]
pub fn score_team(text: &str, team: &EspnTeam) -> f32 {
    let q = normalize(text);
    if q.is_empty() {
        return 0.0;
    }
    let display = normalize(&team.display_name);
    let short = normalize(&team.short_display_name);
    let name = normalize(&team.name);
    let location = normalize(&team.location);
    let abbr = normalize(&team.abbreviation);

    if q == display {
        return 1.0;
    }
    if q == short || q == name || (q == abbr && !abbr.is_empty()) {
        return 0.95;
    }
    if q == location {
        return 0.85;
    }
    // The query has extra words, for example `Buffalo Sabres (Home)`.
    let mut best = 0.0_f32;
    for (cand, score) in [(&display, 0.8), (&short, 0.75), (&name, 0.75)] {
        if cand.split(' ').count() >= 1 && contains_words(&q, cand) {
            best = best.max(score);
        }
    }
    best
}

/// Find a team in a team list. Returns the best team with its score of at least 0.7.
#[must_use]
pub fn find_team<'a>(teams: &'a [EspnTeam], text: &str) -> Option<(&'a EspnTeam, f32)> {
    teams
        .iter()
        .map(|t| (t, score_team(text, t)))
        .filter(|(_, s)| *s >= MIN_TEAM_SCORE)
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Find the event that matches the query. Pure function.
///
/// An event matches when each query team matches a different ESPN team.
/// With more than one match, the highest confidence wins. A tie goes to the event
/// closest to the parsed time, then to the earliest event.
#[must_use]
pub fn match_event(query: &MatchQuery, events: &[EspnEvent]) -> Option<EventMatch> {
    let mut best: Option<(EventMatch, Duration)> = None;
    for event in events {
        let Some((conf, first_is_home)) = score_event(query, event) else {
            continue;
        };
        let time_conflict = query
            .parsed_start
            .is_some_and(|p| (p - event.start_utc).abs() > CONFLICT_THRESHOLD);
        let distance = query
            .parsed_start
            .map_or(Duration::zero(), |p| (p - event.start_utc).abs());
        let candidate = EventMatch {
            event_id: event.id.clone(),
            start_utc: event.start_utc,
            home: MatchedTeam::from(&event.home),
            away: MatchedTeam::from(&event.away),
            venue: event.venue.clone(),
            broadcasters: event.broadcasters.clone(),
            confidence: conf,
            time_conflict,
            first_team_is_home: first_is_home,
        };
        let better = match &best {
            None => true,
            Some((cur, cur_dist)) => {
                if (candidate.confidence - cur.confidence).abs() > f32::EPSILON {
                    candidate.confidence > cur.confidence
                } else if query.parsed_start.is_some() && distance != *cur_dist {
                    distance < *cur_dist
                } else {
                    candidate.start_utc < cur.start_utc
                }
            }
        };
        if better {
            best = Some((candidate, distance));
        }
    }
    best.map(|(m, _)| m)
}

/// Returns the confidence and whether the first query team is the home team.
fn score_event(query: &MatchQuery, event: &EspnEvent) -> Option<(f32, bool)> {
    let first_home = score_team(&query.first_team, &event.home);
    let second_away = score_team(&query.second_team, &event.away);
    let first_away = score_team(&query.first_team, &event.away);
    let second_home = score_team(&query.second_team, &event.home);

    let straight = (first_away.min(second_home), false);
    let swapped = (first_home.min(second_away), true);
    let (score, first_is_home) = if query.separator == Separator::At && straight.0 >= swapped.0 {
        straight
    } else if query.separator == Separator::At {
        swapped
    } else if straight.0 >= swapped.0 {
        straight
    } else {
        swapped
    };
    // A swapped `at` match is real but less sure, because the source order is odd.
    if score < MIN_TEAM_SCORE {
        return None;
    }
    let score = if query.separator == Separator::At && first_is_home {
        score * 0.9
    } else {
        score
    };
    Some((score, first_is_home))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_scoreboard;
    use chrono::TimeZone;

    fn events() -> Vec<EspnEvent> {
        let mut all = parse_scoreboard(include_bytes!(
            "../tests/fixtures/nhl_scoreboard_20261104.json"
        ))
        .expect("fixture parses");
        all.extend(
            parse_scoreboard(include_bytes!(
                "../tests/fixtures/nhl_scoreboard_20261105.json"
            ))
            .expect("fixture parses"),
        );
        all
    }

    fn q(a: &str, sep: Separator, b: &str, start: Option<DateTime<Utc>>) -> MatchQuery {
        MatchQuery {
            first_team: a.into(),
            second_team: b.into(),
            separator: sep,
            parsed_start: start,
        }
    }

    #[test]
    fn senators_at_sabres() {
        let m =
            match_event(&q("Senators", Separator::At, "Sabres", None), &events()).expect("match");
        assert_eq!(m.home.abbreviation, "BUF");
        assert_eq!(m.away.abbreviation, "OTT");
        assert_eq!(
            m.start_utc,
            Utc.with_ymd_and_hms(2026, 11, 5, 0, 30, 0).unwrap()
        );
        assert_eq!(m.venue.as_deref(), Some("KeyBank Center"));
        assert_eq!(m.broadcasters, vec!["ESPN+", "MSGB"]);
        assert_eq!(
            m.home.logo_url.as_deref(),
            Some("https://a.espncdn.com/i/teamlogos/nhl/500/scoreboard/buf.png")
        );
        assert!(!m.time_conflict);
        assert!(!m.first_team_is_home);
        assert!(m.confidence >= 0.9);
    }

    #[test]
    fn full_names_and_abbreviations() {
        let m = match_event(
            &q("Ottawa Senators", Separator::At, "Buffalo Sabres", None),
            &events(),
        )
        .expect("match");
        assert_eq!(m.event_id, "401892644");
        assert!((m.confidence - 1.0).abs() < f32::EPSILON);
        let m = match_event(&q("OTT", Separator::At, "BUF", None), &events()).expect("match");
        assert_eq!(m.event_id, "401892644");
    }

    #[test]
    fn location_only_matches_with_lower_confidence() {
        let m =
            match_event(&q("Ottawa", Separator::At, "Buffalo", None), &events()).expect("match");
        assert_eq!(m.event_id, "401892644");
        assert!(m.confidence < 0.9);
    }

    #[test]
    fn vs_uses_espn_home_away() {
        let m =
            match_event(&q("Sabres", Separator::Vs, "Senators", None), &events()).expect("match");
        assert_eq!(m.home.abbreviation, "BUF");
        assert!(m.first_team_is_home);
    }

    #[test]
    fn matching_time_is_not_a_conflict() {
        let p = Utc.with_ymd_and_hms(2026, 11, 5, 0, 30, 0).unwrap();
        let m = match_event(&q("Senators", Separator::At, "Sabres", Some(p)), &events()).unwrap();
        assert!(!m.time_conflict);
    }

    #[test]
    fn wrong_time_sets_conflict_and_keeps_espn_time() {
        // The provider time was read as UTC: 19:30Z instead of 00:30Z the next day.
        let p = Utc.with_ymd_and_hms(2026, 11, 4, 19, 30, 0).unwrap();
        let m = match_event(&q("Senators", Separator::At, "Sabres", Some(p)), &events()).unwrap();
        assert!(m.time_conflict);
        assert_eq!(
            m.start_utc,
            Utc.with_ymd_and_hms(2026, 11, 5, 0, 30, 0).unwrap()
        );
    }

    #[test]
    fn no_match_for_unknown_or_single_team() {
        assert!(match_event(&q("Senators", Separator::At, "Nowhere", None), &events()).is_none());
        assert!(match_event(&q("Foo", Separator::At, "Bar", None), &events()).is_none());
        assert!(match_event(&q("Senators", Separator::At, "Senators", None), &events()).is_none());
    }

    #[test]
    fn swapped_at_order_matches_with_flag() {
        let m = match_event(&q("Sabres", Separator::At, "Senators", None), &events()).unwrap();
        assert!(m.first_team_is_home);
        assert!(m.confidence < 0.9);
    }

    #[test]
    fn normalize_handles_punctuation() {
        assert_eq!(normalize("St. Louis Blues"), "st louis blues");
        assert_eq!(
            normalize("  Texas A&M "),
            "texas aand m".replace("aand", "a and")
        );
    }

    #[test]
    fn separator_parse() {
        assert_eq!(Separator::parse("@"), Some(Separator::At));
        assert_eq!(Separator::parse("AT"), Some(Separator::At));
        assert_eq!(Separator::parse("vs."), Some(Separator::Vs));
        assert_eq!(Separator::parse("x"), None);
    }
}
