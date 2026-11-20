//! Time values with a stable wire format.
//!
//! [`Timestamp`] wraps [`jiff::Timestamp`]. It keeps the JSON and text formats
//! that the relay wrote before the move from chrono to jiff:
//!
//! - JSON uses RFC 3339 with a `Z` suffix, for example `2026-03-08T09:00:00.120Z`.
//! - [`Timestamp::to_rfc3339`] uses a `+00:00` suffix.
//! - The fraction has 0, 3, 6 or 9 digits. Each form uses the fewest digits
//!   that keep the exact value.
//!
//! The type has no `Display` implementation. Each caller selects a format.

use jiff::{
    RoundMode, SignedDuration, TimestampRound, Unit, Zoned,
    civil::DateTime,
    tz::{AmbiguousOffset, TimeZone},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{
    fmt,
    ops::{Add, AddAssign, Sub, SubAssign},
    str::FromStr,
};

/// An exact instant in UTC.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    /// The Unix epoch, `1970-01-01T00:00:00Z`.
    pub const UNIX_EPOCH: Self = Self(jiff::Timestamp::UNIX_EPOCH);

    /// Returns the current system time.
    #[must_use]
    pub fn now() -> Self {
        Self(jiff::Timestamp::now())
    }

    /// Wraps a jiff timestamp.
    #[must_use]
    pub const fn from_jiff(value: jiff::Timestamp) -> Self {
        Self(value)
    }

    /// Returns the jiff timestamp.
    #[must_use]
    pub const fn as_jiff(self) -> jiff::Timestamp {
        self.0
    }

    /// Returns the instant `seconds` after the Unix epoch, or `None` when it is
    /// out of range.
    #[must_use]
    pub fn from_second(seconds: i64) -> Option<Self> {
        jiff::Timestamp::from_second(seconds).ok().map(Self)
    }

    /// Returns the instant `milliseconds` after the Unix epoch, or `None` when
    /// it is out of range.
    #[must_use]
    pub fn from_millisecond(milliseconds: i64) -> Option<Self> {
        jiff::Timestamp::from_millisecond(milliseconds)
            .ok()
            .map(Self)
    }

    /// Returns the whole seconds since the Unix epoch, rounded toward zero.
    #[must_use]
    pub fn as_second(self) -> i64 {
        self.0.as_second()
    }

    /// Returns the whole milliseconds since the Unix epoch, rounded toward zero.
    #[must_use]
    pub fn as_millisecond(self) -> i64 {
        self.0.as_millisecond()
    }

    /// Formats the instant as RFC 3339 with a `+00:00` suffix.
    #[must_use]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "a reference receiver keeps `Option::map(|value| value.to_rfc3339(..))` call sites valid"
    )]
    pub fn to_rfc3339(&self) -> String {
        self.rfc3339_with_suffix("+00:00")
    }

    /// Formats the instant as RFC 3339 with a `Z` suffix.
    ///
    /// This is the JSON form.
    #[must_use]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "a reference receiver keeps `Option::map(|value| value.to_rfc3339_z(..))` call sites valid"
    )]
    pub fn to_rfc3339_z(&self) -> String {
        self.rfc3339_with_suffix("Z")
    }

    /// Formats the instant in UTC with a `strftime` format string.
    ///
    /// In this format, `%z` writes `+0000` and `%Z` writes `UTC`.
    ///
    /// # Panics
    ///
    /// Panics when `format` contains an unsupported directive. Use only
    /// static format strings.
    #[must_use]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "a reference receiver keeps `Option::map(|value| value.format(..))` call sites valid"
    )]
    pub fn format(&self, format: &str) -> String {
        self.to_zoned(TimeZone::UTC).strftime(format).to_string()
    }

    /// Returns the instant in `timezone`.
    #[must_use]
    pub fn to_zoned(self, timezone: TimeZone) -> Zoned {
        self.0.to_zoned(timezone)
    }

    /// Returns the civil date and time of the instant in UTC.
    #[must_use]
    pub fn to_utc_datetime(self) -> DateTime {
        TimeZone::UTC.to_datetime(self.0)
    }

    /// Adds `duration`, or returns `None` on overflow.
    #[must_use]
    pub fn checked_add(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_add(duration).ok().map(Self)
    }

    /// Subtracts `duration`, or returns `None` on overflow.
    #[must_use]
    pub fn checked_sub(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_sub(duration).ok().map(Self)
    }

    /// Returns `self - earlier` as an exact duration.
    #[must_use]
    pub fn duration_since(self, earlier: Self) -> SignedDuration {
        self.0.duration_since(earlier.0)
    }

    /// Rounds the instant down to a whole multiple of `unit`.
    ///
    /// Returns `None` when the result is out of range.
    #[must_use]
    pub fn floor(self, unit: Unit) -> Option<Self> {
        self.0
            .round(TimestampRound::new().smallest(unit).mode(RoundMode::Floor))
            .ok()
            .map(Self)
    }

    fn rfc3339_with_suffix(self, suffix: &str) -> String {
        let datetime = self.to_utc_datetime();
        let fraction = fraction(datetime.subsec_nanosecond());
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}{suffix}",
            datetime.year(),
            datetime.month(),
            datetime.day(),
            datetime.hour(),
            datetime.minute(),
            datetime.second(),
        )
    }
}

