//! Sports metadata of one generated event programme.
//!
//! The scan builds a [`SportsInfo`] from the parsed stream name and, when the
//! ESPN client finds the event, from the ESPN data. The scan stores the value
//! in `generated_programmes.metadata` under the key `sports`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use iptv_sports::{EspnClient, EventMatch, League, MatchQuery, MatchedTeam, Separator};
use serde::{Deserialize, Serialize};

/// A team of a matched event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SportsTeam {
    pub name: String,
    pub short_name: String,
    pub abbreviation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
}

impl From<&MatchedTeam> for SportsTeam {
    fn from(team: &MatchedTeam) -> Self {
        Self {
            name: team.name.clone(),
            short_name: team.short_name.clone(),
            abbreviation: team.abbreviation.clone(),
            logo_url: team.logo_url.clone(),
        }
    }
}

/// Sports metadata of one event programme.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SportsInfo {
    /// League key, for example `NHL`.
    pub league: String,
    /// Event date in the template timezone, `YYYY-MM-DD`.
    pub local_date: NaiveDate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<SportsTeam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub away: Option<SportsTeam>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broadcasters: Vec<String>,
    /// ESPN start time. The programme keeps the parsed time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub espn_start: Option<DateTime<Utc>>,
    /// True when the parsed and ESPN start times differ by more than 2 hours.
    #[serde(default)]
    pub time_conflict: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

impl SportsInfo {
    /// Returns the information for a known league and no ESPN match.
    #[must_use]
    pub fn league_only(league: League, local_date: NaiveDate) -> Self {
        Self {
            league: league.key().to_owned(),
            local_date,
            event_id: None,
            home: None,
            away: None,
            venue: None,
            broadcasters: Vec::new(),
            espn_start: None,
            time_conflict: false,
            confidence: None,
        }
    }

    fn from_match(league: League, local_date: NaiveDate, found: &EventMatch) -> Self {
        Self {
            event_id: Some(found.event_id.clone()),
            home: Some(SportsTeam::from(&found.home)),
            away: Some(SportsTeam::from(&found.away)),
            venue: found.venue.clone(),
            broadcasters: found.broadcasters.clone(),
            espn_start: Some(found.start_utc),
            time_conflict: found.time_conflict,
            confidence: Some(found.confidence),
            ..Self::league_only(league, local_date)
        }
    }

    /// Returns `<Away full name> at <Home full name>` when ESPN matched.
    #[must_use]
    pub fn matchup(&self) -> Option<String> {
        let (home, away) = self.home.as_ref().zip(self.away.as_ref())?;
        Some(format!("{} at {}", away.name, home.name))
    }

    /// Returns the programme categories.
    #[must_use]
    pub fn categories(&self) -> Vec<String> {
        vec![
            "Sports".to_owned(),
            "Sports event".to_owned(),
            self.league.clone(),
        ]
    }

    /// Returns the programme description: matchup, league, venue, broadcasters.
    #[must_use]
    pub fn description(&self, fallback_title: &str) -> String {
        let mut parts = vec![
            self.matchup().unwrap_or_else(|| fallback_title.to_owned()),
            format!("League: {}", self.league),
        ];
        if let Some(venue) = &self.venue {
            parts.push(format!("Venue: {venue}"));
        }
        if !self.broadcasters.is_empty() {
            parts.push(format!("Broadcast: {}", self.broadcasters.join(", ")));
        }
        let mut text = parts.join(". ");
        text.push('.');
        text
    }

    /// Returns the home-team logo URL.
    #[must_use]
    pub fn icon_url(&self) -> Option<&str> {
        self.home.as_ref()?.logo_url.as_deref()
    }
}

/// Returns the league of an event from its captures or its group name.
#[must_use]
pub fn detect_league(captures: &BTreeMap<String, String>, group: Option<&str>) -> Option<League> {
    captures
        .get("league")
        .map(String::as_str)
        .into_iter()
        .chain(group)
        .find_map(League::from_key)
}

/// Builds the ESPN query from the team captures.
///
/// `away @ home` gives the separator `At`. `A vs B` has no fixed order.
#[must_use]
pub fn match_query(
    captures: &BTreeMap<String, String>,
    parsed_start: Option<DateTime<Utc>>,
) -> Option<MatchQuery> {
    let (first, second, separator) = match (
        captures.get("away"),
        captures.get("home"),
        captures.get("team_a"),
        captures.get("team_b"),
    ) {
        (Some(away), Some(home), _, _) => (away, home, Separator::At),
        (_, _, Some(first), Some(second)) => (first, second, Separator::Vs),
        _ => return None,
    };
    Some(MatchQuery {
        first_team: first.clone(),
        second_team: second.clone(),
        separator,
        parsed_start,
    })
}

/// Looks up one event. An ESPN error gives the league-only information.
pub async fn lookup_sports_info(
    client: &EspnClient,
    league: League,
    local_date: NaiveDate,
    captures: &BTreeMap<String, String>,
    parsed_start: DateTime<Utc>,
) -> SportsInfo {
    let Some(query) = match_query(captures, Some(parsed_start)) else {
        return SportsInfo::league_only(league, local_date);
    };
    match client.lookup(league, local_date, &query).await {
        Ok(Some(found)) => SportsInfo::from_match(league, local_date, &found),
        Ok(None) => SportsInfo::league_only(league, local_date),
        Err(error) => {
            tracing::warn!(%error, league = league.key(), "ESPN lookup failed; the scan uses parsed values");
            SportsInfo::league_only(league, local_date)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captures(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn at_captures_make_an_away_first_query() {
        let query = match_query(&captures(&[("away", "Senators"), ("home", "Sabres")]), None)
            .expect("query");
        assert_eq!(query.first_team, "Senators");
        assert_eq!(query.separator, Separator::At);
    }

    #[test]
    fn vs_captures_keep_the_source_order() {
        let query = match_query(
            &captures(&[("team_a", "Kings"), ("team_b", "Sharks")]),
            None,
        )
        .expect("query");
        assert_eq!(query.first_team, "Kings");
        assert_eq!(query.separator, Separator::Vs);
        assert!(match_query(&captures(&[("league", "NHL")]), None).is_none());
    }

    #[test]
    fn league_comes_from_the_capture_then_the_group() {
        assert_eq!(
            detect_league(&captures(&[("league", "NHL")]), None),
            Some(League::Nhl)
        );
        assert_eq!(
            detect_league(&captures(&[("league", "Darts")]), Some("NBA")),
            Some(League::Nba)
        );
        assert_eq!(detect_league(&captures(&[]), Some("Darts")), None);
    }
}
