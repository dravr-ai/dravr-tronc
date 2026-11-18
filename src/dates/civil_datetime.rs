// ABOUTME: CivilDateTime, the Dravr-owned date and time of day that replaces chrono's NaiveDateTime
// ABOUTME: A wall-clock reading with no zone; it becomes a timestamp only through UTC or a Zone
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::str::FromStr;

use jiff::civil::DateTime;
use jiff::tz::Offset;

use super::civil_date::CivilDate;
use super::civil_time::CivilTime;
use super::elapsed::Elapsed;
use super::error::DateError;
use super::text::{self, FractionDigits, Moment};
use super::timestamp::Timestamp;

/// A calendar date and a time of day, with no zone: `2026-11-17T07:30:00`.
///
/// Replaces `chrono::NaiveDateTime`. It is what a wall clock and a calendar
/// show somewhere, without saying where, so it is not a point on the time
/// line. [`and_utc`](Self::and_utc) reads it as UTC, and a
/// `Zone` (feature `zones`) resolves it in a named zone, where it may
/// exist once, twice or not at all.
///
/// On the wire it is `YYYY-MM-DDTHH:MM:SS` with chrono's fraction rule; in
/// SQLite it is `YYYY-MM-DD HH:MM:SS`.
///
/// ```
/// use dravr_tronc::dates::{CivilDate, CivilDateTime, Elapsed};
///
/// let local = CivilDate::from_ymd(2026, 11, 17)?.and_hms(7, 30, 0)?;
/// assert_eq!(format!("{local:?}"), "2026-11-17T07:30:00");
/// assert_eq!(local.to_string(), "2026-11-17 07:30:00");
/// assert_eq!((local + Elapsed::hours(20)).date(), CivilDate::from_ymd(2026, 11, 18)?);
/// assert_eq!(local.and_utc()?.to_rfc3339(), "2026-11-17T07:30:00+00:00");
/// # Ok::<(), dravr_tronc::dates::DateError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDateTime(pub(super) DateTime);

impl CivilDateTime {
    /// The first representable reading: midnight on [`CivilDate::MIN`].
    pub const MIN: Self = Self(DateTime::MIN);
    /// The last representable reading: the last nanosecond of
    /// [`CivilDate::MAX`].
    pub const MAX: Self = Self(DateTime::MAX);

    /// A date at a time of day.
    #[must_use]
    pub const fn new(date: CivilDate, time: CivilTime) -> Self {
        Self(DateTime::from_parts(date.0, time.0))
    }

    /// The calendar date.
    #[must_use]
    pub fn date(self) -> CivilDate {
        CivilDate(self.0.date())
    }

    /// The time of day.
    #[must_use]
    pub fn time(self) -> CivilTime {
        CivilTime(self.0.time())
    }

    /// The timestamp at which UTC clocks show this reading.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] within a day or two of the ends of the
    /// range, where [`Timestamp`] reaches less far than the calendar.
    pub fn and_utc(self) -> Result<Timestamp, DateError> {
        Offset::UTC
            .to_timestamp(self.0)
            .map(Timestamp)
            .map_err(|_| DateError::OutOfRange)
    }

    /// This reading `elapsed` later on the same clock, or `None` outside the
    /// range. Exact time, not calendar time: a day is 24 hours.
    #[must_use]
    pub fn checked_add(self, elapsed: Elapsed) -> Option<Self> {
        self.0.checked_add(elapsed.0).ok().map(Self)
    }

    /// This reading `elapsed` earlier on the same clock, or `None` outside
    /// the range.
    #[must_use]
    pub fn checked_sub(self, elapsed: Elapsed) -> Option<Self> {
        self.0.checked_sub(elapsed.0).ok().map(Self)
    }

    /// The length from `earlier` to this reading on one clock; negative when
    /// this reading comes first.
    #[must_use]
    pub fn signed_duration_since(self, earlier: Self) -> Elapsed {
        Elapsed(self.0.duration_since(earlier.0))
    }

    /// Formats with a `strftime` string, such as `"%Y-%m-%d %H:%M"`. The
    /// [module documentation](crate::dates#format-strings) lists the
    /// specifiers.
    ///
    /// # Errors
    ///
    /// [`DateError::UnsupportedFormat`] for a specifier that is not
    /// implemented or that needs a zone, such as `%z`.
    pub fn format(&self, format: &str) -> Result<String, DateError> {
        text::format(
            &Moment {
                date: Some(self.0.date()),
                time: Some(self.0.time()),
                offset: None,
            },
            format,
        )
    }

    /// Reads a date and time written in a `strftime` format, with chrono's
    /// leniency. The date, hour and minute are required; a missing second is
    /// zero; an offset in the text is read and ignored.
    ///
    /// # Errors
    ///
    /// [`DateError::InvalidText`] when the text does not match,
    /// [`DateError::OutOfRange`] when it is not a date and time, and
    /// [`DateError::UnsupportedFormat`] for a specifier that cannot be read.
    pub fn parse_from_str(text: &str, format: &str) -> Result<Self, DateError> {
        text::parse(text, format)?.datetime().map(Self)
    }
}

impl Default for CivilDateTime {
    /// Midnight on 1 January 1970, as `NaiveDateTime::default()` was.
    fn default() -> Self {
        Self(DateTime::constant(1970, 1, 1, 0, 0, 0, 0))
    }
}

impl Add<Elapsed> for CivilDateTime {
    type Output = Self;

    /// Stops at the bounds of the range.
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

impl Sub<Elapsed> for CivilDateTime {
    type Output = Self;

    /// Stops at the bounds of the range.
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

impl AddAssign<Elapsed> for CivilDateTime {
    fn add_assign(&mut self, elapsed: Elapsed) {
        *self = *self + elapsed;
    }
}

impl SubAssign<Elapsed> for CivilDateTime {
    fn sub_assign(&mut self, elapsed: Elapsed) {
        *self = *self - elapsed;
    }
}

impl Sub for CivilDateTime {
    type Output = Elapsed;

    fn sub(self, earlier: Self) -> Elapsed {
        self.signed_duration_since(earlier)
    }
}

impl fmt::Display for CivilDateTime {
    /// `YYYY-MM-DD HH:MM:SS` with chrono's fraction rule: the form for
    /// people, and the one SQLite stores.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        text::write_date(f, self.0.date())?;
        f.write_str(" ")?;
        text::write_time(f, self.0.time(), FractionDigits::Auto)
    }
}

impl fmt::Debug for CivilDateTime {
    /// `YYYY-MM-DDTHH:MM:SS` with chrono's fraction rule: the wire form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        text::write_date(f, self.0.date())?;
        f.write_str("T")?;
        text::write_time(f, self.0.time(), FractionDigits::Auto)
    }
}

impl FromStr for CivilDateTime {
    type Err = DateError;

    /// Reads `YYYY-MM-DDTHH:MM:SS` with an optional fraction, the way chrono
    /// does: an upper-case `T`, digits that need no padding, and whitespace
    /// allowed around the separators.
    fn from_str(text: &str) -> Result<Self, DateError> {
        text::relaxed_datetime(text).map(Self)
    }
}
