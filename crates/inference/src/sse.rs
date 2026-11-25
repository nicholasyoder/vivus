//! Minimal server-sent events parser: only `data:` fields matter for chat
//! completions; comments (`: keep-alive`), `event:` and `id:` are ignored.

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: String,
    has_data: bool,
}

impl SseParser {
    /// Feeds raw bytes and returns the `data` payloads of completed events.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            self.line(line.trim_end_matches(['\n', '\r']), &mut out);
        }
        out
    }

    /// Flushes an event left unterminated when the body ends.
    pub fn finish(&mut self) -> Option<String> {
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            let mut out = Vec::new();
            self.line(line.trim_end_matches('\r'), &mut out);
            if let Some(d) = out.pop() {
                return Some(d);
            }
        }
        self.take()
    }

    fn line(&mut self, line: &str, out: &mut Vec<String>) {
        if line.is_empty() {
            if let Some(d) = self.take() {
                out.push(d);
            }
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        if field == "data" {
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(value);
            self.has_data = true;
        }
    }

    fn take(&mut self) -> Option<String> {
        self.has_data.then(|| {
            self.has_data = false;
            std::mem::take(&mut self.data)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_across_chunks_and_crlf() {
        let mut p = SseParser::default();
        assert!(p.push(b": keep-alive\r\n\r\ndata: {\"a\"").is_empty());
        assert_eq!(
            p.push(b":1}\r\n\r\ndata:[DONE]\n\n"),
            ["{\"a\":1}", "[DONE]"]
        );
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn multiline_data_and_unterminated_tail() {
        let mut p = SseParser::default();
        assert_eq!(p.push(b"event: x\ndata: a\ndata: b\n\ndata: c"), ["a\nb"]);
        assert_eq!(p.finish().as_deref(), Some("c"));
    }
}
