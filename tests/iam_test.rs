// ABOUTME: Tests the identity-token middleware refuses every unauthenticated shape, offline
// ABOUTME: The fail-closed paths are the point — require_auth's fail-open default is what this replaces
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(feature = "google-iam")]
// Same allowances tests/integration_test.rs carries: an integration test is not
// covered by the lib's `cfg_attr(test, ...)`, and assertions are what these are.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{middleware, Router};
use dravr_tronc::iam::{require_google_id_token, GoogleIdTokenVerifier, IamError, IdTokenSource};
use http_body_util::BodyExt;
use reqwest::Client;
use tower::ServiceExt;

/// The audience these tests pin against — shape-identical to a Cloud Run URL.
const AUDIENCE: &str = "https://example-service-abc123-nn.a.run.app";

/// A router whose only route is guarded by the identity-token middleware.
fn guarded_app() -> Router {
    let verifier = Arc::new(GoogleIdTokenVerifier::new(AUDIENCE, Client::new()));
    Router::new()
        .route("/protected", get(|| async { "reached the handler" }))
        .layer(middleware::from_fn(move |req, next| {
            let verifier = Arc::clone(&verifier);
            async move { require_google_id_token(verifier, req, next).await }
        }))
}

/// Send one request and return (status, body).
async fn call(request: Request<Body>) -> (StatusCode, String) {
    let response = guarded_app()
        .oneshot(request)
        .await
        .expect("router should always answer");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body should collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn rejects_a_request_with_no_authorization_header() {
    let (status, body) = call(
        Request::builder()
            .uri("/protected")
            .body(Body::empty())
            .expect("request builds"),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The handler's own string must not appear: a fail-open middleware would
    // return it with a 200, which is precisely the regression this guards.
    assert!(
        !body.contains("reached the handler"),
        "unauthenticated request reached the handler: {body}"
    );
    assert!(
        body.contains("unauthorized"),
        "expected a structured refusal, got {body}"
    );
}

#[tokio::test]
async fn rejects_a_non_bearer_scheme() {
    let (status, body) = call(
        Request::builder()
            .uri("/protected")
            .header("authorization", "Basic dXNlcjpwYXNz")
            .body(Body::empty())
            .expect("request builds"),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.contains("reached the handler"));
}

#[tokio::test]
async fn rejects_a_bearer_token_that_is_not_a_jwt() {
    let (status, body) = call(
        Request::builder()
            .uri("/protected")
            .header("authorization", "Bearer not-a-jwt")
            .body(Body::empty())
            .expect("request builds"),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.contains("reached the handler"));
}

#[tokio::test]
async fn refusal_does_not_disclose_which_check_failed() {
    // A verifier that explains itself is a claim-guessing oracle: an attacker
    // adjusts one field per attempt until it stops complaining. Every refusal
    // must read the same from outside, whatever went wrong inside.
    let (_, missing) = call(
        Request::builder()
            .uri("/protected")
            .body(Body::empty())
            .expect("request builds"),
    )
    .await;
    let (_, malformed) = call(
        Request::builder()
            .uri("/protected")
            .header("authorization", "Bearer eyJhbGciOiJSUzI1NiJ9.e30.sig")
            .body(Body::empty())
            .expect("request builds"),
    )
    .await;

    for body in [&missing, &malformed] {
        assert!(!body.contains("kid"), "leaked a claim name: {body}");
        assert!(
            !body.contains("audience"),
            "leaked the expected audience: {body}"
        );
        assert!(
            !body.contains(AUDIENCE),
            "leaked the audience value: {body}"
        );
    }
}

#[tokio::test]
async fn verifier_rejects_a_token_with_no_kid_without_network() {
    // Header {"alg":"RS256"} with no kid, empty claims, junk signature. Rejected
    // before any key lookup, so this holds with no Google reachable.
    let verifier = GoogleIdTokenVerifier::new(AUDIENCE, Client::new());
    let result = verifier.verify("eyJhbGciOiJSUzI1NiJ9.e30.c2ln").await;

    match result {
        Err(IamError::Rejected(why)) => {
            assert!(
                why.contains("kid"),
                "expected the cause to name the missing kid, got: {why}"
            );
        }
        Err(other) => panic!("expected Rejected, got {other}"),
        Ok(_) => panic!("a token with no kid must not verify"),
    }
}

#[tokio::test]
async fn token_source_reports_metadata_absence_distinctly() {
    // Off Google infrastructure metadata.google.internal does not resolve. The
    // error must say that rather than looking like a rejected credential —
    // conflating the two sends a developer to IAM bindings for a problem that
    // is only "this laptop is not Cloud Run".
    let source = IdTokenSource::new(AUDIENCE, Client::new());
    assert_eq!(source.audience(), AUDIENCE);

    match source.token().await {
        Err(IamError::MetadataUnavailable(_)) => {}
        Err(other) => panic!("expected MetadataUnavailable off Google infra, got {other}"),
        Ok(_) => panic!("a token should not be obtainable in CI"),
    }
}
