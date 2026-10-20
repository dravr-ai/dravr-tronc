// ABOUTME: Process-wide redaction of configured secret values from logs and notifications
// ABOUTME: Matches the values themselves, so a token logged under any field name or inside any message is masked
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Redaction of the process's configured secret values.
//!
//! A field-name denylist only catches a secret logged under a name someone
//! thought of; a provider error that echoes its request, a `Debug` of a
//! config struct or a URL with a key in its query string slip past it. This
//! module matches the *values*: the host registers what it holds (a bearer
//! token, an API key, a database password), and every sink tronc owns — the
//! log writer of [`tracing_init`](crate::server::tracing_init), the Slack and
//! email digests of the error notifier, the Slack and `PostHog` sinks of the
//! notify layer — replaces each occurrence with [`MASK`] before it leaves the
//! process.
//!
//! [`register_env`] seeds the registry from every environment variable whose
//! name ends like a credential (see [`SECRET_NAME_SUFFIXES`]); the
//! `tracing_init` entry points call it, so a service that keeps its secrets in
//! the environment needs nothing more. A secret read from elsewhere (Secret
//! Manager, a file, a database row) is added with [`register`].
//!
//! ```rust
//! use dravr_tronc::secrets;
//!
//! secrets::register("sk-live-0123456789abcdef");
//! assert_eq!(
//!     secrets::redact("upstream said: bad key sk-live-0123456789abcdef"),
//!     "upstream said: bad key [REDACTED]"
//! );
//! ```

use std::borrow::Cow;
use std::cmp::Reverse;
use std::env;
use std::io::{self, Write};
use std::sync::{PoisonError, RwLock};

use tracing_subscriber::fmt::MakeWriter;

/// What a redacted value is replaced with.
pub const MASK: &str = "[REDACTED]";

/// Values shorter than this are never registered: a short value (`true`,
/// `8081`, `admin`) also occurs in ordinary text, and masking it would hide
/// the text without protecting anything.
pub const MIN_SECRET_LEN: usize = 8;

/// Environment variable name endings [`register_env`] treats as credentials.
pub const SECRET_NAME_SUFFIXES: &[&str] = &[
    "_TOKEN",
    "_SECRET",
    "_PASSWORD",
    "_API_KEY",
    "_KEY",
    "_PAT",
    "_CREDENTIALS",
    "_DSN",
];

/// The registered values, longest first, so a secret that contains another
/// is masked whole rather than leaving its remainder in the text.
static SECRETS: RwLock<Vec<String>> = RwLock::new(Vec::new());

/// Register `value` as a secret. Values shorter than [`MIN_SECRET_LEN`]
/// (after trimming) are ignored; registering a value twice is a no-op.
pub fn register(value: &str) {
    let value = value.trim();
    if value.len() < MIN_SECRET_LEN {
        return;
    }
    let mut secrets = SECRETS.write().unwrap_or_else(PoisonError::into_inner);
    if secrets.iter().any(|known| known == value) {
        return;
    }
    secrets.push(value.to_owned());
    secrets.sort_by_key(|known| Reverse(known.len()));
}

/// Register the value of every environment variable whose name ends with one
/// of [`SECRET_NAME_SUFFIXES`], and return how many names matched.
pub fn register_env() -> usize {
    let mut matched = 0;
    for (name, value) in env::vars_os() {
        let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
            continue;
        };
        if is_secret_name(name) {
            register(value);
            matched += 1;
        }
    }
    matched
}

/// Whether an environment variable named `name` holds a credential.
#[must_use]
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_NAME_SUFFIXES
        .iter()
        .any(|suffix| upper.ends_with(suffix))
}

/// `text` with every registered secret replaced by [`MASK`]. Borrows when
/// nothing matched, which is the common case.
#[must_use]
pub fn redact(text: &str) -> Cow<'_, str> {
    let secrets = SECRETS.read().unwrap_or_else(PoisonError::into_inner);
    let mut out = Cow::Borrowed(text);
    for secret in secrets.iter() {
        if out.contains(secret.as_str()) {
            out = Cow::Owned(out.replace(secret.as_str(), MASK));
        }
    }
    out
}

/// Replace every registered secret in `text`, in place.
pub fn redact_in_place(text: &mut String) {
    if let Cow::Owned(redacted) = redact(text) {
        *text = redacted;
    }
}

/// A [`MakeWriter`] whose writers mask registered secrets before passing the
/// bytes on to `inner`'s.
///
/// The `tracing_subscriber` fmt layer formats an event into one buffer and
/// writes it with a single `write_all`, so a secret is never split across two
/// writes this sees.
#[derive(Debug, Clone)]
pub struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    /// Wrap `inner`.
    pub const fn new(inner: M) -> Self {
        Self { inner }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for RedactingMakeWriter<M> {
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
        }
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer_for(meta),
        }
    }
}

/// The writer of a [`RedactingMakeWriter`].
#[derive(Debug)]
pub struct RedactingWriter<W> {
    inner: W,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match String::from_utf8_lossy(buf) {
            Cow::Borrowed(text) => match redact(text) {
                Cow::Borrowed(_) => self.inner.write_all(buf)?,
                Cow::Owned(masked) => self.inner.write_all(masked.as_bytes())?,
            },
            Cow::Owned(text) => self.inner.write_all(redact(&text).as_bytes())?,
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registered_value_is_masked_wherever_it_occurs() {
        register("tok-aaaa-1111-registered");
        assert_eq!(
            redact("Bearer tok-aaaa-1111-registered failed; retry tok-aaaa-1111-registered"),
            "Bearer [REDACTED] failed; retry [REDACTED]"
        );
    }

    #[test]
    fn a_short_value_is_never_registered() {
        register("short");
        assert_eq!(redact("a short message"), "a short message");
    }

    #[test]
    fn a_secret_containing_another_is_masked_whole() {
        register("inner-secret-value");
        register("outer-inner-secret-value-tail");
        assert_eq!(
            redact("x outer-inner-secret-value-tail y"),
            "x [REDACTED] y"
        );
    }

    #[test]
    fn text_without_a_secret_is_borrowed() {
        assert!(matches!(redact("nothing to hide here"), Cow::Borrowed(_)));
    }

    #[test]
    fn credential_names_are_recognised_and_others_are_not() {
        for name in [
            "SLACK_BOT_TOKEN",
            "resend_api_key",
            "DB_PASSWORD",
            "SENTRY_DSN",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in ["RUST_LOG", "HTTP_PORT", "OTEL_SERVICE_NAME", "TOKEN_TTL"] {
            assert!(!is_secret_name(name), "{name}");
        }
    }

    #[test]
    fn the_writer_masks_what_it_passes_on() {
        register("writer-secret-0123456789");
        let mut out = Vec::new();
        let mut writer = RedactingWriter { inner: &mut out };
        let line = b"ERROR call failed key=writer-secret-0123456789\n";
        assert_eq!(writer.write(line).unwrap(), line.len());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "ERROR call failed key=[REDACTED]\n"
        );
    }
}
