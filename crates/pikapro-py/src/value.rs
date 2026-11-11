//! Engine values as a generic tree, for typed Python objects.
//!
//! The engine's public types (`BattleEvent`, `RequestData`, `BattleState`,
//! ...) all derive `Debug`, and derived `Debug` output is a faithful,
//! structural rendering of a value: `Name { field: value, .. }`,
//! `Name(value, ..)`, `Name`, `[..]`, `(..)`, integers, booleans and
//! strings. [`to_value`] parses it back into a [`Value`], so every type,
//! including event variants added later, reaches Python without a binding
//! per variant. On the way, compact IDs become names (`SpeciesId(445)` →
//! `"Garchomp"`), `Some(x)` becomes `x`, `Stats([..])` gets stat keys and
//! a `TypeSet` becomes a list of types.

use core::fmt::Debug;

use pikapro_data::{AbilityId, Dex, ItemId, MoveId, SpeciesId, Stat};

/// Fields of the engine's public types whose type is an enum with a
/// variant named `None` rather than an `Option` (`Status`, `FailNote`,
/// `AbilityNote`, `ItemEndNote`, `LineArg`, `SwitchFlag`, `Stab`). Their
/// `None` prints like `Option::None`, so it is told apart by the field
/// name: it becomes the string `"None"`, not Python's `None`. No field of
/// these names is an `Option` (a test checks the engine's source).
pub const NONE_VARIANT_FIELDS: [&str; 5] = ["status", "note", "what", "switch_flag", "stab"];

/// A parsed engine value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i128),
    /// A string field, or the name an ID stands for.
    Str(String),
    /// A unit variant or field-less struct (`P1`, `Burn`, `Start`).
    Unit(String),
    List(Vec<Value>),
    Tuple(Vec<Value>),
    /// `Name { field: value, .. }`: a struct or struct variant.
    Record {
        kind: String,
        fields: Vec<(String, Value)>,
    },
    /// `Name(value, ..)`: a tuple struct or tuple variant.
    Variant {
        kind: String,
        values: Vec<Value>,
    },
}

/// Parses `value`'s `Debug` output, naming IDs from `dex`.
///
/// Output outside the grammar would be a bug (a hand-written `Debug`
/// impl, say): debug builds and tests panic, release builds fall back to
/// the raw text as a string.
pub fn to_value<T: Debug + ?Sized>(value: &T, dex: &'static Dex) -> Value {
    let text = format!("{value:?}");
    match parse(&text, dex) {
        Some(v) => v,
        None => {
            debug_assert!(false, "pikapro-py: Debug output outside the grammar: {text}");
            Value::Str(text)
        }
    }
}

