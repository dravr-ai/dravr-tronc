// ABOUTME: PostgreSQL encoding goldens: the bytes the date types send, written out and compared with sqlx's chrono encoder
// ABOUTME: No server is contacted: the argument buffer is filled in memory and its bytes are read
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use dravr_tronc::dates::{CivilDate, CivilDateTime, CivilTime, Timestamp};
use sqlx::encode::IsNull;
use sqlx::postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo};
use sqlx::{Encode, Postgres, Type, TypeInfo};

use crate::dates_support::{pair, pairs};

fn bytes<'q, T: Encode<'q, Postgres>>(value: &T) -> Vec<u8> {
    let mut buffer = PgArgumentBuffer::default();
    let is_null = value.encode_by_ref(&mut buffer).unwrap();
    assert!(matches!(is_null, IsNull::No));
    buffer.to_vec()
}

#[test]
fn the_bytes_sent_are_the_recorded_ones() {
    let (timestamp, _) = pair(1_794_909_600, 123_456_789);
    // 848,224,800,123,456 microseconds since 2000-01-01, big endian; the
    // 789 nanoseconds are dropped.
    assert_eq!(
        bytes(&timestamp),
        [0x00, 0x03, 0x03, 0x74, 0xb9, 0xe6, 0xaa, 0x40]
    );
    assert_eq!(
        bytes(&timestamp.utc_datetime()),
        [0x00, 0x03, 0x03, 0x74, 0xb9, 0xe6, 0xaa, 0x40]
    );
    // Day 9,817 since 2000-01-01.
    assert_eq!(bytes(&timestamp.utc_date()), [0x00, 0x00, 0x26, 0x59]);
    // 36,000,123,456 microseconds since midnight.
    assert_eq!(
        bytes(&timestamp.utc_time()),
        [0x00, 0x00, 0x00, 0x08, 0x61, 0xc6, 0x4a, 0x40]
    );
    assert_eq!(
        bytes(&Timestamp::parse_from_rfc3339("2000-01-01T00:00:00Z").unwrap()),
        [0; 8]
    );
    // One microsecond before the PostgreSQL epoch.
    assert_eq!(
        bytes(&Timestamp::parse_from_rfc3339("1999-12-31T23:59:59.999999Z").unwrap()),
        [0xff; 8]
    );
    assert_eq!(bytes(&Some(timestamp)), bytes(&timestamp));
    let mut buffer = PgArgumentBuffer::default();
    assert!(matches!(
        Encode::<Postgres>::encode_by_ref(&None::<Timestamp>, &mut buffer),
        Ok(IsNull::Yes)
    ));
}

#[test]
fn the_bytes_sent_match_sqlx_chrono_for_every_value() {
    let mut compared = 0;
    for (ours, theirs) in pairs() {
        let naive: NaiveDateTime = theirs.naive_utc();
        assert_eq!(bytes(&ours), bytes(&theirs), "{theirs:?}");
        assert_eq!(bytes(&ours.utc_datetime()), bytes(&naive), "{naive:?}");
        assert_eq!(bytes(&ours.utc_date()), bytes(&naive.date()), "{naive:?}");
        assert_eq!(bytes(&ours.utc_time()), bytes(&naive.time()), "{naive:?}");
        assert_eq!(
            Encode::<Postgres>::size_hint(&ours),
            Encode::<Postgres>::size_hint(&theirs)
        );
        assert_eq!(
            Encode::<Postgres>::size_hint(&ours.utc_date()),
            Encode::<Postgres>::size_hint(&naive.date())
        );
        compared += 1;
    }
    assert_eq!(compared, 16);
    // Values before the PostgreSQL epoch with a remainder below the
    // microsecond: the encoder cuts toward zero, on both sides.
    for (seconds, nanos) in [
        (946_684_799, 999_998_001),
        (-14_182_940, 123_456_789),
        (946_684_800, 1_999),
    ] {
        let (ours, theirs) = pair(seconds, nanos);
        assert_eq!(bytes(&ours), bytes(&theirs), "{theirs:?}");
        assert_eq!(bytes(&ours.utc_datetime()), bytes(&theirs.naive_utc()));
        assert_eq!(bytes(&ours.utc_time()), bytes(&theirs.time()));
    }
}

#[test]
fn the_declared_types_are_the_ones_sqlx_chrono_declares() {
    fn same<Ours: Type<Postgres> + PgHasArrayType, Theirs: Type<Postgres> + PgHasArrayType>(
        oid: u32,
        array_oid: u32,
    ) {
        let (ours, theirs) = (Ours::type_info(), Theirs::type_info());
        assert_eq!(ours, theirs);
        assert!(ours.type_eq(&theirs));
        assert_eq!(ours.oid().map(|o| o.0), Some(oid));
        assert_eq!(theirs.oid().map(|o| o.0), Some(oid));
        // What a column of that type reports is accepted by `row.get()`.
        assert!(Ours::compatible(&theirs));
        assert!(<Option<Ours> as Type<Postgres>>::compatible(&theirs));
        let (ours, theirs) = (Ours::array_type_info(), Theirs::array_type_info());
        assert_eq!(ours, theirs);
        assert_eq!(ours.oid().map(|o| o.0), Some(array_oid));
        assert!(<Vec<Ours> as Type<Postgres>>::compatible(&theirs));
    }
    same::<Timestamp, DateTime<Utc>>(1184, 1185);
    same::<CivilDateTime, NaiveDateTime>(1114, 1115);
    same::<CivilDate, NaiveDate>(1082, 1182);
    same::<CivilTime, NaiveTime>(1083, 1183);

    // A column of another type is still refused.
    let text = <String as Type<Postgres>>::type_info();
    assert_eq!(text.name(), "TEXT");
    assert!(!<Timestamp as Type<Postgres>>::compatible(&text));
    assert!(!<CivilDate as Type<Postgres>>::compatible(
        &<DateTime<Utc> as Type<Postgres>>::type_info()
    ));
    assert!(
        !<Timestamp as Type<Postgres>>::type_info().type_eq(&PgTypeInfo::with_name("timestamp"))
    );
}

#[test]
fn an_array_of_timestamps_encodes_like_an_array_of_chrono_values() {
    let all = pairs();
    let ours: Vec<Timestamp> = all.iter().map(|(o, _)| *o).collect();
    let theirs: Vec<DateTime<Utc>> = all.iter().map(|(_, t)| *t).collect();
    let encoded = bytes(&ours);
    assert_eq!(encoded, bytes(&theirs));
    // One dimension, no nulls, element type timestamptz (1184), 16 elements.
    assert_eq!(
        encoded[..20],
        [0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0x04, 0xa0, 0, 0, 0, 16, 0, 0, 0, 1]
    );
    let days: Vec<CivilDate> = ours.iter().map(|i| i.utc_date()).collect();
    let naive_days: Vec<NaiveDate> = theirs.iter().map(DateTime::date_naive).collect();
    assert_eq!(bytes(&days), bytes(&naive_days));
}
