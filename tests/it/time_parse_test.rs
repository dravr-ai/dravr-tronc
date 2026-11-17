// ABOUTME: Input goldens for the date types: what is accepted and what it means, recorded from chrono's behaviour
// ABOUTME: A literal verdict table, and a few hundred thousand generated inputs compared with chrono itself
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use dravr_tronc::time::{CivilDate, CivilDateTime, CivilTime, Instant, TimeError, Weekday};

const DATES: [&str; 17] = [
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

const SEPARATORS: [&str; 6] = ["T", "t", " ", "  ", "_", ""];

const TIMES: [&str; 11] = [
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

const FRACTIONS: [&str; 11] = [
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

const OFFSETS: [&str; 26] = [
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
type Moment = (i64, u32);

fn of_chrono(value: DateTime<Utc>) -> Moment {
    (
        value.timestamp(),
        value.timestamp_subsec_nanos() % 1_000_000_000,
    )
}

fn of_ours(value: Instant) -> Moment {
    (value.epoch_seconds(), value.subsec_nanos())
}

/// What ours must give for what chrono gave: the same instant, or an error
/// where chrono errs or where the instant is outside the range of `Instant`.
fn expected(theirs: Option<DateTime<Utc>>) -> Option<Moment> {
    theirs
        .filter(|value| {
            (Instant::MIN.epoch_seconds()..=Instant::MAX.epoch_seconds())
                .contains(&value.timestamp())
        })
        .map(of_chrono)
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

/// The inbound verdict table: each input, and the instant it means (as
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
            |text: Option<&str>| text.map(|t| of_ours(Instant::parse_from_rfc3339(t).unwrap()));
        assert_eq!(
            input.parse::<Instant>().ok().map(of_ours),
            meaning(lenient),
            "lenient {input:?}"
        );
        assert_eq!(
            Instant::parse_from_rfc3339(input).ok().map(of_ours),
            meaning(strict),
            "strict {input:?}"
        );
        // serde reads through the lenient reader.
        let quoted = serde_json::to_string(input).unwrap();
        assert_eq!(
            serde_json::from_str::<Instant>(&quoted).ok().map(of_ours),
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
    assert!(serde_json::from_str::<Instant>("1794909600").is_err());
    assert!(serde_json::from_str::<Instant>("null").is_err());
    assert!(serde_json::from_str::<CivilDate>("20261117").is_err());
    assert_eq!(
        serde_json::from_str::<Option<Instant>>("null").unwrap(),
        None
    );
}

#[test]
fn instants_read_like_chrono_across_generated_inputs() {
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
                        let lenient = expected(input.parse::<DateTime<Utc>>().ok());
                        assert_eq!(
                            input.parse::<Instant>().ok().map(of_ours),
                            lenient,
                            "lenient {input:?}"
                        );
                        let strict = expected(
                            DateTime::parse_from_rfc3339(&input)
                                .ok()
                                .map(|value| value.to_utc()),
                        );
                        assert_eq!(
                            Instant::parse_from_rfc3339(&input).ok().map(of_ours),
                            strict,
                            "strict {input:?}"
                        );
                        if lenient.is_some() {
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
    let read = |text| Instant::parse_from_rfc3339_with_offset(text).map(|(i, o)| (of_ours(i), o));
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
        let (instant, offset) = Instant::parse_from_rfc3339_with_offset(text).unwrap();
        assert_eq!(of_ours(instant), of_chrono(theirs.to_utc()));
        assert_eq!(offset, theirs.offset().local_minus_utc());
    }
}

#[test]
fn civil_dates_read_like_chrono() {
    assert_eq!("2026-11-17".parse(), CivilDate::from_ymd(2026, 11, 17));
    assert_eq!("2026-1-7 ".parse(), CivilDate::from_ymd(2026, 1, 7));
    assert_eq!(
        "2026-02-30".parse::<CivilDate>(),
        Err(TimeError::OutOfRange)
    );
    assert_eq!("2026-11".parse::<CivilDate>(), Err(TimeError::InvalidText));
    let mut compared = 0;
    for date in DATES {
        for lead in ["", " ", "\t"] {
            for trail in ["", " ", "  ", "x", "T10:00:00", "-", "\u{e9}"] {
                let input = format!("{lead}{date}{trail}");
                let theirs = input
                    .parse::<NaiveDate>()
                    .ok()
                    .filter(|d| date_in_range(*d))
                    .map(ymd_chrono);
                assert_eq!(
                    input.parse::<CivilDate>().ok().map(ymd_ours),
                    theirs,
                    "{input:?}"
                );
                let fixed = NaiveDate::parse_from_str(&input, "%Y-%m-%d")
                    .ok()
                    .filter(|d| date_in_range(*d))
                    .map(ymd_chrono);
                assert_eq!(
                    CivilDate::parse_from_str(&input, "%Y-%m-%d")
                        .ok()
                        .map(ymd_ours),
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
    assert_eq!("24:00".parse::<CivilTime>(), Err(TimeError::OutOfRange));
    let mut compared = 0;
    for time in TIMES {
        for fraction in FRACTIONS {
            for trail in ["", " ", "Z", "+01:00", "x"] {
                for lead in ["", " "] {
                    let input = format!("{lead}{time}{fraction}{trail}");
                    let theirs = input.parse::<NaiveTime>().ok().map(hms_chrono);
                    assert_eq!(
                        input.parse::<CivilTime>().ok().map(hms_ours),
                        theirs,
                        "{input:?}"
                    );
                    for format in ["%H:%M:%S%.f", "%H:%M", "%T%.fZ", "%R%:z", "%T.f", "%H%M%S"] {
                        let theirs = NaiveTime::parse_from_str(&input, format)
                            .ok()
                            .map(hms_chrono);
                        assert_eq!(
                            CivilTime::parse_from_str(&input, format).ok().map(hms_ours),
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
        Err(TimeError::InvalidText)
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
                        let theirs = input.parse::<NaiveDateTime>().ok().and_then(reading_chrono);
                        assert_eq!(
                            input.parse::<CivilDateTime>().ok().map(reading_ours),
                            theirs,
                            "{input:?}"
                        );
                        for format in READ_FORMATS {
                            let naive = NaiveDateTime::parse_from_str(&input, format)
                                .ok()
                                .and_then(reading_chrono);
                            assert_eq!(
                                CivilDateTime::parse_from_str(&input, format)
                                    .ok()
                                    .map(reading_ours),
                                naive,
                                "{format} {input:?}"
                            );
                            let fixed =
                                expected_instant(DateTime::parse_from_str(&input, format).ok());
                            assert_eq!(
                                Instant::parse_from_str(&input, format).ok().map(of_ours),
                                fixed,
                                "instant {format} {input:?}"
                            );
                            accepted += u32::from(naive.is_some()) + u32::from(fixed.is_some());
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

fn expected_instant(theirs: Option<DateTime<chrono::FixedOffset>>) -> Option<Moment> {
    expected(theirs.map(|value| value.to_utc()))
}

#[test]
fn weekdays_read_like_chrono() {
    assert_eq!("Mon".parse(), Ok(Weekday::Mon));
    assert_eq!("WEDNESDAY".parse(), Ok(Weekday::Wed));
    assert_eq!("Mond".parse::<Weekday>(), Err(TimeError::InvalidText));
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
        let theirs = input
            .parse::<chrono::Weekday>()
            .ok()
            .map(|day| day.num_days_from_monday());
        assert_eq!(
            input
                .parse::<Weekday>()
                .ok()
                .map(Weekday::num_days_from_monday),
            theirs,
            "{input:?}"
        );
        let quoted = serde_json::to_string(input).unwrap();
        assert_eq!(
            serde_json::from_str::<Weekday>(&quoted)
                .ok()
                .map(Weekday::num_days_from_monday),
            theirs,
            "serde {input:?}"
        );
    }
}

#[test]
fn a_format_that_cannot_be_read_is_an_error() {
    let unsupported = |spec: &str| TimeError::UnsupportedFormat(spec.to_owned());
    assert_eq!(
        CivilDate::parse_from_str("17 Nov 2026", "%d %b %Y"),
        Err(unsupported("%b"))
    );
    assert_eq!(
        CivilTime::parse_from_str("10:00 AM", "%I:%M %p"),
        Err(unsupported("%I"))
    );
    assert_eq!(
        CivilDate::parse_from_str("2026-11-17", "%Y-%m-%d %U"),
        Err(unsupported("%U"))
    );
    // A date-time format with no offset names no instant.
    assert_eq!(
        Instant::parse_from_str("2026-11-17 10:00:00", "%Y-%m-%d %H:%M:%S"),
        Err(TimeError::InvalidText)
    );
}

#[test]
fn values_beyond_the_range_are_rejected_not_wrapped() {
    // chrono accepts these; the date types hold years -9999 to 9999.
    assert!("+10000-01-01T00:00:00Z".parse::<DateTime<Utc>>().is_ok());
    assert_eq!(
        "+10000-01-01T00:00:00Z".parse::<Instant>(),
        Err(TimeError::OutOfRange)
    );
    assert!("+10000-01-01".parse::<NaiveDate>().is_ok());
    assert_eq!(
        "+10000-01-01".parse::<CivilDate>(),
        Err(TimeError::OutOfRange)
    );
    assert_eq!(
        "9999-12-31T23:59:59Z".parse::<Instant>(),
        Err(TimeError::OutOfRange)
    );
    assert_eq!(
        "9999-12-30T22:00:00Z".parse::<Instant>().map(of_ours),
        Ok((253_402_207_200, 0))
    );
    assert_eq!(
        Instant::from_epoch_seconds(i64::MAX),
        Err(TimeError::OutOfRange)
    );
    assert_eq!(
        Instant::from_epoch_seconds_and_nanos(0, 1_000_000_000),
        Err(TimeError::OutOfRange)
    );
}