/// Parses derived-`Debug` text. `None` if it isn't in that grammar.
pub fn parse(text: &str, dex: &'static Dex) -> Option<Value> {
    let mut p = Parser { s: text.as_bytes(), i: 0, dex };
    let v = p.value()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    dex: &'static Dex,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Option<String> {
        self.ws();
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    /// Values separated by commas up to `close` (a trailing comma allowed).
    fn seq(&mut self, close: u8) -> Option<Vec<Value>> {
        let mut out = Vec::new();
        loop {
            if self.eat(close) {
                return Some(out);
            }
            out.push(self.value()?);
            if !self.eat(b',') {
                return self.eat(close).then_some(out);
            }
        }
    }

    fn value(&mut self) -> Option<Value> {
        match self.peek()? {
            b'[' => {
                self.i += 1;
                self.seq(b']').map(Value::List)
            }
            b'(' => {
                self.i += 1;
                self.seq(b')').map(Value::Tuple)
            }
            b'"' => self.string().map(Value::Str),
            b'\'' => self.char_lit().map(|c| Value::Str(c.to_string())),
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.named(),
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        core::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().map(Value::Int)
    }

    fn string(&mut self) -> Option<String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => out.push(self.escape()?),
                _ => {
                    // Copy a whole UTF-8 sequence.
                    let start = self.i - 1;
                    while self.s.get(self.i).is_some_and(|b| b & 0xC0 == 0x80) {
                        self.i += 1;
                    }
                    out.push_str(core::str::from_utf8(&self.s[start..self.i]).ok()?);
                }
            }
        }
    }

    fn char_lit(&mut self) -> Option<char> {
        self.i += 1;
        let c = if self.s.get(self.i) == Some(&b'\\') {
            self.i += 1;
            self.escape()?
        } else {
            let rest = core::str::from_utf8(&self.s[self.i..]).ok()?;
            let c = rest.chars().next()?;
            self.i += c.len_utf8();
            c
        };
        (self.s.get(self.i) == Some(&b'\'')).then(|| {
            self.i += 1;
            c
        })
    }

    fn escape(&mut self) -> Option<char> {
        let c = *self.s.get(self.i)?;
        self.i += 1;
        Some(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0' => '\0',
            b'u' => {
                // \u{XXXX}
                let close = self.s[self.i..].iter().position(|&b| b == b'}')?;
                let hex = core::str::from_utf8(&self.s[self.i + 1..self.i + close]).ok()?;
                self.i += close + 1;
                char::from_u32(u32::from_str_radix(hex, 16).ok()?)?
            }
            other => other as char,
        })
    }

    fn named(&mut self) -> Option<Value> {
        let name = self.ident()?;
        match name.as_str() {
            "true" => return Some(Value::Bool(true)),
            "false" => return Some(Value::Bool(false)),
            "None" => return Some(Value::None),
            _ => {}
        }
        if self.eat(b'{') {
            let mut fields = Vec::new();
            loop {
                if self.eat(b'}') {
                    break;
                }
                let key = self.ident()?;
                if !self.eat(b':') {
                    return None;
                }
                fields.push((key, self.value()?));
                if !self.eat(b',') {
                    if !self.eat(b'}') {
                        return None;
                    }
                    break;
                }
            }
            return Some(self.record(name, fields));
        }
        if self.eat(b'(') {
            let values = self.seq(b')')?;
            return Some(self.variant(name, values));
        }
        Some(Value::Unit(name))
    }

    fn record(&self, kind: String, mut fields: Vec<(String, Value)>) -> Value {
        // A field whose type is an enum with a `None` variant (not an
        // `Option`): its `None` is that variant, a name like any other.
        for (k, v) in &mut fields {
            if *v == Value::None && NONE_VARIANT_FIELDS.contains(&k.as_str()) {
                *v = Value::Unit("None".into());
            }
        }
        if kind == "TypeSet" {
            // `TypeSet { primary, secondary }` → `[primary, secondary?]`.
            let types = fields.into_iter().map(|(_, v)| v).filter(|v| *v != Value::None).collect();
            return Value::List(types);
        }
        Value::Record { kind, fields }
    }

    fn variant(&self, kind: String, mut values: Vec<Value>) -> Value {
        let id = match (&values[..], kind.as_str()) {
            ([Value::Int(n)], "SpeciesId" | "MoveId" | "AbilityId" | "ItemId") => Some(*n),
            _ => None,
        };
        if let Some(n) = id {
            return Value::Str(self.name(&kind, n));
        }
        match kind.as_str() {
            "Some" if values.len() == 1 => values.pop().unwrap_or(Value::None),
            // `Effect::Status(Status::None)`.
            "Status" if values == [Value::None] => Value::Variant { kind, values: vec![Value::Unit("None".into())] },
            "Stats" if values.len() == 1 => match values.pop() {
                Some(Value::List(stats)) if stats.len() == 6 => {
                    Value::Record { kind, fields: Stat::ALL.iter().map(|s| s.id().to_string()).zip(stats).collect() }
                }
                other => Value::Variant { kind, values: other.into_iter().collect() },
            },
            _ => Value::Variant { kind, values },
        }
    }

    fn name(&self, kind: &str, n: i128) -> String {
        let i = n as usize;
        let dex = self.dex;
        let name = match kind {
            "SpeciesId" => dex.species.get(i).map(|_| dex.species(SpeciesId(n as u16)).name),
            "MoveId" => dex.moves.get(i).map(|_| dex.move_(MoveId(n as u16)).name),
            "AbilityId" => dex.abilities.get(i).map(|_| dex.ability(AbilityId(n as u16)).name),
            _ => dex.items.get(i).map(|_| dex.item(ItemId(n as u16)).name),
        };
        name.map_or_else(|| format!("{kind}({n})"), str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::testing::sample_team;
    use pikapro::{Battle, BattleOptions, Format, Seed, SideId};
    use pikapro_data::gen9::{abilities, items, moves, species};
    use pikapro_data::{Stats, Type, TypeSet};

    fn dex() -> &'static Dex {
        &pikapro_data::gen9::DEX
    }

    fn str(s: &str) -> Value {
        Value::Str(s.into())
    }

    #[test]
    fn parses_the_grammar() {
        let v = parse(r#"A { b: Some(-3), c: [None, true], d: ("x\"y", 'z'), e: B(1, C), f: SpeciesId(0) }"#, dex());
        let Some(Value::Record { kind, fields }) = v else { panic!("{v:?}") };
        assert_eq!(kind, "A");
        assert_eq!(fields[0].1, Value::Int(-3));
        assert_eq!(fields[1].1, Value::List(vec![Value::None, Value::Bool(true)]));
        assert_eq!(fields[2].1, Value::Tuple(vec![Value::Str("x\"y".into()), Value::Str("z".into())]));
        assert_eq!(
            fields[3].1,
            Value::Variant { kind: "B".into(), values: vec![Value::Int(1), Value::Unit("C".into())] }
        );
        assert!(matches!(&fields[4].1, Value::Str(_)));
        assert_eq!(parse("A {", dex()), None);
    }

    /// IDs become names, `Stats` a record keyed by stat, a `TypeSet` a list.
    #[test]
    fn ids_stats_and_type_sets_have_their_shapes() {
        assert_eq!(to_value(&species::GARCHOMP, dex()), str("Garchomp"));
        assert_eq!(to_value(&moves::PROTECT, dex()), str("Protect"));
        assert_eq!(to_value(&abilities::ROUGH_SKIN, dex()), str("Rough Skin"));
        assert_eq!(to_value(&Some(items::LEFTOVERS), dex()), str("Leftovers"));
        let stats = to_value(&Stats::<u16>([1, 2, 3, 4, 5, 6]), dex());
        let keys = ["hp", "atk", "def", "spa", "spd", "spe"];
        let fields = keys.iter().zip(1..).map(|(k, v)| (k.to_string(), Value::Int(v))).collect();
        assert_eq!(stats, Value::Record { kind: "Stats".into(), fields });
        let unit = |s: &str| Value::Unit(s.into());
        let two = TypeSet { primary: Type::Dragon, secondary: Some(Type::Ground) };
        assert_eq!(to_value(&two, dex()), Value::List(vec![unit("Dragon"), unit("Ground")]));
        let one = TypeSet { primary: Type::Fire, secondary: None };
        assert_eq!(to_value(&one, dex()), Value::List(vec![unit("Fire")]));
    }

    /// `Status::None` (an enum variant, not an `Option`) is the name
    /// `"None"`; an `Option`'s `None` stays `Value::None`.
    #[test]
    fn none_variants_are_names() {
        let format = Format::gen9_singles();
        let battle = Battle::new(
            format.clone(),
            [sample_team(&format), sample_team(&format)],
            BattleOptions::new(Seed::fast(1)),
        )
        .unwrap();
        let Value::Record { fields, .. } = to_value(&battle.state().side(SideId::P1).team[0], dex()) else { panic!() };
        let get = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(get("status"), Value::Unit("None".into()));
        assert_eq!(get("switch_flag"), Value::Unit("None".into()));
        let v = parse("A { status: None, item: None, x: Status(None) }", dex()).unwrap();
        let Value::Record { fields, .. } = v else { panic!() };
        assert_eq!(fields[0].1, Value::Unit("None".into()));
        assert_eq!(fields[1].1, Value::None);
        assert_eq!(fields[2].1, Value::Variant { kind: "Status".into(), values: vec![Value::Unit("None".into())] });
    }

    /// No field named in [`NONE_VARIANT_FIELDS`] is an `Option` in the
    /// engine's public types (their `None` would become a name).
    #[test]
    fn none_variant_fields_are_never_options() {
        // The files of the types Python sees (events, state, requests,
        // results, teams); `resolve/` holds function parameters.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pikapro/src");
        let mut checked = 0;
        for file in ["event.rs", "state.rs", "request.rs", "result.rs", "team.rs", "battle.rs", "choice.rs"] {
            let path = root.join(file);
            let text = std::fs::read_to_string(&path).unwrap();
            for line in text.lines() {
                let line = line.trim().trim_start_matches("pub ").trim_start_matches("pub(crate) ");
                for name in NONE_VARIANT_FIELDS {
                    if let Some(ty) = line.strip_prefix(name).and_then(|r| r.strip_prefix(':')) {
                        checked += 1;
                        assert!(!ty.trim().starts_with("Option"), "{}: `{line}`", path.display());
                    }
                }
            }
        }
        assert!(checked > 20, "{checked}");
    }

    /// Parses (panicking on a fallback, in tests) everything a battle
    /// shows: each side's request and the state before every decision,
    /// the result and every event. Returns the number of events.
    fn sweep(format: &Format, teams: [pikapro::Team; 2], seed: u64) -> usize {
        let dex = format.dex;
        let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
        let mut rng = pikapro::BattleRng::new(Seed::fast(!seed));
        while !battle.is_over() {
            for side in SideId::BOTH {
                to_value(&battle.request_data(side), dex);
                let legal = battle.legal_actions(side);
                if !legal.is_waiting() {
                    let choice = legal.sample(&mut rng);
                    battle.submit(side, choice).unwrap();
                }
            }
            to_value(battle.state(), dex);
        }
        to_value(&battle.result(), dex);
        for event in battle.events() {
            to_value(event, dex);
        }
        battle.events().len()
    }

    #[test]
    fn every_engine_value_parses() {
        for format in Format::presets() {
            sweep(format, [sample_team(format), sample_team(format)], 3);
        }
    }

    /// The same on random legal teams, in every preset and mode: many
    /// more species, moves, items, abilities and events.
    #[test]
    fn every_engine_value_parses_on_random_teams() {
        let mut events = 0;
        for format in Format::presets() {
            for mode in pikapro_teams::Mode::ALL {
                let g = crate::teams::generator(format, mode, pikapro_teams::Options::default()).unwrap();
                for seed in 0..3 {
                    let [a, b] = crate::teams::pair(&g, seed).unwrap();
                    events += sweep(format, [a.team, b.team], seed);
                }
            }
        }
        assert!(events > 1000, "{events} events");
    }
}
