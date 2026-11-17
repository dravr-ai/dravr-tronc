// ABOUTME: sqlx 0.8 SQLite codecs for the date types: the text sqlx's chrono support stores, and every shape it reads
// ABOUTME: So `.bind(instant)` and `row.get()` keep working and rows written before the move still decode
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! What is stored, unchanged from sqlx's `chrono` feature:
//!
//! | Type | Text written |
//! |---|---|
//! | [`Instant`] | `2026-11-17T10:00:00.100+00:00` ([`Instant::to_rfc3339`]) |
//! | [`CivilDateTime`] | `2026-11-17 10:00:00.100` |
//! | [`CivilDate`] | `2026-11-17` |
//! | [`CivilTime`] | `10:00:00.100` |
//!
//! Reading an [`Instant`] or a [`CivilDateTime`] accepts what sqlx's chrono
//! decoder accepts: RFC 3339, the twelve SQLite date-time shapes (with or
//! without seconds, `T` or a space, no offset, `Z` or `±hh:mm`; no offset
//! means UTC, which is what `datetime('now')` and `CURRENT_TIMESTAMP`
//! write), an integer as Unix seconds and a real as a Julian day.
//!
//! A [`CivilDateTime`] is read as the value shows it, over its whole range.
//! An [`Instant`] reaches about a day less far at each end; a value out
//! there is an error for an `Instant`, never a shifted one.
//!
//! One difference a caller cannot see through `bind` or `get`:
//! `Type::type_info()` reports `TEXT` where chrono's impl reported
//! `DATETIME`, because sqlx keeps that constructor private. SQLite binds by
//! value, and `compatible()` accepts the same column types as before.

use jiff::civil::DateTime;
use jiff::SignedDuration;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef};
use sqlx::{Decode, Encode, Sqlite, Type, TypeInfo, ValueRef};

use super::text;
use super::{CivilDate, CivilDateTime, CivilTime, Instant, TimeError};

/// The date-time shapes sqlx's chrono decoder tries after RFC 3339, in its
/// order.
const DATETIME_FORMATS: [&str; 12] = [
    "%F %T%.f",
    "%F %R",
    "%F %RZ",
    "%F %R%:z",
    "%F %T%.fZ",
    "%F %T%.f%:z",
    "%FT%R",
    "%FT%RZ",
    "%FT%R%:z",
    "%FT%T%.f",
    "%FT%T%.fZ",
    "%FT%T%.f%:z",
];

/// The time-of-day shapes sqlx's chrono decoder tries, in its order. The
/// first is a literal `.f` in that decoder, kept as it is.
const TIME_FORMATS: [&str; 7] = ["%T.f", "%T%.f", "%R", "%RZ", "%T%.fZ", "%R%:z", "%T%.f%:z"];

/// Days from the Julian epoch to the Unix epoch.
const UNIX_EPOCH_JULIAN_DAY: f64 = 2_440_587.5;
const SECONDS_PER_DAY: f64 = 86_400.0;

/// How SQLite holds a value, by the name sqlx gives its type.
#[derive(Clone, Copy)]
enum Storage {
    Text,
    Integer,
    Real,
    Other,
}

fn storage(value: &SqliteValueRef<'_>) -> Storage {
    match value.type_info().name() {
        "TEXT" => Storage::Text,
        "INTEGER" => Storage::Integer,
        "REAL" => Storage::Real,
        _ => Storage::Other,
    }
}

/// A date and time as the stored value shows them, and the offset in seconds
/// east of UTC they were written with. Text without an offset, an integer
/// and a real are all UTC.
///
/// A local reading is kept as read and never passed through an instant: the
/// calendar reaches about a day further than [`Instant`] at each end.
type Reading = (DateTime, i32);

const UNIX_EPOCH: DateTime = DateTime::constant(1970, 1, 1, 0, 0, 0, 0);

fn reading_from_text(value: &str) -> Option<Reading> {
    let fields = text::strict_rfc3339_fields(value).ok().or_else(|| {
        DATETIME_FORMATS
            .into_iter()
            .find_map(|format| text::parse(value, format).ok())
    })?;
    match fields.reading() {
        Ok((civil, offset)) => Some((civil, offset.unwrap_or(0))),
        // sqlx's chrono decoder reads the date and time alone, as UTC, when
        // the offset is not one: `+24:00` and beyond.
        Err(_) => Some((fields.datetime().ok()?, 0)),
    }
}

/// The UTC reading `seconds` and `nanos` after the Unix epoch.
fn reading_at_epoch(seconds: i64, nanos: u32) -> Option<Reading> {
    let since = SignedDuration::from_secs(seconds)
        .checked_add(SignedDuration::from_nanos(i64::from(nanos)))?;
    Some((UNIX_EPOCH.checked_add(since).ok()?, 0))
}

