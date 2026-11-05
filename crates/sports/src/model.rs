//! Parsed ESPN data. Parsing is tolerant: unknown fields are ignored.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;

/// A team as seen in a scoreboard or team list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspnTeam {
    pub id: String,
    pub display_name: String,
    pub short_display_name: String,
    pub location: String,
    pub name: String,
    pub abbreviation: String,
    pub logo_url: Option<String>,
}

/// One scheduled game from a scoreboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EspnEvent {
    pub id: String,
    pub name: String,
    pub start_utc: DateTime<Utc>,
    pub home: EspnTeam,
    pub away: EspnTeam,
    pub venue: Option<String>,
    pub broadcasters: Vec<String>,
}

#[derive(Deserialize)]
struct RawScoreboard {
    #[serde(default)]
    events: Vec<RawEvent>,
}

#[derive(Deserialize)]
struct RawEvent {
    id: String,
    #[serde(default)]
    name: String,
    date: String,
    #[serde(default)]
    competitions: Vec<RawCompetition>,
}

#[derive(Deserialize)]
struct RawCompetition {
    #[serde(default)]
    venue: Option<RawVenue>,
    #[serde(default)]
    broadcasts: Vec<RawBroadcast>,
    #[serde(default)]
    competitors: Vec<RawCompetitor>,
}

#[derive(Deserialize)]
struct RawVenue {
    #[serde(default, rename = "fullName")]
    full_name: Option<String>,
}

#[derive(Deserialize)]
struct RawBroadcast {
    #[serde(default)]
    names: Vec<String>,
}

#[derive(Deserialize)]
struct RawCompetitor {
    #[serde(default, rename = "homeAway")]
    home_away: String,
    team: RawTeam,
}

#[derive(Deserialize)]
struct RawTeam {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default, rename = "shortDisplayName")]
    short_display_name: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    abbreviation: String,
    #[serde(default)]
    logo: Option<String>,
    #[serde(default)]
    logos: Vec<RawLogo>,
}

#[derive(Deserialize)]
struct RawLogo {
    href: String,
    #[serde(default)]
    rel: Vec<String>,
}

#[derive(Deserialize)]
struct RawTeams {
    #[serde(default)]
    sports: Vec<RawSport>,
}

#[derive(Deserialize)]
struct RawSport {
    #[serde(default)]
    leagues: Vec<RawLeague>,
}

#[derive(Deserialize)]
struct RawLeague {
    #[serde(default)]
    teams: Vec<RawTeamEntry>,
}

#[derive(Deserialize)]
struct RawTeamEntry {
    team: RawTeam,
}

impl RawTeam {
    fn into_team(self) -> EspnTeam {
        let logo_url = self.logo.or_else(|| {
            let default = self
                .logos
                .iter()
                .find(|l| l.rel.iter().any(|r| r == "default"))
                .or_else(|| self.logos.first());
            default.map(|l| l.href.clone())
        });
        EspnTeam {
            id: self.id,
            display_name: self.display_name,
            short_display_name: self.short_display_name,
            location: self.location,
            name: self.name,
            abbreviation: self.abbreviation,
            logo_url,
        }
    }
}

/// Parse an ESPN date such as `2026-11-05T00:30Z` or an RFC 3339 date.
#[must_use]
pub fn parse_espn_date(text: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%MZ")
        .ok()
        .map(|n| n.and_utc())
}

/// Parse a scoreboard body. An event without two competitors or a valid date is skipped.
///
/// # Errors
///
/// Returns an error when the body is not valid scoreboard JSON.
pub fn parse_scoreboard(body: &[u8]) -> Result<Vec<EspnEvent>, serde_json::Error> {
    let raw: RawScoreboard = serde_json::from_slice(body)?;
    let mut out = Vec::new();
    for event in raw.events {
        let Some(start_utc) = parse_espn_date(&event.date) else {
            continue;
        };
        let Some(comp) = event.competitions.into_iter().next() else {
            continue;
        };
        let venue = comp.venue.and_then(|v| v.full_name);
        let mut broadcasters: Vec<String> = Vec::new();
        for name in comp.broadcasts.into_iter().flat_map(|b| b.names) {
            if !broadcasters.contains(&name) {
                broadcasters.push(name);
            }
        }
        let (mut home, mut away) = (None, None);
        for c in comp.competitors {
            match c.home_away.as_str() {
                "home" => home = Some(c.team.into_team()),
                "away" => away = Some(c.team.into_team()),
                _ => {}
            }
        }
        let (Some(home), Some(away)) = (home, away) else {
            continue;
        };
        out.push(EspnEvent {
            id: event.id,
            name: event.name,
            start_utc,
            home,
            away,
            venue,
            broadcasters,
        });
    }
    Ok(out)
}

/// Parse a team list body.
///
/// # Errors
///
/// Returns an error when the body is not valid team-list JSON.
pub fn parse_teams(body: &[u8]) -> Result<Vec<EspnTeam>, serde_json::Error> {
    let raw: RawTeams = serde_json::from_slice(body)?;
    Ok(raw
        .sports
        .into_iter()
        .flat_map(|s| s.leagues)
        .flat_map(|l| l.teams)
        .map(|t| t.team.into_team())
        .collect())
}
