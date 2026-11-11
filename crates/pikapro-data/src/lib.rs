//! Static game data for the `pikapro` battle engine.
//!
//! This crate has **no behaviour**: it only describes what things *are*
//! (a species' base stats, a move's base power, the type chart). All
//! battle logic lives in `pikapro`.
//!
//! Every entity is addressed by a compact integer ID ([`SpeciesId`],
//! [`MoveId`], ...) that indexes into a [`Dex`]. IDs are what the simulator
//! stores in its hot state; names are only used at the edges (team import,
//! protocol rendering).
//!
//! The tables are generated from Pokémon Showdown's data by
//! `tools/datagen` in the repository, at the Showdown commit the engine is
//! checked against: [`gen9`] (Scarlet/Violet) and [`champions`] (Showdown's
//! Pokémon Champions mod), which share one ID space. The `legality`
//! feature adds what team validation needs (learnsets, events, ...).

#![cfg_attr(not(test), no_std)]
// Data must be integer-only to be deterministic across platforms.
#![deny(clippy::float_arithmetic)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

mod dex;
mod ids;
mod natures;
mod stats;
mod types;

/// The Pokémon Champions dex (generated).
pub mod champions;
/// The Scarlet/Violet dex and the shared ID constants (generated).
pub mod gen9;
#[cfg(feature = "legality")]
pub mod legality;

pub use dex::{
    AbilityData, AbilityFlags, AbilitySlots, Dex, Event, EventHandler, EventPrefix, EventSet, Fraction, GenderRule,
    ItemData, MoveCategory, MoveData, MoveFlags, MoveTarget, NameIndex, Nonstandard, Secondary, SelfSwitch,
    SpeciesData, StatBoosts, StatusId, find_handler,
};
pub use ids::{AbilityId, ItemId, MoveId, SpeciesId};
pub use natures::Nature;
pub use stats::{BoostId, Stat, Stats};
pub use types::{Effectiveness, TeraType, Type, TypeSet};

/// The characters of `text`'s Showdown ID (`toID`: `text.toLowerCase()`
/// with everything but `a-z` and `0-9` removed), without allocating. As in
/// JavaScript, the Kelvin sign lowercases to `k` and `İ` to `i` (plus a
/// combining dot, which is dropped).
pub fn id_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().filter_map(|c| match c {
        'a'..='z' | '0'..='9' => Some(c),
        'A'..='Z' => Some(c.to_ascii_lowercase()),
        '\u{130}' => Some('i'),
        '\u{212a}' => Some('k'),
        _ => None,
    })
}

/// Showdown's `toID` (`"Flutter Mane"` -> `"fluttermane"`).
pub fn to_id(text: &str) -> String {
    id_chars(text).collect()
}

/// Whether two names have the same Showdown ID (ignoring case, spaces and
/// punctuation).
pub fn id_eq(a: &str, b: &str) -> bool {
    id_chars(a).eq(id_chars(b))
}

/// A bound on the Showdown ID of any name in the dexes (datagen checks it):
/// a text whose ID is longer names nothing.
pub const MAX_ID_LEN: usize = 64;

/// A name's Showdown ID, computed once to be compared with many names
/// ([`Dex`]'s lookups, [`Nature::from_name`], [`Type::from_name`]).
#[derive(Clone, Copy)]
pub struct NameId {
    buf: [u8; MAX_ID_LEN],
    len: usize,
}

impl core::fmt::Debug for NameId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "NameId({:?})", self.as_str())
    }
}

impl NameId {
    /// `None` when the ID is longer than [`MAX_ID_LEN`] (no name has it):
    /// the text is read up to that point only.
    pub fn new(text: &str) -> Option<NameId> {
        let mut id = NameId { buf: [0; MAX_ID_LEN], len: 0 };
        for c in id_chars(text) {
            *id.buf.get_mut(id.len)? = c as u8;
            id.len += 1;
        }
        Some(id)
    }

