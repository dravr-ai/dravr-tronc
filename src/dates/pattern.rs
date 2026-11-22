// ABOUTME: Pattern, a strftime string checked when it is compiled, so formatting with it cannot fail
// ABOUTME: Tied to the kind of value it formats; the date_pattern! macro refuses a bad pattern at build time
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::str;

use jiff::civil::{Date, DateTime, Time};

use super::civil_date::CivilDate;
use super::civil_datetime::CivilDateTime;
use super::civil_time::CivilTime;
use super::text::{self, DateField, OffsetField, Pad, Specifier, TimeField, Writes};
use super::timestamp::Timestamp;

/// The most parts a pattern holds: a literal run or one specifier each.
/// `%Y-%m-%dT%H:%M:%S%.f%:z` is twelve.
pub const MAX_PATTERN_PARTS: usize = 48;

mod sealed {
    /// What a kind of value can give a pattern. Each component is a witness
    /// type: `()` when the value has it, `Infallible` when it has none, so a
    /// part that needs a missing component cannot be built, and a writer
    /// never meets one.
    pub trait Kind: Copy + 'static {
        /// `()` when the value has a calendar date.
        type Date: Copy;
        /// `()` when the value has a time of day.
        type Time: Copy;
        /// `()` when the value has an offset and a zone name.
        type Offset: Copy;
        /// The date witness, when there is one.
        const DATE: Option<Self::Date>;
        /// The time witness, when there is one.
        const TIME: Option<Self::Time>;
        /// The offset witness, when there is one.
        const OFFSET: Option<Self::Offset>;
    }
}

/// A kind of value a [`Pattern`] formats: [`Timestamp`] (also through a
/// `Zone`), [`CivilDateTime`], [`CivilDate`] or [`CivilTime`].
///
/// Sealed: the four date types are the only kinds.
pub trait Formattable: sealed::Kind {}

impl sealed::Kind for Timestamp {
    type Date = ();
    type Time = ();
    type Offset = ();
    const DATE: Option<()> = Some(());
    const TIME: Option<()> = Some(());
    const OFFSET: Option<()> = Some(());
}
impl Formattable for Timestamp {}

impl sealed::Kind for CivilDateTime {
    type Date = ();
    type Time = ();
    type Offset = Infallible;
    const DATE: Option<()> = Some(());
    const TIME: Option<()> = Some(());
    const OFFSET: Option<Infallible> = None;
}
impl Formattable for CivilDateTime {}

impl sealed::Kind for CivilDate {
    type Date = ();
    type Time = Infallible;
    type Offset = Infallible;
    const DATE: Option<()> = Some(());
    const TIME: Option<Infallible> = None;
    const OFFSET: Option<Infallible> = None;
}
impl Formattable for CivilDate {}

impl sealed::Kind for CivilTime {
    type Date = Infallible;
    type Time = ();
    type Offset = Infallible;
    const DATE: Option<Infallible> = None;
    const TIME: Option<()> = Some(());
    const OFFSET: Option<Infallible> = None;
}
impl Formattable for CivilTime {}

/// One part of a compiled pattern. A component part carries its kind's
/// witness, so it exists only for a kind that has the component.
#[derive(Clone, Copy)]
enum Part<V: Formattable> {
    Text(&'static str),
    Date(DateField, V::Date),
    Time(TimeField, V::Time),
    Offset(OffsetField, V::Offset),
    Unix(Pad, V::Date, V::Time),
}

/// Why a string is not a [`Pattern`] for a kind of value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum PatternError {
    /// A specifier that is not implemented, that is read and never written
    /// (`%#z`), that the kind of value has no component for (`%H` for a
    /// [`CivilDate`]), or that can fail on the value (`%C`, which has no
    /// century to write before year 0). The string is the specifier.
    Unsupported(&'static str),
    /// More than [`MAX_PATTERN_PARTS`] parts.
    TooLong,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(spec) => write!(f, "unsupported format specifier `{spec}`"),
            Self::TooLong => write!(f, "a pattern holds at most {MAX_PATTERN_PARTS} parts"),
        }
    }
}

impl Error for PatternError {}

