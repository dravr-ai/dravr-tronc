// ABOUTME: The text layer of the date types: the scanners and writers that reproduce chrono's formats
// ABOUTME: One item engine serves FromStr, the RFC 3339 forms, the strftime subset and the database text shapes
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Formatting and parsing, written here and not delegated to the engine.
//!
//! The date types promise the bytes chrono wrote and the inputs chrono
//! accepted. Neither is the engine's default, so both are owned by this
//! module: a small item list (`Item`) that a format string compiles to, a
//! scanner that reads text against it with chrono's leniency (numbers need no
//! padding, a space in the format matches any run of whitespace, a field set
//! twice must agree), and writers with chrono's padding rules.

use std::fmt::{self, Write};
use std::ops::RangeInclusive;

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::Offset;
use jiff::Timestamp as EngineTimestamp;

use super::error::DateError;

/// Seconds in a day: an offset must be strictly inside it, as chrono's
/// `FixedOffset` requires.
const SECONDS_PER_DAY: i64 = 86_400;

/// Three-letter month names, January first.
const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Full month names, January first.
const LONG_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Three-letter weekday names, Monday first.
pub(super) const SHORT_WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// Full weekday names, Monday first.
const LONG_WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// How a numeric item is padded to its width when written. Ignored when read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Pad {
    /// No padding (`%-d`).
    None,
    /// Zeros (`%d`).
    Zero,
    /// Spaces (`%e`).
    Space,
}

/// A numeric component.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Num {
    /// `%Y`
    Year,
    /// `%C`
    YearDiv100,
    /// `%y`
    YearMod100,
    /// `%m`
    Month,
    /// `%d`, `%e`
    Day,
    /// `%j`
    Ordinal,
    /// `%H`, `%k`
    Hour,
    /// `%I`, `%l`
    Hour12,
    /// `%M`
    Minute,
    /// `%S`
    Second,
    /// `%f`
    Nanosecond,
    /// `%s`
    UnixSeconds,
    /// `%u`
    WeekdayFromMonday,
    /// `%w`
    DaysFromSunday,
}

/// One element of a format: what a `strftime` string compiles to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Item<'a> {
    /// Text that must appear as is.
    Literal(&'a str),
    /// Any run of whitespace, including none, when read; itself when written.
    Space(&'a str),
    /// A number.
    Numeric(Num, Pad),
    /// `%.f`: a dot and 3, 6 or 9 digits, or nothing for a whole second.
    Fraction,
    /// `%.3f`, `%.6f`, `%.9f`: a dot and exactly that many digits.
    FractionFixed(usize),
    /// `%b`
    ShortMonth,
    /// `%B`
    LongMonth,
    /// `%a`
    ShortWeekday,
    /// `%A`
    LongWeekday,
    /// `%P`
    LowerAmPm,
    /// `%p`
    UpperAmPm,
    /// `%z`: `+0000`.
    Offset,
    /// `%:z`: `+00:00`.
    OffsetColon,
    /// `%#z`: read only; accepts `Z` and an offset without minutes.
    OffsetPermissive,
    /// `%Z`
    ZoneName,
}

/// `%Y-%m-%d`
const DATE_ITEMS: [Item<'static>; 5] = [
    Item::Numeric(Num::Year, Pad::Zero),
    Item::Literal("-"),
    Item::Numeric(Num::Month, Pad::Zero),
    Item::Literal("-"),
    Item::Numeric(Num::Day, Pad::Zero),
];

/// `%H:%M`
const HOUR_MINUTE_ITEMS: [Item<'static>; 3] = [
    Item::Numeric(Num::Hour, Pad::Zero),
    Item::Literal(":"),
    Item::Numeric(Num::Minute, Pad::Zero),
];

/// `:%S`
const SECOND_ITEMS: [Item<'static>; 2] =
    [Item::Literal(":"), Item::Numeric(Num::Second, Pad::Zero)];

/// `%m/%d/%y`
const US_DATE_ITEMS: [Item<'static>; 5] = [
    Item::Numeric(Num::Month, Pad::Zero),
    Item::Literal("/"),
    Item::Numeric(Num::Day, Pad::Zero),
    Item::Literal("/"),
    Item::Numeric(Num::YearMod100, Pad::Zero),
];

fn unsupported(spec: &str) -> DateError {
    DateError::UnsupportedFormat(spec.to_owned())
}

/// Compiles a `strftime` string to items, with chrono's tokenization: a run
/// of whitespace is one [`Item::Space`], any other run up to the next `%` or
/// whitespace is one literal.
pub(super) fn compile(format: &str) -> Result<Vec<Item<'_>>, DateError> {
    let mut items = Vec::new();
    let mut rest = format;
    while let Some(first) = rest.chars().next() {
        if first == '%' {
            rest = specifier(rest, &mut items)?;
        } else if first.is_whitespace() {
            let end = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            let (run, tail) = rest.split_at(end);
            items.push(Item::Space(run));
            rest = tail;
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '%')
                .unwrap_or(rest.len());
            let (run, tail) = rest.split_at(end);
            items.push(Item::Literal(run));
            rest = tail;
        }
    }
    Ok(items)
}

