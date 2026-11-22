// ABOUTME: Input goldens for the date types: what is accepted and what it means, recorded from chrono's behaviour
// ABOUTME: A literal verdict table, and a few hundred thousand generated inputs compared with chrono itself
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt::{Debug, Display};

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use dravr_tronc::dates::{CivilDate, CivilDateTime, CivilTime, DateError, Timestamp, Weekday};

pub(super) const DATES: [&str; 17] = [
    "2026-11-17",
    "2026-1-7",
    "2026-02-30",
    "2024-02-29",
    "0001-01-01",
    "+2026-11-17",
    "-0044-03-15",
    "20261-11-17",
    "2026 - 11 - 17",
    " 2026-11-17",
    "2026-13-01",
    "2026-00-10",
    "9999-12-31",
    "+10000-01-01",
    "226-11-17",
    "2026-11-1\u{ff17}",
    "2026/11/17",
];

pub(super) const SEPARATORS: [&str; 6] = ["T", "t", " ", "  ", "_", ""];

pub(super) const TIMES: [&str; 11] = [
    "10:00:00",
    "1:2:3",
    "10:00",
    "24:00:00",
    "23:59:60",
    "23:60:00",
    "10 : 00 : 00",
    "00:00:00",
    "10:0:0",
    "100000",
    "10:00:0x",
];

pub(super) const FRACTIONS: [&str; 11] = [
    "",
    ".1",
    ".12",
    ".123",
    ".1234",
    ".123456",
    ".123456789",
    ".1234567890123",
    ".",
    ".x",
    ",5",
];

pub(super) const OFFSETS: [&str; 26] = [
    "Z",
    "z",
    "+00:00",
    "-05:00",
    "+05:30",
    "+0530",
    "+05",
    "+05:3",
    "+05:60",
    "+24:00",
    "+23:59",
    "-23:59",
    " UTC",
    "UTC",
    "utc",
    " +01:00",
    "\u{2212}05:00",
    "+05 30",
    "+05::30",
    "",
    "Z ",
    "Zx",
    "+0a:00",
    "GMT",
    "+5:00",
    "\u{e9}",
];

/// Seconds and nanoseconds since the epoch; a leap second reads as `:59`.
pub(super) type Moment = (i64, u32);

pub(super) fn of_chrono(value: DateTime<Utc>) -> Moment {
    (
        value.timestamp(),
        value.timestamp_subsec_nanos() % 1_000_000_000,
    )
}

pub(super) fn of_ours(value: Timestamp) -> Moment {
    (value.epoch_seconds(), value.subsec_nanos())
}

/// What ours must give for what chrono gave: the same timestamp, or an error
/// where chrono errs or where the timestamp is outside the range of `Timestamp`.
pub(super) fn expected(theirs: Option<DateTime<Utc>>) -> Option<Moment> {
    theirs
        .filter(|value| {
            (Timestamp::MIN.epoch_seconds()..=Timestamp::MAX.epoch_seconds())
                .contains(&value.timestamp())
        })
        .map(of_chrono)
}

/// A timestamp chrono read, as ours must give it: `None` beyond the range of
/// `Timestamp`.
pub(super) fn in_range(value: DateTime<Utc>) -> Option<Moment> {
    expected(Some(value))
}

/// chrono's message for a value it accepted that the date types cannot hold.
pub(super) const OUT_OF_RANGE: &str = "input is out of range";

