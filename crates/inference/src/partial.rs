//! Reading a string field out of tool-call arguments while they are still
//! streaming, so `speak`'s `text` can reach TTS before the call is complete.

/// Accumulates argument deltas and yields newly available text of one
/// top-level string field.
#[derive(Debug, Clone)]
pub struct StringField {
    field: String,
    args: String,
    emitted: usize,
    complete: bool,
}

impl StringField {
    pub fn new(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            args: String::new(),
            emitted: 0,
            complete: false,
        }
    }

    /// Feeds an argument delta; returns text decoded since the last call, if any.
    pub fn push(&mut self, delta: &str) -> Option<String> {
        if self.complete {
            return None;
        }
        self.args.push_str(delta);
        let (value, complete) = partial_string(&self.args, &self.field)?;
        self.complete = complete;
        // Incomplete escapes are held back, so the decoded value only grows.
        let new = value.get(self.emitted..).filter(|s| !s.is_empty())?.to_owned();
        self.emitted = value.len();
        Some(new)
    }

    /// The closing quote of the field has arrived.
    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

/// Decodes the value of top-level string `field` in possibly truncated JSON.
/// Returns the text so far and whether the string is closed; `None` if the
/// field hasn't started (or isn't a string, or the JSON is malformed).
pub fn partial_string(json: &str, field: &str) -> Option<(String, bool)> {
    let mut s = Scanner { b: json.as_bytes(), i: 0 };
    s.ws();
    s.eat(b'{')?;
    loop {
        s.ws();
        if s.peek()? == b'}' {
            return None;
        }
        let (key, closed) = s.string()?;
        if !closed {
            return None;
        }
        s.ws();
        s.eat(b':')?;
        s.ws();
        if key == field {
            return (s.peek()? == b'"').then(|| s.string()).flatten();
        }
        s.skip_value()?;
        s.ws();
        match s.peek()? {
            b',' => s.i += 1,
            _ => return None,
        }
    }
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek()? == c).then(|| self.i += 1)
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    /// Decodes a string at the cursor; stops cleanly at end of input.
    fn string(&mut self) -> Option<(String, bool)> {
        self.eat(b'"')?;
        let mut out = String::new();
        let src = std::str::from_utf8(self.b).ok()?;
        loop {
            let start = self.i;
            while !matches!(self.peek(), None | Some(b'"' | b'\\')) {
                self.i += 1;
            }
            out.push_str(&src[start..self.i]);
            match self.peek() {
                None => return Some((out, false)),
                Some(b'"') => {
                    self.i += 1;
                    return Some((out, true));
                }
                _ => match self.escape() {
                    Some(c) => out.push(c),
                    // Truncated mid-escape: hold it back until more arrives.
                    None => return Some((out, false)),
                },
            }
        }
    }

    fn escape(&mut self) -> Option<char> {
        let c = *self.b.get(self.i + 1)?;
        self.i += 2;
        Some(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'u' => {
                let hi = self.hex4()?;
                if (0xD800..0xDC00).contains(&hi) {
                    if self.b.get(self.i..self.i + 2)? != b"\\u" {
                        return Some('\u{FFFD}');
                    }
                    self.i += 2;
                    let lo = self.hex4()?;
                    let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                    char::from_u32(cp).unwrap_or('\u{FFFD}')
                } else {
                    char::from_u32(hi).unwrap_or('\u{FFFD}')
                }
            }
            c if c.is_ascii() => c as char,
            // Not valid JSON; treat as truncated rather than split a UTF-8 sequence.
            _ => return None,
        })
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn skip_value(&mut self) -> Option<()> {
        match self.peek()? {
            b'"' => self.string().and_then(|(_, closed)| closed.then_some(())),
            b'{' | b'[' => {
                let mut depth = 0usize;
                loop {
                    match self.peek()? {
                        b'"' => {
                            self.string().and_then(|(_, closed)| closed.then_some(()))?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                self.i += 1;
                                return Some(());
                            }
                        }
                        _ => {}
                    }
                    self.i += 1;
                }
            }
            _ => {
                while !matches!(self.peek()?, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t') {
                    self.i += 1;
                }
                Some(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_values() {
        assert_eq!(partial_string(r#"{"te"#, "text"), None);
        assert_eq!(partial_string(r#"{"text": "#, "text"), None);
        assert_eq!(partial_string(r#"{"text": "Hel"#, "text"), Some(("Hel".into(), false)));
        assert_eq!(partial_string(r#"{"text": "a\"b\n"}"#, "text"), Some(("a\"b\n".into(), true)));
        assert_eq!(partial_string(r#"{"text": "a\"#, "text"), Some(("a".into(), false)));
        assert_eq!(partial_string(r#"{"text": "é😀"#, "text"), Some(("é😀".into(), false)));
        assert_eq!(partial_string(r#"{"text": "\ud83d"#, "text"), Some(("".into(), false)));
        assert_eq!(partial_string(r#"{"text": 5}"#, "text"), None);
    }

    #[test]
    fn skips_other_fields() {
        let j = r#"{"mode": "new", "n": -1.5e3, "o": {"text": "no", "a": [1, "}"]}, "ok": true, "text": "yes"#;
        assert_eq!(partial_string(j, "text"), Some(("yes".into(), false)));
    }

    #[test]
    fn streaming_deltas() {
        let mut f = StringField::new("text");
        let chunks = [r#"{"te"#, r#"xt":"It's "#, r#"3\"#, r#"u00b0 now."#, r#"" , "x": 1}"#];
        let got: Vec<_> = chunks.iter().map(|c| f.push(c)).collect();
        assert_eq!(got, [None, Some("It's ".into()), Some("3".into()), Some("° now.".into()), None]);
        assert!(f.is_complete());
    }
}
