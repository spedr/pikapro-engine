//! Mega Evolution: Showdown's `actions.canMegaEvo` (the Champions mod's,
//! `data/mods/champions/scripts.ts`) and `actions.runMegaEvo`
//! (`sim/battle-actions.ts`), and Mega Sol's part of
//! `pokemon.effectiveWeather()` (`sim/pokemon.ts`).
//!
//! - `canMegaEvo` is computed once, when the battle builds the Pokémon
//!   ([`can_mega_evo`], stored in `PokemonState::can_mega_evo`), and
//!   offered in the legal actions while the side has not Mega Evolved.
//!   Showdown also offers it for supplied Past stones in Gen 9 formats;
//!   their validators, rather than the simulator, exclude those stones.
//! - A move chosen with [`Gimmick::MegaEvolve`] queues a `megaEvo` action
//!   (order 104, `resolve/queue.rs`), which runs [`run_mega_evo`]: the
//!   permanent forme change (`resolve/forme.rs`: stats, types, `-mega`,
//!   `detailschange`, the Mega's ability), every Pokémon of the side loses
//!   `canMegaEvo`, then `AfterMega`. The Gen 8+ re-sort before the next
//!   move uses the new Speed.
//! - The Champions mod keeps the Mega forme after fainting (no
//!   `formeRegression`).

use pikapro_data::gen9::{abilities as a, moves as ids};
use pikapro_data::{Event, ItemId, MoveId, SpeciesId};

use super::events::Effect;
use super::{abilities, forme};
use crate::battle::Battle;
use crate::format::{Format, Gimmick};
use crate::state::{Position, Weather};

/// Showdown's `actions.canMegaEvo(pokemon)` when the battle builds the
/// Pokémon: the Mega forme its held Mega Stone makes of its species
/// (the Champions mod's `item.megaStone?.[species.name]`). Supplied stones
/// work in Gen 9 too (BattleActions.canMegaEvo has no format gate), with
/// a base-species fallback. Mega Rayquaza's Dragon Ascent needs Gen 7 or
/// a `+past` / `+future` rule, which no format here has.
pub(crate) fn can_mega_evo(format: &Format, species: SpeciesId, item: Option<ItemId>) -> Option<SpeciesId> {
    let stone = format.dex.item(item?);
    let direct = stone.mega.iter().find(|&&(base, _)| base == species).map(|&(_, mega)| mega);
    // In the pinned data every non-base direct mapping is a Gen 9 Mega
    // (Floette-Eternal, Magearna-Original, Meowstic-F, Tatsugiri's formes,
    // Zygarde-Complete). The base mapping gives the same result otherwise.
    if format.mechanics == crate::Mechanics::Champions || direct.is_some() {
        return direct;
    }
    let base = format.dex.species(species).base_species;
    stone.mega.iter().find(|&&(from, into)| from == base && into != species).map(|&(_, mega)| mega)
}

/// Showdown's `actions.runMegaEvo(pokemon)` (the `megaEvo` action): the
/// Pokémon becomes its Mega forme (`formeChange(speciesid,
/// pokemon.getItem(), true)`), no Pokémon of its side can Mega Evolve any
/// more (`ally.canMegaEvo = false`), then `runEvent('AfterMega')` (Eject
/// Pack, Mirror Herb, White Herb, Opportunist).
pub(crate) fn run_mega_evo(battle: &mut Battle, pos: Position) {
    let side = battle.state.side(pos.side);
    let Some(mon) = side.team.get(pos.slot as usize) else { return };
    if side.gimmicks_used.contains(Gimmick::MegaEvolve) {
        return;
    }
    // A Mega Stone can't be taken from the Pokémon it works for (its
    // `onTakeItem`), so the Pokémon still holds it.
    let (Some(mega), Some(item)) = (mon.can_mega_evo, mon.item) else { return };
    forme::forme_change(battle, pos, mega, Effect::Item(item), true, false);
    let side = battle.state.side_mut(pos.side);
    for ally in side.team.iter_mut() {
        ally.can_mega_evo = None;
    }
    side.gimmicks_used.insert(Gimmick::MegaEvolve);
    super::run_mon_event(battle, Event::AfterMega, pos);
}

