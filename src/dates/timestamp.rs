// ABOUTME: Timestamp, the Dravr-owned point on the UTC time line that replaces chrono's DateTime<Utc>
// ABOUTME: Writes chrono's exact RFC 3339 bytes, reads what chrono read, and has no calendar-unit arithmetic
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::str::FromStr;
use std::time::SystemTime;

use jiff::civil::DateTime;
use jiff::tz::Offset;
use jiff::Timestamp as EngineTimestamp;

use super::civil_date::CivilDate;
use super::civil_datetime::CivilDateTime;
use super::civil_time::CivilTime;
use super::elapsed::Elapsed;
use super::error::DateError;
use super::text::{self, FractionDigits, Moment};

const NANOS_PER_SECOND: i128 = 1_000_000_000;
const NANOS_PER_MILLI: i128 = 1_000_000;
const NANOS_PER_MICRO: i128 = 1_000;

/// How many fraction digits [`Timestamp::to_rfc3339_opts`] writes.
///
/// The variants and their meaning are chrono's `SecondsFormat`, so a call
/// site keeps its arguments.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum SecondsFormat {
    /// Whole seconds; the fraction is dropped, not rounded.
    Secs,
    /// Exactly three digits.
    Millis,
    /// Exactly six digits.
    Micros,
    /// Exactly nine digits.
    Nanos,
    /// No digits for a whole second, otherwise three, six or nine: the
    /// fewest of those that lose nothing.
    AutoSi,
}

impl SecondsFormat {
    const fn digits(self) -> FractionDigits {
        match self {
            Self::Secs => FractionDigits::Secs,
            Self::Millis => FractionDigits::Millis,
            Self::Micros => FractionDigits::Micros,
            Self::Nanos => FractionDigits::Nanos,
            Self::AutoSi => FractionDigits::Auto,
        }
    }
}

/// A point on the time line, in UTC, with nanosecond precision.
///
/// Replaces `chrono::DateTime<Utc>`.
///
/// # Bytes
///
/// - serde writes RFC 3339 ending in `Z`, with no fraction for a whole second
///   and otherwise exactly three, six or nine digits: `2026-11-17T10:00:00Z`,
///   `2026-11-17T10:00:00.100Z`. These are the bytes chrono's serde writes.
/// - [`to_rfc3339`](Self::to_rfc3339) writes the same digits ending in
///   `+00:00`, as chrono's method of that name does. It is also the text
///   stored in SQLite.
/// - serde and `FromStr` read RFC 3339 with chrono's leniency (a space for
///   the `T`, `UTC` for the offset, no colon in the offset, unpadded digits);
///   [`parse_from_rfc3339`](Self::parse_from_rfc3339) reads RFC 3339 strictly.
///
/// # Arithmetic
///
/// A timestamp moves by an exact [`Elapsed`] and by nothing else: there is no
/// "add a month" here, because a month has no length on the time line. Take
/// the date ([`utc_date`](Self::utc_date), or `Zone::date_at` for an
/// athlete's zone), do the calendar arithmetic on
/// [`CivilDate`], and come back through the zone.
///
/// No operation panics. `+` and `-` stop at [`Timestamp::MIN`] and
/// [`Timestamp::MAX`]; `checked_add` and `checked_sub` report leaving the range.
///
/// # Range
///
/// Years −9999 to 9999, less about a day at each end. chrono reached
/// ±262,142 years; a value outside the new range is rejected when read.
///
/// ```
/// use dravr_tronc::dates::{Elapsed, Timestamp, SecondsFormat};
///
/// let start = Timestamp::from_epoch_millis(1_794_909_600_100)?;
/// assert_eq!(serde_json::to_string(&start).ok().as_deref(), Some("\"2026-11-17T10:00:00.100Z\""));
/// assert_eq!(start.to_rfc3339(), "2026-11-17T10:00:00.100+00:00");
/// assert_eq!(start.to_rfc3339_opts(SecondsFormat::Secs, true), "2026-11-17T10:00:00Z");
///
/// let week_later = start + Elapsed::days(7);
/// assert_eq!((week_later - start).num_seconds(), 604_800);
/// assert_eq!("2026-11-17 10:00:00.1 UTC".parse::<Timestamp>()?, start);
/// # Ok::<(), dravr_tronc::dates::DateError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub(super) EngineTimestamp);

