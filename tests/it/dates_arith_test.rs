// ABOUTME: Arithmetic goldens for the date types: literal expected results, and the same operation through chrono
// ABOUTME: Days of 86,400 seconds, truncated differences, month ends, ISO weeks, wrapping times and saturation
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveTime, TimeDelta, Timelike};
use dravr_tronc::dates::{
    CivilDate, CivilDateTime, CivilTime, DateError, Elapsed, Timestamp, Weekday,
};

use crate::dates_support::{pair, pairs};

fn timestamp(text: &str) -> Timestamp {
    Timestamp::parse_from_rfc3339(text).unwrap()
}

fn date(year: i32, month: u32, day: u32) -> CivilDate {
    CivilDate::from_ymd(year, month, day).unwrap()
}

fn naive(value: CivilDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(value.year(), value.month(), value.day()).unwrap()
}

fn civil(value: NaiveDate) -> CivilDate {
    date(value.year(), value.month(), value.day())
}

/// Every date from `first`, `count` days on.
fn run(first: CivilDate, count: i64) -> Vec<CivilDate> {
    (0..count)
        .map(|days| first.checked_add_days(days).unwrap())
        .collect()
}

#[test]
fn a_day_on_an_timestamp_is_86_400_seconds_also_across_a_clock_change() {
    // 2026-03-08 is the night North American clocks go forward; a timestamp
    // does not care.
    let before = timestamp("2026-03-08T04:00:00Z");
    for (days, expected) in [
        (1, "2026-03-09T04:00:00Z"),
        (7, "2026-03-15T04:00:00Z"),
        (30, "2026-04-07T04:00:00Z"),
        (90, "2026-06-06T04:00:00Z"),
    ] {
        let later = before + Elapsed::days(days);
        assert_eq!(later, timestamp(expected));
        assert_eq!((later - before).num_seconds(), days * 86_400);
        assert_eq!(later - Elapsed::days(days), before);
    }
    for (ours, theirs) in pairs() {
        for days in [1, 7, 30, 90, -1, -7, -30, -90] {
            let (plus, minus) = (ours + Elapsed::days(days), ours - Elapsed::days(days));
            let (chrono_plus, chrono_minus) =
                (theirs + Duration::days(days), theirs - Duration::days(days));
            assert_eq!(plus.epoch_nanos(), nanos(chrono_plus));
            assert_eq!(minus.epoch_nanos(), nanos(chrono_minus));
        }
        for elapsed in [
            (Elapsed::weeks(2), Duration::weeks(2)),
            (Elapsed::hours(-36), Duration::hours(-36)),
            (Elapsed::minutes(90), Duration::minutes(90)),
            (Elapsed::seconds(-1), Duration::seconds(-1)),
            (Elapsed::milliseconds(1_500), Duration::milliseconds(1_500)),
            (Elapsed::microseconds(-7), Duration::microseconds(-7)),
            (Elapsed::nanoseconds(1), Duration::nanoseconds(1)),
        ] {
            assert_eq!((ours + elapsed.0).epoch_nanos(), nanos(theirs + elapsed.1));
        }
    }
}

fn nanos(value: chrono::DateTime<chrono::Utc>) -> i128 {
    i128::from(value.timestamp()) * 1_000_000_000 + i128::from(value.timestamp_subsec_nanos())
}

#[test]
fn a_difference_in_days_is_truncated_not_rounded() {
    let start = timestamp("2026-11-17T10:00:00Z");
    for (later, days, hours) in [
        ("2026-11-18T09:59:59Z", 0, 23),
        ("2026-11-18T10:00:00Z", 1, 24),
        ("2026-11-19T09:59:59.999Z", 1, 47),
        ("2026-11-16T10:00:01Z", 0, -23),
        ("2026-11-16T10:00:00Z", -1, -24),
        ("2026-11-10T09:00:00Z", -7, -169),
        ("2026-12-17T10:00:00Z", 30, 720),
    ] {
        let elapsed = timestamp(later) - start;
        assert_eq!(
            (elapsed.num_days(), elapsed.num_hours()),
            (days, hours),
            "{later}"
        );
        assert_eq!(timestamp(later).signed_duration_since(start), elapsed);
    }
}

