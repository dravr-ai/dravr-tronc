// ABOUTME: sqlx 0.8 PostgreSQL codecs for the date types: the binary values sqlx's chrono support sends and reads
// ABOUTME: timestamptz, timestamp and time as microseconds, date as days, all counted from 2000-01-01
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! | Type | PostgreSQL type | Binary value |
//! |---|---|---|
//! | [`Instant`] | `TIMESTAMPTZ` | `i64` microseconds since 2000-01-01T00:00:00Z |
//! | [`CivilDateTime`] | `TIMESTAMP` | `i64` microseconds since 2000-01-01T00:00:00 |
//! | [`CivilDate`] | `DATE` | `i32` days since 2000-01-01 |
//! | [`CivilTime`] | `TIME` | `i64` microseconds since midnight |
//!
//! Nanoseconds below the microsecond are dropped toward zero when written,
//! as sqlx's chrono encoder drops them. Each type also implements
//! `PgHasArrayType`, so a `Vec` of it binds as the matching array.
//!
//! The types are declared by their built-in OIDs, which never differ
//! between servers, because sqlx keeps its named constants private.

use std::mem;

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::Offset;
use jiff::{SignedDuration, Timestamp};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueFormat, PgValueRef};
use sqlx::{Decode, Encode, Postgres, Type};

use super::error::TimeError;
use super::{CivilDate, CivilDateTime, CivilTime, Instant};

const DATE: Oid = Oid(1082);
const TIME: Oid = Oid(1083);
const TIMESTAMP: Oid = Oid(1114);
const TIMESTAMP_ARRAY: Oid = Oid(1115);
const DATE_ARRAY: Oid = Oid(1182);
const TIME_ARRAY: Oid = Oid(1183);
const TIMESTAMPTZ: Oid = Oid(1184);
const TIMESTAMPTZ_ARRAY: Oid = Oid(1185);

/// The day PostgreSQL counts dates and timestamps from.
const EPOCH_DATE: Date = Date::constant(2000, 1, 1);
const EPOCH: DateTime = DateTime::constant(2000, 1, 1, 0, 0, 0, 0);

const TIMESTAMPTZ_TEXT: &str = "%Y-%m-%d %H:%M:%S%.f%#z";
const TIMESTAMP_TEXT: &str = "%Y-%m-%d %H:%M:%S%.f";

/// Microseconds from the PostgreSQL epoch to a date and time, toward zero.
fn micros_since_epoch(civil: DateTime) -> Result<i64, TimeError> {
    i64::try_from(civil.duration_since(EPOCH).as_micros()).map_err(|_| TimeError::OutOfRange)
}

/// The date and time `micros` microseconds from the PostgreSQL epoch.
fn civil_at_micros(micros: i64) -> Result<DateTime, TimeError> {
    EPOCH
        .checked_add(SignedDuration::from_micros(micros))
        .map_err(|_| TimeError::OutOfRange)
}

fn utc_instant(civil: DateTime) -> Result<Timestamp, TimeError> {
    Offset::UTC
        .to_timestamp(civil)
        .map_err(|_| TimeError::OutOfRange)
}

impl Instant {
    /// The `TIMESTAMPTZ` binary value: microseconds since 2000-01-01 UTC.
    fn to_pg_micros(self) -> Result<i64, TimeError> {
        micros_since_epoch(Offset::UTC.to_datetime(self.0))
    }

    fn from_pg_micros(micros: i64) -> Result<Self, TimeError> {
        utc_instant(civil_at_micros(micros)?).map(Self)
    }

    /// The text form PostgreSQL sends outside the extended protocol:
    /// `2026-11-17 10:00:00.1+00`.
    fn from_pg_text(value: &str) -> Result<Self, TimeError> {
        Self::parse_from_str(value, TIMESTAMPTZ_TEXT)
    }
}

impl CivilDateTime {
    fn from_pg_text(value: &str) -> Result<Self, TimeError> {
        let format = if value.contains('+') {
            TIMESTAMPTZ_TEXT
        } else {
            TIMESTAMP_TEXT
        };
        Self::parse_from_str(value, format)
    }
}

impl CivilDate {
    /// The `DATE` binary value: days since 2000-01-01.
    fn to_pg_days(self) -> Result<i32, TimeError> {
        i32::try_from(self.0.duration_since(EPOCH_DATE).as_hours() / 24)
            .map_err(|_| TimeError::OutOfRange)
    }

