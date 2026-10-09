//! The Claude Messages API, over plain HTTP: there is no official Rust SDK.
//!
//! One briefing is one conversation of a single user message, streamed as
//! server-sent events so the panel shows the answer while it is written
//! ([`super::sse`] splits the stream into events, [`super::stream`] rebuilds
//! the answer from them). A deep briefing may search the web on Anthropic's
//! side while it writes; when that loop runs long the answer stops with
//! `pause_turn`, and the conversation is sent again with everything Claude
//! produced so far so it can carry on.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::Instant;

use super::prompt;
use super::stream::{self, Message, Update};
use super::{Error, Mode, Phase, Progress, sse};

const URL: &str = "https://api.anthropic.com/v1/messages";
pub const MODEL: &str = "claude-sonnet-5-5";
const API_VERSION: &str = "2023-06-01";
/// Lets Anthropic answer a declined request with its recommended fallback
/// model instead of returning the refusal.
const BETA: &str = "server-side-fallback-2026-07-01";
/// A quick briefing is 200 to 300 words; this leaves room for thinking.
const QUICK_MAX_TOKENS: u32 = 4096;
const DEEP_MAX_TOKENS: u32 = 16_000;
/// How long the response may take to begin: connecting, sending the
/// question, and the reply's headers.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest silence once the stream has begun. Anthropic sends pings
/// while it thinks or searches, so a silence this long means the connection
/// is gone. There is no limit on the whole answer: it keeps arriving.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Text is handed to the panel at most this often, so a fast stream does
/// not redraw the window for every word.
const FLUSH_EVERY: Duration = Duration::from_millis(50);
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

/// The JSON body for the conversation so far. A quick briefing has no
/// tools and little thinking; `thinking` is left out in both modes, so the
/// model thinks adaptively as much as the effort asks.
pub fn request_body(mode: Mode, messages: &[Value]) -> Value {
    let (max_tokens, effort) = match mode {
        Mode::Quick => (QUICK_MAX_TOKENS, "low"),
        Mode::Deep => (DEEP_MAX_TOKENS, "medium"),
    };
    let mut body = json!({
        "model": MODEL,
        "max_tokens": max_tokens,
        "stream": true,
        "output_config": { "effort": effort },
        "fallbacks": "default",
        "system": prompt::system(mode),
        "messages": messages,
    });
    if mode == Mode::Deep {
        body["tools"] = json!([{
            "type": "web_search_20260209",
            "name": "web_search",
            "max_uses": 5,
        }]);
    }
    body
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

/// How long to wait; tests shorten these.
#[derive(Clone, Copy, Debug)]
struct Timing {
    response: Duration,
    idle: Duration,
}

const TIMING: Timing = Timing {
    response: RESPONSE_TIMEOUT,
    idle: IDLE_TIMEOUT,
};

/// Writes the briefing for `prompt`, resuming paused turns. Text and
/// changes of phase go to `progress` as they arrive.
pub async fn generate(
    http: &reqwest::Client,
    key: &str,
    mode: Mode,
    prompt: &str,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Answer, Error> {
    generate_at(http, URL, key, mode, prompt, TIMING, progress).await
}

/// [`generate`] against `url`, which tests point at a local server.
async fn generate_at(
    http: &reqwest::Client,
    url: &str,
    key: &str,
    mode: Mode,
    prompt: &str,
    timing: Timing,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Answer, Error> {
    let user = user_message(prompt);
    let mut produced: Vec<Value> = Vec::new();
    let mut text = String::new();
    let mut out = Emitter::new(progress);
    for _ in 0..=MAX_CONTINUATIONS {
        let messages = if produced.is_empty() {
            vec![user.clone()]
        } else {
            continuation(&user, &stream::echoable(&produced))
        };
        let body = request_body(mode, &messages);
        let message = turn(http, url, key, &body, timing, &mut out).await?;
        out.flush();
        text.push_str(message.text());
        // The stop reason decides before any content is trusted.
        match message.stop_reason() {
            // Whatever was shown is thrown away with the refusal.
            Some("refusal") => return Err(Error::Refused),
            Some("pause_turn") => produced.extend(message.content()),
            Some("max_tokens") => return finish(text, true),
            _ => return finish(text, false),
        }
    }
    // Still searching after every continuation: keep what was written.
    finish(text, true)
}

fn finish(text: String, truncated: bool) -> Result<Answer, Error> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Answer { text, truncated })
}

/// Hands text to the panel in batches, and phases as they change.
struct Emitter<'a> {
    sink: &'a mut (dyn FnMut(Progress) + Send),
    pending: String,
    flushed_at: Instant,
    phase: Phase,
}

