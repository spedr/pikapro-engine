//! The JavaScript semantics Showdown's text formats inherit: its white
//! space, `trim`, `parseInt`, `Number(string)`, `Number.prototype.toString`
//! and `JSON.stringify` of a string. Showdown reads choices, teams and
//! stream commands with these, and its messages print numbers with them.

use core::fmt::Write as _;

/// JavaScript's white space and line terminators (what `trim` removes and
/// `\s` matches).
pub use pikapro_data::is_js_whitespace;

/// `s.trim()`.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `s.length`: UTF-16 code units.
pub fn length(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// JavaScript's `parseInt(s, radix)`, `None` for `NaN`. `radix` 0 is
/// `parseInt(s)` (base 10, or 16 after a `0x` prefix); 16 also takes the
/// prefix. Leading white space and a sign are skipped, then the longest run
/// of digits is read, however long: decimal and hexadecimal values are
/// correctly rounded (as V8 does), `Infinity` when too large.
pub fn parse_int(s: &str, radix: u32) -> Option<f64> {
    let mut t = s.trim_start_matches(is_js_whitespace);
    let negative = t.starts_with('-');
    if let Some(rest) = t.strip_prefix(['+', '-']) {
        t = rest;
    }
    let mut radix = radix;
    if radix != 0 && !(2..=36).contains(&radix) {
        return None;
    }
    if radix == 0 || radix == 16 {
        match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            Some(rest) => {
                t = rest;
                radix = 16;
            }
            None if radix == 0 => radix = 10,
            None => {}
        }
    }
    let len = t.bytes().take_while(|b| (*b as char).is_digit(radix)).count();
    if len == 0 {
        return None;
    }
    let v = digits_value(&t[..len], radix);
    Some(if negative { -v } else { v })
}

/// The value of a run of valid digits.
fn digits_value(digits: &str, radix: u32) -> f64 {
    match radix {
        10 => digits.parse().unwrap_or(f64::NAN),
        2 | 4 | 8 | 16 | 32 => power_of_two_value(digits, radix),
        _ => digits.bytes().fold(0.0, |acc, b| acc * radix as f64 + (b as char).to_digit(radix).unwrap_or(0) as f64),
    }
}

/// Digits in a power-of-two radix, rounded to nearest (ties to even) with
/// every digit taken into account.
fn power_of_two_value(digits: &str, radix: u32) -> f64 {
    let bits = radix.trailing_zeros();
    let mut mantissa: u64 = 0;
    let mut exp: u64 = 0;
    let mut sticky = false;
    for b in digits.bytes() {
        let d = (b as char).to_digit(radix).unwrap_or(0) as u64;
        if mantissa >> (64 - bits) == 0 {
            mantissa = mantissa << bits | d;
        } else {
            exp += bits as u64;
            sticky |= d != 0;
        }
    }
    // The mantissa has more than 53 significant bits whenever digits were
    // dropped, so a set lowest bit only decides exact halves.
    let mut v = (mantissa | sticky as u64) as f64;
    while exp > 0 && v.is_finite() {
        let step = exp.min(1000);
        v *= f64::from_bits((1023 + step) << 52);
        exp -= step;
    }
    v
}

/// JavaScript's `Number(s)` for a string: `NaN` for anything that is not a
/// number, 0 for white space alone.
pub fn number(s: &str) -> f64 {
    let t = trim(s);
    if t.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            if rest.is_empty() || !rest.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return power_of_two_value(rest, radix);
        }
    }
    let unsigned = t.strip_prefix(['+', '-']).unwrap_or(t);
    if unsigned == "Infinity" {
        return if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    // StrDecimalLiteral: digits, an optional fraction, an optional exponent.
    let b = unsigned.as_bytes();
    let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    let mut i = int_digits;
    let mut frac_digits = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        frac_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        i += frac_digits;
    }
    if int_digits + frac_digits == 0 {
        return f64::NAN;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let exp_digits = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp_digits == 0 {
            return f64::NAN;
        }
        i = j + exp_digits;
    }
    if i != b.len() {
        return f64::NAN;
    }
    t.parse().unwrap_or(f64::NAN)
}

