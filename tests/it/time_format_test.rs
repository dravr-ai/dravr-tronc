// ABOUTME: Output goldens for the date types: literal expected bytes, and the same value through chrono
// ABOUTME: Pins RFC 3339 forms, serde JSON, Display and Debug, and every supported strftime specifier
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat as ChronoSeconds, Utc};
use dravr_tronc::time::{
    CivilDate, CivilDateTime, CivilTime, Elapsed, Instant, SecondsFormat, TimeError, Weekday,
};
use serde::{Deserialize, Serialize};

use crate::time_support::{pair, pairs};

/// The serde form and the `to_rfc3339()` form of each value, written out.
const GOLDEN: [(i64, u32, &str, &str); 12] = [
    (
        1_794_909_600,
        0,
        "2026-11-17T10:00:00Z",
        "2026-11-17T10:00:00+00:00",
    ),
    (
        1_794_909_600,
        100_000_000,
        "2026-11-17T10:00:00.100Z",
        "2026-11-17T10:00:00.100+00:00",
    ),
    (
        1_794_909_600,
        120_000_000,
        "2026-11-17T10:00:00.120Z",
        "2026-11-17T10:00:00.120+00:00",
    ),
    (
        1_794_909_600,
        123_000_000,
        "2026-11-17T10:00:00.123Z",
        "2026-11-17T10:00:00.123+00:00",
    ),
    (
        1_794_909_600,
        123_400_000,
        "2026-11-17T10:00:00.123400Z",
        "2026-11-17T10:00:00.123400+00:00",
    ),
    (
        1_794_909_600,
        123_456_000,
        "2026-11-17T10:00:00.123456Z",
        "2026-11-17T10:00:00.123456+00:00",
    ),
    (
        1_794_909_600,
        123_456_789,
        "2026-11-17T10:00:00.123456789Z",
        "2026-11-17T10:00:00.123456789+00:00",
    ),
    (
        1_794_909_600,
        1,
        "2026-11-17T10:00:00.000000001Z",
        "2026-11-17T10:00:00.000000001+00:00",
    ),
    (
        -14_182_940,
        123_000_000,
        "1969-07-20T20:17:40.123Z",
        "1969-07-20T20:17:40.123+00:00",
    ),
    (
        1_709_251_199,
        999_000_000,
        "2024-02-29T23:59:59.999Z",
        "2024-02-29T23:59:59.999+00:00",
    ),
    (
        -1,
        500_000_000,
        "1969-12-31T23:59:59.500Z",
        "1969-12-31T23:59:59.500+00:00",
    ),
    (
        -62_135_596_800,
        0,
        "0001-01-01T00:00:00Z",
        "0001-01-01T00:00:00+00:00",
    ),
];

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn serde_and_to_rfc3339_write_the_recorded_bytes() {
    for (seconds, nanos, wire, stored) in GOLDEN {
        let (ours, theirs) = pair(seconds, nanos);
        assert_eq!(json(&ours), format!("\"{wire}\""));
        assert_eq!(json(&theirs), format!("\"{wire}\""), "chrono, {wire}");
        assert_eq!(ours.to_rfc3339(), stored);
        assert_eq!(theirs.to_rfc3339(), stored, "chrono, {stored}");
    }
}

#[test]
fn to_rfc3339_opts_writes_each_fixed_width_form() {
    let (ours, _) = pair(1_794_909_600, 123_456_789);
    let cases = [
        (SecondsFormat::Secs, true, "2026-11-17T10:00:00Z"),
        (SecondsFormat::Millis, true, "2026-11-17T10:00:00.123Z"),
        (SecondsFormat::Micros, true, "2026-11-17T10:00:00.123456Z"),
        (SecondsFormat::Nanos, true, "2026-11-17T10:00:00.123456789Z"),
        (
            SecondsFormat::AutoSi,
            true,
            "2026-11-17T10:00:00.123456789Z",
        ),
        (SecondsFormat::Secs, false, "2026-11-17T10:00:00+00:00"),
        (
            SecondsFormat::Millis,
            false,
            "2026-11-17T10:00:00.123+00:00",
        ),
    ];
    for (format, use_z, expected) in cases {
        assert_eq!(ours.to_rfc3339_opts(format, use_z), expected);
    }
    // A whole second still gets its zeros in a fixed-width form.
    let (whole, _) = pair(1_794_909_600, 0);
    assert_eq!(
        whole.to_rfc3339_opts(SecondsFormat::Millis, true),
        "2026-11-17T10:00:00.000Z"
    );
    assert_eq!(
        whole.to_rfc3339_opts(SecondsFormat::Micros, true),
        "2026-11-17T10:00:00.000000Z"
    );
}

