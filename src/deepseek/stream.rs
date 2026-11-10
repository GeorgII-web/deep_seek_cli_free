//! Parser of the completion SSE stream.
//!
//! The stream is a list of patches to one `response` object: `p` = path (kept until
//! the next `p`), `o` = operation, `v` = value. The answer is a list of fragments
//! (`THINK`, `RESPONSE`, ...); text is appended to the last one.

use crate::backend::Event;
use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::HashSet;

#[derive(Default)]
pub struct Parser {
    /// Bytes of a line not finished yet
    line: Vec<u8>,
    /// `event:` name that applies to the next `data:` line
    event: Option<String>,
    path: String,
    seen_fragments: HashSet<i64>,
    /// Type of the last fragment
    fragment_type: String,
    status: Option<String>,
    /// Id of the answer message; the parent of the next prompt
    pub response_message_id: Option<u64>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the next bytes of the body, in any chunking.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                let line = std::mem::take(&mut self.line);
                self.on_line(
                    String::from_utf8_lossy(&line).trim_end_matches('\r'),
                    &mut events,
                );
            } else {
                self.line.push(byte);
            }
        }
        events
    }

    /// Call at the end of the body: an answer that did not finish is an error.
    pub fn finish(&self) -> Result<()> {
        match self.status.as_deref() {
            Some("FINISHED") => Ok(()),
            Some(status) => bail!("the answer ended with status {status}"),
            None => bail!("the answer stream ended early"),
        }
    }

    fn on_line(&mut self, line: &str, events: &mut Vec<Event>) {
        if let Some(name) = line.strip_prefix("event:") {
            self.event = Some(name.trim().to_string());
            return;
        }
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let event = self.event.take();
        let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
            return;
        };
        match event.as_deref() {
            None => self.on_patch(&value, events),
            Some("ready") => {
                self.response_message_id = value.get("response_message_id").and_then(Value::as_u64);
            }
            _ => {}
        }
    }

    fn on_patch(&mut self, patch: &Value, events: &mut Vec<Event>) {
        if let Some(path) = patch.get("p").and_then(Value::as_str) {
            self.path = path.to_string();
        }
        let Some(value) = patch.get("v") else {
            return;
        };
        match self.path.as_str() {
            // The first patch: the whole response object.
            "" => {
                if let Some(response) = value.get("response") {
                    self.on_status(response.get("status"));
                    self.on_fragments(response.get("fragments"), events);
                }
            }
            "response/fragments" => self.on_fragments(Some(value), events),
            "response/fragments/-1/content" => {
                if let Some(text) = value.as_str() {
                    self.emit(text, events);
                }
            }
            "response/status" => self.on_status(Some(value)),
            "response" => {
                // BATCH: a list of patches relative to `response`.
                for item in value.as_array().into_iter().flatten() {
                    if item.get("p").and_then(Value::as_str) == Some("status") {
                        self.on_status(item.get("v"));
                    }
                }
            }
            _ => {}
        }
    }

    fn on_status(&mut self, status: Option<&Value>) {
        if let Some(status) = status.and_then(Value::as_str) {
            self.status = Some(status.to_string());
        }
    }

    /// A fragment list repeats fragments already streamed; only new ids count.
    fn on_fragments(&mut self, fragments: Option<&Value>, events: &mut Vec<Event>) {
        for fragment in fragments.and_then(Value::as_array).into_iter().flatten() {
            let id = fragment.get("id").and_then(Value::as_i64).unwrap_or(-1);
            if !self.seen_fragments.insert(id) {
                continue;
            }
            self.fragment_type = fragment
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(text) = fragment.get("content").and_then(Value::as_str) {
                self.emit(text, events);
            }
        }
    }

    fn emit(&self, text: &str, events: &mut Vec<Event>) {
        if text.is_empty() {
            return;
        }
        match self.fragment_type.as_str() {
            "RESPONSE" => events.push(Event::Text(text.to_string())),
            "THINK" => events.push(Event::Thinking(text.to_string())),
            // Search results and other fragment kinds are not part of the answer.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = include_str!("fixtures/sse_plain.txt");
    const THINK: &str = include_str!("fixtures/sse_think.txt");

    fn collect(parser: &mut Parser, body: &str, chunk: usize) -> (String, String) {
        let (mut text, mut thinking) = (String::new(), String::new());
        for part in body.as_bytes().chunks(chunk) {
            for event in parser.feed(part) {
                match event {
                    Event::Text(t) => text.push_str(&t),
                    Event::Thinking(t) => thinking.push_str(&t),
                }
            }
        }
        (text, thinking)
    }

    #[test]
    fn plain_answer() {
        let mut parser = Parser::new();
        let (text, thinking) = collect(&mut parser, PLAIN, 4096);
        assert_eq!(text, "2+2 equals 4.");
        assert_eq!(thinking, "");
        assert_eq!(parser.response_message_id, Some(2));
        parser.finish().unwrap();
    }

    #[test]
    fn thinking_then_answer_in_any_chunking() {
        for chunk in [1, 7, 4096] {
            let mut parser = Parser::new();
            let (text, thinking) = collect(&mut parser, THINK, chunk);
            assert!(thinking.starts_with("We need"), "{thinking}");
            assert!(thinking.ends_with("One short sentence."), "{thinking}");
            assert!(text.starts_with("7*8"), "{text}");
            assert!(!text.contains("We need"));
            parser.finish().unwrap();
        }
    }

    #[test]
    fn cut_stream_is_an_error() {
        let mut parser = Parser::new();
        let half = &PLAIN[..PLAIN.find("BATCH").unwrap()];
        collect(&mut parser, half, 4096);
        assert_eq!(
            parser.finish().unwrap_err().to_string(),
            "the answer ended with status WIP"
        );
        assert!(Parser::new().finish().is_err());
    }
}
