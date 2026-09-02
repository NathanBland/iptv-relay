//! Deterministic scale-test fixtures and budget gates.
//!
//! This module is gated behind the `scale` cargo feature. It adds no new
//! dependencies. The generators use a fixed-seed pseudo-random sequence so
//! every run produces byte-identical output for the same configuration. The
//! pinned-runner scale gate writes the generated artifacts to a temporary
//! directory and never commits them.
//!
//! # Determinism
//!
//! [`Rng`] implements `SplitMix64`. The same seed yields the same sequence on
//! every supported target. Generators write only ASCII bytes, so output does
//! not depend on platform locale or line endings.

use std::io::Write;

use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Fixed-seed `SplitMix64` pseudo-random generator.
///
/// The algorithm is deterministic and portable. It is not a cryptographic
/// primitive. Use it only for deterministic fixture generation.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Creates a new generator from a 64-bit seed.
    ///
    /// The seed is mixed once so that small seed differences spread entropy
    /// across the first output.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(0x9E37_79B9_7F4A_7C15),
        }
    }

    /// Returns the next 64-bit pseudo-random value.
    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        let mut z = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.state = z;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Returns the next 32-bit pseudo-random value.
    #[must_use]
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Returns a value in the inclusive range `low..=high`.
    ///
    /// `high` must be greater than or equal to `low`.
    #[must_use]
    pub fn between(&mut self, low: u32, high: u32) -> u32 {
        let span = high.saturating_sub(low).saturating_add(1).max(1);
        low + (self.next_u32() % span)
    }
}

/// Configuration for the deterministic M3U fixture generator.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ScaleM3uConfig {
    pub entries: usize,
    pub seed: u64,
}

impl ScaleM3uConfig {
    /// The task workload: 1,160,000 entries.
    pub const WORKLOAD_ENTRIES: usize = 1_160_000;

    /// The default fixed seed.
    pub const DEFAULT_SEED: u64 = 0x4950_5456_5343_414c;

    #[must_use]
    pub const fn workload() -> Self {
        Self {
            entries: Self::WORKLOAD_ENTRIES,
            seed: Self::DEFAULT_SEED,
        }
    }
}

/// Configuration for the deterministic XMLTV fixture generator.
///
/// `programmes` are distributed round-robin across `channels` channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ScaleXmltvConfig {
    pub channels: usize,
    pub programmes: usize,
    pub seed: u64,
}

impl ScaleXmltvConfig {
    /// The task workload: 2,750 channels and 275,000 programmes.
    pub const WORKLOAD_CHANNELS: usize = 2_750;
    pub const WORKLOAD_PROGRAMMES: usize = 275_000;

    /// The default fixed seed.
    pub const DEFAULT_SEED: u64 = 0x584d_4c54_5653_4341;

    #[must_use]
    pub const fn workload() -> Self {
        Self {
            channels: Self::WORKLOAD_CHANNELS,
            programmes: Self::WORKLOAD_PROGRAMMES,
            seed: Self::DEFAULT_SEED,
        }
    }
}

/// Writes a deterministic extended M3U playlist to `writer`.
///
/// The playlist begins with `#EXTM3U`. Each entry has a stable `tvg-id`,
/// `tvg-name`, `tvg-chno`, `tvg-logo`, `group-title`, and a unique stream URL.
///
/// # Errors
///
/// Returns an error when `writer` fails.
pub fn generate_m3u<W: Write>(config: ScaleM3uConfig, mut writer: W) -> std::io::Result<()> {
    writeln!(writer, "#EXTM3U")?;
    let mut rng = Rng::new(config.seed);
    for index in 0..config.entries {
        let n = u32::try_from(index).unwrap_or(u32::MAX);
        let group = rng.between(0, 999);
        let quality = match rng.next_u32() % 4 {
            0 => "FHD",
            1 => "HD",
            2 => "SD",
            _ => "4K",
        };
        let chno_major = (n % 999) + 1;
        let chno_minor = (rng.next_u32() % 9) + 1;
        let tvg_id = format!("ch-{n:07}");
        let tvg_name = format!("Channel {n}");
        let title = format!("Channel {n} ({quality})");
        let logo = format!("https://cdn.example/logos/{n:07}.png");
        let group_title = format!("Group {group:03}");
        let token = rng.next_u64();
        let url = format!("https://stream.example/{n}/master.m3u8?token={token:016x}");
        writeln!(
            writer,
            "#EXTINF:-1 tvg-id=\"{tvg_id}\" tvg-name=\"{tvg_name}\" tvg-chno=\"{chno_major}.{chno_minor}\" tvg-logo=\"{logo}\" group-title=\"{group_title}\",{title}"
        )?;
        writeln!(writer, "{url}")?;
    }
    Ok(())
}

