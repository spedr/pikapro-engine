//! Forme changes: Showdown's `pokemon.setSpecies` and `formeChange`
//! (`sim/pokemon.ts`), with the stats recalculated from the set.
//!
//! Used by Shaymin-Sky thawing into Land Forme, Relic Song, and the
//! forme-changing abilities (Zen Mode, Stance Change, Disguise, Schooling,
//! ...). Mega Evolution (`resolve/mega.rs`) is
//! a permanent forme change caused by an item.

use pikapro_data::{Dex, SpeciesId, Stat};

use super::abilities::{self, SetAbilityKind};
use super::events::{self, Effect};
use super::{actor, manipulation};
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause};
use crate::state::{PokemonState, Position, Volatiles};
use crate::stats::calc_stat;

/// Showdown's `pokemon.setSpecies(species)` for a Pokémon on the field:
/// its species, types (`setType(species.types, true)`) and stats other
/// than HP (`spreadModify(species.baseStats, set)`) become the new
/// species', and the stored speed is reset to the raw Speed stat.
pub(crate) fn set_species(battle: &mut Battle, pos: Position, species: SpeciesId) {
    // `runEvent('ModifySpecies')`: no ability or item handles it.
    let dex = battle.format.dex;
    battle.state.side_mut(pos.side).team[pos.slot as usize].change_species(dex, species);
}

impl PokemonState {
    /// Showdown's `pokemon.setSpecies(species)` (`sim/pokemon.ts`), as a
    /// forme change that isn't permanent leaves the Pokémon (`formeChange(
    /// species, source, false)`: Stance Change, Hunger Switch, Forecast,
    /// ...): its species, its types (`setType(species.types, true)`, which
    /// also drops an added type and the apparent types) and its stats other
    /// than HP (`spreadModify(species.baseStats, set)`) become the new
    /// species', and the stored speed is the raw Speed stat. HP, ability
    /// and the base species stay. The engine's own forme changes run it;
    /// it is public for building positions by hand (a Pokémon in a
    /// temporary forme).
    pub fn change_species(&mut self, dex: &Dex, species: SpeciesId) {
        let data = dex.species(species);
        self.species = species;
        // `baseStoredStats` follow every `setSpecies` but Transform's: a
        // forme change while transformed (Mega Evolution) sets them
        // (requests). `this.species = species` too, which Transform had set
        // to its target's: a Pokémon that Mega Evolves after transforming
        // weighs what the Mega forme weighs (`weighthg`), and species
        // checks see it.
        if self.volatiles.has(Volatiles::TRANSFORMED)
            && let Some(base) = &mut self.volatile_counters.transform
        {
            base.own_species = species;
            base.species = species;
        }
        let c = &mut self.volatile_counters;
        c.types = Some(data.types);
        c.added_type = None;
        c.apparent_types = None;
        c.type_unknown = false;
        manipulation::refresh_types(self);
        for stat in [Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe] {
            self.stats[stat] =
                calc_stat(stat, data.base_stats[stat], self.ivs[stat], self.evs[stat], self.level, self.nature);
        }
        self.speed = self.stats[Stat::Spe] as i32;
    }
}

