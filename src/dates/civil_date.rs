// ABOUTME: CivilDate, the Dravr-owned calendar date that replaces chrono's NaiveDate
// ABOUTME: Holds the calendar arithmetic (days, months, weeks) that a timestamp deliberately does not have
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::str::FromStr;

use jiff::civil::{Date, ISOWeekDate};
use jiff::Span;

use super::civil_datetime::CivilDateTime;
use super::civil_time::CivilTime;
use super::elapsed::Elapsed;
use super::error::DateError;
use super::text::{self, Moment};
use super::weekday::Weekday;

/// A calendar date with no time of day and no zone: `2026-11-17`.
///
/// Replaces `chrono::NaiveDate`. On the wire and in a database it is
/// `YYYY-MM-DD`, as before. Calendar arithmetic lives here: a day added to a
/// date is the next date, whatever the clocks did that night, and a month
/// added to 31 January is the last day of February.
///
/// The range is the years −9999 to 9999 ([`CivilDate::MIN`],
/// [`CivilDate::MAX`]); chrono's reached ±262,142. Nothing panics: arithmetic
/// that would leave the range returns `None` from the `checked_*` methods and
/// stops at the bound in the operators.
///
/// ```
/// use dravr_tronc::dates::{CivilDate, Elapsed, Weekday};
///
/// let date = CivilDate::from_ymd(2026, 1, 31)?;
/// assert_eq!(date.weekday(), Weekday::Sat);
/// assert_eq!(date.checked_add_months(1), Some(CivilDate::from_ymd(2026, 2, 28)?));
/// assert_eq!((date + Elapsed::days(1)).to_string(), "2026-02-01");
/// assert_eq!("2026-01-31".parse::<CivilDate>()?, date);
/// # Ok::<(), dravr_tronc::dates::DateError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDate(pub(super) Date);

impl CivilDate {
    /// The first date: 1 January −9999.
    pub const MIN: Self = Self(Date::MIN);
    /// The last date: 31 December 9999.
    pub const MAX: Self = Self(Date::MAX);

    /// The date with this year, month (1 to 12) and day of the month.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] when there is no such date.
    pub fn from_ymd(year: i32, month: u32, day: u32) -> Result<Self, DateError> {
        let year = i16::try_from(year).map_err(|_| DateError::OutOfRange)?;
        let month = i8::try_from(month).map_err(|_| DateError::OutOfRange)?;
        let day = i8::try_from(day).map_err(|_| DateError::OutOfRange)?;
        Date::new(year, month, day)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }

    /// The date of an ISO 8601 week: the week-numbering year, the week (1 to
    /// 53) and the day in it.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] when that year has no such week.
    pub fn from_iso_week(year: i32, week: u32, weekday: Weekday) -> Result<Self, DateError> {
        let year = i16::try_from(year).map_err(|_| DateError::OutOfRange)?;
        let week = i8::try_from(week).map_err(|_| DateError::OutOfRange)?;
        ISOWeekDate::new(year, week, weekday.to_engine())
            .map(|week_date| Self(Date::from_iso_week_date(week_date)))
            .map_err(|_| DateError::OutOfRange)
    }

    /// The year.
    #[must_use]
    pub fn year(self) -> i32 {
        i32::from(self.0.year())
    }

    /// The month, 1 to 12.
    #[must_use]
    pub fn month(self) -> u32 {
        u32::from(self.0.month().unsigned_abs())
    }

    /// The day of the month, 1 to 31.
    #[must_use]
    pub fn day(self) -> u32 {
        u32::from(self.0.day().unsigned_abs())
    }

    /// The day of the year, 1 to 366.
    #[must_use]
    pub fn ordinal(self) -> u32 {
        u32::from(self.0.day_of_year().unsigned_abs())
    }

    /// The day of the week.
    #[must_use]
    pub fn weekday(self) -> Weekday {
        Weekday::from_engine(self.0.weekday())
    }

    /// The ISO 8601 week: its week-numbering year and the week, 1 to 53. The
    /// year differs from [`year`](Self::year) for a few days around 1 January.
    #[must_use]
    pub fn iso_week(self) -> (i32, u32) {
        let week_date = self.0.iso_week_date();
        (
            i32::from(week_date.year()),
            u32::from(week_date.week().unsigned_abs()),
        )
    }

