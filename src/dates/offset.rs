// ABOUTME: Offset, a fixed distance from UTC in seconds: what chrono's FixedOffset was when it was a value
// ABOUTME: Reads what FixedOffset::from_str read and writes what its Display wrote, with chrono's range
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::str::FromStr;

use jiff::tz::Offset as EngineOffset;

use super::error::DateError;
use super::text;

/// An offset must be strictly less than a day from UTC, as chrono's
/// `FixedOffset::east_opt` required.
const SECONDS_PER_DAY: u32 = 86_400;

/// A fixed offset from UTC, to the second, less than a day either way.
///
/// Replaces `chrono::FixedOffset` where it was a value: the offset a sender
/// wrote a timestamp in, kept beside the [`Timestamp`](crate::dates::Timestamp)
/// it names ([`Timestamp::parse_with_offset`](crate::dates::Timestamp::parse_with_offset)),
/// or an offset read alone (`"+05:30".parse::<Offset>()`). As a zone it is
/// `Zone::from(offset)` (feature `zones`).
///
/// It is written as chrono's `FixedOffset` displayed itself: `+05:30`,
/// `-08:00`, `+00:00`, and `+05:45:30` when the offset has seconds.
///
/// ```
/// use dravr_tronc::dates::Offset;
///
/// let india: Offset = "+05:30".parse()?;
/// assert_eq!(india.seconds(), 19_800);
/// assert_eq!(india.to_string(), "+05:30");
/// assert_eq!("-0800".parse::<Offset>()?, Offset::from_seconds(-8 * 3600)?);
/// assert!("Z".parse::<Offset>().is_err());
/// # Ok::<(), dravr_tronc::dates::DateError>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Offset(pub(super) EngineOffset);

impl Offset {
    /// Zero: UTC itself.
    pub const UTC: Self = Self(EngineOffset::UTC);

    /// The offset `seconds` east of UTC; negative is west.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] unless `seconds` is less than a day from
    /// zero, the range chrono's `FixedOffset::east_opt` accepted.
    pub fn from_seconds(seconds: i32) -> Result<Self, DateError> {
        if seconds.unsigned_abs() >= SECONDS_PER_DAY {
            return Err(DateError::OutOfRange);
        }
        EngineOffset::from_seconds(seconds)
            .map(Self)
            .map_err(|_| DateError::OutOfRange)
    }

    /// Seconds east of UTC; negative is west. What chrono's
    /// `local_minus_utc()` returned.
    #[must_use]
    pub const fn seconds(self) -> i32 {
        self.0.seconds()
    }
}

impl Default for Offset {
    /// [`Offset::UTC`].
    fn default() -> Self {
        Self::UTC
    }
}

impl fmt::Display for Offset {
    /// `+05:30`, or `+05:45:30` with seconds: what chrono's `FixedOffset`
    /// displayed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.seconds();
        let sign = if seconds < 0 { '-' } else { '+' };
        let magnitude = seconds.unsigned_abs();
        let (hours, minutes, seconds) = (magnitude / 3600, magnitude / 60 % 60, magnitude % 60);
        if seconds == 0 {
            write!(f, "{sign}{hours:02}:{minutes:02}")
        } else {
            write!(f, "{sign}{hours:02}:{minutes:02}:{seconds:02}")
        }
    }
}

impl fmt::Debug for Offset {
    /// The same text as `Display`, as chrono's `Debug` was.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Offset {
    type Err = DateError;

    /// Reads an offset as chrono's `FixedOffset::from_str` did: a sign (`+`,
    /// `-`, or the minus sign U+2212), two digits of hours, then two of
    /// minutes after any run of colons and whitespace, including none. So
    /// `+05:30`, `+0530`, `+05 30` and `−05:00` are read, and `+05`, `+5:30`,
    /// `Z`, `UTC` and text with leading whitespace are not.
    ///
    /// As in chrono, whatever follows the minutes is not looked at:
    /// `+05:30:15` and `+05:30 ` read as `+05:30`.
    ///
    /// # Errors
    ///
    /// chrono's kind for the same text: [`DateError::TooShort`],
    /// [`DateError::InvalidText`], or [`DateError::OutOfRange`] for minutes
    /// past 59 or an offset of a day or more.
    fn from_str(text: &str) -> Result<Self, DateError> {
        let seconds = text::offset_alone(text)?;
        let seconds = i32::try_from(seconds).map_err(|_| DateError::OutOfRange)?;
        Self::from_seconds(seconds)
    }
}