/// A `strftime` string checked once, when it is compiled, so that formatting
/// a value with it cannot fail.
///
/// `format("…")` on the date types returns a `Result`, because the string
/// could name a specifier the value cannot write. A `Pattern` is checked
/// against the kind of value `V` it formats: every specifier is one the
/// types implement and the kind has a component for, and none can fail on
/// the value. So `format_with` returns the `String` itself, with nothing to
/// unwrap and nothing to default.
///
/// Write one with [`date_pattern!`](crate::date_pattern), which checks it
/// when the crate is built: a pattern that does not hold is a compile error,
/// never a runtime one. [`Pattern::new`] does the same check as a `const fn`
/// returning a `Result`, for a string that is only known later.
///
/// The output is what `format` writes for the same string, byte for byte.
///
/// ```
/// use dravr_tronc::date_pattern;
/// use dravr_tronc::dates::{CivilDate, Pattern, Timestamp};
///
/// const MINUTE_UTC: Pattern<Timestamp> = date_pattern!(Timestamp, "%Y-%m-%d %H:%M UTC");
/// const MONTH: Pattern<CivilDate> = date_pattern!(CivilDate, "%Y-%m");
///
/// let at = Timestamp::parse_from_rfc3339("2026-11-17T10:00:05Z")?;
/// assert_eq!(at.format_with(&MINUTE_UTC), "2026-11-17 10:00 UTC");
/// assert_eq!(at.utc_date().format_with(&MONTH), "2026-11");
///
/// // A date has no hour: refused when checked, never when formatting.
/// assert!(Pattern::<CivilDate>::new("%Y-%m-%d %H").is_err());
/// # Ok::<(), dravr_tronc::dates::DateError>(())
/// ```
///
/// ```compile_fail,E0277
/// use dravr_tronc::date_pattern;
/// use dravr_tronc::dates::{CivilDate, Pattern};
///
/// // `%H` on a date does not build.
/// const BROKEN: Pattern<CivilDate> = date_pattern!(CivilDate, "%Y %H");
/// ```
#[derive(Clone, Copy)]
pub struct Pattern<V: Formattable> {
    text: &'static str,
    parts: [Part<V>; MAX_PATTERN_PARTS],
    len: usize,
}

impl<V: Formattable> Pattern<V> {
    /// Compiles `text` for values of kind `V`.
    ///
    /// # Errors
    ///
    /// [`PatternError::Unsupported`] naming the first specifier that cannot
    /// be written for every value of the kind, and
    /// [`PatternError::TooLong`] past [`MAX_PATTERN_PARTS`] parts.
    pub const fn new(text: &'static str) -> Result<Self, PatternError> {
        let compiled = compile::<V>(text);
        match compiled.error {
            Some(error) => Err(error),
            None => Ok(compiled.pattern),
        }
    }

    /// The string the pattern was compiled from.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.text
    }

    /// Writes `source`'s components by the parts, failing only when `out`
    /// does.
    fn write(&self, out: &mut fmt::Formatter<'_>, source: &impl Source<V>) -> fmt::Result {
        for part in self.parts.iter().take(self.len) {
            match *part {
                Part::Text(text) => out.write_str(text)?,
                Part::Date(field, witness) => field.write(out, source.date(witness))?,
                Part::Time(field, witness) => field.write(out, source.time(witness))?,
                Part::Offset(field, witness) => {
                    let (seconds, name) = source.offset(witness);
                    field.write(out, seconds, name)?;
                }
                Part::Unix(pad, date, time) => text::write_unix(
                    out,
                    DateTime::from_parts(source.date(date), source.time(time)),
                    source.utc_offset(),
                    pad,
                )?,
            }
        }
        Ok(())
    }

    /// The value written by this pattern: what `format_with` returns.
    pub(super) fn render(&self, source: &impl Source<V>) -> String {
        struct Rendered<'a, V: Formattable, S: Source<V>>(&'a Pattern<V>, &'a S);

        impl<V: Formattable, S: Source<V>> fmt::Display for Rendered<'_, V, S> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.write(f, self.1)
            }
        }

        Rendered(self, source).to_string()
    }
}

impl<V: Formattable> fmt::Debug for Pattern<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Pattern").field(&self.text).finish()
    }
}

impl<V: Formattable> PartialEq for Pattern<V> {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl<V: Formattable> Eq for Pattern<V> {}

/// Where a pattern's components come from. Implemented inside the module
/// for each value; a component the kind lacks takes an `Infallible`, so its
/// method is never called.
pub(super) trait Source<V: Formattable> {
    fn date(&self, witness: V::Date) -> Date;
    fn time(&self, witness: V::Time) -> Time;
    fn offset(&self, witness: V::Offset) -> (i32, &str);
    /// Seconds east of UTC that `%s` subtracts: zero for a value with no
    /// offset, as `format` does.
    fn utc_offset(&self) -> i32;
}

/// A value with a place on the time line, as a clock shows it: a timestamp
/// in UTC, or in a zone.
pub(super) struct OnClock<'a> {
    pub(super) civil: DateTime,
    pub(super) offset: i32,
    pub(super) name: &'a str,
}

