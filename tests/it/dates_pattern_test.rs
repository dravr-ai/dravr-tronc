// ABOUTME: Goldens for Pattern and date_pattern!: what a checked pattern writes, and what it refuses to compile
// ABOUTME: Its bytes are format's for the same string and chrono's; its refusals are exactly the strings format can fail on
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! `Pattern`, `date_pattern!` and `format_with` (carnet#1620).
//!
//! A pattern is accepted for a kind of value exactly when `format` cannot
//! fail on any value of that kind, and then writes what `format` writes,
//! which is what chrono wrote. The platform's constant patterns, collected
//! from its `.format("…").unwrap_or_default()` sites, are written through
//! the macro and compared with chrono.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use dravr_tronc::date_pattern;
use dravr_tronc::dates::{
    CivilDate, CivilDateTime, CivilTime, DateError, Formattable, Pattern, PatternError, Timestamp,
    MAX_PATTERN_PARTS,
};

use crate::dates_format_test::{PLATFORM_FORMATS, SPECIFIERS};
use crate::dates_support::pairs;

/// A value a pattern can format, with both ways of formatting it.
trait Sample: Formattable + Sized {
    fn by_string(&self, format: &str) -> Result<String, DateError>;
    fn by_pattern(&self, pattern: &Pattern<Self>) -> String;
}

impl Sample for Timestamp {
    fn by_string(&self, format: &str) -> Result<String, DateError> {
        self.format(format)
    }
    fn by_pattern(&self, pattern: &Pattern<Self>) -> String {
        self.format_with(pattern)
    }
}

impl Sample for CivilDateTime {
    fn by_string(&self, format: &str) -> Result<String, DateError> {
        self.format(format)
    }
    fn by_pattern(&self, pattern: &Pattern<Self>) -> String {
        self.format_with(pattern)
    }
}

impl Sample for CivilDate {
    fn by_string(&self, format: &str) -> Result<String, DateError> {
        self.format(format)
    }
    fn by_pattern(&self, pattern: &Pattern<Self>) -> String {
        self.format_with(pattern)
    }
}

impl Sample for CivilTime {
    fn by_string(&self, format: &str) -> Result<String, DateError> {
        self.format(format)
    }
    fn by_pattern(&self, pattern: &Pattern<Self>) -> String {
        self.format_with(pattern)
    }
}

/// Strings that are not patterns of any kind, or are only for some, beside
/// [`SPECIFIERS`] and the platform's [`PLATFORM_FORMATS`].
const EDGES: [&str; 26] = [
    "",
    "plain text",
    "%n%t%%",
    "a  b\t%d  %H",
    "é%Yü",
    "%#z",
    "%q",
    "%E",
    "%-a",
    "%-%",
    "%.",
    "%.x",
    "%.3",
    "%.3x",
    "%.4f",
    "%:",
    "%:x",
    "%#",
    "%#x",
    "%",
    "%-",
    "%é",
    "x%",
    "%Y%",
    "%-C%d",
    "%Y-%m-%dT%H:%M:%S%.f%:z",
];

/// Every string the tests ask about.
fn strings() -> impl Iterator<Item = &'static str> {
    PLATFORM_FORMATS.into_iter().chain(SPECIFIERS).chain(EDGES)
}

/// For each string: a pattern of kind `V` compiles exactly when `format`
/// succeeds on every sample value and the string names no century (`%C`
/// fails on a year before 0 whatever the samples are), and it then writes
/// what `format` writes.
fn assert_kind<V: Sample>(samples: &[V]) -> (u32, u32) {
    let (mut accepted, mut refused) = (0, 0);
    for text in strings() {
        let format_holds = samples.iter().all(|value| value.by_string(text).is_ok());
        let century = text.contains("%C") || text.contains("%-C") || text.contains("%_C");
        match Pattern::<V>::new(text) {
            Ok(pattern) => {
                assert!(format_holds && !century, "{text:?} compiled");
                assert_eq!(pattern.as_str(), text);
                for value in samples {
                    assert_eq!(
                        Ok(value.by_pattern(&pattern)),
                        value.by_string(text),
                        "{text:?}"
                    );
                }
                accepted += 1;
            }
            Err(error) => {
                assert!(!format_holds || century, "{text:?} refused: {error}");
                refused += 1;
            }
        }
    }
    (accepted, refused)
}

#[test]
fn a_pattern_compiles_exactly_when_format_cannot_fail_and_writes_the_same() {
    let values: Vec<Timestamp> = pairs().into_iter().map(|(ours, _)| ours).collect();
    let civil: Vec<CivilDateTime> = values.iter().map(|at| at.utc_datetime()).collect();
    let dates: Vec<CivilDate> = values.iter().map(|at| at.utc_date()).collect();
    let times: Vec<CivilTime> = values.iter().map(|at| at.utc_time()).collect();
    // Accepted and refused per kind: a kind with fewer components refuses
    // more, and every kind refuses some and accepts some.
    let counts = [
        assert_kind(&values),
        assert_kind(&civil),
        assert_kind(&dates),
        assert_kind(&times),
    ];
    let total = (PLATFORM_FORMATS.len() + SPECIFIERS.len() + EDGES.len()) as u32;
    for (accepted, refused) in counts {
        assert_eq!(accepted + refused, total);
        assert!(accepted >= 10 && refused >= 15, "{counts:?}");
    }
    assert!(
        counts[0].0 > counts[1].0 && counts[1].0 > counts[2].0,
        "{counts:?}"
    );
}

