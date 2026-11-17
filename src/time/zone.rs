// ABOUTME: Zone, the Dravr-owned IANA time zone that replaces chrono_tz::Tz, on a zone database compiled in
// ABOUTME: Converts between instants and wall-clock readings and reports gaps and folds the way chrono did
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
use super::error::TimeError;
use super::instant::Instant;
use super::text::{self, Moment};

/// The zone database compiled into the binary. Named explicitly and never
/// the process-wide default, so the host's zone files, a `TZDIR` variable or
/// another crate's features cannot change which rules a `Zone` uses.
fn database() -> &'static TimeZoneDatabase {
    static DATABASE: OnceLock<TimeZoneDatabase> = OnceLock::new();
    DATABASE.get_or_init(TimeZoneDatabase::bundled)
}

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
/// use dravr_tronc::time::{CivilDate, Instant, Zone};
///
/// let zone = Zone::from_name("America/Toronto")?;
/// let instant = Instant::parse_from_rfc3339("2026-07-01T03:30:00Z")?;
///
/// // The athlete's calendar day is still 30 June.
/// assert_eq!(zone.date_at(instant), CivilDate::from_ymd(2026, 6, 30)?);
/// assert_eq!(zone.offset_seconds_at(instant), -4 * 3600);
///
/// // 02:30 on the night the clocks go forward does not exist.
/// let skipped = CivilDate::from_ymd(2026, 3, 8)?.and_hms(2, 30, 0)?;
/// assert_eq!(zone.resolve(skipped).earliest(), None);
/// # Ok::<(), dravr_tronc::time::TimeError>(())
/// ```
#[derive(Clone)]
pub struct Zone {
    name: Arc<str>,
    rules: TimeZone,
}

/// What a wall-clock reading is in a zone: one instant, two, or none.
///
/// The counterpart of chrono's `LocalResult`, with the same three cases and
/// the same accessors, so a call site that wrote
/// `tz.from_local_datetime(&local).earliest()` writes
/// `zone.resolve(local).earliest()` and gets the same answer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LocalResolution {
    /// The reading does not exist: the clocks jumped over it (a spring
    /// "gap"), or it is outside the range of [`Instant`].
    None,
    /// The reading happens exactly once.
    Single(Instant),
    /// The reading happens twice, because the clocks were set back over it
    /// (an autumn "fold"): the earlier instant, then the later one.
    Ambiguous(Instant, Instant),
}

impl LocalResolution {
    /// The instant when there is exactly one.
    #[must_use]
    pub const fn single(self) -> Option<Instant> {
        match self {
            Self::Single(instant) => Some(instant),
            Self::None | Self::Ambiguous(..) => None,
        }
    }

    /// The only instant, or the first of two. `None` in a gap.
    #[must_use]
    pub const fn earliest(self) -> Option<Instant> {
        match self {
            Self::Single(instant) | Self::Ambiguous(instant, _) => Some(instant),
            Self::None => None,
        }
    }

    /// The only instant, or the second of two. `None` in a gap.
    #[must_use]
    pub const fn latest(self) -> Option<Instant> {
        match self {
            Self::Single(instant) | Self::Ambiguous(_, instant) => Some(instant),
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
        }
    }

    /// The zone with this IANA name, such as `Europe/Paris`. The name must
    /// match exactly, case included.
    ///
    /// # Errors
    ///
    /// [`TimeError::UnknownZone`] when the compiled-in database has no zone
    /// of that name.
    pub fn from_name(name: &str) -> Result<Self, TimeError> {
        let unknown = || TimeError::UnknownZone(name.to_owned());
        let rules = database().get(name).map_err(|_| unknown())?;
        // The database matches without regard to case and knows a
        // placeholder zone; chrono-tz did neither.
        if rules.is_unknown() || rules.iana_name() != Some(name) {
            return Err(unknown());
        }
        Ok(Self {
            name: Arc::from(name),
            rules,
        })
    }

