//! Small helpers: text and seeds.

/// Showdown's `toID`.
pub use pikapro_data::to_id;

/// Showdown's `Utils.escapeHTML`.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '/' => out.push_str("&#x2f;"),
            _ => out.push(c),
        }
    }
    out
}

/// Splits `text` at the first `sep`: `("a", "b c")` for `"a b c"`, the
/// whole text and `""` without one.
pub fn split_first(text: &str, sep: char) -> (&str, &str) {
    text.split_once(sep).unwrap_or((text, ""))
}

/// JavaScript's `text.length`.
pub use pikapro_protocol::js::length as js_length;

/// Showdown's `Dex.getName`, which `Chat.namefilter` ends with.
pub use pikapro_data::get_name;

/// A `sodium` seed for `>start` (Showdown's default generator) from
/// `entropy`: 32 bytes of SplitMix64 output, in hex.
pub fn sodium_seed(entropy: u64) -> String {
    let mut z = entropy;
    let mut out = String::from("sodium,");
    for _ in 0..4 {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut x = z;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.push_str(&format!("{:016x}", x ^ (x >> 31)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_escapes() {
        assert_eq!(to_id("PIKAPRO a 12"), "pikaproa12");
        assert_eq!(escape_html("<b>'x'</b>"), "&lt;b&gt;&apos;x&apos;&lt;&#x2f;b&gt;");
        assert_eq!(split_first("move 1", ' '), ("move", "1"));
        assert_eq!(split_first("undo", ' '), ("undo", ""));
        assert_eq!(js_length("a\u{1F600}"), 3);
    }

    #[test]
    fn names_as_showdown_cleans_them() {
        assert_eq!(get_name("  Ash|K,[x] "), "Ash K x");
        assert_eq!(get_name("a\u{202e}b\r\n\tc"), "a b c");
        assert_eq!(get_name("abcdefghijklmnopqrstuvwxyz"), "abcdefghijklmnopqr");
        assert_eq!(get_name("abcdefghijklmnopq rst"), "abcdefghijklmnopq");
        assert_eq!(get_name("ze\u{301}\u{302}\u{303}d a\u{301}"), "zed a\u{301}");
        assert_eq!(get_name("x\u{23b0}y"), "xy");
    }

    #[test]
    fn sodium_seeds() {
        let seed = sodium_seed(7);
        assert!(seed.starts_with("sodium,") && seed.len() == 7 + 64, "{seed}");
        assert_eq!(seed, sodium_seed(7));
        assert_ne!(seed, sodium_seed(8));
        assert!(pikapro_server::ShowdownSeed::parse(&seed).is_ok());
    }
}