/// Reads one `%…` specifier at the start of `text`, pushes its items and
/// returns what follows it.
fn specifier<'a>(text: &'a str, items: &mut Vec<Item<'a>>) -> Result<&'a str, DateError> {
    let mut chars = text.chars();
    chars.next();
    let whole = |rest: &str| {
        let len = text.len() - rest.len();
        unsupported(text.get(..len).unwrap_or(text))
    };
    let mut letter = chars.next().ok_or_else(|| unsupported("%"))?;
    let pad = match letter {
        '-' => Some(Pad::None),
        '0' => Some(Pad::Zero),
        '_' => Some(Pad::Space),
        _ => None,
    };
    if pad.is_some() {
        letter = chars.next().ok_or_else(|| whole(chars.as_str()))?;
    }
    let numeric = |num, default| Item::Numeric(num, pad.unwrap_or(default));
    let before = items.len();
    match letter {
        'Y' => items.push(numeric(Num::Year, Pad::Zero)),
        'C' => items.push(numeric(Num::YearDiv100, Pad::Zero)),
        'y' => items.push(numeric(Num::YearMod100, Pad::Zero)),
        'm' => items.push(numeric(Num::Month, Pad::Zero)),
        'd' => items.push(numeric(Num::Day, Pad::Zero)),
        'e' => items.push(numeric(Num::Day, Pad::Space)),
        'j' => items.push(numeric(Num::Ordinal, Pad::Zero)),
        'H' => items.push(numeric(Num::Hour, Pad::Zero)),
        'k' => items.push(numeric(Num::Hour, Pad::Space)),
        'I' => items.push(numeric(Num::Hour12, Pad::Zero)),
        'l' => items.push(numeric(Num::Hour12, Pad::Space)),
        'M' => items.push(numeric(Num::Minute, Pad::Zero)),
        'S' => items.push(numeric(Num::Second, Pad::Zero)),
        'f' => items.push(numeric(Num::Nanosecond, Pad::Zero)),
        // Unpadded unless the format asks: `%0s` and `%_s` fill nine places.
        's' => items.push(numeric(Num::UnixSeconds, Pad::None)),
        'u' => items.push(numeric(Num::WeekdayFromMonday, Pad::Zero)),
        'w' => items.push(numeric(Num::DaysFromSunday, Pad::Zero)),
        _ if pad.is_some() => return Err(whole(chars.as_str())),
        'a' => items.push(Item::ShortWeekday),
        'A' => items.push(Item::LongWeekday),
        'b' | 'h' => items.push(Item::ShortMonth),
        'B' => items.push(Item::LongMonth),
        'p' => items.push(Item::UpperAmPm),
        'P' => items.push(Item::LowerAmPm),
        'z' => items.push(Item::Offset),
        'Z' => items.push(Item::ZoneName),
        'F' => items.extend(DATE_ITEMS),
        'R' => items.extend(HOUR_MINUTE_ITEMS),
        'T' => {
            items.extend(HOUR_MINUTE_ITEMS);
            items.extend(SECOND_ITEMS);
        }
        'D' => items.extend(US_DATE_ITEMS),
        'n' => items.push(Item::Space("\n")),
        't' => items.push(Item::Space("\t")),
        '%' => items.push(Item::Literal("%")),
        '.' => match (chars.next(), chars.clone().next()) {
            (Some('f'), _) => items.push(Item::Fraction),
            (Some(digits @ ('3' | '6' | '9')), Some('f')) => {
                chars.next();
                let width = digits.to_digit(10).map_or(3, |d| d as usize);
                items.push(Item::FractionFixed(width));
            }
            _ => {}
        },
        ':' if chars.clone().next() == Some('z') => {
            chars.next();
            items.push(Item::OffsetColon);
        }
        '#' if chars.clone().next() == Some('z') => {
            chars.next();
            items.push(Item::OffsetPermissive);
        }
        _ => {}
    }
    if items.len() == before {
        return Err(whole(chars.as_str()));
    }
    Ok(chars.as_str())
}

/// What a format string is applied to: the components the value has.
pub(super) struct Moment<'a> {
    /// The calendar date, when the value has one.
    pub(super) date: Option<Date>,
    /// The time of day, when the value has one.
    pub(super) time: Option<Time>,
    /// Seconds east of UTC and the name `%Z` prints, when the value has a
    /// place on the time line.
    pub(super) offset: Option<(i32, &'a str)>,
}

/// Formats `moment` with a `strftime` string, as chrono's `format` does for
/// the specifiers this module implements.
pub(super) fn format(moment: &Moment<'_>, format: &str) -> Result<String, DateError> {
    let items = compile(format)?;
    let mut out = String::with_capacity(format.len() + 16);
    for item in &items {
        write_item(&mut out, moment, *item)?;
    }
    Ok(out)
}

