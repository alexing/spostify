//! The Claude Messages API, over plain HTTP: there is no official Rust SDK.
//!
//! One briefing is one conversation of a single user message. Claude may
//! search the web on Anthropic's side while it writes; when that loop runs
//! long the answer stops with `pause_turn`, and the conversation is sent
//! again with everything Claude produced so far so it can carry on.

use std::time::Duration;

use serde_json::{Value, json};

use super::Error;
use super::prompt::SYSTEM;

const URL: &str = "https://api.anthropic.com/v1/messages";
pub const MODEL: &str = "claude-sonnet-5-5";
const API_VERSION: &str = "2023-06-01";
/// Lets Anthropic answer a declined request with its recommended fallback
/// model instead of returning the refusal.
const BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 16_000;
/// Searching the web and writing can take several minutes, and a request
/// cut off by the deadline is still paid for. Ten minutes is the official
/// SDKs' default.
const TIMEOUT: Duration = Duration::from_secs(600);
/// How many times a paused turn is resumed before giving up on more.
const MAX_CONTINUATIONS: usize = 5;
/// Retries after an overloaded or failing server.
const MAX_SERVER_RETRIES: u32 = 2;
/// The longest `retry-after` worth waiting for in an open panel.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// A finished answer.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub text: String,
    /// The answer hit `max_tokens` and ends early.
    pub truncated: bool,
}

/// The JSON body for the conversation so far.
pub fn request_body(messages: &[Value]) -> Value {
    json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "output_config": { "effort": "medium" },
        "fallbacks": "default",
        "system": SYSTEM,
        "tools": [{
            "type": "web_search_20260209",
            "name": "web_search",
            "max_uses": 5,
        }],
        "messages": messages,
    })
}

pub fn user_message(text: &str) -> Value {
    json!({ "role": "user", "content": text })
}

/// The conversation that resumes a paused turn: the original question and
/// every block Claude has produced so far, unchanged, with no new user
/// message. Claude picks up where the trailing block left off.
pub fn continuation(user: &Value, produced: &[Value]) -> Vec<Value> {
    vec![
        user.clone(),
        json!({ "role": "assistant", "content": produced }),
    ]
}

/// What one response says to do next.
#[derive(Debug, PartialEq)]
pub enum Turn {
    /// The answer is complete.
    Done {
        text: String,
        truncated: bool,
    },
    /// The server-side tool loop paused; resend with these blocks appended.
    Paused {
        content: Vec<Value>,
        text: String,
    },
    Refused,
}

/// Reads one response body. `stop_reason` decides before any content is
/// trusted.
pub fn parse_response(body: &Value) -> Result<Turn, Error> {
    let stop_reason = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if stop_reason == "refusal" {
        return Ok(Turn::Refused);
    }
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| Error::Api {
            status: 200,
            message: "the answer had no content".into(),
        })?;
    let text = text_of(&content);
    Ok(match stop_reason {
        "pause_turn" => Turn::Paused { content, text },
        "max_tokens" => Turn::Done {
            text,
            truncated: true,
        },
        _ => Turn::Done {
            text,
            truncated: false,
        },
    })
}

/// The text blocks, joined. Search results, tool calls, thinking and
/// fallback markers are not part of the answer. Cited passages arrive as
/// separate blocks mid-sentence, so they are joined without a separator.
pub fn text_of(content: &[Value]) -> String {
    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect()
}

/// Writes the briefing for `prompt`, resuming paused turns.
pub async fn generate(http: &reqwest::Client, key: &str, prompt: &str) -> Result<Answer, Error> {
    generate_at(http, URL, key, prompt).await
}

/// [`generate`] against `url`, which tests point at a local server.
async fn generate_at(
    http: &reqwest::Client,
    url: &str,
    key: &str,
    prompt: &str,
) -> Result<Answer, Error> {
    let user = user_message(prompt);
    let mut produced: Vec<Value> = Vec::new();
    let mut text = String::new();
    let mut messages = vec![user.clone()];
    for _ in 0..=MAX_CONTINUATIONS {
        let body = send(http, url, key, &request_body(&messages)).await?;
        match parse_response(&body)? {
            Turn::Refused => return Err(Error::Refused),
            Turn::Done {
                text: last,
                truncated,
            } => {
                text.push_str(&last);
                return finish(text, truncated);
            }
            Turn::Paused {
                content,
                text: partial,
            } => {
                text.push_str(&partial);
                produced.extend(content);
                messages = continuation(&user, &produced);
            }
        }
    }
    // Still searching after every continuation: show what was written.
    finish(text, true)
}

fn finish(text: String, truncated: bool) -> Result<Answer, Error> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Answer { text, truncated })
}

