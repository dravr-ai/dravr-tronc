// ABOUTME: Zone goldens: named zones, local days, offsets, gaps and folds, compared with chrono-tz
// ABOUTME: Uses zones and years whose rules are the same in chrono-tz's 2025b database and the compiled-in one
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashSet;

use chrono::offset::LocalResult;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::{Tz, TZ_VARIANTS};
use dravr_tronc::time::{
    CivilDate, CivilDateTime, Elapsed, Instant, LocalResolution, TimeError, Zone,
};

/// Zones whose rules did not change between 2025b and today, with and
/// without daylight saving, north and south, and with a half-hour and a
/// quarter-hour offset.
const STABLE_ZONES: [&str; 9] = [
    "UTC",
    "America/Toronto",
    "America/Los_Angeles",
    "Europe/Paris",
    "Europe/London",
    "Australia/Sydney",
    "Asia/Kolkata",
    "Asia/Kathmandu",
    "Asia/Tokyo",
];

fn instant(text: &str) -> Instant {
    Instant::parse_from_rfc3339(text).unwrap()
}

fn date(year: i32, month: u32, day: u32) -> CivilDate {
    CivilDate::from_ymd(year, month, day).unwrap()
}

fn ours_of(value: DateTime<Utc>) -> Instant {
    Instant::from_epoch_seconds_and_nanos(value.timestamp(), value.timestamp_subsec_nanos())
        .unwrap()
}

fn civil_of(value: NaiveDateTime) -> CivilDateTime {
    format!("{value:?}").parse().unwrap()
}

