// ABOUTME: Slack API client for posting and updating messages, opening modals, uploading files, verifying signatures
// ABOUTME: Shared across all dravr-xxx services for consistent Slack integration
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;
use std::str;

use reqwest::{Client, Response};
use ring::hmac;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tracing::{error, warn};

use super::SlackConfig;
use crate::http_client::describe_request_error;

/// Slack API endpoint for posting messages
const SLACK_POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";

/// Slack API endpoint for updating messages
const SLACK_CHAT_UPDATE_URL: &str = "https://slack.com/api/chat.update";

/// Slack API endpoint for opening a modal
const SLACK_VIEWS_OPEN_URL: &str = "https://slack.com/api/views.open";

/// Slack API endpoint that reserves an upload URL for one file
const SLACK_GET_UPLOAD_URL_URL: &str = "https://slack.com/api/files.getUploadURLExternal";

/// Slack API endpoint that shares reserved, uploaded files into a channel
const SLACK_COMPLETE_UPLOAD_URL: &str = "https://slack.com/api/files.completeUploadExternal";

/// Maximum age of a Slack request timestamp before rejection (5 minutes)
const MAX_TIMESTAMP_AGE_SECS: u64 = 300;

/// Slack API error codes that mean the delivery target is misconfigured
///
/// These never clear on their own — a renamed channel, an uninvited bot, or a
/// revoked token stays broken until an operator changes configuration. Posting
/// is fire-and-forget, so a `warn!` here is invisible: the caller believes it
/// notified and nothing ever arrives. Escalating to `error!` routes the failure
/// through `ErrorNotificationLayer` (which fires only on `Level::ERROR`) and out
/// to the independent error channel, so a dead route announces itself instead of
/// going quiet.
///
/// Transient codes (`ratelimited`, `service_unavailable`, `request_timeout`) are
/// deliberately absent — they retry into success and would only add noise.
const SLACK_MISCONFIGURED_TARGET_ERRORS: &[&str] = &[
    "channel_not_found",
    "is_archived",
    "not_in_channel",
    "invalid_auth",
    "not_authed",
    "account_inactive",
    "token_revoked",
    "missing_scope",
    "restricted_action",
];

/// Whether a Slack API error code indicates a misconfigured delivery target
///
/// See [`SLACK_MISCONFIGURED_TARGET_ERRORS`] for why these are treated apart
/// from transient failures.
fn is_misconfigured_target(api_error: &str) -> bool {
    SLACK_MISCONFIGURED_TARGET_ERRORS.contains(&api_error)
}

/// Log a failed fire-and-forget Slack call at the severity its cause deserves
///
/// A misconfigured target is an operator-actionable outage of the notification
/// path itself, so it logs at ERROR with the channel that failed. Everything
/// else stays at WARN.
fn log_delivery_failure(result: SlackResult, channel: &str, operation: &str) {
    match result {
        SlackResult::Ok => {}
        SlackResult::ApiError(e) if is_misconfigured_target(&e) => {
            error!(
                error = %e,
                channel = %channel,
                operation = operation,
                "Slack delivery target is misconfigured: notifications to this channel are being dropped"
            );
        }
        SlackResult::ApiError(e) | SlackResult::HttpError(e) => {
            warn!(error = %e, channel = %channel, operation = operation, "Slack {operation} failed");
        }
    }
}

/// Slack API client with bot token authentication
///
/// Provides methods for posting Block Kit messages, updating existing messages,
/// and verifying HMAC-SHA256 signatures on incoming Slack requests.
#[derive(Clone)]
pub struct SlackClient {
    http: Client,
    bot_token: String,
    signing_secret: Option<String>,
}

/// Result of a Slack API call
#[derive(Debug)]
#[non_exhaustive]
pub enum SlackResult {
    /// Message sent successfully
    Ok,
    /// Slack API returned ok=false with an error string
    ApiError(String),
    /// HTTP-level failure
    HttpError(String),
}

