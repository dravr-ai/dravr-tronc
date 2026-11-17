// ABOUTME: SQLite storage goldens through real sqlx: rows written by sqlx's chrono support and by the date types
// ABOUTME: Pins the stored text, proves each side decodes the other's rows, and covers every shape sqlx reads
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use dravr_tronc::time::{CivilDate, CivilDateTime, CivilTime, Elapsed, Instant};
use sqlx::sqlite::SqliteConnection;
use sqlx::{Connection, Row};

use crate::time_support::{pair, pairs};

const SCHEMA: &str = "CREATE TABLE stamps (
    id INTEGER PRIMARY KEY,
    writer TEXT NOT NULL,
    at_text TEXT,
    at_datetime DATETIME,
    at_timestamp TIMESTAMP,
    local_text TEXT,
    local_datetime DATETIME,
    day_text TEXT,
    day_date DATE,
    clock_text TEXT,
    clock_time TIME,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    stamped_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
)";

const INSERT: &str = "INSERT INTO stamps
    (writer, at_text, at_datetime, at_timestamp, local_text, local_datetime,
     day_text, day_date, clock_text, clock_time)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

/// Every stored column read back as SQLite holds it: raw text.
const RAW: &str = "SELECT
    CAST(at_text AS TEXT), CAST(at_datetime AS TEXT), CAST(at_timestamp AS TEXT),
    CAST(local_text AS TEXT), CAST(local_datetime AS TEXT),
    CAST(day_text AS TEXT), CAST(day_date AS TEXT),
    CAST(clock_text AS TEXT), CAST(clock_time AS TEXT),
    typeof(at_text), typeof(at_datetime), typeof(day_date), typeof(clock_time)
    FROM stamps WHERE writer = ? ORDER BY id";

async fn database() -> SqliteConnection {
    let mut connection = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    sqlx::query(SCHEMA).execute(&mut connection).await.unwrap();
    connection
}

async fn write_with_chrono(connection: &mut SqliteConnection, value: DateTime<Utc>) {
    let local = value.naive_utc();
    sqlx::query(INSERT)
        .bind("chrono")
        .bind(value)
        .bind(value)
        .bind(value)
        .bind(local)
        .bind(local)
        .bind(local.date())
        .bind(local.date())
        .bind(local.time())
        .bind(local.time())
        .execute(connection)
        .await
        .unwrap();
}

async fn write_with_ours(connection: &mut SqliteConnection, value: Instant) {
    let local = value.utc_datetime();
    sqlx::query(INSERT)
        .bind("ours")
        .bind(value)
        .bind(value)
        .bind(value)
        .bind(local)
        .bind(local)
        .bind(local.date())
        .bind(local.date())
        .bind(local.time())
        .bind(local.time())
        .execute(connection)
        .await
        .unwrap();
}

async fn raw_rows(connection: &mut SqliteConnection, writer: &str) -> Vec<Vec<String>> {
    sqlx::query(RAW)
        .bind(writer)
        .fetch_all(connection)
        .await
        .unwrap()
        .iter()
        .map(|row| (0..13).map(|column| row.get::<String, _>(column)).collect())
        .collect()
}

#[tokio::test]
async fn the_stored_text_is_what_sqlx_chrono_stores() {
    let mut connection = database().await;
    for (ours, theirs) in pairs() {
        write_with_chrono(&mut connection, theirs).await;
        write_with_ours(&mut connection, ours).await;
    }
    let from_chrono = raw_rows(&mut connection, "chrono").await;
    let from_ours = raw_rows(&mut connection, "ours").await;
    assert_eq!(from_chrono.len(), 16);
    assert_eq!(from_ours, from_chrono);

    // The second table value, written out: `+00:00`, never `Z`, and the
    // fraction padded to three digits.
    assert_eq!(
        from_ours[1],
        [
            "2026-11-17T10:00:00.100+00:00",
            "2026-11-17T10:00:00.100+00:00",
            "2026-11-17T10:00:00.100+00:00",
            "2026-11-17 10:00:00.100",
            "2026-11-17 10:00:00.100",
            "2026-11-17",
            "2026-11-17",
            "10:00:00.100",
            "10:00:00.100",
            "text",
            "text",
            "text",
            "text",
        ]
    );
    assert_eq!(from_ours[0][0], "2026-11-17T10:00:00+00:00");
    assert_eq!(from_ours[0][3], "2026-11-17 10:00:00");
    assert_eq!(from_ours[6][0], "2026-11-17T10:00:00.123456789+00:00");
}

