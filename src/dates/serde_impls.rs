// ABOUTME: serde for the date types: each one is a JSON string, in the bytes chrono's default serde writes
// ABOUTME: Reading goes through the type's FromStr, with chrono's leniency and chrono's rejection messages
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{CivilDate, CivilDateTime, CivilTime, SecondsFormat, Timestamp, Weekday};

/// Reads a string and hands it to the type's `FromStr`.
struct TextVisitor<T> {
    /// What the error for a value that is not a string says was expected:
    /// chrono's visitor's words.
    expecting: &'static str,
    /// The message for a string that is not a value, when chrono's visitor
    /// had its own instead of the parse error's.
    rejection: Option<&'static str>,
    target: PhantomData<T>,
}

impl<T> Visitor<'_> for TextVisitor<T>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.expecting)
    }

    fn visit_str<E: DeError>(self, text: &str) -> Result<T, E> {
        text.parse()
            .map_err(|error: T::Err| self.rejection.map_or_else(|| E::custom(error), E::custom))
    }
}

macro_rules! text_serde {
    ($type:ty, $expecting:literal, |$value:ident| $text:expr) => {
        text_serde!($type, $expecting, None, |$value| $text);
    };
    ($type:ty, $expecting:literal, $rejection:expr, |$value:ident| $text:expr) => {
        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let $value = self;
                serializer.collect_str(&$text)
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                deserializer.deserialize_str(TextVisitor {
                    expecting: $expecting,
                    rejection: $rejection,
                    target: PhantomData,
                })
            }
        }
    };
}

text_serde!(
    Timestamp,
    "an RFC 3339 formatted date and time string",
    |timestamp| timestamp.to_rfc3339_opts(SecondsFormat::AutoSi, true)
);
text_serde!(CivilDate, "a formatted date string", |date| date);
text_serde!(CivilTime, "a formatted time string", |time| time);
text_serde!(
    CivilDateTime,
    "a formatted date and time string",
    |datetime| format_args!("{datetime:?}")
);
text_serde!(
    Weekday,
    "Weekday",
    Some("short or long weekday names expected"),
    |weekday| weekday
);