fn write_item(out: &mut String, moment: &Moment<'_>, item: Item<'_>) -> Result<(), DateError> {
    let need_date = |spec: &str| moment.date.ok_or_else(|| unsupported(spec));
    let need_time = |spec: &str| moment.time.ok_or_else(|| unsupported(spec));
    let need_offset = |spec: &str| moment.offset.ok_or_else(|| unsupported(spec));
    let written = match item {
        Item::Literal(text) | Item::Space(text) => {
            out.push_str(text);
            Ok(())
        }
        Item::Numeric(num, pad) => return write_numeric(out, moment, num, pad),
        Item::Fraction => write_fraction(
            out,
            need_time("%.f")?.subsec_nanosecond(),
            FractionDigits::Auto,
        ),
        Item::FractionFixed(width) => {
            let digits = match width {
                3 => FractionDigits::Millis,
                6 => FractionDigits::Micros,
                _ => FractionDigits::Nanos,
            };
            write_fraction(out, need_time("%.3f")?.subsec_nanosecond(), digits)
        }
        Item::ShortMonth => {
            out.push_str(month_name(&SHORT_MONTHS, need_date("%b")?));
            Ok(())
        }
        Item::LongMonth => {
            out.push_str(month_name(&LONG_MONTHS, need_date("%B")?));
            Ok(())
        }
        Item::ShortWeekday => {
            out.push_str(weekday_name(&SHORT_WEEKDAYS, need_date("%a")?));
            Ok(())
        }
        Item::LongWeekday => {
            out.push_str(weekday_name(&LONG_WEEKDAYS, need_date("%A")?));
            Ok(())
        }
        Item::LowerAmPm => {
            out.push_str(if need_time("%P")?.hour() >= 12 {
                "pm"
            } else {
                "am"
            });
            Ok(())
        }
        Item::UpperAmPm => {
            out.push_str(if need_time("%p")?.hour() >= 12 {
                "PM"
            } else {
                "AM"
            });
            Ok(())
        }
        Item::Offset => write_offset(out, need_offset("%z")?.0, false),
        Item::OffsetColon => write_offset(out, need_offset("%:z")?.0, true),
        Item::OffsetPermissive => return Err(unsupported("%#z")),
        Item::ZoneName => {
            out.push_str(need_offset("%Z")?.1);
            Ok(())
        }
    };
    written.map_err(|_| DateError::OutOfRange)
}

fn month_name(names: &'static [&'static str; 12], date: Date) -> &'static str {
    let index = usize::try_from(date.month() - 1).unwrap_or(0);
    names.get(index).copied().unwrap_or("")
}

fn weekday_name(names: &'static [&'static str; 7], date: Date) -> &'static str {
    let index = usize::try_from(date.weekday().to_monday_zero_offset()).unwrap_or(0);
    names.get(index).copied().unwrap_or("")
}

fn write_numeric(
    out: &mut String,
    moment: &Moment<'_>,
    num: Num,
    pad: Pad,
) -> Result<(), DateError> {
    let need_date = |spec: &str| moment.date.ok_or_else(|| unsupported(spec));
    let need_time = |spec: &str| moment.time.ok_or_else(|| unsupported(spec));
    let written = match num {
        Num::Year => {
            let year = i64::from(need_date("%Y")?.year());
            write_padded(out, 4, year, pad, !(0..10_000).contains(&year))
        }
        Num::YearDiv100 => {
            // chrono writes two meaningless characters for a century before
            // year 0; that is not a format to reproduce.
            let year = i64::from(need_date("%C")?.year());
            if year < 0 {
                return Err(DateError::OutOfRange);
            }
            write_two(out, year / 100, pad)
        }
        Num::YearMod100 => write_two(out, i64::from(need_date("%y")?.year()).rem_euclid(100), pad),
        Num::Month => write_two(out, i64::from(need_date("%m")?.month()), pad),
        Num::Day => write_two(out, i64::from(need_date("%d")?.day()), pad),
        Num::Ordinal => write_padded(
            out,
            3,
            i64::from(need_date("%j")?.day_of_year()),
            pad,
            false,
        ),
        Num::Hour => write_two(out, i64::from(need_time("%H")?.hour()), pad),
        Num::Hour12 => {
            let hour = i64::from(need_time("%I")?.hour()) % 12;
            write_two(out, if hour == 0 { 12 } else { hour }, pad)
        }
        Num::Minute => write_two(out, i64::from(need_time("%M")?.minute()), pad),
        Num::Second => write_two(out, i64::from(need_time("%S")?.second()), pad),
        Num::Nanosecond => write_padded(
            out,
            9,
            i64::from(need_time("%f")?.subsec_nanosecond()),
            pad,
            false,
        ),
        Num::UnixSeconds => {
            let civil = DateTime::from_parts(need_date("%s")?, need_time("%s")?);
            let offset = moment.offset.map_or(0, |(seconds, _)| i64::from(seconds));
            let local = civil.duration_since(DateTime::constant(1970, 1, 1, 0, 0, 0, 0));
            // Whole seconds rounded down, as `timestamp()` gives before 1970.
            let seconds = i64::try_from(local.as_nanos().div_euclid(1_000_000_000))
                .map_err(|_| DateError::OutOfRange)?;
            write_padded(out, 9, seconds - offset, pad, false)
        }
        Num::WeekdayFromMonday => {
            write!(out, "{}", need_date("%u")?.weekday().to_monday_one_offset())
        }
        Num::DaysFromSunday => write!(
            out,
            "{}",
            need_date("%w")?.weekday().to_sunday_zero_offset()
        ),
    };
    written.map_err(|_| DateError::OutOfRange)
}

/// A two-digit field: chrono writes the low two digits whatever the value.
fn write_two(out: &mut String, value: i64, pad: Pad) -> fmt::Result {
    let value = value.rem_euclid(100);
    match (value / 10, pad) {
        (0, Pad::None) => write!(out, "{value}"),
        (0, Pad::Space) => write!(out, " {value}"),
        _ => write!(out, "{value:02}"),
    }
}

fn write_padded(out: &mut String, width: usize, value: i64, pad: Pad, sign: bool) -> fmt::Result {
    match (sign, pad) {
        (true, Pad::None) => write!(out, "{value:+}"),
        (true, Pad::Zero) => write!(out, "{value:+0w$}", w = width + 1),
        (true, Pad::Space) => write!(out, "{value:+w$}", w = width + 1),
        (false, Pad::None) => write!(out, "{value}"),
        (false, Pad::Zero) => write!(out, "{value:0width$}"),
        (false, Pad::Space) => write!(out, "{value:width$}"),
    }
}