impl Timestamp {
    /// Midnight UTC on 1 January 1970.
    pub const UNIX_EPOCH: Self = Self(EngineTimestamp::UNIX_EPOCH);
    /// The earliest timestamp: `-9999-01-02T01:59:59Z`.
    pub const MIN: Self = Self(EngineTimestamp::MIN);
    /// The latest timestamp: `9999-12-30T22:00:00.999999999Z`.
    pub const MAX: Self = Self(EngineTimestamp::MAX);

    /// The current timestamp, from the system clock.
    #[must_use]
    pub fn now() -> Self {
        // A system clock set outside the range reads as the nearest bound.
        let now = SystemTime::now();
        EngineTimestamp::try_from(now).map_or_else(
            |_| {
                if now < SystemTime::UNIX_EPOCH {
                    Self::MIN
                } else {
                    Self::MAX
                }
            },
            Self,
        )
    }

    /// The timestamp this many seconds after the Unix epoch.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] outside [`Timestamp::MIN`]..=[`Timestamp::MAX`].
    pub fn from_epoch_seconds(seconds: i64) -> Result<Self, DateError> {
        EngineTimestamp::from_second(seconds)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }

    /// The timestamp this many seconds and nanoseconds after the Unix epoch.
    /// The nanoseconds always count forward, as in chrono's `from_timestamp`.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] outside the range, or when `nanoseconds` is
    /// a second or more.
    pub fn from_epoch_seconds_and_nanos(seconds: i64, nanoseconds: u32) -> Result<Self, DateError> {
        if i128::from(nanoseconds) >= NANOS_PER_SECOND {
            return Err(DateError::OutOfRange);
        }
        Self::from_epoch_nanos(i128::from(seconds) * NANOS_PER_SECOND + i128::from(nanoseconds))
    }

    /// The timestamp this many milliseconds after the Unix epoch.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] outside [`Timestamp::MIN`]..=[`Timestamp::MAX`].
    pub fn from_epoch_millis(milliseconds: i64) -> Result<Self, DateError> {
        EngineTimestamp::from_millisecond(milliseconds)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }

    /// The timestamp this many microseconds after the Unix epoch.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] outside [`Timestamp::MIN`]..=[`Timestamp::MAX`].
    pub fn from_epoch_micros(microseconds: i64) -> Result<Self, DateError> {
        EngineTimestamp::from_microsecond(microseconds)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }

    /// The timestamp this many nanoseconds after the Unix epoch.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] outside [`Timestamp::MIN`]..=[`Timestamp::MAX`].
    pub fn from_epoch_nanos(nanoseconds: i128) -> Result<Self, DateError> {
        EngineTimestamp::from_nanosecond(nanoseconds)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }

    /// Whole seconds since the Unix epoch, rounded down: half a second
    /// before the epoch is −1, as chrono's `timestamp()` gives.
    #[must_use]
    pub fn epoch_seconds(self) -> i64 {
        self.floor_units(NANOS_PER_SECOND)
    }

    /// Whole milliseconds since the Unix epoch, rounded down.
    #[must_use]
    pub fn epoch_millis(self) -> i64 {
        self.floor_units(NANOS_PER_MILLI)
    }

    /// Whole microseconds since the Unix epoch, rounded down.
    #[must_use]
    pub fn epoch_micros(self) -> i64 {
        self.floor_units(NANOS_PER_MICRO)
    }

    /// Nanoseconds since the Unix epoch.
    #[must_use]
    pub fn epoch_nanos(self) -> i128 {
        self.0.as_nanosecond()
    }

    /// The part of the timestamp below its whole second, in nanoseconds: 0 to
    /// 999,999,999, also before the epoch.
    #[must_use]
    pub fn subsec_nanos(self) -> u32 {
        u32::try_from(self.0.as_nanosecond().rem_euclid(NANOS_PER_SECOND)).unwrap_or(0)
    }

    /// The part of the timestamp below its whole second, in milliseconds.
    #[must_use]
    pub fn subsec_millis(self) -> u32 {
        self.subsec_nanos() / 1_000_000
    }

    /// The part of the timestamp below its whole second, in microseconds.
    #[must_use]
    pub fn subsec_micros(self) -> u32 {
        self.subsec_nanos() / 1_000
    }

    fn floor_units(self, nanos_per_unit: i128) -> i64 {
        // The whole range in nanoseconds divided by a thousand or more fits
        // an i64 with room to spare.
        i64::try_from(self.0.as_nanosecond().div_euclid(nanos_per_unit)).unwrap_or(i64::MAX)
    }