#[test]
fn every_chrono_tz_zone_name_is_a_zone() {
    let mut found = 0;
    for tz in TZ_VARIANTS {
        let zone = Zone::from_name(tz.name()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(zone.name(), tz.name());
        assert_eq!(zone.to_string(), tz.to_string());
        assert_eq!(tz.name().parse::<Zone>(), Ok(zone));
        found += 1;
    }
    assert!(found > 590, "only {found} zones");
}

#[test]
fn a_zone_is_found_by_its_exact_name_only() {
    let unknown = |name: &str| Err(TimeError::UnknownZone(name.to_owned()));
    for name in [
        "america/toronto",
        "AMERICA/TORONTO",
        "utc",
        "Etc/Unknown",
        "Mars/Olympus_Mons",
        "",
        " America/Toronto",
        "America/Toronto ",
        "EST5EDT4",
        "+05:00",
        "Am\u{e9}rica/Toronto",
    ] {
        assert_eq!(Zone::from_name(name), unknown(name), "{name:?}");
        assert!(name.parse::<Tz>().is_err(), "chrono-tz accepts {name:?}");
    }
    assert_eq!(
        Zone::from_name("nowhere").unwrap_err().to_string(),
        "unknown time zone `nowhere`"
    );
}

#[test]
fn zones_compare_and_hash_by_name() {
    let toronto = Zone::from_name("America/Toronto").unwrap();
    assert_eq!(toronto, Zone::from_name("America/Toronto").unwrap());
    // An alias has the same rules and is still another zone, as with Tz.
    assert_ne!(
        Zone::from_name("US/Eastern").unwrap(),
        Zone::from_name("America/New_York").unwrap()
    );
    assert_ne!(Tz::US__Eastern, Tz::America__New_York);
    let set: HashSet<Zone> = [toronto.clone(), toronto.clone(), Zone::utc()].into();
    assert_eq!(set.len(), 2);
    assert_eq!(Zone::utc(), Zone::from_name("UTC").unwrap());
    assert_eq!(
        format!("{toronto:?} {toronto}"),
        "America/Toronto America/Toronto"
    );
}

#[test]
fn the_local_day_and_offset_are_the_recorded_ones() {
    let toronto = Zone::from_name("America/Toronto").unwrap();
    // 03:30 UTC on 1 July is still 30 June in Toronto.
    let summer = instant("2026-07-01T03:30:00Z");
    assert_eq!(toronto.date_at(summer), date(2026, 6, 30));
    assert_eq!(
        format!("{:?}", toronto.civil_at(summer)),
        "2026-06-30T23:30:00"
    );
    assert_eq!(toronto.offset_seconds_at(summer), -14_400);
    assert_eq!(toronto.abbreviation_at(summer), "EDT");
    let winter = instant("2026-01-15T03:30:00Z");
    assert_eq!(toronto.date_at(winter), date(2026, 1, 14));
    assert_eq!(toronto.offset_seconds_at(winter), -18_000);
    assert_eq!(toronto.abbreviation_at(winter), "EST");
    // The local date across the spring gap and the autumn overlap.
    assert_eq!(
        toronto.date_at(instant("2026-03-08T06:59:59Z")),
        date(2026, 3, 8)
    );
    assert_eq!(
        format!("{:?}", toronto.civil_at(instant("2026-03-08T07:00:00Z"))),
        "2026-03-08T03:00:00"
    );
    assert_eq!(
        format!("{:?}", toronto.civil_at(instant("2026-11-01T05:30:00Z"))),
        "2026-11-01T01:30:00"
    );
    assert_eq!(
        format!("{:?}", toronto.civil_at(instant("2026-11-01T06:30:00Z"))),
        "2026-11-01T01:30:00"
    );
    let kathmandu = Zone::from_name("Asia/Kathmandu").unwrap();
    assert_eq!(kathmandu.offset_seconds_at(summer), 20_700);
    assert_eq!(
        kathmandu.date_at(instant("2026-06-30T18:15:00Z")),
        date(2026, 7, 1)
    );
    assert_eq!(Zone::utc().civil_at(summer), summer.utc_datetime());
}

#[test]
fn local_time_matches_chrono_tz_through_two_years() {
    let mut compared = 0;
    for name in STABLE_ZONES {
        let (zone, tz): (Zone, Tz) = (name.parse().unwrap(), name.parse().unwrap());
        let mut at = DateTime::parse_from_rfc3339("2024-01-01T00:17:00Z")
            .unwrap()
            .to_utc();
        let end = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .to_utc();
        while at < end {
            let (ours, local) = (ours_of(at), at.with_timezone(&tz));
            assert_eq!(
                zone.civil_at(ours),
                civil_of(local.naive_local()),
                "{name} {at}"
            );
            assert_eq!(
                zone.date_at(ours).to_string(),
                local.date_naive().to_string()
            );
            assert_eq!(
                zone.offset_seconds_at(ours),
                local.offset().fix().local_minus_utc(),
                "{name} {at}"
            );
            for format in ["%Y-%m-%d %H:%M:%S %Z %z %:z", "%a %-d %b %-I:%M %p", "%s"] {
                assert_eq!(
                    zone.format(ours, format),
                    Ok(local.format(format).to_string()),
                    "{name} {at} {format}"
                );
            }
            at += Duration::minutes(7 * 60 + 1);
            compared += 1;
        }
    }
    assert!(compared > 20_000, "compared {compared}");
}

fn resolution_of(result: LocalResult<DateTime<Tz>>) -> LocalResolution {
    match result {
        LocalResult::None => LocalResolution::None,
        LocalResult::Single(only) => LocalResolution::Single(ours_of(only.to_utc())),
        LocalResult::Ambiguous(first, second) => {
            LocalResolution::Ambiguous(ours_of(first.to_utc()), ours_of(second.to_utc()))
        }
    }
}

#[test]
fn gaps_and_folds_resolve_to_the_recorded_instants() {
    let toronto = Zone::from_name("America/Toronto").unwrap();
    let at = |day: CivilDate, h, m| day.and_hms(h, m, 0).unwrap();

    // Clocks go from 02:00 to 03:00: 02:30 does not exist.
    let gap = toronto.resolve(at(date(2026, 3, 8), 2, 30));
    assert_eq!(gap, LocalResolution::None);
    assert_eq!(
        (gap.single(), gap.earliest(), gap.latest()),
        (None, None, None)
    );

    // Clocks go from 02:00 back to 01:00: 01:30 happens twice.
    let fold = toronto.resolve(at(date(2026, 11, 1), 1, 30));
    let (first, second) = (
        instant("2026-11-01T05:30:00Z"),
        instant("2026-11-01T06:30:00Z"),
    );
    assert_eq!(fold, LocalResolution::Ambiguous(first, second));
    assert_eq!(
        (fold.single(), fold.earliest(), fold.latest()),
        (None, Some(first), Some(second))
    );

    let plain = toronto.resolve(at(date(2026, 11, 17), 5, 0));
    let only = instant("2026-11-17T10:00:00Z");
    assert_eq!(plain, LocalResolution::Single(only));
    assert_eq!(
        (plain.single(), plain.earliest(), plain.latest()),
        (Some(only), Some(only), Some(only))
    );
    // The edges of the gap and of the fold.
    assert_eq!(
        toronto.resolve(at(date(2026, 3, 8), 1, 59)).single(),
        Some(instant("2026-03-08T06:59:00Z"))
    );
    assert_eq!(
        toronto.resolve(at(date(2026, 3, 8), 2, 0)),
        LocalResolution::None
    );
    assert_eq!(
        toronto.resolve(at(date(2026, 3, 8), 3, 0)).single(),
        Some(instant("2026-03-08T07:00:00Z"))
    );
    assert_eq!(
        toronto.resolve(at(date(2026, 11, 1), 2, 0)).single(),
        Some(instant("2026-11-01T07:00:00Z"))
    );
    assert_eq!(
        Zone::utc().resolve(CivilDateTime::MAX),
        LocalResolution::None
    );
}

#[test]
fn gaps_and_folds_match_chrono_tz_around_every_transition() {
    let transitions = [
        ("America/Toronto", (2025, 3, 9)),
        ("America/Toronto", (2025, 11, 2)),
        ("America/Los_Angeles", (2025, 3, 9)),
        ("America/Los_Angeles", (2025, 11, 2)),
        ("Europe/Paris", (2025, 3, 30)),
        ("Europe/Paris", (2025, 10, 26)),
        ("Europe/London", (2024, 3, 31)),
        ("Europe/London", (2024, 10, 27)),
        ("Australia/Sydney", (2025, 4, 6)),
        ("Australia/Sydney", (2025, 10, 5)),
        ("Asia/Kolkata", (2025, 6, 1)),
        ("America/Sao_Paulo", (2018, 11, 4)),
        ("America/Havana", (2024, 11, 3)),
        ("America/Havana", (2024, 3, 10)),
    ];
    let (mut none, mut twice, mut once) = (0, 0, 0);
    for (name, (year, month, day)) in transitions {
        let (zone, tz): (Zone, Tz) = (name.parse().unwrap(), name.parse().unwrap());
        let start = NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            - Duration::hours(12);
        for step in 0..(48 * 12) {
            let local = start + Duration::minutes(step * 5);
            let expected = resolution_of(tz.from_local_datetime(&local));
            assert_eq!(zone.resolve(civil_of(local)), expected, "{name} {local}");
            match expected {
                LocalResolution::None => none += 1,
                LocalResolution::Single(_) => once += 1,
                LocalResolution::Ambiguous(..) => twice += 1,
            }
        }
    }
    // Each kind of answer was actually exercised.
    assert_eq!(none + twice + once, 14 * 576);
    assert!(none >= 70 && twice >= 70, "gaps {none}, folds {twice}");
}

#[test]
fn the_start_of_a_day_is_its_first_instant_even_without_a_midnight() {
    let toronto = Zone::from_name("America/Toronto").unwrap();
    assert_eq!(
        toronto.start_of_day(date(2026, 11, 17)),
        Ok(instant("2026-11-17T05:00:00Z"))
    );
    assert_eq!(
        toronto.start_of_day(date(2026, 7, 1)),
        Ok(instant("2026-07-01T04:00:00Z"))
    );
    // A day with a clock change is 23 or 25 hours long.
    let length = |day: CivilDate| {
        toronto.start_of_day(day.succ().unwrap()).unwrap() - toronto.start_of_day(day).unwrap()
    };
    assert_eq!(length(date(2026, 3, 8)), Elapsed::hours(23));
    assert_eq!(length(date(2026, 11, 1)), Elapsed::hours(25));
    assert_eq!(length(date(2026, 11, 17)), Elapsed::hours(24));

    // In 2018 Brazil went forward at midnight: 4 November had no 00:00,
    // which chrono-tz reports as no instant at all.
    let sao_paulo = Zone::from_name("America/Sao_Paulo").unwrap();
    let no_midnight = date(2018, 11, 4);
    assert_eq!(
        sao_paulo.resolve(no_midnight.at_midnight()),
        LocalResolution::None
    );
    assert_eq!(
        sao_paulo.start_of_day(no_midnight),
        Ok(instant("2018-11-04T03:00:00Z"))
    );
    assert_eq!(
        format!("{:?}", sao_paulo.civil_at(instant("2018-11-04T03:00:00Z"))),
        "2018-11-04T01:00:00"
    );
    // Cuba goes back at 01:00 to 00:00: midnight happens twice, and the day
    // starts at the first.
    let havana = Zone::from_name("America/Havana").unwrap();
    assert_eq!(
        havana.resolve(date(2024, 11, 3).at_midnight()),
        LocalResolution::Ambiguous(
            instant("2024-11-03T04:00:00Z"),
            instant("2024-11-03T05:00:00Z")
        )
    );
    assert_eq!(
        havana.start_of_day(date(2024, 11, 3)),
        Ok(instant("2024-11-03T04:00:00Z"))
    );
    assert_eq!(
        Zone::utc().start_of_day(CivilDate::MIN),
        Err(TimeError::OutOfRange)
    );

    // Where midnight exists once, it is what chrono-tz resolved.
    for name in STABLE_ZONES {
        let (zone, tz): (Zone, Tz) = (name.parse().unwrap(), name.parse().unwrap());
        let mut day = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        for _ in 0..730 {
            let midnight = tz.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap());
            let ours: CivilDate = day.to_string().parse().unwrap();
            assert_eq!(
                zone.start_of_day(ours).ok(),
                midnight.earliest().map(|at| ours_of(at.to_utc())),
                "{name} {day}"
            );
            day = day.succ_opt().unwrap();
        }
    }
}

#[test]
fn a_zone_formats_an_instant_as_its_clocks_show_it() {
    let paris = Zone::from_name("Europe/Paris").unwrap();
    let at = instant("2026-07-14T10:00:00.250Z");
    assert_eq!(
        paris.format(at, "%Y-%m-%d %H:%M:%S%.3f %Z %z %:z"),
        Ok("2026-07-14 12:00:00.250 CEST +0200 +02:00".to_owned())
    );
    assert_eq!(
        paris.format(at, "%A %-d %B, %H:%M"),
        Ok("Tuesday 14 July, 12:00".to_owned())
    );
    assert_eq!(
        paris.format(at, "%U"),
        Err(TimeError::UnsupportedFormat("%U".to_owned()))
    );
}
