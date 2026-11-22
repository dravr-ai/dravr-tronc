// ABOUTME: Goldens for reading a timestamp with the offset it was written in, and an offset alone
// ABOUTME: The platform's recorded chrono verdicts as tables, and every row and generated input against chrono itself
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! `Timestamp::parse_with_offset`, `Timestamp::parse_from_str_with_offset`
//! and `Offset` (carnet#1620).
//!
//! The tables are the platform's goldens for the code that read a
//! `DateTime<FixedOffset>` and a `FixedOffset` with chrono: JSON into a value
//! that keeps its offset (the shared inbound list and Terra's payloads), the
//! text PostgreSQL renders a `TIMESTAMPTZ` in (`%#z`), and an offset read
//! alone (WHOOP). Each row is checked twice: against the verdict recorded
//! from chrono, and against chrono now, error message included.

use chrono::{DateTime, FixedOffset};
use dravr_tronc::dates::{DateError, Offset, Timestamp};

use crate::dates_parse_test::{
    as_chrono, in_range, of_ours, shown, Moment, DATES, FRACTIONS, OFFSETS, SEPARATORS, TIMES,
};

const REJECTED: &str = "rejected";

/// The PostgreSQL `TIMESTAMPTZ` text pattern the platform reads.
const POSTGRES_PATTERN: &str = "%Y-%m-%d %H:%M:%S%.f%#z";

/// A verdict as the platform's goldens write it: the instant in UTC to the
/// nanosecond and the offset kept, as seconds east.
fn verdict(read: Result<(Timestamp, Offset), DateError>) -> String {
    read.map_or_else(
        |_| REJECTED.to_owned(),
        |(at, offset)| {
            format!(
                "{} @ {}",
                at.format("%Y-%m-%dT%H:%M:%S%.9fZ").unwrap(),
                offset.seconds()
            )
        },
    )
}

/// What ours must give for what chrono gave: the instant and offset, or
/// chrono's message word for word.
fn chrono_kept(
    theirs: Result<DateTime<FixedOffset>, chrono::ParseError>,
) -> Result<(Moment, i32), String> {
    as_chrono(theirs, |value| {
        in_range(value.to_utc()).map(|moment| (moment, value.offset().local_minus_utc()))
    })
}

fn ours_kept(read: Result<(Timestamp, Offset), DateError>) -> Result<(Moment, i32), String> {
    shown(read.map(|(at, offset)| (of_ours(at), offset.seconds())))
}

fn assert_table(name: &str, read: fn(&str) -> String, table: &[(&str, &str)]) {
    let actual: Vec<(&str, String)> = table
        .iter()
        .map(|(input, _)| (*input, read(input)))
        .collect();
    let wanted: Vec<(&str, String)> = table
        .iter()
        .map(|(input, verdict)| (*input, (*verdict).to_owned()))
        .collect();
    assert_eq!(actual, wanted, "{name} changed a verdict");
}

