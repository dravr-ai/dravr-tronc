// ABOUTME: Elapsed, the Dravr-owned signed length of time that replaces chrono's Duration and TimeDelta
// ABOUTME: A day is exactly 86,400 seconds here; calendar units live on CivilDate, never on a timestamp
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};
use std::time::Duration;

use jiff::SignedDuration;

use super::error::DateError;

const SECONDS_PER_MINUTE: i64 = 60;
const SECONDS_PER_HOUR: i64 = 3_600;
const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_WEEK: i64 = 604_800;

/// A signed, exact length of time with nanosecond precision.
///
/// Replaces `chrono::Duration` / `chrono::TimeDelta`. Every unit is a fixed
/// number of seconds: [`Elapsed::days(1)`](Self::days) is 86,400 seconds
/// whatever the calendar says, which is what `chrono::Duration::days` meant.
/// A length in months or years is not an `Elapsed`; that arithmetic is on
/// [`CivilDate`](crate::dates::CivilDate).
///
/// Nothing here panics. The plain constructors and the operators saturate at
/// [`Elapsed::MIN`] and [`Elapsed::MAX`] (about ±292 billion years); the
/// `try_*` and `checked_*` forms report the overflow instead.
///
/// ```
/// use dravr_tronc::dates::Elapsed;
///
/// let week = Elapsed::days(7);
/// assert_eq!(week, Elapsed::weeks(1));
/// assert_eq!(week.num_seconds(), 604_800);
/// assert_eq!((Elapsed::hours(36) - Elapsed::days(2)).num_hours(), -12);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Elapsed(pub(super) SignedDuration);

impl Elapsed {
    /// No time at all.
    pub const ZERO: Self = Self(SignedDuration::ZERO);
    /// The longest negative length.
    pub const MIN: Self = Self(SignedDuration::MIN);
    /// The longest positive length.
    pub const MAX: Self = Self(SignedDuration::MAX);

    const fn saturated(seconds: Option<i64>, negative: bool) -> Self {
        match seconds {
            Some(seconds) => Self(SignedDuration::from_secs(seconds)),
            None if negative => Self::MIN,
            None => Self::MAX,
        }
    }

    /// `weeks` × 604,800 seconds, saturating.
    #[must_use]
    pub const fn weeks(weeks: i64) -> Self {
        Self::saturated(weeks.checked_mul(SECONDS_PER_WEEK), weeks < 0)
    }

    /// `days` × 86,400 seconds, saturating. Not a calendar day: across a
    /// daylight-saving change the local clock reads an hour off.
    #[must_use]
    pub const fn days(days: i64) -> Self {
        Self::saturated(days.checked_mul(SECONDS_PER_DAY), days < 0)
    }

    /// `hours` × 3,600 seconds, saturating.
    #[must_use]
    pub const fn hours(hours: i64) -> Self {
        Self::saturated(hours.checked_mul(SECONDS_PER_HOUR), hours < 0)
    }

    /// `minutes` × 60 seconds, saturating.
    #[must_use]
    pub const fn minutes(minutes: i64) -> Self {
        Self::saturated(minutes.checked_mul(SECONDS_PER_MINUTE), minutes < 0)
    }

    /// A number of seconds.
    #[must_use]
    pub const fn seconds(seconds: i64) -> Self {
        Self(SignedDuration::from_secs(seconds))
    }

    /// A number of milliseconds.
    #[must_use]
    pub const fn milliseconds(milliseconds: i64) -> Self {
        Self(SignedDuration::from_millis(milliseconds))
    }

    /// A number of microseconds.
    #[must_use]
    pub const fn microseconds(microseconds: i64) -> Self {
        Self(SignedDuration::from_micros(microseconds))
    }

    /// A number of nanoseconds.
    #[must_use]
    pub const fn nanoseconds(nanoseconds: i64) -> Self {
        Self(SignedDuration::from_nanos(nanoseconds))
    }

    /// `weeks` × 604,800 seconds, or `None` when that overflows.
    #[must_use]
    pub const fn try_weeks(weeks: i64) -> Option<Self> {
        match weeks.checked_mul(SECONDS_PER_WEEK) {
            Some(seconds) => Some(Self::seconds(seconds)),
            None => None,
        }
    }

    /// `days` × 86,400 seconds, or `None` when that overflows.
    #[must_use]
    pub const fn try_days(days: i64) -> Option<Self> {
        match days.checked_mul(SECONDS_PER_DAY) {
            Some(seconds) => Some(Self::seconds(seconds)),
            None => None,
        }
    }

    /// `hours` × 3,600 seconds, or `None` when that overflows.
    #[must_use]
    pub const fn try_hours(hours: i64) -> Option<Self> {
        match hours.checked_mul(SECONDS_PER_HOUR) {
            Some(seconds) => Some(Self::seconds(seconds)),
            None => None,
        }
    }

    /// `minutes` × 60 seconds, or `None` when that overflows.
    #[must_use]
    pub const fn try_minutes(minutes: i64) -> Option<Self> {
        match minutes.checked_mul(SECONDS_PER_MINUTE) {
            Some(seconds) => Some(Self::seconds(seconds)),
            None => None,
        }
    }

    /// Whole weeks, truncated toward zero.
    #[must_use]
    pub const fn num_weeks(&self) -> i64 {
        self.0.as_secs() / SECONDS_PER_WEEK
    }