/// `+hhmm` or `+hh:mm`, rounded to the minute as chrono's `%z` does.
fn write_offset(out: &mut String, seconds: i32, colon: bool) -> fmt::Result {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = (seconds.unsigned_abs() + 30) / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    if colon {
        write!(out, "{sign}{hours:02}:{minutes:02}")
    } else {
        write!(out, "{sign}{hours:02}{minutes:02}")
    }
}

/// How many fraction digits a writer emits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FractionDigits {
    /// None: whole seconds, the fraction dropped.
    Secs,
    /// Exactly three.
    Millis,
    /// Exactly six.
    Micros,
    /// Exactly nine.
    Nanos,
    /// None for a whole second, else three, six or nine: the shortest of
    /// those that loses nothing. chrono's `AutoSi`, and its `Debug`.
    Auto,
}

/// Writes the fraction of a second with its leading dot.
pub(super) fn write_fraction(
    out: &mut impl Write,
    nanos: i32,
    digits: FractionDigits,
) -> fmt::Result {
    match digits {
        FractionDigits::Secs => Ok(()),
        FractionDigits::Millis => write!(out, ".{:03}", nanos / 1_000_000),
        FractionDigits::Micros => write!(out, ".{:06}", nanos / 1_000),
        FractionDigits::Nanos => write!(out, ".{nanos:09}"),
        FractionDigits::Auto => {
            if nanos == 0 {
                Ok(())
            } else if nanos % 1_000_000 == 0 {
                write!(out, ".{:03}", nanos / 1_000_000)
            } else if nanos % 1_000 == 0 {
                write!(out, ".{:06}", nanos / 1_000)
            } else {
                write!(out, ".{nanos:09}")
            }
        }
    }
}

/// `YYYY-MM-DD`; a year outside 0..=9999 carries its sign, as ISO 8601 asks
/// and chrono writes.
pub(super) fn write_date(out: &mut impl Write, date: Date) -> fmt::Result {
    let year = date.year();
    if (0..=9999).contains(&year) {
        write!(out, "{year:04}")?;
    } else {
        write!(out, "{year:+05}")?;
    }
    write!(out, "-{:02}-{:02}", date.month(), date.day())
}

/// `HH:MM:SS` and the fraction.
pub(super) fn write_time(out: &mut impl Write, time: Time, digits: FractionDigits) -> fmt::Result {
    write!(
        out,
        "{:02}:{:02}:{:02}",
        time.hour(),
        time.minute(),
        time.second()
    )?;
    write_fraction(out, time.subsec_nanosecond(), digits)
}

/// What a scan has read so far. A field read twice must hold the same value
/// both times, as in chrono's `Parsed`.
#[derive(Default)]
pub(super) struct Parsed {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    /// Days since Monday, when the text named a weekday.
    weekday: Option<i64>,
    /// 0 for the morning, 1 for the afternoon: set by a 24-hour hour or by
    /// AM/PM.
    hour_div_12: Option<i64>,
    /// The hour within its half of the day, 0 to 11: set by a 24-hour hour
    /// or by a 12-hour one.
    hour_mod_12: Option<i64>,
    minute: Option<i64>,
    second: Option<i64>,
    nanosecond: Option<i64>,
    offset: Option<i64>,
}

fn set(slot: &mut Option<i64>, value: i64, range: RangeInclusive<i64>) -> Result<(), DateError> {
    if !range.contains(&value) {
        return Err(DateError::OutOfRange);
    }
    match *slot {
        Some(old) if old != value => Err(DateError::Impossible),
        _ => {
            *slot = Some(value);
            Ok(())
        }
    }
}

/// The years chrono's `NaiveDate` held. A date beyond them was out of range
/// before chrono looked at its weekday, so it is here too.
const CHRONO_YEARS: RangeInclusive<i64> = -262_143..=262_142;

/// The days in a month of the proleptic Gregorian calendar, for any year;
/// zero for a month that is not one.
const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since Monday of a valid proleptic Gregorian date, for any year:
/// the days-from-civil count of the date, from a Thursday 1970-01-01.
const fn weekday_from_monday(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    (days + 3).rem_euclid(7)
}

/// A checked year, month and day as the engine's date: out of range when
/// the year is beyond what the date types hold.
fn engine_date((year, month, day): (i64, i64, i64)) -> Result<Date, DateError> {
    let year = i16::try_from(year).map_err(|_| DateError::OutOfRange)?;
    let month = i8::try_from(month).map_err(|_| DateError::OutOfRange)?;
    let day = i8::try_from(day).map_err(|_| DateError::OutOfRange)?;
    Date::new(year, month, day).map_err(|_| DateError::OutOfRange)
}

impl Parsed {
    /// The year, month and day read, checked in chrono's order: all three
    /// are required, then the date must exist, then a weekday in the text
    /// must be the date's own. The range of the date types is checked after
    /// this, by [`engine_date`], so a rejection chrono made is reported as
    /// chrono reported it.
    fn checked_ymd(&self) -> Result<(i64, i64, i64), DateError> {
        let (Some(year), Some(month), Some(day)) = (self.year, self.month, self.day) else {
            return Err(DateError::NotEnough);
        };
        if !CHRONO_YEARS.contains(&year) || !(1..=days_in_month(year, month)).contains(&day) {
            return Err(DateError::OutOfRange);
        }
        match self.weekday {
            Some(named) if named != weekday_from_monday(year, month, day) => {
                Err(DateError::Impossible)
            }
            _ => Ok((year, month, day)),
        }
    }

