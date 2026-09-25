// ABOUTME: Outbound HTTP support shared by every reqwest caller: errors described without their URL
// ABOUTME: describe_request_error strips the request URL, which can carry a credential, and keeps the cause
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Outbound HTTP support for services that call vendor APIs with `reqwest`.
//!
//! **Every `reqwest::Error` becomes text through [`describe_request_error`].**
//! `reqwest::Error`'s `Display` ends with `for url (<request url>)`, and a
//! vendor API can carry its credential in that URL — Telegram's Bot API puts
//! the bot token in the path, `OpenWeatherMap` puts its `appid` key in the
//! query — so formatting the error as it comes writes the credential into
//! every error message and log line built from it.

use std::error::Error;
use std::iter;

/// Describe a failed request without the URL it was sent to.
///
/// The description is the error with its URL removed, followed by each
/// underlying cause (`tcp connect error`, `operation timed out`, a TLS
/// failure), joined by `": "`: `reqwest` names only the error's kind, and the
/// cause is what says why the call failed.
///
/// A cause that is itself a `reqwest::Error` is left out of the chain: it can
/// carry a URL of its own, and only the outer error can be stripped. Its own
/// causes are still described.
#[must_use]
pub fn describe_request_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    iter::once(error.to_string())
        .chain(
            iter::successors(error.source(), |&cause| cause.source())
                .filter(|cause| !cause.is::<reqwest::Error>())
                .map(ToString::to_string),
        )
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// A URL on a loopback port nothing listens on, carrying a secret in both
    /// the path and the query, as Telegram and `OpenWeatherMap` do.
    fn refused_url_with_secret() -> String {
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("bind") // Safe: test assertion
            .local_addr()
            .expect("addr") // Safe: test assertion
            .port();
        format!("http://127.0.0.1:{port}/botSECRET-TOKEN/sendMessage?appid=SECRET-KEY")
    }

    #[tokio::test]
    async fn the_description_drops_the_url_and_keeps_the_cause() {
        let url = refused_url_with_secret();
        let error = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect_err("nothing listens on the port"); // Safe: test assertion

        // The premise: reqwest's own Display leaks the credential.
        assert!(error.to_string().contains("SECRET-TOKEN"));

        let description = describe_request_error(error);
        assert!(
            !description.contains("SECRET"),
            "the description must carry no part of the URL; it was {description:?}"
        );
        assert!(
            description.starts_with("error sending request"),
            "description was {description:?}"
        );
        assert!(
            description.contains(": "),
            "the underlying cause must follow the kind; description was {description:?}"
        );
    }
}