impl Source<Timestamp> for OnClock<'_> {
    fn date(&self, (): ()) -> Date {
        self.civil.date()
    }
    fn time(&self, (): ()) -> Time {
        self.civil.time()
    }
    fn offset(&self, (): ()) -> (i32, &str) {
        (self.offset, self.name)
    }
    fn utc_offset(&self) -> i32 {
        self.offset
    }
}

impl Source<CivilDateTime> for DateTime {
    fn date(&self, (): ()) -> Date {
        Self::date(*self)
    }
    fn time(&self, (): ()) -> Time {
        Self::time(*self)
    }
    fn offset(&self, never: Infallible) -> (i32, &str) {
        match never {}
    }
    fn utc_offset(&self) -> i32 {
        0
    }
}

impl Source<CivilDate> for Date {
    fn date(&self, (): ()) -> Date {
        *self
    }
    fn time(&self, never: Infallible) -> Time {
        match never {}
    }
    fn offset(&self, never: Infallible) -> (i32, &str) {
        match never {}
    }
    fn utc_offset(&self) -> i32 {
        0
    }
}

impl Source<CivilTime> for Time {
    fn date(&self, never: Infallible) -> Date {
        match never {}
    }
    fn time(&self, (): ()) -> Time {
        *self
    }
    fn offset(&self, never: Infallible) -> (i32, &str) {
        match never {}
    }
    fn utc_offset(&self) -> i32 {
        0
    }
}

/// The part `writes` asks for, when the kind has what it needs.
const fn part<V: Formattable>(writes: Writes<'static>) -> Option<Part<V>> {
    match writes {
        Writes::Text(text) => Some(Part::Text(text)),
        Writes::Date(field) => match V::DATE {
            Some(witness) => Some(Part::Date(field, witness)),
            None => None,
        },
        Writes::Time(field) => match V::TIME {
            Some(witness) => Some(Part::Time(field, witness)),
            None => None,
        },
        Writes::Offset(field) => match V::OFFSET {
            Some(witness) => Some(Part::Offset(field, witness)),
            None => None,
        },
        Writes::Unix(pad) => match (V::DATE, V::TIME) {
            (Some(date), Some(time)) => Some(Part::Unix(pad, date, time)),
            _ => None,
        },
        Writes::Century(_) | Writes::ReadOnly => None,
    }
}

/// `bytes[start..end]` as text. The bounds are always char boundaries (a
/// `%` or the end of a specifier), so the conversion always succeeds.
const fn slice(bytes: &'static [u8], start: usize, end: usize) -> Option<&'static str> {
    let Some((_, tail)) = bytes.split_at_checked(start) else {
        return None;
    };
    let Some((run, _)) = tail.split_at_checked(end - start) else {
        return None;
    };
    match str::from_utf8(run) {
        Ok(run) => Some(run),
        Err(_) => None,
    }
}

/// A compiled pattern and, when compiling stopped, why. The parts read so
/// far are kept either way, so [`date_pattern!`](crate::date_pattern) can
/// take the pattern out only through a check the compiler makes.
#[doc(hidden)]
pub struct Compiled<V: Formattable> {
    pattern: Pattern<V>,
    error: Option<PatternError>,
}

/// `VALID`, as a type the compiler can be asked about.
#[doc(hidden)]
pub struct Valid<const VALID: bool>;

/// Implemented only for `Valid<true>`.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "this date pattern does not hold for the kind of value it formats",
    label = "`Pattern::new` refuses it: an unknown specifier, one the value has no component for, `%C`, `%#z`, or too many parts",
    note = "call `dravr_tronc::dates::Pattern::<Kind>::new(\"…\")` to see which specifier"
)]
pub trait Holds {}

impl Holds for Valid<true> {}

impl<V: Formattable> Compiled<V> {
    /// Whether the string compiled.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.error.is_none()
    }

    /// The pattern, which the compiler hands over only when `VALID`, the
    /// verdict of [`is_valid`](Self::is_valid) on the same string, is true.
    #[must_use]
    pub const fn accept<const VALID: bool>(self) -> Pattern<V>
    where
        Valid<VALID>: Holds,
    {
        self.pattern
    }
}

