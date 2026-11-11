//! A small JSON reader for Showdown's `|request|` bodies: values borrow the
//! text, objects and arrays are walked lazily, and a string is unescaped
//! only when it has escapes. No allocation for ordinary requests.

use core::fmt;

/// Why a JSON text could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// Byte offset of the problem.
    pub at: usize,
    pub what: &'static str,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.what, self.at)
    }
}

/// A JSON value borrowing its text. Arrays and objects keep their raw
/// text (brackets included) and are read with [`Value::items`] and
/// [`Value::entries`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Null,
    Bool(bool),
    /// The number's text.
    Number(&'a str),
    /// The string's raw content between its quotes (escapes kept).
    Str(&'a str),
    Array(&'a str),
    Object(&'a str),
}

impl<'a> Value<'a> {
    /// Parses a whole text as one value. Nested values are read lazily: a
    /// malformed one is an error where it is read.
    pub fn parse(text: &'a str) -> Result<Value<'a>, JsonError> {
        let mut r = Reader { s: text.as_bytes(), text, pos: 0 };
        let v = r.value()?;
        r.ws();
        if r.pos != r.s.len() {
            return Err(r.err("trailing characters"));
        }
        Ok(v)
    }

    /// [`Value::parse`], every nested value read (for the tests).
    #[cfg(test)]
    fn parse_strict(text: &'a str) -> Result<Value<'a>, JsonError> {
        let v = Value::parse(text)?;
        v.validate()?;
        Ok(v)
    }

    /// The values of `keys` in an object, read in one pass (a repeated
    /// key: its last value, as `JSON.parse` keeps it).
    pub fn fields<const N: usize>(&self, keys: [&str; N]) -> Result<[Option<Value<'a>>; N], JsonError> {
        let mut out = [None; N];
        for e in self.entries() {
            let (k, v) = e?;
            if let Some(i) = keys.iter().position(|key| k.0 == *key) {
                out[i] = Some(v);
            }
        }
        Ok(out)
    }

    /// Reads every nested value once, so that a malformed one is an error
    /// here rather than a missing value later.
    #[cfg(test)]
    fn validate(&self) -> Result<(), JsonError> {
        match self {
            Value::Array(_) => {
                for v in self.items() {
                    v?.validate()?;
                }
            }
            Value::Object(_) => {
                for e in self.entries() {
                    e?.1.validate()?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// JavaScript truthiness (`!!value`), for flags Showdown may write as
    /// strings (`disabled: "hidden"`).
    pub fn truthy(&self) -> bool {
        match *self {
            Value::Null => false,
            Value::Bool(b) => b,
            Value::Number(n) => !matches!(n, "0" | "-0" | "0.0"),
            Value::Str(s) => !s.is_empty(),
            Value::Array(_) | Value::Object(_) => true,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Number(n) => n.parse().ok(),
            _ => None,
        }
    }

    /// The raw string (escapes kept): compare with [`unescape`] when it
    /// may hold one.
    pub fn as_raw_str(&self) -> Option<&'a str> {
        match *self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The string, unescaped (borrowed when it has no escapes).
    pub fn as_str(&self) -> Option<Str<'a>> {
        self.as_raw_str().map(Str)
    }

    /// The items of an array (nothing for anything else).
    pub fn items(&self) -> Items<'a> {
        match *self {
            Value::Array(raw) => Items { r: Reader::inner(raw), done: false, first: true },
            _ => Items { r: Reader::inner(""), done: true, first: true },
        }
    }

    /// The entries of an object (nothing for anything else).
    pub fn entries(&self) -> Entries<'a> {
        match *self {
            Value::Object(raw) => Entries { r: Reader::inner(raw), done: false, first: true },
            _ => Entries { r: Reader::inner(""), done: true, first: true },
        }
    }

    /// The value of `key` in an object (the last one, as `JSON.parse`
    /// keeps it).
    pub fn get(&self, key: &str) -> Option<Value<'a>> {
        let mut found = None;
        for (k, v) in self.entries().flatten() {
            if k.eq_str(key) {
                found = Some(v);
            }
        }
        found
    }
}

/// A JSON string's raw content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Str<'a>(pub &'a str);

impl<'a> Str<'a> {
    /// Whether it unescapes to `other`.
    pub fn eq_str(&self, other: &str) -> bool {
        if !self.0.contains('\\') {
            return self.0 == other;
        }
        unescape(self.0) == other
    }

    /// The text, unescaped (allocates only when it has escapes).
    pub fn text(&self) -> std::borrow::Cow<'a, str> {
        if self.0.contains('\\') {
            std::borrow::Cow::Owned(unescape(self.0))
        } else {
            std::borrow::Cow::Borrowed(self.0)
        }
    }
}

/// Unescapes a JSON string's raw content (malformed escapes are kept as
/// they are; lone surrogates become U+FFFD).
pub fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex = |chars: &mut core::str::Chars<'_>| -> Option<u32> {
                    let h: String = chars.by_ref().take(4).collect();
                    u32::from_str_radix(&h, 16).ok()
                };
                let Some(hi) = hex(&mut chars) else { continue };
                if (0xD800..0xDC00).contains(&hi) {
                    let rest = chars.as_str();
                    if let Some(lo) =
                        rest.strip_prefix("\\u").and_then(|r| r.get(..4)).and_then(|h| u32::from_str_radix(h, 16).ok())
                        && (0xDC00..0xE000).contains(&lo)
                    {
                        chars = rest[6..].chars();
                        out.push(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{fffd}'));
                        continue;
                    }
                    out.push('\u{fffd}');
                } else {
                    out.push(char::from_u32(hi).unwrap_or('\u{fffd}'));
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// An array's items.
#[derive(Debug)]
pub struct Items<'a> {
    r: Reader<'a>,
    done: bool,
    first: bool,
}

impl<'a> Iterator for Items<'a> {
    type Item = Result<Value<'a>, JsonError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let r = &mut self.r;
        r.ws();
        if r.peek() == Some(b']') {
            self.done = true;
            return None;
        }
        if !self.first {
            if r.peek() != Some(b',') {
                self.done = true;
                return Some(Err(r.err("expected ','")));
            }
            r.pos += 1;
        }
        self.first = false;
        let v = r.value();
        if v.is_err() {
            self.done = true;
        }
        Some(v)
    }
}

/// An object's entries.
#[derive(Debug)]
pub struct Entries<'a> {
    r: Reader<'a>,
    done: bool,
    first: bool,
}

impl<'a> Iterator for Entries<'a> {
    type Item = Result<(Str<'a>, Value<'a>), JsonError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let r = &mut self.r;
        r.ws();
        if r.peek() == Some(b'}') {
            self.done = true;
            return None;
        }
        if !self.first {
            if r.peek() != Some(b',') {
                self.done = true;
                return Some(Err(r.err("expected ','")));
            }
            r.pos += 1;
            r.ws();
        }
        self.first = false;
        let entry = (|| {
            let Value::Str(key) = r.string()? else { unreachable!() };
            r.ws();
            if r.peek() != Some(b':') {
                return Err(r.err("expected ':'"));
            }
            r.pos += 1;
            Ok((Str(key), r.value()?))
        })();
        if entry.is_err() {
            self.done = true;
        }
        Some(entry)
    }
}

#[derive(Clone, Copy, Debug)]
struct Reader<'a> {
    s: &'a [u8],
    text: &'a str,
    pos: usize,
}

