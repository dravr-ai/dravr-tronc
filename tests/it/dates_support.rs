// ABOUTME: Shared values for the date-type tests: one table of timestamps, built once as ours and as chrono's
// ABOUTME: chrono is a dev-dependency only; it is the reference every formatter and parser is compared with
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chrono::{DateTime, Utc};
use dravr_tronc::dates::Timestamp;

/// Seconds and nanoseconds since the Unix epoch. The fractions cover every
/// branch of chrono's digit rule (none, three, six, nine), and the dates
/// cover a leap day, a value before 1970, and years below 1000 and below 0,
/// where the year is padded or signed.
pub const VALUES: [(i64, u32); 16] = [
    (1_794_909_600, 0),
    (1_794_909_600, 100_000_000),
    (1_794_909_600, 120_000_000),
    (1_794_909_600, 123_000_000),
    (1_794_909_600, 123_400_000),
    (1_794_909_600, 123_456_000),
    (1_794_909_600, 123_456_789),
    (1_794_909_600, 1),
    (1_794_909_605, 999_999_999),
    (-14_182_940, 123_000_000),
    (1_709_251_199, 999_000_000),
    (0, 0),
    (-1, 500_000_000),
    (-62_135_596_800, 0),
    (-31_000_000_000, 250_000),
    (-63_600_000_000, 0),
];

/// The same timestamp as ours and as chrono's.
pub fn pair(seconds: i64, nanos: u32) -> (Timestamp, DateTime<Utc>) {
    (
        Timestamp::from_epoch_seconds_and_nanos(seconds, nanos).unwrap(),
        DateTime::from_timestamp(seconds, nanos).unwrap(),
    )
}

/// Every table value, as ours and as chrono's.
pub fn pairs() -> Vec<(Timestamp, DateTime<Utc>)> {
    VALUES.iter().map(|&(s, n)| pair(s, n)).collect()
}
