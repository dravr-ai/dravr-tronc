// ABOUTME: Zone, the Dravr-owned IANA time zone that replaces chrono_tz::Tz, on a zone database compiled in
// ABOUTME: Converts between timestamps and wall-clock readings and reports gaps and folds the way chrono did
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use jiff::tz::{AmbiguousOffset, Offset, TimeZone, TimeZoneDatabase};

use super::civil_date::CivilDate;
use super::civil_datetime::CivilDateTime;
use super::error::DateError;
use super::offset::Offset as UtcOffset;
use super::pattern::{OnClock, Pattern};
use super::text::{self, Moment};
use super::timestamp::Timestamp;

/// The zone database compiled into the binary. Named explicitly and never
/// the process-wide default, so the host's zone files, a `TZDIR` variable or
/// another crate's features cannot change which rules a `Zone` uses.
fn database() -> &'static TimeZoneDatabase {
    static DATABASE: OnceLock<TimeZoneDatabase> = OnceLock::new();
    DATABASE.get_or_init(TimeZoneDatabase::bundled)
}

/// The name the IANA database gives a machine whose zone was never set. It
/// is in the compiled-in database and it is not a place: chrono-tz left it
/// out, and so does [`Zone::from_name`].
const PLACEHOLDER_ZONE: &str = "Factory";

/// An IANA time zone, such as `America/Toronto`.
///
/// Replaces `chrono_tz::Tz`. The zone rules come from a database compiled
/// into the binary, so they do not depend on the host and they move forward
/// with a tronc release.
///
/// A zone is found by its exact name, case included, as with chrono-tz. It
/// is not serialized: a zone crosses the wire and the database as its
/// [`name`](Self::name), a plain string, and is looked up again on the way
/// in.
///
/// ```
/// use dravr_tronc::dates::{CivilDate, Timestamp, Zone};
///
/// let zone = Zone::from_name("America/Toronto")?;
/// let timestamp = Timestamp::parse_from_rfc3339("2026-07-01T03:30:00Z")?;
///
/// // The athlete's calendar day is still 30 June.
/// assert_eq!(zone.date_at(timestamp), CivilDate::from_ymd(2026, 6, 30)?);
/// assert_eq!(zone.offset_seconds_at(timestamp), -4 * 3600);
///
/// // 02:30 on the night the clocks go forward does not exist.
/// let skipped = CivilDate::from_ymd(2026, 3, 8)?.and_hms(2, 30, 0)?;
/// assert_eq!(zone.resolve(skipped).earliest(), None);
/// # Ok::<(), dravr_tronc::dates::DateError>(())
/// ```
#[derive(Clone)]
pub struct Zone {
    name: Arc<str>,
    rules: TimeZone,
    /// Made by [`Zone::fixed`]: its name is its offset and its abbreviation.
    fixed: bool,
}

/// What a wall-clock reading is in a zone: one timestamp, two, or none.
///
/// The counterpart of chrono's `LocalResult`, with the same three cases and
/// the same accessors, so a call site that wrote
/// `tz.from_local_datetime(&local).earliest()` writes
/// `zone.resolve(local).earliest()` and gets the same answer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LocalResolution {
    /// The reading does not exist: the clocks jumped over it (a spring
    /// "gap"), or it is outside the range of [`Timestamp`].
    None,
    /// The reading happens exactly once.
    Single(Timestamp),
    /// The reading happens twice, because the clocks were set back over it
    /// (an autumn "fold"): the earlier timestamp, then the later one.
    Ambiguous(Timestamp, Timestamp),
}

impl LocalResolution {
    /// The timestamp when there is exactly one.
    #[must_use]
    pub const fn single(self) -> Option<Timestamp> {
        match self {
            Self::Single(timestamp) => Some(timestamp),
            Self::None | Self::Ambiguous(..) => None,
        }
    }

    /// The only timestamp, or the first of two. `None` in a gap.
    #[must_use]
    pub const fn earliest(self) -> Option<Timestamp> {
        match self {
            Self::Single(timestamp) | Self::Ambiguous(timestamp, _) => Some(timestamp),
            Self::None => None,
        }
    }