    /// The ID as text.
    pub fn as_str(&self) -> &str {
        // Only ASCII letters and digits are written.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// Whether `name` has this ID.
    pub fn matches(&self, name: &str) -> bool {
        id_chars(name).map(|c| c as u8).eq(self.buf[..self.len].iter().copied())
    }

    /// This ID against `name`'s.
    pub fn cmp_name(&self, name: &str) -> core::cmp::Ordering {
        self.buf[..self.len].iter().copied().cmp(id_chars(name).map(|c| c as u8))
    }
}

/// JavaScript's white space and line terminators (what `String.prototype.trim`
/// removes and `\s` matches). Unlike [`char::is_whitespace`], U+FEFF is one
/// and U+0085 is not.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// Showdown's `Dex.getName`, which the team validator applies to nicknames:
/// runs of white space, `|`, `[`, `]`, `,` and U+202E become one space, the
/// result is trimmed and cut to 18 UTF-16 units (and trimmed again), then
/// runs of three or more combining marks ("zalgo") and the characters
/// U+239B–U+23B9 are removed.
///
/// A cut through a surrogate pair leaves JavaScript a lone surrogate; it is
/// U+FFFD here, as Node writes it out.
pub fn get_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(80));
    let mut units = 0;
    let mut pending_space = false;
    // `replace(/[|\s[\],\u202e]+/g, ' ').trim()`, stopping once past 18
    // units (the cut needs no more).
    for c in name.chars() {
        if is_js_whitespace(c) || matches!(c, '|' | '[' | ']' | ',' | '\u{202e}') {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            units += 1;
            pending_space = false;
        }
        out.push(c);
        units += c.len_utf16();
        if units > 18 {
            break;
        }
    }
    if units > 18 {
        // `substr(0, 18).trim()`.
        let mut cut = String::with_capacity(out.len());
        let mut n = 0;
        for c in out.chars() {
            let w = c.len_utf16();
            if n + w > 18 {
                if n < 18 {
                    cut.push('\u{fffd}');
                }
                break;
            }
            cut.push(c);
            n += w;
        }
        out = cut.trim_matches(is_js_whitespace).into();
    }
    let zalgo = |c: char| {
        matches!(c,
            '\u{300}'..='\u{36f}' | '\u{483}'..='\u{489}' | '\u{610}'..='\u{615}' | '\u{64b}'..='\u{65f}'
            | '\u{670}' | '\u{6d6}'..='\u{6dc}' | '\u{6df}'..='\u{6ed}' | '\u{e31}' | '\u{e34}'..='\u{e3a}'
            | '\u{e47}'..='\u{e4e}')
    };
    let brace = |c: char| ('\u{239b}'..='\u{23b9}').contains(&c);
    if out.chars().any(|c| zalgo(c) || brace(c)) {
        // `replace(/[zalgo]{3,}/g, '')`, then `replace(/[\u239b-\u23b9]/g, '')`.
        let chars: Vec<char> = out.chars().collect();
        let mut kept = String::with_capacity(out.len());
        let mut i = 0;
        while i < chars.len() {
            let run = chars[i..].iter().take_while(|&&c| zalgo(c)).count();
            if run >= 3 {
                i += run;
            } else {
                let n = run.max(1);
                kept.extend(&chars[i..i + n]);
                i += n;
            }
        }
        out = kept.chars().filter(|&c| !brace(c)).collect();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_id_normalises() {
        assert_eq!(to_id("Flutter Mane"), "fluttermane");
        assert_eq!(to_id("U-turn"), "uturn");
        assert_eq!(to_id("\u{212a}ing's Rock \u{130}"), "kingsrocki");
        assert!(id_eq("King's Rock", "kingsrock"));
        assert!(!id_eq("Tackle", "Tackles"));
        let id = NameId::new("Flutter Mane").unwrap();
        assert!(id.matches("flutter-mane") && id.as_str() == "fluttermane");
        assert!(NameId::new(&"a".repeat(MAX_ID_LEN + 1)).is_none());
        assert!(NameId::new(&" ".repeat(1000)).is_some_and(|id| id.as_str().is_empty()));
    }

    #[test]
    fn get_name_is_showdowns() {
        // Expected: Showdown 0.11.11's `Dex.getName`.
        assert_eq!(get_name("  A|B [x],\u{3000}\u{feff}y  "), "A B x y");
        assert_eq!(get_name("a\u{85}b"), "a\u{85}b");
        assert_eq!(get_name("ThisNameIsFarTooLongToKeep"), "ThisNameIsFarTooLo");
        assert_eq!(get_name("1234567890123456 xyz"), "1234567890123456 x");
        assert_eq!(get_name("12345678901234567 xyz"), "12345678901234567");
        assert_eq!(get_name("12345678901234567\u{1f600}"), "12345678901234567\u{fffd}");
        assert_eq!(get_name("a\u{300}\u{301}\u{302}b\u{300}\u{301}c"), "ab\u{300}\u{301}c");
        assert_eq!(get_name("x\u{239b}\u{23b9}\u{23ba}"), "x\u{23ba}");
        assert_eq!(get_name("||"), "");
    }

    /// The generated index is sorted by this crate's `to_id` (datagen sorts
    /// by Showdown's) and every name finds its own row.
    #[test]
    fn every_name_finds_its_row() {
        type Names<'a> = &'a dyn Fn(usize) -> &'static str;
        type Find<'a> = &'a dyn Fn(&str) -> Option<u16>;
        fn check(order: &[u16], len: usize, names: Names<'_>, find: Find<'_>) {
            assert_eq!(order.len(), len);
            assert!(order.windows(2).all(|w| to_id(names(w[0] as usize)) < to_id(names(w[1] as usize))));
            for &i in order {
                let name = names(i as usize);
                assert_eq!(find(name), Some(i), "{name}");
                assert_eq!(find(&name.to_uppercase().replace(' ', "-")), Some(i), "{name}");
            }
        }
        for dex in [&gen9::DEX, &champions::DEX] {
            let n = dex.names;
            check(n.species, dex.species.len(), &|i| dex.species[i].name, &|s| dex.species_by_name(s).map(|x| x.0));
            check(n.moves, dex.moves.len(), &|i| dex.moves[i].name, &|s| dex.move_by_name(s).map(|x| x.0));
            check(n.abilities, dex.abilities.len(), &|i| dex.abilities[i].name, &|s| {
                dex.ability_by_name(s).map(|x| x.0)
            });
            check(n.items, dex.items.len(), &|i| dex.items[i].name, &|s| dex.item_by_name(s).map(|x| x.0));
            assert_eq!(dex.species_by_name(""), None);
            assert_eq!(dex.move_by_name(&"x".repeat(1 << 20)), None);
        }
    }
}

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
