//! What both modes share: the [`Restrict`] filters, genders, Tera types
//! and the final check of a set.

use pikapro::{BattleRng, Format, Gender, Gimmick, PokemonSet};
use pikapro_data::{AbilityId, Dex, GenderRule, ItemId, MoveId, SpeciesId, Type};
use pikapro_validate::SetInfo;

use crate::{Redraws, Restrict, TeamGenerator};

/// A [`Restrict`] as lookup tables, plus the format's dex.
#[derive(Debug)]
pub(crate) struct Pools {
    pub dex: &'static Dex,
    /// The sets get a Tera type (Gen 9 formats with Terastallization).
    pub tera: bool,
    species: Option<Vec<bool>>,
    moves: Option<Vec<bool>>,
    abilities: Option<Vec<bool>>,
    items: Option<Vec<bool>>,
}

fn table<T: Copy>(ids: &Option<Vec<T>>, len: usize, index: impl Fn(T) -> usize) -> Option<Vec<bool>> {
    ids.as_ref().map(|ids| {
        let mut t = vec![false; len];
        for &id in ids {
            if let Some(slot) = t.get_mut(index(id)) {
                *slot = true;
            }
        }
        t
    })
}

fn allows(t: &Option<Vec<bool>>, i: usize) -> bool {
    t.as_ref().is_none_or(|t| t.get(i).copied().unwrap_or(false))
}

impl Pools {
    pub fn new(format: &Format, restrict: &Restrict) -> Pools {
        let dex = format.dex;
        Pools {
            dex,
            tera: format.gimmicks.contains(Gimmick::Terastallize),
            species: table(&restrict.species, dex.species.len(), SpeciesId::index),
            moves: table(&restrict.moves, dex.moves.len(), MoveId::index),
            abilities: table(&restrict.abilities, dex.abilities.len(), AbilityId::index),
            items: table(&restrict.items, dex.items.len(), ItemId::index),
        }
    }

    pub fn species_ok(&self, s: SpeciesId) -> bool {
        allows(&self.species, s.index())
    }

    pub fn move_ok(&self, m: MoveId) -> bool {
        allows(&self.moves, m.index())
    }

    pub fn ability_ok(&self, a: AbilityId) -> bool {
        allows(&self.abilities, a.index())
    }

    pub fn item_ok(&self, i: ItemId) -> bool {
        allows(&self.items, i.index())
    }

    /// A gender for `species`: its fixed one, none if genderless, else
    /// drawn by its ratio.
    pub fn gender(&self, rng: &mut BattleRng, species: SpeciesId) -> Option<Gender> {
        match self.dex.species(species).gender {
            GenderRule::Male => Some(Gender::Male),
            GenderRule::Female => Some(Gender::Female),
            GenderRule::Genderless => None,
            GenderRule::Ratio { male_per_mille } => {
                Some(if rng.random(1000) < male_per_mille as u32 { Gender::Male } else { Gender::Female })
            }
        }
    }

    /// The species' fixed gender, if any (what a set falls back to).
    pub fn fixed_gender(&self, species: SpeciesId) -> Option<Gender> {
        match self.dex.species(species).gender {
            GenderRule::Male => Some(Gender::Male),
            GenderRule::Female => Some(Gender::Female),
            _ => None,
        }
    }

    /// A uniform Tera type (Stellar included), or the species' required
    /// one; `None` without Terastallization. Draws either way.
    pub fn tera_type(&self, rng: &mut BattleRng, species: SpeciesId) -> Option<Type> {
        if !self.tera {
            return None;
        }
        let t = Type::TERA[rng.random(Type::TERA.len() as u32) as usize];
        Some(self.dex.species(species).required_tera_type.map_or(t, |r| r.ty()))
    }
}

impl TeamGenerator {
    /// Whether pikapro-validate accepts `set` on its own; counts its problems
    /// otherwise.
    pub(crate) fn check(&self, set: &PokemonSet, redraws: &mut Redraws) -> bool {
        self.accept(set, redraws).is_some()
    }

    /// [`check`](Self::check), with the set as the validator leaves it if
    /// it accepts it: what Showdown's server plays (a set whose move or
    /// item makes another forme, such as Keldeo with Secret Sword, plays
    /// that forme; `pikapro_validate::Checked::team`).
    pub(crate) fn accept(&self, set: &PokemonSet, redraws: &mut Redraws) -> Option<PokemonSet> {
        let (problems, played) = self.validator.check_set(set, &SetInfo::default());
        redraws.add_problems(&problems);
        problems.is_empty().then_some(played)
    }
}