/// A result as a client is shown it: the value, or the error's message.
pub(super) fn shown<T, E: Display>(result: Result<T, E>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

/// What ours must show for what chrono gave: chrono's value, or chrono's
/// message word for word. A value chrono accepted and `keep` cannot hold is
/// out of range.
pub(super) fn as_chrono<T, U>(
    theirs: Result<U, chrono::ParseError>,
    keep: impl FnOnce(U) -> Option<T>,
) -> Result<T, String> {
    match theirs {
        Ok(value) => keep(value).ok_or_else(|| OUT_OF_RANGE.to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

fn date_in_range(date: NaiveDate) -> bool {
    (-9999..=9999).contains(&date.year())
}

fn ymd_ours(date: CivilDate) -> (i32, u32, u32) {
    (date.year(), date.month(), date.day())
}

fn ymd_chrono(date: NaiveDate) -> (i32, u32, u32) {
    (date.year(), date.month(), date.day())
}

fn hms_ours(time: CivilTime) -> (u32, u32, u32, u32) {
    (time.hour(), time.minute(), time.second(), time.nanosecond())
}

fn hms_chrono(time: NaiveTime) -> (u32, u32, u32, u32) {
    (
        time.hour(),
        time.minute(),
        time.second(),
        time.nanosecond() % 1_000_000_000,
    )
}

type Reading = ((i32, u32, u32), (u32, u32, u32, u32));

fn reading_ours(value: CivilDateTime) -> Reading {
    (ymd_ours(value.date()), hms_ours(value.time()))
}

fn reading_chrono(value: NaiveDateTime) -> Option<Reading> {
    date_in_range(value.date()).then(|| (ymd_chrono(value.date()), hms_chrono(value.time())))
}

/// The inbound verdict table: each input, and the timestamp it means (as
/// strict RFC 3339) through the lenient reader serde uses and through the
/// strict one. `None` is a rejection.
const VERDICTS: [(&str, Option<&str>, Option<&str>); 34] = [
    (
        "2026-11-17T10:00:00Z",
        Some("2026-11-17T10:00:00Z"),
        Some("2026-11-17T10:00:00Z"),
    ),
    (
        "2026-11-17T10:00:00+00:00",
        Some("2026-11-17T10:00:00Z"),
        Some("2026-11-17T10:00:00Z"),
    ),
    (
        "2026-11-17T05:00:00-05:00",
        Some("2026-11-17T10:00:00Z"),
        Some("2026-11-17T10:00:00Z"),
    ),
    (
        "2026-11-17T15:30:00+05:30",
        Some("2026-11-17T10:00:00Z"),
        Some("2026-11-17T10:00:00Z"),
    ),
    (
        "2026-11-17t10:00:00z",
        Some("2026-11-17T10:00:00Z"),
        Some("2026-11-17T10:00:00Z"),
    ),
    (
        "2026-11-17 10:00:00Z",
        Some("2026-11-17T10:00:00Z"),
        Some("2026-11-17T10:00:00Z"),
    ),
    (
        "2026-11-17 10:00:00 UTC",
        Some("2026-11-17T10:00:00Z"),
        None,
    ),
    (
        "2026-11-17 10:00:00 utc",
        Some("2026-11-17T10:00:00Z"),
        None,
    ),
    (
        "2026-11-17T10:00:00+0000",
        Some("2026-11-17T10:00:00Z"),
        None,
    ),
    (
        "2026-11-17T10:00:00 +01:00",
        Some("2026-11-17T09:00:00Z"),
        None,
    ),
    ("2026-11-17T10:00:00+00", None, None),
    ("2026-1-7T1:2:3Z", Some("2026-01-07T01:02:03Z"), None),
    (" 2026-11-17T10:00:00Z", Some("2026-11-17T10:00:00Z"), None),
    ("2026-11-17T10:00:00Z ", Some("2026-11-17T10:00:00Z"), None),
    ("2026-11-17T10:00:00", None, None),
    ("2026-11-17", None, None),
    ("2026-11-17T10:00Z", None, None),
    (
        "2026-11-17T10:00:00.1Z",
        Some("2026-11-17T10:00:00.100Z"),
        Some("2026-11-17T10:00:00.100Z"),
    ),
    (
        "2026-11-17T10:00:00.12Z",
        Some("2026-11-17T10:00:00.120Z"),
        Some("2026-11-17T10:00:00.120Z"),
    ),
    (
        "2026-11-17T10:00:00.123Z",
        Some("2026-11-17T10:00:00.123Z"),
        Some("2026-11-17T10:00:00.123Z"),
    ),
    (
        "2026-11-17T10:00:00.1234Z",
        Some("2026-11-17T10:00:00.123400Z"),
        Some("2026-11-17T10:00:00.123400Z"),
    ),
    (
        "2026-11-17T10:00:00.12345Z",
        Some("2026-11-17T10:00:00.123450Z"),
        Some("2026-11-17T10:00:00.123450Z"),
    ),
    (
        "2026-11-17T10:00:00.123456Z",
        Some("2026-11-17T10:00:00.123456Z"),
        Some("2026-11-17T10:00:00.123456Z"),
    ),
    (
        "2026-11-17T10:00:00.1234567Z",
        Some("2026-11-17T10:00:00.123456700Z"),
        Some("2026-11-17T10:00:00.123456700Z"),
    ),
    (
        "2026-11-17T10:00:00.12345678Z",
        Some("2026-11-17T10:00:00.123456780Z"),
        Some("2026-11-17T10:00:00.123456780Z"),
    ),
    (
        "2026-11-17T10:00:00.123456789Z",
        Some("2026-11-17T10:00:00.123456789Z"),
        Some("2026-11-17T10:00:00.123456789Z"),
    ),
    (
        "2026-11-17T10:00:00.1234567891Z",
        Some("2026-11-17T10:00:00.123456789Z"),
        Some("2026-11-17T10:00:00.123456789Z"),
    ),
    (
        "2026-11-17T23:59:60Z",
        Some("2026-11-17T23:59:59Z"),
        Some("2026-11-17T23:59:59Z"),
    ),
    ("2026-11-17T10:00:00.Z", None, None),
    ("2026-02-30T10:00:00Z", None, None),
    ("2026-11-17T24:00:00Z", None, None),
    ("2026-11-17T10:00:00+24:00", None, None),
    ("", None, None),
    ("2026-11-17T10:00:00Z\u{e9}", None, None),
];

#[test]
fn inbound_verdicts_are_the_recorded_ones() {
    for (input, lenient, strict) in VERDICTS {
        let meaning =
            |text: Option<&str>| text.map(|t| of_ours(Timestamp::parse_from_rfc3339(t).unwrap()));
        assert_eq!(
            input.parse::<Timestamp>().ok().map(of_ours),
            meaning(lenient),
            "lenient {input:?}"
        );
        assert_eq!(
            Timestamp::parse_from_rfc3339(input).ok().map(of_ours),
            meaning(strict),
            "strict {input:?}"
        );
        // serde reads through the lenient reader.
        let quoted = serde_json::to_string(input).unwrap();
        assert_eq!(
            serde_json::from_str::<Timestamp>(&quoted).ok().map(of_ours),
            meaning(lenient),
            "serde {input:?}"
        );
    }
}

#[test]
fn chrono_gives_the_recorded_inbound_verdicts() {
    // The table above is chrono's behaviour, not an opinion: this fails if
    // a row was recorded wrong.
    for (input, lenient, strict) in VERDICTS {
        let meaning = |text: Option<&str>| {
            text.map(|t| of_chrono(DateTime::parse_from_rfc3339(t).unwrap().to_utc()))
        };
        assert_eq!(
            input.parse::<DateTime<Utc>>().ok().map(of_chrono),
            meaning(lenient),
            "lenient {input:?}"
        );
        assert_eq!(
            DateTime::parse_from_rfc3339(input)
                .ok()
                .map(|value| of_chrono(value.to_utc())),
            meaning(strict),
            "strict {input:?}"
        );
        let quoted = serde_json::to_string(input).unwrap();
        assert_eq!(
            serde_json::from_str::<DateTime<Utc>>(&quoted)
                .ok()
                .map(of_chrono),
            meaning(lenient),
            "serde {input:?}"
        );
    }
}

#[test]
fn serde_rejects_what_is_not_a_string() {
    assert!(serde_json::from_str::<Timestamp>("1794909600").is_err());
    assert!(serde_json::from_str::<Timestamp>("null").is_err());
    assert!(serde_json::from_str::<CivilDate>("20261117").is_err());
    assert_eq!(
        serde_json::from_str::<Option<Timestamp>>("null").unwrap(),
        None
    );
}

#[test]
fn each_rejection_reads_as_chrono_worded_it() {
    let lenient = |text: &str| {
        (
            text.parse::<Timestamp>().unwrap_err().to_string(),
            text.parse::<DateTime<Utc>>().unwrap_err().to_string(),
        )
    };
    let strict = |text: &str| {
        (
            Timestamp::parse_from_rfc3339(text).unwrap_err().to_string(),
            DateTime::parse_from_rfc3339(text).unwrap_err().to_string(),
        )
    };
    let date = |text: &str, format: &str| {
        (
            CivilDate::parse_from_str(text, format)
                .unwrap_err()
                .to_string(),
            NaiveDate::parse_from_str(text, format)
                .unwrap_err()
                .to_string(),
        )
    };
    let with_offset = |text: &str, format: &str| {
        (
            Timestamp::parse_from_str(text, format)
                .unwrap_err()
                .to_string(),
            DateTime::parse_from_str(text, format)
                .unwrap_err()
                .to_string(),
        )
    };
    let cases = [
        (lenient("2026-11-17T10:00"), "premature end of input"),
        (lenient("2026-11-17T10:00:00Zx"), "trailing input"),
        (
            lenient("2026-11-17X10:00:00Z"),
            "input contains invalid characters",
        ),
        (lenient("2026-02-30T10:00:00Z"), "input is out of range"),
        (strict("2026-11-17"), "premature end of input"),
        (strict("2026-02-30x10:00:00Z"), "input is out of range"),
        (strict("2026-11-17T10:00:00+01:00 "), "trailing input"),
        (
            strict("2026-11-17T10:00:00+0100"),
            "input contains invalid characters",
        ),
        (
            date("Mon, 17 Nov 2026", "%a, %d %b %Y"),
            "no possible date and time matching input",
        ),
        (
            date("Nov 2026", "%b %Y"),
            "input is not enough for unique date and time",
        ),
        (
            with_offset("2026-11-17 10:00", "%Y-%m-%d %H:%M"),
            "input is not enough for unique date and time",
        ),
    ];
    for ((ours, theirs), worded) in cases {
        assert_eq!(theirs, worded);
        assert_eq!(ours, theirs);
    }
    assert_eq!(
        "Mond".parse::<Weekday>().unwrap_err().to_string(),
        "Mond".parse::<chrono::Weekday>().unwrap_err().to_string()
    );
    assert_eq!(
        serde_json::from_str::<Weekday>("\"Mond\"")
            .unwrap_err()
            .to_string(),
        "short or long weekday names expected at line 1 column 6"
    );
    assert_eq!(
        serde_json::from_str::<Timestamp>("1794909600")
            .unwrap_err()
            .to_string(),
        serde_json::from_str::<DateTime<Utc>>("1794909600")
            .unwrap_err()
            .to_string()
    );
}

/// Compares what serde read from `json` into ours and into chrono's type:
/// the same value, or the same message, position included. `Ok(None)` from
/// chrono is a value it read that the date types cannot hold.
fn same_serde<V: PartialEq + Debug>(
    json: &str,
    ours: Result<V, serde_json::Error>,
    theirs: Result<Option<V>, serde_json::Error>,
) -> bool {
    match (ours, theirs) {
        (Ok(ours), Ok(Some(theirs))) => {
            assert_eq!(ours, theirs, "{json}");
            true
        }
        (Err(ours), Err(theirs)) => {
            assert_eq!(ours.to_string(), theirs.to_string(), "{json}");
            false
        }
        (Err(ours), Ok(None)) => {
            assert!(ours.to_string().starts_with(OUT_OF_RANGE), "{json}: {ours}");
            false
        }
        (ours, theirs) => panic!("{json}: ours {ours:?}, chrono {theirs:?}"),
    }
}

#[test]
fn serde_rejections_read_like_chrono() {
    let mut texts: Vec<String> = VERDICTS.iter().map(|row| row.0.to_owned()).collect();
    for date in DATES {
        texts.push(date.to_owned());
        for time in TIMES {
            for offset in ["", "Z", "+01:00", "x"] {
                texts.push(format!("{date}T{time}{offset}"));
            }
        }
    }
    for time in TIMES {
        for fraction in FRACTIONS {
            texts.push(format!("{time}{fraction}"));
        }
    }
    texts.extend(["Mon", "monday", "Mond", "", " Mon", "1"].map(String::from));
    let mut documents: Vec<String> = texts
        .iter()
        .map(|text| serde_json::to_string(text).unwrap())
        .collect();
    documents.extend(["1794909600", "-1", "1.5", "true", "null", "[]", "{}"].map(String::from));
    let mut accepted = 0;
    for json in &documents {
        let json = json.as_str();
        let results = [
            same_serde(
                json,
                serde_json::from_str::<Timestamp>(json).map(of_ours),
                serde_json::from_str::<DateTime<Utc>>(json).map(in_range),
            ),
            same_serde(
                json,
                serde_json::from_str::<CivilDate>(json).map(ymd_ours),
                serde_json::from_str::<NaiveDate>(json)
                    .map(|d| date_in_range(d).then(|| ymd_chrono(d))),
            ),
            same_serde(
                json,
                serde_json::from_str::<CivilTime>(json).map(hms_ours),
                serde_json::from_str::<NaiveTime>(json).map(|t| Some(hms_chrono(t))),
            ),
            same_serde(
                json,
                serde_json::from_str::<CivilDateTime>(json).map(reading_ours),
                serde_json::from_str::<NaiveDateTime>(json).map(reading_chrono),
            ),
            same_serde(
                json,
                serde_json::from_str::<Weekday>(json).map(Weekday::num_days_from_monday),
                serde_json::from_str::<chrono::Weekday>(json)
                    .map(|day| Some(day.num_days_from_monday())),
            ),
        ];
        accepted += results.into_iter().filter(|read| *read).count();
    }
    // Both sides of the comparison are exercised.
    assert!(documents.len() > 800, "{} documents", documents.len());
    assert!(accepted > 100, "accepted {accepted}");
}

#[test]
fn timestamps_read_like_chrono_across_generated_inputs() {
    let (mut accepted, mut rejected) = (0_u32, 0_u32);
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
                        let lenient = as_chrono(input.parse::<DateTime<Utc>>(), in_range);
                        assert_eq!(
                            shown(input.parse::<Timestamp>().map(of_ours)),
                            lenient,
                            "lenient {input:?}"
                        );
                        let strict = as_chrono(
                            DateTime::parse_from_rfc3339(&input).map(|value| value.to_utc()),
                            in_range,
                        );
                        assert_eq!(
                            shown(Timestamp::parse_from_rfc3339(&input).map(of_ours)),
                            strict,
                            "strict {input:?}"
                        );
                        if lenient.is_ok() {
                            accepted += 1;
                        } else {
                            rejected += 1;
                        }
                    }
                }
            }
        }
    }
    // Both verdicts are well represented, so neither side of the comparison
    // is vacuous.
    assert_eq!(accepted + rejected, 17 * 6 * 11 * 11 * 26);
    assert!(accepted > 5_000, "accepted {accepted}");
    assert!(rejected > 200_000, "rejected {rejected}");
}

