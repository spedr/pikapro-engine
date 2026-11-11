//! [`Mode::Sets`](crate::Mode::Sets): competitive-style sets from
//! Showdown's random-battle data ([`crate::sets`]).

use std::collections::BTreeMap;

use pikapro::{BattleRng, PokemonSet};
use pikapro_data::{ItemId, SpeciesId};

use crate::feasible::NumberItems;
use crate::sets::SetTable;
use crate::{Redraws, Spreads, TeamGenerator, spread};

/// The sets of a table a generator can use, by Pokédex number and forme.
#[derive(Debug)]
pub(crate) struct SetsPool {
    table: &'static SetTable,
    /// Per Pokédex number (by number), per forme (by species ID), the
    /// rows' indices.
    numbers: Vec<Vec<Vec<usize>>>,
}

impl SetsPool {
    pub fn new(g: &TeamGenerator, table: &'static SetTable) -> SetsPool {
        let dex = g.pools.dex;
        let mut by_number: BTreeMap<i16, BTreeMap<SpeciesId, Vec<usize>>> = BTreeMap::new();
        for (i, set) in table.iter().enumerate() {
            let ok = dex.species(set.species).nonstandard.is_none()
                && g.pools.species_ok(set.species)
                && g.pools.ability_ok(set.ability)
                && set.item.is_none_or(|i| g.pools.item_ok(i))
                && set.moves().iter().all(|&m| g.pools.move_ok(m));
            if ok {
                by_number.entry(dex.species(set.species).num).or_default().entry(set.species).or_default().push(i);
            }
        }
        let numbers = by_number.into_values().map(|formes| formes.into_values().collect()).collect();
        SetsPool { table, numbers }
    }

    pub fn numbers(&self) -> usize {
        self.numbers.len()
    }

    /// What each Pokédex number's sets can hold (for the team rules).
    pub fn number_items(&self, g: &TeamGenerator) -> Vec<NumberItems> {
        let vf = g.validator.format();
        self.numbers
            .iter()
            .map(|formes| {
                let rows = || formes.iter().flatten().map(|&r| self.table.get(r));
                NumberItems {
                    restricted: rows().all(|r| vf.is_restricted_species(r.species)),
                    free: rows().any(|r| r.item.is_none()),
                    items: rows().filter_map(|r| r.item).collect(),
                }
            })
            .collect()
    }

    /// Draws a Pokédex number, a forme and one of its sets; `None`
    /// (counted in `redraws`) if a rule forces a redraw.
    pub fn draw(
        &self,
        g: &TeamGenerator,
        rng: &mut BattleRng,
        used_numbers: &[usize],
        used_items: &[ItemId],
        redraws: &mut Redraws,
    ) -> Option<(usize, PokemonSet)> {
        let n = rng.random(self.numbers.len() as u32) as usize;
        if used_numbers.contains(&n) {
            redraws.add("Species Clause");
            return None;
        }
        let formes = &self.numbers[n];
        let rows = &formes[rng.random(formes.len() as u32) as usize];
        let mut data = self.table.get(rows[rng.random(rows.len() as u32) as usize]);
        if data.item.is_some_and(|i| used_items.contains(&i)) {
            // Another of the forme's sets, among those whose item is free.
            redraws.add("Item Clause");
            let free: Vec<usize> = rows
                .iter()
                .copied()
                .filter(|&r| self.table.get(r).item.is_none_or(|i| !used_items.contains(&i)))
                .collect();
            if free.is_empty() {
                return None;
            }
            data = self.table.get(free[rng.random(free.len() as u32) as usize]);
        }
        let stat_points = g.validator.format().champions_mod;
        let (nature, investment) = match g.options().spreads {
            Spreads::Competitive => spread::competitive(g.pools.dex, &data, stat_points),
            Spreads::Showdown => spread::showdown(&data, stat_points),
        };
        let investment = g.mark_level_50(investment);
        let mut set = PokemonSet::new(data.species)
            .with_level(g.level)
            .with_ability(data.ability)
            .with_nature(nature)
            .with_investment(investment)
            .with_moves(data.moves());
        set.item = data.item;
        // Showdown's Tera type for the set; the species' required one wins.
        set.tera_type = match g.pools.tera_type(rng, data.species) {
            Some(t) if g.pools.dex.species(data.species).required_tera_type.is_some() => Some(t),
            Some(t) => data.tera_type.or(Some(t)),
            None => None,
        };
        set.gender = g.pools.gender(rng, data.species);
        if let Some(played) = g.accept(&set, redraws) {
            return Some((n, played));
        }
        // An event's IVs or gender: 31s and no gender.
        set.investment = spread::max_ivs(set.investment);
        set.gender = g.pools.fixed_gender(data.species);
        if let Some(played) = g.accept(&set, redraws) {
            return Some((n, played));
        }
        redraws.add("Set rejected");
        None
    }
}