/// Error from Slack signature verification
#[derive(Debug)]
#[non_exhaustive]
pub enum SignatureError {
    /// Missing required header
    MissingHeader(&'static str),
    /// Timestamp too old (replay attack protection)
    TimestampExpired(u64),
    /// HMAC signature mismatch
    InvalidSignature,
    /// No signing secret configured
    NotConfigured,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader(h) => write!(f, "missing header: {h}"),
            Self::TimestampExpired(age) => write!(f, "timestamp too old ({age}s)"),
            Self::InvalidSignature => write!(f, "invalid HMAC signature"),
            Self::NotConfigured => write!(f, "signing secret not configured"),
        }
    }
}

/// A file to share into a channel with [`SlackClient::upload_file`]
#[derive(Clone)]
#[non_exhaustive]
pub struct SlackFile {
    /// Name Slack stores the file under, extension included
    pub filename: String,
    /// Title shown with the file; the filename when absent
    pub title: Option<String>,
    /// What a screen reader says for an image
    pub alt_text: Option<String>,
    /// The file's content
    pub bytes: Vec<u8>,
}

impl SlackFile {
    /// A file named `filename` holding `bytes`, with no title or alt text
    #[must_use]
    pub fn new(filename: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            filename: filename.into(),
            title: None,
            alt_text: None,
            bytes,
        }
    }

    /// Show `title` with the file instead of its filename
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Have a screen reader say `alt_text` for the image
    #[must_use]
    pub fn with_alt_text(mut self, alt_text: impl Into<String>) -> Self {
        self.alt_text = Some(alt_text.into());
        self
    }
}

impl fmt::Debug for SlackFile {
    /// The content is a user's file: its size is logged, never its bytes
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackFile")
            .field("filename", &self.filename)
            .field("title", &self.title)
            .field("alt_text", &self.alt_text)
            .field("len", &self.bytes.len())
            .finish()
    }
}

impl SlackClient {
    /// Create a new Slack client from configuration
    pub fn new(config: &SlackConfig) -> Self {
        Self {
            http: Client::new(),
            bot_token: config.bot_token.clone(),
            signing_secret: config.signing_secret.clone(),
        }
    }

    /// Post a Block Kit message to a channel
    ///
    /// Fire-and-forget: spawns a background task. Errors are logged, never propagated.
    pub fn post_message(&self, channel: &str, blocks: &Value) {
        let token = self.bot_token.clone();
        let client = self.http.clone();
        let channel = channel.to_owned();
        let payload = serde_json::json!({
            "channel": channel,
            "blocks": blocks,
        });

        tokio::spawn(async move {
            let result =
                send_slack_request(&client, SLACK_POST_MESSAGE_URL, &token, &payload).await;
            log_delivery_failure(result, &channel, "post_message");
        });
    }

    /// Post a Block Kit message and return the result (awaitable)
    ///
    /// Use this when you need to know whether the message was sent.
    pub async fn post_message_await(&self, channel: &str, blocks: &Value) -> SlackResult {
        let payload = serde_json::json!({
            "channel": channel,
            "blocks": blocks,
        });
        send_slack_request(
            &self.http,
            SLACK_POST_MESSAGE_URL,
            &self.bot_token,
            &payload,
        )
        .await
    }

    /// Update an existing Slack message (replace blocks)
    ///
    /// Fire-and-forget: spawns a background task.
    pub fn update_message(&self, channel: &str, message_ts: &str, blocks: &Value) {
        let token = self.bot_token.clone();
        let client = self.http.clone();
        let channel = channel.to_owned();
        let payload = serde_json::json!({
            "channel": channel,
            "ts": message_ts,
            "blocks": blocks,
        });

        tokio::spawn(async move {
            let result = send_slack_request(&client, SLACK_CHAT_UPDATE_URL, &token, &payload).await;
            log_delivery_failure(result, &channel, "update_message");
        });
    }