#[test]
fn elapsed_getters_match_chrono() {
    let lengths: [i64; 14] = [
        0,
        1,
        -1,
        999_999_999,
        1_000_000_000,
        -1_500_000_000,
        86_399_999_999_999,
        86_400_000_000_000,
        -86_399_999_999_999,
        -86_400_000_000_001,
        7_776_000_000_000_000,
        604_800_000_000_001,
        -604_799_999_999_999,
        3_600_000_000_500,
    ];
    for length in lengths {
        let (ours, theirs) = (Elapsed::nanoseconds(length), TimeDelta::nanoseconds(length));
        assert_eq!(ours.num_weeks(), theirs.num_weeks(), "{length}");
        assert_eq!(ours.num_days(), theirs.num_days(), "{length}");
        assert_eq!(ours.num_hours(), theirs.num_hours(), "{length}");
        assert_eq!(ours.num_minutes(), theirs.num_minutes(), "{length}");
        assert_eq!(ours.num_seconds(), theirs.num_seconds(), "{length}");
        assert_eq!(
            ours.num_milliseconds(),
            theirs.num_milliseconds(),
            "{length}"
        );
        assert_eq!(
            ours.num_microseconds(),
            theirs.num_microseconds(),
            "{length}"
        );
        assert_eq!(ours.num_nanoseconds(), theirs.num_nanoseconds(), "{length}");
        assert_eq!(ours.subsec_nanos(), theirs.subsec_nanos(), "{length}");
        assert_eq!(ours.abs().num_nanoseconds(), theirs.abs().num_nanoseconds());
        assert_eq!((-ours).num_nanoseconds(), (-theirs).num_nanoseconds());
        assert_eq!(ours.is_zero(), theirs.is_zero());
    }
    assert_eq!(Elapsed::days(1), Elapsed::seconds(86_400));
    assert_eq!(Elapsed::weeks(1), Elapsed::days(7));
    assert_eq!(Elapsed::hours(1), Elapsed::minutes(60));
    assert_eq!(Elapsed::seconds(1), Elapsed::milliseconds(1_000));
    assert_eq!(Elapsed::milliseconds(1), Elapsed::microseconds(1_000));
    assert_eq!(Elapsed::microseconds(1), Elapsed::nanoseconds(1_000));
    assert_eq!(Elapsed::hours(36) * 2, Elapsed::days(3));
    assert_eq!(Elapsed::days(3).checked_div(2), Some(Elapsed::hours(36)));
    assert_eq!(Elapsed::days(3).checked_div(0), None);
    assert_eq!(
        Elapsed::seconds(90).to_std(),
        Ok(StdDuration::from_secs(90))
    );
    assert_eq!(Elapsed::seconds(-1).to_std(), Err(DateError::OutOfRange));
    assert_eq!(
        Elapsed::from_std(StdDuration::from_millis(1_500)),
        Ok(Elapsed::milliseconds(1_500))
    );
}