impl From<jiff::Timestamp> for Timestamp {
    fn from(value: jiff::Timestamp) -> Self {
        Self(value)
    }
}

impl From<Timestamp> for jiff::Timestamp {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

impl FromStr for Timestamp {
    type Err = jiff::Error;

    /// Parses RFC 3339 text. The text must have an offset.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

impl Add<SignedDuration> for Timestamp {
    type Output = Self;

    fn add(self, duration: SignedDuration) -> Self {
        Self(self.0 + duration)
    }
}

impl AddAssign<SignedDuration> for Timestamp {
    fn add_assign(&mut self, duration: SignedDuration) {
        *self = *self + duration;
    }
}

impl Sub<SignedDuration> for Timestamp {
    type Output = Self;

    fn sub(self, duration: SignedDuration) -> Self {
        Self(self.0 - duration)
    }
}

impl SubAssign<SignedDuration> for Timestamp {
    fn sub_assign(&mut self, duration: SignedDuration) {
        *self = *self - duration;
    }
}

impl Sub for Timestamp {
    type Output = SignedDuration;

    fn sub(self, earlier: Self) -> SignedDuration {
        self.duration_since(earlier)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_rfc3339_z())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl de::Visitor<'_> for Visitor {
            type Value = Timestamp;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an RFC 3339 date and time with an offset")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Timestamp, E> {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

impl utoipa::PartialSchema for Timestamp {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                utoipa::openapi::KnownFormat::DateTime,
            )))
            .into()
    }
}

impl utoipa::ToSchema for Timestamp {}

/// Returns the IANA time zone with the exact name `name`.
///
/// The name match is case-sensitive. `america/denver` gives `None`.
#[must_use]
pub fn time_zone(name: &str) -> Option<TimeZone> {
    let timezone = TimeZone::get(name).ok()?;
    (timezone.iana_name() == Some(name)).then_some(timezone)
}

/// The result of a local date and time in a time zone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalTime {
    /// The local time occurs one time.
    Single(Timestamp),
    /// The local time occurs two times (fall daylight saving change).
    Ambiguous,
    /// The local time does not occur (spring daylight saving change).
    Nonexistent,
}

/// Resolves a local date and time in `timezone`.
#[must_use]
pub fn resolve_local(timezone: &TimeZone, datetime: DateTime) -> LocalTime {
    match timezone.to_ambiguous_timestamp(datetime).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset
            .to_timestamp(datetime)
            .map_or(LocalTime::Nonexistent, |value| {
                LocalTime::Single(Timestamp(value))
            }),
        AmbiguousOffset::Gap { .. } => LocalTime::Nonexistent,
        AmbiguousOffset::Fold { .. } => LocalTime::Ambiguous,
    }
}

/// Formats a civil date and time as `YYYY-MM-DD HH:MM:SS`.
///
/// Error messages use this form.
#[must_use]
pub fn format_civil(datetime: DateTime) -> String {
    format!(
        "{}{}",
        datetime.strftime("%Y-%m-%d %H:%M:%S"),
        fraction(datetime.subsec_nanosecond())
    )
}

/// Formats a fraction of a second with 0, 3, 6 or 9 digits.
fn fraction(nanos: i32) -> String {
    if nanos == 0 {
        String::new()
    } else if nanos % 1_000_000 == 0 {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos % 1_000 == 0 {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{nanos:09}")
    }
}

/// Postgres support for [`Timestamp`] through the `jiff-sqlx` wrappers.
///
/// `sqlx` 0.8 has no `jiff` feature. The column type is `timestamptz`.
#[cfg(feature = "sqlx")]
mod sqlx_support {
    use super::Timestamp;
    use sqlx::{
        Decode, Encode, Postgres, Type,
        encode::IsNull,
        error::BoxDynError,
        postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueRef},
    };

    impl Type<Postgres> for Timestamp {
        fn type_info() -> PgTypeInfo {
            <jiff_sqlx::Timestamp as Type<Postgres>>::type_info()
        }

        fn compatible(ty: &PgTypeInfo) -> bool {
            <jiff_sqlx::Timestamp as Type<Postgres>>::compatible(ty)
        }
    }

    impl PgHasArrayType for Timestamp {
        fn array_type_info() -> PgTypeInfo {
            <jiff_sqlx::Timestamp as PgHasArrayType>::array_type_info()
        }
    }

    impl Encode<'_, Postgres> for Timestamp {
        fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
            jiff_sqlx::Timestamp::from(self.0).encode_by_ref(buf)
        }
    }

    impl<'r> Decode<'r, Postgres> for Timestamp {
        fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
            Ok(Self(jiff_sqlx::Timestamp::decode(value)?.to_jiff()))
        }
    }
}