    /// The calendar date read: year, month and day are all required.
    pub(super) fn date(&self) -> Result<Date, DateError> {
        engine_date(self.checked_ymd()?)
    }

    /// The time of day read: hour and minute are required, the second
    /// defaults to zero, and a fraction needs a second. A 12-hour hour needs
    /// its AM or PM. A leap second `:60` reads as `:59`: the types hold no
    /// leap seconds.
    pub(super) fn time(&self) -> Result<Time, DateError> {
        let (Some(half), Some(hour), Some(minute)) =
            (self.hour_div_12, self.hour_mod_12, self.minute)
        else {
            return Err(DateError::NotEnough);
        };
        let hour = half * 12 + hour;
        let second = self.second.unwrap_or(0).min(59);
        let nanosecond = match self.nanosecond {
            Some(nanos) if self.second.is_some() => nanos,
            Some(_) => return Err(DateError::NotEnough),
            None => 0,
        };
        let hour = i8::try_from(hour).map_err(|_| DateError::OutOfRange)?;
        let minute = i8::try_from(minute).map_err(|_| DateError::OutOfRange)?;
        let second = i8::try_from(second).map_err(|_| DateError::OutOfRange)?;
        let nanosecond = i32::try_from(nanosecond).map_err(|_| DateError::OutOfRange)?;
        Time::new(hour, minute, second, nanosecond).map_err(|_| DateError::OutOfRange)
    }

    /// The date and time read, with no place on the time line. As in
    /// chrono, the date's rejection comes before the time's, and both come
    /// before the range of the date types.
    pub(super) fn datetime(&self) -> Result<DateTime, DateError> {
        let ymd = self.checked_ymd()?;
        let time = self.time()?;
        Ok(DateTime::from_parts(engine_date(ymd)?, time))
    }

    /// The date and time read, as the text showed them, and the offset read
    /// with them in seconds east of UTC, when the text had one. The offset
    /// must be less than a day, which chrono checked after the date and time.
    pub(super) fn reading(&self) -> Result<(DateTime, Option<i32>), DateError> {
        let civil = self.datetime()?;
        let offset = match self.offset {
            Some(offset) if offset.abs() >= SECONDS_PER_DAY => return Err(DateError::OutOfRange),
            Some(offset) => Some(i32::try_from(offset).map_err(|_| DateError::OutOfRange)?),
            None => None,
        };
        Ok((civil, offset))
    }

    /// The timestamp read and its offset in seconds east of UTC. The offset is
    /// required, before anything else is looked at, and must be less than a
    /// day.
    pub(super) fn timestamp(&self) -> Result<(EngineTimestamp, i32), DateError> {
        if self.offset.is_none() {
            return Err(DateError::NotEnough);
        }
        let (civil, offset) = self.reading()?;
        let seconds = offset.unwrap_or(0);
        Ok((timestamp_at(civil, seconds)?, seconds))
    }
}

/// The timestamp at which a clock `offset_seconds` east of UTC shows `civil`.
pub(super) fn timestamp_at(
    civil: DateTime,
    offset_seconds: i32,
) -> Result<EngineTimestamp, DateError> {
    Offset::from_seconds(offset_seconds)
        .and_then(|offset| offset.to_timestamp(civil))
        .map_err(|_| DateError::OutOfRange)
}

/// How the colon between an offset's hours and minutes is read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Colon {
    /// Exactly one colon: RFC 3339.
    Required,
    /// Any run of colons and whitespace, including none.
    Loose,
}

/// Reads an unsigned number of `min` to `max` digits. Text shorter than
/// `min` bytes ended too soon; fewer digits than that is a wrong character.
fn number(text: &str, min: usize, max: usize) -> Result<(&str, i64), DateError> {
    if text.len() < min {
        return Err(DateError::TooShort);
    }
    let mut value: i64 = 0;
    let mut taken = 0;
    for &byte in text.as_bytes().iter().take(max) {
        if !byte.is_ascii_digit() {
            break;
        }
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(i64::from(byte - b'0')))
            .ok_or(DateError::OutOfRange)?;
        taken += 1;
    }
    if taken < min {
        return Err(DateError::InvalidText);
    }
    let rest = text.get(taken..).ok_or(DateError::InvalidText)?;
    Ok((rest, value))
}

/// Reads the digits after a fraction's dot, scaled to nanoseconds. Digits
/// past the ninth are read and dropped.
fn fraction(text: &str) -> Result<(&str, i64), DateError> {
    let (rest, value) = number(text, 1, 9)?;
    let consumed = text.len() - rest.len();
    let scale = 10_i64.pow(u32::try_from(9 - consumed).map_err(|_| DateError::OutOfRange)?);
    Ok((
        rest.trim_start_matches(|c: char| c.is_ascii_digit()),
        value * scale,
    ))
}