/// Writes a deterministic XMLTV document to `writer`.
///
/// Each channel has one display name and one icon. Each programme has a title,
/// a sub-title, a description, and a one-hour interval. Programme start times
/// advance by one hour per programme on each channel. Start and stop times
/// roll across day boundaries so every slot stays a valid XMLTV timestamp.
///
/// # Errors
///
/// Returns an error when `writer` fails.
///
/// # Panics
///
/// Panics only when the fixed base date `2026-01-01` fails to construct. This
/// date is a known-valid constant, so this call never panics in practice.
pub fn generate_xmltv<W: Write>(config: ScaleXmltvConfig, mut writer: W) -> std::io::Result<()> {
    writeln!(writer, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(
        writer,
        "<tv generator-info-name=\"scale-fixture\" source-info-url=\"https://guide.example\">"
    )?;
    let mut rng = Rng::new(config.seed);
    for channel in 0..config.channels {
        let id = format!("ch-{channel:05}");
        let display = format!("Channel {channel}");
        let icon = format!("https://cdn.example/logos/{channel:05}.png");
        writeln!(writer, "  <channel id=\"{id}\">")?;
        writeln!(
            writer,
            "    <display-name lang=\"en\">{display}</display-name>"
        )?;
        writeln!(writer, "    <icon src=\"{icon}\"/>")?;
        writeln!(writer, "  </channel>")?;
    }
    let channels = config.channels.max(1);
    let base = NaiveDate::from_ymd_opt(2026, 1, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .expect("valid base timestamp");
    for programme in 0..config.programmes {
        let channel = programme % channels;
        let slot = u32::try_from(programme / channels).unwrap_or(u32::MAX);
        let id = format!("ch-{channel:05}");
        let start = base + TimeDelta::hours(i64::from(slot));
        let stop = base + TimeDelta::hours(i64::from(slot) + 1);
        let start = format_xmltv_timestamp(start);
        let stop = format_xmltv_timestamp(stop);
        let title = format!("Programme {programme}");
        let subtitle = format!("Slot {slot}");
        let category = match rng.next_u32() % 3 {
            0 => "News",
            1 => "Sports",
            _ => "Movie",
        };
        let desc = format!("Deterministic description for programme {programme}.");
        writeln!(
            writer,
            "  <programme channel=\"{id}\" start=\"{start}\" stop=\"{stop}\">"
        )?;
        writeln!(writer, "    <title lang=\"en\">{title}</title>")?;
        writeln!(writer, "    <sub-title lang=\"en\">{subtitle}</sub-title>")?;
        writeln!(writer, "    <desc lang=\"en\">{desc}</desc>")?;
        writeln!(writer, "    <category lang=\"en\">{category}</category>")?;
        writeln!(writer, "  </programme>")?;
    }
    writeln!(writer, "</tv>")?;
    Ok(())
}

/// Formats a [`NaiveDateTime`] as a compact XMLTV timestamp in UTC.
fn format_xmltv_timestamp(instant: NaiveDateTime) -> String {
    format!("{} +0000", instant.format("%Y%m%d%H%M%S"))
}

/// Computes the SHA-256 digest of the M3U fixture for `config`.
///
/// Use this to verify generator determinism without keeping the full artifact.
///
/// # Panics
///
/// Panics only if the in-memory hasher rejects the generated bytes. The
/// `Sha256` writer never returns an error, so this call never panics in
/// practice.
#[must_use]
pub fn m3u_sha256(config: ScaleM3uConfig) -> String {
    let mut hasher = Sha256::new();
    generate_m3u(config, &mut hasher).expect("hasher write cannot fail");
    format!("{:x}", hasher.finalize())
}

/// Computes the SHA-256 digest of the XMLTV fixture for `config`.
///
/// Use this to verify generator determinism without keeping the full artifact.
///
/// # Panics
///
/// Panics only if the in-memory hasher rejects the generated bytes. The
/// `Sha256` writer never returns an error, so this call never panics in
/// practice.
#[must_use]
pub fn xmltv_sha256(config: ScaleXmltvConfig) -> String {
    let mut hasher = Sha256::new();
    generate_xmltv(config, &mut hasher).expect("hasher write cannot fail");
    format!("{:x}", hasher.finalize())
}

/// Scale-gate budgets for the pinned-runner workload.
///
/// Values come from the task acceptance criteria. A measurement fails the gate
/// when it exceeds its budget. A measurement regresses when it exceeds the
/// pinned baseline by more than ten percent.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct ScaleBudgets {
    /// Maximum M3U parse time in seconds.
    pub m3u_parse_seconds: f64,
    /// Maximum XMLTV parse time in seconds.
    pub xmltv_parse_seconds: f64,
    /// Maximum M3U activation time in seconds.
    pub m3u_activation_seconds: f64,
    /// Maximum XMLTV activation time in seconds.
    pub xmltv_activation_seconds: f64,
    /// Maximum peak resident set size in mebibytes.
    pub peak_rss_mib: f64,
}

impl Default for ScaleBudgets {
    fn default() -> Self {
        Self {
            m3u_parse_seconds: 15.0,
            xmltv_parse_seconds: 10.0,
            m3u_activation_seconds: 90.0,
            xmltv_activation_seconds: 45.0,
            peak_rss_mib: 512.0,
        }
    }
}

/// The regression tolerance for baseline comparisons.
pub const REGRESSION_TOLERANCE: f64 = 0.10;

/// Returns `true` when `measured` exceeds `budget`.
#[must_use]
pub fn exceeds_budget(measured: f64, budget: f64) -> bool {
    measured > budget
}

/// Returns `true` when `measured` regresses against `baseline` by more than
/// the configured tolerance.
///
/// A regression is a measurement that is greater than `baseline * (1.0 + tolerance)`.
/// Lower measurements never count as regressions.
#[must_use]
pub fn regresses(measured: f64, baseline: f64, tolerance: f64) -> bool {
    measured > baseline * (1.0 + tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix64_is_deterministic_for_a_fixed_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1_000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        let mut differences = 0;
        for _ in 0..1_000 {
            if a.next_u64() != b.next_u64() {
                differences += 1;
            }
        }
        assert!(differences > 900);
    }

    #[test]
    fn between_respects_the_inclusive_range() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            let value = rng.between(5, 9);
            assert!((5..=9).contains(&value));
        }
    }

    #[test]
    fn m3u_generator_is_byte_stable_for_a_fixed_seed() {
        let config = ScaleM3uConfig {
            entries: 1_000,
            seed: ScaleM3uConfig::DEFAULT_SEED,
        };
        let first = m3u_sha256(config);
        let second = m3u_sha256(config);
        assert_eq!(first, second);
    }

    #[test]
    fn m3u_generator_seed_changes_output() {
        let a = m3u_sha256(ScaleM3uConfig {
            entries: 1_000,
            seed: 1,
        });
        let b = m3u_sha256(ScaleM3uConfig {
            entries: 1_000,
            seed: 2,
        });
        assert_ne!(a, b);
    }

    #[test]
    fn m3u_generator_produces_a_header_and_the_requested_entries() {
        let mut buffer = Vec::new();
        generate_m3u(
            ScaleM3uConfig {
                entries: 3,
                seed: ScaleM3uConfig::DEFAULT_SEED,
            },
            &mut buffer,
        )
        .expect("write");
        let text = String::from_utf8(buffer).expect("ascii");
        let extinf_count = text.matches("#EXTINF:").count();
        assert_eq!(extinf_count, 3);
        assert!(text.starts_with("#EXTM3U\n"));
    }

    #[test]
    fn xmltv_generator_is_byte_stable_for_a_fixed_seed() {
        let config = ScaleXmltvConfig {
            channels: 10,
            programmes: 200,
            seed: ScaleXmltvConfig::DEFAULT_SEED,
        };
        let first = xmltv_sha256(config);
        let second = xmltv_sha256(config);
        assert_eq!(first, second);
    }

    #[test]
    fn xmltv_generator_seed_changes_output() {
        let a = xmltv_sha256(ScaleXmltvConfig {
            channels: 10,
            programmes: 200,
            seed: 1,
        });
        let b = xmltv_sha256(ScaleXmltvConfig {
            channels: 10,
            programmes: 200,
            seed: 2,
        });
        assert_ne!(a, b);
    }

    #[test]
    fn xmltv_generator_emits_channels_and_programmes() {
        let mut buffer = Vec::new();
        generate_xmltv(
            ScaleXmltvConfig {
                channels: 4,
                programmes: 9,
                seed: ScaleXmltvConfig::DEFAULT_SEED,
            },
            &mut buffer,
        )
        .expect("write");
        let text = String::from_utf8(buffer).expect("ascii");
        assert_eq!(text.matches("<channel ").count(), 4);
        assert_eq!(text.matches("<programme ").count(), 9);
        assert!(text.contains("</tv>"));
    }

    #[test]
    fn xmltv_generator_rolls_dates_across_one_hundred_slots_per_channel() {
        let mut buffer = Vec::new();
        generate_xmltv(
            ScaleXmltvConfig {
                channels: 3,
                programmes: 300,
                seed: ScaleXmltvConfig::DEFAULT_SEED,
            },
            &mut buffer,
        )
        .expect("write");
        let text = String::from_utf8(buffer).expect("ascii");
        // Slot 99 starts on 2026-01-05 at 03:00 and stops at 04:00.
        assert!(
            text.contains("start=\"20260105030000 +0000\""),
            "missing rolled start timestamp for slot 99"
        );
        assert!(
            text.contains("stop=\"20260105040000 +0000\""),
            "missing rolled stop timestamp for slot 99"
        );
        // No hour field may exceed 23.
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("  <programme ") {
                let start_hour = rest
                    .split("start=\"")
                    .nth(1)
                    .and_then(|tail| tail.get(8..10))
                    .expect("start hour");
                let hour: u32 = start_hour.parse().expect("start hour digits");
                assert!(
                    hour <= 23,
                    "invalid start hour {hour} in programme line: {line}"
                );
            }
        }
    }

    #[test]
    fn budget_check_flags_overruns() {
        assert!(exceeds_budget(16.0, 15.0));
        assert!(!exceeds_budget(15.0, 15.0));
        assert!(!exceeds_budget(14.9, 15.0));
    }

    #[test]
    fn regression_check_flags_only_large_overruns() {
        // Ten percent above baseline is the boundary and is allowed.
        assert!(!regresses(16.5, 15.0, REGRESSION_TOLERANCE));
        // Strictly above ten percent is a regression.
        assert!(regresses(16.6, 15.0, REGRESSION_TOLERANCE));
        // Lower measurements never regress.
        assert!(!regresses(10.0, 15.0, REGRESSION_TOLERANCE));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn default_budgets_match_the_task_acceptance_criteria() {
        let budgets = ScaleBudgets::default();
        assert_eq!(budgets.m3u_parse_seconds, 15.0);
        assert_eq!(budgets.xmltv_parse_seconds, 10.0);
        assert_eq!(budgets.m3u_activation_seconds, 90.0);
        assert_eq!(budgets.xmltv_activation_seconds, 45.0);
        assert_eq!(budgets.peak_rss_mib, 512.0);
    }
}