/// Checks the Postgres wire form of `Timestamp`.
///
/// Before the move to jiff, the relay wrote `timestamptz` values through the
/// `sqlx` chrono support. That form is a big-endian `i64` count of
/// microseconds since 2000-01-01 UTC. This test makes sure that the form did
/// not change.
#[cfg(all(test, feature = "sqlx"))]
mod sqlx_tests {
    use super::Timestamp;
    use sqlx::{
        Encode, Postgres, Type,
        postgres::{PgArgumentBuffer, types::Oid},
    };

    const POSTGRES_EPOCH_MICROS: i64 = 946_684_800_000_000;

    fn encoded(value: &Timestamp) -> Vec<u8> {
        let mut buffer = PgArgumentBuffer::default();
        let _ = value.encode_by_ref(&mut buffer).unwrap();
        buffer.to_vec()
    }

    #[test]
    fn postgres_encoding_is_microseconds_since_2000() {
        // OID 1184 is the Postgres `timestamptz` type.
        assert_eq!(
            <Timestamp as Type<Postgres>>::type_info().oid(),
            Some(Oid(1184))
        );
        for (text, micros) in [
            ("2000-01-01T00:00:00Z", 0_i64),
            ("1999-12-31T23:59:59.999999Z", -1),
            ("1970-01-01T00:00:00Z", -POSTGRES_EPOCH_MICROS),
            ("2026-03-08T09:00:00.5Z", 826_275_600_500_000),
            ("2026-11-01T08:30:00.123456Z", 846_837_000_123_456),
            ("2038-01-19T03:14:08Z", 1_200_798_848_000_000),
        ] {
            let value: Timestamp = text.parse().unwrap();
            assert_eq!(
                value.as_jiff().as_microsecond() - POSTGRES_EPOCH_MICROS,
                micros,
                "{text}"
            );
            assert_eq!(encoded(&value), micros.to_be_bytes(), "{text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    #[test]
    fn rfc3339_uses_three_six_or_nine_fraction_digits() {
        let cases = [
            ("2026-03-08T09:00:00Z", "2026-03-08T09:00:00"),
            ("2026-03-08T09:00:00.5Z", "2026-03-08T09:00:00.500"),
            ("2026-03-08T09:00:00.12Z", "2026-03-08T09:00:00.120"),
            ("2026-03-08T09:00:00.000001Z", "2026-03-08T09:00:00.000001"),
            (
                "2026-03-08T09:00:00.1234567Z",
                "2026-03-08T09:00:00.123456700",
            ),
            (
                "2026-03-08T09:00:00.000000001Z",
                "2026-03-08T09:00:00.000000001",
            ),
            ("1969-12-31T23:59:59.5Z", "1969-12-31T23:59:59.500"),
            ("2026-03-08T02:00:00-07:00", "2026-03-08T09:00:00"),
        ];
        for (input, expected) in cases {
            assert_eq!(at(input).to_rfc3339(), format!("{expected}+00:00"));
            assert_eq!(at(input).to_rfc3339_z(), format!("{expected}Z"));
            assert_eq!(
                serde_json::to_string(&at(input)).unwrap(),
                format!("\"{expected}Z\"")
            );
        }
    }

    #[test]
    fn json_round_trips_and_rejects_text_without_an_offset() {
        let value: Timestamp = serde_json::from_str("\"2027-01-01T02:00:00.25+02:00\"").unwrap();
        assert_eq!(value.to_rfc3339(), "2027-01-01T00:00:00.250+00:00");
        assert!(serde_json::from_str::<Timestamp>("\"2027-01-01T02:00:00\"").is_err());
        assert!(serde_json::from_str::<Timestamp>("1").is_err());
    }

    #[test]
    fn format_writes_utc_offset() {
        assert_eq!(
            at("2026-03-08T01:59:59.999-07:00").format("%Y%m%d%H%M%S %z"),
            "20260308085959 +0000"
        );
    }

    #[test]
    fn floor_truncates_to_the_hour() {
        assert_eq!(
            at("2026-03-08T08:59:59.999Z").floor(Unit::Hour),
            Some(at("2026-03-08T08:00:00Z"))
        );
    }

    #[test]
    fn time_zone_names_are_case_sensitive() {
        assert!(time_zone("America/Denver").is_some());
        assert!(time_zone("UTC").is_some());
        assert!(time_zone("america/denver").is_none());
        assert!(time_zone("Nowhere/City").is_none());
    }

    #[test]
    fn local_times_resolve_at_daylight_saving_changes() {
        let denver = time_zone("America/Denver").unwrap();
        let local = |text: &str| text.parse::<DateTime>().unwrap();
        assert_eq!(
            resolve_local(&denver, local("2026-03-08T01:59")),
            LocalTime::Single(at("2026-03-08T08:59:00Z"))
        );
        assert_eq!(
            resolve_local(&denver, local("2026-03-08T02:30")),
            LocalTime::Nonexistent
        );
        assert_eq!(
            resolve_local(&denver, local("2026-11-01T01:30")),
            LocalTime::Ambiguous
        );
        assert_eq!(
            format_civil(local("2026-11-01T01:30")),
            "2026-11-01 01:30:00"
        );
        assert_eq!(
            format_civil(local("2026-11-01T01:30:00.25")),
            "2026-11-01 01:30:00.250"
        );
    }
}
