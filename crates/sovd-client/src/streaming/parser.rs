//! SSE (Server-Sent Events) parser
//!
//! Parses the SSE wire format into structured events.

use bytes::Bytes;
use tracing::trace;

use super::types::{StreamError, StreamEvent, StreamResult};

/// SSE parser state
#[derive(Debug, Default)]
pub struct SseParser {
    /// Buffer for incomplete lines
    buffer: Vec<u8>,
    /// Current event data being accumulated
    data_buffer: String,
    /// Current event type (if any)
    event_type: Option<String>,
    /// Last event ID (if any)
    last_id: Option<String>,
}

impl SseParser {
    /// Create a new SSE parser
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes into the parser and extract any complete events
    pub fn feed(&mut self, bytes: Bytes) -> Vec<StreamResult<StreamEvent>> {
        let mut events = Vec::new();

        // Append new bytes to buffer
        self.buffer.extend_from_slice(&bytes);

        // Process complete lines
        loop {
            // Find the next newline
            let newline_pos = self.buffer.iter().position(|&b| b == b'\n');

            match newline_pos {
                Some(pos) => {
                    // Extract the line (excluding newline)
                    let line = self.buffer.drain(..=pos).collect::<Vec<_>>();
                    let line = &line[..line.len() - 1]; // Remove trailing \n

                    // Handle \r\n line endings
                    let line = if line.last() == Some(&b'\r') {
                        &line[..line.len() - 1]
                    } else {
                        line
                    };

                    // Process the line
                    if let Some(event) = self.process_line(line) {
                        events.push(event);
                    }
                }
                None => break, // No more complete lines
            }
        }

        events
    }

    /// Process a single line of SSE data
    fn process_line(&mut self, line: &[u8]) -> Option<StreamResult<StreamEvent>> {
        // Empty line signals end of event
        if line.is_empty() {
            return self.dispatch_event();
        }

        // Comment line (keepalive)
        if line.starts_with(b":") {
            trace!("SSE keepalive/comment");
            return None;
        }

        // Parse field: value
        let line_str = match std::str::from_utf8(line) {
            Ok(s) => s,
            Err(_) => {
                return Some(Err(StreamError::Parse("Invalid UTF-8 in SSE line".into())));
            }
        };

        // Split on first colon
        let (field, value) = if let Some(colon_pos) = line_str.find(':') {
            let (f, v) = line_str.split_at(colon_pos);
            // Skip the colon and optional leading space
            let v = &v[1..];
            let v = v.strip_prefix(' ').unwrap_or(v);
            (f, v)
        } else {
            // Field with no value
            (line_str, "")
        };

        match field {
            "data" => {
                // Accumulate data (multiple data lines are joined with newlines)
                if !self.data_buffer.is_empty() {
                    self.data_buffer.push('\n');
                }
                self.data_buffer.push_str(value);
            }
            "event" => {
                self.event_type = Some(value.to_string());
            }
            "id" => {
                self.last_id = Some(value.to_string());
            }
            "retry" => {
                // Retry timeout - we don't handle reconnection, so ignore
                trace!("SSE retry: {}", value);
            }
            _ => {
                // Unknown field - ignore per SSE spec
                trace!("SSE unknown field: {}", field);
            }
        }

        None
    }

    /// Dispatch the accumulated event
    fn dispatch_event(&mut self) -> Option<StreamResult<StreamEvent>> {
        // If no data, nothing to dispatch
        if self.data_buffer.is_empty() {
            return None;
        }

        // Take the data buffer
        let data = std::mem::take(&mut self.data_buffer);

        // Clear event type for next event
        let _event_type = self.event_type.take();

        // Parse the JSON data
        match serde_json::from_str::<StreamEvent>(&data) {
            Ok(event) => Some(Ok(event)),
            Err(e) => {
                // Try to provide helpful error context
                let preview = if data.len() > 100 {
                    format!("{}...", &data[..100])
                } else {
                    data.clone()
                };
                Some(Err(StreamError::Parse(format!(
                    "Failed to parse event JSON: {} (data: {})",
                    e, preview
                ))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_event() {
        let mut parser = SseParser::new();

        let input = b"data: {\"ts\":123,\"seq\":1,\"speed\":60}\n\n";
        let events = parser.feed(Bytes::from_static(input));

        assert_eq!(events.len(), 1);
        let event = events[0].as_ref().unwrap();
        assert_eq!(event.timestamp, 123);
        assert_eq!(event.sequence, 1);
        assert_eq!(event.get_i64("speed"), Some(60));
    }

    #[test]
    fn test_parse_multiple_events() {
        let mut parser = SseParser::new();

        let input = b"data: {\"ts\":1,\"seq\":1,\"a\":1}\n\ndata: {\"ts\":2,\"seq\":2,\"b\":2}\n\n";
        let events = parser.feed(Bytes::from_static(input));

        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_parse_chunked_data() {
        let mut parser = SseParser::new();

        // First chunk - incomplete
        let events1 = parser.feed(Bytes::from_static(b"data: {\"ts\":1,\"seq\":"));
        assert_eq!(events1.len(), 0);

        // Second chunk - completes the event
        let events2 = parser.feed(Bytes::from_static(b"1,\"x\":42}\n\n"));
        assert_eq!(events2.len(), 1);
    }

    #[test]
    fn test_ignore_comments() {
        let mut parser = SseParser::new();

        let input = b": keepalive\ndata: {\"ts\":1,\"seq\":1}\n\n";
        let events = parser.feed(Bytes::from_static(input));

        assert_eq!(events.len(), 1);
    }
}