/// One request, retried for rate limits and overloaded servers.
async fn send(http: &reqwest::Client, url: &str, key: &str, body: &Value) -> Result<Value, Error> {
    let mut server_retries = 0;
    let mut waited_for_rate_limit = false;
    loop {
        let response = http
            .post(url)
            .timeout(TIMEOUT)
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", BETA)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|error| Error::Network(error.without_url().to_string()))?;
        let status = response.status().as_u16();
        let retry_after = retry_after(response.headers());
        if response.status().is_success() {
            return response
                .json::<Value>()
                .await
                .map_err(|error| Error::Network(error.without_url().to_string()));
        }
        let message = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|body| api_message(&body))
            .unwrap_or_default();
        match classify(status, message) {
            Failure::RateLimited if !waited_for_rate_limit => {
                waited_for_rate_limit = true;
                tokio::time::sleep(retry_after.unwrap_or(Duration::from_secs(10))).await;
            }
            Failure::Retry if server_retries < MAX_SERVER_RETRIES => {
                server_retries += 1;
                let backoff = Duration::from_secs(2u64.pow(server_retries));
                tokio::time::sleep(retry_after.unwrap_or(backoff).max(backoff)).await;
            }
            Failure::RateLimited => return Err(Error::RateLimited),
            Failure::Retry if status == 529 => return Err(Error::Overloaded),
            Failure::Retry => {
                return Err(Error::Api {
                    status,
                    message: String::from("server error"),
                });
            }
            Failure::Fatal(error) => return Err(error),
        }
    }
}

/// How an HTTP error status is handled.
#[derive(Debug, PartialEq)]
enum Failure {
    RateLimited,
    /// Overloaded or failing: worth trying again after a pause.
    Retry,
    Fatal(Error),
}

fn classify(status: u16, message: String) -> Failure {
    match status {
        401 | 403 => Failure::Fatal(Error::InvalidKey),
        429 => Failure::RateLimited,
        500..=599 => Failure::Retry,
        _ => Failure::Fatal(Error::Api { status, message }),
    }
}