#[test]
fn the_strict_reader_keeps_the_offset_the_text_carried() {
    let read = |text| Timestamp::parse_from_rfc3339_with_offset(text).map(|(i, o)| (of_ours(i), o));
    assert_eq!(
        read("2026-11-17T05:00:00-05:00"),
        Ok(((1_794_909_600, 0), -18_000))
    );
    assert_eq!(
        read("2026-11-17T15:30:00+05:30"),
        Ok(((1_794_909_600, 0), 19_800))
    );
    assert_eq!(read("2026-11-17T10:00:00Z"), Ok(((1_794_909_600, 0), 0)));
    for text in ["2026-11-17T05:00:00-05:00", "2026-11-17T15:30:00.5+05:30"] {
        let theirs = DateTime::parse_from_rfc3339(text).unwrap();
        let (timestamp, offset) = Timestamp::parse_from_rfc3339_with_offset(text).unwrap();
        assert_eq!(of_ours(timestamp), of_chrono(theirs.to_utc()));
        assert_eq!(offset, theirs.offset().local_minus_utc());
    }
}

#[test]
fn civil_dates_read_like_chrono() {
    assert_eq!("2026-11-17".parse(), CivilDate::from_ymd(2026, 11, 17));
    assert_eq!("2026-1-7 ".parse(), CivilDate::from_ymd(2026, 1, 7));
    assert_eq!(
        "2026-02-30".parse::<CivilDate>(),
        Err(DateError::OutOfRange)
    );
    assert_eq!("2026-11".parse::<CivilDate>(), Err(DateError::TooShort));
    let mut compared = 0;
    for date in DATES {
        for lead in ["", " ", "\t"] {
            for trail in ["", " ", "  ", "x", "T10:00:00", "-", "\u{e9}"] {
                let input = format!("{lead}{date}{trail}");
                let keep = |d: NaiveDate| date_in_range(d).then(|| ymd_chrono(d));
                let theirs = as_chrono(input.parse::<NaiveDate>(), keep);
                assert_eq!(
                    shown(input.parse::<CivilDate>().map(ymd_ours)),
                    theirs,
                    "{input:?}"
                );
                let fixed = as_chrono(NaiveDate::parse_from_str(&input, "%Y-%m-%d"), keep);
                assert_eq!(
                    shown(CivilDate::parse_from_str(&input, "%Y-%m-%d").map(ymd_ours)),
                    fixed,
                    "%Y-%m-%d {input:?}"
                );
                compared += 1;
            }
        }
    }
    assert_eq!(compared, 17 * 3 * 7);
}