    /// The only timestamp, or the second of two. `None` in a gap.
    #[must_use]
    pub const fn latest(self) -> Option<Timestamp> {
        match self {
            Self::Single(timestamp) | Self::Ambiguous(_, timestamp) => Some(timestamp),
            Self::None => None,
        }
    }
}

impl Zone {
    /// Coordinated Universal Time, under the name `UTC`.
    #[must_use]
    pub fn utc() -> Self {
        Self {
            name: Arc::from("UTC"),
            rules: TimeZone::UTC,
            fixed: false,
        }
    }

    /// The zone with this IANA name, such as `Europe/Paris`. The name must
    /// match exactly, case included.
    ///
    /// # Errors
    ///
    /// [`DateError::UnknownZone`] when the compiled-in database has no zone
    /// of that name.
    pub fn from_name(name: &str) -> Result<Self, DateError> {
        let unknown = || DateError::UnknownZone(name.to_owned());
        let rules = database().get(name).map_err(|_| unknown())?;
        // The database matches without regard to case, and knows two
        // placeholders for "no zone was set"; chrono-tz did neither.
        if name == PLACEHOLDER_ZONE || rules.is_unknown() || rules.iana_name() != Some(name) {
            return Err(unknown());
        }
        Ok(Self {
            name: Arc::from(name),
            rules,
            fixed: false,
        })
    }

    /// A zone whose offset never changes, `offset_seconds` east of UTC: what
    /// chrono's `FixedOffset` was, for a sender that states an offset and no
    /// zone.
    ///
    /// It is named as chrono wrote the offset: `+10:00`, `-07:00`, and
    /// `+05:45:30` when the offset has seconds. That is not an IANA name, so
    /// [`Zone::from_name`] does not find it again.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] unless `offset_seconds` is less than a day
    /// from UTC, the range chrono's `FixedOffset::east_opt` accepted.
    pub fn fixed(offset_seconds: i32) -> Result<Self, DateError> {
        UtcOffset::from_seconds(offset_seconds).map(Self::from)
    }