/// The `error.message` of an error body.
fn api_message(body: &Value) -> Option<String> {
    body.pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The server's `retry-after`, in seconds, within reason.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| Duration::from_secs_f64(seconds).min(MAX_RETRY_AFTER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_has_the_exact_shape_the_api_expects() {
        let body = request_body(&[user_message("About this album")]);
        assert_eq!(body["model"], "claude-sonnet-5-5");
        assert_eq!(body["max_tokens"], 16_000);
        assert_eq!(body["output_config"], json!({ "effort": "medium" }));
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["system"], SYSTEM);
        assert_eq!(
            body["tools"],
            json!([{ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }])
        );
        assert_eq!(
            body["messages"],
            json!([{ "role": "user", "content": "About this album" }])
        );
        let keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut keys = keys;
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "fallbacks",
                "max_tokens",
                "messages",
                "model",
                "output_config",
                "system",
                "tools"
            ]
        );
    }

    #[test]
    fn nothing_the_model_rejects_is_sent() {
        let body = request_body(&[user_message("x")]);
        for field in ["thinking", "temperature", "top_p", "top_k"] {
            assert!(body.get(field).is_none(), "{field} must not be sent");
        }
        // No assistant prefill: the conversation ends with the user.
        assert_eq!(
            body["messages"].as_array().unwrap().last().unwrap()["role"],
            "user"
        );
    }

    #[test]
    fn the_beta_and_version_headers_are_the_documented_ones() {
        assert_eq!(BETA, "server-side-fallback-2026-07-01");
        assert_eq!(API_VERSION, "2023-06-01");
        assert_eq!(URL, "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn only_text_blocks_make_the_answer() {
        let body = json!({
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "", "signature": "x" },
                { "type": "server_tool_use", "id": "s1", "name": "web_search", "input": { "query": "q" } },
                { "type": "web_search_tool_result", "tool_use_id": "s1", "content": { "type": "web_search_tool_result_error", "error_code": "max_uses_exceeded" } },
                { "type": "fallback", "from": { "model": "a" }, "to": { "model": "b" } },
                { "type": "text", "text": "## Overview\nProduced by " },
                { "type": "text", "text": "Prince", "citations": [] },
                { "type": "text", "text": "." }
            ]
        });
        assert_eq!(
            parse_response(&body).unwrap(),
            Turn::Done {
                text: "## Overview\nProduced by Prince.".into(),
                truncated: false
            }
        );
    }

    #[test]
    fn a_refusal_is_reported_before_reading_content() {
        let body = json!({ "stop_reason": "refusal", "content": [], "stop_details": null });
        assert_eq!(parse_response(&body).unwrap(), Turn::Refused);
        let without_content = json!({ "stop_reason": "refusal" });
        assert_eq!(parse_response(&without_content).unwrap(), Turn::Refused);
    }

    #[test]
    fn max_tokens_keeps_the_text_and_marks_it_truncated() {
        let body = json!({
            "stop_reason": "max_tokens",
            "content": [{ "type": "text", "text": "Half an answer" }]
        });
        assert_eq!(
            parse_response(&body).unwrap(),
            Turn::Done {
                text: "Half an answer".into(),
                truncated: true
            }
        );
    }

    #[test]
    fn a_paused_turn_resumes_with_the_full_content_and_no_new_user_message() {
        let content = vec![
            json!({ "type": "thinking", "thinking": "", "signature": "sig" }),
            json!({ "type": "server_tool_use", "id": "s1", "name": "web_search", "input": { "query": "q" } }),
        ];
        let body = json!({ "stop_reason": "pause_turn", "content": content });
        let Turn::Paused {
            content: paused,
            text,
        } = parse_response(&body).unwrap()
        else {
            panic!("expected a paused turn");
        };
        assert_eq!(text, "");
        let user = user_message("About this album");
        let messages = continuation(&user, &paused);
        assert_eq!(
            Value::Array(messages.clone()),
            json!([
                { "role": "user", "content": "About this album" },
                { "role": "assistant", "content": content }
            ])
        );
        // The resumed request ends with the assistant turn, unchanged.
        let resumed = request_body(&messages);
        assert_eq!(resumed["messages"][1]["content"][0]["signature"], "sig");
        assert_eq!(resumed["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn content_missing_from_a_normal_answer_is_an_error() {
        assert!(parse_response(&json!({ "stop_reason": "end_turn" })).is_err());
    }

    #[test]
    fn statuses_map_to_useful_errors() {
        assert_eq!(
            classify(401, String::new()),
            Failure::Fatal(Error::InvalidKey)
        );
        assert_eq!(classify(429, String::new()), Failure::RateLimited);
        assert_eq!(classify(529, String::new()), Failure::Retry);
        assert_eq!(classify(500, String::new()), Failure::Retry);
        assert_eq!(
            classify(400, "bad".into()),
            Failure::Fatal(Error::Api {
                status: 400,
                message: "bad".into()
            })
        );
    }

    #[test]
    fn error_messages_and_retry_after_are_read() {
        let body = json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "slow down" } });
        assert_eq!(api_message(&body).as_deref(), Some("slow down"));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(7)));
        headers.insert(reqwest::header::RETRY_AFTER, "100000".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(MAX_RETRY_AFTER));
        headers.insert(reqwest::header::RETRY_AFTER, "soon".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn empty_answers_are_errors() {
        assert_eq!(finish("  \n".into(), false), Err(Error::Empty));
        assert_eq!(
            finish(" ok ".into(), true),
            Ok(Answer {
                text: "ok".into(),
                truncated: true
            })
        );
    }

    // ---- against a local server ------------------------------------------

    /// One request as the server saw it.
    struct Seen {
        method: String,
        path: String,
        /// Header names in lower case.
        headers: Vec<(String, String)>,
        body: Value,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }
    }

    /// A canned answer: status, extra headers, JSON body.
    type Reply = (u16, Vec<(&'static str, &'static str)>, Value);

    /// Serves `replies` in order, one connection each, and records what
    /// arrived. Returns the messages URL and the record.
    async fn serve(replies: Vec<Reply>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Seen>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            for (status, headers, body) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let mut buffer = [0u8; 8192];
                let split = loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    assert!(read > 0, "the client hung up mid-request");
                    raw.extend_from_slice(&buffer[..read]);
                    if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at;
                    }
                };
                let head = String::from_utf8(raw[..split].to_vec()).unwrap();
                let mut lines = head.split("\r\n");
                let mut start = lines.next().unwrap().split(' ');
                let method = start.next().unwrap().to_string();
                let path = start.next().unwrap().to_string();
                let headers_in: Vec<(String, String)> = lines
                    .filter_map(|line| line.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let length: usize = headers_in
                    .iter()
                    .find(|(k, _)| k == "content-length")
                    .map(|(_, v)| v.parse().unwrap())
                    .expect("the body has a length");
                let mut body_in = raw[split + 4..].to_vec();
                while body_in.len() < length {
                    let read = socket.read(&mut buffer).await.unwrap();
                    body_in.extend_from_slice(&buffer[..read]);
                }
                record.lock().unwrap().push(Seen {
                    method,
                    path,
                    headers: headers_in,
                    body: serde_json::from_slice(&body_in).unwrap(),
                });
                let text = body.to_string();
                let mut response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                    text.len()
                );
                for (name, value) in headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(&text);
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.ok();
            }
        });
        (url, seen)
    }

    /// A client that reaches the local server even behind a proxy.
    fn local() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn reply(stop_reason: &str, content: Value) -> Reply {
        (
            200,
            Vec::new(),
            json!({
                "id": "msg_1", "type": "message", "role": "assistant",
                "model": MODEL, "stop_reason": stop_reason, "stop_details": null,
                "content": content,
            }),
        )
    }

    #[tokio::test]
    async fn paused_turns_are_resumed_with_everything_produced_so_far() {
        let first = json!([
            { "type": "thinking", "thinking": "", "signature": "sig-1" },
            { "type": "text", "text": "Sign o' the Times is " },
            { "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": { "query": "Sign o' the Times producer" } }
        ]);
        let second = json!([
            { "type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [
                { "type": "web_search_result", "url": "https://example.org", "title": "x", "encrypted_content": "e" }
            ] },
            { "type": "text", "text": "a 1987 double album" },
            { "type": "server_tool_use", "id": "srvtoolu_2", "name": "web_search", "input": { "query": "reception" } }
        ]);
        let last = json!([
            { "type": "web_search_tool_result", "tool_use_id": "srvtoolu_2", "content": [] },
            { "type": "text", "text": " produced by **Prince**." }
        ]);
        let (url, seen) = serve(vec![
            reply("pause_turn", first.clone()),
            reply("pause_turn", second.clone()),
            reply("end_turn", last),
        ])
        .await;
        let answer = generate_at(&local(), &url, "sk-ant-test", "About this album")
            .await
            .unwrap();
        assert_eq!(
            answer,
            Answer {
                text: "Sign o' the Times is a 1987 double album produced by **Prince**.".into(),
                truncated: false
            }
        );

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        for request in seen.iter() {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/v1/messages");
            assert_eq!(request.header("x-api-key"), Some("sk-ant-test"));
            assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
            assert_eq!(
                request.header("anthropic-beta"),
                Some("server-side-fallback-2026-07-01")
            );
            assert_eq!(request.header("content-type"), Some("application/json"));
            assert!(request.header("authorization").is_none());
        }
        let user = json!({ "role": "user", "content": "About this album" });
        assert_eq!(seen[0].body, request_body(std::slice::from_ref(&user)));
        // Each resumption repeats the question and every block so far,
        // unchanged, as one assistant turn, with no new user message.
        let mut so_far: Vec<Value> = first.as_array().unwrap().clone();
        assert_eq!(
            seen[1].body["messages"],
            json!([user, { "role": "assistant", "content": so_far }])
        );
        so_far.extend(second.as_array().unwrap().iter().cloned());
        assert_eq!(
            seen[2].body,
            request_body(&[
                user.clone(),
                json!({ "role": "assistant", "content": so_far })
            ])
        );
    }

    #[tokio::test]
    async fn a_rate_limit_waits_for_retry_after_once() {
        let limited: Reply = (
            429,
            vec![("retry-after", "1")],
            json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "slow down" } }),
        );
        let (url, seen) = serve(vec![
            limited.clone(),
            reply("end_turn", json!([{ "type": "text", "text": "ok" }])),
        ])
        .await;
        let started = std::time::Instant::now();
        let answer = generate_at(&local(), &url, "k", "q").await;
        assert_eq!(answer.unwrap().text, "ok");
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert_eq!(seen.lock().unwrap().len(), 2);

        let quick: Reply = (429, vec![("retry-after", "0")], limited.2.clone());
        let (url, seen) = serve(vec![quick.clone(), quick]).await;
        let answer = generate_at(&local(), &url, "k", "q").await;
        assert_eq!(answer, Err(Error::RateLimited));
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_rejected_key_is_reported_without_the_key() {
        let rejected: Reply = (
            401,
            Vec::new(),
            json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } }),
        );
        let (url, _) = serve(vec![rejected]).await;
        let error = generate_at(&local(), &url, "sk-ant-secret", "q")
            .await
            .unwrap_err();
        assert_eq!(error, Error::InvalidKey);
        assert!(!format!("{error} {error:?}").contains("sk-ant-secret"));
    }

    #[tokio::test]
    async fn a_refusal_and_a_cut_off_answer_are_reported() {
        let (url, _) = serve(vec![
            reply("refusal", json!([])),
            reply("max_tokens", json!([{ "type": "text", "text": "Half" }])),
        ])
        .await;
        let http = local();
        assert_eq!(
            generate_at(&http, &url, "k", "q").await,
            Err(Error::Refused)
        );
        assert_eq!(
            generate_at(&http, &url, "k", "q").await,
            Ok(Answer {
                text: "Half".into(),
                truncated: true
            })
        );
    }
}