    /// The IANA name this zone was found by.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The date and time of day the zone's clocks show at `instant`. What
    /// `instant.with_timezone(&tz).naive_local()` returned.
    #[must_use]
    pub fn civil_at(&self, instant: Instant) -> CivilDateTime {
        CivilDateTime(self.rules.to_datetime(instant.0))
    }

    /// The calendar date in the zone at `instant`: the athlete's local day.
    /// What `instant.with_timezone(&tz).date_naive()` returned.
    #[must_use]
    pub fn date_at(&self, instant: Instant) -> CivilDate {
        self.civil_at(instant).date()
    }

    /// The zone's offset at `instant`, in seconds east of UTC.
    #[must_use]
    pub fn offset_seconds_at(&self, instant: Instant) -> i32 {
        self.rules.to_offset(instant.0).seconds()
    }

    /// The zone's abbreviation at `instant`, such as `EST` or `CEST`.
    #[must_use]
    pub fn abbreviation_at(&self, instant: Instant) -> String {
        self.rules
            .to_offset_info(instant.0)
            .abbreviation()
            .to_owned()
    }

    /// The instant or instants at which the zone's clocks show `local`.
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
        // instant it gives, so every answer shows `local` on the zone's
        // clocks. That is no instant in a gap, and one where the rule
        // changed and the clocks did not move.
        let shown_at = |offset: Offset| {
            let instant = offset.to_timestamp(local.0).ok()?;
            (self.rules.to_offset(instant) == offset).then_some(Instant(instant))
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

    /// The first instant of `date` in the zone.
    ///
    /// Usually local midnight. Where the clocks skip midnight (a zone that
    /// goes forward at 00:00) it is the first instant after the gap, and
    /// where midnight happens twice it is the first of the two. Total where
    /// `resolve(date.at_midnight())` is not.
    ///
    /// # Errors
    ///
    /// [`TimeError::OutOfRange`] at the ends of the range of [`Instant`].
    pub fn start_of_day(&self, date: CivilDate) -> Result<Instant, TimeError> {
        self.rules
            .to_ambiguous_timestamp(date.at_midnight().0)
            .compatible()
            .map(Instant)
            .map_err(|_| TimeError::OutOfRange)
    }

    /// Formats `instant` as the zone's clocks show it, with a `strftime`
    /// string. `%z` and `%:z` write the zone's offset and `%Z` its
    /// abbreviation. The [module documentation](crate::time#format-strings)
    /// lists the specifiers.
    ///
    /// # Errors
    ///
    /// [`TimeError::UnsupportedFormat`] for a specifier that is not
    /// implemented.
    pub fn format(&self, instant: Instant, format: &str) -> Result<String, TimeError> {
        let info = self.rules.to_offset_info(instant.0);
        let civil = info.offset().to_datetime(instant.0);
        text::format(
            &Moment {
                date: Some(civil.date()),
                time: Some(civil.time()),
                offset: Some((info.offset().seconds(), info.abbreviation())),
            },
            format,
        )
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
    type Err = TimeError;

    fn from_str(name: &str) -> Result<Self, TimeError> {
        Self::from_name(name)
    }
}

#[cfg(test)]
mod tests {
    use chrono_tz::TZ_VARIANTS;
    use jiff::civil::date;
    use jiff::tz::TimeZone;
    use jiff::{SignedDuration, Timestamp};

    use super::{database, Instant, LocalResolution, Zone};

    fn utc_midnight(year: i16) -> Timestamp {
        TimeZone::UTC
            .to_timestamp(date(year, 1, 1).at(0, 0, 0, 0))
            .unwrap()
    }

    /// Whether the reading `zone` shows at `at` resolves back to `at`, and
    /// every instant it resolves to shows that reading.
    fn round_trips(zone: &Zone, at: Instant) -> bool {
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
    /// `tests/it/time_zone_test.rs` does not, and that grid reaches readings
    /// far from any listed change.
    #[test]
    fn a_reading_resolves_back_to_its_instant_around_every_change_of_rule() {
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
                    let at = Instant(change.timestamp() + quarter * step);
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
}
