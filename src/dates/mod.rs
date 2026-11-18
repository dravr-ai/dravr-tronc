// ABOUTME: Dravr-owned date and time types: Timestamp, CivilDate, CivilTime, CivilDateTime, Weekday, Zone, Elapsed
// ABOUTME: The date library that does the work is private to this module and appears in no public signature
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Date and time types owned by Dravr.
//!
//! # Why these exist
//!
//! Every dravr service used to put `chrono` types in its public structs.
//! chrono and chrono-tz are no longer maintained, and chrono-tz's zone
//! database stopped at 2025b. Replacing them with another library's types
//! would have moved three contracts at once: the JSON bytes clients read,
//! the text and binary values stored in the databases, and the JSON schemas
//! of the MCP tools. It would also have put that library in the public API
//! of every satellite, so its next major release would be everyone's
//! breaking release.
//!
//! So the types are ours. A date library does the calendar and zone work
//! inside them, and **no public signature, field, trait bound or re-export
//! names it**: replacing or upgrading it is an internal change to this
//! crate. What the types promise instead is what the code already relied on:
//!
//! - **the same bytes out**: serde output, [`Timestamp::to_rfc3339`], the
//!   SQLite text and the PostgreSQL binary values are what chrono and sqlx's
//!   chrono support produced;
//! - **the same text in**: serde, `FromStr`, the strict RFC 3339 parser and
//!   the database decoders accept what chrono's accepted;
//! - **the same schemas**: `date-time`, `date`, `partial-date-time`,
//!   `partial-time`, and the weekday enum;
//! - **no panics**: every operation that can fail returns a `Result` or an
//!   `Option`, or stops at a documented bound.
//!
//! # The types
//!
//! | Type | Replaces | What it is |
//! |---|---|---|
//! | [`Timestamp`] | `DateTime<Utc>` | A point on the UTC time line |
//! | [`CivilDate`] | `NaiveDate` | A calendar date, `YYYY-MM-DD` |
//! | [`CivilTime`] | `NaiveTime` | A time of day |
//! | [`CivilDateTime`] | `NaiveDateTime` | A date and a time of day, no zone |
//! | [`Weekday`] | `Weekday` | A day of the week, `"Mon"` on the wire |
//! | `Zone` (feature `zones`) | `chrono_tz::Tz` | An IANA zone, found by name |
//! | [`Elapsed`] | `Duration` / `TimeDelta` | A signed, exact length of time |
//!
//! `DateTime<FixedOffset>` has no counterpart: keep the [`Timestamp`], and
//! where the sender's offset is read, keep it beside it
//! ([`Timestamp::parse_from_rfc3339_with_offset`]).
//!
//! # Features
//!
//! The seven types, their serde and their text forms are always compiled;
//! the rest is opt-in.
//!
//! | Feature | Adds |
//! |---|---|
//! | `zones` | `Zone` and `LocalResolution`, with the IANA zone database compiled into the binary |
//! | `sqlx08-sqlite` | sqlx 0.8 `Type` / `Encode` / `Decode` for SQLite |
//! | `sqlx08-postgres` | the same for PostgreSQL, and `PgHasArrayType` |
//! | `schemars1` | `schemars::JsonSchema` (also on with `computation`) |
//!
//! # Moving a field from chrono
//!
//! Change the type and keep the rest: the derives, the `.bind(x)` and
//! `row.get("col")` calls, and the stored and transmitted values all stay
//! as they are.
//!
//! ```
//! use dravr_tronc::dates::{CivilDate, Timestamp};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Goal {
//!     created_at: Timestamp,        // was DateTime<Utc>
//!     target_date: CivilDate,     // was NaiveDate
//!     achieved_at: Option<Timestamp>,
//! }
//!
//! let json = r#"{"created_at":"2026-11-17T10:00:00.100Z","target_date":"2027-04-25","achieved_at":null}"#;
//! let goal: Goal = serde_json::from_str(json).map_err(|e| e.to_string())?;
//! assert_eq!(serde_json::to_string(&goal).map_err(|e| e.to_string())?, json);
//! # Ok::<(), String>(())
//! ```
//!
//! At the call sites:
//!
//! | chrono | here |
//! |---|---|
//! | `Utc::now()` | [`Timestamp::now()`] |
//! | `.timestamp()`, `.timestamp_millis()` | [`Timestamp::epoch_seconds`], [`Timestamp::epoch_millis`] |
//! | `DateTime::from_timestamp(s, 0)` | [`Timestamp::from_epoch_seconds`] (a `Result`) |
//! | `.to_rfc3339()`, `.to_rfc3339_opts(..)` | the same names, the same bytes |
//! | `DateTime::parse_from_rfc3339(s)` | [`Timestamp::parse_from_rfc3339`] |
//! | `now - Duration::days(7)` | `now - Elapsed::days(7)` |
//! | `a.signed_duration_since(b).num_days()` | the same names |
//! | `.date_naive()` | [`Timestamp::utc_date`] |
//! | `.with_timezone(&tz).date_naive()` | `zone.date_at(timestamp)` |
//! | `date.and_hms_opt(h, m, s)?.and_utc()` | `date.and_hms(h, m, s)?.and_utc()?` |
//! | `tz.from_local_datetime(&dt).earliest()` | `zone.resolve(dt).earliest()` |
//! | `date + Months::new(1)` | [`CivilDate::checked_add_months`] |
//! | `"America/Toronto".parse::<Tz>()` | `"America/Toronto".parse::<Zone>()` |
//!
//! # What is deliberately different
//!
//! - **A timestamp has no calendar arithmetic.** `Timestamp + Elapsed` is exact
//!   seconds, and [`Elapsed::days`] is 86,400 of them, which is what
//!   `Duration::days` always meant. Adding a month, or a day that follows
//!   the local clock across a daylight-saving change, is done on a
//!   [`CivilDate`] and brought back through a `Zone`.
//! - **Operators stop at the bounds instead of panicking.** chrono's `+`
//!   panicked on overflow; here it returns the nearest bound, and
//!   `checked_add` reports the overflow.
//! - **The range is years −9999 to 9999.** chrono's reached ±262,142. A
//!   value outside the new range is rejected when read.
//! - **No leap second is held.** `23:59:60` is accepted where chrono
//!   accepted it and reads as `23:59:59`; chrono kept it and wrote `:60`
//!   back.
//! - **`format` returns a `Result`.** chrono panicked when a format string
//!   was wrong, at the moment the result was displayed.
//!
//! # Format strings
//!
//! `format` and `parse_from_str` take `strftime` strings and implement the
//! specifiers below with chrono's output and chrono's leniency. Any other
//! specifier is [`DateError::UnsupportedFormat`], never a silent difference.
//!
//! | Written and read | Written only |
//! |---|---|
//! | `%Y` `%m` `%d` `%e` `%H` `%k` `%I` `%l` `%M` `%S` `%f` `%.f` `%.3f` `%.6f` `%.9f` `%p` `%P` `%a` `%A` `%b` `%h` `%B` `%z` `%:z` `%F` `%T` `%R` `%n` `%t` `%%` | `%C` `%y` `%j` `%u` `%w` `%s` `%D` `%Z` |
//!
//! `%#z` is read only. `%-d`, `%_d` and `%0d` change the padding of a
//! numeric specifier. When read, a month or weekday name is matched in any
//! case, a weekday must be the date's own, and a 12-hour `%I` needs its
//! `%p`.

mod civil_date;
mod civil_datetime;
mod civil_time;
mod elapsed;
mod error;
#[cfg(feature = "schemars1")]
mod schema;
mod serde_impls;
#[cfg(feature = "sqlx08-postgres")]
mod sqlx_postgres;
#[cfg(feature = "sqlx08-sqlite")]
mod sqlx_sqlite;
mod text;
mod timestamp;
mod weekday;
#[cfg(feature = "zones")]
mod zone;

pub use civil_date::CivilDate;
pub use civil_datetime::CivilDateTime;
pub use civil_time::CivilTime;
pub use elapsed::Elapsed;
pub use error::DateError;
pub use timestamp::{SecondsFormat, Timestamp};
pub use weekday::Weekday;
#[cfg(feature = "zones")]
pub use zone::{LocalResolution, Zone};
