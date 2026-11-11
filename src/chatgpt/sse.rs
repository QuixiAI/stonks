//! An incremental server-sent-events decoder.
//!
//! It works on the raw response body, so it does not depend on the server
//! labelling the stream with a `Content-Type` (the ChatGPT backend does not).

/// A frame larger than this means the stream is not what we think it is.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Bytes of the line being received.
    line: Vec<u8>,
    /// `data:` lines of the event being received.
    data: Vec<String>,
    /// Size of `data` plus `line`, for the frame limit.
    frame_bytes: usize,
    /// The previous byte was `\r`, so a `\n` now belongs to the same line end.
    after_cr: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FrameTooLarge;

impl SseDecoder {
    /// Feeds `bytes` and returns the data of every event they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, FrameTooLarge> {
        let mut events = Vec::new();
        for &byte in bytes {
            let after_cr = std::mem::replace(&mut self.after_cr, byte == b'\r');
            match byte {
                b'\n' if after_cr => {} // second half of `\r\n`
                b'\n' | b'\r' => self.end_line(&mut events),
                _ => {
                    self.frame_bytes += 1;
                    if self.frame_bytes > MAX_FRAME_BYTES {
                        return Err(FrameTooLarge);
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }

    /// The stream ended: returns a final event that had no closing blank line.
    pub fn finish(&mut self) -> Option<String> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.end_line(&mut events);
        }
        self.end_line(&mut events);
        events.pop()
    }

    fn end_line(&mut self, events: &mut Vec<String>) {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            // A blank line dispatches the event.
            self.frame_bytes = 0;
            if !self.data.is_empty() {
                events.push(std::mem::take(&mut self.data).join("\n"));
            }
            return;
        }
        // Only `data` matters here; `event:`, `id:`, `retry:` and comments are
        // skipped because every payload carries its own `type`.
        let line = String::from_utf8_lossy(&line);
        if let Some(value) = line.strip_prefix("data:") {
            self.data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        } else if line == "data" {
            self.data.push(String::new());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&str]) -> Vec<String> {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(decoder.push(chunk.as_bytes()).unwrap());
        }
        events.extend(decoder.finish());
        events
    }

    #[test]
    fn decodes_events_and_ignores_other_fields() {
        let events = decode(&["event: a\ndata: {\"x\":1}\n\n: comment\nid: 7\ndata:two\n\n"]);
        assert_eq!(events, ["{\"x\":1}", "two"]);
    }

    #[test]
    fn handles_chunk_boundaries_anywhere() {
        let whole = "data: hello\r\n\r\ndata: wor\u{00e9}ld\n\n";
        for split in 1..whole.len() {
            let (a, b) = whole.as_bytes().split_at(split);
            let mut decoder = SseDecoder::default();
            let mut events = decoder.push(a).unwrap();
            events.extend(decoder.push(b).unwrap());
            assert_eq!(events, ["hello", "wor\u{00e9}ld"], "split at {split}");
        }
    }

    #[test]
    fn joins_multi_line_data_and_accepts_all_line_endings() {
        assert_eq!(decode(&["data: a\ndata: b\n\n"]), ["a\nb"]);
        assert_eq!(decode(&["data: a\r\rdata: b\r\n\r\n"]), ["a", "b"]);
    }

    #[test]
    fn dispatches_a_final_event_without_a_blank_line() {
        assert_eq!(decode(&["data: first\n\ndata: last"]), ["first", "last"]);
        assert_eq!(decode(&["data: first\n\n"]), ["first"]);
    }

    #[test]
    fn rejects_an_oversized_frame() {
        let mut decoder = SseDecoder::default();
        let big = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert_eq!(decoder.push(&big), Err(FrameTooLarge));
    }
}
