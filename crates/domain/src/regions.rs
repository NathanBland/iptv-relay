//! Region prefix mapping for channel group filtering.
//!
//! Provider M3U data uses region prefixes in group names (e.g., "US: NEWS",
//! "UK: SKY SPORTS", "AF: AFRICAN SERIES"). This module maps IANA timezones
//! to the region prefixes that are most relevant for that timezone.

/// All known region prefixes found in provider M3U group names.
pub const REGION_PREFIXES: &[&str] = &[
    "US", "USA", "CAN", "EN", "UK", "AU", "NZ", "LA", "BR", "ESP", "PT", "IT", "FR", "QC", "DE",
    "AT", "CH", "NL", "BE", "SE", "DK", "NO", "FI", "SCAN", "IS", "PL", "GR", "BG", "RO", "HU",
    "CZ", "SK", "SL", "AL", "RS", "HR", "MK", "BH", "KO", "EXYU", "RU", "BY", "UKR", "LT", "LV",
    "EE", "EST", "TR", "IR", "ARA", "AF", "AFRICAN", "AFG", "IN", "PK", "LK", "BAN", "PH", "ID",
    "TH", "VT", "MY", "SG", "HK", "CN", "JP", "SK", "TW", "KA", "GLOBAL", "MULTI", "EU", "SPT",
    "MU",
];