#[tokio::test]
async fn each_side_decodes_the_rows_the_other_wrote() {
    let mut connection = database().await;
    let all = pairs();
    for (ours, theirs) in &all {
        write_with_chrono(&mut connection, *theirs).await;
        write_with_ours(&mut connection, *ours).await;
    }
    let select = "SELECT at_text, at_datetime, at_timestamp, local_text, local_datetime,
        day_text, day_date, clock_text, clock_time FROM stamps WHERE writer = ? ORDER BY id";

    // Rows chrono wrote, read with the date types and no type named in the
    // call: the binding of the result decides, as it does in the repositories.
    let rows = sqlx::query(select)
        .bind("chrono")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert_eq!(rows.len(), all.len());
    for (row, (ours, _)) in rows.iter().zip(&all) {
        for column in ["at_text", "at_datetime", "at_timestamp"] {
            let read: Instant = row.get(column);
            assert_eq!(read, *ours, "{column}");
            let optional: Option<Instant> = row.get(column);
            assert_eq!(optional, Some(*ours));
        }
        for column in ["local_text", "local_datetime"] {
            let read: CivilDateTime = row.get(column);
            assert_eq!(read, ours.utc_datetime(), "{column}");
        }
        for column in ["day_text", "day_date"] {
            let read: CivilDate = row.get(column);
            assert_eq!(read, ours.utc_date(), "{column}");
        }
        for column in ["clock_text", "clock_time"] {
            let read: CivilTime = row.get(column);
            assert_eq!(read, ours.utc_time(), "{column}");
        }
    }

    // Rows the date types wrote, read with chrono.
    let rows = sqlx::query(select)
        .bind("ours")
        .fetch_all(&mut connection)
        .await
        .unwrap();
    assert_eq!(rows.len(), all.len());
    for (row, (_, theirs)) in rows.iter().zip(&all) {
        let local = theirs.naive_utc();
        assert_eq!(row.get::<DateTime<Utc>, _>("at_text"), *theirs);
        assert_eq!(row.get::<DateTime<Utc>, _>("at_datetime"), *theirs);
        assert_eq!(row.get::<DateTime<Utc>, _>("at_timestamp"), *theirs);
        assert_eq!(row.get::<NaiveDateTime, _>("local_text"), local);
        assert_eq!(row.get::<NaiveDateTime, _>("local_datetime"), local);
        assert_eq!(row.get::<NaiveDate, _>("day_date"), local.date());
        assert_eq!(row.get::<NaiveTime, _>("clock_time"), local.time());
    }

    // A tuple through FromRow, and a scalar: neither names a column type.
    let (at, day): (Instant, CivilDate) =
        sqlx::query_as("SELECT at_datetime, day_date FROM stamps WHERE id = 1")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!((at, day), (all[0].0, all[0].0.utc_date()));
    let latest: Instant =
        sqlx::query_scalar("SELECT MAX(at_text) FROM stamps WHERE at_text LIKE '2026%'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(latest, pair(1_794_909_605, 999_999_999).0);
}

#[tokio::test]
async fn null_columns_and_database_defaults_decode() {
    let mut connection = database().await;
    sqlx::query("INSERT INTO stamps (writer, at_text, at_datetime) VALUES ('none', ?, ?)")
        .bind(None::<Instant>)
        .bind(None::<Instant>)
        .execute(&mut connection)
        .await
        .unwrap();
    let row =
        sqlx::query("SELECT at_text, at_datetime, day_date, created_at, stamped_at FROM stamps")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(row.get::<Option<Instant>, _>("at_text"), None);
    assert_eq!(row.get::<Option<Instant>, _>("at_datetime"), None);
    assert_eq!(row.get::<Option<CivilDate>, _>("day_date"), None);
    assert!(row.try_get::<Instant, _>("at_datetime").is_err());

    // `datetime('now')` and CURRENT_TIMESTAMP write `YYYY-MM-DD HH:MM:SS`
    // with no offset; it is read as UTC, by both.
    for column in ["created_at", "stamped_at"] {
        let raw: String = row.get(column);
        assert_eq!(raw.len(), 19, "{raw}");
        let ours: Instant = row.get(column);
        let theirs: DateTime<Utc> = row.get(column);
        assert_eq!(ours.epoch_seconds(), theirs.timestamp());
        assert_eq!(ours.format("%Y-%m-%d %H:%M:%S"), Ok(raw));
        assert!((Instant::now() - ours).abs() < Elapsed::seconds(60));
        let local: CivilDateTime = row.get(column);
        assert_eq!(local, ours.utc_datetime());
    }
}

/// Text a date-time column may hold: each shape sqlx's decoder knows, and
/// text it must reject.
const DATETIME_TEXT: [&str; 34] = [
    "2026-11-17T10:00:00+00:00",
    "2026-11-17T10:00:00.100+00:00",
    "2026-11-17T10:00:00Z",
    "2026-11-17T12:00:00.5+02:00",
    "2026-11-17 10:00:00",
    "2026-11-17 10:00:00.123456",
    "2026-11-17 10:00",
    "2026-11-17 10:00Z",
    "2026-11-17 10:00+02:00",
    "2026-11-17 10:00:00Z",
    "2026-11-17 10:00:00.25Z",
    "2026-11-17 10:00:00+02:00",
    "2026-11-17 10:00:00.25-05:30",
    "2026-11-17T10:00",
    "2026-11-17T10:00Z",
    "2026-11-17T10:00+02:00",
    "2026-11-17T10:00:00",
    "2026-11-17T10:00:00.999999999",
    "2026-11-17T10:00:00.5Z",
    "2026-11-17T10:00:00 +0200",
    "2026-11-17  10:00:00",
    "2026-11-1710:00:00",
    "2026-1-7 1:2:3",
    "2026-11-17 23:59:60",
    "2026-11-17 10:00:00+24:00",
    "2026-11-17 10:00:00 UTC",
    "2026-11-17",
    "10:00:00",
    "",
    "not a date",
    "2026-02-30 10:00:00",
    "2026-11-17 24:00:00",
    "1794909600",
    "2026-11-17t10:00:00z",
];

#[tokio::test]
async fn every_text_shape_decodes_as_sqlx_chrono_decodes_it() {
    let mut connection = database().await;
    let (mut accepted, mut rejected) = (0, 0);
    for text in DATETIME_TEXT {
        let row = sqlx::query("SELECT ? AS value")
            .bind(text)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        let theirs = row.try_get::<DateTime<Utc>, _>("value").ok();
        let ours = row.try_get::<Instant, _>("value").ok();
        assert_eq!(
            ours.map(|i| (i.epoch_seconds(), i.subsec_nanos())),
            theirs.map(|d| (d.timestamp(), d.timestamp_subsec_nanos() % 1_000_000_000)),
            "instant {text:?}"
        );
        let naive = row.try_get::<NaiveDateTime, _>("value").ok();
        let local = row.try_get::<CivilDateTime, _>("value").ok();
        assert_eq!(
            local.map(|l| format!("{l:?}")),
            naive.map(|n| format!("{n:?}").replace(":60", ":59")),
            "local {text:?}"
        );
        if theirs.is_some() {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    assert_eq!((accepted, rejected), (26, 8));

    // The offset-less shape means UTC; a stored offset is applied for an
    // instant and kept for a local reading.
    let read = |text: &'static str| async move {
        let mut connection = database().await;
        let row = sqlx::query("SELECT ? AS value")
            .bind(text)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        (
            row.get::<Instant, _>("value").to_rfc3339(),
            format!("{:?}", row.get::<CivilDateTime, _>("value")),
        )
    };
    assert_eq!(
        read("2026-11-17 10:00:00").await,
        (
            "2026-11-17T10:00:00+00:00".to_owned(),
            "2026-11-17T10:00:00".to_owned()
        )
    );
    assert_eq!(
        read("2026-11-17 12:00:00.5+02:00").await,
        (
            "2026-11-17T10:00:00.500+00:00".to_owned(),
            "2026-11-17T12:00:00.500".to_owned()
        )
    );
}

#[tokio::test]
async fn integers_and_reals_decode_as_sqlx_chrono_decodes_them() {
    let mut connection = database().await;
    for sql in [
        "SELECT 1794909600 AS value",
        "SELECT 0 AS value",
        "SELECT -14182940 AS value",
        "SELECT 2461362.9166666665 AS value",
        "SELECT 2440587.5 AS value",
        "SELECT 2440587.25 AS value",
        "SELECT julianday('2026-11-17 10:00:00') AS value",
        "SELECT unixepoch('2026-11-17 10:00:00') AS value",
        "SELECT 1e300 AS value",
        "SELECT 9223372036854775807 AS value",
        "SELECT x'00' AS value",
    ] {
        let row = sqlx::query(sql).fetch_one(&mut connection).await.unwrap();
        let theirs = row.try_get::<DateTime<Utc>, _>("value").ok();
        let ours = row.try_get::<Instant, _>("value").ok();
        assert_eq!(
            ours.map(|i| (i.epoch_seconds(), i.subsec_nanos())),
            theirs.map(|d| (d.timestamp(), d.timestamp_subsec_nanos())),
            "{sql}"
        );
    }
    let row = sqlx::query("SELECT 1794909600 AS seconds, x'00' AS blob, 1e300 AS huge")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(row.get::<Instant, _>("seconds"), pair(1_794_909_600, 0).0);
    assert!(row.try_get::<Instant, _>("blob").is_err());
    assert!(row.try_get::<Instant, _>("huge").is_err());
    assert!(row.try_get::<CivilDate, _>("seconds").is_err());
}

#[tokio::test]
async fn dates_and_times_of_day_decode_as_sqlx_chrono_decodes_them() {
    let mut connection = database().await;
    for text in [
        "2026-11-17",
        "2026-1-7",
        "2026-11-17 ",
        "2026-11-17T10:00:00",
        "2026-02-30",
        "",
        "10:00:00",
        "10:00:00.f",
        "10:00:00.250",
        "10:00",
        "10:00Z",
        "10:00:00.5Z",
        "10:00+02:00",
        "10:00:00.5+02:00",
        "1:2:3",
        "24:00:00",
        "10:00:00 ",
        "noon",
    ] {
        let row = sqlx::query("SELECT ? AS value")
            .bind(text)
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(
            row.try_get::<CivilDate, _>("value")
                .ok()
                .map(|d| d.to_string()),
            row.try_get::<NaiveDate, _>("value")
                .ok()
                .map(|d| d.to_string()),
            "date {text:?}"
        );
        assert_eq!(
            row.try_get::<CivilTime, _>("value")
                .ok()
                .map(|t| t.to_string()),
            row.try_get::<NaiveTime, _>("value")
                .ok()
                .map(|t| t.to_string()),
            "time {text:?}"
        );
    }
    let row = sqlx::query("SELECT '2026-11-17' AS day, '10:00:00.250' AS clock")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!(
        row.get::<CivilDate, _>("day"),
        CivilDate::from_ymd(2026, 11, 17).unwrap()
    );
    assert_eq!(
        row.get::<CivilTime, _>("clock"),
        CivilTime::from_hms_milli(10, 0, 0, 250).unwrap()
    );
}

#[tokio::test]
async fn stored_text_orders_and_compares_as_before() {
    // Range filters compare the stored text, so a bound must have the
    // stored shape: it does, whichever side binds it.
    let mut connection = database().await;
    for (ours, theirs) in pairs() {
        write_with_chrono(&mut connection, theirs).await;
        write_with_ours(&mut connection, ours).await;
    }
    let (bound, chrono_bound) = pair(1_794_909_600, 123_000_000);
    let count = "SELECT COUNT(*) FROM stamps WHERE at_text > ?";
    let with_ours: i64 = sqlx::query_scalar(count)
        .bind(bound)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let with_chrono: i64 = sqlx::query_scalar(count)
        .bind(chrono_bound)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    assert_eq!((with_ours, with_chrono), (8, 8));
}