/// Power Construct's `canMegaEvo` refresh after it turns Zygarde into its
/// Complete forme: `pokemon.canMegaEvo === false ? false :
/// actions.canMegaEvo(pokemon)`.
pub(crate) fn refresh_can_mega_evo(battle: &mut Battle, pos: Position) {
    let side = battle.state.side(pos.side);
    let Some(mon) = side.team.get(pos.slot as usize) else { return };
    let can = if side.gimmicks_used.contains(Gimmick::MegaEvolve) {
        None
    } else {
        can_mega_evo(&battle.format, mon.base_species, mon.item)
    };
    battle.state.side_mut(pos.side).team[pos.slot as usize].can_mega_evo = can;
}

// ---------------------------------------------------------------------------
// Mega Sol.

/// What asks for a Pokémon's weather: the `sourceEffect` of Showdown's
/// `pokemon.effectiveWeather(sourceEffect)`, by default `battle.effect`
/// (the effect whose handler or callback is running). Abilities' and
/// items' handlers use `abilities::pokemon_weather`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WeatherAsker {
    /// A move's own callback (Weather Ball, Solar Beam, Synthesis,
    /// Thunder, ...), or `getMoveTargets` for a charging move.
    Move(MoveId),
    /// A weather's handler (its damage modifier, Sunny Day's freeze
    /// immunity, the sandstorm's and snow's stat boosts).
    Weather,
    /// Mega Sol's own `onWeatherModifyDamage`.
    MegaSol,
}

/// Mega Sol (`pokemon.effectiveWeather()`): while the Pokémon using a move
/// (`battle.activePokemon`) has Mega Sol, a move's, a weather's or Mega
/// Sol's own question gets sun, whatever the weather (Electro Shot's is
/// not changed).
pub(crate) fn mega_sol(battle: &Battle, asker: WeatherAsker) -> bool {
    if asker == WeatherAsker::Move(ids::ELECTRO_SHOT) {
        return false;
    }
    battle.active_move.is_some_and(|(user, _)| abilities::has_ability(battle, user, a::MEGA_SOL))
}

/// Showdown's `pokemon.effectiveWeather(sourceEffect)` for the Pokémon at
/// `pos`, asked by `asker`: sun under Mega Sol ([`mega_sol`]), otherwise
/// the effective weather unless Utility Umbrella hides it.
pub(crate) fn pokemon_weather(battle: &Battle, pos: Position, asker: WeatherAsker) -> Option<Weather> {
    if mega_sol(battle, asker) { Some(Weather::Sun) } else { abilities::pokemon_weather(battle, pos) }
}

/// `pokemon.effectiveWeather(undefined, true)` from `move_id`'s callback
/// (Solar Beam's and Solar Blade's charge, Synthesis, Moonlight and
/// Morning Sun): as [`pokemon_weather`], and Mega Sol announces itself
/// (`-activate|<pokemon>|ability: Mega Sol`) unless the weather is sun
/// already.
pub(crate) fn pokemon_weather_shown(battle: &mut Battle, pos: Position, move_id: MoveId) -> Option<Weather> {
    if !mega_sol(battle, WeatherAsker::Move(move_id)) {
        return abilities::pokemon_weather(battle, pos);
    }
    if abilities::effective_weather(battle) != Some(Weather::Sun) {
        abilities::activate(battle, pos, a::MEGA_SOL);
    }
    Some(Weather::Sun)
}