#[test]
fn platform_patterns_write_what_chrono_wrote() {
    const MINUTE_UTC: Pattern<Timestamp> = date_pattern!(Timestamp, "%Y-%m-%d %H:%M UTC");
    const SECOND_UTC: Pattern<Timestamp> = date_pattern!(Timestamp, "%Y-%m-%d %H:%M:%S UTC");
    const FILE_STAMP: Pattern<Timestamp> = date_pattern!(Timestamp, "%Y%m%d_%H%M%S");
    const QUERY_DATETIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";
    const QUERY: Pattern<Timestamp> = date_pattern!(Timestamp, QUERY_DATETIME_FORMAT);
    const YEAR: Pattern<CivilDate> = date_pattern!(CivilDate, "%Y");
    const MONTH: Pattern<CivilDate> = date_pattern!(CivilDate, "%Y-%m");
    const WEEKDAY: Pattern<CivilDate> = date_pattern!(CivilDate, "%A");
    const CLOCK: Pattern<CivilTime> = date_pattern!(CivilTime, "%H:%M");
    const WALL_CLOCK: Pattern<CivilDateTime> = date_pattern!(CivilDateTime, "%Y-%m-%d %H:%M");

    let mut compared = 0;
    for (ours, theirs) in pairs() {
        let instant: [(&Pattern<Timestamp>, &str); 4] = [
            (&MINUTE_UTC, "%Y-%m-%d %H:%M UTC"),
            (&SECOND_UTC, "%Y-%m-%d %H:%M:%S UTC"),
            (&FILE_STAMP, "%Y%m%d_%H%M%S"),
            (&QUERY, QUERY_DATETIME_FORMAT),
        ];
        for (pattern, text) in instant {
            assert_eq!(
                ours.format_with(pattern),
                theirs.format(text).to_string(),
                "{text}"
            );
            compared += 1;
        }
        let date: NaiveDate = theirs.date_naive();
        for (pattern, text) in [(&YEAR, "%Y"), (&MONTH, "%Y-%m"), (&WEEKDAY, "%A")] {
            assert_eq!(
                ours.utc_date().format_with(pattern),
                date.format(text).to_string()
            );
            compared += 1;
        }
        let time: NaiveTime = theirs.time();
        assert_eq!(
            ours.utc_time().format_with(&CLOCK),
            time.format("%H:%M").to_string()
        );
        let wall: NaiveDateTime = theirs.naive_utc();
        assert_eq!(
            ours.utc_datetime().format_with(&WALL_CLOCK),
            wall.format("%Y-%m-%d %H:%M").to_string()
        );
        compared += 2;
    }
    assert_eq!(compared, 16 * 9);
}

#[test]
fn every_written_specifier_matches_chrono_through_a_pattern() {
    const DATE_PARTS: Pattern<Timestamp> = date_pattern!(
        Timestamp,
        "%Y %-Y %y %m %_m %d %-d %e %0e %j %u %w %a %A %b %h %B"
    );
    const TIME_PARTS: Pattern<Timestamp> =
        date_pattern!(Timestamp, "%H %k %I %l %M %S %f %.f %.3f %.6f %.9f %p %P");
    const OFFSET_PARTS: Pattern<Timestamp> = date_pattern!(Timestamp, "%z %:z %Z %s %0s %_s%n%t%%");
    const SHORTHANDS: Pattern<Timestamp> = date_pattern!(Timestamp, "%F %T %R %D");
    for (ours, theirs) in pairs() {
        for pattern in [&DATE_PARTS, &TIME_PARTS, &OFFSET_PARTS, &SHORTHANDS] {
            assert_eq!(
                ours.format_with(pattern),
                theirs.format(pattern.as_str()).to_string(),
                "{pattern:?} at {theirs:?}"
            );
        }
    }
}

