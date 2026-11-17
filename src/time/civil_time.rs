// ABOUTME: CivilTime, the Dravr-owned time of day that replaces chrono's NaiveTime
// ABOUTME: A wall-clock reading with nanosecond precision, no date and no zone; arithmetic wraps at midnight
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::str::FromStr;

use jiff::civil::Time;

use super::elapsed::Elapsed;
use super::error::TimeError;
use super::text::{self, FractionDigits, Moment};

/// A time of day with no date and no zone: `07:30:00`.
///
/// Replaces `chrono::NaiveTime`. On the wire it is `HH:MM:SS`, followed by
/// three, six or nine fraction digits when the second is not whole, as
/// chrono writes it. Unlike chrono it holds no leap second: `23:59:60` reads
/// as `23:59:59`.
///
/// ```
/// use dravr_tronc::time::{CivilTime, Elapsed};
///
/// let time = CivilTime::from_hms(23, 30, 0)?;
/// assert_eq!((time + Elapsed::hours(1)).to_string(), "00:30:00");
/// assert_eq!("7:05".parse::<CivilTime>()?, CivilTime::from_hms(7, 5, 0)?);
/// assert_eq!(CivilTime::from_hms_milli(7, 5, 0, 250)?.to_string(), "07:05:00.250");
/// # Ok::<(), dravr_tronc::time::TimeError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CivilTime(pub(super) Time);

impl CivilTime {
    /// 00:00:00, the first time of the day.
    pub const MIDNIGHT: Self = Self(Time::midnight());
    /// 23:59:59.999999999, the last time of the day.
    pub const MAX: Self = Self(Time::MAX);

    /// The time with this hour (0 to 23), minute and second (0 to 59).
    ///
    /// # Errors
    ///
    /// [`TimeError::OutOfRange`] when a component is out of range.
    pub fn from_hms(hour: u32, minute: u32, second: u32) -> Result<Self, TimeError> {
        Self::from_hms_nano(hour, minute, second, 0)
    }

    /// The time with this hour, minute, second and millisecond (0 to 999).
    ///
    /// # Errors
    ///
    /// [`TimeError::OutOfRange`] when a component is out of range.
    pub fn from_hms_milli(
        hour: u32,
        minute: u32,
        second: u32,
        millisecond: u32,
    ) -> Result<Self, TimeError> {
        let nanosecond = millisecond
            .checked_mul(1_000_000)
            .ok_or(TimeError::OutOfRange)?;
        Self::from_hms_nano(hour, minute, second, nanosecond)
    }

    /// The time with this hour, minute, second and microsecond (0 to 999,999).
    ///
    /// # Errors
    ///
    /// [`TimeError::OutOfRange`] when a component is out of range.
    pub fn from_hms_micro(
        hour: u32,
        minute: u32,
        second: u32,
        microsecond: u32,
    ) -> Result<Self, TimeError> {
        let nanosecond = microsecond
            .checked_mul(1_000)
            .ok_or(TimeError::OutOfRange)?;
        Self::from_hms_nano(hour, minute, second, nanosecond)
    }

    /// The time with this hour, minute, second and nanosecond
    /// (0 to 999,999,999).
    ///
    /// # Errors
    ///
    /// [`TimeError::OutOfRange`] when a component is out of range.
    pub fn from_hms_nano(
        hour: u32,
        minute: u32,
        second: u32,
        nanosecond: u32,
    ) -> Result<Self, TimeError> {
        let hour = i8::try_from(hour).map_err(|_| TimeError::OutOfRange)?;
        let minute = i8::try_from(minute).map_err(|_| TimeError::OutOfRange)?;
        let second = i8::try_from(second).map_err(|_| TimeError::OutOfRange)?;
        let nanosecond = i32::try_from(nanosecond).map_err(|_| TimeError::OutOfRange)?;
        Time::new(hour, minute, second, nanosecond)
            .map(Self)
            .map_err(|_| TimeError::OutOfRange)
    }