/// `Elapsed::from_std` refuses what `TimeDelta::from_std` refused: past
/// `i64::MAX` milliseconds, 9,223,372,036,854,775.807 seconds.
#[test]
fn from_std_refuses_what_chrono_refused() {
    const LONGEST_SECONDS: u64 = 9_223_372_036_854_775;
    let durations = [
        StdDuration::ZERO,
        StdDuration::from_nanos(1),
        StdDuration::from_millis(1_500),
        StdDuration::from_secs(31_536_000_000_123),
        StdDuration::new(LONGEST_SECONDS, 806_999_999),
        StdDuration::new(LONGEST_SECONDS, 807_000_000),
        StdDuration::new(LONGEST_SECONDS, 807_000_001),
        StdDuration::new(LONGEST_SECONDS, 999_999_999),
        StdDuration::from_secs(LONGEST_SECONDS + 1),
        StdDuration::from_secs(i64::MAX.unsigned_abs()),
        StdDuration::from_secs(u64::MAX),
        StdDuration::MAX,
    ];
    let mut accepted = 0;
    for duration in durations {
        let ours = Elapsed::from_std(duration);
        match TimeDelta::from_std(duration) {
            Ok(theirs) => {
                let ours = ours.unwrap_or_else(|error| panic!("{duration:?}: {error}"));
                assert_eq!(ours.num_seconds(), theirs.num_seconds(), "{duration:?}");
                assert_eq!(ours.subsec_nanos(), theirs.subsec_nanos(), "{duration:?}");
                assert_eq!(ours.to_std(), Ok(duration), "{duration:?}");
                accepted += 1;
            }
            Err(_) => assert_eq!(ours, Err(DateError::OutOfRange), "{duration:?}"),
        }
    }
    // The boundary is where chrono put it: the last accepted row is chrono's
    // TimeDelta::MAX itself.
    assert_eq!(accepted, 6);
    assert_eq!(
        Elapsed::from_std(StdDuration::new(LONGEST_SECONDS, 807_000_000))
            .map(|e| e.num_milliseconds()),
        Ok(i64::MAX)
    );
}

#[test]
fn epoch_getters_round_down_like_chrono() {
    for (ours, theirs) in pairs() {
        assert_eq!(ours.epoch_seconds(), theirs.timestamp());
        assert_eq!(ours.epoch_millis(), theirs.timestamp_millis());
        assert_eq!(ours.epoch_micros(), theirs.timestamp_micros());
        assert_eq!(ours.subsec_nanos(), theirs.timestamp_subsec_nanos());
        assert_eq!(ours.subsec_micros(), theirs.timestamp_subsec_micros());
        assert_eq!(ours.subsec_millis(), theirs.timestamp_subsec_millis());
        assert_eq!(
            Timestamp::from_epoch_millis(theirs.timestamp_millis())
                .unwrap()
                .epoch_millis(),
            theirs.timestamp_millis()
        );
        assert_eq!(
            Timestamp::from_epoch_micros(theirs.timestamp_micros()).unwrap(),
            Timestamp::from_epoch_nanos(i128::from(theirs.timestamp_micros()) * 1_000).unwrap()
        );
    }
    let (before_epoch, _) = pair(-1, 500_000_000);
    assert_eq!(before_epoch.epoch_seconds(), -1);
    assert_eq!(before_epoch.epoch_millis(), -500);
    assert_eq!(before_epoch.subsec_nanos(), 500_000_000);
    assert_eq!(Timestamp::default(), Timestamp::UNIX_EPOCH);
    assert_eq!(Timestamp::UNIX_EPOCH.epoch_nanos(), 0);
}

#[test]
fn the_utc_date_turns_at_midnight() {
    let last = timestamp("2026-11-17T23:59:59.999Z");
    assert_eq!(last.utc_date(), date(2026, 11, 17));
    assert_eq!(
        last.utc_time(),
        CivilTime::from_hms_milli(23, 59, 59, 999).unwrap()
    );
    let midnight = timestamp("2026-11-18T00:00:00Z");
    assert_eq!(midnight.utc_date(), date(2026, 11, 18));
    assert_eq!(midnight.utc_time(), CivilTime::MIDNIGHT);
    assert_eq!(midnight.utc_datetime(), date(2026, 11, 18).at_midnight());
    assert_eq!(date(2026, 11, 18).at_midnight().and_utc(), Ok(midnight));
    assert_eq!(
        date(2026, 11, 17).and_hms(23, 59, 59).unwrap().and_utc(),
        Ok(timestamp("2026-11-17T23:59:59Z"))
    );
    assert_eq!(
        date(2026, 11, 17).and_hms(24, 0, 0),
        Err(DateError::OutOfRange)
    );
    for (ours, theirs) in pairs() {
        assert_eq!(naive(ours.utc_date()), theirs.date_naive());
        assert_eq!(ours.utc_datetime().and_utc(), Ok(ours));
    }
}