/// Reads a UTC offset: `+hh:mm`, `+hhmm`, and per the flags `Z` and `+hh`.
/// A `U+2212` minus sign is accepted, as chrono accepts it.
fn offset(
    text: &str,
    colon: Colon,
    zulu: bool,
    missing_minutes: bool,
) -> Result<(&str, i64), DateError> {
    if zulu {
        if let Some(rest) = text.strip_prefix(['Z', 'z']) {
            return Ok((rest, 0));
        }
    }
    let mut chars = text.chars();
    let negative = match chars.next() {
        Some('+') => false,
        Some('-' | '\u{2212}') => true,
        Some(_) => return Err(DateError::InvalidText),
        None => return Err(DateError::TooShort),
    };
    let rest = chars.as_str();
    if rest.len() < 2 {
        return Err(DateError::TooShort);
    }
    let (rest, hours) = two_digits(rest).ok_or(DateError::InvalidText)?;
    let rest = match colon {
        Colon::Required => match rest.strip_prefix(':') {
            Some(tail) => tail,
            None if rest.is_empty() => return Err(DateError::TooShort),
            None => return Err(DateError::InvalidText),
        },
        Colon::Loose => rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace()),
    };
    // chrono's order: two characters are looked at when there are two, and
    // only a missing pair falls back to the flag.
    let (rest, minutes) = match rest.as_bytes() {
        [b'0'..=b'5', b'0'..=b'9', ..] => two_digits(rest).ok_or(DateError::InvalidText)?,
        [b'6'..=b'9', b'0'..=b'9', ..] => return Err(DateError::OutOfRange),
        [_, _, ..] => return Err(DateError::InvalidText),
        [] if missing_minutes => (rest, 0),
        _ => return Err(DateError::TooShort),
    };
    let seconds = hours * 3600 + minutes * 60;
    Ok((rest, if negative { -seconds } else { seconds }))
}

/// Reads a month or weekday name: its first three letters in any case, and
/// then, when `long` is given, the rest of the full name if it is there.
/// Returns what follows and the name's index.
fn name<'t, const N: usize>(
    text: &'t str,
    short: &[&str; N],
    long: Option<&[&str; N]>,
) -> Result<(&'t str, i64), DateError> {
    if text.len() < 3 {
        return Err(DateError::TooShort);
    }
    let (head, tail) = text.split_at_checked(3).ok_or(DateError::InvalidText)?;
    let index = short
        .iter()
        .position(|candidate| head.eq_ignore_ascii_case(candidate))
        .ok_or(DateError::InvalidText)?;
    let suffix = long
        .and_then(|names| names.get(index))
        .and_then(|full| full.get(3..))
        .unwrap_or("");
    let rest = match tail.get(..suffix.len()) {
        Some(found) if found.eq_ignore_ascii_case(suffix) => tail.get(suffix.len()..).unwrap_or(""),
        _ => tail,
    };
    let index = i64::try_from(index).map_err(|_| DateError::OutOfRange)?;
    Ok((rest, index))
}

/// Reads exactly two ASCII digits.
fn two_digits(text: &str) -> Option<(&str, i64)> {
    let bytes = text.as_bytes();
    let (tens, ones) = (*bytes.first()?, *bytes.get(1)?);
    if !tens.is_ascii_digit() || !ones.is_ascii_digit() {
        return None;
    }
    let value = i64::from(tens - b'0') * 10 + i64::from(ones - b'0');
    Some((text.get(2..)?, value))
}

/// Reads `text` against `items` and returns what is left of it.
pub(super) fn scan<'t>(
    parsed: &mut Parsed,
    mut text: &'t str,
    items: &[Item<'_>],
) -> Result<&'t str, DateError> {
    for item in items {
        match *item {
            Item::Literal(literal) => {
                if text.len() < literal.len() {
                    return Err(DateError::TooShort);
                }
                text = text.strip_prefix(literal).ok_or(DateError::InvalidText)?;
            }
            Item::Space(_) => text = text.trim_start(),
            Item::Numeric(num, _) => text = scan_numeric(parsed, text.trim_start(), num)?,
            Item::Fraction => {
                if let Some(digits) = text.strip_prefix('.') {
                    let (rest, nanos) = fraction(digits)?;
                    set(&mut parsed.nanosecond, nanos, 0..=999_999_999)?;
                    text = rest;
                }
            }
            Item::FractionFixed(width) => {
                if let Some(digits) = text.strip_prefix('.') {
                    let (rest, value) = number(digits, width, width)?;
                    let scale = 10_i64.pow(
                        u32::try_from(9_usize.saturating_sub(width))
                            .map_err(|_| DateError::OutOfRange)?,
                    );
                    set(&mut parsed.nanosecond, value * scale, 0..=999_999_999)?;
                    text = rest;
                }
            }
            Item::Offset | Item::OffsetColon => {
                let (rest, seconds) = offset(text.trim_start(), Colon::Loose, false, false)?;
                set(&mut parsed.offset, seconds, i64::MIN..=i64::MAX)?;
                text = rest;
            }
            Item::OffsetPermissive => {
                let (rest, seconds) = offset(text.trim_start(), Colon::Loose, true, true)?;
                set(&mut parsed.offset, seconds, i64::MIN..=i64::MAX)?;
                text = rest;
            }
            Item::ShortMonth | Item::LongMonth => {
                let long = if matches!(*item, Item::LongMonth) {
                    Some(&LONG_MONTHS)
                } else {
                    None
                };
                let (rest, index) = name(text, &SHORT_MONTHS, long)?;
                set(&mut parsed.month, index + 1, 1..=12)?;
                text = rest;
            }
            Item::ShortWeekday | Item::LongWeekday => {
                let long = if matches!(*item, Item::LongWeekday) {
                    Some(&LONG_WEEKDAYS)
                } else {
                    None
                };
                let (rest, index) = name(text, &SHORT_WEEKDAYS, long)?;
                set(&mut parsed.weekday, index, 0..=6)?;
                text = rest;
            }
            Item::LowerAmPm | Item::UpperAmPm => {
                if text.len() < 2 {
                    return Err(DateError::TooShort);
                }
                let half = match text.get(..2) {
                    Some(word) if word.eq_ignore_ascii_case("am") => 0,
                    Some(word) if word.eq_ignore_ascii_case("pm") => 1,
                    _ => return Err(DateError::InvalidText),
                };
                set(&mut parsed.hour_div_12, half, 0..=1)?;
                text = text.get(2..).unwrap_or("");
            }
            Item::ZoneName => return Err(unsupported("%Z")),
        }
    }
    Ok(text)
}

