//! A small JSON value with JavaScript's semantics, for the parts of the
//! stream protocol Showdown reads with `JSON.parse` and writes with
//! `JSON.stringify` (`>start` and `>player` options, Showdown sets, the
//! `end` log, requests updated in place).
//!
//! Objects keep their keys in insertion order, as JavaScript objects do
//! for non-numeric keys, so `stringify(parse(text))` is what Showdown
//! writes back. Numbers are `f64` and printed like JavaScript's
//! `Number.prototype.toString`.

use std::collections::HashMap;

/// A JSON value. A key that JavaScript would hold as `undefined` is left
/// out of its object, as `JSON.stringify` does.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// An empty object.
    pub fn object() -> Json {
        Json::Object(Vec::new())
    }

    pub fn str(s: impl Into<String>) -> Json {
        Json::String(s.into())
    }

    /// The value at `key`, if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Object(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `obj[key] = value`: replaces the value in place, or appends the key.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Object(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some((_, v)) => *v = value,
                None => entries.push((key.to_string(), value)),
            }
        }
    }

    /// `delete obj[key]`.
    pub fn remove(&mut self, key: &str) -> Option<Json> {
        match self {
            Json::Object(entries) => {
                let i = entries.iter().position(|(k, _)| k == key)?;
                Some(entries.remove(i).1)
            }
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Json>> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    /// JavaScript truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Number(n) => *n != 0.0 && !n.is_nan(),
            Json::String(s) => !s.is_empty(),
            Json::Array(_) | Json::Object(_) => true,
        }
    }

    /// JavaScript's `String(value)` (template literals, `Array.join`).
    pub fn to_js_string(&self) -> String {
        match self {
            Json::Null => "null".into(),
            Json::Bool(b) => b.to_string(),
            Json::Number(n) => number_to_string(*n),
            Json::String(s) => s.clone(),
            // `Array.prototype.join` writes null and undefined as "".
            Json::Array(a) => a
                .iter()
                .map(|v| if matches!(v, Json::Null) { String::new() } else { v.to_js_string() })
                .collect::<Vec<_>>()
                .join(","),
            Json::Object(_) => "[object Object]".into(),
        }
    }

    /// `JSON.stringify(value)`.
    pub fn stringify(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) if n.is_finite() => out.push_str(&number_to_string(*n)),
            Json::Number(_) => out.push_str("null"),
            Json::String(s) => pikapro_protocol::js::push_json_string(out, s),
            Json::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Object(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pikapro_protocol::js::push_json_string(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// `JSON.parse(text)`. Duplicate keys keep their first position and
    /// their last value, as in JavaScript.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser { s: text.as_bytes(), text, i: 0 };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        if p.i < p.s.len() {
            return Err(p.unexpected());
        }
        Ok(v)
    }
}

// The JavaScript semantics live in pikapro-protocol, shared with the choice
// and team parsers.
pub use pikapro_protocol::js::json_string as quote;
pub(crate) use pikapro_protocol::js::{
    is_js_whitespace, number as js_number, number_to_string, parse_int as js_parse_int,
};

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