#[test]
fn months_reach_from_one_end_of_the_range_to_the_other() {
    // 239,987 months separate January -9999 from December 9999.
    assert_eq!(
        CivilDate::MIN.checked_add_months(239_987),
        Some(date(9999, 12, 1))
    );
    assert_eq!(
        date(-9999, 1, 31).checked_add_months(239_987),
        Some(CivilDate::MAX)
    );
    assert_eq!(
        CivilDate::MAX.checked_sub_months(239_987),
        Some(date(-9999, 1, 31))
    );
    assert_eq!(CivilDate::MIN.checked_add_months(239_988), None);
    assert_eq!(CivilDate::MAX.checked_sub_months(239_988), None);
    assert_eq!(CivilDate::MIN.checked_add_months(u32::MAX), None);
    assert_eq!(CivilDate::MAX.checked_sub_months(u32::MAX), None);

    // A date wherever chrono gives one inside the range, and the same one.
    let in_range = |day: Option<NaiveDate>| day.filter(|d| (-9999..=9999).contains(&d.year()));
    let days = [
        CivilDate::MIN,
        date(-9999, 1, 31),
        date(-9999, 12, 31),
        date(-4800, 2, 29),
        date(-1, 12, 31),
        date(0, 2, 29),
        date(1, 1, 1),
        date(2024, 1, 31),
        date(2024, 2, 29),
        date(9999, 1, 31),
        date(9999, 12, 1),
        CivilDate::MAX,
    ];
    let spans = [
        0,
        1,
        11,
        12,
        13,
        1_200,
        119_993,
        119_994,
        239_975,
        239_976,
        239_977,
        239_986,
        239_987,
        239_988,
        240_000,
        3_000_000,
        u32::MAX,
    ];
    let (mut compared, mut found) = (0, 0);
    for day in days {
        for months in spans {
            let later = day.checked_add_months(months);
            let earlier = day.checked_sub_months(months);
            assert_eq!(
                later.map(naive),
                in_range(naive(day).checked_add_months(Months::new(months))),
                "{day} + {months} months"
            );
            assert_eq!(
                earlier.map(naive),
                in_range(naive(day).checked_sub_months(Months::new(months))),
                "{day} - {months} months"
            );
            compared += 2;
            found += u32::from(later.is_some()) + u32::from(earlier.is_some());
        }
    }
    assert_eq!(compared, 12 * 17 * 2);
    assert!(found > 150, "found {found}");
}