#[test]
fn to_rfc3339_opts_matches_chrono_for_every_value_and_option() {
    let formats = [
        (SecondsFormat::Secs, ChronoSeconds::Secs),
        (SecondsFormat::Millis, ChronoSeconds::Millis),
        (SecondsFormat::Micros, ChronoSeconds::Micros),
        (SecondsFormat::Nanos, ChronoSeconds::Nanos),
        (SecondsFormat::AutoSi, ChronoSeconds::AutoSi),
    ];
    let mut compared = 0;
    for (ours, theirs) in pairs() {
        for (mine, chronos) in formats {
            for use_z in [true, false] {
                assert_eq!(
                    ours.to_rfc3339_opts(mine, use_z),
                    theirs.to_rfc3339_opts(chronos, use_z)
                );
                compared += 1;
            }
        }
    }
    assert_eq!(compared, 160);
}

#[test]
fn display_and_debug_match_chrono_for_every_value() {
    for (ours, theirs) in pairs() {
        assert_eq!(ours.to_string(), theirs.to_string());
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
        assert_eq!(json(&ours), json(&theirs));

        let (date, time, both) = (ours.utc_date(), ours.utc_time(), ours.utc_datetime());
        let naive = theirs.naive_utc();
        assert_eq!(date.to_string(), naive.date().to_string());
        assert_eq!(format!("{date:?}"), format!("{:?}", naive.date()));
        assert_eq!(json(&date), json(&naive.date()));
        assert_eq!(time.to_string(), naive.time().to_string());
        assert_eq!(format!("{time:?}"), format!("{:?}", naive.time()));
        assert_eq!(json(&time), json(&naive.time()));
        assert_eq!(both.to_string(), naive.to_string());
        assert_eq!(format!("{both:?}"), format!("{naive:?}"));
        assert_eq!(json(&both), json(&naive));
    }
    let (ours, _) = pair(1_794_909_600, 100_000_000);
    assert_eq!(ours.to_string(), "2026-11-17 10:00:00.100 UTC");
    assert_eq!(format!("{ours:?}"), "2026-11-17T10:00:00.100Z");
}

