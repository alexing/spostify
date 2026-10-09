//! Server-sent events, as the Claude Messages API streams them.
//!
//! The network hands over bytes in arbitrary pieces: an event can be split
//! across pieces, a piece can hold several events, and a piece can even end
//! in the middle of a multi-byte character. So bytes are buffered until a
//! whole line has arrived, and only whole lines are decoded. Lines may end
//! in `\n`, `\r\n` or a lone `\r`, as the format allows.

/// One event: its `event:` name, if any, and its `data:` lines joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub name: Option<String>,
    pub data: String,
}

/// Turns pieces of a stream into whole events.
#[derive(Debug, Default)]
pub struct Parser {
    /// Bytes not yet part of a whole line.
    pending: Vec<u8>,
    /// The event being read.
    name: Option<String>,
    data: String,
    has_data: bool,
    /// Whether the first line has been seen, to drop a byte order mark.
    started: bool,
}

impl Parser {
    /// Feeds the next piece of the stream and returns the events it
    /// completes, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut start = 0;
        let mut index = 0;
        while index < self.pending.len() {
            let end = match self.pending[index] {
                b'\n' => index + 1,
                b'\r' => match self.pending.get(index + 1) {
                    Some(b'\n') => index + 2,
                    Some(_) => index + 1,
                    // A `\r` that ends the piece may be half of a `\r\n`:
                    // wait for the next piece to tell.
                    None => break,
                },
                _ => {
                    index += 1;
                    continue;
                }
            };
            let line = String::from_utf8_lossy(&self.pending[start..index]).into_owned();
            if let Some(event) = self.line(&line) {
                events.push(event);
            }
            start = end;
            index = end;
        }
        self.pending.drain(..start);
        events
    }

    /// Ends the stream: an event left without its closing blank line is
    /// still delivered, as the network may cut the last one short.
    pub fn finish(&mut self) -> Option<Event> {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            let line = String::from_utf8_lossy(&rest).into_owned();
            let line = line.strip_suffix('\r').unwrap_or(&line).to_string();
            if let Some(event) = self.line(&line) {
                return Some(event);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, line: &str) -> Option<Event> {
        let line = if self.started {
            line
        } else {
            self.started = true;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        };
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            // A comment, sometimes sent to keep the connection open.
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => self.name = Some(value.to_string()),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            // `id` and `retry` matter only to browsers that reconnect.
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<Event> {
        let name = self.name.take();
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        Some(Event {
            name,
            data: std::mem::take(&mut self.data),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short stream with a multi-byte character in a text delta.
    const SAMPLE: &str = "event: message_start\n\
        data: {\"type\":\"message_start\"}\n\
        \n\
        event: ping\n\
        data: {\"type\": \"ping\"}\n\
        \n\
        event: content_block_delta\n\
        data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"Caf\u{e9} \u{2605} \u{6c34}\u{1f3b8}\"}}\n\
        \n\
        : a comment\n\
        event: message_stop\n\
        data: {\"type\":\"message_stop\"}\n\
        \n";

    fn whole(text: &str) -> Vec<Event> {
        let mut parser = Parser::default();
        let mut events = parser.push(text.as_bytes());
        events.extend(parser.finish());
        events
    }

    #[test]
    fn whole_events_are_read_with_their_names() {
        let events = whole(SAMPLE);
        let names: Vec<_> = events.iter().map(|e| e.name.as_deref().unwrap()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "ping",
                "content_block_delta",
                "message_stop"
            ]
        );
        assert!(
            events[2]
                .data
                .contains("Caf\u{e9} \u{2605} \u{6c34}\u{1f3b8}")
        );
    }

    #[test]
    fn every_split_point_gives_the_same_events() {
        let expected = whole(SAMPLE);
        let bytes = SAMPLE.as_bytes();
        for split in 0..=bytes.len() {
            let mut parser = Parser::default();
            let mut events = parser.push(&bytes[..split]);
            events.extend(parser.push(&bytes[split..]));
            events.extend(parser.finish());
            assert_eq!(events, expected, "split at byte {split}");
        }
    }

    #[test]
    fn one_byte_at_a_time_gives_the_same_events() {
        let expected = whole(SAMPLE);
        let mut parser = Parser::default();
        let mut events = Vec::new();
        for byte in SAMPLE.as_bytes() {
            events.extend(parser.push(std::slice::from_ref(byte)));
        }
        events.extend(parser.finish());
        assert_eq!(events, expected);
    }

    #[test]
    fn crlf_and_lone_cr_end_lines_too() {
        let expected = whole(SAMPLE);
        let crlf = SAMPLE.replace('\n', "\r\n");
        assert_eq!(whole(&crlf), expected);
        let cr = SAMPLE.replace('\n', "\r");
        assert_eq!(whole(&cr), expected);
        // A CRLF split between two pieces is one line end, not two.
        let bytes = crlf.as_bytes();
        for split in 0..=bytes.len() {
            let mut parser = Parser::default();
            let mut events = parser.push(&bytes[..split]);
            events.extend(parser.push(&bytes[split..]));
            events.extend(parser.finish());
            assert_eq!(events, expected, "split at byte {split}");
        }
    }

    #[test]
    fn several_events_in_one_piece_and_multi_line_data() {
        let mut parser = Parser::default();
        let events = parser.push(b"data: a\ndata: b\n\nevent: x\ndata:c\n\nevent: y\n\n");
        assert_eq!(
            events,
            [
                Event {
                    name: None,
                    data: "a\nb".into()
                },
                Event {
                    name: Some("x".into()),
                    data: "c".into()
                },
            ]
        );
        // An event without data is not delivered, and its name does not
        // leak into the next one.
        let next = parser.push(b"data: d\n\n");
        assert_eq!(next[0].name, None);
    }

    #[test]
    fn a_last_event_without_its_blank_line_is_still_delivered() {
        let mut parser = Parser::default();
        assert!(parser.push(b"event: error\ndata: {\"x\":1}").is_empty());
        assert_eq!(
            parser.finish(),
            Some(Event {
                name: Some("error".into()),
                data: "{\"x\":1}".into()
            })
        );
    }

    #[test]
    fn a_byte_order_mark_is_dropped() {
        let events = whole("\u{feff}event: ping\ndata: {}\n\n");
        assert_eq!(events[0].name.as_deref(), Some("ping"));
    }
}