impl<'a> Emitter<'a> {
    fn new(sink: &'a mut (dyn FnMut(Progress) + Send)) -> Self {
        Self {
            sink,
            pending: String::new(),
            flushed_at: Instant::now(),
            phase: Phase::Writing,
        }
    }

    fn text(&mut self, text: &str) {
        self.pending.push_str(text);
        if self.flushed_at.elapsed() >= FLUSH_EVERY {
            self.flush();
        }
    }

    fn phase(&mut self, phase: Phase) {
        if phase != self.phase {
            self.flush();
            self.phase = phase;
            (self.sink)(Progress::Phase(phase));
        }
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            (self.sink)(Progress::Text(std::mem::take(&mut self.pending)));
        }
        self.flushed_at = Instant::now();
    }

    /// When the text held back is due, if there is any.
    fn due(&self) -> Option<Instant> {
        (!self.pending.is_empty()).then(|| self.flushed_at + FLUSH_EVERY)
    }
}

/// One response, retried for rate limits and overloaded servers. A stream
/// that fails after it has shown text is not retried: the text would
/// repeat.
async fn turn(
    http: &reqwest::Client,
    url: &str,
    key: &str,
    body: &Value,
    timing: Timing,
    out: &mut Emitter<'_>,
) -> Result<Message, Error> {
    let mut server_retries = 0;
    let mut waited_for_rate_limit = false;
    loop {
        let request = http
            .post(url)
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", BETA)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .json(body)
            .send();
        let response = tokio::time::timeout(timing.response, request)
            .await
            .map_err(|_| Error::Network("Anthropic did not answer in time".into()))?
            .map_err(|error| Error::Network(error.without_url().to_string()))?;
        let status = response.status().as_u16();
        let retry_after = retry_after(response.headers());
        let failure = if response.status().is_success() {
            match read(response, timing.idle, out).await {
                Ok(message) => return Ok(message),
                Err(Interrupted { error, wrote }) => {
                    if !wrote && stream::retryable(&error) {
                        Failure::Retry(error)
                    } else {
                        return Err(error);
                    }
                }
            }
        } else {
            let message = tokio::time::timeout(timing.idle, response.json::<Value>())
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|body| api_message(&body))
                .unwrap_or_default();
            classify(status, message)
        };
        match failure {
            Failure::RateLimited if !waited_for_rate_limit => {
                waited_for_rate_limit = true;
                tokio::time::sleep(retry_after.unwrap_or(Duration::from_secs(10))).await;
            }
            Failure::Retry(_) if server_retries < MAX_SERVER_RETRIES => {
                server_retries += 1;
                let backoff = Duration::from_secs(2u64.pow(server_retries));
                tokio::time::sleep(retry_after.unwrap_or(backoff).max(backoff)).await;
            }
            Failure::RateLimited => return Err(Error::RateLimited),
            Failure::Retry(error) | Failure::Fatal(error) => return Err(error),
        }
    }
}

/// A stream that broke off, and whether it had shown any text by then.
struct Interrupted {
    error: Error,
    wrote: bool,
}

/// Reads one streamed response to its end, passing text on as it comes.
async fn read(
    mut response: reqwest::Response,
    idle: Duration,
    out: &mut Emitter<'_>,
) -> Result<Message, Interrupted> {
    let mut parser = sse::Parser::default();
    let mut message = Message::default();
    let mut wrote = false;
    let mut heard = Instant::now();
    loop {
        let give_up = heard + idle;
        let wake = out.due().map_or(give_up, |due| due.min(give_up));
        let chunk = match tokio::time::timeout_at(wake, response.chunk()).await {
            // Quiet for a moment: show the text held back, keep waiting.
            Err(_) if Instant::now() < give_up => {
                out.flush();
                continue;
            }
            Err(_) => {
                return Err(Interrupted {
                    error: Error::Network("the answer stopped arriving".into()),
                    wrote,
                });
            }
            Ok(Err(error)) => {
                return Err(Interrupted {
                    error: Error::Network(error.without_url().to_string()),
                    wrote,
                });
            }
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => chunk,
        };
        heard = Instant::now();
        for event in parser.push(&chunk) {
            handle(&event, &mut message, out, &mut wrote)?;
            if message.is_complete() {
                return Ok(message);
            }
        }
    }
    if let Some(event) = parser.finish() {
        handle(&event, &mut message, out, &mut wrote)?;
    }
    if message.is_complete() {
        Ok(message)
    } else {
        Err(Interrupted {
            error: Error::Network("the answer was cut off".into()),
            wrote,
        })
    }
}