fn scan_numeric<'t>(parsed: &mut Parsed, text: &'t str, num: Num) -> Result<&'t str, DateError> {
    let (slot, width, range) = match num {
        Num::Year => {
            // A signed year may have any number of digits; an unsigned one
            // at most four, so `20261117` is not read as a year.
            let (rest, year) = if let Some(digits) = text.strip_prefix('-') {
                let (rest, value) = number(digits, 1, usize::MAX)?;
                (rest, -value)
            } else if let Some(digits) = text.strip_prefix('+') {
                number(digits, 1, usize::MAX)?
            } else {
                number(text, 1, 4)?
            };
            set(
                &mut parsed.year,
                year,
                i64::from(i32::MIN)..=i64::from(i32::MAX),
            )?;
            return Ok(rest);
        }
        Num::Month => (&mut parsed.month, 2, 1..=12),
        Num::Day => (&mut parsed.day, 2, 1..=31),
        Num::Hour => {
            let (rest, hour) = number(text, 1, 2)?;
            if !(0..=23).contains(&hour) {
                return Err(DateError::OutOfRange);
            }
            set(&mut parsed.hour_div_12, hour / 12, 0..=1)?;
            set(&mut parsed.hour_mod_12, hour % 12, 0..=11)?;
            return Ok(rest);
        }
        Num::Hour12 => {
            let (rest, hour) = number(text, 1, 2)?;
            if !(1..=12).contains(&hour) {
                return Err(DateError::OutOfRange);
            }
            set(&mut parsed.hour_mod_12, hour % 12, 0..=11)?;
            return Ok(rest);
        }
        Num::Minute => (&mut parsed.minute, 2, 0..=59),
        Num::Second => (&mut parsed.second, 2, 0..=60),
        Num::Nanosecond => (&mut parsed.nanosecond, 9, 0..=999_999_999),
        Num::YearDiv100 => return Err(unsupported("%C")),
        Num::YearMod100 => return Err(unsupported("%y")),
        Num::Ordinal => return Err(unsupported("%j")),
        Num::UnixSeconds => return Err(unsupported("%s")),
        Num::WeekdayFromMonday => return Err(unsupported("%u")),
        Num::DaysFromSunday => return Err(unsupported("%w")),
    };
    let (rest, value) = number(text, 1, width)?;
    set(slot, value, range)?;
    Ok(rest)
}

/// Reads `text` against a `strftime` string; nothing may be left over.
pub(super) fn parse(text: &str, format: &str) -> Result<Parsed, DateError> {
    let items = compile(format)?;
    let mut parsed = Parsed::default();
    let rest = scan(&mut parsed, text, &items)?;
    finished(parsed, rest)
}

/// `Y - m - d` with optional whitespace around the dashes: the date part of
/// chrono's `FromStr` impls.
const RELAXED_DATE: [Item<'static>; 7] = [
    Item::Numeric(Num::Year, Pad::Zero),
    Item::Space(""),
    Item::Literal("-"),
    Item::Numeric(Num::Month, Pad::Zero),
    Item::Space(""),
    Item::Literal("-"),
    Item::Numeric(Num::Day, Pad::Zero),
];

/// `H : M`
const RELAXED_HOUR_MINUTE: [Item<'static>; 4] = [
    Item::Numeric(Num::Hour, Pad::Zero),
    Item::Space(""),
    Item::Literal(":"),
    Item::Numeric(Num::Minute, Pad::Zero),
];

/// ` : S[.f] `
const RELAXED_SECOND: [Item<'static>; 5] = [
    Item::Space(""),
    Item::Literal(":"),
    Item::Numeric(Num::Second, Pad::Zero),
    Item::Fraction,
    Item::Space(""),
];

/// The fields read, when nothing is left of the text.
fn finished(parsed: Parsed, rest: &str) -> Result<Parsed, DateError> {
    if rest.is_empty() {
        Ok(parsed)
    } else {
        Err(DateError::TooLong)
    }
}

/// What chrono's `NaiveDate: FromStr` reads.
pub(super) fn relaxed_date(text: &str) -> Result<Date, DateError> {
    let mut parsed = Parsed::default();
    let rest = scan(&mut parsed, text, &RELAXED_DATE)?;
    finished(parsed, rest.trim_start())?.date()
}

