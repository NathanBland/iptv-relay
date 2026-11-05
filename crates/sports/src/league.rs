//! Static table from league keys to ESPN `<sport>/<league>` paths.

/// A league that ESPN covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum League {
    Nhl,
    Nfl,
    Nba,
    Mlb,
    Ncaaf,
    Ncaam,
    Ncaaw,
    Wnba,
    Mls,
}

impl League {
    /// Every supported league.
    pub const ALL: [League; 9] = [
        League::Nhl,
        League::Nfl,
        League::Nba,
        League::Mlb,
        League::Ncaaf,
        League::Ncaam,
        League::Ncaaw,
        League::Wnba,
        League::Mls,
    ];

    /// The upper-case league key, for example `NHL`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            League::Nhl => "NHL",
            League::Nfl => "NFL",
            League::Nba => "NBA",
            League::Mlb => "MLB",
            League::Ncaaf => "NCAAF",
            League::Ncaam => "NCAAM",
            League::Ncaaw => "NCAAW",
            League::Wnba => "WNBA",
            League::Mls => "MLS",
        }
    }

    /// The ESPN `<sport>/<league>` path, for example `hockey/nhl`.
    #[must_use]
    pub fn espn_path(self) -> &'static str {
        match self {
            League::Nhl => "hockey/nhl",
            League::Nfl => "football/nfl",
            League::Nba => "basketball/nba",
            League::Mlb => "baseball/mlb",
            League::Ncaaf => "football/college-football",
            League::Ncaam => "basketball/mens-college-basketball",
            League::Ncaaw => "basketball/womens-college-basketball",
            League::Wnba => "basketball/wnba",
            League::Mls => "soccer/usa.1",
        }
    }

    /// Parse a league key or common name. Case and punctuation do not matter.
    ///
    /// Returns `None` for a league that ESPN does not cover here.
    #[must_use]
    pub fn from_key(text: &str) -> Option<League> {
        let norm: String = text
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        match norm.as_str() {
            "nhl" => Some(League::Nhl),
            "nfl" => Some(League::Nfl),
            "nba" => Some(League::Nba),
            "mlb" => Some(League::Mlb),
            "ncaaf" | "ncaafootball" | "collegefootball" => Some(League::Ncaaf),
            "ncaam"
            | "ncaabasketball"
            | "ncaamensbasketball"
            | "mensncaabasketball"
            | "menscollegebasketball" => Some(League::Ncaam),
            "ncaaw" | "ncaawomensbasketball" | "womenscollegebasketball" => Some(League::Ncaaw),
            "wnba" => Some(League::Wnba),
            "mls" | "majorleaguesoccer" => Some(League::Mls),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::League;

    #[test]
    fn paths_match_table() {
        assert_eq!(League::Nhl.espn_path(), "hockey/nhl");
        assert_eq!(League::Ncaaf.espn_path(), "football/college-football");
        assert_eq!(
            League::Ncaaw.espn_path(),
            "basketball/womens-college-basketball"
        );
    }

    #[test]
    fn key_round_trips() {
        for league in League::ALL {
            assert_eq!(League::from_key(league.key()), Some(league));
        }
    }

    #[test]
    fn names_and_unknowns() {
        assert_eq!(League::from_key("NCAA Football"), Some(League::Ncaaf));
        assert_eq!(
            League::from_key("NCAA Women's Basketball"),
            Some(League::Ncaaw)
        );
        assert_eq!(League::from_key("nhl"), Some(League::Nhl));
        assert_eq!(League::from_key("OHL"), None);
    }
}