const MAX_DEPTH: usize = 128;

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn unexpected(&self) -> String {
        match self.text[self.i..].chars().next() {
            Some(c) => format!("Unexpected token '{c}' in JSON at position {}", self.i),
            None => "Unexpected end of JSON input".into(),
        }
    }

    fn eat(&mut self, lit: &str) -> Result<(), String> {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nested too deeply".into());
        }
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut entries: Vec<(String, Json)> = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Object(entries));
                }
                // Past a few keys, where each key is found by a map instead
                // of a scan (a scan per key is quadratic in the keys).
                let mut index: Option<HashMap<String, usize>> = None;
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.unexpected());
                    }
                    let key = self.string()?;
                    self.ws();
                    self.eat(":")?;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    // `obj[key] = v`: a repeated key keeps its first
                    // position and takes the last value.
                    let found = match &index {
                        Some(index) => index.get(&key).copied(),
                        None => entries.iter().position(|(k, _)| *k == key),
                    };
                    match found {
                        Some(i) => entries[i].1 = v,
                        None => {
                            if let Some(index) = &mut index {
                                index.insert(key.clone(), entries.len());
                            }
                            entries.push((key, v));
                            if index.is_none() && entries.len() > 16 {
                                index = Some(entries.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect());
                            }
                        }
                    }
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Object(entries));
                        }
                        _ => return Err(self.unexpected()),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut arr = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Array(arr));
                }
                loop {
                    self.ws();
                    arr.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Array(arr));
                        }
                        _ => return Err(self.unexpected()),
                    }
                }
            }
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.eat("true").map(|_| Json::Bool(true)),
            Some(b'f') => self.eat("false").map(|_| Json::Bool(false)),
            Some(b'n') => self.eat("null").map(|_| Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.unexpected()),
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let n = p.s[p.i..].iter().take_while(|c| c.is_ascii_digit()).count();
            p.i += n;
            n
        };
        match self.s.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.unexpected()),
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return Err(self.unexpected());
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return Err(self.unexpected());
            }
        }
        self.text[start..self.i].parse().map(Json::Number).map_err(|_| self.unexpected())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.text.get(self.i..self.i + 4).ok_or_else(|| self.unexpected())?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.unexpected())?;
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.unexpected());
        }
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(&b) = self.s.get(self.i) {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(&self.text[start..self.i]);
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.s.get(self.i).ok_or_else(|| self.unexpected())?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with(b"\\u") {
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    self.i = save;
                                    hi
                                }
                            } else {
                                hi
                            };
                            // A lone surrogate has no `char`: U+FFFD.
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => {
                            self.i -= 1;
                            return Err(self.unexpected());
                        }
                    }
                }
                _ => return Err(self.unexpected()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_like_javascript() {
        let text = r#" { "b" : [1, 2.5, -0, 1e21, 1e-7, 0.000001, 123456789012], "a":"xé\n\"", "b2": null, "c": {"d": true} } "#;
        let v = Json::parse(text).unwrap();
        assert_eq!(
            v.stringify(),
            r#"{"b":[1,2.5,0,1e+21,1e-7,0.000001,123456789012],"a":"xé\n\"","b2":null,"c":{"d":true}}"#
        );
        let dup = Json::parse(r#"{"a":1,"b":2,"a":3}"#).unwrap();
        assert_eq!(dup.stringify(), r#"{"a":3,"b":2}"#);
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("[1,]").is_err());
        assert!(Json::parse("01").is_err());
        assert_eq!(Json::parse("\"\\ud83d\\ude00\"").unwrap(), Json::str("😀"));
    }

    #[test]
    fn many_keys_parse_in_linear_time() {
        let keys: Vec<String> = (0..200_000).map(|i| format!("\"k{i}\":{i}")).collect();
        let text = format!("{{{},\"k5\":\"last\"}}", keys.join(","));
        let start = std::time::Instant::now();
        let v = Json::parse(&text).unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        let Json::Object(entries) = &v else { panic!() };
        assert_eq!(entries.len(), 200_000);
        assert_eq!(entries[5], ("k5".to_string(), Json::str("last")));
    }

    #[test]
    fn numbers_print_like_javascript() {
        for (n, s) in [
            (0.1, "0.1"),
            (1.5e300, "1.5e+300"),
            (-2.5e-8, "-2.5e-8"),
            (123.456, "123.456"),
            (1e20, "100000000000000000000"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(number_to_string(n), s);
        }
        assert_eq!(js_number(" 12 "), 12.0);
        assert_eq!(js_number("0x10"), 16.0);
        assert!(js_number("12a").is_nan());
        assert_eq!(js_number(""), 0.0);
        assert_eq!(js_parse_int("1g", 16), Some(1.0));
        assert_eq!(js_parse_int("-f", 16), Some(-15.0));
        assert_eq!(js_parse_int("g", 16), None);
        assert_eq!(js_parse_int("0x", 16), None);
    }
}
