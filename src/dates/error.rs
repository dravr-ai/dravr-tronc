// ABOUTME: The one error the date types return: text that is not a date, a value out of range, an unknown zone
// ABOUTME: Carries no engine type, so the library inside the date types never reaches a caller through an error
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::fmt;

/// Why a date operation could not produce a value.
///
/// Every fallible operation of [`dravr_tronc::dates`](crate::dates) returns
/// this; none of them panics.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DateError {
    /// The text is not in the shape the parser reads: a missing component, a
    /// wrong separator, or characters left over after the value.
    InvalidText,
    /// The value is well formed but outside what the type holds: a month 13,
    /// a 30 February, a year beyond 9999, an offset of a day or more, or
    /// arithmetic that left the supported range.
    OutOfRange,
    /// The `strftime`-style format string uses a specifier the date types do
    /// not implement, or one the value has no component for (an hour of a
    /// [`CivilDate`](crate::dates::CivilDate)). The string is the specifier.
    UnsupportedFormat(String),
    /// No IANA time zone has this name in the compiled-in database. Names are
    /// case sensitive, as they were with chrono-tz.
    UnknownZone(String),
}

impl fmt::Display for DateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidText => f.write_str("text is not a valid date or time"),
            Self::OutOfRange => f.write_str("date or time value out of range"),
            Self::UnsupportedFormat(spec) => {
                write!(f, "unsupported format specifier `{spec}`")
            }
            Self::UnknownZone(name) => write!(f, "unknown time zone `{name}`"),
        }
    }
}

impl Error for DateError {}