/// JavaScript's `Number.prototype.toString()` (radix 10): `NaN`,
/// `Infinity`, `1e+21`, `0.000001`, `1e-7`, ...
pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n == 0.0 {
        return "0".into();
    }
    // Integers are exact below 2^53, so every digit is needed; above it the
    // shortest round-tripping digits are padded with zeros.
    if n.fract() == 0.0 && n.abs() < 9007199254740992.0 {
        return format!("{}", n as i128);
    }
    // The shortest round-tripping digits, then ECMAScript's layout
    // (Number::toString, steps 6-12).
    let sci = format!("{:e}", n.abs());
    let (mantissa, exp) = sci.split_once('e').expect("LowerExp has an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let k = digits.len() as i32;
    let e: i32 = exp.parse().expect("exponent");
    let n_exp = e + 1;
    let mut out = String::new();
    if n < 0.0 {
        out.push('-');
    }
    if k <= n_exp && n_exp <= 21 {
        out.push_str(&digits);
        out.extend(core::iter::repeat_n('0', (n_exp - k) as usize));
    } else if 0 < n_exp && n_exp <= 21 {
        out.push_str(&digits[..n_exp as usize]);
        out.push('.');
        out.push_str(&digits[n_exp as usize..]);
    } else if -6 < n_exp && n_exp <= 0 {
        out.push_str("0.");
        out.extend(core::iter::repeat_n('0', (-n_exp) as usize));
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{}", if n_exp > 0 { "+" } else { "-" }, (n_exp - 1).abs());
    }
    out
}

/// Appends `JSON.stringify(s)`: quoted, with `"`, `\` and the control
/// characters escaped.
pub fn push_json_string(out: &mut String, s: &str) {
    out.reserve(s.len() + 2);
    out.push('"');
    push_json_body(out, s);
    out.push('"');
}

/// [`push_json_string`] without the quotes.
pub(crate) fn push_json_body(out: &mut String, s: &str) {
    // Most strings need no escape: copied at once.
    if !s.bytes().any(|b| b == b'"' || b == b'\\' || b < 0x20) {
        out.push_str(s);
        return;
    }
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// Appends `n` in decimal (as `write!(out, "{n}")`, without the
/// formatting machinery).
pub(crate) fn push_uint(out: &mut String, n: u32) {
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    let mut n = n;
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for &d in &digits[i..] {
        out.push(d as char);
    }
}

/// Appends `true` or `false`.
pub(crate) fn push_bool(out: &mut String, b: bool) {
    out.push_str(if b { "true" } else { "false" });
}

/// `JSON.stringify(s)`.
pub fn json_string(s: &str) -> String {
    let mut out = String::new();
    push_json_string(&mut out, s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_is_javascripts() {
        // Expected: Node 22's `parseInt`.
        assert_eq!(parse_int("  12abc", 0), Some(12.0));
        assert_eq!(parse_int("0x1F", 0), Some(31.0));
        assert_eq!(parse_int("-0x10", 0), Some(-16.0));
        assert_eq!(parse_int("0x", 0), None);
        assert_eq!(parse_int("0x10", 10), Some(0.0));
        assert_eq!(parse_int("\u{feff}\u{3000}7", 0), Some(7.0));
        assert_eq!(parse_int("\u{85}7", 0), None);
        assert_eq!(parse_int("1e5", 0), Some(1.0));
        assert_eq!(parse_int("", 0), None);
        assert_eq!(parse_int("ff", 16), Some(255.0));
        assert_eq!(parse_int("9", 8), None);
        let big = parse_int("123456789012345678901234567890", 0).unwrap();
        assert_eq!(number_to_string(big), "1.2345678901234568e+29");
        assert_eq!(number_to_string(parse_int("9007199254740993", 0).unwrap()), "9007199254740992");
        assert_eq!(number_to_string(parse_int("0x20000000000001", 0).unwrap()), "9007199254740992");
        assert_eq!(number_to_string(parse_int("0x20000000000003", 0).unwrap()), "9007199254740996");
        assert_eq!(number_to_string(parse_int("0x200000000000011", 0).unwrap()), "144115188075855900");
        let long = format!("0x1{}1", "0".repeat(20));
        assert_eq!(number_to_string(parse_int(&long, 0).unwrap()), "1.9342813113834067e+25");
        assert_eq!(parse_int(&"9".repeat(400), 0), Some(f64::INFINITY));
        assert_eq!(parse_int(&"f".repeat(300), 16), Some(f64::INFINITY));
    }

    #[test]
    fn number_is_javascripts() {
        assert_eq!(number(" 12 "), 12.0);
        assert_eq!(number(""), 0.0);
        assert_eq!(number("0x1F"), 31.0);
        assert_eq!(number("0b101"), 5.0);
        assert!(number("-0x1").is_nan());
        assert!(number("12abc").is_nan());
        assert!(number("inf").is_nan());
        assert_eq!(number("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(number(".5e1"), 5.0);
    }

    #[test]
    fn numbers_print_like_javascript() {
        for (n, s) in [(1e21, "1e+21"), (1e20, "100000000000000000000"), (0.000001, "0.000001"), (1e-7, "1e-7")] {
            assert_eq!(number_to_string(n), s);
        }
        assert_eq!(number_to_string(-0.0), "0");
        assert_eq!(number_to_string(f64::NAN), "NaN");
    }

    #[test]
    fn strings_escape_like_json_stringify() {
        assert_eq!(json_string("a\"b\\c\nd\u{1}é’\u{2028}"), "\"a\\\"b\\\\c\\nd\\u0001é’\u{2028}\"");
    }
}