/// Mega Sol's `onWeatherModifyDamage` (priority 1, before the weather's
/// own): Sunny Day's handler with Mega Sol as `battle.effect`, then
/// `return damage`, which ends the `priorityEvent`.
pub(crate) fn weather_modify_damage(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
) -> Option<super::events::Relay> {
    super::field::sun_modify_damage(battle, ev, WeatherAsker::MegaSol);
    Some(ev.relay)
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::{abilities as ab, items as i, moves as m, species as s};
    use pikapro_data::{AbilityId, Nature, Stat, Stats, Type};

    use super::*;
    use crate::choice::{Choice, SlotAction};
    use crate::event::BattleEvent;
    use crate::rng::Seed;
    use crate::state::SideId;
    use crate::team::{Investment, PokemonSet, Team};
    use crate::{BattleOptions, ChoiceError, Request};

    fn set(species: SpeciesId, ability: AbilityId, item: ItemId) -> PokemonSet {
        PokemonSet::new(species)
            .with_level(50)
            .with_ability(ability)
            .with_item(item)
            .with_nature(Nature::Hardy)
            .with_tera_type(Type::Normal)
            .with_investment(Investment::StatPoints(Stats::new(10, 10, 10, 10, 10, 10)))
            .with_moves(&[m::PROTECT, m::TACKLE])
    }

    /// Charizard (Charizardite Y) and Manectric (Manectite) lead, then
    /// Garchomp with a stone it can't use.
    fn team() -> Team {
        Team::new(vec![
            set(s::CHARIZARD, ab::BLAZE, i::CHARIZARDITE_Y),
            set(s::MANECTRIC, ab::STATIC, i::MANECTITE),
            set(s::GARCHOMP, ab::ROUGH_SKIN, i::CHARIZARDITE_X),
            set(s::VENUSAUR, ab::OVERGROW, i::LEFTOVERS),
            set(s::GYARADOS, ab::INTIMIDATE, i::SITRUS_BERRY),
            set(s::MEOWSTIC_F, ab::KEEN_EYE, i::MEOWSTICITE),
        ])
    }

    fn battle() -> Battle {
        let format = Format::champions_vgc();
        let mut b = Battle::new(format, [team(), team()], BattleOptions::new(Seed::fast(7))).unwrap();
        for side in SideId::BOTH {
            b.submit(side, Choice::team(&[0, 1, 2, 3])).unwrap();
        }
        b
    }

    fn protect(gimmick: Option<Gimmick>) -> SlotAction {
        SlotAction::Move { index: 0, target: None, gimmick }
    }

    /// Player 1's Charizard Mega Evolves; everyone protects.
    fn mega_turn(b: &mut Battle) {
        b.submit(SideId::P1, Choice::doubles(protect(Some(Gimmick::MegaEvolve)), protect(None))).unwrap();
        b.submit(SideId::P2, Choice::doubles(protect(None), protect(None))).unwrap();
    }

    #[test]
    fn can_mega_evo_follows_the_stone_and_mod() {
        let ch = Format::champions_vgc();
        assert_eq!(can_mega_evo(&ch, s::CHARIZARD, Some(i::CHARIZARDITE_Y)), Some(s::CHARIZARD_MEGA_Y));
        assert_eq!(can_mega_evo(&ch, s::MEOWSTIC_F, Some(i::MEOWSTICITE)), Some(s::MEOWSTIC_F_MEGA));
        assert_eq!(can_mega_evo(&ch, s::GARCHOMP, Some(i::CHARIZARDITE_X)), None);
        assert_eq!(can_mega_evo(&ch, s::CHARIZARD, None), None);
        for format in [Format::gen9_singles(), Format::gen9_doubles(), Format::gen9_vgc()] {
            assert_eq!(can_mega_evo(&format, s::CHARIZARD, Some(i::CHARIZARDITE_Y)), Some(s::CHARIZARD_MEGA_Y));
            assert_eq!(can_mega_evo(&format, s::CHARIZARD_MEGA_X, Some(i::CHARIZARDITE_Y)), Some(s::CHARIZARD_MEGA_Y));
            assert_eq!(can_mega_evo(&format, s::CHARIZARD_MEGA_Y, Some(i::CHARIZARDITE_Y)), None);
        }
        assert_eq!(can_mega_evo(&ch, s::CHARIZARD_MEGA_X, Some(i::CHARIZARDITE_Y)), None);
    }

    #[test]
    fn one_mega_evolution_per_side() {
        let mut b = battle();
        let legal = b.legal_actions(SideId::P1);
        for slot in 0..2 {
            assert!(legal.slot(slot).contains(&protect(Some(Gimmick::MegaEvolve))), "slot {slot}");
        }
        // Both leads can Mega Evolve, but not in the same turn.
        let both = Choice::doubles(protect(Some(Gimmick::MegaEvolve)), protect(Some(Gimmick::MegaEvolve)));
        assert_eq!(legal.check(&both), Err(ChoiceError::MultipleGimmicks));

        mega_turn(&mut b);
        let side = b.state.side(SideId::P1);
        let mon = &side.team[0];
        assert_eq!((mon.species, mon.base_species), (s::CHARIZARD_MEGA_Y, s::CHARIZARD_MEGA_Y));
        assert_eq!((mon.ability, mon.base_ability), (ab::DROUGHT, ab::DROUGHT));
        assert_eq!(b.state.field.weather, Some(Weather::Sun));
        assert!(side.gimmicks_used.contains(Gimmick::MegaEvolve));
        assert!(side.team.iter().all(|m| m.can_mega_evo.is_none()));
        assert!(b.events().iter().any(|e| matches!(
            e,
            BattleEvent::MegaEvolve { species: s::CHARIZARD, item: Some(i::CHARIZARDITE_Y), .. }
        )));
        // Manectric can't any more; the other side still can.
        assert_eq!(b.request(SideId::P1), Request::Move);
        let legal = b.legal_actions(SideId::P1);
        assert!(!legal.slot(1).iter().any(|a| matches!(a, SlotAction::Move { gimmick: Some(_), .. })));
        assert!(b.legal_actions(SideId::P2).slot(0).contains(&protect(Some(Gimmick::MegaEvolve))));
    }

    /// Side.chooseMove tracks Mega Evolution and Tera separately. Supplied
    /// Past stones expose both in a Gen 9 doubles simulator battle.
    #[test]
    fn supplied_gen9_stone_can_mega_alongside_tera() {
        let mon = |sp| PokemonSet::new(sp).with_ability(ab::NO_ABILITY).with_moves(&[m::SPLASH]);
        let teams = [
            Team::new(vec![
                mon(s::CHARIZARD).with_item(i::CHARIZARDITE_Y),
                mon(s::BLASTOISE).with_tera_type(Type::Water),
            ]),
            Team::new(vec![mon(s::SNORLAX), mon(s::PIKACHU)]),
        ];
        let mut b = Battle::new(Format::gen9_doubles(), teams, BattleOptions::new(Seed::fast(49_001_700))).unwrap();
        assert!(b.active_request(SideId::P1, 0).unwrap().can_mega_evo);
        let mixed = Choice::doubles(protect(Some(Gimmick::MegaEvolve)), protect(Some(Gimmick::Terastallize)));
        b.submit(SideId::P1, mixed).unwrap();
        b.submit(SideId::P2, Choice::doubles(protect(None), protect(None))).unwrap();
        let side = b.state.side(SideId::P1);
        assert_eq!(side.active(0).unwrap().species, s::CHARIZARD_MEGA_Y);
        assert!(side.active(1).unwrap().terastallized);
        assert!(side.gimmicks_used.contains(Gimmick::MegaEvolve));
        assert!(side.gimmicks_used.contains(Gimmick::Terastallize));
    }

    #[test]
    fn the_mega_forme_has_its_stats_and_stays_after_fainting() {
        let mut b = battle();
        mega_turn(&mut b);
        let pos = Position::new(SideId::P1, 0);
        let mega = *b.state.at(pos).unwrap();
        // `setSpecies`: Charizard-Mega-Y's stats (159 base SpA, 104 Atk).
        assert!(mega.stats[Stat::SpA] > mega.stats[Stat::Atk] + 40);
        // The Champions mod keeps the Mega forme when it faints.
        assert_eq!(mega.forme_regression, None);
        b.state.at_mut(pos).unwrap().hp = 0;
        abilities::switchin::faint_forme_regression(&mut b, pos, false);
        assert_eq!(b.state.side(SideId::P1).team[0].base_species, s::CHARIZARD_MEGA_Y);
    }

    #[test]
    fn a_mega_stone_stays_on_its_user() {
        let mut b = battle();
        let pos = Position::new(SideId::P1, 0);
        assert_eq!(crate::resolve::items::take_item(&mut b, pos, None), None);
        // Garchomp can't use Charizardite X: it can be taken.
        b.state.side_mut(SideId::P1).team.swap(0, 2);
        assert_eq!(crate::resolve::items::take_item(&mut b, pos, None), Some(i::CHARIZARDITE_X));
    }
}
