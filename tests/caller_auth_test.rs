// ABOUTME: Tests CallerAuthHook over the real /mcp transport: a verified token is the caller's only identity
// ABOUTME: Wrong audience, issuer, key, expiry or tenant all fail closed; the shared key reaches no tenant tool
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg(all(feature = "google-iam", feature = "testkit"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::env;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use dravr_tronc::iam::GoogleKeySet;
use dravr_tronc::mcp::auth::{ApiKeyAuthHook, API_KEY_AUTH_METHOD};
use dravr_tronc::mcp::caller::{CallerAuthHook, CallerTokenVerifier, VERIFIED_CALLER_AUTH_METHOD};
use dravr_tronc::mcp::resource_metadata::ProtectedResourceMetadata;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::tool::{McpTool, ToolCapabilities, ToolContext, ToolRegistry};
use dravr_tronc::testkit::{McpTestClient, TestIssuer};
use reqwest::Client;
use serde_json::{json, Value};

const ISSUER: &str = "pierre-mcp-server";
const AUDIENCE: &str = "https://satellite.example.com/mcp";
const KEY_ENV: &str = "CALLER_AUTH_TEST_KEY";
const KEY: &str = "caller-auth-test-shared-key";

struct State;

/// Answers with the identity its context carries — and with nothing from its
/// arguments, which a caller can forge.
struct MyIdentity;

#[async_trait]
impl McpTool<State> for MyIdentity {
    fn definition(&self) -> Tool {
        Tool {
            name: "my_identity".to_owned(),
            description: "The caller's own identity".to_owned(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities::REQUIRES_AUTH
            | ToolCapabilities::REQUIRES_TENANT
            | ToolCapabilities::READS_DATA
    }

    fn required_scopes(&self) -> &'static [&'static str] {
        &["profile:read"]
    }

    async fn execute(&self, _state: &Arc<State>, ctx: &ToolContext, _args: Value) -> ToolResponse {
        ToolResponse::text(format!(
            "user={} tenant={} method={} client={} scopes={}",
            ctx.user_id.as_deref().unwrap_or("none"),
            ctx.tenant_id.as_deref().unwrap_or("none"),
            ctx.auth_method.as_deref().unwrap_or("none"),
            ctx.client_id.as_deref().unwrap_or("none"),
            ctx.scopes.join(","),
        ))
    }
}

/// A computation over its arguments, touching no tenant's data.
struct Echo;

#[async_trait]
impl McpTool<State> for Echo {
    fn definition(&self) -> Tool {
        Tool {
            name: "echo".to_owned(),
            description: "Echoes".to_owned(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            annotations: None,
            execution: None,
        }
    }

    async fn execute(&self, _state: &Arc<State>, ctx: &ToolContext, _args: Value) -> ToolResponse {
        ToolResponse::text(format!(
            "echo method={}",
            ctx.auth_method.as_deref().unwrap_or("none")
        ))
    }
}

fn metadata() -> ProtectedResourceMetadata {
    ProtectedResourceMetadata::new(AUDIENCE, vec!["https://api.example.com".to_owned()]).unwrap()
}

fn server(hook: CallerAuthHook) -> Arc<McpServer<State>> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(MyIdentity));
    registry.register(Box::new(Echo));
    Arc::new(
        McpServer::new("caller-auth-test", "0.1.0", registry, Arc::new(State))
            .with_protected_resource_metadata(metadata())
            .with_auth_hook(Arc::new(hook)),
    )
}

fn hook(issuer: &TestIssuer) -> CallerAuthHook {
    CallerAuthHook::new(
        Arc::new(issuer.verifier(AUDIENCE)),
        metadata().www_authenticate(),
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The text of a tool result.
fn text(result: &ToolResponse) -> &str {
    result.content[0].as_text().expect("a text result")
}

/// `token` presented to `server`: the HTTP status, and the challenge on a 401.
async fn status_of(server: Arc<McpServer<State>>, token: &str) -> (u16, Option<String>) {
    let answer = McpTestClient::in_process(server)
        .with_bearer(token)
        .raw(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await
        .expect("an answer");
    let challenge = answer
        .headers
        .get("www-authenticate")
        .map(|v| v.to_str().unwrap().to_owned());
    (answer.status.as_u16(), challenge)
}

#[tokio::test]
async fn a_verified_token_is_the_caller_and_its_arguments_are_not() {
    let issuer = TestIssuer::start(ISSUER).await.unwrap();
    let token = issuer
        .mint(
            &issuer
                .claims(AUDIENCE, "user-a")
                .tenant("tenant-a")
                .scopes(&["profile:read", "fitness:read"])
                .set("client_id", json!("client-7"))
                .build(),
        )
        .unwrap();

    let result = McpTestClient::in_process(server(hook(&issuer)))
        .with_bearer(token)
        .call_tool(
            "my_identity",
            json!({ "user_id": "user-b", "tenant_id": "tenant-b" }),
        )
        .await
        .expect("tools/call");
    assert!(!result.is_error, "{}", text(&result));
    assert_eq!(
        text(&result),
        format!(
            "user=user-a tenant=tenant-a method={VERIFIED_CALLER_AUTH_METHOD} client=client-7 \
             scopes=profile:read,fitness:read"
        )
    );
}

#[tokio::test]
async fn a_token_that_fails_any_check_is_a_401_with_the_metadata_challenge() {
    let issuer = TestIssuer::start(ISSUER).await.unwrap();
    let server = server(hook(&issuer));
    let challenge = Some(metadata().www_authenticate());
    let valid = || issuer.claims(AUDIENCE, "user-a").tenant("tenant-a");

    let refused = [
        (
            "minted for another resource server",
            issuer
                .mint(
                    &issuer
                        .claims("https://other.example.com/mcp", "user-a")
                        .build(),
                )
                .unwrap(),
        ),
        (
            "minted by another issuer",
            issuer
                .mint(&valid().set("iss", json!("someone-else")).build())
                .unwrap(),
        ),
        (
            "expired",
            issuer
                .mint(&valid().set("exp", json!(now() - 3600)).build())
                .unwrap(),
        ),
        (
            "not yet valid",
            issuer
                .mint(&valid().set("nbf", json!(now() + 3600)).build())
                .unwrap(),
        ),
        (
            "carrying no expiry",
            issuer.mint(&valid().without("exp").build()).unwrap(),
        ),
        (
            "carrying no subject",
            issuer.mint(&valid().without("sub").build()).unwrap(),
        ),
        (
            "carrying an empty subject",
            issuer.mint(&valid().set("sub", json!("")).build()).unwrap(),
        ),
        (
            "carrying a tenant that is not a string",
            issuer
                .mint(&valid().set("active_tenant_id", json!(42)).build())
                .unwrap(),
        ),
        ("not a token", "not-a-jwt".to_owned()),
    ];
    for (why, token) in refused {
        assert_eq!(
            status_of(Arc::clone(&server), &token).await,
            (401, challenge.clone()),
            "a token {why} must be refused"
        );
    }
}

#[tokio::test]
async fn a_token_signed_by_a_key_the_issuer_does_not_publish_is_refused() {
    let issuer = TestIssuer::start(ISSUER).await.unwrap();
    // Same issuer name, same key id, different key: a forgery.
    let forger = TestIssuer::start(ISSUER).await.unwrap();
    let forgery = forger
        .mint(
            &forger
                .claims(AUDIENCE, "user-a")
                .tenant("tenant-a")
                .scopes(&["profile:read"])
                .build(),
        )
        .unwrap();
    assert_eq!(status_of(server(hook(&issuer)), &forgery).await.0, 401);
}

#[tokio::test]
async fn an_issuer_whose_keys_cannot_be_fetched_answers_500_not_401() {
    let issuer = TestIssuer::start(ISSUER).await.unwrap();
    let token = issuer
        .mint(&issuer.claims(AUDIENCE, "user-a").tenant("tenant-a").build())
        .unwrap();
    // Nothing listens on port 9 of loopback (discard), so the fetch fails.
    let unreachable = CallerTokenVerifier::new(
        ISSUER,
        AUDIENCE,
        GoogleKeySet::new("http://127.0.0.1:9/jwks.json", Client::new()),
    );
    let server = server(CallerAuthHook::new(
        Arc::new(unreachable),
        metadata().www_authenticate(),
    ));
    assert_eq!(status_of(server, &token).await, (500, None));
}

#[tokio::test]
async fn a_caller_with_no_tenant_or_no_grant_never_reaches_a_tenant_tool() {
    let issuer = TestIssuer::start(ISSUER).await.unwrap();
    let client =
        |token: String| McpTestClient::in_process(server(hook(&issuer))).with_bearer(token);

    let tenantless = issuer
        .mint(
            &issuer
                .claims(AUDIENCE, "user-a")
                .scopes(&["profile:read"])
                .build(),
        )
        .unwrap();
    let refused = client(tenantless)
        .call_tool("my_identity", json!({ "tenant_id": "tenant-a" }))
        .await
        .unwrap();
    assert!(refused.is_error);
    assert!(
        text(&refused).contains("requires a tenant"),
        "{}",
        text(&refused)
    );

    let ungranted = issuer
        .mint(
            &issuer
                .claims(AUDIENCE, "user-a")
                .tenant("tenant-a")
                .scopes(&["fitness:read"])
                .build(),
        )
        .unwrap();
    let refused = client(ungranted)
        .call_tool("my_identity", json!({}))
        .await
        .unwrap();
    assert!(refused.is_error);
    assert_eq!(
        text(&refused),
        "Tool 'my_identity' requires the 'profile:read' scope"
    );
}

/// One test for the shared key, so this binary's only environment writes are
/// sequential.
#[tokio::test]
async fn the_shared_key_reaches_tenant_free_tools_only() {
    let issuer = TestIssuer::start(ISSUER).await.unwrap();
    env::set_var(KEY_ENV, KEY);
    let server = server(hook(&issuer).with_shared_key(ApiKeyAuthHook::new(KEY_ENV, "test")));
    let keyholder = McpTestClient::in_process(Arc::clone(&server)).with_bearer(KEY);

    let echoed = keyholder.call_tool("echo", json!({})).await.unwrap();
    assert_eq!(text(&echoed), format!("echo method={API_KEY_AUTH_METHOD}"));

    let refused = keyholder
        .call_tool(
            "my_identity",
            json!({ "user_id": "user-a", "tenant_id": "tenant-a" }),
        )
        .await
        .unwrap();
    assert!(refused.is_error);
    assert!(
        text(&refused).contains("verified caller"),
        "{}",
        text(&refused)
    );

    // Anything that is not the key is verified as a token, and fails.
    assert_eq!(status_of(Arc::clone(&server), "not-the-key").await.0, 401);
    env::remove_var(KEY_ENV);
    // A removed key admits nobody: the hook fails closed.
    assert_eq!(status_of(server, KEY).await.0, 401);
}

#[tokio::test]
async fn anonymous_admits_no_credential_but_never_a_bad_one() {
    let issuer = TestIssuer::start(ISSUER).await.unwrap();

    let closed = McpTestClient::in_process(server(hook(&issuer)))
        .raw(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await
        .unwrap();
    assert_eq!(closed.status.as_u16(), 401, "no credential, no anonymous");

    let dev = server(hook(&issuer).allowing_anonymous());
    let anonymous = McpTestClient::in_process(Arc::clone(&dev));
    assert_eq!(
        text(&anonymous.call_tool("echo", json!({})).await.unwrap()),
        "echo method=none"
    );
    let refused = anonymous.call_tool("my_identity", json!({})).await.unwrap();
    assert!(refused.is_error);
    assert!(text(&refused).contains("verified caller"));

    assert_eq!(status_of(dev, "a-bad-token").await.0, 401);
}