    /// The release of the IANA zone database compiled into the binary, such
    /// as `2026f`: the rules every `Zone` uses. A service can report it, and
    /// a test can pin it so that a tronc release moving the zone rules is
    /// seen and reviewed.
    #[must_use]
    pub fn database_release() -> &'static str {
        jiff_tzdb::VERSION
    }

    /// The name this zone was made with: the IANA name it was found by, or
    /// the offset of a [`Zone::fixed`] zone.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The date and time of day the zone's clocks show at `timestamp`. What
    /// `timestamp.with_timezone(&tz).naive_local()` returned.
    #[must_use]
    pub fn civil_at(&self, timestamp: Timestamp) -> CivilDateTime {
        CivilDateTime(self.rules.to_datetime(timestamp.0))
    }

    /// The calendar date in the zone at `timestamp`: the athlete's local day.
    /// What `timestamp.with_timezone(&tz).date_naive()` returned.
    #[must_use]
    pub fn date_at(&self, timestamp: Timestamp) -> CivilDate {
        self.civil_at(timestamp).date()
    }

    /// The zone's offset at `timestamp`, in seconds east of UTC.
    #[must_use]
    pub fn offset_seconds_at(&self, timestamp: Timestamp) -> i32 {
        self.rules.to_offset(timestamp.0).seconds()
    }

    /// The zone's abbreviation at `timestamp`, such as `EST` or `CEST`; a
    /// [`Zone::fixed`] zone's is its name.
    #[must_use]
    pub fn abbreviation_at(&self, timestamp: Timestamp) -> String {
        let info = self.rules.to_offset_info(timestamp.0);
        self.abbreviation(info.abbreviation()).to_owned()
    }

    /// The abbreviation the rules give, or for a [`Zone::fixed`] zone its
    /// name, which is what chrono's `%Z` wrote for a `FixedOffset`.
    fn abbreviation<'a>(&'a self, from_rules: &'a str) -> &'a str {
        if self.fixed {
            &self.name
        } else {
            from_rules
        }
    }

    /// The timestamp or timestamps at which the zone's clocks show `local`.
    ///
    /// A reading the clocks skipped is [`LocalResolution::None`] and one
    /// they showed twice is [`LocalResolution::Ambiguous`]; nothing is
    /// shifted or chosen for the caller.
    #[must_use]
    pub fn resolve(&self, local: CivilDateTime) -> LocalResolution {
        let (before, after) = match self.rules.to_ambiguous_timestamp(local.0).offset() {
            AmbiguousOffset::Unambiguous { offset } => (offset, offset),
            AmbiguousOffset::Gap { before, after } | AmbiguousOffset::Fold { before, after } => {
                (before, after)
            }
        };
        // The engine reports the offsets on each side of the nearest change
        // of rule. An offset counts only when it is the zone's own at the
        // timestamp it gives, so every answer shows `local` on the zone's
        // clocks. That is no timestamp in a gap, and one where the rule
        // changed and the clocks did not move.
        let shown_at = |offset: Offset| {
            let timestamp = offset.to_timestamp(local.0).ok()?;
            (self.rules.to_offset(timestamp) == offset).then_some(Timestamp(timestamp))
        };
        match (shown_at(before), shown_at(after)) {
            (Some(first), Some(second)) if first < second => {
                LocalResolution::Ambiguous(first, second)
            }
            (Some(first), Some(second)) if second < first => {
                LocalResolution::Ambiguous(second, first)
            }
            (Some(only), _) | (None, Some(only)) => LocalResolution::Single(only),
            (None, None) => LocalResolution::None,
        }
    }

    /// The first timestamp of `date` in the zone.
    ///
    /// Usually local midnight. Where the clocks skip midnight (a zone that
    /// goes forward at 00:00) it is the first timestamp after the gap, and
    /// where midnight happens twice it is the first of the two. Total where
    /// `resolve(date.at_midnight())` is not.
    ///
    /// # Errors
    ///
    /// [`DateError::OutOfRange`] at the ends of the range of [`Timestamp`].
    pub fn start_of_day(&self, date: CivilDate) -> Result<Timestamp, DateError> {
        self.rules
            .to_ambiguous_timestamp(date.at_midnight().0)
            .compatible()
            .map(Timestamp)
            .map_err(|_| DateError::OutOfRange)
    }

    /// Formats `timestamp` as the zone's clocks show it, with a `strftime`
    /// string. `%z` and `%:z` write the zone's offset and `%Z` its
    /// abbreviation. The [module documentation](crate::dates#format-strings)
    /// lists the specifiers.
    ///
    /// # Errors
    ///
    /// [`DateError::UnsupportedFormat`] for a specifier that is not
    /// implemented.
    pub fn format(&self, timestamp: Timestamp, format: &str) -> Result<String, DateError> {
        let info = self.rules.to_offset_info(timestamp.0);
        let civil = info.offset().to_datetime(timestamp.0);
        text::format(
            &Moment {
                date: Some(civil.date()),
                time: Some(civil.time()),
                offset: Some((
                    info.offset().seconds(),
                    self.abbreviation(info.abbreviation()),
                )),
            },
            format,
        )
    }

    /// Formats `timestamp` as the zone's clocks show it, with a [`Pattern`]
    /// checked when it was compiled, so the formatting cannot fail. The
    /// bytes are what [`format`](Self::format) writes for the same string.
    ///
    /// ```
    /// use dravr_tronc::date_pattern;
    /// use dravr_tronc::dates::{Timestamp, Zone};
    ///
    /// let toronto = Zone::from_name("America/Toronto")?;
    /// let at = Timestamp::parse_from_rfc3339("2026-11-17T15:30:00Z")?;
    /// assert_eq!(toronto.format_with(at, &date_pattern!(Timestamp, "%Y-%m-%d %H:%M")), "2026-11-17 10:30");
    /// # Ok::<(), dravr_tronc::dates::DateError>(())
    /// ```
    #[must_use]
    pub fn format_with(&self, timestamp: Timestamp, pattern: &Pattern<Timestamp>) -> String {
        let info = self.rules.to_offset_info(timestamp.0);
        pattern.render(&OnClock {
            civil: info.offset().to_datetime(timestamp.0),
            offset: info.offset().seconds(),
            name: self.abbreviation(info.abbreviation()),
        })
    }
}