fn reading_from_julian_day(value: f64) -> Option<Reading> {
    let timestamp = (value - UNIX_EPOCH_JULIAN_DAY) * SECONDS_PER_DAY;
    if !timestamp.is_finite() {
        return None;
    }
    // The lossy casts are sqlx's own: whole seconds toward zero, and the
    // fraction counted forward from them.
    let seconds = timestamp.trunc() as i64;
    let nanos = (timestamp.fract() * 1E9).abs() as u32;
    reading_at_epoch(seconds, nanos)
}

fn decode_reading(value: SqliteValueRef<'_>) -> Result<Reading, BoxDynError> {
    let found = match storage(&value) {
        Storage::Text => {
            let text = <&str as Decode<Sqlite>>::decode(value)?;
            return reading_from_text(text)
                .ok_or_else(|| format!("invalid datetime: {text}").into());
        }
        Storage::Integer => reading_at_epoch(<i64 as Decode<Sqlite>>::decode(value)?, 0),
        Storage::Real => reading_from_julian_day(<f64 as Decode<Sqlite>>::decode(value)?),
        Storage::Other => None,
    };
    found.ok_or_else(|| "invalid datetime: not text, an integer or a real".into())
}

/// The instant of a reading, or [`TimeError::OutOfRange`] when the reading is
/// in the last day of the calendar, past the last instant.
fn instant_of((civil, offset): Reading) -> Result<Instant, TimeError> {
    text::timestamp_at(civil, offset).map(Instant)
}

fn datetime_compatible(ty: &SqliteTypeInfo) -> bool {
    matches!(ty.name(), "DATETIME" | "TEXT" | "INTEGER" | "REAL")
}

fn encode_text<'q>(
    text: String,
    buf: &mut Vec<SqliteArgumentValue<'q>>,
) -> Result<IsNull, BoxDynError> {
    <String as Encode<'q, Sqlite>>::encode(text, buf)
}

impl Type<Sqlite> for Instant {
    fn type_info() -> SqliteTypeInfo {
        <str as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        datetime_compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for Instant {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        encode_text(self.to_rfc3339(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for Instant {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        Ok(instant_of(decode_reading(value)?)?)
    }
}

impl Type<Sqlite> for CivilDateTime {
    fn type_info() -> SqliteTypeInfo {
        <str as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        datetime_compatible(ty)
    }
}

impl<'q> Encode<'q, Sqlite> for CivilDateTime {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        encode_text(self.to_string(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for CivilDateTime {
    /// The reading as written: a value stored with an offset decodes to its
    /// local date and time at that offset, as sqlx's chrono decoder gave.
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        decode_reading(value).map(|(local, _)| Self(local))
    }
}

impl Type<Sqlite> for CivilDate {
    fn type_info() -> SqliteTypeInfo {
        <str as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        matches!(ty.name(), "DATE" | "TEXT")
    }
}

impl<'q> Encode<'q, Sqlite> for CivilDate {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        encode_text(self.to_string(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for CivilDate {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <&str as Decode<Sqlite>>::decode(value)?;
        Ok(Self::parse_from_str(text, "%F")?)
    }
}

impl Type<Sqlite> for CivilTime {
    fn type_info() -> SqliteTypeInfo {
        <str as Type<Sqlite>>::type_info()
    }

    fn compatible(ty: &SqliteTypeInfo) -> bool {
        matches!(ty.name(), "TIME" | "TEXT")
    }
}

impl<'q> Encode<'q, Sqlite> for CivilTime {
    fn encode_by_ref(&self, buf: &mut Vec<SqliteArgumentValue<'q>>) -> Result<IsNull, BoxDynError> {
        encode_text(self.to_string(), buf)
    }
}

impl<'r> Decode<'r, Sqlite> for CivilTime {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, BoxDynError> {
        let text = <&str as Decode<Sqlite>>::decode(value)?;
        TIME_FORMATS
            .into_iter()
            .find_map(|format| Self::parse_from_str(text, format).ok())
            .ok_or_else(|| format!("invalid time: {text}").into())
    }
}

#[cfg(test)]
mod tests {
    use super::{instant_of, Instant, TimeError, UNIX_EPOCH};

    #[test]
    fn an_offset_no_clock_has_is_out_of_range_not_an_engine_error() {
        assert_eq!(
            instant_of((UNIX_EPOCH, 3600)).map(Instant::epoch_seconds),
            Ok(-3600)
        );
        // Past 25:59:59 the engine has no such offset; its own error type
        // stays inside.
        for offset in [93_600, 100_000, i32::MAX, i32::MIN] {
            assert_eq!(
                instant_of((UNIX_EPOCH, offset)),
                Err(TimeError::OutOfRange),
                "{offset}"
            );
        }
    }
}
