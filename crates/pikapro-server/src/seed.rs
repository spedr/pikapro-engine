//! Showdown's seed strings (`sim/prng.ts`): what `new PRNG(seed)` reads,
//! what `getSeed()` writes, and `PRNG.generateSeed()`.

use pikapro::Seed;

use crate::json::{Json, js_number, js_parse_int};

/// A seed as Showdown's `PRNG` holds it: the string it started from
/// (`prng.startingSeed`, which the input log and the `end` log show) and
/// the generator it made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShowdownSeed {
    /// `startingSeed`: the seed as given (an array joined with commas).
    pub text: String,
    pub seed: Seed,
}

impl ShowdownSeed {
    /// `new PRNG(seed)`'s `setSeed`: `sodium,<hex>`, `gen5,<16 hex
    /// digits>`, or four comma-separated numbers (the Gen 5 LCG's 16-bit
    /// words). An empty seed is a fresh one (`PRNG.generateSeed()`).
    pub fn parse(text: &str) -> Result<ShowdownSeed, String> {
        if text.is_empty() {
            return Ok(generate());
        }
        let seed = if text.starts_with("sodium,") {
            // `seed.split(',')[1].padEnd(64, '0')`, then `bufWriteHex`
            // into 32 bytes: `parseInt` of each pair (NaN is 0, extra
            // digits are dropped).
            let hex: Vec<u16> = text.split(',').nth(1).unwrap_or("").encode_utf16().collect();
            let mut key = [0u8; 32];
            for (i, byte) in key.iter_mut().enumerate() {
                let pair: Vec<u16> = (0..2).map(|j| hex.get(2 * i + j).copied().unwrap_or(u16::from(b'0'))).collect();
                let pair = String::from_utf16_lossy(&pair);
                // A Uint8Array stores the value modulo 256.
                *byte = js_parse_int(&pair, 16).map_or(0, |v| v.rem_euclid(256.0) as u8);
            }
            Seed::Sodium(key)
        } else if let Some(rest) = text.strip_prefix("gen5,") {
            // `seed.slice(5, 9)`, ... `seed.slice(17, 21)`, each `parseInt(n, 16)`.
            let units: Vec<u16> = rest.encode_utf16().collect();
            let mut words = [0u16; 4];
            for (i, w) in words.iter_mut().enumerate() {
                let part = String::from_utf16_lossy(units.get(4 * i..(4 * i + 4).min(units.len())).unwrap_or(&[]));
                *w = word(js_parse_int(&part, 16)).ok_or_else(|| unsupported(text))?;
            }
            Seed::ShowdownGen5(words)
        } else if text.starts_with(|c: char| c.is_ascii_digit()) {
            // `seed.split(',').map(Number)`.
            let parts: Vec<&str> = text.split(',').collect();
            if parts.len() != 4 {
                return Err(unsupported(text));
            }
            let mut words = [0u16; 4];
            for (w, part) in words.iter_mut().zip(parts) {
                *w = word(Some(js_number(part))).ok_or_else(|| unsupported(text))?;
            }
            Seed::ShowdownGen5(words)
        } else {
            return Err(format!("Unrecognized RNG seed {text}"));
        };
        Ok(ShowdownSeed { text: text.to_string(), seed })
    }

    /// The `seed` option of `>start` (`new PRNG(options.seed || undefined)`):
    /// a string, an array (joined with commas) or nothing.
    pub fn from_option(value: Option<&Json>) -> Result<ShowdownSeed, String> {
        match value {
            None => Ok(generate()),
            Some(v) if !v.truthy() => Ok(generate()),
            Some(Json::String(s)) => ShowdownSeed::parse(s),
            // An array is truthy even when it joins to "", which
            // `setSeed` rejects.
            Some(a @ Json::Array(_)) => match a.to_js_string() {
                text if text.is_empty() => Err("Unrecognized RNG seed ".to_string()),
                text => ShowdownSeed::parse(&text),
            },
            Some(v) => Err(format!("PRNG: Seed {} must be a string", v.to_js_string())),
        }
    }

    /// `prng.getSeed()` of a fresh generator: `sodium,<64 hex digits>` or
    /// the Gen 5 words in decimal.
    pub fn current(&self) -> String {
        match self.seed {
            Seed::Sodium(key) => {
                let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
                format!("sodium,{hex}")
            }
            Seed::ShowdownGen5(w) => format!("{},{},{},{}", w[0], w[1], w[2], w[3]),
            Seed::Fast(s) => s.to_string(),
        }
    }
}

/// A Gen 5 seed word: Showdown's LCG does its arithmetic on 16-bit limbs,
/// so only integers in `0..65536` give a meaningful stream.
fn word(v: Option<f64>) -> Option<u16> {
    let v = v?;
    (v.fract() == 0.0 && (0.0..65536.0).contains(&v)).then_some(v as u16)
}

fn unsupported(text: &str) -> String {
    format!("pikapro-server: unsupported RNG seed {text} (Gen 5 seeds need four 16-bit words)")
}

/// `PRNG.generateSeed()`: `sodium,` and 32 random hex digits. The entropy
/// comes from the standard library's per-process hash keys and the clock;
/// it only has to differ between battles, as `crypto.getRandomValues` does.
pub fn generate() -> ShowdownSeed {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut words = [0u64; 2];
    for (i, w) in words.iter_mut().enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(now);
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_usize(i);
        *w = h.finish();
    }
    let text = format!("sodium,{:016x}{:016x}", words[0], words[1]);
    ShowdownSeed::parse(&text).expect("a generated seed parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_showdown_seeds() {
        let s = ShowdownSeed::parse("gen5,0001000200030004").unwrap();
        assert_eq!(s.seed, Seed::ShowdownGen5([1, 2, 3, 4]));
        assert_eq!(s.current(), "1,2,3,4");
        assert_eq!(ShowdownSeed::parse("1,2,3,4").unwrap().seed, Seed::ShowdownGen5([1, 2, 3, 4]));
        let s = ShowdownSeed::parse("sodium,0123").unwrap();
        let Seed::Sodium(key) = s.seed else { panic!() };
        assert_eq!(key[..3], [0x01, 0x23, 0]);
        assert_eq!(s.current(), format!("sodium,0123{}", "0".repeat(60)));
        // `parseInt("1g", 16)` is 1; `parseInt("zz", 16)` is NaN, stored as 0.
        let Seed::Sodium(key) = ShowdownSeed::parse("sodium,1gzz").unwrap().seed else { panic!() };
        assert_eq!(key[..2], [1, 0]);
        assert!(ShowdownSeed::parse("xyz").unwrap_err().starts_with("Unrecognized RNG seed"));
        let g = generate();
        assert!(g.text.starts_with("sodium,") && g.text.len() == 39, "{}", g.text);
        assert_ne!(generate().text, g.text);
        let arr = Json::parse("[1,2,3,4]").unwrap();
        assert_eq!(ShowdownSeed::from_option(Some(&arr)).unwrap().text, "1,2,3,4");
    }
}
