//! [`Mode::Uniform`](crate::Mode::Uniform): every legal choice uniformly.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use pikapro::{BattleRng, Investment, PokemonSet};
use pikapro_data::gen9::moves;
use pikapro_data::legality::BattleOnly;
use pikapro_data::{AbilityId, ItemId, MoveId, Nature, SpeciesId, Stats};
use pikapro_validate::ProblemKind;
use pikapro_validate::rules::{Check, Target};

use crate::feasible::NumberItems;
use crate::{Redraws, TeamGenerator, spread};

/// A forme a Pokédex number can be drawn as.
#[derive(Clone, Debug)]
struct Forme {
    /// The species the set registers (a Mega's base species).
    species: SpeciesId,
    /// The forme's required items (Plates, masks, Mega Stones, ...), one of
    /// which it holds; empty: any legal item.
    required_items: Vec<ItemId>,
    required_move: Option<MoveId>,
    abilities: Vec<AbilityId>,
    /// Items that would make it another forme of its number (the others'
    /// required items, forced-forme items).
    excluded_items: Vec<ItemId>,
    /// The moves it can have on its own, by move ID; worked out on first
    /// use (every forme that passes the checks here has some).
    moves: OnceLock<Vec<MoveId>>,
}

/// The uniform mode's pools for one generator.
#[derive(Debug)]
pub(crate) struct UniformPool {
    formes: Vec<Forme>,
    /// Forme indices per Pokédex number, by number.
    numbers: Vec<Vec<usize>>,
    /// The items any set may hold: every legal item but the Mega Stones.
    items: Vec<ItemId>,
}

/// The attributes a set is checked with while its moves are drawn.
fn probe(g: &TeamGenerator, species: SpeciesId, ability: AbilityId, item: Option<ItemId>) -> PokemonSet {
    let stat_points = g.validator.format().champions_mod;
    let investment = if stat_points {
        Investment::StatPoints(Stats::new(1, 0, 0, 0, 0, 0))
    } else {
        Investment::Evs { evs: Stats::new(4, 0, 0, 0, 0, 0), ivs: Stats::splat(31) }
    };
    let mut set = PokemonSet::new(species)
        .with_level(g.level)
        .with_ability(ability)
        .with_nature(Nature::Hardy)
        .with_investment(g.mark_level_50(investment));
    set.item = item;
    set.tera_type = g.pools.dex.species(species).required_tera_type.map(|t| t.ty());
    set
}

impl UniformPool {
    pub fn new(g: &TeamGenerator) -> UniformPool {
        let dex = g.pools.dex;
        let vf = g.validator.format();
        let leg = vf.legality;
        let legal: Vec<ItemId> = (0..dex.items.len() as u16)
            .map(ItemId)
            .filter(|&i| {
                dex.item(i).nonstandard.is_none()
                    && g.pools.item_ok(i)
                    && !matches!(vf.check(Target::Item(i)), Check::Banned(_))
            })
            .collect();
        // Mega Stones are held by the Megas' formes (below), not drawn for
        // species that can't use them.
        let items: Vec<ItemId> = legal.iter().copied().filter(|&i| dex.item(i).mega.is_empty()).collect();
        let mut by_number: BTreeMap<i16, Vec<Forme>> = BTreeMap::new();
        for s in (0..dex.species.len() as u16).map(SpeciesId) {
            let data = dex.species(s);
            // The cosmetic formes added to the ID space for the random
            // battles' generator are drawn by it alone.
            if data.nonstandard.is_some() || crate::random_battle::ADDED_FORMES.contains(&s) {
                continue;
            }
            let l = leg.species(s);
            // Battle-only formes that an item brings about (Megas, Primals,
            // the Crowned heroes) are drawn as their base species holding it.
            let (species, required_ability) = match l.battle_only {
                BattleOnly::No => (s, None),
                BattleOnly::Species(base) if !data.required_items.is_empty() => (base, l.required_ability),
                _ => continue,
            };
            if !g.pools.species_ok(species) {
                continue;
            }
            let required_items: Vec<ItemId> =
                data.required_items.iter().copied().filter(|i| legal.contains(i)).collect();
            if !data.required_items.is_empty() && required_items.is_empty() {
                continue;
            }
            let required_move = l.required_move;
            if required_move.is_some_and(|m| !g.pools.move_ok(m)) {
                continue;
            }
            let abilities: Vec<AbilityId> = match required_ability {
                Some(a) => vec![a],
                None => {
                    let slots = dex.species(species).abilities;
                    let mut v = Vec::new();
                    for a in [Some(slots.primary), slots.secondary, slots.hidden, slots.special].into_iter().flatten() {
                        if !v.contains(&a) && a != AbilityId::NONE && dex.ability(a).nonstandard.is_none() {
                            v.push(a);
                        }
                    }
                    v
                }
            };
            let group = by_number.entry(data.num).or_default();
            if group.iter().any(|f| f.species == species && f.required_items == required_items) {
                continue;
            }
            // The abilities Showdown accepts with no moves wrong (Battle
            // Bond makes Greninja-Bond, ...); none: the forme can't be used.
            let abilities: Vec<AbilityId> = abilities
                .into_iter()
                .filter(|&a| {
                    let mut set = probe(g, species, a, required_items.first().copied());
                    if let Some(m) = required_move {
                        set.moves.push(m);
                    }
                    g.pools.ability_ok(a)
                        && g.validator
                            .set_problems(&set, &Default::default())
                            .iter()
                            .all(|p| matches!(p, ProblemKind::NoMoves { .. }))
                })
                .collect();
            if abilities.is_empty() {
                continue;
            }
            let forme = Forme {
                species,
                required_items,
                required_move,
                abilities,
                excluded_items: Vec::new(),
                moves: OnceLock::new(),
            };
            group.push(forme);
        }
        let mut formes = Vec::new();
        let mut numbers = Vec::new();
        for (_, mut group) in by_number {
            // The items that make another forme of the number.
            let taken: Vec<ItemId> = group.iter().flat_map(|f| f.required_items.iter().copied()).collect();
            for f in &mut group {
                let base = dex.species(f.species).base_species;
                f.excluded_items = items
                    .iter()
                    .copied()
                    .filter(|&i| {
                        (taken.contains(&i) && !f.required_items.contains(&i))
                            || leg
                                .item(i)
                                .forced_forme
                                .is_some_and(|ff| ff != f.species && dex.species(ff).base_species == base)
                    })
                    .collect();
            }
            if group.is_empty() {
                continue;
            }
            numbers.push((formes.len()..formes.len() + group.len()).collect());
            formes.extend(group);
        }
        UniformPool { formes, numbers, items }
    }