    /// Open `view` as a modal for the person whose interaction carried
    /// `trigger_id` (`views.open`)
    ///
    /// Awaitable rather than fire-and-forget: Slack honours a `trigger_id` for
    /// three seconds after the tap, so the caller opens the modal before it
    /// answers the interaction, and learns whether it opened.
    pub async fn open_view(&self, trigger_id: &str, view: &Value) -> SlackResult {
        let payload = views_open_payload(trigger_id, view);
        send_slack_request(&self.http, SLACK_VIEWS_OPEN_URL, &self.bot_token, &payload).await
    }

    /// Upload `file` and share it into `channel`, with `initial_comment` as
    /// the message it is posted under
    ///
    /// Slack's external upload: `files.getUploadURLExternal` reserves a URL
    /// for the file's name and length, the bytes are posted to that URL, and
    /// `files.completeUploadExternal` shares the file into the channel. The
    /// file stays inside the workspace, unlike an `image` block, which needs a
    /// public URL. The bot needs the `files:write` scope; a missing scope comes
    /// back as `ApiError("missing_scope")`.
    pub async fn upload_file(
        &self,
        channel: &str,
        file: SlackFile,
        initial_comment: Option<&str>,
    ) -> SlackResult {
        let reserve = reserve_upload_form(&file);
        let reserved = match call_slack_form(
            &self.http,
            SLACK_GET_UPLOAD_URL_URL,
            &self.bot_token,
            &reserve,
        )
        .await
        .and_then(|body| reserved_upload(&body))
        {
            Ok(reserved) => reserved,
            Err(failure) => return failure,
        };

        let title = file.title.unwrap_or(file.filename);
        if let Err(failure) = post_upload_bytes(&self.http, &reserved.upload_url, file.bytes).await
        {
            return failure;
        }

        let complete = complete_upload_form(&reserved.file_id, &title, channel, initial_comment);
        match call_slack_form(
            &self.http,
            SLACK_COMPLETE_UPLOAD_URL,
            &self.bot_token,
            &complete,
        )
        .await
        {
            Ok(_) => SlackResult::Ok,
            Err(failure) => failure,
        }
    }

    /// Verify a Slack request signature (HMAC-SHA256 v0 scheme)
    ///
    /// Validates:
    /// - `x-slack-request-timestamp` is present and within 5 minutes
    /// - `x-slack-signature` matches HMAC-SHA256 of `v0:{timestamp}:{body}`
    pub fn verify_signature(
        &self,
        timestamp: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<(), SignatureError> {
        let signing_secret = self
            .signing_secret
            .as_deref()
            .ok_or(SignatureError::NotConfigured)?;

        // Replay protection
        let ts: u64 = timestamp
            .parse()
            .map_err(|_| SignatureError::MissingHeader("x-slack-request-timestamp"))?;
        let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0);
        let age = now.saturating_sub(ts);
        if age > MAX_TIMESTAMP_AGE_SECS {
            return Err(SignatureError::TimestampExpired(age));
        }

        // Compute HMAC-SHA256 using Slack v0 scheme
        let body_str = str::from_utf8(body).unwrap_or("");
        let basestring = format!("v0:{timestamp}:{body_str}");
        let key = hmac::Key::new(hmac::HMAC_SHA256, signing_secret.as_bytes());
        let tag = hmac::sign(&key, basestring.as_bytes());
        let expected = format!("v0={}", hex::encode(tag.as_ref()));

        // Constant-time comparison to avoid leaking the expected HMAC via timing
        // (mirrors the bearer-token check in `crate::server::auth`). Length is not
        // secret, so the length-mismatch short-circuit in `ct_eq` is acceptable.
        let equal: bool = signature.as_bytes().ct_eq(expected.as_bytes()).into();
        equal.ok_or(SignatureError::InvalidSignature)
    }
}

