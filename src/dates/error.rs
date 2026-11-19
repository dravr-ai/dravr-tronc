// ABOUTME: The one error the date types return: text that is not a date, a value out of range, an unknown zone
// ABOUTME: Its messages are chrono's for the same rejection, so what a client reads does not change with the move
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::fmt;

/// Why a date operation could not produce a value.
///
/// Every fallible operation of [`dravr_tronc::dates`](crate::dates) returns
/// this; none of them panics.
///
/// Reading text fails with the kind chrono reported for the same input, and
/// the message is chrono's, word for word: a service that answered a client
/// with a chrono parse error answers with the same text. Each variant names
/// the chrono message it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DateError {
    /// A character where the reader expected another one: a wrong
    /// separator, a letter in a number, an unknown month name.
    /// "input contains invalid characters".
    InvalidText,
    /// The text ended before the value did. "premature end of input".
    TooShort,
    /// Characters are left over after the value. "trailing input".
    TooLong,
    /// The text read well but names no single value: a date without its
    /// day, a 12-hour hour without AM or PM, a timestamp with no offset.
    /// "input is not enough for unique date and time".
    NotEnough,
    /// The text contradicts itself: a field given twice with two values, or
    /// a weekday that is not the date's own.
    /// "no possible date and time matching input".
    Impossible,
    /// The value is well formed but outside what the type holds: a month 13,
    /// a 30 February, a year beyond 9999, an offset of a day or more, or
    /// arithmetic that left the supported range. "input is out of range".
    OutOfRange,
    /// The `strftime`-style format string uses a specifier the date types do
    /// not implement, or one the value has no component for (an hour of a
    /// [`CivilDate`](crate::dates::CivilDate)). The string is the specifier.
    /// A format string is written by the service, never by its client.
    UnsupportedFormat(String),
    /// No IANA time zone has this name in the compiled-in database. Names are
    /// case sensitive, as they were with chrono-tz. The string is the name;
    /// the message is chrono-tz's, "failed to parse timezone".
    UnknownZone(String),
    /// Not a short or long English weekday name. The message is what
    /// chrono's `Weekday: FromStr` error displayed, `ParseWeekdayError { .. }`;
    /// serde reports chrono's own serde text instead, "short or long weekday
    /// names expected".
    InvalidWeekday,
}

impl fmt::Display for DateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidText => f.write_str("input contains invalid characters"),
            Self::TooShort => f.write_str("premature end of input"),
            Self::TooLong => f.write_str("trailing input"),
            Self::NotEnough => f.write_str("input is not enough for unique date and time"),
            Self::Impossible => f.write_str("no possible date and time matching input"),
            Self::OutOfRange => f.write_str("input is out of range"),
            Self::UnsupportedFormat(spec) => {
                write!(f, "unsupported format specifier `{spec}`")
            }
            Self::UnknownZone(_) => f.write_str("failed to parse timezone"),
            Self::InvalidWeekday => f.write_str("ParseWeekdayError { .. }"),
        }
    }
}

impl Error for DateError {}
