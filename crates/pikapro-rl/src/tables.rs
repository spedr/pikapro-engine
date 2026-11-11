//! What an [`Encoder`](crate::encode::Encoder) reads beside the view: the
//! format's dex and a few facts worked out once per dex ([`Tables`]). A
//! `Copy` handle; everything else about a species, a move, an item or an
//! ability is in the dex itself ([`Tables::dex`]).

use core::fmt;
use std::sync::{Mutex, OnceLock};

use pikapro::{Format, Gimmick, Mechanics};
use pikapro_data::{Dex, Effectiveness, MoveCategory, MoveId, SpeciesId, Type, TypeSet};

/// A move's facts as an encoder reads them on every step, packed (so the
/// whole dex stays in cache): [`Tables::move_facts`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MoveFacts {
    pub base_power: u8,
    /// [`Type`] discriminant.
    pub ty: u8,
    /// 0 physical, 1 special, 2 status.
    pub category: u8,
    /// Percent; 100 for a move that never misses.
    pub accuracy: u8,
    pub priority: i8,
    /// Maximum PP in a set (every PP Up, by the format's mechanics).
    pub max_pp: u8,
}

impl MoveFacts {
    fn of(m: &pikapro_data::MoveData, mechanics: Mechanics) -> MoveFacts {
        MoveFacts {
            base_power: m.base_power,
            ty: m.ty as u8,
            category: match m.category {
                MoveCategory::Physical => 0,
                MoveCategory::Special => 1,
                MoveCategory::Status => 2,
            },
            accuracy: m.accuracy.unwrap_or(100),
            priority: m.priority,
            max_pp: pikapro::battle::move_max_pp(mechanics, m),
        }
    }

    /// The move's type.
    pub fn move_type(self) -> Type {
        Type::TERA.get(self.ty as usize).copied().unwrap_or(Type::Normal)
    }
}

/// What is worked out once per dex and mechanics, and kept for the
/// process.
struct Data {
    /// Packed move facts by dex id.
    move_facts: Vec<MoveFacts>,
    /// Species' types by dex id.
    species_types: Vec<TypeSet>,
    /// `chart16[attack][defend]`: the multiplier against one type, in
    /// sixteenths (0, 8, 16, 32), by [`Type`] discriminant (Stellar
    /// neutral both ways).
    chart16: [[u16; Type::COUNT + 1]; Type::COUNT + 1],
    /// By species dex id: the types every forme of the species shares
    /// ([`pikapro_view::wildcard_types`]).
    wildcard_types: Vec<([Option<Type>; 2], bool)>,
}

/// Effectiveness in sixteenths (0, 4, 8, 16, 32, 64).
fn sixteenths(e: Effectiveness) -> u16 {
    match e {
        Effectiveness::Immune => 0,
        Effectiveness::Exp(n) if n >= 0 => 16 << n,
        Effectiveness::Exp(n) => 16 >> (-n),
    }
}

impl Data {
    fn build(dex: &Dex, mechanics: Mechanics) -> Data {
        Data {
            move_facts: dex.moves.iter().map(|m| MoveFacts::of(m, mechanics)).collect(),
            species_types: dex.species.iter().map(|s| s.types).collect(),
            chart16: core::array::from_fn(|a| {
                core::array::from_fn(|d| sixteenths(Type::TERA[a].against(Type::TERA[d])))
            }),
            wildcard_types: (0..dex.species.len())
                .map(|i| {
                    let base = dex.species(SpeciesId(i as u16)).base_species;
                    pikapro_view::wildcard_types(dex, base)
                })
                .collect(),
        }
    }
}

/// A format's dex and the facts encoders need of it: a `Copy` handle to
/// data built once per dex and mechanics.
#[derive(Clone, Copy)]
pub struct Tables {
    /// The format's id (`"championsvgc"`), for messages.
    pub format: &'static str,
    pub dex: &'static Dex,
    pub mechanics: Mechanics,
    /// Active Pokémon per side.
    pub active: u8,
    /// The format has Terastallization. Without it no Pokémon has a Tera
    /// type.
    pub tera: bool,
    data: &'static Data,
}

impl Tables {
    /// The tables of `format`'s dex and mechanics (built on first use,
    /// then shared).
    pub fn for_format(format: &Format) -> Tables {
        Tables {
            format: format.id,
            dex: format.dex,
            mechanics: format.mechanics,
            active: format.active_per_side() as u8,
            tera: format.gimmicks.contains(Gimmick::Terastallize),
            data: data_for(format.dex, format.mechanics),
        }
    }