    fn from_pg_days(days: i32) -> Result<Self, TimeError> {
        EPOCH_DATE
            .checked_add(SignedDuration::from_hours(i64::from(days) * 24))
            .map(Self)
            .map_err(|_| TimeError::OutOfRange)
    }
}

impl CivilTime {
    /// The `TIME` binary value: microseconds since midnight.
    fn to_pg_micros(self) -> i64 {
        i64::try_from(self.0.duration_since(Time::midnight()).as_micros()).unwrap_or(0)
    }

    /// Wraps at 24 hours, as sqlx's chrono decoder does for PostgreSQL's
    /// `24:00:00`.
    fn from_pg_micros(micros: i64) -> Self {
        Self(Time::midnight().wrapping_add(SignedDuration::from_micros(micros)))
    }
}

impl Type<Postgres> for Instant {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(TIMESTAMPTZ)
    }
}

impl PgHasArrayType for Instant {
    fn array_type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(TIMESTAMPTZ_ARRAY)
    }
}

impl Encode<'_, Postgres> for Instant {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Encode::<Postgres>::encode(self.to_pg_micros()?, buf)
    }

    fn size_hint(&self) -> usize {
        mem::size_of::<i64>()
    }
}

impl<'r> Decode<'r, Postgres> for Instant {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        Ok(match value.format() {
            PgValueFormat::Binary => Self::from_pg_micros(Decode::<Postgres>::decode(value)?)?,
            PgValueFormat::Text => Self::from_pg_text(value.as_str()?)?,
        })
    }
}

impl Type<Postgres> for CivilDateTime {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(TIMESTAMP)
    }
}

impl PgHasArrayType for CivilDateTime {
    fn array_type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(TIMESTAMP_ARRAY)
    }
}

impl Encode<'_, Postgres> for CivilDateTime {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Encode::<Postgres>::encode(micros_since_epoch(self.0)?, buf)
    }

    fn size_hint(&self) -> usize {
        mem::size_of::<i64>()
    }
}

impl<'r> Decode<'r, Postgres> for CivilDateTime {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        Ok(match value.format() {
            PgValueFormat::Binary => Self(civil_at_micros(Decode::<Postgres>::decode(value)?)?),
            PgValueFormat::Text => Self::from_pg_text(value.as_str()?)?,
        })
    }
}

impl Type<Postgres> for CivilDate {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(DATE)
    }
}

impl PgHasArrayType for CivilDate {
    fn array_type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(DATE_ARRAY)
    }
}

impl Encode<'_, Postgres> for CivilDate {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Encode::<Postgres>::encode(self.to_pg_days()?, buf)
    }

    fn size_hint(&self) -> usize {
        mem::size_of::<i32>()
    }
}

impl<'r> Decode<'r, Postgres> for CivilDate {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        Ok(match value.format() {
            PgValueFormat::Binary => Self::from_pg_days(Decode::<Postgres>::decode(value)?)?,
            PgValueFormat::Text => Self::parse_from_str(value.as_str()?, "%Y-%m-%d")?,
        })
    }
}

impl Type<Postgres> for CivilTime {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(TIME)
    }
}

impl PgHasArrayType for CivilTime {
    fn array_type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(TIME_ARRAY)
    }
}

impl Encode<'_, Postgres> for CivilTime {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Encode::<Postgres>::encode(self.to_pg_micros(), buf)
    }

    fn size_hint(&self) -> usize {
        mem::size_of::<i64>()
    }
}