#[test]
fn each_wire_type_serializes_to_its_recorded_json() {
    let (instant, _) = pair(1_794_909_600, 100_000_000);
    assert_eq!(json(&instant), r#""2026-11-17T10:00:00.100Z""#);
    assert_eq!(json(&Some(instant)), r#""2026-11-17T10:00:00.100Z""#);
    assert_eq!(json(&None::<Instant>), "null");
    assert_eq!(json(&instant.utc_date()), r#""2026-11-17""#);
    assert_eq!(json(&instant.utc_time()), r#""10:00:00.100""#);
    assert_eq!(
        json(&CivilTime::from_hms(7, 5, 0).unwrap()),
        r#""07:05:00""#
    );
    assert_eq!(
        json(&instant.utc_datetime()),
        r#""2026-11-17T10:00:00.100""#
    );
    assert_eq!(json(&instant.utc_date().weekday()), r#""Tue""#);
    assert_eq!(
        json(&Weekday::ALL),
        r#"["Mon","Tue","Wed","Thu","Fri","Sat","Sun"]"#
    );
    assert_eq!(
        json(&CivilDate::from_ymd(-44, 3, 15).unwrap()),
        r#""-0044-03-15""#
    );
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Ours {
    created_at: Instant,
    ended_at: Option<Instant>,
    day: CivilDate,
    wake: CivilTime,
    local: CivilDateTime,
    weekday: Weekday,
    history: Vec<Instant>,
}

#[derive(Serialize, Deserialize)]
struct Theirs {
    created_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    day: NaiveDate,
    wake: NaiveTime,
    local: NaiveDateTime,
    weekday: chrono::Weekday,
    history: Vec<DateTime<Utc>>,
}

#[test]
fn a_struct_of_date_fields_serializes_like_its_chrono_twin() {
    let all = pairs();
    for (index, (ours, theirs)) in all.iter().enumerate() {
        let (other_ours, other_theirs) = all[(index + 3) % all.len()];
        let mine = Ours {
            created_at: *ours,
            ended_at: (index % 2 == 0).then_some(other_ours),
            day: ours.utc_date(),
            wake: other_ours.utc_time(),
            local: ours.utc_datetime(),
            weekday: ours.utc_date().weekday(),
            history: vec![*ours, other_ours],
        };
        let twin = Theirs {
            created_at: *theirs,
            ended_at: (index % 2 == 0).then_some(other_theirs),
            day: theirs.date_naive(),
            wake: other_theirs.time(),
            local: theirs.naive_utc(),
            weekday: chrono::Datelike::weekday(theirs),
            history: vec![*theirs, other_theirs],
        };
        let document = json(&twin);
        assert_eq!(json(&mine), document);
        // A document chrono wrote reads back to the same value and the same
        // bytes: stored JSON survives the move.
        let read: Ours = serde_json::from_str(&document).unwrap();
        assert_eq!(read, mine);
        assert_eq!(json(&read), document);
    }
}

/// Every format string the platform passes to `.format(..)` today.
const PLATFORM_FORMATS: [&str; 20] = [
    "%Y-%m-%d",
    "%Y-%m-%d %H:%M UTC",
    "%Y%m%d_%H%M%S",
    "%H:%M",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M:%SZ",
    "%Y-%m-%dT%H:%M:%S%.3fZ",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%d %H:%M:%S UTC",
    "%Y",
    "%Y-%m-%d, started %H:%M UTC",
    "%Y-%m-%d %H:%M:%S +0200",
    "%b %-d, %Y, %-I:%M:%S %p",
    "%Y-%m",
    "%A",
    "%A %Y-%m-%d",
    "%A %Y-%m-%d, started %H:%M UTC",
    "%a %b %-d",
    "%a %-d %b",
    "%-d/%-m/%Y",
];

/// Every specifier the date types implement, alone and with each padding.
const SPECIFIERS: [&str; 64] = [
    "%Y", "%-Y", "%_Y", "%0Y", "%C", "%-C", "%_C", "%y", "%-y", "%_y", "%m", "%-m", "%_m", "%d",
    "%-d", "%_d", "%0d", "%e", "%-e", "%0e", "%j", "%-j", "%_j", "%H", "%-H", "%_H", "%k", "%-k",
    "%I", "%-I", "%_I", "%l", "%M", "%-M", "%_M", "%S", "%-S", "%_S", "%f", "%-f", "%_f", "%.f",
    "%.3f", "%.6f", "%.9f", "%a", "%A", "%b", "%h", "%B", "%p", "%P", "%z", "%:z", "%Z", "%s",
    "%-s", "%0s", "%_s", "%F", "%T", "%R", "%D", "%u",
];

#[test]
fn platform_format_strings_write_the_recorded_text() {
    let (ours, _) = pair(1_794_909_605, 999_999_999);
    let expected = [
        "2026-11-17",
        "2026-11-17 10:00 UTC",
        "20261117_100005",
        "10:00",
        "2026-11-17T10:00:05",
        "2026-11-17T10:00:05Z",
        "2026-11-17T10:00:05.999Z",
        "2026-11-17 10:00",
        "2026-11-17 10:00:05 UTC",
        "2026",
        "2026-11-17, started 10:00 UTC",
        "2026-11-17 10:00:05 +0200",
        "Nov 17, 2026, 10:00:05 AM",
        "2026-11",
        "Tuesday",
        "Tuesday 2026-11-17",
        "Tuesday 2026-11-17, started 10:00 UTC",
        "Tue Nov 17",
        "Tue 17 Nov",
        "17/11/2026",
    ];
    for (format, text) in PLATFORM_FORMATS.into_iter().zip(expected) {
        assert_eq!(ours.format(format).as_deref(), Ok(text), "{format}");
    }
}

#[test]
fn format_matches_chrono_for_every_specifier_and_value() {
    let (mut compared, mut skipped) = (0, 0);
    for (ours, theirs) in pairs() {
        for format in PLATFORM_FORMATS.into_iter().chain(SPECIFIERS).chain([
            "%w",
            "%n%t%%",
            "a  b\t%d  %H",
            "%d%%%m",
            "é%Yü",
        ]) {
            // The one place chrono's output is not reproduced: the century
            // of a year before 0, where it writes two meaningless characters.
            if ["%C", "%-C", "%_C"].contains(&format) && chrono::Datelike::year(&theirs) < 0 {
                assert_eq!(ours.format(format), Err(TimeError::OutOfRange));
                skipped += 1;
                continue;
            }
            assert_eq!(
                ours.format(format),
                Ok(theirs.format(format).to_string()),
                "{format} at {theirs:?}"
            );
            compared += 1;
        }
    }
    assert_eq!((compared, skipped), (16 * 89 - 3, 3));
}

#[test]
fn unix_seconds_pad_to_nine_places_only_when_asked() {
    let (small, _) = pair(0, 0);
    let (early, _) = pair(-14_182_940, 123_000_000);
    let (today, _) = pair(1_794_909_600, 0);
    for (value, plain, zeros, spaces) in [
        (small, "0", "000000000", "        0"),
        (early, "-14182940", "-14182940", "-14182940"),
        (today, "1794909600", "1794909600", "1794909600"),
    ] {
        assert_eq!(value.format("%s").as_deref(), Ok(plain));
        assert_eq!(value.format("%-s").as_deref(), Ok(plain));
        assert_eq!(value.format("%0s").as_deref(), Ok(zeros));
        assert_eq!(value.format("%_s").as_deref(), Ok(spaces));
    }
}

#[test]
fn chrono_writes_no_usable_century_before_year_zero() {
    // Recorded so the exception above is checked against chrono itself, and
    // fails if a chrono release ever changes it.
    let (_, theirs) = pair(-63_600_000_000, 0);
    assert_eq!(theirs.format("%Y %C").to_string(), "-0046 I5");
}

#[test]
fn civil_values_format_like_chrono_naive_values() {
    let date_formats = [
        "%Y-%m-%d",
        "%A %Y-%m-%d",
        "%a %-d %b",
        "%-d/%-m/%Y",
        "%j",
        "%F",
    ];
    let time_formats = ["%H:%M", "%H:%M:%S%.f", "%-I:%M %p", "%T", "%R", "%.3f"];
    for (ours, theirs) in pairs() {
        let naive = theirs.naive_utc();
        for format in date_formats {
            assert_eq!(
                ours.utc_date().format(format),
                Ok(naive.date().format(format).to_string())
            );
        }
        for format in time_formats {
            assert_eq!(
                ours.utc_time().format(format),
                Ok(naive.time().format(format).to_string())
            );
        }
        for format in date_formats.into_iter().chain(time_formats).chain(["%s"]) {
            assert_eq!(
                ours.utc_datetime().format(format),
                Ok(naive.format(format).to_string())
            );
        }
    }
}

#[test]
fn a_format_the_value_cannot_fill_is_an_error_not_a_panic() {
    let (instant, _) = pair(1_794_909_600, 0);
    let unsupported = |spec: &str| Err(TimeError::UnsupportedFormat(spec.to_owned()));
    assert_eq!(instant.format("%Y %U"), unsupported("%U"));
    assert_eq!(instant.format("%Y %"), unsupported("%"));
    assert_eq!(instant.format("%-"), unsupported("%-"));
    assert_eq!(instant.format("%-a"), unsupported("%-a"));
    assert_eq!(instant.format("%.4f"), unsupported("%.4"));
    assert_eq!(instant.format("%#z"), unsupported("%#z"));
    assert_eq!(instant.format("%é"), unsupported("%é"));
    assert_eq!(instant.utc_date().format("%H"), unsupported("%H"));
    assert_eq!(instant.utc_time().format("%Y"), unsupported("%Y"));
    assert_eq!(instant.utc_datetime().format("%z"), unsupported("%z"));
    assert_eq!(instant.utc_datetime().format("%Z"), unsupported("%Z"));
}

#[test]
fn elapsed_displays_seconds_with_the_fraction_that_is_there() {
    assert_eq!(Elapsed::seconds(90).to_string(), "90s");
    assert_eq!(Elapsed::milliseconds(-500).to_string(), "-0.5s");
    assert_eq!(
        Elapsed::nanoseconds(1_000_000_001).to_string(),
        "1.000000001s"
    );
    assert_eq!(format!("{:?}", Elapsed::ZERO), "0s");
}