    /// The multiplier of an attack of type `attack` against the types
    /// `defender` (plus an added type), in sixteenths: 16 neutral, 0
    /// immune, 2 for 1/8. Exact for up to three types. It is
    /// [`Type::against`] (the dex's type chart) in sixteenths.
    #[inline]
    pub fn effect16(&self, attack: Type, defender: TypeSet, added: Option<Type>) -> u32 {
        let row = &self.data.chart16[(attack as usize).min(Type::COUNT)];
        let at = |t: Type| u32::from(row[(t as usize).min(Type::COUNT)]);
        let mut m = at(defender.primary);
        if let Some(t) = defender.secondary {
            m = m * at(t) / 16;
        }
        if let Some(t) = added {
            m = m * at(t) / 16;
        }
        m
    }

    /// A move's packed facts.
    #[inline]
    pub fn move_facts(&self, id: MoveId) -> MoveFacts {
        self.data.move_facts.get(id.index()).copied().unwrap_or_default()
    }

    /// The types known of a Pokémon whose forme team preview hid, shown as
    /// `species` (its base species): the types all its formes share, and
    /// whether those are all of them.
    #[inline]
    pub fn wildcard_types(&self, species: SpeciesId) -> ([Option<Type>; 2], bool) {
        self.data.wildcard_types.get(species.index()).copied().unwrap_or(([None; 2], false))
    }

    /// A species' types.
    #[inline]
    pub fn species_types(&self, id: SpeciesId) -> TypeSet {
        self.data.species_types.get(id.index()).copied().unwrap_or(TypeSet::mono(Type::Normal))
    }
}

impl fmt::Debug for Tables {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tables")
            .field("format", &self.format)
            .field("dex", &self.dex.name)
            .field("mechanics", &self.mechanics)
            .field("active", &self.active)
            .field("tera", &self.tera)
            .finish()
    }
}

/// A dex (by address) and the mechanics its data was built for.
type CacheKey = (usize, Mechanics);

/// The data of `dex` under `mechanics`, built once for the life of the
/// process (dexes are `'static` and few, so this leaks a bounded amount).
fn data_for(dex: &'static Dex, mechanics: Mechanics) -> &'static Data {
    static CACHE: OnceLock<Mutex<Vec<(CacheKey, &'static Data)>>> = OnceLock::new();
    let key = (dex as *const Dex as usize, mechanics);
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, d)) = cache.iter().find(|(k, _)| *k == key) {
        return d;
    }
    let d: &'static Data = Box::leak(Box::new(Data::build(dex, mechanics)));
    cache.push((key, d));
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro_data::gen9::moves;

    #[test]
    fn built_once_per_dex() {
        let a = Tables::for_format(&Format::gen9_vgc());
        let b = Tables::for_format(&Format::gen9_singles());
        let c = Tables::for_format(&Format::champions_vgc());
        assert!(core::ptr::eq(a.data, b.data));
        assert!(!core::ptr::eq(a.data, c.data));
    }

    #[test]
    fn facts_match_the_dex() {
        for format in [Format::gen9_vgc(), Format::champions_vgc()] {
            let t = Tables::for_format(&format);
            for (i, m) in format.dex.moves.iter().enumerate() {
                let f = t.move_facts(MoveId(i as u16));
                assert_eq!((f.base_power, f.move_type(), f.priority), (m.base_power, m.ty, m.priority), "{}", m.name);
                assert_eq!(f.accuracy, m.accuracy.unwrap_or(100), "{}", m.name);
                assert_eq!(f.max_pp, pikapro::battle::move_max_pp(format.mechanics, m), "{}", m.name);
            }
            for (i, s) in format.dex.species.iter().enumerate() {
                assert_eq!(t.species_types(SpeciesId(i as u16)), s.types, "{}", s.name);
            }
            assert_eq!(t.tera, format.gimmicks.contains(Gimmick::Terastallize));
        }
        let gen9 = Tables::for_format(&Format::gen9_vgc());
        let champions = Tables::for_format(&Format::champions_vgc());
        // Maximum PP follows the mechanics (Champions has no PP Ups).
        assert_ne!(gen9.move_facts(moves::TACKLE).max_pp, champions.move_facts(moves::TACKLE).max_pp);
    }

    #[test]
    fn the_type_chart_in_sixteenths() {
        let t = Tables::for_format(&Format::gen9_vgc());
        for a in Type::ALL {
            for d in Type::ALL {
                let want = sixteenths(a.against(d));
                assert_eq!(t.effect16(a, TypeSet::mono(d), None), u32::from(want));
            }
            assert_eq!(t.effect16(Type::Stellar, TypeSet::mono(a), None), 16);
            // Burn Up's and Double Shock's `???` is no type: neutral.
            assert_eq!(t.effect16(a, TypeSet::mono(Type::Unknown), None), 16);
        }
        assert_eq!(t.effect16(Type::Ground, TypeSet::mono(Type::Flying), None), 0);
        let dual = TypeSet { primary: Type::Grass, secondary: Some(Type::Bug) };
        assert_eq!(t.effect16(Type::Fire, dual, None), 64);
        assert_eq!(t.effect16(Type::Fire, dual, Some(Type::Water)), 32);
    }
}