// JSON_KEEPS_OFFSET: 54
const JSON_KEEPS_OFFSET: [(&str, &str); 54] = [
    ("2026-11-17T10:00:05Z", "2026-11-17T10:00:05.000000000Z @ 0"),
    (
        "2026-11-17T10:00:05+00:00",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17T05:00:05-05:00",
        "2026-11-17T10:00:05.000000000Z @ -18000",
    ),
    (
        "2026-11-17T15:30:05+05:30",
        "2026-11-17T10:00:05.000000000Z @ 19800",
    ),
    (
        "2026-11-17T10:00:05-00:00",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05+0000",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17T05:00:05-0500",
        "2026-11-17T10:00:05.000000000Z @ -18000",
    ),
    ("2026-11-17T10:00:05+00", "rejected"),
    (
        "2026-11-17T10:00:05 +00:00",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    ("2026-11-17t10:00:05z", "2026-11-17T10:00:05.000000000Z @ 0"),
    ("2026-11-17T10:00:05z", "2026-11-17T10:00:05.000000000Z @ 0"),
    ("2026-11-17t10:00:05Z", "2026-11-17T10:00:05.000000000Z @ 0"),
    ("2026-11-17 10:00:05Z", "2026-11-17T10:00:05.000000000Z @ 0"),
    (
        "2026-11-17 10:00:05+00:00",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17  10:00:05Z",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    ("2026-11-17_10:00:05Z", "rejected"),
    (
        "2026-11-17 10:00:05 UTC",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05 UTC",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05UTC",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05 utc",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    ("2026-11-17T10:00:05 GMT", "rejected"),
    ("2026-11-17T05:00:05 EST", "rejected"),
    ("2026-1-7T9:0:5Z", "2026-01-07T09:00:05.000000000Z @ 0"),
    ("2026-11-17T9:00:05Z", "2026-11-17T09:00:05.000000000Z @ 0"),
    ("2026-11-17T10:00:05", "rejected"),
    ("2026-11-17 10:00:05", "rejected"),
    ("2026-11-17T10:00Z", "rejected"),
    ("2026-11-17", "rejected"),
    ("2026-11-17Z", "rejected"),
    ("", "rejected"),
    (
        "2026-11-17T10:00:05.1Z",
        "2026-11-17T10:00:05.100000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.12Z",
        "2026-11-17T10:00:05.120000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.123Z",
        "2026-11-17T10:00:05.123000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.1234Z",
        "2026-11-17T10:00:05.123400000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.12345Z",
        "2026-11-17T10:00:05.123450000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.123456Z",
        "2026-11-17T10:00:05.123456000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.1234567Z",
        "2026-11-17T10:00:05.123456700Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.12345678Z",
        "2026-11-17T10:00:05.123456780Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.123456789Z",
        "2026-11-17T10:00:05.123456789Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.1234567891Z",
        "2026-11-17T10:00:05.123456789Z @ 0",
    ),
    (
        "2026-11-17T10:00:05.100+00:00",
        "2026-11-17T10:00:05.100000000Z @ 0",
    ),
    ("2026-11-17T10:00:05,123Z", "rejected"),
    ("2026-11-17T10:00:05.Z", "rejected"),
    // chrono kept second 60; the date types read it as :59 (carnet#1620, JF 2026-11-18).
    ("2016-12-31T23:59:60Z", "2016-12-31T23:59:59.000000000Z @ 0"),
    // chrono kept second 60; the date types read it as :59 (carnet#1620, JF 2026-11-18).
    ("2026-11-17T10:00:60Z", "2026-11-17T10:00:59.000000000Z @ 0"),
    ("2026-11-17T24:00:00Z", "rejected"),
    ("2026-02-30T10:00:05Z", "rejected"),
    ("2025-02-29T10:00:05Z", "rejected"),
    ("2026-13-01T10:00:05Z", "rejected"),
    (
        " 2026-11-17T10:00:05Z",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    (
        "2026-11-17T10:00:05Z ",
        "2026-11-17T10:00:05.000000000Z @ 0",
    ),
    ("20261117T100005Z", "rejected"),
    ("Tue, 17 Nov 2026 10:00:05 GMT", "rejected"),
    ("1794909605", "rejected"),
];

// OFFSET_PATTERN: 28
const OFFSET_PATTERN: [(&str, &str); 28] = [
    (
        "2026-11-13 10:04:05+00",
        "2026-11-13T10:04:05.000000000Z @ 0",
    ),
    (
        "2026-11-13 05:04:05-05",
        "2026-11-13T10:04:05.000000000Z @ -18000",
    ),
    (
        "2026-11-13 15:34:05+05:30",
        "2026-11-13T10:04:05.000000000Z @ 19800",
    ),
    (
        "2026-11-13 10:04:05.250000+00",
        "2026-11-13T10:04:05.250000000Z @ 0",
    ),
    (
        "2026-11-13 10:04:05.25+00",
        "2026-11-13T10:04:05.250000000Z @ 0",
    ),
    (
        "2026-11-13 10:04:05.123456789+00",
        "2026-11-13T10:04:05.123456789Z @ 0",
    ),
    (
        "2026-11-13 10:04:05.1234567891+00",
        "2026-11-13T10:04:05.123456789Z @ 0",
    ),
    (
        "2026-11-13 10:04:05+0000",
        "2026-11-13T10:04:05.000000000Z @ 0",
    ),
    (
        "2026-11-13 10:04:05+00:00",
        "2026-11-13T10:04:05.000000000Z @ 0",
    ),
    (
        "2026-11-13 10:04:05-00",
        "2026-11-13T10:04:05.000000000Z @ 0",
    ),
    ("2026-11-13 10:04:05Z", "2026-11-13T10:04:05.000000000Z @ 0"),
    ("2026-11-13 10:04:05z", "2026-11-13T10:04:05.000000000Z @ 0"),
    (
        "2026-11-13 10:04:05 +00",
        "2026-11-13T10:04:05.000000000Z @ 0",
    ),
    ("2026-11-13 10:04:05", "rejected"),
    ("2026-11-13T10:04:05+00", "rejected"),
    ("2026-11-13 10:04+00", "rejected"),
    ("2026-11-13 10:04:05+5", "rejected"),
    ("2026-11-13 10:04:05+05:3", "rejected"),
    // chrono kept second 60; the date types read it as :59 (carnet#1620, JF 2026-11-18).
    (
        "2026-11-13 10:04:60+00",
        "2026-11-13T10:04:59.000000000Z @ 0",
    ),
    ("2026-11-13 24:00:00+00", "rejected"),
    ("2026-11-13 10:04:05,250+00", "rejected"),
    (
        " 2026-11-13 10:04:05+00",
        "2026-11-13T10:04:05.000000000Z @ 0",
    ),
    (
        "2026-11-13 10:04:05+00 ",
        "2026-11-13T10:04:05.000000000Z @ 0",
    ),
    ("2026-11-13 10:04:05 UTC", "rejected"),
    ("2026-1-3 9:4:5+00", "2026-01-03T09:04:05.000000000Z @ 0"),
    ("2026-02-30 10:04:05+00", "rejected"),
    ("", "rejected"),
    ("yesterday", "rejected"),
];

// OFFSET_ALONE: 24
const OFFSET_ALONE: [(&str, &str); 24] = [
    ("+00:00", "0"),
    ("-00:00", "0"),
    ("-05:00", "-18000"),
    ("+05:30", "19800"),
    ("+05:45", "20700"),
    ("+14:00", "50400"),
    ("-12:00", "-43200"),
    ("+23:59", "86340"),
    ("-23:59", "-86340"),
    ("+24:00", "rejected"),
    ("+0530", "19800"),
    ("+05", "rejected"),
    ("+5:30", "rejected"),
    ("+05:3", "rejected"),
    ("05:30", "rejected"),
    ("+05:30:15", "19800"),
    ("Z", "rejected"),
    ("z", "rejected"),
    ("UTC", "rejected"),
    (" +05:30", "rejected"),
    ("+05:30 ", "19800"),
    ("+05 30", "19800"),
    ("−05:00", "-18000"),
    ("", "rejected"),
];

// TERRA: 14
const TERRA: [(&str, &str); 14] = [
    (
        "2024-01-15T08:00:00-05:00",
        "2024-01-15T13:00:00.000000000Z @ -18000",
    ),
    (
        "2024-01-15T08:00:00.000000-05:00",
        "2024-01-15T13:00:00.000000000Z @ -18000",
    ),
    (
        "2024-01-15T08:00:00.120-05:00",
        "2024-01-15T13:00:00.120000000Z @ -18000",
    ),
    (
        "2024-01-15T18:30:00+05:30",
        "2024-01-15T13:00:00.000000000Z @ 19800",
    ),
    ("2024-01-15T13:00:00Z", "2024-01-15T13:00:00.000000000Z @ 0"),
    (
        "2024-01-15T13:00:00+00:00",
        "2024-01-15T13:00:00.000000000Z @ 0",
    ),
    (
        "2024-01-15T13:00:00-00:00",
        "2024-01-15T13:00:00.000000000Z @ 0",
    ),
    (
        "2024-01-15T08:00:00-0500",
        "2024-01-15T13:00:00.000000000Z @ -18000",
    ),
    (
        "2024-01-15 08:00:00-05:00",
        "2024-01-15T13:00:00.000000000Z @ -18000",
    ),
    (
        "2024-01-15T13:00:00 UTC",
        "2024-01-15T13:00:00.000000000Z @ 0",
    ),
    ("2024-01-15T08:00:00-05", "rejected"),
    ("2024-01-15T08:00:00", "rejected"),
    ("2024-01-15", "rejected"),
    ("", "rejected"),
];

#[test]
fn a_relaxed_timestamp_keeps_the_offset_the_platform_recorded() {
    assert_table(
        "parse_with_offset",
        |text| verdict(Timestamp::parse_with_offset(text)),
        &JSON_KEEPS_OFFSET,
    );
    assert_table(
        "parse_with_offset (Terra)",
        |text| verdict(Timestamp::parse_with_offset(text)),
        &TERRA,
    );
}

#[test]
fn a_relaxed_timestamp_keeps_the_offset_chrono_kept() {
    for (text, _) in JSON_KEEPS_OFFSET.iter().chain(TERRA.iter()) {
        let theirs = chrono_kept(text.parse::<DateTime<FixedOffset>>());
        assert_eq!(
            ours_kept(Timestamp::parse_with_offset(text)),
            theirs,
            "{text:?}"
        );
        // serde read a DateTime<FixedOffset> field through the same parser.
        let quoted = serde_json::Value::String((*text).to_owned()).to_string();
        let through_serde = serde_json::from_str::<DateTime<FixedOffset>>(&quoted);
        assert_eq!(theirs.is_ok(), through_serde.is_ok(), "serde {text:?}");
        // The instant is the one FromStr and serde give a Timestamp.
        assert_eq!(
            shown(Timestamp::parse_with_offset(text).map(|(at, _)| at)),
            shown(text.parse::<Timestamp>()),
            "{text:?}"
        );
    }
}

#[test]
fn a_relaxed_timestamp_keeps_the_offset_chrono_kept_across_generated_inputs() {
    let (mut accepted, mut rejected, mut nonzero) = (0_u32, 0_u32, 0_u32);
    let mut input = String::new();
    for date in DATES {
        for separator in SEPARATORS {
            for time in TIMES {
                for fraction in FRACTIONS {
                    for offset in OFFSETS {
                        input.clear();
                        for part in [date, separator, time, fraction, offset] {
                            input.push_str(part);
                        }
                        let theirs = chrono_kept(input.parse::<DateTime<FixedOffset>>());
                        assert_eq!(
                            ours_kept(Timestamp::parse_with_offset(&input)),
                            theirs,
                            "{input:?}"
                        );
                        match theirs {
                            Ok((_, 0)) => accepted += 1,
                            Ok(_) => {
                                accepted += 1;
                                nonzero += 1;
                            }
                            Err(_) => rejected += 1,
                        }
                    }
                }
            }
        }
    }
    assert_eq!(accepted + rejected, 17 * 6 * 11 * 11 * 26);
    assert!(accepted > 5_000, "accepted {accepted}");
    assert!(nonzero > 2_000, "kept a non-zero offset {nonzero}");
    assert!(rejected > 200_000, "rejected {rejected}");
}

#[test]
fn the_postgres_pattern_keeps_the_offset_the_platform_recorded() {
    assert_table(
        POSTGRES_PATTERN,
        |text| {
            verdict(Timestamp::parse_from_str_with_offset(
                text,
                POSTGRES_PATTERN,
            ))
        },
        &OFFSET_PATTERN,
    );
}

#[test]
fn an_offset_pattern_keeps_the_offset_chrono_kept() {
    for (text, _) in OFFSET_PATTERN {
        assert_eq!(
            ours_kept(Timestamp::parse_from_str_with_offset(
                text,
                POSTGRES_PATTERN
            )),
            chrono_kept(DateTime::parse_from_str(text, POSTGRES_PATTERN)),
            "{text:?}"
        );
    }
    let formats = [
        POSTGRES_PATTERN,
        "%Y-%m-%d %H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S %#z",
        "%Y-%m-%d %H:%M%#z",
    ];
    let (mut accepted, mut rejected) = (0_u32, 0_u32);
    for format in formats {
        for clock in [
            "2026-11-13 10:04:05",
            "2026-11-13T10:04:05",
            "2026-11-13 10:04",
            "2026-1-3 9:4:5",
        ] {
            for fraction in FRACTIONS {
                for offset in OFFSETS
                    .iter()
                    .chain(&["+05", "-05", "+5", " -00", "+12345", "−0800"])
                {
                    let text = format!("{clock}{fraction}{offset}");
                    let theirs = chrono_kept(DateTime::parse_from_str(&text, format));
                    assert_eq!(
                        ours_kept(Timestamp::parse_from_str_with_offset(&text, format)),
                        theirs,
                        "{text:?} as {format:?}"
                    );
                    if theirs.is_ok() {
                        accepted += 1;
                    } else {
                        rejected += 1;
                    }
                }
            }
        }
    }
    assert!(accepted > 150, "accepted {accepted}");
    assert!(rejected > 1_500, "rejected {rejected}");
}

#[test]
fn a_pattern_without_an_offset_is_not_enough() {
    assert_eq!(
        Timestamp::parse_from_str_with_offset("2026-11-13 10:04:05", "%Y-%m-%d %H:%M:%S"),
        Err(DateError::NotEnough)
    );
    assert_eq!(
        DateTime::parse_from_str("2026-11-13 10:04:05", "%Y-%m-%d %H:%M:%S")
            .unwrap_err()
            .to_string(),
        DateError::NotEnough.to_string()
    );
}

fn offset_alone(text: &str) -> String {
    text.parse::<Offset>().map_or_else(
        |_| REJECTED.to_owned(),
        |offset| offset.seconds().to_string(),
    )
}

fn chrono_offset(text: &str) -> Result<i32, String> {
    text.parse::<FixedOffset>()
        .map(|offset| offset.local_minus_utc())
        .map_err(|error| error.to_string())
}

#[test]
fn an_offset_alone_reads_what_the_platform_recorded() {
    assert_table("Offset::from_str", offset_alone, &OFFSET_ALONE);
    for (text, _) in OFFSET_ALONE {
        assert_eq!(
            shown(text.parse::<Offset>().map(Offset::seconds)),
            chrono_offset(text),
            "{text:?}"
        );
    }
}

#[test]
fn an_offset_alone_reads_like_chrono_across_generated_inputs() {
    let signs = ["+", "-", "\u{2212}", "", " +", "Z", "z"];
    let hours = ["00", "05", "5", "14", "23", "24", "99", "0a", ""];
    let separators = ["", ":", " ", "::", ": ", "\t", "-"];
    let minutes = ["", "00", "30", "3", "59", "60", "99", "x0"];
    let tails = ["", " ", ":15", "x", "Z", "00"];
    let (mut accepted, mut rejected) = (0_u32, 0_u32);
    let mut text = String::new();
    for sign in signs {
        for hour in hours {
            for separator in separators {
                for minute in minutes {
                    for tail in tails {
                        text.clear();
                        for part in [sign, hour, separator, minute, tail] {
                            text.push_str(part);
                        }
                        let theirs = chrono_offset(&text);
                        assert_eq!(
                            shown(text.parse::<Offset>().map(Offset::seconds)),
                            theirs,
                            "{text:?}"
                        );
                        if theirs.is_ok() {
                            accepted += 1;
                        } else {
                            rejected += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(accepted + rejected, 7 * 9 * 7 * 8 * 6);
    assert!(accepted > 300, "accepted {accepted}");
    assert!(rejected > 10_000, "rejected {rejected}");
}

#[test]
fn an_offset_has_chrono_s_range_and_text() {
    let mut seconds = -86_400;
    while seconds <= 86_400 {
        for nudge in [-1, 0, 1] {
            let value = seconds + nudge;
            let theirs = FixedOffset::east_opt(value);
            let ours = Offset::from_seconds(value);
            assert_eq!(ours.is_ok(), theirs.is_some(), "{value}");
            if let (Ok(ours), Some(theirs)) = (ours, theirs) {
                assert_eq!(ours.seconds(), theirs.local_minus_utc());
                assert_eq!(ours.to_string(), theirs.to_string(), "{value}");
                assert_eq!(format!("{ours:?}"), format!("{theirs:?}"), "{value}");
                assert_eq!(
                    ours.to_string().parse::<Offset>().map(Offset::seconds),
                    Ok(value - value % 60)
                );
            } else {
                assert_eq!(Offset::from_seconds(value), Err(DateError::OutOfRange));
            }
        }
        seconds += 1_234;
    }
    assert_eq!(Offset::UTC.seconds(), 0);
    assert_eq!(Offset::default(), Offset::UTC);
    assert_eq!(Offset::UTC.to_string(), "+00:00");
    assert!(Offset::from_seconds(-3_600).unwrap() < Offset::UTC);
}