/// Compiles `text` for kind `V`. Used by [`Pattern::new`] and by
/// [`date_pattern!`](crate::date_pattern).
#[doc(hidden)]
#[must_use]
pub const fn compile<V: Formattable>(text: &'static str) -> Compiled<V> {
    let bytes = text.as_bytes();
    let mut compiled = Compiled {
        pattern: Pattern {
            text,
            parts: [Part::Text(""); MAX_PATTERN_PARTS],
            len: 0,
        },
        error: None,
    };
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        if bytes[at] == b'%' {
            let Some((_, rest)) = bytes.split_at_checked(at) else {
                compiled.error = Some(PatternError::Unsupported("%"));
                return compiled;
            };
            let (specifier, used) = match text::lookup(rest) {
                Ok(found) => found,
                Err(used) => {
                    compiled.error = Some(unsupported(bytes, start, start + used));
                    return compiled;
                }
            };
            at += used;
            let refused = match specifier {
                Specifier::One(item) => push(&mut compiled.pattern, text::writes(item)),
                Specifier::Many(items) => {
                    let mut index = 0;
                    let mut refused = None;
                    while index < items.len() && refused.is_none() {
                        refused = push(&mut compiled.pattern, text::writes(items[index]));
                        index += 1;
                    }
                    refused
                }
            };
            if let Some(error) = refused {
                compiled.error = Some(match error {
                    PatternError::Unsupported(_) => unsupported(bytes, start, at),
                    PatternError::TooLong => PatternError::TooLong,
                });
                return compiled;
            }
        } else {
            // Text up to the next specifier is written as it is; how chrono
            // splits it at whitespace makes no difference to the output.
            while at < bytes.len() && bytes[at] != b'%' {
                at += 1;
            }
            let refused = match slice(bytes, start, at) {
                Some(run) => push(&mut compiled.pattern, text::Writes::Text(run)),
                None => Some(PatternError::Unsupported("")),
            };
            if let Some(error) = refused {
                compiled.error = Some(error);
                return compiled;
            }
        }
    }
    compiled
}

/// The specifier at `bytes[start..end]`, refused.
const fn unsupported(bytes: &'static [u8], start: usize, end: usize) -> PatternError {
    let end = if end > bytes.len() { bytes.len() } else { end };
    PatternError::Unsupported(match slice(bytes, start, end) {
        Some(spec) => spec,
        None => "%",
    })
}

/// Appends the part `writes` asks for, or says why it cannot.
const fn push<V: Formattable>(
    pattern: &mut Pattern<V>,
    writes: Writes<'static>,
) -> Option<PatternError> {
    let Some(part) = part::<V>(writes) else {
        return Some(PatternError::Unsupported(""));
    };
    if pattern.len >= MAX_PATTERN_PARTS {
        return Some(PatternError::TooLong);
    }
    pattern.parts[pattern.len] = part;
    pattern.len += 1;
    None
}

/// A [`Pattern`] checked when the crate is built.
///
/// `date_pattern!(Kind, "…")` is a `Pattern<Kind>` for `Timestamp`,
/// `CivilDateTime`, `CivilDate` or `CivilTime`. The string may be a literal
/// or a `&'static str` constant. A string [`Pattern::new`] would refuse does
/// not compile, so a pattern in the source can never fail at run time.
///
/// ```
/// use dravr_tronc::date_pattern;
/// use dravr_tronc::dates::{Pattern, Timestamp};
///
/// const QUERY: &str = "%Y-%m-%dT%H:%M:%S";
/// const QUERY_PATTERN: Pattern<Timestamp> = date_pattern!(Timestamp, QUERY);
///
/// let at = Timestamp::from_epoch_seconds(1_794_909_605)?;
/// assert_eq!(at.format_with(&QUERY_PATTERN), "2026-11-17T10:00:05");
/// assert_eq!(at.format_with(&date_pattern!(Timestamp, "%Y%m%d_%H%M%S")), "20261117_100005");
/// # Ok::<(), dravr_tronc::dates::DateError>(())
/// ```
#[macro_export]
macro_rules! date_pattern {
    ($kind:ty, $text:expr $(,)?) => {
        const {
            $crate::dates::__pattern::compile::<$kind>($text)
                .accept::<{ $crate::dates::__pattern::compile::<$kind>($text).is_valid() }>()
        }
    };
}