    /// How many days this date's month has.
    #[must_use]
    pub fn days_in_month(self) -> u32 {
        u32::from(self.0.days_in_month().unsigned_abs())
    }

    /// Whether this date's year has a 29 February.
    #[must_use]
    pub fn is_leap_year(self) -> bool {
        self.0.in_leap_year()
    }

    /// The first day of this date's month.
    #[must_use]
    pub fn first_of_month(self) -> Self {
        Self(self.0.first_of_month())
    }

    /// The last day of this date's month.
    #[must_use]
    pub fn last_of_month(self) -> Self {
        Self(self.0.last_of_month())
    }

    /// The next date, or `None` after [`CivilDate::MAX`].
    #[must_use]
    pub fn succ(self) -> Option<Self> {
        self.0.tomorrow().ok().map(Self)
    }

    /// The previous date, or `None` before [`CivilDate::MIN`].
    #[must_use]
    pub fn pred(self) -> Option<Self> {
        self.0.yesterday().ok().map(Self)
    }

    /// This date `days` calendar days later (earlier when negative), or
    /// `None` outside the range.
    #[must_use]
    pub fn checked_add_days(self, days: i64) -> Option<Self> {
        let span = Span::new().try_days(days).ok()?;
        self.0.checked_add(span).ok().map(Self)
    }

    /// This date `days` calendar days earlier, or `None` outside the range.
    #[must_use]
    pub fn checked_sub_days(self, days: i64) -> Option<Self> {
        self.checked_add_days(days.checked_neg()?)
    }

    /// The same day of the month `months` months later, or the last day of
    /// that month when it is shorter: 31 January plus one month is 28 or 29
    /// February. `None` outside the range.
    #[must_use]
    pub fn checked_add_months(self, months: u32) -> Option<Self> {
        self.shifted_by_months(i64::from(months))
    }

    /// The same day of the month `months` months earlier, or the last day of
    /// that month when it is shorter. `None` outside the range.
    #[must_use]
    pub fn checked_sub_months(self, months: u32) -> Option<Self> {
        self.shifted_by_months(-i64::from(months))
    }

    /// The same day of the month `months` months away, or the last day of
    /// that month when it is shorter. Counted in whole months from year 0,
    /// so one end of the range reaches the other; the engine's own month
    /// span stops short of that.
    fn shifted_by_months(self, months: i64) -> Option<Self> {
        let index = i64::from(self.0.year()) * 12 + i64::from(self.0.month()) - 1 + months;
        let year = i16::try_from(index.div_euclid(12)).ok()?;
        let month = i8::try_from(index.rem_euclid(12) + 1).ok()?;
        let first = Date::new(year, month, 1).ok()?;
        let day = self.0.day().min(first.days_in_month());
        Date::new(year, month, day).ok().map(Self)
    }

    /// This date in another year, or `None` when that year has no such date
    /// (29 February in a common year) or is out of range.
    #[must_use]
    pub fn with_year(self, year: i32) -> Option<Self> {
        let year = i16::try_from(year).ok()?;
        self.0.with().year(year).build().ok().map(Self)
    }

    /// This date in another month of its year, or `None` when that month has
    /// no such day.
    #[must_use]
    pub fn with_month(self, month: u32) -> Option<Self> {
        let month = i8::try_from(month).ok()?;
        self.0.with().month(month).build().ok().map(Self)
    }

    /// Another day of this date's month, or `None` when the month has no
    /// such day.
    #[must_use]
    pub fn with_day(self, day: u32) -> Option<Self> {
        let day = i8::try_from(day).ok()?;
        self.0.with().day(day).build().ok().map(Self)
    }

    /// The first day of the week this date is in, for a week that starts on
    /// `first`. Stops at [`CivilDate::MIN`].
    #[must_use]
    pub fn start_of_week(self, first: Weekday) -> Self {
        let back = i64::from(self.weekday().days_since(first));
        self.checked_sub_days(back).unwrap_or(Self::MIN)
    }