    /// Whole days of 86,400 seconds, truncated toward zero: 47 hours is one
    /// day, and −47 hours is minus one.
    #[must_use]
    pub const fn num_days(&self) -> i64 {
        self.0.as_secs() / SECONDS_PER_DAY
    }

    /// Whole hours, truncated toward zero.
    #[must_use]
    pub const fn num_hours(&self) -> i64 {
        self.0.as_secs() / SECONDS_PER_HOUR
    }

    /// Whole minutes, truncated toward zero.
    #[must_use]
    pub const fn num_minutes(&self) -> i64 {
        self.0.as_secs() / SECONDS_PER_MINUTE
    }

    /// Whole seconds, truncated toward zero.
    #[must_use]
    pub const fn num_seconds(&self) -> i64 {
        self.0.as_secs()
    }

    /// Whole milliseconds, truncated toward zero; saturates beyond `i64`
    /// (±292 million years).
    #[must_use]
    pub fn num_milliseconds(&self) -> i64 {
        saturate(self.0.as_millis())
    }

    /// Whole microseconds, truncated toward zero, or `None` beyond `i64`
    /// (±292,000 years).
    #[must_use]
    pub fn num_microseconds(&self) -> Option<i64> {
        i64::try_from(self.0.as_micros()).ok()
    }

    /// Nanoseconds, or `None` beyond `i64` (±292 years).
    #[must_use]
    pub fn num_nanoseconds(&self) -> Option<i64> {
        i64::try_from(self.0.as_nanos()).ok()
    }

    /// The part below one second, in nanoseconds. It has the sign of the
    /// whole: −1.5 seconds is −1 second and −500,000,000 nanoseconds.
    #[must_use]
    pub const fn subsec_nanos(&self) -> i32 {
        self.0.subsec_nanos()
    }

    /// The length in seconds as a float.
    #[must_use]
    pub fn as_seconds_f64(&self) -> f64 {
        self.0.as_secs_f64()
    }

    /// Whether this is no time at all.
    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    /// Whether this is less than no time.
    #[must_use]
    pub const fn is_negative(&self) -> bool {
        self.0.is_negative()
    }

    /// The length without its sign, saturating: `MIN` has no positive twin.
    #[must_use]
    pub fn abs(&self) -> Self {
        self.0
            .checked_neg()
            .map_or(Self::MAX, |negated| Self(negated.max(self.0)))
    }

    /// The sum, or `None` on overflow.
    #[must_use]
    pub const fn checked_add(&self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(sum) => Some(Self(sum)),
            None => None,
        }
    }

    /// The difference, or `None` on overflow.
    #[must_use]
    pub const fn checked_sub(&self, other: Self) -> Option<Self> {
        match self.0.checked_sub(other.0) {
            Some(difference) => Some(Self(difference)),
            None => None,
        }
    }

    /// The product, or `None` on overflow.
    #[must_use]
    pub const fn checked_mul(&self, factor: i32) -> Option<Self> {
        match self.0.checked_mul(factor) {
            Some(product) => Some(Self(product)),
            None => None,
        }
    }

    /// The quotient, or `None` when `divisor` is zero or it overflows.
    #[must_use]
    pub const fn checked_div(&self, divisor: i32) -> Option<Self> {
        match self.0.checked_div(divisor) {
            Some(quotient) => Some(Self(quotient)),
            None => None,
        }
    }

    /// As a `std::time::Duration`, which cannot be negative.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] when this is negative.
    pub fn to_std(&self) -> Result<Duration, DateError> {
        Duration::try_from(self.0).map_err(|_| DateError::OutOfRange)
    }

    /// From a `std::time::Duration`.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] when it is longer than [`Elapsed::MAX`].
    pub fn from_std(duration: Duration) -> Result<Self, DateError> {
        SignedDuration::try_from(duration)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }
}

fn saturate(value: i128) -> i64 {
    i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
}

impl Add for Elapsed {
    type Output = Self;

    /// Saturating.
    fn add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl Sub for Elapsed {
    type Output = Self;

    /// Saturating.
    fn sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl AddAssign for Elapsed {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl SubAssign for Elapsed {
    fn sub_assign(&mut self, other: Self) {
        *self = *self - other;
    }
}

impl Neg for Elapsed {
    type Output = Self;

    /// Saturating: the negation of `MIN` is `MAX`.
    fn neg(self) -> Self {
        self.0.checked_neg().map_or(Self::MAX, Self)
    }
}

impl Mul<i32> for Elapsed {
    type Output = Self;

    /// Saturating.
    fn mul(self, factor: i32) -> Self {
        Self(self.0.saturating_mul(factor))
    }
}

impl fmt::Debug for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Elapsed {
    /// Seconds with the fraction that is there, such as `90s`, `-0.5s` or
    /// `1.000000001s`. For people and logs; it is not a wire format.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = self.0.as_nanos();
        let sign = if nanos < 0 { "-" } else { "" };
        let magnitude = nanos.unsigned_abs();
        let (seconds, fraction) = (magnitude / 1_000_000_000, magnitude % 1_000_000_000);
        if fraction == 0 {
            write!(f, "{sign}{seconds}s")
        } else {
            let digits = format!("{fraction:09}");
            write!(f, "{sign}{seconds}.{}s", digits.trim_end_matches('0'))
        }
    }
}