/// Send a JSON request to a Slack API endpoint and parse the response
async fn send_slack_request(
    client: &Client,
    url: &str,
    token: &str,
    payload: &Value,
) -> SlackResult {
    let sent = client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .json(payload)
        .send()
        .await;
    match read_slack_response(sent).await {
        Ok(_) => SlackResult::Ok,
        Err(failure) => failure,
    }
}

/// Send a form-encoded request to a Slack API endpoint, returning the body of
/// an `ok` answer
///
/// The `files.*` upload methods take form fields rather than a JSON body.
async fn call_slack_form(
    client: &Client,
    url: &str,
    token: &str,
    form: &[(&str, String)],
) -> Result<Value, SlackResult> {
    let sent = client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .form(form)
        .send()
        .await;
    read_slack_response(sent).await
}

/// Read a Slack Web API answer: the body when it says `ok`, the failure
/// otherwise
async fn read_slack_response(sent: Result<Response, reqwest::Error>) -> Result<Value, SlackResult> {
    let response = sent.map_err(|e| SlackResult::HttpError(describe_request_error(e)))?;
    if !response.status().is_success() {
        return Err(SlackResult::HttpError(format!(
            "HTTP {}",
            response.status()
        )));
    }
    let body = response.json::<Value>().await.map_err(|e| {
        SlackResult::HttpError(format!("response parse: {}", describe_request_error(e)))
    })?;
    ok_body(body)
}

/// A Slack answer's body when `ok` is true; otherwise its `error` code
fn ok_body(body: Value) -> Result<Value, SlackResult> {
    if body.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(body);
    }
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Err(SlackResult::ApiError(error.to_owned()))
}

/// The `views.open` body that opens `view` on `trigger_id`
fn views_open_payload(trigger_id: &str, view: &Value) -> Value {
    json!({ "trigger_id": trigger_id, "view": view })
}

/// Where `files.getUploadURLExternal` says to post the file, and the id the
/// file is shared under
#[derive(Debug, PartialEq, Eq)]
struct ReservedUpload {
    upload_url: String,
    file_id: String,
}

/// The fields `files.getUploadURLExternal` reserves `file` with: its name and
/// its length in bytes, plus alt text when it has one
fn reserve_upload_form(file: &SlackFile) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("filename", file.filename.clone()),
        ("length", file.bytes.len().to_string()),
    ];
    if let Some(alt_text) = &file.alt_text {
        form.push(("alt_txt", alt_text.clone()));
    }
    form
}

/// Read the upload URL and file id out of an `ok` reservation
fn reserved_upload(body: &Value) -> Result<ReservedUpload, SlackResult> {
    let field = |name: &str| body.get(name).and_then(Value::as_str).map(str::to_owned);
    match (field("upload_url"), field("file_id")) {
        (Some(upload_url), Some(file_id)) => Ok(ReservedUpload {
            upload_url,
            file_id,
        }),
        _ => Err(SlackResult::ApiError(
            "files.getUploadURLExternal answered without upload_url and file_id".to_owned(),
        )),
    }
}

/// Post the file's bytes to the URL its reservation named
///
/// The URL is pre-signed for this one file, so the request carries no token.
async fn post_upload_bytes(
    client: &Client,
    upload_url: &str,
    bytes: Vec<u8>,
) -> Result<(), SlackResult> {
    let response = client
        .post(upload_url)
        .body(bytes)
        .send()
        .await
        .map_err(|e| SlackResult::HttpError(describe_request_error(e)))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(SlackResult::HttpError(format!(
            "file upload HTTP {}",
            response.status()
        )))
    }
}