    pub fn numbers(&self) -> usize {
        self.numbers.len()
    }

    /// What each Pokédex number's sets can hold (for the team rules): a
    /// forme with no required item can go without one ([`draw`](Self::draw)).
    pub fn number_items(&self, g: &TeamGenerator) -> Vec<NumberItems> {
        let vf = g.validator.format();
        self.numbers
            .iter()
            .map(|formes| {
                let formes = || formes.iter().map(|&f| &self.formes[f]);
                NumberItems {
                    restricted: formes().all(|f| vf.is_restricted_species(f.species)),
                    free: formes().any(|f| f.required_items.is_empty()),
                    items: formes().flat_map(|f| f.required_items.iter().copied()).collect(),
                }
            })
            .collect()
    }

    /// Draws a Pokédex number and a set for it; `None` (counted in
    /// `redraws`) if a rule forces a redraw.
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
        let f = &self.formes[formes[rng.random(formes.len() as u32) as usize]];
        let item = if f.required_items.is_empty() {
            // Redrawn until allowed; no item when none is (every allowed
            // item taken, or `items` restricted to none).
            let free = |i: &ItemId| !f.excluded_items.contains(i) && !used_items.contains(i);
            if !self.items.iter().any(free) {
                None
            } else {
                loop {
                    let i = self.items[rng.random(self.items.len() as u32) as usize];
                    if f.excluded_items.contains(&i) {
                        redraws.add("Forme item");
                    } else if used_items.contains(&i) {
                        redraws.add("Item Clause");
                    } else {
                        break Some(i);
                    }
                }
            }
        } else {
            let i = f.required_items[rng.random(f.required_items.len() as u32) as usize];
            if used_items.contains(&i) {
                redraws.add("Item Clause");
                return None;
            }
            Some(i)
        };
        let ability = f.abilities[rng.random(f.abilities.len() as u32) as usize];
        let stat_points = g.validator.format().champions_mod;
        let mut set = probe(g, f.species, ability, item)
            .with_nature(Nature::ALL[rng.random(25) as usize])
            .with_investment(g.mark_level_50(spread::uniform(rng, stat_points)));
        set.tera_type = g.pools.tera_type(rng, f.species);
        // The moves one by one, each kept if the set stays legal.
        let mut candidates = f.moves.get_or_init(|| legal_moves(g, f)).clone();
        rng.shuffle(&mut candidates);
        if let Some(m) = f.required_move {
            set.moves.push(m);
        }
        for m in candidates {
            if set.moves.len() == 4 {
                break;
            }
            if set.moves.contains(&m) {
                continue;
            }
            set.moves.push(m);
            if !g.check(&set, redraws) {
                set.moves.pop();
            }
        }
        if set.moves.is_empty() {
            redraws.add("No moves");
            return None;
        }
        // IVs and gender, redrawn a few times if the set's events or its
        // legendary status rule them out, else 31s and no gender.
        let fallback = set.clone();
        for _ in 0..4 {
            if let Investment::Evs { evs, .. } = set.investment {
                set.investment = Investment::Evs { evs, ivs: spread::uniform_ivs(rng) };
            }
            set.gender = g.pools.gender(rng, f.species);
            if let Some(played) = g.accept(&set, redraws) {
                return Some((n, played));
            }
        }
        let mut set = fallback;
        set.gender = g.pools.fixed_gender(f.species);
        set.investment = spread::max_ivs(set.investment);
        Some((n, g.validator.check_set(&set, &Default::default()).1))
    }
}

/// The moves a forme can have on its own: its full learnset's (every
/// standard move if it can Sketch), each checked alone (after its required
/// move).
fn legal_moves(g: &TeamGenerator, f: &Forme) -> Vec<MoveId> {
    let dex = g.pools.dex;
    let vf = g.validator.format();
    let mut candidate = vec![false; dex.moves.len()];
    for (_, learnset) in g.validator.full_learnset(f.species) {
        if learnset.get(moves::SKETCH).is_some() {
            candidate.iter_mut().for_each(|c| *c = true);
        }
        for e in learnset.iter() {
            candidate[e.move_.index()] = true;
        }
    }
    let base = probe(g, f.species, f.abilities[0], f.required_items.first().copied());
    let mut out = Vec::new();
    for m in (0..dex.moves.len() as u16).map(MoveId) {
        if !candidate[m.index()]
            || Some(m) == f.required_move
            || dex.move_(m).nonstandard.is_some()
            || !g.pools.move_ok(m)
            || matches!(vf.check(Target::Move(vf.legality.move_(m).id)), Check::Banned(_))
        {
            continue;
        }
        let mut set = base.clone();
        if let Some(m) = f.required_move {
            set.moves.push(m);
        }
        set.moves.push(m);
        if g.validator.set_problems(&set, &Default::default()).is_empty() {
            out.push(m);
        }
    }
    out
}