impl<'a> Reader<'a> {
    /// A reader positioned inside an array's or object's raw text.
    fn inner(raw: &'a str) -> Reader<'a> {
        Reader { s: raw.as_bytes(), text: raw, pos: usize::from(!raw.is_empty()) }
    }

    fn err(&self, what: &'static str) -> JsonError {
        JsonError { at: self.pos, what }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Result<Value<'a>, JsonError> {
        self.ws();
        match self.peek() {
            Some(b'"') => self.string(),
            Some(b'{') => self.container().map(Value::Object),
            Some(b'[') => self.container().map(Value::Array),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.pos;
                while matches!(self.peek(), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.pos += 1;
                }
                Ok(Value::Number(&self.text[start..self.pos]))
            }
            _ => Err(self.err("expected a value")),
        }
    }

    fn word(&mut self, w: &str, v: Value<'a>) -> Result<Value<'a>, JsonError> {
        if self.s[self.pos..].starts_with(w.as_bytes()) {
            self.pos += w.len();
            Ok(v)
        } else {
            Err(self.err("expected a value"))
        }
    }

    fn string(&mut self) -> Result<Value<'a>, JsonError> {
        if self.peek() != Some(b'"') {
            return Err(self.err("expected a string"));
        }
        self.pos += 1;
        let start = self.pos;
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    let s = &self.text[start..self.pos];
                    self.pos += 1;
                    return Ok(Value::Str(s));
                }
                Some(b'\\') => self.pos += 2,
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Skips a balanced container, returning its raw text.
    fn container(&mut self) -> Result<&'a str, JsonError> {
        let start = self.pos;
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated container")),
                Some(b'"') => {
                    self.string()?;
                    continue;
                }
                Some(b'{' | b'[') => depth += 1,
                Some(b'}' | b']') => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return Ok(&self.text[start..self.pos]);
                    }
                }
                Some(_) => {}
            }
            self.pos += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_values() {
        let v = Value::parse(r#"{"a":[1,{"b":"x\"y"},null],"c":true,"d":"Zoë","e":-2}"#).unwrap();
        let a: Vec<_> = v.get("a").unwrap().items().map(Result::unwrap).collect();
        assert_eq!(a[0], Value::Number("1"));
        assert_eq!(a[1].get("b").unwrap().as_str().unwrap().text(), "x\"y");
        assert_eq!(a[2], Value::Null);
        assert_eq!(v.get("c"), Some(Value::Bool(true)));
        assert!(v.get("d").unwrap().as_str().unwrap().eq_str("Zoë"));
        assert_eq!(v.get("e").unwrap(), Value::Number("-2"));
        assert_eq!(v.get("zz"), None);
        assert!(Value::parse_strict("{\"a\":}").is_err());
        assert!(Value::parse("[1,2").is_err());
        assert_eq!(unescape(r"😀"), "😀");
    }
}