fn handle(
    event: &sse::Event,
    message: &mut Message,
    out: &mut Emitter<'_>,
    wrote: &mut bool,
) -> Result<(), Interrupted> {
    let interrupted = |error, wrote: &mut bool| Interrupted {
        error,
        wrote: *wrote,
    };
    let data: Value = serde_json::from_str(&event.data).map_err(|_| {
        interrupted(
            Error::Api {
                status: 200,
                message: "unreadable event in the answer".into(),
            },
            wrote,
        )
    })?;
    match message.apply(&data) {
        Ok(Update::Text(text)) => {
            *wrote = true;
            out.text(&text);
        }
        Ok(Update::Searching) => out.phase(Phase::Searching),
        Ok(Update::Writing) => out.phase(Phase::Writing),
        Ok(Update::Nothing | Update::Stopped) => {}
        Err(error) => return Err(interrupted(error, wrote)),
    }
    Ok(())
}

/// How an HTTP error status is handled.
#[derive(Debug, PartialEq)]
enum Failure {
    RateLimited,
    /// Overloaded or failing: worth trying again after a pause. The error
    /// is reported if the retries run out.
    Retry(Error),
    Fatal(Error),
}

fn classify(status: u16, message: String) -> Failure {
    match status {
        401 | 403 => Failure::Fatal(Error::InvalidKey),
        429 => Failure::RateLimited,
        529 => Failure::Retry(Error::Overloaded),
        500..=599 => Failure::Retry(Error::Api {
            status,
            message: String::from("server error"),
        }),
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
    use crate::insights::prompt::{DEEP_SYSTEM, QUICK_SYSTEM};

    fn keys(body: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn a_quick_request_has_no_tools_and_low_effort() {
        let body = request_body(Mode::Quick, &[user_message("About this album")]);
        assert_eq!(body["model"], "claude-sonnet-5-5");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["stream"], true);
        assert_eq!(body["output_config"], json!({ "effort": "low" }));
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["system"], QUICK_SYSTEM);
        assert_eq!(
            body["messages"],
            json!([{ "role": "user", "content": "About this album" }])
        );
        assert_eq!(
            keys(&body),
            [
                "fallbacks",
                "max_tokens",
                "messages",
                "model",
                "output_config",
                "stream",
                "system"
            ]
        );
    }

    #[test]
    fn a_deep_request_searches_the_web_at_medium_effort() {
        let body = request_body(Mode::Deep, &[user_message("About this album")]);
        assert_eq!(body["model"], "claude-sonnet-5-5");
        assert_eq!(body["max_tokens"], 16_000);
        assert_eq!(body["stream"], true);
        assert_eq!(body["output_config"], json!({ "effort": "medium" }));
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["system"], DEEP_SYSTEM);
        assert_eq!(
            body["tools"],
            json!([{ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }])
        );
        assert_eq!(
            keys(&body),
            [
                "fallbacks",
                "max_tokens",
                "messages",
                "model",
                "output_config",
                "stream",
                "system",
                "tools"
            ]
        );
    }

    #[test]
    fn nothing_the_model_rejects_is_sent() {
        for mode in [Mode::Quick, Mode::Deep] {
            let body = request_body(mode, &[user_message("x")]);
            // `thinking` is omitted: `disabled` is a 400 on this model.
            for field in ["thinking", "temperature", "top_p", "top_k", "tool_choice"] {
                assert!(body.get(field).is_none(), "{field} must not be sent");
            }
            // No assistant prefill: the conversation ends with the user.
            assert_eq!(
                body["messages"].as_array().unwrap().last().unwrap()["role"],
                "user"
            );
        }
    }

    #[test]
    fn the_beta_and_version_headers_are_the_documented_ones() {
        assert_eq!(BETA, "server-side-fallback-2026-07-01");
        assert_eq!(API_VERSION, "2023-06-01");
        assert_eq!(URL, "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn statuses_map_to_useful_errors() {
        assert_eq!(
            classify(401, String::new()),
            Failure::Fatal(Error::InvalidKey)
        );
        assert_eq!(classify(429, String::new()), Failure::RateLimited);
        assert_eq!(
            classify(529, String::new()),
            Failure::Retry(Error::Overloaded)
        );
        assert!(matches!(
            classify(500, String::new()),
            Failure::Retry(Error::Api { status: 500, .. })
        ));
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

    #[test]
    fn a_paused_turn_resumes_with_the_full_content_and_no_new_user_message() {
        let content = vec![
            json!({ "type": "thinking", "thinking": "", "signature": "sig" }),
            json!({ "type": "server_tool_use", "id": "s1", "name": "web_search", "input": { "query": "q" } }),
        ];
        let user = user_message("About this album");
        let messages = continuation(&user, &content);
        assert_eq!(
            Value::Array(messages.clone()),
            json!([
                { "role": "user", "content": "About this album" },
                { "role": "assistant", "content": content }
            ])
        );
        let resumed = request_body(Mode::Deep, &messages);
        assert_eq!(resumed["messages"][1]["content"][0]["signature"], "sig");
        assert_eq!(resumed["messages"].as_array().unwrap().len(), 2);
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

    /// A canned answer.
    #[derive(Clone)]
    struct Reply {
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        content_type: &'static str,
        body: String,
        /// Hold the connection open after the body instead of closing it.
        hang: bool,
    }

    /// An error answer with a JSON body.
    fn json_reply(status: u16, headers: Vec<(&'static str, &'static str)>, body: Value) -> Reply {
        Reply {
            status,
            headers,
            content_type: "application/json",
            body: body.to_string(),
            hang: false,
        }
    }

    /// A 200 that streams `events`.
    fn sse_reply(events: &[Value]) -> Reply {
        Reply {
            status: 200,
            headers: Vec::new(),
            content_type: "text/event-stream",
            body: sse_text(events),
            hang: false,
        }
    }

    /// Events as the API frames them, with CRLF line ends to keep the
    /// parser honest.
    fn sse_text(events: &[Value]) -> String {
        events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\r\ndata: {}\r\n\r\n",
                    event["type"].as_str().unwrap(),
                    event
                )
            })
            .collect()
    }

    /// The events that stream `content` with `stop_reason`, piece by piece
    /// the way the API sends each kind of block.
    fn events_for(stop_reason: &str, content: &Value) -> Vec<Value> {
        let mut events = vec![
            json!({ "type": "message_start", "message": {
                "id": "msg_1", "type": "message", "role": "assistant", "model": MODEL,
                "content": [], "stop_reason": null, "usage": { "input_tokens": 10, "output_tokens": 1 }
            }}),
            json!({ "type": "ping" }),
        ];
        for (index, block) in content.as_array().unwrap().iter().enumerate() {
            let halves = |text: &str| {
                let middle = text
                    .char_indices()
                    .map(|(at, _)| at)
                    .nth(text.chars().count() / 2)
                    .unwrap_or(0);
                [text[..middle].to_string(), text[middle..].to_string()]
            };
            let delta = |delta: Value| json!({ "type": "content_block_delta", "index": index, "delta": delta });
            match block["type"].as_str().unwrap() {
                "text" => {
                    let mut start = json!({ "type": "text", "text": "" });
                    if block.get("citations").is_some() {
                        start["citations"] = json!([]);
                    }
                    events.push(json!({ "type": "content_block_start", "index": index, "content_block": start }));
                    for citation in block["citations"].as_array().into_iter().flatten() {
                        events.push(delta(json!({ "type": "citations_delta", "citation": citation })));
                    }
                    for piece in halves(block["text"].as_str().unwrap()) {
                        events.push(delta(json!({ "type": "text_delta", "text": piece })));
                    }
                }
                "server_tool_use" => {
                    let mut start = block.clone();
                    start["input"] = json!({});
                    events.push(json!({ "type": "content_block_start", "index": index, "content_block": start }));
                    for piece in halves(&block["input"].to_string()) {
                        events.push(delta(json!({ "type": "input_json_delta", "partial_json": piece })));
                    }
                }
                "thinking" => {
                    events.push(json!({ "type": "content_block_start", "index": index,
                        "content_block": { "type": "thinking", "thinking": "", "signature": "" } }));
                    for piece in halves(block["thinking"].as_str().unwrap()) {
                        events.push(delta(json!({ "type": "thinking_delta", "thinking": piece })));
                    }
                    events.push(delta(json!({ "type": "signature_delta", "signature": block["signature"] })));
                }
                _ => events.push(json!({ "type": "content_block_start", "index": index, "content_block": block })),
            }
            events.push(json!({ "type": "content_block_stop", "index": index }));
        }
        events.push(json!({ "type": "message_delta",
            "delta": { "stop_reason": stop_reason, "stop_sequence": null },
            "usage": { "output_tokens": 42 } }));
        events.push(json!({ "type": "message_stop" }));
        events
    }

    fn streamed(stop_reason: &str, content: Value) -> Reply {
        sse_reply(&events_for(stop_reason, &content))
    }

    /// Serves `replies` in order, one connection each, and records what
    /// arrived. A streamed body is written in small pieces with pauses, as
    /// a network would deliver it. Returns the messages URL and the record.
    async fn serve(replies: Vec<Reply>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Seen>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                socket.set_nodelay(true).unwrap();
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
                let mut head = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: {}\r\nconnection: close\r\n",
                    reply.status, reply.content_type
                );
                if !reply.hang {
                    head.push_str(&format!("content-length: {}\r\n", reply.body.len()));
                }
                for (name, value) in &reply.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                if socket.write_all(head.as_bytes()).await.is_err() {
                    continue;
                }
                // Odd-sized pieces split events, lines and characters.
                for piece in reply.body.as_bytes().chunks(97) {
                    if socket.write_all(piece).await.is_err() {
                        break;
                    }
                    socket.flush().await.ok();
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                if reply.hang {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                socket.shutdown().await.ok();
            }
        });
        (url, seen)
    }

    /// A client that reaches the local server even behind a proxy.
    fn local() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    /// Runs a generation against `url`, collecting what it reported.
    async fn run(url: &str, mode: Mode, prompt: &str) -> (Result<Answer, Error>, Vec<Progress>) {
        run_with(url, mode, prompt, TIMING).await
    }

    async fn run_with(
        url: &str,
        mode: Mode,
        prompt: &str,
        timing: Timing,
    ) -> (Result<Answer, Error>, Vec<Progress>) {
        let mut reported = Vec::new();
        let mut sink = |progress| reported.push(progress);
        let result = generate_at(
            &local(),
            url,
            "sk-ant-test",
            mode,
            prompt,
            timing,
            &mut sink,
        )
        .await;
        (result, reported)
    }

    fn shown(reported: &[Progress]) -> String {
        reported
            .iter()
            .filter_map(|progress| match progress {
                Progress::Text(text) => Some(text.as_str()),
                Progress::Phase(_) => None,
            })
            .collect()
    }

    fn check_headers(request: &Seen) {
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

    #[tokio::test]
    async fn a_quick_answer_streams_into_the_panel() {
        let (url, seen) = serve(vec![streamed(
            "end_turn",
            json!([
                { "type": "thinking", "thinking": "", "signature": "sig" },
                { "type": "text", "text": "## Caf\u{e9} Tacvba\nProduced by **Gustavo Santaolalla** \u{2605}." }
            ]),
        )])
        .await;
        let (result, reported) = run(&url, Mode::Quick, "About this album").await;
        let text = "## Caf\u{e9} Tacvba\nProduced by **Gustavo Santaolalla** \u{2605}.";
        assert_eq!(
            result,
            Ok(Answer {
                text: text.into(),
                truncated: false
            })
        );
        assert_eq!(shown(&reported), text);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        check_headers(&seen[0]);
        let body = &seen[0].body;
        assert_eq!(
            body,
            &request_body(Mode::Quick, &[user_message("About this album")])
        );
        assert_eq!(body["stream"], true);
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("tools").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[tokio::test]
    async fn paused_turns_are_resumed_with_the_rebuilt_blocks() {
        let citation = json!({
            "type": "web_search_result_location", "url": "https://example.org",
            "title": "Example", "encrypted_index": "idx", "cited_text": "a 1987 double album"
        });
        let first = json!([
            { "type": "thinking", "thinking": "Looking up the producer.", "signature": "sig-1" },
            { "type": "text", "text": "Sign o' the Times is " },
            { "type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
              "input": { "query": "Sign o' the Times producer \u{e9}" } }
        ]);
        let second = json!([
            { "type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [
                { "type": "web_search_result", "url": "https://example.org", "title": "x",
                  "encrypted_content": "e", "page_age": null }
            ] },
            { "type": "text", "text": "a 1987 double album", "citations": [citation] },
            { "type": "server_tool_use", "id": "srvtoolu_2", "name": "web_search", "input": { "query": "reception" } }
        ]);
        let last = json!([
            { "type": "web_search_tool_result", "tool_use_id": "srvtoolu_2", "content": [] },
            { "type": "text", "text": " produced by **Prince**." }
        ]);
        let (url, seen) = serve(vec![
            streamed("pause_turn", first.clone()),
            streamed("pause_turn", second.clone()),
            streamed("end_turn", last),
        ])
        .await;
        let (result, reported) = run(&url, Mode::Deep, "About this album").await;
        let text = "Sign o' the Times is a 1987 double album produced by **Prince**.";
        assert_eq!(
            result,
            Ok(Answer {
                text: text.into(),
                truncated: false
            })
        );
        // Text shown before a pause stays, and the rest follows it.
        assert_eq!(shown(&reported), text);
        assert!(reported.contains(&Progress::Phase(Phase::Searching)));
        assert_eq!(
            reported.last(),
            Some(&Progress::Text(" produced by **Prince**.".into()))
        );

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        for request in seen.iter() {
            check_headers(request);
        }
        let user = json!({ "role": "user", "content": "About this album" });
        assert_eq!(
            seen[0].body,
            request_body(Mode::Deep, std::slice::from_ref(&user))
        );
        // Each resumption repeats the question and every block so far,
        // rebuilt from the stream exactly as the API sent them, as one
        // assistant turn, with no new user message.
        let mut so_far: Vec<Value> = first.as_array().unwrap().clone();
        assert_eq!(
            seen[1].body,
            request_body(
                Mode::Deep,
                &[
                    user.clone(),
                    json!({ "role": "assistant", "content": so_far })
                ]
            )
        );
        so_far.extend(second.as_array().unwrap().iter().cloned());
        assert_eq!(
            seen[2].body,
            request_body(
                Mode::Deep,
                &[
                    user.clone(),
                    json!({ "role": "assistant", "content": so_far })
                ]
            )
        );
    }

    #[tokio::test]
    async fn resuming_stops_after_five_continuations() {
        let paused = streamed("pause_turn", json!([{ "type": "text", "text": "x" }]));
        let (url, seen) = serve(vec![paused; MAX_CONTINUATIONS + 1]).await;
        let (result, _) = run(&url, Mode::Deep, "q").await;
        assert_eq!(
            result,
            Ok(Answer {
                text: "xxxxxx".into(),
                truncated: true
            })
        );
        assert_eq!(seen.lock().unwrap().len(), MAX_CONTINUATIONS + 1);
    }

    #[tokio::test]
    async fn a_refusal_mid_stream_discards_the_text() {
        let (url, _) = serve(vec![streamed(
            "refusal",
            json!([{ "type": "text", "text": "Here is the start of" }]),
        )])
        .await;
        let (result, reported) = run(&url, Mode::Quick, "q").await;
        assert_eq!(result, Err(Error::Refused));
        // The panel saw some text, which the error then replaces.
        assert!(!shown(&reported).is_empty());
    }

    #[tokio::test]
    async fn a_cut_off_answer_keeps_its_text() {
        let (url, _) = serve(vec![streamed(
            "max_tokens",
            json!([{ "type": "text", "text": "Half" }]),
        )])
        .await;
        let (result, _) = run(&url, Mode::Quick, "q").await;
        assert_eq!(
            result,
            Ok(Answer {
                text: "Half".into(),
                truncated: true
            })
        );
    }

    fn overloaded_event() -> Value {
        json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } })
    }

    #[tokio::test]
    async fn an_error_event_before_any_text_is_retried() {
        let mut events = events_for(
            "end_turn",
            &json!([{ "type": "thinking", "thinking": "", "signature": "s" }]),
        );
        // Overloaded after a few events, before any text.
        events.truncate(4);
        events.push(overloaded_event());
        let (url, seen) = serve(vec![
            sse_reply(&events),
            streamed("end_turn", json!([{ "type": "text", "text": "ok" }])),
        ])
        .await;
        let (result, reported) = run(&url, Mode::Quick, "q").await;
        assert_eq!(result.unwrap().text, "ok");
        assert_eq!(shown(&reported), "ok");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].body, seen[1].body);
    }

    #[tokio::test]
    async fn an_error_event_after_text_is_reported() {
        let mut events = events_for(
            "end_turn",
            &json!([{ "type": "text", "text": "Already on screen" }]),
        );
        // Overloaded right after the text deltas.
        let at = events
            .iter()
            .rposition(|event| event["delta"]["type"] == "text_delta")
            .unwrap();
        events.truncate(at + 1);
        events.push(overloaded_event());
        let (url, seen) = serve(vec![sse_reply(&events)]).await;
        let (result, _) = run(&url, Mode::Quick, "q").await;
        // Text had arrived, so trying again would show it twice.
        assert_eq!(result, Err(Error::Overloaded));
        assert_eq!(seen.lock().unwrap().len(), 1);

        let invalid = json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "bad" } });
        let (url, seen) = serve(vec![sse_reply(&[invalid])]).await;
        let (result, _) = run(&url, Mode::Quick, "q").await;
        assert_eq!(
            result,
            Err(Error::Api {
                status: 400,
                message: "bad".into()
            })
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_stream_that_ends_early_or_goes_quiet_is_an_error() {
        let mut events = events_for(
            "end_turn",
            &json!([{ "type": "text", "text": "Never finished" }]),
        );
        events.truncate(events.len() - 2);
        let (url, _) = serve(vec![sse_reply(&events)]).await;
        let (result, _) = run(&url, Mode::Quick, "q").await;
        assert!(matches!(result, Err(Error::Network(_))), "{result:?}");

        let mut quiet = sse_reply(&events);
        quiet.hang = true;
        let (url, _) = serve(vec![quiet]).await;
        let timing = Timing {
            response: Duration::from_secs(5),
            idle: Duration::from_millis(300),
        };
        let started = std::time::Instant::now();
        let (result, reported) = run_with(&url, Mode::Quick, "q", timing).await;
        assert_eq!(
            result,
            Err(Error::Network("the answer stopped arriving".into()))
        );
        assert!(started.elapsed() < Duration::from_secs(4));
        // What arrived before the silence was still shown in time.
        assert_eq!(shown(&reported), "Never finished");
    }

    #[tokio::test]
    async fn a_rate_limit_waits_for_retry_after_once() {
        let limited = json_reply(
            429,
            vec![("retry-after", "1")],
            json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "slow down" } }),
        );
        let (url, seen) = serve(vec![
            limited.clone(),
            streamed("end_turn", json!([{ "type": "text", "text": "ok" }])),
        ])
        .await;
        let started = std::time::Instant::now();
        let (result, _) = run(&url, Mode::Quick, "q").await;
        assert_eq!(result.unwrap().text, "ok");
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert_eq!(seen.lock().unwrap().len(), 2);

        let mut quick = limited;
        quick.headers = vec![("retry-after", "0")];
        let (url, seen) = serve(vec![quick.clone(), quick]).await;
        let (result, _) = run(&url, Mode::Quick, "q").await;
        assert_eq!(result, Err(Error::RateLimited));
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_rejected_key_is_reported_without_the_key() {
        let rejected = json_reply(
            401,
            Vec::new(),
            json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } }),
        );
        let (url, _) = serve(vec![rejected]).await;
        let (result, _) = run(&url, Mode::Quick, "q").await;
        let error = result.unwrap_err();
        assert_eq!(error, Error::InvalidKey);
        assert!(!format!("{error} {error:?}").contains("sk-ant-test"));
    }
}