    /// Calendar days from `earlier` to this date; negative when this date
    /// comes first.
    #[must_use]
    pub fn days_since(self, earlier: Self) -> i64 {
        self.signed_duration_since(earlier).num_days()
    }

    /// The length from `earlier` to this date, a whole number of 86,400
    /// second days; negative when this date comes first.
    #[must_use]
    pub fn signed_duration_since(self, earlier: Self) -> Elapsed {
        Elapsed(self.0.duration_since(earlier.0))
    }

    /// This date at a time of day.
    #[must_use]
    pub const fn and_time(self, time: CivilTime) -> CivilDateTime {
        CivilDateTime(self.0.to_datetime(time.0))
    }

    /// This date at an hour, minute and second.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] when that is not a time of day.
    pub fn and_hms(self, hour: u32, minute: u32, second: u32) -> Result<CivilDateTime, DateError> {
        Ok(self.and_time(CivilTime::from_hms(hour, minute, second)?))
    }

    /// This date at 00:00:00.
    #[must_use]
    pub const fn at_midnight(self) -> CivilDateTime {
        self.and_time(CivilTime::MIDNIGHT)
    }

    /// Formats with a `strftime` string, such as `"%Y-%m-%d"` or `"%a %-d %b"`.
    /// The [module documentation](crate::dates#format-strings) lists the
    /// specifiers.
    ///
    /// # Errors
    ///
    /// [`DateError::UnsupportedFormat`] for a specifier that is not
    /// implemented or that a date has no value for, such as `%H`.
    pub fn format(&self, format: &str) -> Result<String, DateError> {
        text::format(
            &Moment {
                date: Some(self.0),
                time: None,
                offset: None,
            },
            format,
        )
    }

    /// Reads a date written in a `strftime` format, with chrono's leniency:
    /// numbers need no padding and may follow whitespace.
    ///
    /// # Errors
    ///
    /// [`DateError::InvalidText`] when the text does not match,
    /// [`DateError::OutOfRange`] when it is not a date, and
    /// [`DateError::UnsupportedFormat`] for a specifier that cannot be read.
    pub fn parse_from_str(text: &str, format: &str) -> Result<Self, DateError> {
        text::parse(text, format)?.date().map(Self)
    }
}

impl Default for CivilDate {
    /// 1 January 1970, as `NaiveDate::default()` was.
    fn default() -> Self {
        Self(Date::constant(1970, 1, 1))
    }
}

impl Add<Elapsed> for CivilDate {
    type Output = Self;

    /// Adds the whole days of `elapsed`, dropping any part of a day, as
    /// `NaiveDate + Duration` did. Stops at the bounds of the range.
    fn add(self, elapsed: Elapsed) -> Self {
        let days = elapsed.num_days();
        self.checked_add_days(days)
            .unwrap_or(if days < 0 { Self::MIN } else { Self::MAX })
    }
}

impl Sub<Elapsed> for CivilDate {
    type Output = Self;

    /// Subtracts the whole days of `elapsed`. Stops at the bounds of the range.
    fn sub(self, elapsed: Elapsed) -> Self {
        self + (-elapsed)
    }
}

impl AddAssign<Elapsed> for CivilDate {
    fn add_assign(&mut self, elapsed: Elapsed) {
        *self = *self + elapsed;
    }
}

impl SubAssign<Elapsed> for CivilDate {
    fn sub_assign(&mut self, elapsed: Elapsed) {
        *self = *self - elapsed;
    }
}

impl Sub for CivilDate {
    type Output = Elapsed;

    fn sub(self, earlier: Self) -> Elapsed {
        self.signed_duration_since(earlier)
    }
}

impl fmt::Display for CivilDate {
    /// `YYYY-MM-DD`, the wire form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        text::write_date(f, self.0)
    }
}

impl fmt::Debug for CivilDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for CivilDate {
    type Err = DateError;

    /// Reads `YYYY-MM-DD` the way chrono does: digits need no padding and
    /// whitespace may surround the dashes and follow the date.
    fn from_str(text: &str) -> Result<Self, DateError> {
        text::relaxed_date(text).map(Self)
    }
}