/// Returns the suggested region prefixes for a given IANA timezone.
///
/// The mapping prioritizes content that is most relevant for the timezone's
/// region. International prefixes (GLOBAL, MULTI, SPT) are always included.
///
/// # Arguments
///
/// * `timezone` - An IANA timezone identifier (e.g., "America/Denver").
///
/// # Returns
///
/// A vector of region prefix strings that are suggested for the timezone.
#[must_use]
pub fn suggested_prefixes_for_timezone(timezone: &str) -> Vec<&'static str> {
    let region = timezone.split('/').next().unwrap_or("Global");

    let regional: &[&str] = match timezone {
        // North America
        "America/New_York"
        | "America/Denver"
        | "America/Chicago"
        | "America/Los_Angeles"
        | "America/Phoenix"
        | "America/Detroit"
        | "America/Toronto"
        | "America/Vancouver"
        | "America/Edmonton"
        | "America/Halifax"
        | "America/Winnipeg"
        | "America/Saskatoon"
        | "US/Eastern"
        | "US/Central"
        | "US/Mountain"
        | "US/Pacific"
        | "US/Arizona"
        | "Canada/Eastern"
        | "Canada/Central"
        | "Canada/Mountain"
        | "Canada/Pacific" => &["US", "USA", "CAN", "EN", "LA"],

        // Latin America
        "America/Mexico_City"
        | "America/Bogota"
        | "America/Lima"
        | "America/Buenos_Aires"
        | "America/Santiago"
        | "America/Sao_Paulo"
        | "America/Caracas"
        | "America/Guatemala"
        | "America/Havana" => &["LA", "BR", "ES", "PT"],

        // UK and Ireland
        "Europe/London" | "Europe/Dublin" | "GMT" | "Etc/GMT" | "Atlantic/Reykjavik" => {
            &["UK", "EN"]
        }

        // Western Europe
        "Europe/Paris" | "Europe/Brussels" | "Europe/Amsterdam" | "Europe/Luxembourg"
        | "Europe/Madrid" | "Europe/Lisbon" | "CET" => &["FR", "QC", "ESP", "PT", "NL", "BE", "EN"],

        // Central Europe
        "Europe/Berlin" | "Europe/Vienna" | "Europe/Zurich" | "Europe/Rome" | "Europe/Athens"
        | "Europe/Malta" => &["DE", "AT", "CH", "IT", "GR", "EN"],

        // Northern Europe / Scandinavia
        "Europe/Stockholm" | "Europe/Copenhagen" | "Europe/Oslo" | "Europe/Helsinki"
        | "Europe/Tallinn" => &["SE", "DK", "NO", "FI", "SCAN", "EN"],

        // Eastern Europe
        "Europe/Warsaw" | "Europe/Budapest" | "Europe/Prague" | "Europe/Sofia"
        | "Europe/Bucharest" | "Europe/Bratislava" | "Europe/Ljubljana" | "Europe/Zagreb"
        | "Europe/Belgrade" | "Europe/Skopje" | "Europe/Sarajevo" | "Europe/Podgorica"
        | "Europe/Tirane" | "Europe/Vilnius" | "Europe/Riga" | "Europe/Kiev" => &[
            "PL", "CZ", "SK", "HU", "BG", "RO", "SL", "EXYU", "AL", "LT", "LV", "UKR",
        ],

        // Russia and neighbors
        "Europe/Moscow" | "Europe/Kaliningrad" | "Europe/Minsk" | "Asia/Yekaterinburg"
        | "Asia/Novosibirsk" | "Asia/Vladivostok" => &["RU", "BY", "UKR"],

        // Middle East
        "Asia/Jerusalem" | "Asia/Riyadh" | "Asia/Dubai" | "Asia/Qatar" | "Asia/Kuwait"
        | "Asia/Bahrain" | "Asia/Baghdad" | "Asia/Beirut" | "Asia/Damascus" | "Asia/Amman"
        | "Africa/Cairo" => &["ARA", "IS", "TR"],

        // Turkey
        "Europe/Istanbul" => &["TR", "ARA", "EN"],

        // Iran
        "Asia/Tehran" => &["IR", "TR", "ARA"],

        // South Asia
        "Asia/Kolkata" | "Asia/Karachi" | "Asia/Dhaka" | "Asia/Colombo" | "Asia/Kathmandu" => {
            &["IN", "PK", "BAN", "LK", "EN"]
        }

        // Southeast Asia
        "Asia/Bangkok" | "Asia/Jakarta" | "Asia/Manila" | "Asia/Singapore"
        | "Asia/Kuala_Lumpur" | "Asia/Ho_Chi_Minh" => &["TH", "ID", "PH", "MY", "SG", "VT", "EN"],

        // East Asia
        "Asia/Tokyo" | "Asia/Seoul" | "Asia/Shanghai" | "Asia/Hong_Kong" | "Asia/Taipei"
        | "Asia/Ulaanbaatar" => &["JP", "SK", "CN", "HK", "TW", "EN"],

        // Central Asia
        "Asia/Almaty" | "Asia/Tashkent" | "Asia/Baku" | "Asia/Yerevan" | "Asia/Tbilisi" => {
            &["KA", "RU", "EN"]
        }

        // Africa
        "Africa/Johannesburg"
        | "Africa/Lagos"
        | "Africa/Nairobi"
        | "Africa/Casablanca"
        | "Africa/Accra"
        | "Africa/Addis_Ababa"
        | "Africa/Algiers"
        | "Africa/Tunis"
        | "Africa/Dakar" => &["AF", "AFRICAN", "ARA", "FR", "EN"],

        // Oceania
        "Australia/Sydney"
        | "Australia/Melbourne"
        | "Australia/Brisbane"
        | "Australia/Perth"
        | "Pacific/Auckland"
        | "Pacific/Fiji" => &["AU", "NZ", "EN"],

        // Fallback: include the broadest set of English/international prefixes
        _ => &["US", "USA", "EN", "UK", "GLOBAL"],
    };

    let _ = region; // Suppress unused variable warning for the split result.

    let mut result: Vec<&'static str> = regional.to_vec();
    // Always include international prefixes.
    for prefix in &["GLOBAL", "MULTI", "SPT"] {
        if !result.contains(prefix) {
            result.push(prefix);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn america_denver_suggests_us_prefixes() {
        let suggested = suggested_prefixes_for_timezone("America/Denver");
        assert!(suggested.contains(&"US"));
        assert!(suggested.contains(&"USA"));
        assert!(suggested.contains(&"CAN"));
        assert!(suggested.contains(&"EN"));
        assert!(suggested.contains(&"LA"));
        assert!(suggested.contains(&"GLOBAL"));
        assert!(suggested.contains(&"MULTI"));
        assert!(suggested.contains(&"SPT"));
        // Should not suggest African or Arabic content for Denver.
        assert!(!suggested.contains(&"AF"));
        assert!(!suggested.contains(&"ARA"));
        assert!(!suggested.contains(&"IR"));
    }

    #[test]
    fn europe_london_suggests_uk_prefixes() {
        let suggested = suggested_prefixes_for_timezone("Europe/London");
        assert!(suggested.contains(&"UK"));
        assert!(suggested.contains(&"EN"));
        assert!(suggested.contains(&"GLOBAL"));
        // Should not suggest US-specific prefixes.
        assert!(!suggested.contains(&"USA"));
        assert!(!suggested.contains(&"LA"));
    }

    #[test]
    fn africa_johannesburg_suggests_african_prefixes() {
        let suggested = suggested_prefixes_for_timezone("Africa/Johannesburg");
        assert!(suggested.contains(&"AF"));
        assert!(suggested.contains(&"AFRICAN"));
        assert!(suggested.contains(&"EN"));
        assert!(suggested.contains(&"GLOBAL"));
    }

    #[test]
    fn unknown_timezone_falls_back_to_international() {
        let suggested = suggested_prefixes_for_timezone("Antarctica/South_Pole");
        assert!(suggested.contains(&"US"));
        assert!(suggested.contains(&"EN"));
        assert!(suggested.contains(&"GLOBAL"));
    }

    #[test]
    fn all_prefixes_are_uppercase() {
        for prefix in REGION_PREFIXES {
            assert!(
                prefix.chars().all(|c| c.is_ascii_uppercase()),
                "prefix {prefix} must be uppercase"
            );
        }
    }
}