/// What chrono's `NaiveTime: FromStr` reads: the seconds are optional.
pub(super) fn relaxed_time(text: &str) -> Result<Time, DateError> {
    let mut parsed = Parsed::default();
    let after_minute = scan(&mut parsed, text, &RELAXED_HOUR_MINUTE)?;
    let mut with_seconds = Parsed {
        hour_div_12: parsed.hour_div_12,
        hour_mod_12: parsed.hour_mod_12,
        minute: parsed.minute,
        ..Parsed::default()
    };
    match scan(&mut with_seconds, after_minute, &RELAXED_SECOND) {
        Ok(rest) => finished(with_seconds, rest.trim_start())?.time(),
        Err(_) => finished(parsed, after_minute.trim_start())?.time(),
    }
}

/// What chrono's `NaiveDateTime: FromStr` reads: an upper-case `T` between
/// the date and a time whose seconds are required.
pub(super) fn relaxed_datetime(text: &str) -> Result<DateTime, DateError> {
    let mut parsed = Parsed::default();
    let rest = scan(&mut parsed, text, &RELAXED_DATE)?;
    let rest = scan(&mut parsed, rest, &[Item::Space(""), Item::Literal("T")])?;
    let rest = scan(&mut parsed, rest, &RELAXED_HOUR_MINUTE)?;
    let rest = scan(&mut parsed, rest, &RELAXED_SECOND)?;
    finished(parsed, rest)?.datetime()
}

/// What chrono's `DateTime<Utc>: FromStr` reads, and so what its serde
/// reads: RFC 3339 relaxed. Digits need no padding, the separator may be `T`,
/// `t` or a space, whitespace may sit between components and after the
/// value, `UTC` is an offset, and the offset's colon may be missing.
pub(super) fn relaxed_rfc3339(text: &str) -> Result<(EngineTimestamp, i32), DateError> {
    let mut parsed = Parsed::default();
    let rest = scan(&mut parsed, text, &RELAXED_DATE)?;
    let rest = match rest.strip_prefix(['t', 'T', ' ']) {
        Some(tail) => tail,
        None if rest.is_empty() => return Err(DateError::TooShort),
        None => return Err(DateError::InvalidText),
    };
    let rest = scan(&mut parsed, rest, &RELAXED_HOUR_MINUTE)?;
    let rest = scan(&mut parsed, rest, &RELAXED_SECOND)?.trim_start();
    let (rest, seconds) = match rest.get(..3) {
        Some(name) if name.eq_ignore_ascii_case("UTC") => (rest.get(3..).unwrap_or(""), 0),
        _ => offset(rest, Colon::Loose, true, false)?,
    };
    set(&mut parsed.offset, seconds, i64::MIN..=i64::MAX)?;
    finished(parsed, rest.trim_start())?.timestamp()
}

/// What chrono's `DateTime::parse_from_rfc3339` reads: RFC 3339 itself.
/// Every field has its full width, the separator is `T`, `t` or a space, and
/// the offset is `Z`, `z` or `±hh:mm` with its colon.
pub(super) fn strict_rfc3339(text: &str) -> Result<(EngineTimestamp, i32), DateError> {
    strict_rfc3339_fields(text)?.timestamp()
}

/// The fields of strict RFC 3339 text, before they are placed on the time
/// line: for a reader that wants the date and time as written.
///
/// The checks run in the order chrono's `parse_rfc3339` ran them, so a text
/// with two faults is rejected for the one chrono named: the first 19 bytes
/// must be there, the date must exist before the `T` is looked at, and the
/// fraction is read before the time of day is checked.
pub(super) fn strict_rfc3339_fields(text: &str) -> Result<Parsed, DateError> {
    let head = text.as_bytes().get(..19).ok_or(DateError::TooShort)?;
    let digit = |index: usize| match head.get(index) {
        Some(&byte) if byte.is_ascii_digit() => Ok(i64::from(byte - b'0')),
        _ => Err(DateError::InvalidText),
    };
    let separator = |index: usize, allowed: &[u8]| match head.get(index) {
        Some(byte) if allowed.contains(byte) => Ok(()),
        _ => Err(DateError::InvalidText),
    };
    let year = digit(0)? * 1000 + digit(1)? * 100 + digit(2)? * 10 + digit(3)?;
    separator(4, b"-")?;
    let month = digit(5)? * 10 + digit(6)?;
    separator(7, b"-")?;
    let day = digit(8)? * 10 + digit(9)?;
    if !(1..=days_in_month(year, month)).contains(&day) {
        return Err(DateError::OutOfRange);
    }
    separator(10, b"tT ")?;
    let hour = digit(11)? * 10 + digit(12)?;
    separator(13, b":")?;
    let minute = digit(14)? * 10 + digit(15)?;
    separator(16, b":")?;
    let second = digit(17)? * 10 + digit(18)?;
    // The 19 bytes are ASCII digits and separators, so 19 is a boundary.
    let mut rest = text.get(19..).ok_or(DateError::InvalidText)?;
    let mut nanosecond = None;
    if let Some(digits) = rest.strip_prefix('.') {
        let (tail, nanos) = fraction(digits)?;
        nanosecond = Some(nanos);
        rest = tail;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return Err(DateError::OutOfRange);
    }
    let (rest, offset) = offset(rest, Colon::Required, true, false)?;
    let parsed = Parsed {
        year: Some(year),
        month: Some(month),
        day: Some(day),
        weekday: None,
        hour_div_12: Some(hour / 12),
        hour_mod_12: Some(hour % 12),
        minute: Some(minute),
        second: Some(second),
        nanosecond,
        offset: Some(offset),
    };
    finished(parsed, rest)
}