    /// This timestamp `elapsed` later, or `None` outside the range.
    #[must_use]
    pub fn checked_add(self, elapsed: Elapsed) -> Option<Self> {
        self.0.checked_add(elapsed.0).ok().map(Self)
    }

    /// This timestamp `elapsed` earlier, or `None` outside the range.
    #[must_use]
    pub fn checked_sub(self, elapsed: Elapsed) -> Option<Self> {
        self.0.checked_sub(elapsed.0).ok().map(Self)
    }

    /// The exact length from `earlier` to this timestamp; negative when this
    /// timestamp comes first. The same as `self - earlier`.
    #[must_use]
    pub fn signed_duration_since(self, earlier: Self) -> Elapsed {
        Elapsed(self.0.duration_since(earlier.0))
    }

    fn utc(self) -> DateTime {
        Offset::UTC.to_datetime(self.0)
    }

    /// The calendar date in UTC. What `date_naive()` returned.
    #[must_use]
    pub fn utc_date(self) -> CivilDate {
        CivilDate(self.utc().date())
    }

    /// The time of day in UTC.
    #[must_use]
    pub fn utc_time(self) -> CivilTime {
        CivilTime(self.utc().time())
    }

    /// The date and time of day in UTC. What `naive_utc()` returned.
    #[must_use]
    pub fn utc_datetime(self) -> CivilDateTime {
        CivilDateTime(self.utc())
    }

    /// RFC 3339 ending in `+00:00`, with no fraction for a whole second and
    /// otherwise three, six or nine digits: `2026-11-17T10:00:00.100+00:00`.
    /// Byte for byte what chrono's `to_rfc3339()` writes for a UTC value.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.to_rfc3339_opts(SecondsFormat::AutoSi, false)
    }

    /// RFC 3339 with a chosen number of fraction digits, ending in `Z` when
    /// `use_z` is set and in `+00:00` otherwise. Byte for byte what chrono's
    /// `to_rfc3339_opts` writes for a UTC value.
    #[must_use]
    pub fn to_rfc3339_opts(self, seconds: SecondsFormat, use_z: bool) -> String {
        let mut out = String::with_capacity(35);
        // Writing to a String cannot fail.
        if self
            .write_rfc3339(&mut out, seconds.digits(), use_z)
            .is_err()
        {
            out.clear();
        }
        out
    }

    fn write_rfc3339(
        self,
        out: &mut impl fmt::Write,
        digits: FractionDigits,
        use_z: bool,
    ) -> fmt::Result {
        let civil = self.utc();
        text::write_date(out, civil.date())?;
        out.write_char('T')?;
        text::write_time(out, civil.time(), digits)?;
        out.write_str(if use_z { "Z" } else { "+00:00" })
    }

    /// Reads RFC 3339 strictly, as chrono's `parse_from_rfc3339` does: four,
    /// two and two digits for the date, `T`, `t` or a space, two digits each
    /// for the time, any number of fraction digits, and `Z`, `z` or `±hh:mm`.
    /// The offset is applied and dropped; see
    /// [`parse_from_rfc3339_with_offset`](Self::parse_from_rfc3339_with_offset)
    /// to keep it.
    ///
    /// # Errors
    ///
    /// [`DateError::InvalidText`] when the text is not RFC 3339, and
    /// [`DateError::OutOfRange`] when a component or the timestamp is out of
    /// range.
    pub fn parse_from_rfc3339(text: &str) -> Result<Self, DateError> {
        Self::parse_from_rfc3339_with_offset(text).map(|(timestamp, _)| timestamp)
    }

    /// Reads RFC 3339 strictly and also returns the offset the text carried,
    /// in seconds east of UTC. For the few callers that kept a
    /// `DateTime<FixedOffset>` to read the sender's local offset.
    ///
    /// # Errors
    ///
    /// As [`parse_from_rfc3339`](Self::parse_from_rfc3339).
    pub fn parse_from_rfc3339_with_offset(text: &str) -> Result<(Self, i32), DateError> {
        text::strict_rfc3339(text).map(|(timestamp, offset)| (Self(timestamp), offset))
    }

    /// Formats the UTC date and time with a `strftime` string, such as
    /// `"%Y-%m-%d %H:%M UTC"`. `%z` writes `+0000` and `%Z` writes `UTC`. The
    /// [module documentation](crate::dates#format-strings) lists the
    /// specifiers.
    ///
    /// # Errors
    ///
    /// [`DateError::UnsupportedFormat`] for a specifier that is not
    /// implemented.
    pub fn format(&self, format: &str) -> Result<String, DateError> {
        let civil = self.utc();
        text::format(
            &Moment {
                date: Some(civil.date()),
                time: Some(civil.time()),
                offset: Some((0, "UTC")),
            },
            format,
        )
    }

    /// Reads a timestamp written in a `strftime` format that includes an
    /// offset (`%z`, `%:z` or `%#z`), with chrono's leniency.
    ///
    /// # Errors
    ///
    /// [`DateError::InvalidText`] when the text does not match or carries no
    /// offset, [`DateError::OutOfRange`] when it is not a timestamp in range,
    /// and [`DateError::UnsupportedFormat`] for a specifier that cannot be
    /// read.
    pub fn parse_from_str(text: &str, format: &str) -> Result<Self, DateError> {
        text::parse(text, format)?
            .timestamp()
            .map(|(timestamp, _)| Self(timestamp))
    }

    /// As a `std::time::SystemTime`, or `None` when the platform's clock
    /// type cannot hold it.
    #[must_use]
    pub fn to_system_time(self) -> Option<SystemTime> {
        SystemTime::try_from(self.0).ok()
    }
}