impl<'r> Decode<'r, Postgres> for CivilTime {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        Ok(match value.format() {
            PgValueFormat::Binary => Self::from_pg_micros(Decode::<Postgres>::decode(value)?),
            PgValueFormat::Text => Self::parse_from_str(value.as_str()?, "%H:%M:%S%.f")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(text: &str) -> Instant {
        Instant::parse_from_rfc3339(text).unwrap()
    }

    #[test]
    fn timestamptz_counts_microseconds_from_the_year_2000() {
        assert_eq!(instant("2000-01-01T00:00:00Z").to_pg_micros(), Ok(0));
        assert_eq!(
            instant("2026-11-17T10:00:00.123456Z").to_pg_micros(),
            Ok(848_224_800_123_456)
        );
        assert_eq!(
            instant("1999-12-31T23:59:59Z").to_pg_micros(),
            Ok(-1_000_000)
        );
    }

    #[test]
    fn timestamptz_drops_nanoseconds_toward_zero_on_both_sides_of_the_epoch() {
        // After 2000 the remainder is cut; before 2000 it is cut toward
        // zero too, which is one microsecond later than rounding down.
        assert_eq!(
            instant("2000-01-01T00:00:00.000001999Z").to_pg_micros(),
            Ok(1)
        );
        assert_eq!(
            instant("1999-12-31T23:59:59.999998001Z").to_pg_micros(),
            Ok(-1)
        );
        assert_eq!(
            instant("1969-07-20T20:17:40.123456789Z").to_pg_micros(),
            Ok(-960_867_739_876_543)
        );
    }

    #[test]
    fn timestamptz_decodes_what_it_encodes_at_microsecond_precision() {
        for text in [
            "2026-11-17T10:00:00.123456Z",
            "1969-07-20T20:17:40.123456Z",
            "2000-01-01T00:00:00Z",
            "0001-01-01T00:00:00Z",
            "9999-12-30T21:59:59.999999Z",
        ] {
            let value = instant(text);
            let micros = value.to_pg_micros().unwrap();
            assert_eq!(Instant::from_pg_micros(micros), Ok(value), "{text}");
        }
        assert_eq!(
            Instant::from_pg_micros(i64::MAX),
            Err(TimeError::OutOfRange)
        );
        assert_eq!(
            Instant::from_pg_micros(i64::MIN),
            Err(TimeError::OutOfRange)
        );
    }

    #[test]
    fn timestamptz_text_is_read_with_its_short_offset() {
        assert_eq!(
            Instant::from_pg_text("2026-11-17 10:00:00.1+00"),
            Ok(instant("2026-11-17T10:00:00.100Z"))
        );
        assert_eq!(
            Instant::from_pg_text("2026-11-17 05:00:00-05"),
            Ok(instant("2026-11-17T10:00:00Z"))
        );
        assert_eq!(
            Instant::from_pg_text("2026-11-17 15:30:00+05:30"),
            Ok(instant("2026-11-17T10:00:00Z"))
        );
        assert_eq!(
            Instant::from_pg_text("2026-11-17 10:00:00"),
            Err(TimeError::InvalidText)
        );
    }

    #[test]
    fn timestamp_text_ignores_an_offset_and_reads_without_one() {
        let expected: CivilDateTime = "2026-11-17T10:00:00.250".parse().unwrap();
        assert_eq!(
            CivilDateTime::from_pg_text("2026-11-17 10:00:00.25"),
            Ok(expected)
        );
        assert_eq!(
            CivilDateTime::from_pg_text("2026-11-17 10:00:00.25+02"),
            Ok(expected)
        );
    }

    #[test]
    fn date_counts_days_from_the_year_2000() {
        let date = |y, m, d| CivilDate::from_ymd(y, m, d).unwrap();
        assert_eq!(date(2000, 1, 1).to_pg_days(), Ok(0));
        assert_eq!(date(2026, 11, 17).to_pg_days(), Ok(9_817));
        assert_eq!(date(1999, 12, 31).to_pg_days(), Ok(-1));
        assert_eq!(date(1970, 1, 1).to_pg_days(), Ok(-10_957));
        assert_eq!(CivilDate::from_pg_days(9_817), Ok(date(2026, 11, 17)));
        assert_eq!(CivilDate::from_pg_days(-10_957), Ok(date(1970, 1, 1)));
        assert_eq!(
            CivilDate::from_pg_days(i32::MAX),
            Err(TimeError::OutOfRange)
        );
    }

    #[test]
    fn time_counts_microseconds_from_midnight_and_wraps_at_a_day() {
        let time = CivilTime::from_hms_nano(10, 0, 0, 123_456_789).unwrap();
        assert_eq!(time.to_pg_micros(), 36_000_123_456);
        assert_eq!(
            CivilTime::from_pg_micros(36_000_123_456),
            CivilTime::from_hms_micro(10, 0, 0, 123_456).unwrap()
        );
        assert_eq!(CivilTime::MIDNIGHT.to_pg_micros(), 0);
        assert_eq!(
            CivilTime::from_pg_micros(86_400_000_000),
            CivilTime::MIDNIGHT
        );
    }
}