impl From<UtcOffset> for Zone {
    /// The zone that keeps this offset all year: [`Zone::fixed`] for an
    /// offset already checked. It is named as chrono wrote the offset,
    /// `+05:30` or `+05:45:30`.
    fn from(offset: UtcOffset) -> Self {
        Self {
            name: Arc::from(offset.to_string()),
            rules: TimeZone::fixed(offset.0),
            fixed: true,
        }
    }
}

impl PartialEq for Zone {
    /// Two zones are equal when they have the same name. `US/Eastern` and
    /// `America/New_York` share their rules and are still different zones,
    /// as their `Tz` variants were.
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Zone {}

impl Hash for Zone {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl fmt::Display for Zone {
    /// The IANA name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.name)
    }
}

impl fmt::Debug for Zone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.name)
    }
}

impl FromStr for Zone {
    type Err = DateError;

    fn from_str(name: &str) -> Result<Self, DateError> {
        Self::from_name(name)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono_tz::TZ_VARIANTS;
    use jiff::civil::date;
    use jiff::tz::TimeZone;
    use jiff::{SignedDuration, Timestamp as EngineTimestamp};

    use super::{database, LocalResolution, Timestamp, Zone};

    fn utc_midnight(year: i16) -> EngineTimestamp {
        TimeZone::UTC
            .to_timestamp(date(year, 1, 1).at(0, 0, 0, 0))
            .unwrap()
    }

    /// Whether the reading `zone` shows at `at` resolves back to `at`, and
    /// every timestamp it resolves to shows that reading.
    fn round_trips(zone: &Zone, at: Timestamp) -> bool {
        let local = zone.civil_at(at);
        match zone.resolve(local) {
            LocalResolution::Single(only) => only == at,
            LocalResolution::Ambiguous(first, second) => {
                first < second
                    && (first == at || second == at)
                    && zone.civil_at(first) == local
                    && zone.civil_at(second) == local
            }
            LocalResolution::None => false,
        }
    }

    /// Covers every zone of the compiled-in database and every change of
    /// rule it lists from 1900 to 2040: each quarter hour from three hours
    /// before the change to three hours after it. The changes are the ones
    /// the engine lists, so this reaches years the uniform grid in
    /// `tests/it/dates_zone_test.rs` does not, and that grid reaches readings
    /// far from any listed change.
    #[test]
    fn a_reading_resolves_back_to_its_timestamp_around_every_change_of_rule() {
        let (from, until) = (utc_midnight(1900), utc_midnight(2040));
        let quarter = SignedDuration::from_mins(15);
        let (mut zones, mut changes, mut broken) = (0, 0, Vec::new());
        for name in database().available() {
            let Ok(zone) = Zone::from_name(name.as_str()) else {
                continue;
            };
            zones += 1;
            for change in zone
                .rules
                .following(from)
                .take_while(|change| change.timestamp() < until)
            {
                changes += 1;
                for step in -12..=12 {
                    let at = Timestamp(change.timestamp() + quarter * step);
                    if !round_trips(&zone, at) {
                        let local = zone.civil_at(at);
                        broken.push(format!("{name} {at} {local}: {:?}", zone.resolve(local)));
                    }
                }
            }
        }
        assert_eq!(broken, Vec::<String>::new());
        // Every zone was walked and the walk found their changes of rule.
        assert!(zones >= TZ_VARIANTS.len(), "zones {zones}");
        assert!(changes > 30_000, "changes {changes}");
    }

    /// The names the database offers and `Zone` accepts are chrono-tz's
    /// names: it knows no placeholder zone and no zone chrono-tz lacked.
    #[test]
    fn the_accepted_names_are_the_names_chrono_tz_had() {
        let theirs: BTreeSet<&str> = TZ_VARIANTS.iter().map(|tz| tz.name()).collect();
        let offered: Vec<String> = database()
            .available()
            .map(|name| name.as_str().to_owned())
            .collect();
        let extra: Vec<&str> = offered
            .iter()
            .map(String::as_str)
            .filter(|name| Zone::from_name(name).is_ok() && !theirs.contains(name))
            .collect();
        assert_eq!(extra, Vec::<&str>::new());
        assert!(offered.len() >= theirs.len(), "offered {}", offered.len());
    }
}
