//! One streamed Messages API response, rebuilt from its events.
//!
//! Text is handed out as it arrives so the panel can show it. Every content
//! block is also rebuilt whole (text with its citations, a web search call
//! with its input, the search results, thinking with its signature) because
//! a paused turn is resumed by sending all of them back unchanged.

use serde_json::{Value, json};

use super::Error;

/// What one event meant to the reader.
#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    /// Nothing worth showing.
    Nothing,
    /// More of the answer's text.
    Text(String),
    /// Claude started a web search.
    Searching,
    /// Claude went back to writing after a search.
    Writing,
    /// The response is complete.
    Stopped,
}

/// A block being rebuilt.
#[derive(Debug)]
struct Building {
    block: Value,
    /// A tool call's input, which arrives as pieces of JSON text.
    input: String,
}

/// The response so far.
#[derive(Debug, Default)]
pub struct Message {
    blocks: Vec<Option<Building>>,
    stop_reason: Option<String>,
    /// Whether `message_stop` arrived.
    complete: bool,
    /// The answer's text, as handed out.
    text: String,
}

impl Message {
    /// Applies one event's JSON data. An `error` event becomes an error.
    pub fn apply(&mut self, event: &Value) -> Result<Update, Error> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(match kind {
            "content_block_start" => self.start(event),
            "content_block_delta" => self.delta(event),
            "content_block_stop" => {
                self.stop(event);
                Update::Nothing
            }
            "message_delta" => {
                if let Some(reason) = event.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_string());
                }
                Update::Nothing
            }
            "message_stop" => {
                self.complete = true;
                Update::Stopped
            }
            "error" => return Err(stream_error(event)),
            // `message_start`, `ping`, and whatever a later API adds.
            _ => Update::Nothing,
        })
    }

    fn start(&mut self, event: &Value) -> Update {
        let (Some(index), Some(block)) = (index(event), event.get("content_block")) else {
            return Update::Nothing;
        };
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, || None);
        }
        let update = match block.get("type").and_then(Value::as_str) {
            Some("server_tool_use" | "tool_use") => Update::Searching,
            Some("text") => match block.get("text").and_then(Value::as_str) {
                Some(text) if !text.is_empty() => {
                    self.text.push_str(text);
                    Update::Text(text.to_string())
                }
                _ => Update::Writing,
            },
            _ => Update::Nothing,
        };
        self.blocks[index] = Some(Building {
            block: block.clone(),
            input: String::new(),
        });
        update
    }

    fn delta(&mut self, event: &Value) -> Update {
        let Some(building) = index(event)
            .and_then(|index| self.blocks.get_mut(index))
            .and_then(Option::as_mut)
        else {
            return Update::Nothing;
        };
        let Some(delta) = event.get("delta") else {
            return Update::Nothing;
        };
        let piece = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
        match delta
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "text_delta" => {
                let text = piece("text");
                append(&mut building.block, "text", text);
                self.text.push_str(text);
                return Update::Text(text.to_string());
            }
            "input_json_delta" => building.input.push_str(piece("partial_json")),
            "thinking_delta" => append(&mut building.block, "thinking", piece("thinking")),
            "signature_delta" => {
                building.block["signature"] = Value::String(piece("signature").to_string());
            }
            "citations_delta" => {
                if let Some(citation) = delta.get("citation") {
                    match building.block.get_mut("citations") {
                        Some(Value::Array(citations)) => citations.push(citation.clone()),
                        _ => building.block["citations"] = json!([citation]),
                    }
                }
            }
            _ => {}
        }
        Update::Nothing
    }

    fn stop(&mut self, event: &Value) {
        let Some(building) = index(event)
            .and_then(|index| self.blocks.get_mut(index))
            .and_then(Option::as_mut)
        else {
            return;
        };
        if building.input.trim().is_empty() {
            return;
        }
        match serde_json::from_str::<Value>(&building.input) {
            Ok(input) => building.block["input"] = input,
            // Kept as the block started; the API names the problem if this
            // block is ever sent back.
            Err(error) => log::warn!("unreadable tool input in Claude's stream: {error}"),
        }
        building.input.clear();
    }

    /// Whether `message_stop` arrived.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }

    /// The answer's text so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Every block, rebuilt, in the order the API numbered them.
    pub fn content(self) -> Vec<Value> {
        self.blocks
            .into_iter()
            .flatten()
            .map(|building| building.block)
            .collect()
    }
}