impl Default for Timestamp {
    /// The Unix epoch, as `DateTime::<Utc>::default()` was.
    fn default() -> Self {
        Self::UNIX_EPOCH
    }
}

impl TryFrom<SystemTime> for Timestamp {
    type Error = DateError;

    fn try_from(time: SystemTime) -> Result<Self, DateError> {
        EngineTimestamp::try_from(time)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }
}

impl Add<Elapsed> for Timestamp {
    type Output = Self;

    /// Stops at [`Timestamp::MAX`] or [`Timestamp::MIN`]; it does not panic.
    fn add(self, elapsed: Elapsed) -> Self {
        self.checked_add(elapsed).unwrap_or_else(|| {
            if elapsed.is_negative() {
                Self::MIN
            } else {
                Self::MAX
            }
        })
    }
}

impl Sub<Elapsed> for Timestamp {
    type Output = Self;

    /// Stops at [`Timestamp::MIN`] or [`Timestamp::MAX`]; it does not panic.
    fn sub(self, elapsed: Elapsed) -> Self {
        self.checked_sub(elapsed).unwrap_or_else(|| {
            if elapsed.is_negative() {
                Self::MAX
            } else {
                Self::MIN
            }
        })
    }
}

impl AddAssign<Elapsed> for Timestamp {
    fn add_assign(&mut self, elapsed: Elapsed) {
        *self = *self + elapsed;
    }
}

impl SubAssign<Elapsed> for Timestamp {
    fn sub_assign(&mut self, elapsed: Elapsed) {
        *self = *self - elapsed;
    }
}

impl Sub for Timestamp {
    type Output = Elapsed;

    fn sub(self, earlier: Self) -> Elapsed {
        self.signed_duration_since(earlier)
    }
}

impl fmt::Display for Timestamp {
    /// `2026-11-17 10:00:00 UTC`, with chrono's fraction rule: what
    /// `DateTime<Utc>` displayed. For people and logs; the wire forms are
    /// serde and [`Timestamp::to_rfc3339`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let civil = self.utc();
        text::write_date(f, civil.date())?;
        f.write_str(" ")?;
        text::write_time(f, civil.time(), FractionDigits::Auto)?;
        f.write_str(" UTC")
    }
}

impl fmt::Debug for Timestamp {
    /// `2026-11-17T10:00:00Z`, with chrono's fraction rule.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write_rfc3339(f, FractionDigits::Auto, true)
    }
}

impl FromStr for Timestamp {
    type Err = DateError;

    /// Reads RFC 3339 with chrono's leniency: digits need no padding, the
    /// separator may be `T`, `t` or a space, whitespace may sit between
    /// components and after the value, `UTC` is an offset and the offset's
    /// colon may be missing.
    /// serde reads through this.
    fn from_str(text: &str) -> Result<Self, DateError> {
        text::relaxed_rfc3339(text).map(|(timestamp, _)| Self(timestamp))
    }
}