/// Showdown's `pokemon.formeChange(species, source, isPermanent,
/// abilitySlot, message)`. A permanent change (`permanent`) also changes
/// the base species (it survives switching out: `detailschange`), its max
/// HP and, unless Disguise or Ice Face caused it, its ability (the new
/// forme's first ability). Otherwise it is announced with `-formechange`,
/// `[from] ability: X` when an ability caused it. `msg` adds `[msg]`.
pub(crate) fn forme_change(
    battle: &mut Battle,
    pos: Position,
    species: SpeciesId,
    source: Effect,
    permanent: bool,
    msg: bool,
) {
    use pikapro_data::gen9::abilities as a;
    set_species(battle, pos, species);
    let target = actor(battle, pos);
    // Pokemon.formeChange (also the Champions override) keeps Illusion's
    // cached details and species for its messages. A permanent change
    // updates our own details, but does not reveal them through a disguise.
    let side = battle.state.side(pos.side);
    let mon = &side.team[pos.slot as usize];
    let disguise = mon.illusion.and_then(|id| side.team.iter().find(|m| m.id == id));
    let apparent_species = disguise.map_or(battle.format.dex.species(species).base_species, super::tera::species_of);
    let details = disguise.map(|m| (m.base_species, m.level, m.gender));
    if permanent {
        let mon = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
        mon.base_species = species;
        mon.base_types = battle.format.dex.species(species).types;
        // `setSpecies` (permanent): the shown types are the new forme's.
        mon.species_types = mon.base_types;
        for stat in [Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe] {
            mon.set_stats[stat] = mon.stats[stat];
        }
        let (level, tera, gender) = (mon.level, mon.terastallized.then_some(mon.tera_type), mon.gender);
        let (shown, level, gender) = details.unwrap_or((species, level, gender));
        battle.emit(BattleEvent::DetailsChange { actor: target, species: shown, level, tera, silent: false, gender });
        update_max_hp(battle, pos);
        match source {
            // An item's forme change is Mega Evolution (`runMegaEvo`).
            // `-mega|<pokemon>|<its base species>|<the forme's
            // requiredItem>`, and it counts as an action for Truant
            // (`moveThisTurnResult = true`). The Champions mod's
            // `formeChange` does not set `formeRegression` ("Don't revert
            // Mega Evolutions after fainting"); Showdown's own does.
            // Z-Crystals (Ultra Burst's `-burst`) and Primal Orbs
            // (`-primal`) are not in Gen 9 or Champions.
            Effect::Item(_) => {
                let data = battle.format.dex.species(species);
                let required = data.required_items.first().copied();
                battle.emit(BattleEvent::MegaEvolve { actor: target, species: apparent_species, item: required });
                super::power::set_move_result(battle, pos, crate::state::MoveResult::Succeeded);
                if battle.format.mechanics != crate::format::Mechanics::Champions {
                    let mon = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
                    mon.forme_regression = Some(mon.set_species);
                }
            }
            // No source (`formeChange(species, null, true)`: Ogerpon's and
            // Terapagos's Tera formes): `formeRegression`, it reverts on
            // fainting.
            Effect::None => {
                let mon = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
                mon.forme_regression = Some(mon.set_species);
            }
            Effect::Status(_) => {
                battle.emit(BattleEvent::FormeChange { actor: target, species, msg, cause: None });
            }
            _ => {}
        }
    } else {
        let cause = match source {
            Effect::Ability(id) => Some(Cause::ability(id)),
            _ => None,
        };
        let shown = if cause.is_none() && details.is_some() { apparent_species } else { species };
        battle.emit(BattleEvent::FormeChange { actor: target, species: shown, msg, cause });
    }
    if permanent && !matches!(source, Effect::Ability(a::DISGUISE | a::ICE_FACE)) {
        // Pokemon.formeChange clears the raw ability before setAbility
        // when a source changes a disguised Pokemon permanently. This
        // skips the old ability's End (Illusion would reveal it).
        if details.is_some() && source != Effect::None {
            battle.state.side_mut(pos.side).team[pos.slot as usize].ability = a::NO_ABILITY;
        }
        let dex = battle.format.dex;
        let ability = dex.species(species).abilities.primary;
        // `if (source || !this.getAbility().flags['cantsuppress'])`: with
        // no source, a `cantsuppress` ability stays.
        let current = battle.state.side(pos.side).team[pos.slot as usize].ability;
        if source != Effect::None || !dex.ability(current).flags.contains(pikapro_data::AbilityFlags::CANT_SUPPRESS) {
            abilities::set_ability(battle, pos, ability, None, None, SetAbilityKind::FormeChange);
        }
        // `baseAbility = toID(ability)`: the forme's, even if its `Start`
        // already replaced it (a Mega's Trace).
        battle.state.side_mut(pos.side).team[pos.slot as usize].base_ability = ability;
        // `set_ability` noted it unless a `cantsuppress` ability stayed:
        // leaving the field restores `base_ability`, so it must be in
        // `Battle::event_filter` either way.
        events::note_held(battle, Some(ability), None);
    }
    let mon = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
    if mon.terastallized {
        mon.volatile_counters.type_unknown = false;
        // `apparentType = terastallized` (Transform copies it).
        mon.volatile_counters.tera_apparent = true;
    }
}

/// Showdown's `pokemon.updateMaxHp()` after a permanent forme change: the
/// new forme's max HP, keeping the HP lost (at least 1 left), shown with a
/// silent `-heal`.
fn update_max_hp(battle: &mut Battle, pos: Position) {
    let data = battle.format.dex.species(battle.state.side(pos.side).team[pos.slot as usize].species);
    let mon = &mut battle.state.side_mut(pos.side).team[pos.slot as usize];
    let new_max = match data.max_hp {
        Some(hp) => hp,
        None => {
            calc_stat(Stat::Hp, data.base_stats[Stat::Hp], mon.ivs[Stat::Hp], mon.evs[Stat::Hp], mon.level, mon.nature)
        }
    };
    let old_max = mon.max_hp();
    if new_max == old_max {
        return;
    }
    mon.hp = if mon.hp == 0 { 0 } else { new_max.saturating_sub(old_max - mon.hp).max(1) };
    mon.stats[Stat::Hp] = new_max;
    mon.set_stats[Stat::Hp] = new_max;
    let (hp, max_hp, status) = (mon.hp, mon.max_hp(), mon.status);
    if hp > 0 {
        let target = actor(battle, pos);
        battle.emit(BattleEvent::HealSilent { target, hp, max_hp, status });
    }
}