#[test]
fn civil_times_read_like_chrono() {
    assert_eq!("07:05".parse(), CivilTime::from_hms(7, 5, 0));
    assert_eq!("7:5:9.25".parse(), CivilTime::from_hms_milli(7, 5, 9, 250));
    assert_eq!("24:00".parse::<CivilTime>(), Err(DateError::OutOfRange));
    let mut compared = 0;
    for time in TIMES {
        for fraction in FRACTIONS {
            for trail in ["", " ", "Z", "+01:00", "x"] {
                for lead in ["", " "] {
                    let input = format!("{lead}{time}{fraction}{trail}");
                    let keep = |t: NaiveTime| Some(hms_chrono(t));
                    let theirs = as_chrono(input.parse::<NaiveTime>(), keep);
                    assert_eq!(
                        shown(input.parse::<CivilTime>().map(hms_ours)),
                        theirs,
                        "{input:?}"
                    );
                    for format in ["%H:%M:%S%.f", "%H:%M", "%T%.fZ", "%R%:z", "%T.f", "%H%M%S"] {
                        let theirs = as_chrono(NaiveTime::parse_from_str(&input, format), keep);
                        assert_eq!(
                            shown(CivilTime::parse_from_str(&input, format).map(hms_ours)),
                            theirs,
                            "{format} {input:?}"
                        );
                    }
                    compared += 1;
                }
            }
        }
    }
    assert_eq!(compared, 11 * 11 * 5 * 2);
}

