// ABOUTME: Weekday, the Dravr-owned day of the week that replaces chrono's Weekday
// ABOUTME: Keeps chrono's three-letter wire form ("Mon") and its lenient reading of short and long names
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::str::FromStr;

use jiff::civil::Weekday as EngineWeekday;

use super::error::TimeError;
use super::text::SHORT_WEEKDAYS;

/// What follows the first three letters of each full weekday name, Monday
/// first, in lower case.
const LONG_SUFFIXES: [&str; 7] = ["day", "sday", "nesday", "rsday", "day", "urday", "day"];

/// A day of the week.
///
/// Replaces `chrono::Weekday`, with the same variant names. On the wire it is
/// the three-letter English name (`"Mon"`), as chrono writes it; reading
/// accepts the short or the full name in any case (`"mon"`, `"MONDAY"`), as
/// chrono does.
///
/// ```
/// use dravr_tronc::time::Weekday;
///
/// assert_eq!(Weekday::Sun.succ(), Weekday::Mon);
/// assert_eq!(Weekday::Wed.num_days_from_monday(), 2);
/// assert_eq!("thursday".parse::<Weekday>(), Ok(Weekday::Thu));
/// assert_eq!(Weekday::Sat.to_string(), "Sat");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Weekday {
    /// Monday.
    Mon,
    /// Tuesday.
    Tue,
    /// Wednesday.
    Wed,
    /// Thursday.
    Thu,
    /// Friday.
    Fri,
    /// Saturday.
    Sat,
    /// Sunday.
    Sun,
}

impl Weekday {
    /// The seven days, Monday first.
    pub const ALL: [Self; 7] = [
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];

    /// The next day; Monday follows Sunday.
    #[must_use]
    pub const fn succ(self) -> Self {
        match self {
            Self::Mon => Self::Tue,
            Self::Tue => Self::Wed,
            Self::Wed => Self::Thu,
            Self::Thu => Self::Fri,
            Self::Fri => Self::Sat,
            Self::Sat => Self::Sun,
            Self::Sun => Self::Mon,
        }
    }

    /// The previous day; Sunday precedes Monday.
    #[must_use]
    pub const fn pred(self) -> Self {
        match self {
            Self::Mon => Self::Sun,
            Self::Tue => Self::Mon,
            Self::Wed => Self::Tue,
            Self::Thu => Self::Wed,
            Self::Fri => Self::Thu,
            Self::Sat => Self::Fri,
            Self::Sun => Self::Sat,
        }
    }

    /// Days since Monday: Monday is 0, Sunday is 6.
    #[must_use]
    pub const fn num_days_from_monday(self) -> u32 {
        match self {
            Self::Mon => 0,
            Self::Tue => 1,
            Self::Wed => 2,
            Self::Thu => 3,
            Self::Fri => 4,
            Self::Sat => 5,
            Self::Sun => 6,
        }
    }

    /// Days since Sunday: Sunday is 0, Saturday is 6.
    #[must_use]
    pub const fn num_days_from_sunday(self) -> u32 {
        (self.num_days_from_monday() + 1) % 7
    }

    /// The ISO number: Monday is 1, Sunday is 7.
    #[must_use]
    pub const fn number_from_monday(self) -> u32 {
        self.num_days_from_monday() + 1
    }

    /// Sunday is 1, Saturday is 7.
    #[must_use]
    pub const fn number_from_sunday(self) -> u32 {
        self.num_days_from_sunday() + 1
    }

    /// How many days after `other` this day comes, from 0 to 6.
    #[must_use]
    pub const fn days_since(self, other: Self) -> u32 {
        (7 + self.num_days_from_monday() - other.num_days_from_monday()) % 7
    }

    /// The three-letter English name.
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        SHORT_WEEKDAYS[self.num_days_from_monday() as usize]
    }

    pub(super) const fn from_engine(weekday: EngineWeekday) -> Self {
        match weekday {
            EngineWeekday::Monday => Self::Mon,
            EngineWeekday::Tuesday => Self::Tue,
            EngineWeekday::Wednesday => Self::Wed,
            EngineWeekday::Thursday => Self::Thu,
            EngineWeekday::Friday => Self::Fri,
            EngineWeekday::Saturday => Self::Sat,
            EngineWeekday::Sunday => Self::Sun,
        }
    }

    pub(super) const fn to_engine(self) -> EngineWeekday {
        match self {
            Self::Mon => EngineWeekday::Monday,
            Self::Tue => EngineWeekday::Tuesday,
            Self::Wed => EngineWeekday::Wednesday,
            Self::Thu => EngineWeekday::Thursday,
            Self::Fri => EngineWeekday::Friday,
            Self::Sat => EngineWeekday::Saturday,
            Self::Sun => EngineWeekday::Sunday,
        }
    }
}

impl fmt::Display for Weekday {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.short_name())
    }
}

impl FromStr for Weekday {
    type Err = TimeError;

    /// Reads the short or the full English name, in any case.
    fn from_str(text: &str) -> Result<Self, TimeError> {
        let (head, tail) = text.split_at_checked(3).ok_or(TimeError::InvalidText)?;
        Self::ALL
            .into_iter()
            .zip(LONG_SUFFIXES)
            .find(|(day, suffix)| {
                head.eq_ignore_ascii_case(day.short_name())
                    && (tail.is_empty() || tail.eq_ignore_ascii_case(suffix))
            })
            .map(|(day, _)| day)
            .ok_or(TimeError::InvalidText)
    }
}