    /// The time this many seconds after midnight (0 to 86,399), with a
    /// nanosecond part.
    ///
    /// # Errors
    ///
    /// [`TimeError::OutOfRange`] when that is past the end of the day.
    pub fn from_num_seconds_from_midnight(
        seconds: u32,
        nanosecond: u32,
    ) -> Result<Self, TimeError> {
        Self::from_hms_nano(seconds / 3_600, seconds / 60 % 60, seconds % 60, nanosecond)
    }

    /// The hour, 0 to 23.
    #[must_use]
    pub fn hour(self) -> u32 {
        u32::from(self.0.hour().unsigned_abs())
    }

    /// The minute, 0 to 59.
    #[must_use]
    pub fn minute(self) -> u32 {
        u32::from(self.0.minute().unsigned_abs())
    }

    /// The second, 0 to 59.
    #[must_use]
    pub fn second(self) -> u32 {
        u32::from(self.0.second().unsigned_abs())
    }

    /// The part below one second, in nanoseconds: 0 to 999,999,999.
    #[must_use]
    pub fn nanosecond(self) -> u32 {
        self.0.subsec_nanosecond().unsigned_abs()
    }

    /// Whole seconds since midnight, 0 to 86,399.
    #[must_use]
    pub fn num_seconds_from_midnight(self) -> u32 {
        self.hour() * 3_600 + self.minute() * 60 + self.second()
    }

    /// The length from `earlier` to this time, within one day; negative when
    /// this time is the earlier one.
    #[must_use]
    pub fn signed_duration_since(self, earlier: Self) -> Elapsed {
        Elapsed(self.0.duration_since(earlier.0))
    }

    /// Formats with a `strftime` string, such as `"%H:%M"`. The
    /// [module documentation](crate::time#format-strings) lists the
    /// specifiers.
    ///
    /// # Errors
    ///
    /// [`TimeError::UnsupportedFormat`] for a specifier that is not
    /// implemented or that a time has no value for, such as `%Y`.
    pub fn format(&self, format: &str) -> Result<String, TimeError> {
        text::format(
            &Moment {
                date: None,
                time: Some(self.0),
                offset: None,
            },
            format,
        )
    }

    /// Reads a time written in a `strftime` format, with chrono's leniency.
    /// The hour and minute are required; a missing second is zero.
    ///
    /// # Errors
    ///
    /// [`TimeError::InvalidText`] when the text does not match,
    /// [`TimeError::OutOfRange`] when it is not a time, and
    /// [`TimeError::UnsupportedFormat`] for a specifier that cannot be read.
    pub fn parse_from_str(text: &str, format: &str) -> Result<Self, TimeError> {
        text::parse(text, format)?.time().map(Self)
    }
}

impl Add<Elapsed> for CivilTime {
    type Output = Self;

    /// Wraps around midnight, as `NaiveTime + Duration` did.
    fn add(self, elapsed: Elapsed) -> Self {
        Self(self.0.wrapping_add(elapsed.0))
    }
}

impl Sub<Elapsed> for CivilTime {
    type Output = Self;

    /// Wraps around midnight.
    fn sub(self, elapsed: Elapsed) -> Self {
        Self(self.0.wrapping_sub(elapsed.0))
    }
}

impl AddAssign<Elapsed> for CivilTime {
    fn add_assign(&mut self, elapsed: Elapsed) {
        *self = *self + elapsed;
    }
}

impl SubAssign<Elapsed> for CivilTime {
    fn sub_assign(&mut self, elapsed: Elapsed) {
        *self = *self - elapsed;
    }
}

impl Sub for CivilTime {
    type Output = Elapsed;

    fn sub(self, earlier: Self) -> Elapsed {
        self.signed_duration_since(earlier)
    }
}

impl fmt::Display for CivilTime {
    /// `HH:MM:SS` and, for a second that is not whole, three, six or nine
    /// fraction digits: the wire form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        text::write_time(f, self.0, FractionDigits::Auto)
    }
}

impl fmt::Debug for CivilTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for CivilTime {
    type Err = TimeError;

    /// Reads `HH:MM` or `HH:MM:SS` with an optional fraction, the way chrono
    /// does: digits need no padding and whitespace may surround the colons.
    fn from_str(text: &str) -> Result<Self, TimeError> {
        text::relaxed_time(text).map(Self)
    }
}