/// Formats read with `parse_from_str`: the platform's own, the ones sqlx's
/// decoders use, and shapes that set a field twice or leave one out.
const READ_FORMATS: [&str; 16] = [
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f%#z",
    "%F %T%.f",
    "%F %R",
    "%F %RZ",
    "%F %R%:z",
    "%F %T%.fZ",
    "%F %T%.f%:z",
    "%FT%T%.f%:z",
    "%FT%T%.3f%z",
    "%Y-%m-%d",
    "%Y-%m-%d%t%H:%M:%S %d",
    "%Y%m%d %H%M%S%f",
    "%F%n%e:%k:%M",
];

#[test]
fn civil_date_times_read_like_chrono() {
    let expected: CivilDateTime = CivilDate::from_ymd(2026, 11, 17)
        .unwrap()
        .and_hms(10, 0, 0)
        .unwrap();
    assert_eq!("2026-11-17T10:00:00".parse(), Ok(expected));
    assert_eq!(
        "2026-11-17 10:00:00".parse::<CivilDateTime>(),
        Err(DateError::InvalidText)
    );
    assert_eq!(
        CivilDateTime::parse_from_str("2026-11-17 10:00", "%Y-%m-%d %H:%M"),
        Ok(expected)
    );
    let (mut accepted, mut compared) = (0, 0);
    let mut input = String::new();
    for date in DATES {
        for separator in SEPARATORS {
            for time in TIMES {
                for fraction in FRACTIONS {
                    for offset in ["", "Z", "+01:00", "+01", " -0530", "x"] {
                        input.clear();
                        for part in [date, separator, time, fraction, offset] {
                            input.push_str(part);
                        }
                        let theirs = as_chrono(input.parse::<NaiveDateTime>(), reading_chrono);
                        assert_eq!(
                            shown(input.parse::<CivilDateTime>().map(reading_ours)),
                            theirs,
                            "{input:?}"
                        );
                        for format in READ_FORMATS {
                            let naive = as_chrono(
                                NaiveDateTime::parse_from_str(&input, format),
                                reading_chrono,
                            );
                            assert_eq!(
                                shown(
                                    CivilDateTime::parse_from_str(&input, format).map(reading_ours)
                                ),
                                naive,
                                "{format} {input:?}"
                            );
                            let fixed = as_chrono(
                                DateTime::parse_from_str(&input, format)
                                    .map(|value| value.to_utc()),
                                in_range,
                            );
                            assert_eq!(
                                shown(Timestamp::parse_from_str(&input, format).map(of_ours)),
                                fixed,
                                "timestamp {format} {input:?}"
                            );
                            accepted += u32::from(naive.is_ok()) + u32::from(fixed.is_ok());
                            compared += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(compared, 17 * 6 * 11 * 11 * 6 * 16);
    assert!(accepted > 2_000, "accepted {accepted}");
}

#[test]
fn weekdays_read_like_chrono() {
    assert_eq!("Mon".parse(), Ok(Weekday::Mon));
    assert_eq!("WEDNESDAY".parse(), Ok(Weekday::Wed));
    assert_eq!("Mond".parse::<Weekday>(), Err(DateError::InvalidWeekday));
    let inputs = [
        "Mon",
        "mon",
        "MON",
        "Monday",
        "monday",
        "Tue",
        "tuesday",
        "Wed",
        "Wednesday",
        "Thu",
        "thursday",
        "Fri",
        "FRIDAY",
        "Sat",
        "Saturday",
        "Sun",
        "sunday",
        "Mond",
        "Mo",
        "",
        "Monday ",
        " Mon",
        "Tues",
        "Thur",
        "Sunda",
        "Sundayy",
        "m\u{f6}n",
        "\u{e9}\u{e9}",
        "Wednesdays",
        "1",
        "Mon\u{e9}",
    ];
    for input in inputs {
        let theirs = shown(
            input
                .parse::<chrono::Weekday>()
                .map(|day| day.num_days_from_monday()),
        );
        assert_eq!(
            shown(input.parse::<Weekday>().map(Weekday::num_days_from_monday)),
            theirs,
            "{input:?}"
        );
        let quoted = serde_json::to_string(input).unwrap();
        assert_eq!(
            shown(serde_json::from_str::<Weekday>(&quoted).map(Weekday::num_days_from_monday)),
            shown(
                serde_json::from_str::<chrono::Weekday>(&quoted)
                    .map(|day| day.num_days_from_monday())
            ),
            "serde {input:?}"
        );
    }
}

/// Formats the satellites read provider and export text with: month and
/// weekday names, a 12-hour clock, slashes.
const NAMED_FORMATS: [&str; 14] = [
    "%b %d, %Y, %I:%M:%S %p",
    "%B %d, %Y",
    "%b %d, %Y",
    "%m/%d/%Y",
    "%a, %d %b %Y",
    "%A %Y-%m-%d",
    "%A, %B %d, %Y %l:%M %P",
    "%h %e %Y %H:%M %p",
    "%I:%M %p",
    "%I:%M:%S%p",
    "%H:%M %p",
    "%I:%M",
    "%Y-%m-%d %a",
    "%b%d%Y",
];

#[test]
fn names_and_the_twelve_hour_clock_read_the_recorded_values() {
    let read = |text, format| CivilDateTime::parse_from_str(text, format);
    let expected = CivilDate::from_ymd(2026, 11, 17).unwrap();
    assert_eq!(
        read("Nov 17, 2026, 10:00:05 PM", "%b %d, %Y, %I:%M:%S %p"),
        expected.and_hms(22, 0, 5)
    );
    assert_eq!(
        read("nov 7, 2026, 12:00:05 am", "%b %d, %Y, %I:%M:%S %p").map(CivilDateTime::time),
        CivilTime::from_hms(0, 0, 5)
    );
    assert_eq!(
        CivilDate::parse_from_str("November 17, 2026", "%B %d, %Y"),
        Ok(expected)
    );
    assert_eq!(
        CivilDate::parse_from_str("11/17/2026", "%m/%d/%Y"),
        Ok(expected)
    );
    assert_eq!(
        CivilDate::parse_from_str("Tue, 17 Nov 2026", "%a, %d %b %Y"),
        Ok(expected)
    );
    // A weekday that is not the date's own is refused, as chrono refuses it.
    assert_eq!(
        CivilDate::parse_from_str("Mon, 17 Nov 2026", "%a, %d %b %Y"),
        Err(DateError::Impossible)
    );
    // A 12-hour hour without AM or PM names no time.
    assert_eq!(
        CivilTime::parse_from_str("10:00", "%I:%M"),
        Err(DateError::NotEnough)
    );
}

/// Text in the shapes the named formats read, and near misses of each.
fn named_inputs() -> Vec<String> {
    let months = [
        "Nov",
        "nov",
        "NOV",
        "November",
        "NOVEMBER",
        "Novem",
        "Sep",
        "Sept",
        "September",
        "May",
        "Mayo",
        "Ma",
        "Feb",
        "february",
        "F\u{e9}v",
        "13",
        "",
    ];
    let weekdays = ["Tue", "tuesday", "TUES", "Mon", "Sunday", "Tu", ""];
    let clocks = [
        "10:00:05 AM",
        "10:00:05 pm",
        "12:00:00 AM",
        "12:30:00 PM",
        "0:00:00 AM",
        "13:00:00 PM",
        "10:00:05",
        "10:00:05 A",
        "10:00:05 XM",
        "10:00:05AM",
        "1:05 pm",
        "01:05 Pm",
        "22:15 PM",
        "10:15 AM",
        "10:00:05 \u{202f}PM",
    ];
    let mut inputs: Vec<String> = Vec::new();
    for month in months {
        for clock in clocks.iter().take(4) {
            inputs.push(format!("{month} 17, 2026, {clock}"));
        }
        for day in ["17", "7", "07", "31", "0"] {
            inputs.push(format!("{month} {day}, 2026"));
            inputs.push(format!("{month}{day}2026"));
            inputs.push(format!("Tue, {day} {month} 2026"));
            inputs.push(format!("{month} {day} 2026 22:15 PM"));
        }
    }
    for weekday in weekdays {
        inputs.push(format!("{weekday} 2026-11-17"));
        inputs.push(format!("2026-11-17 {weekday}"));
        inputs.push(format!("{weekday}, 17 Nov 2026"));
        inputs.push(format!("{weekday}, November 17, 2026 1:05 pm"));
    }
    inputs.extend(clocks.iter().map(|clock| (*clock).to_owned()));
    inputs.extend(
        [
            "11/17/2026",
            "1/7/2026",
            "13/01/2026",
            "11/31/2026",
            "11-17-2026",
        ]
        .map(String::from),
    );

    inputs
}

#[test]
fn names_and_the_twelve_hour_clock_read_like_chrono_across_generated_inputs() {
    let inputs = named_inputs();
    let (mut accepted, mut compared) = (0, 0);
    for input in &inputs {
        for format in NAMED_FORMATS {
            let date = as_chrono(NaiveDate::parse_from_str(input, format), |d| {
                date_in_range(d).then(|| ymd_chrono(d))
            });
            assert_eq!(
                shown(CivilDate::parse_from_str(input, format).map(ymd_ours)),
                date,
                "date {format} {input:?}"
            );
            let time = as_chrono(NaiveTime::parse_from_str(input, format), |t| {
                Some(hms_chrono(t))
            });
            assert_eq!(
                shown(CivilTime::parse_from_str(input, format).map(hms_ours)),
                time,
                "time {format} {input:?}"
            );
            let both = as_chrono(NaiveDateTime::parse_from_str(input, format), reading_chrono);
            assert_eq!(
                shown(CivilDateTime::parse_from_str(input, format).map(reading_ours)),
                both,
                "date-time {format} {input:?}"
            );
            accepted += u32::from(date.is_ok()) + u32::from(time.is_ok()) + u32::from(both.is_ok());
            compared += 1;
        }
    }
    assert_eq!(compared, inputs.len() * 14);
    assert!(
        compared > 6_000 && accepted > 200,
        "{accepted} of {compared}"
    );
}

#[test]
fn a_format_that_cannot_be_read_is_an_error() {
    let unsupported = |spec: &str| DateError::UnsupportedFormat(spec.to_owned());
    assert_eq!(
        CivilDate::parse_from_str("17 11 26", "%d %m %y"),
        Err(unsupported("%y"))
    );
    assert_eq!(
        CivilDate::parse_from_str("2026 321", "%Y %j"),
        Err(unsupported("%j"))
    );
    assert_eq!(
        CivilDate::parse_from_str("2026-11-17", "%Y-%m-%d %U"),
        Err(unsupported("%U"))
    );
    assert_eq!(
        Timestamp::parse_from_str("1794909600", "%s"),
        Err(unsupported("%s"))
    );
    // A date-time format with no offset names no timestamp.
    assert_eq!(
        Timestamp::parse_from_str("2026-11-17 10:00:00", "%Y-%m-%d %H:%M:%S"),
        Err(DateError::NotEnough)
    );
}

#[test]
fn values_beyond_the_range_are_rejected_not_wrapped() {
    // chrono accepts these; the date types hold years -9999 to 9999.
    assert!("+10000-01-01T00:00:00Z".parse::<DateTime<Utc>>().is_ok());
    assert_eq!(
        "+10000-01-01T00:00:00Z".parse::<Timestamp>(),
        Err(DateError::OutOfRange)
    );
    assert!("+10000-01-01".parse::<NaiveDate>().is_ok());
    assert_eq!(
        "+10000-01-01".parse::<CivilDate>(),
        Err(DateError::OutOfRange)
    );
    assert_eq!(
        "9999-12-31T23:59:59Z".parse::<Timestamp>(),
        Err(DateError::OutOfRange)
    );
    assert_eq!(
        "9999-12-30T22:00:00Z".parse::<Timestamp>().map(of_ours),
        Ok((253_402_207_200, 0))
    );
    assert_eq!(
        Timestamp::from_epoch_seconds(i64::MAX),
        Err(DateError::OutOfRange)
    );
    assert_eq!(
        Timestamp::from_epoch_seconds_and_nanos(0, 1_000_000_000),
        Err(DateError::OutOfRange)
    );
}