#[test]
fn a_month_from_the_31st_ends_on_the_last_day_of_the_shorter_month() {
    assert_eq!(
        date(2026, 1, 31).checked_add_months(1),
        Some(date(2026, 2, 28))
    );
    assert_eq!(
        date(2024, 1, 31).checked_add_months(1),
        Some(date(2024, 2, 29))
    );
    assert_eq!(
        date(2026, 3, 31).checked_sub_months(1),
        Some(date(2026, 2, 28))
    );
    assert_eq!(
        date(2024, 2, 29).checked_add_months(12),
        Some(date(2025, 2, 28))
    );
    assert_eq!(
        date(2024, 2, 29).checked_sub_months(12),
        Some(date(2023, 2, 28))
    );
    assert_eq!(
        date(2024, 2, 29).checked_add_months(48),
        Some(date(2028, 2, 29))
    );
    assert_eq!(
        date(2026, 11, 30).checked_add_months(3),
        Some(date(2027, 2, 28))
    );
    assert_eq!(
        date(2026, 5, 15).checked_add_months(0),
        Some(date(2026, 5, 15))
    );
    assert_eq!(CivilDate::MAX.checked_add_months(1), None);
    assert_eq!(CivilDate::MIN.checked_sub_months(1), None);

    let mut compared = 0;
    for day in run(date(2023, 12, 25), 440) {
        for months in 0..=30 {
            assert_eq!(
                day.checked_add_months(months).map(naive),
                naive(day).checked_add_months(Months::new(months)),
                "{day} + {months} months"
            );
            assert_eq!(
                day.checked_sub_months(months).map(naive),
                naive(day).checked_sub_months(Months::new(months)),
                "{day} - {months} months"
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 440 * 31);
}

#[test]
fn changing_one_field_of_a_date_matches_chrono() {
    assert_eq!(date(2024, 2, 29).with_year(2025), None);
    assert_eq!(date(2024, 2, 29).with_year(2028), Some(date(2028, 2, 29)));
    assert_eq!(date(2026, 1, 31).with_month(2), None);
    assert_eq!(date(2026, 1, 31).with_month(3), Some(date(2026, 3, 31)));
    assert_eq!(date(2026, 2, 1).with_day(29), None);
    for day in run(date(2023, 12, 25), 440) {
        for year in [2023, 2024, 2025, 2100] {
            assert_eq!(day.with_year(year).map(naive), naive(day).with_year(year));
        }
        for month in [0, 1, 2, 4, 12, 13] {
            assert_eq!(
                day.with_month(month).map(naive),
                naive(day).with_month(month)
            );
        }
        for dom in [0, 1, 28, 29, 30, 31, 32] {
            assert_eq!(day.with_day(dom).map(naive), naive(day).with_day(dom));
        }
    }
}

#[test]
fn calendar_fields_weeks_and_iso_weeks_match_chrono() {
    // The ISO week-numbering year differs from the calendar year here.
    assert_eq!(date(2024, 12, 30).iso_week(), (2025, 1));
    assert_eq!(date(2021, 1, 3).iso_week(), (2020, 53));
    assert_eq!(date(2026, 12, 31).iso_week(), (2026, 53));
    assert_eq!(date(2027, 1, 1).iso_week(), (2026, 53));
    assert_eq!(date(2026, 11, 17).weekday(), Weekday::Tue);
    assert_eq!(
        date(2026, 11, 17).start_of_week(Weekday::Mon),
        date(2026, 11, 16)
    );
    assert_eq!(
        date(2026, 11, 17).start_of_week(Weekday::Sun),
        date(2026, 11, 15)
    );
    assert_eq!(
        date(2026, 11, 16).start_of_week(Weekday::Mon),
        date(2026, 11, 16)
    );
    assert_eq!(
        date(2027, 1, 1).start_of_week(Weekday::Mon),
        date(2026, 12, 28)
    );
    assert_eq!(
        CivilDate::from_iso_week(2026, 53, Weekday::Fri),
        Ok(date(2027, 1, 1))
    );
    assert_eq!(
        CivilDate::from_iso_week(2025, 53, Weekday::Mon),
        Err(DateError::OutOfRange)
    );

    let mut compared = 0;
    for first in [date(2019, 12, 20), date(2023, 12, 20), date(2026, 2, 20)] {
        for day in run(first, 420) {
            let theirs = naive(day);
            assert_eq!(day.year(), theirs.year());
            assert_eq!(day.month(), theirs.month());
            assert_eq!(day.day(), theirs.day());
            assert_eq!(day.ordinal(), theirs.ordinal());
            assert_eq!(
                day.weekday().num_days_from_monday(),
                theirs.weekday().num_days_from_monday()
            );
            assert_eq!(day.weekday().to_string(), theirs.weekday().to_string());
            assert_eq!(
                day.iso_week(),
                (theirs.iso_week().year(), theirs.iso_week().week())
            );
            assert_eq!(day.is_leap_year(), theirs.leap_year());
            assert_eq!(day.succ().map(naive), theirs.succ_opt());
            assert_eq!(day.pred().map(naive), theirs.pred_opt());
            assert_eq!(
                naive(day.last_of_month()),
                naive(day.first_of_month())
                    .checked_add_months(Months::new(1))
                    .unwrap()
                    .pred_opt()
                    .unwrap()
            );
            assert_eq!(day.days_in_month(), day.last_of_month().day());
            for (ours, chronos) in [
                (Weekday::Mon, chrono::Weekday::Mon),
                (Weekday::Sun, chrono::Weekday::Sun),
                (Weekday::Sat, chrono::Weekday::Sat),
            ] {
                assert_eq!(
                    naive(day.start_of_week(ours)),
                    theirs.week(chronos).first_day()
                );
            }
            let (iso_year, iso_week) = day.iso_week();
            assert_eq!(
                CivilDate::from_iso_week(iso_year, iso_week, day.weekday()),
                Ok(day)
            );
            compared += 1;
        }
    }
    assert_eq!(compared, 3 * 420);
}

#[test]
fn weekday_arithmetic_matches_chrono() {
    let chronos = [
        chrono::Weekday::Mon,
        chrono::Weekday::Tue,
        chrono::Weekday::Wed,
        chrono::Weekday::Thu,
        chrono::Weekday::Fri,
        chrono::Weekday::Sat,
        chrono::Weekday::Sun,
    ];
    for (ours, theirs) in Weekday::ALL.into_iter().zip(chronos) {
        assert_eq!(ours.num_days_from_monday(), theirs.num_days_from_monday());
        assert_eq!(ours.num_days_from_sunday(), theirs.num_days_from_sunday());
        assert_eq!(ours.number_from_monday(), theirs.number_from_monday());
        assert_eq!(ours.number_from_sunday(), theirs.number_from_sunday());
        assert_eq!(ours.succ().to_string(), theirs.succ().to_string());
        assert_eq!(ours.pred().to_string(), theirs.pred().to_string());
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
        for (other, their_other) in Weekday::ALL.into_iter().zip(chronos) {
            assert_eq!(ours.days_since(other), theirs.days_since(their_other));
        }
    }
}

#[test]
fn a_date_moves_by_the_whole_days_of_an_elapsed_time() {
    let day = date(2026, 11, 17);
    assert_eq!(day + Elapsed::days(14), date(2026, 12, 1));
    assert_eq!(day - Elapsed::days(17), date(2026, 10, 31));
    assert_eq!(day + Elapsed::hours(47), date(2026, 11, 18));
    assert_eq!(day + Elapsed::hours(-47), date(2026, 11, 16));
    assert_eq!(date(2026, 12, 1) - day, Elapsed::days(14));
    assert_eq!(day.days_since(date(2026, 12, 1)), -14);
    assert_eq!(day.checked_add_days(14), Some(date(2026, 12, 1)));
    assert_eq!(day.checked_sub_days(17), Some(date(2026, 10, 31)));
    for day in run(date(2023, 12, 25), 440) {
        for hours in [
            -10_000, -49, -48, -47, -24, -23, -1, 0, 1, 23, 24, 47, 48, 10_000,
        ] {
            let theirs = naive(day);
            assert_eq!(
                naive(day + Elapsed::hours(hours)),
                theirs + Duration::hours(hours)
            );
            assert_eq!(
                naive(day - Elapsed::hours(hours)),
                theirs - Duration::hours(hours)
            );
        }
        let other = date(2024, 6, 15);
        assert_eq!(
            (day - other).num_days(),
            (naive(day) - naive(other)).num_days()
        );
        assert_eq!(
            day.signed_duration_since(other).num_seconds(),
            naive(day).signed_duration_since(naive(other)).num_seconds()
        );
    }
    assert_eq!(civil(NaiveDate::default()), CivilDate::default());
}

#[test]
fn a_time_of_day_wraps_at_midnight_like_chrono() {
    let lengths = [
        Duration::hours(1),
        Duration::hours(-1),
        Duration::minutes(1_441),
        Duration::seconds(-86_401),
        Duration::milliseconds(1),
        Duration::nanoseconds(-1),
        Duration::days(400),
    ];
    for (ours, theirs) in pairs() {
        let (time, naive_time) = (ours.utc_time(), theirs.time());
        assert_eq!(
            time.num_seconds_from_midnight(),
            naive_time.num_seconds_from_midnight()
        );
        for length in lengths {
            let elapsed = Elapsed::nanoseconds(length.num_nanoseconds().unwrap());
            assert_eq!(
                (time + elapsed).to_string(),
                (naive_time + length).to_string()
            );
            assert_eq!(
                (time - elapsed).to_string(),
                (naive_time - length).to_string()
            );
        }
        let noon = CivilTime::from_hms(12, 0, 0).unwrap();
        assert_eq!(
            (time - noon).num_nanoseconds(),
            (naive_time - NaiveTime::from_hms_opt(12, 0, 0).unwrap()).num_nanoseconds()
        );
    }
    assert_eq!(CivilTime::default(), CivilTime::MIDNIGHT);
    assert_eq!(
        CivilTime::from_num_seconds_from_midnight(86_399, 5),
        CivilTime::from_hms_nano(23, 59, 59, 5)
    );
    assert_eq!(
        CivilTime::from_num_seconds_from_midnight(86_400, 0),
        Err(DateError::OutOfRange)
    );
    assert_eq!(CivilTime::from_hms(23, 59, 60), Err(DateError::OutOfRange));
    assert_eq!(
        CivilTime::from_hms_milli(1, 2, 3, 1_000),
        Err(DateError::OutOfRange)
    );
    assert_eq!(
        CivilTime::from_hms_micro(1, 2, 3, 4).unwrap().nanosecond(),
        4_000
    );
}

#[test]
fn a_date_time_moves_by_exact_time_like_chrono() {
    for (ours, theirs) in pairs() {
        let (local, naive_local) = (ours.utc_datetime(), theirs.naive_utc());
        for length in [
            Duration::days(1),
            Duration::days(-400),
            Duration::hours(25),
            Duration::nanoseconds(-1),
            Duration::milliseconds(86_400_001),
        ] {
            let elapsed = Elapsed::nanoseconds(length.num_nanoseconds().unwrap());
            assert_eq!(
                format!("{:?}", local + elapsed),
                format!("{:?}", naive_local + length)
            );
            assert_eq!(
                format!("{:?}", local - elapsed),
                format!("{:?}", naive_local - length)
            );
            assert_eq!((local + elapsed) - local, elapsed);
        }
        assert_eq!(CivilDateTime::new(local.date(), local.time()), local);
    }
    assert_eq!(
        format!("{:?}", CivilDateTime::default()),
        "1970-01-01T00:00:00"
    );
}

#[test]
fn nothing_panics_at_the_ends_of_the_range() {
    let day = Elapsed::days(1);
    assert_eq!(Timestamp::MAX + day, Timestamp::MAX);
    assert_eq!(Timestamp::MIN - day, Timestamp::MIN);
    assert_eq!(Timestamp::MAX - Elapsed::MIN, Timestamp::MAX);
    assert_eq!(Timestamp::MIN + Elapsed::MIN, Timestamp::MIN);
    assert_eq!(Timestamp::MAX.checked_add(day), None);
    assert_eq!(Timestamp::MIN.checked_sub(day), None);
    assert_eq!(
        Timestamp::MAX.checked_sub(day).map(Timestamp::to_rfc3339),
        Some("9999-12-29T22:00:00.999999999+00:00".to_owned())
    );
    assert_eq!(
        format!("{:?} {:?}", Timestamp::MIN, Timestamp::MAX),
        "-9999-01-02T01:59:59Z 9999-12-30T22:00:00.999999999Z"
    );
    // The longest difference there is fits an Elapsed.
    assert_eq!((Timestamp::MAX - Timestamp::MIN).num_days(), 7_304_481);

    assert_eq!(Elapsed::days(i64::MAX), Elapsed::MAX);
    assert_eq!(Elapsed::weeks(i64::MIN), Elapsed::MIN);
    assert_eq!(Elapsed::hours(i64::MAX), Elapsed::MAX);
    assert_eq!(Elapsed::minutes(i64::MIN), Elapsed::MIN);
    assert_eq!(Elapsed::try_days(i64::MAX), None);
    assert_eq!(Elapsed::try_weeks(i64::MIN), None);
    assert_eq!(Elapsed::try_hours(i64::MAX), None);
    assert_eq!(Elapsed::try_minutes(i64::MAX), None);
    assert_eq!(Elapsed::try_days(2), Some(Elapsed::hours(48)));
    assert_eq!(Elapsed::MAX + day, Elapsed::MAX);
    assert_eq!(Elapsed::MIN - day, Elapsed::MIN);
    assert_eq!(-Elapsed::MIN, Elapsed::MAX);
    assert_eq!(Elapsed::MIN.abs(), Elapsed::MAX);
    assert_eq!(Elapsed::MAX * 2, Elapsed::MAX);
    assert_eq!(Elapsed::MAX.checked_add(day), None);
    assert_eq!(Elapsed::MIN.checked_sub(day), None);
    assert_eq!(Elapsed::MAX.checked_mul(2), None);
    assert_eq!(Elapsed::MAX.num_milliseconds(), i64::MAX);
    assert_eq!(Elapsed::MAX.num_microseconds(), None);

    assert_eq!(CivilDate::MAX + day, CivilDate::MAX);
    assert_eq!(CivilDate::MIN - day, CivilDate::MIN);
    assert_eq!(CivilDate::MAX + Elapsed::MAX, CivilDate::MAX);
    assert_eq!(CivilDate::MAX.succ(), None);
    assert_eq!(CivilDate::MIN.pred(), None);
    assert_eq!(CivilDate::MAX.checked_add_days(i64::MAX), None);
    assert_eq!(CivilDate::MIN.checked_sub_days(i64::MIN), None);
    assert_eq!(CivilDate::MIN.start_of_week(Weekday::Sun), CivilDate::MIN);
    assert_eq!(
        CivilDate::from_ymd(10_000, 1, 1),
        Err(DateError::OutOfRange)
    );
    assert_eq!(CivilDate::from_ymd(2026, 13, 1), Err(DateError::OutOfRange));
    assert_eq!(CivilDate::from_ymd(2026, 2, 29), Err(DateError::OutOfRange));
    assert_eq!(CivilDateTime::MAX + day, CivilDateTime::MAX);
    assert_eq!(CivilDateTime::MIN - day, CivilDateTime::MIN);
    assert_eq!(CivilDateTime::MAX.checked_add(day), None);
    // The calendar reaches about a day further than the time line.
    assert_eq!(CivilDateTime::MAX.and_utc(), Err(DateError::OutOfRange));
    assert_eq!(CivilDateTime::MIN.and_utc(), Err(DateError::OutOfRange));
}

#[test]
fn a_timestamp_converts_to_and_from_the_system_clock_type() {
    let value = timestamp("2026-11-17T10:00:00.123456789Z");
    let system = value.to_system_time().unwrap();
    assert_eq!(Timestamp::try_from(system), Ok(value));
    assert_eq!(
        system.duration_since(UNIX_EPOCH).unwrap().as_nanos(),
        1_794_909_600_123_456_789
    );
    let now = Timestamp::now();
    let reference = Timestamp::try_from(SystemTime::now()).unwrap();
    assert!((reference - now).abs() < Elapsed::seconds(5));
}