fn index(event: &Value) -> Option<usize> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
}

fn append(block: &mut Value, key: &str, piece: &str) {
    match block.get_mut(key) {
        Some(Value::String(text)) => text.push_str(piece),
        _ => block[key] = Value::String(piece.to_string()),
    }
}

/// The error an `error` event reports. Anthropic sends these with an HTTP
/// 200 once the stream has begun, for instance when it becomes overloaded.
pub fn stream_error(event: &Value) -> Error {
    let kind = event
        .pointer("/error/type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = event
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match kind {
        "overloaded_error" => Error::Overloaded,
        "rate_limit_error" => Error::RateLimited,
        "authentication_error" | "permission_error" => Error::InvalidKey,
        _ => Error::Api {
            status: match kind {
                "invalid_request_error" => 400,
                "not_found_error" => 404,
                "request_too_large" => 413,
                "timeout_error" => 504,
                _ => 500,
            },
            message,
        },
    }
}

/// Whether a failure mid-stream is worth one more try, as the same failure
/// would be before the stream began.
pub fn retryable(error: &Error) -> bool {
    matches!(
        error,
        Error::Overloaded
            | Error::Api {
                status: 500..=599,
                ..
            }
    )
}

/// The blocks to send back when resuming. After a mid-answer fallback to
/// another model, the blocks the declined model produced before the last
/// `fallback` marker are kept only where the API accepts them back: text,
/// and server tool calls paired with their results. Without a fallback,
/// every block goes back exactly as it came.
pub fn echoable(content: &[Value]) -> Vec<Value> {
    let kind = |block: &Value| {
        block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let Some(boundary) = content.iter().rposition(|block| kind(block) == "fallback") else {
        return content.to_vec();
    };
    let answered: Vec<&str> = content
        .iter()
        .filter(|block| kind(block).ends_with("_tool_result"))
        .filter_map(|block| block.get("tool_use_id").and_then(Value::as_str))
        .collect();
    content
        .iter()
        .enumerate()
        .filter(|(position, block)| {
            if *position >= boundary {
                return true;
            }
            match kind(block).as_str() {
                "text" => true,
                "server_tool_use" => block
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| answered.contains(&id)),
                other => other.ends_with("_tool_result"),
            }
        })
        .map(|(_, block)| block.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_all(events: &[Value]) -> (Message, Vec<Update>) {
        let mut message = Message::default();
        let updates = events
            .iter()
            .map(|event| message.apply(event).unwrap())
            .collect();
        (message, updates)
    }

    #[test]
    fn text_with_citations_is_rebuilt_and_handed_out() {
        let citation = json!({
            "type": "web_search_result_location", "url": "https://example.org",
            "title": "Example", "encrypted_index": "abc", "cited_text": "Produced by Prince"
        });
        let (message, updates) = apply_all(&[
            json!({"type": "message_start", "message": {"id": "m", "content": []}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "citations_delta", "citation": citation}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Produced "}}),
            json!({"type": "ping"}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "by Prince"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
            json!({"type": "message_stop"}),
        ]);
        assert_eq!(
            updates,
            [
                Update::Nothing,
                Update::Writing,
                Update::Nothing,
                Update::Text("Produced ".into()),
                Update::Nothing,
                Update::Text("by Prince".into()),
                Update::Nothing,
                Update::Nothing,
                Update::Stopped,
            ]
        );
        assert!(message.is_complete());
        assert_eq!(message.stop_reason(), Some("end_turn"));
        assert_eq!(message.text(), "Produced by Prince");
        assert_eq!(
            message.content(),
            [json!({"type": "text", "text": "Produced by Prince", "citations": [citation]})]
        );
    }

    #[test]
    fn tool_calls_results_and_thinking_are_rebuilt_whole() {
        let results = json!([{
            "type": "web_search_result", "url": "https://example.org", "title": "x",
            "encrypted_content": "enc", "page_age": null
        }]);
        let (message, updates) = apply_all(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Let me "}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "check."}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig=="}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"query\": \"Sign o"}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "' the Times \u{e9}\"}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": results}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "message_delta", "delta": {"stop_reason": "pause_turn"}}),
            json!({"type": "message_stop"}),
        ]);
        assert!(updates.contains(&Update::Searching));
        assert_eq!(message.stop_reason(), Some("pause_turn"));
        assert_eq!(message.text(), "");
        assert_eq!(
            message.content(),
            [
                json!({"type": "thinking", "thinking": "Let me check.", "signature": "sig=="}),
                json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {"query": "Sign o' the Times \u{e9}"}}),
                json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": results}),
            ]
        );
    }

    #[test]
    fn a_tool_call_without_input_pieces_keeps_its_starting_input() {
        let (message, _) = apply_all(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "server_tool_use", "id": "s", "name": "web_search", "input": {"query": "q"}}}),
            json!({"type": "content_block_stop", "index": 0}),
        ]);
        assert_eq!(message.content()[0]["input"], json!({"query": "q"}));
    }

    #[test]
    fn unknown_events_and_blocks_pass_through() {
        let (message, _) = apply_all(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "something_new", "x": 1}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "brand_new_event"}),
        ]);
        assert!(!message.is_complete());
        assert_eq!(
            message.content(),
            [json!({"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}})]
        );
    }

    #[test]
    fn error_events_become_errors() {
        let mut message = Message::default();
        let overloaded = json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}});
        assert_eq!(message.apply(&overloaded), Err(Error::Overloaded));
        assert!(retryable(&Error::Overloaded));
        let api = stream_error(
            &json!({"type": "error", "error": {"type": "api_error", "message": "boom"}}),
        );
        assert_eq!(
            api,
            Error::Api {
                status: 500,
                message: "boom".into()
            }
        );
        assert!(retryable(&api));
        let invalid = stream_error(
            &json!({"type": "error", "error": {"type": "invalid_request_error", "message": "bad"}}),
        );
        assert!(!retryable(&invalid));
        assert_eq!(
            stream_error(&json!({"type": "error", "error": {"type": "rate_limit_error"}})),
            Error::RateLimited
        );
    }

    #[test]
    fn without_a_fallback_every_block_is_echoed_unchanged() {
        let content = vec![
            json!({"type": "thinking", "thinking": "", "signature": "s"}),
            json!({"type": "server_tool_use", "id": "a", "name": "web_search", "input": {}}),
        ];
        assert_eq!(echoable(&content), content);
    }

    #[test]
    fn after_a_fallback_only_echoable_blocks_before_it_are_kept() {
        let content = vec![
            json!({"type": "thinking", "thinking": "", "signature": "s"}),
            json!({"type": "text", "text": "Partial "}),
            json!({"type": "server_tool_use", "id": "paired", "name": "web_search", "input": {}}),
            json!({"type": "web_search_tool_result", "tool_use_id": "paired", "content": []}),
            json!({"type": "server_tool_use", "id": "lonely", "name": "web_search", "input": {}}),
            json!({"type": "redacted_thinking", "data": "x"}),
            json!({"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}}),
            json!({"type": "thinking", "thinking": "", "signature": "t"}),
            json!({"type": "text", "text": "rest"}),
        ];
        assert_eq!(
            echoable(&content),
            [
                content[1].clone(),
                content[2].clone(),
                content[3].clone(),
                content[6].clone(),
                content[7].clone(),
                content[8].clone(),
            ]
        );
    }
}