#[cfg(feature = "zones")]
#[test]
fn a_zone_writes_a_pattern_as_chrono_tz_did() {
    use chrono::{DateTime, TimeZone};
    use chrono_tz::Tz;
    use dravr_tronc::dates::Zone;

    const WALL_MINUTE: Pattern<Timestamp> = date_pattern!(Timestamp, "%Y-%m-%d %H:%M");
    const ZONED_RFC3339: Pattern<Timestamp> = date_pattern!(Timestamp, "%Y-%m-%dT%H:%M:%S%.f%:z");
    const WITH_NAME: Pattern<Timestamp> = date_pattern!(Timestamp, "%a %d %b %Y %H:%M:%S %z %Z %s");
    let mut compared = 0;
    for name in [
        "America/Toronto",
        "Asia/Kolkata",
        "Australia/Lord_Howe",
        "UTC",
    ] {
        let zone = Zone::from_name(name).unwrap();
        let tz: Tz = name.parse().unwrap();
        for (ours, theirs) in pairs() {
            // chrono-tz's 2025b and the compiled-in database differ on a few
            // zones before 1970; the zones here are ones they agree on.
            if theirs.timestamp() < 0 {
                continue;
            }
            let local: DateTime<Tz> = tz.from_utc_datetime(&theirs.naive_utc());
            for pattern in [&WALL_MINUTE, &ZONED_RFC3339, &WITH_NAME] {
                assert_eq!(
                    zone.format_with(ours, pattern),
                    local.format(pattern.as_str()).to_string(),
                    "{name} {pattern:?}"
                );
                assert_eq!(
                    Ok(zone.format_with(ours, pattern)),
                    zone.format(ours, pattern.as_str())
                );
                compared += 1;
            }
        }
    }
    assert!(compared >= 4 * 9 * 3, "{compared}");
    let fixed = Zone::fixed(5 * 3600 + 45 * 60).unwrap();
    let at = Timestamp::parse_from_rfc3339("2026-11-17T10:00:05.25Z").unwrap();
    assert_eq!(
        fixed.format_with(at, &ZONED_RFC3339),
        "2026-11-17T15:45:05.250+05:45"
    );
}

#[test]
fn a_refusal_names_the_specifier() {
    let refused = |text: &'static str| Pattern::<CivilDate>::new(text).map(|_| ());
    assert_eq!(refused("%Y %H"), Err(PatternError::Unsupported("%H")));
    assert_eq!(refused("%F %T"), Err(PatternError::Unsupported("%T")));
    assert_eq!(refused("%Y %k"), Err(PatternError::Unsupported("%k")));
    assert_eq!(refused("%C"), Err(PatternError::Unsupported("%C")));
    assert_eq!(refused("%Y%#z"), Err(PatternError::Unsupported("%#z")));
    assert_eq!(refused("%q"), Err(PatternError::Unsupported("%q")));
    assert_eq!(refused("%.4f"), Err(PatternError::Unsupported("%.4")));
    assert_eq!(refused("%é"), Err(PatternError::Unsupported("%é")));
    assert_eq!(refused("%Y%"), Err(PatternError::Unsupported("%")));
    assert_eq!(refused("%Y %z"), Err(PatternError::Unsupported("%z")));
    assert_eq!(
        Pattern::<CivilDateTime>::new("%F %Z").map(|_| ()),
        Err(PatternError::Unsupported("%Z"))
    );
    assert_eq!(
        Pattern::<CivilTime>::new("%H %s").map(|_| ()),
        Err(PatternError::Unsupported("%s"))
    );
    // An unknown specifier is named as format names it.
    for text in ["%q", "%.4f", "%é", "%:x", "%-a", "%"] {
        let PatternError::Unsupported(spec) = Pattern::<Timestamp>::new(text).unwrap_err() else {
            panic!("{text:?}")
        };
        assert_eq!(
            Timestamp::UNIX_EPOCH.format(text),
            Err(DateError::UnsupportedFormat(spec.to_owned())),
            "{text:?}"
        );
    }
    assert_eq!(
        PatternError::Unsupported("%H").to_string(),
        DateError::UnsupportedFormat("%H".to_owned()).to_string()
    );
}

#[test]
fn a_pattern_holds_a_bounded_number_of_parts() {
    // Each `%d` is one part and each `-` another.
    let at_limit: &'static str =
        "%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-";
    let past_limit: &'static str =
        "%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d-%d";
    assert_eq!(MAX_PATTERN_PARTS, 48);
    assert!(Pattern::<CivilDate>::new(at_limit).is_ok());
    assert_eq!(
        Pattern::<CivilDate>::new(past_limit).map(|_| ()),
        Err(PatternError::TooLong)
    );
    // A shorthand counts every part it stands for.
    assert!(Pattern::<Timestamp>::new("%F %T").is_ok());
    assert_eq!(
        Pattern::<Timestamp>::new("%F %T %F %T %F %T %F %T %F").map(|_| ()),
        Err(PatternError::TooLong)
    );
}

#[test]
fn patterns_compare_by_their_text() {
    const A: Pattern<CivilDate> = date_pattern!(CivilDate, "%Y-%m-%d");
    let b = Pattern::<CivilDate>::new("%Y-%m-%d").unwrap();
    assert_eq!(A, b);
    assert_ne!(A, date_pattern!(CivilDate, "%F"));
    assert_eq!(format!("{A:?}"), "Pattern(\"%Y-%m-%d\")");
    let date = CivilDate::from_ymd(2026, 11, 17).unwrap();
    assert_eq!(
        date.format_with(&A),
        date.format_with(&date_pattern!(CivilDate, "%F"))
    );
}