/// The fields `files.completeUploadExternal` shares the reserved file into
/// `channel` with, under `initial_comment` when there is one
fn complete_upload_form(
    file_id: &str,
    title: &str,
    channel: &str,
    initial_comment: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        (
            "files",
            json!([{ "id": file_id, "title": title }]).to_string(),
        ),
        ("channel_id", channel.to_owned()),
    ];
    if let Some(comment) = initial_comment {
        form.push(("initial_comment", comment.to_owned()));
    }
    form
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_channel_is_a_misconfigured_target() {
        // The exact outage this classifier exists for: renaming a Slack channel
        // makes every post to the old #name return ok=false/channel_not_found,
        // and fire-and-forget posting swallowed it at WARN.
        assert!(is_misconfigured_target("channel_not_found"));
    }

    #[test]
    fn operator_actionable_slack_errors_are_misconfigured_targets() {
        for code in [
            "channel_not_found",
            "is_archived",
            "not_in_channel",
            "invalid_auth",
            "not_authed",
            "account_inactive",
            "token_revoked",
            "missing_scope",
            "restricted_action",
        ] {
            assert!(
                is_misconfigured_target(code),
                "{code} must escalate to ERROR"
            );
        }
        assert_eq!(SLACK_MISCONFIGURED_TARGET_ERRORS.len(), 9);
    }

    #[test]
    fn transient_slack_errors_are_not_misconfigured_targets() {
        // These clear on retry; escalating them would page on ordinary noise.
        for code in [
            "ratelimited",
            "service_unavailable",
            "request_timeout",
            "fatal_error",
            "internal_error",
            "",
        ] {
            assert!(!is_misconfigured_target(code), "{code} must stay at WARN");
        }
    }

    #[test]
    fn signature_error_display() {
        assert_eq!(
            SignatureError::MissingHeader("x-slack-request-timestamp").to_string(),
            "missing header: x-slack-request-timestamp"
        );
        assert_eq!(
            SignatureError::TimestampExpired(600).to_string(),
            "timestamp too old (600s)"
        );
        assert_eq!(
            SignatureError::InvalidSignature.to_string(),
            "invalid HMAC signature"
        );
        assert_eq!(
            SignatureError::NotConfigured.to_string(),
            "signing secret not configured"
        );
    }

    #[test]
    fn verify_signature_rejects_without_secret() {
        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: None,
        };
        let client = SlackClient::new(&config);
        let result = client.verify_signature("1234567890", "v0=abc", b"body");
        assert!(matches!(result, Err(SignatureError::NotConfigured)));
    }

    #[test]
    fn verify_signature_validates_correctly() {
        let secret = "test_signing_secret";
        let timestamp = "1234567890";
        let body = b"test_body";

        // Compute expected signature
        let basestring = format!(
            "v0:{timestamp}:{}",
            str::from_utf8(body).expect("test body is valid UTF-8") // Safe: test assertion
        );
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let tag = hmac::sign(&key, basestring.as_bytes());
        let signature = format!("v0={}", hex::encode(tag.as_ref()));

        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: Some(secret.into()),
        };
        let client = SlackClient::new(&config);

        // Valid signature (timestamp will be "expired" but we test the HMAC path)
        // We expect TimestampExpired since the timestamp is from 2009
        let result = client.verify_signature(timestamp, &signature, body);
        assert!(matches!(result, Err(SignatureError::TimestampExpired(_))));
    }

    #[test]
    fn verify_signature_accepts_valid_with_fresh_timestamp() {
        // Regression: the valid-signature path must accept a correctly computed
        // HMAC when compared in constant time (ct_eq), not only reject bad ones.
        let secret = "test_signing_secret";
        let now = chrono::Utc::now().timestamp().to_string();
        let body = b"payload=hello";

        let basestring = format!(
            "v0:{now}:{}",
            str::from_utf8(body).expect("test body is valid UTF-8") // Safe: test assertion
        );
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let tag = hmac::sign(&key, basestring.as_bytes());
        let signature = format!("v0={}", hex::encode(tag.as_ref()));

        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: Some(secret.into()),
        };
        let client = SlackClient::new(&config);

        let result = client.verify_signature(&now, &signature, body);
        assert!(result.is_ok());
    }

    #[test]
    fn verify_signature_rejects_bad_signature() {
        let config = SlackConfig {
            bot_token: "xoxb-test".into(),
            error_channel: "#errors".into(),
            signing_secret: Some("secret".into()),
        };
        let client = SlackClient::new(&config);

        // Use a recent timestamp so we don't hit the expiry check
        let now = chrono::Utc::now().timestamp().to_string();
        let result = client.verify_signature(&now, "v0=badhash", b"body");
        assert!(matches!(result, Err(SignatureError::InvalidSignature)));
    }

    #[test]
    fn an_ok_answer_hands_back_its_body_and_a_refusal_its_error_code() {
        let body = json!({ "ok": true, "file_id": "F1" });
        assert!(matches!(ok_body(body.clone()), Ok(read) if read == body));

        let refused = ok_body(json!({ "ok": false, "error": "expired_trigger_id" }));
        assert!(
            matches!(refused, Err(SlackResult::ApiError(code)) if code == "expired_trigger_id")
        );

        // An answer that never says ok is a failure, never a success.
        let silent = ok_body(json!({ "file_id": "F1" }));
        assert!(matches!(silent, Err(SlackResult::ApiError(code)) if code == "unknown"));
    }

    #[test]
    fn a_modal_opens_on_the_trigger_the_tap_carried() {
        let view = json!({ "type": "modal", "callback_id": "reply" });
        assert_eq!(
            views_open_payload("13345224609.738474920.8088930838d88f008e0", &view),
            json!({ "trigger_id": "13345224609.738474920.8088930838d88f008e0", "view": view })
        );
    }

    #[test]
    fn a_file_is_reserved_by_its_name_and_byte_length() {
        let file = SlackFile::new("screenshot.png", vec![0_u8; 2048]);
        assert_eq!(
            reserve_upload_form(&file),
            [
                ("filename", "screenshot.png".to_owned()),
                ("length", "2048".to_owned())
            ]
        );

        let described = file.with_alt_text("the week view");
        assert_eq!(
            reserve_upload_form(&described).last(),
            Some(&("alt_txt", "the week view".to_owned()))
        );
    }

    #[test]
    fn a_reservation_names_where_to_post_and_the_file_it_reserved() {
        let body = json!({
            "ok": true,
            "upload_url": "https://files.slack.com/upload/v1/abc",
            "file_id": "F0AB12CD",
        });
        assert_eq!(
            reserved_upload(&body).ok(),
            Some(ReservedUpload {
                upload_url: "https://files.slack.com/upload/v1/abc".to_owned(),
                file_id: "F0AB12CD".to_owned(),
            })
        );

        for partial in [
            json!({ "ok": true, "file_id": "F0AB12CD" }),
            json!({ "ok": true, "upload_url": "https://files.slack.com/upload/v1/abc" }),
        ] {
            assert!(
                matches!(reserved_upload(&partial), Err(SlackResult::ApiError(_))),
                "{partial}"
            );
        }
    }

    #[test]
    fn a_completed_upload_is_shared_into_the_channel_under_its_comment() {
        let form = complete_upload_form(
            "F0AB12CD",
            "Screenshot",
            "C0FEEDBACK",
            Some("from an athlete"),
        );
        assert_eq!(
            form,
            [
                (
                    "files",
                    r#"[{"id":"F0AB12CD","title":"Screenshot"}]"#.to_owned()
                ),
                ("channel_id", "C0FEEDBACK".to_owned()),
                ("initial_comment", "from an athlete".to_owned()),
            ]
        );

        let bare = complete_upload_form("F0AB12CD", "Screenshot", "C0FEEDBACK", None);
        assert!(bare.iter().all(|(name, _)| *name != "initial_comment"));
    }

    #[test]
    fn a_file_logs_its_size_never_its_bytes() {
        let file =
            SlackFile::new("screenshot.png", b"private pixels".to_vec()).with_title("Screenshot");
        let logged = format!("{file:?}");
        assert!(logged.contains("len: 14"), "{logged}");
        assert!(
            !logged.contains("112, 114"),
            "the bytes must not be logged: {logged}"
        );
    }
}
