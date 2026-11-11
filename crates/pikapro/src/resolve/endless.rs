//! Showdown's Endless Battle Clause: the staleness a Pokémon picks up when
//! it restores PP or HP in a way that could go on for ever, and the check
//! at the end of each turn past turn 100 that ends such a battle
//! (`maybeTriggerEndlessBattleClause` in `sim/battle.js`).
//!
//! The staleness is kept in every format, as in Showdown
//! ([`EndlessState`]); only a format with the clause
//! ([`Format::endless_battle_clause`](crate::Format::endless_battle_clause))
//! reads it, and only there does a battle work out what the check reads
//! of each set ([`of_set`]). Nothing here draws a random number.
//!
//! Where Showdown writes it:
//!
//! * `pokemon.setItem`: `pendingStaleness`, see [`item_set`];
//! * `pokemon.eatItem`: `staleness` from `pendingStaleness`, see
//!   [`item_eaten`];
//! * `pokemon.takeItem`: `pendingStaleness` is dropped, see [`item_taken`];
//! * `pokemon.clearVolatile`: `volatileStaleness` is dropped
//!   (`PokemonState::clear_volatile_state`);
//! * `battle.skillSwap` and Entrainment: `volatileStaleness`, see
//!   [`ability_given`] and [`ability_swapped`];
//! * Bug Bite, Pluck and Fling with a Leppa Berry, Heal Pulse and Floral
//!   Healing on a foe: `staleness = 'external'`, see [`external`].

use pikapro_data::ItemId;
use pikapro_data::gen9::{abilities as a, items as i, moves as m};

use crate::Battle;
use crate::event::{BattleEvent, Message};
use crate::result::EndReason;
use crate::state::{EndlessState, Position, SideId, Staleness};
use crate::team::PokemonSet;

/// Showdown's `RESTORATIVE_BERRIES` (`sim/pokemon.js`): the berries that
/// restore PP or HP.
pub(crate) const fn restorative(item: ItemId) -> bool {
    matches!(
        item,
        i::LEPPA_BERRY
            | i::AGUAV_BERRY
            | i::ENIGMA_BERRY
            | i::FIGY_BERRY
            | i::IAPAPA_BERRY
            | i::MAGO_BERRY
            | i::SITRUS_BERRY
            | i::WIKI_BERRY
            | i::ORAN_BERRY
    )
}

/// What `maybeTriggerEndlessBattleClause` reads of a Pokémon's set
/// (`pokemon.set`): `RESTORATIVE_BERRIES.has(toID(set.item))`, and
/// `['harvest', 'pickup'].includes(toID(set.ability)) ||
/// set.moves.map(toID).includes('recycle')`.
pub(crate) fn of_set(set: &PokemonSet) -> EndlessState {
    let berry = set.item.is_some_and(restorative);
    let cycle = matches!(set.ability, a::HARVEST | a::PICKUP) || set.moves.contains(&m::RECYCLE);
    EndlessState::of_set(berry, cycle)
}

/// `pokemon.setItem(item, source, effect)`, once it is known to go ahead
/// (the Pokémon has HP and is active):
///
/// ```js
/// const inflicted = ['trick', 'switcheroo'].includes(effectid);
/// const external = inflicted && source && !source.isAlly(this);
/// this.pendingStaleness = external ? 'external' : 'internal';
/// ```
///
/// `source` is `setItem`'s argument, and Trick and Switcheroo call
/// `setItem(item)` without one (only Recycle passes a source, and its
/// effect is neither), so at the pinned Showdown `external` is never true:
/// every item a Pokémon is given, or loses through `setItem('')`, leaves
/// `pendingStaleness` at `'internal'`.
pub(crate) fn item_set(battle: &mut Battle, pos: Position) {
    if let Some(mon) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        mon.endless.set_pending_staleness(Staleness::Internal);
    }
}

/// `pokemon.takeItem`: `this.pendingStaleness = undefined`.
pub(crate) fn item_taken(battle: &mut Battle, pos: Position) {
    if let Some(mon) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        mon.endless.set_pending_staleness(Staleness::None);
    }
}

/// `pokemon.eatItem`, after the berry's `Eat` and `EatItem`:
///
/// ```js
/// if (RESTORATIVE_BERRIES.has(item.id)) {
///     switch (this.pendingStaleness) {
///     case 'internal':
///         if (this.staleness !== 'external') this.staleness = 'internal';
///         break;
///     case 'external':
///         this.staleness = 'external';
///         break;
///     }
///     this.pendingStaleness = undefined;
/// }
/// ```
pub(crate) fn item_eaten(battle: &mut Battle, pos: Position, item: ItemId) {
    if !restorative(item) {
        return;
    }
    let Some(mon) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) else { return };
    match mon.endless.pending_staleness() {
        Staleness::Internal => {
            if mon.endless.staleness() != Staleness::External {
                mon.endless.set_staleness(Staleness::Internal);
            }
        }
        Staleness::External => mon.endless.set_staleness(Staleness::External),
        Staleness::None => {}
    }
    mon.endless.set_pending_staleness(Staleness::None);
}

/// `target.staleness = 'external'`: a foe restored the Pokémon's PP or HP
/// (Bug Bite, Pluck or Fling with a Leppa Berry; Heal Pulse or Floral
/// Healing from a foe).
pub(crate) fn external(battle: &mut Battle, pos: Position) {
    if let Some(mon) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        mon.endless.set_staleness(Staleness::External);
    }
}

/// Entrainment's `if (!target.isAlly(source)) target.volatileStaleness =
/// 'external'`, after the ability was set.
pub(crate) fn ability_given(battle: &mut Battle, target: Position, source: Position) {
    if target.side != source.side
        && let Some(mon) = battle.state.side_mut(target.side).team.get_mut(target.slot as usize)
    {
        mon.endless.set_volatile_staleness(Staleness::External);
    }
}

/// `battle.skillSwap`, after the abilities were traded:
///
/// ```js
/// source.volatileStaleness = undefined;
/// if (!source.isAlly(target)) target.volatileStaleness = 'external';
/// ```
pub(crate) fn ability_swapped(battle: &mut Battle, source: Position, target: Position) {
    if let Some(mon) = battle.state.side_mut(source.side).team.get_mut(source.slot as usize) {
        mon.endless.set_volatile_staleness(Staleness::None);
    }
    ability_given(battle, target, source);
}

/// The rest of `maybeTriggerEndlessBattleClause(trappedBySide,
/// stalenessBySide)`, after the turn limit and its warnings, in a format
/// with the clause: past turn 100, if every side's active Pokémon are
/// stale, one of them by a foe's doing, and a side is trapped or has no
/// Pokémon left that isn't stale, the battle ends. A side whose team
/// started with the means to cycle a restorative berry loses; if both or
/// neither did, it is a tie. Returns whether the battle ended.
///
/// `endTurn` gathers the two lists while it walks the active Pokémon:
///
/// ```js
/// let sideTrapped = true;
/// let sideStaleness;
/// for (const pokemon of side.active) {
///     ...
///     if (pokemon.fainted) continue;
///     sideTrapped = sideTrapped && pokemon.trapped;
///     const staleness = pokemon.volatileStaleness || pokemon.staleness;
///     if (staleness) sideStaleness = sideStaleness === 'external' ? sideStaleness : staleness;
/// }
/// ```
///
/// Nothing between that walk and the check changes what they read, so
/// they are gathered here.
pub(crate) fn check(battle: &mut Battle) -> bool {
    // `if (this.turn <= 100) return;`
    if !battle.format.endless_battle_clause || battle.state.turn <= 100 {
        return false;
    }
    let active = battle.format.active_per_side();
    let mut trapped = [true; 2];
    let mut staleness = [Staleness::None; 2];
    for side in SideId::BOTH {
        for slot in 0..active {
            let pos = Position::new(side, slot as u8);
            let Some(mon) = battle.state.at(pos) else { continue };
            if mon.is_fainted() {
                continue;
            }
            let stale = mon.endless.stale();
            trapped[side.index()] &= super::is_trapped(battle, pos);
            if stale != Staleness::None && staleness[side.index()] != Staleness::External {
                staleness[side.index()] = stale;
            }
        }
    }
    // `if (!stalenessBySide.every(s => !!s) || !stalenessBySide.some(s =>
    // s === 'external')) return;`
    if staleness.contains(&Staleness::None) || !staleness.contains(&Staleness::External) {
        return false;
    }
    // Whether each side can switch to a Pokémon that isn't stale. (The
    // loop stops at the first trapped side, leaving the rest unset, which
    // `canSwitch.every(s => s)` reads the same way: not every side can.)
    let can_switch = |side: SideId| {
        !trapped[side.index()]
            && battle.state.side(side).team.iter().any(|p| !p.is_fainted() && p.endless.stale() == Staleness::None)
    };
    if SideId::BOTH.into_iter().all(can_switch) {
        return false;
    }
    // The sides whose team started with the means to cycle a restorative
    // berry. `berry` is assigned, not accumulated: it is the last Pokémon
    // looked at that decides, in the team's current order, and the walk
    // stops at the first Pokémon that leaves both set.
    //
    // ```js
    // for (const pokemon of side.pokemon) {
    //     berry = RESTORATIVE_BERRIES.has(toID(pokemon.set.item));
    //     if (['harvest', 'pickup'].includes(toID(pokemon.set.ability)) ||
    //         pokemon.set.moves.map(toID).includes('recycle')) {
    //         cycle = true;
    //     }
    //     if (berry && cycle) break;
    // }
    // if (berry && cycle) losers.push(side);
    // ```
    let loses = |side: SideId| {
        let (mut berry, mut cycle) = (false, false);
        for p in battle.state.side(side).team.iter() {
            berry = p.endless.set_berry();
            cycle |= p.endless.set_cycle();
            if berry && cycle {
                break;
            }
        }
        berry && cycle
    };
    let losers = SideId::BOTH.map(loses);
    // The turn that just ended: `endTurn` has counted the next one, which
    // doesn't start (as at the turn limit, the result counts the turns
    // played).
    let turns = battle.state.turn - 1;
    match losers {
        [true, true] => {
            battle.emit(BattleEvent::Message { message: Message::BerryCyclingBoth });
            super::end(battle, None, EndReason::EndlessBattle);
        }
        [false, false] => super::end(battle, None, EndReason::EndlessBattle),
        [p1, _] => {
            let loser = if p1 { SideId::P1 } else { SideId::P2 };
            battle.emit(BattleEvent::Message { message: Message::BerryCycling { loser } });
            super::end(battle, Some(loser.foe()), EndReason::EndlessBattle);
        }
    }
    if let Some(result) = &mut battle.result {
        result.turns = turns;
    }
    true
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};

    use crate::event::{BattleEvent, Message};
    use crate::{
        Battle, BattleOptions, Choice, EndReason, Format, PokemonSet, Request, Seed, SideId, SlotAction, Staleness,
        Team,
    };

    const MOVES: [pikapro_data::MoveId; 4] = [m::TACKLE, m::HEAL_PULSE, m::SPLASH, m::RECYCLE];
    const TACKLE: u8 = 0;
    const HEAL_PULSE: u8 = 1;
    const SPLASH: u8 = 2;

    fn blissey() -> PokemonSet {
        PokemonSet::new(s::BLISSEY).with_ability(a::NATURAL_CURE).with_moves(&MOVES)
    }

    fn battle(format: Format, p1: Vec<PokemonSet>, p2: Vec<PokemonSet>) -> Battle {
        let mut b = Battle::new(format, [Team::new(p1), Team::new(p2)], BattleOptions::new(Seed::fast(1))).unwrap();
        for side in SideId::BOTH {
            if let Request::TeamPreview { bring } = b.request(side) {
                let order: Vec<u8> = (0..bring).collect();
                b.submit(side, Choice::team(&order)).unwrap();
            }
        }
        b
    }

    fn act(index: u8) -> Choice {
        Choice::single(SlotAction::Move { index, target: None, gimmick: None })
    }

    fn turn(b: &mut Battle, p1: Choice, p2: Choice) {
        b.submit(SideId::P1, p1).unwrap();
        b.submit(SideId::P2, p2).unwrap();
    }

    fn stale(b: &Battle, side: SideId) -> Staleness {
        b.state.side(side).team[0].endless.stale()
    }

    /// Both hurt each other, then heal each other: both stale by a foe's
    /// doing.
    fn heal_each_other(b: &mut Battle) {
        turn(b, act(TACKLE), act(TACKLE));
        assert_eq!([stale(b, SideId::P1), stale(b, SideId::P2)], [Staleness::None; 2]);
        turn(b, act(HEAL_PULSE), act(HEAL_PULSE));
        assert_eq!([stale(b, SideId::P1), stale(b, SideId::P2)], [Staleness::External; 2]);
    }

    /// Plays the turn that ends turn 100 and returns the messages written.
    fn end_turn_100(b: &mut Battle) -> Vec<Message> {
        b.state.turn = 100;
        b.drain_events().for_each(drop);
        turn(b, act(SPLASH), act(SPLASH));
        b.events()
            .iter()
            .filter_map(|e| match e {
                BattleEvent::Message { message } => Some(*message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_side_that_could_cycle_a_berry_loses() {
        let cycler = || blissey().with_item(i::LEPPA_BERRY);
        for (p1, p2, loser) in [
            (cycler(), blissey(), SideId::P1),
            (blissey(), cycler(), SideId::P2),
            // Harvest and Pickup count as Recycle does; any restorative
            // berry() as a Leppa Berry.
            (blissey(), blissey().with_ability(a::HARVEST).with_item(i::ORAN_BERRY), SideId::P2),
        ] {
            let mut b = battle(Format::gen9_ou(), vec![p1], vec![p2]);
            heal_each_other(&mut b);
            // Nothing before turn 101.
            b.state.turn = 99;
            turn(&mut b, act(SPLASH), act(SPLASH));
            assert!(!b.is_over());
            let messages = end_turn_100(&mut b);
            assert_eq!(messages, [Message::BerryCycling { loser }]);
            let result = b.result().unwrap();
            // The result counts the turns played; the battle's own count
            // has moved to the turn that didn't start.
            assert_eq!(
                (result.winner, result.reason, result.turns),
                (Some(loser.foe()), EndReason::EndlessBattle, 100)
            );
            assert_eq!(b.turn(), 101);
            // No `|turn|` line for a turn that doesn't start.
            assert!(!b.events().iter().any(|e| matches!(e, BattleEvent::Turn { .. })));
            assert_eq!(b.events().last(), Some(&BattleEvent::Win { side: loser.foe() }));
            assert_eq!(b.request(SideId::P1), Request::Wait);
        }
    }

    #[test]
    fn both_or_neither_is_a_tie() {
        let cycler = || blissey().with_item(i::SITRUS_BERRY);
        for (p1, p2, messages) in
            [(cycler(), cycler(), vec![Message::BerryCyclingBoth]), (blissey(), blissey(), vec![])]
        {
            let mut b = battle(Format::gen9_ou(), vec![p1], vec![p2]);
            heal_each_other(&mut b);
            assert_eq!(end_turn_100(&mut b), messages);
            let result = b.result().unwrap();
            assert_eq!((result.winner, result.reason), (None, EndReason::EndlessBattle));
            assert_eq!(b.events().last(), Some(&BattleEvent::Tie));
        }
    }

    /// The berry() of the last Pokémon looked at decides (`berry()` is
    /// assigned for each Pokémon, and the walk stops once both are set).
    #[test]
    fn the_last_pokemon_s_berry_decides() {
        let berry = || blissey().with_moves(&[m::TACKLE, m::HEAL_PULSE, m::SPLASH]).with_item(i::LEPPA_BERRY);
        let recycle = || PokemonSet::new(s::SNORLAX).with_ability(a::IMMUNITY).with_moves(&MOVES);
        let plain = || PokemonSet::new(s::SNORLAX).with_ability(a::IMMUNITY).with_moves(&[m::SPLASH]);
        for (team, loses) in [
            // The berry(), then Recycle without one: `berry()` ends false.
            (vec![berry(), recycle()], false),
            (vec![recycle(), berry()], true),
            (vec![berry(), plain()], false),
            (vec![berry().with_ability(a::PICKUP), plain()], true),
        ] {
            let mut b = battle(Format::gen9_ou(), team, vec![blissey()]);
            // Whatever leads, it and the foe heal each other; the other
            // Pokémon faints, so that the side can't switch to it.
            heal_each_other(&mut b);
            b.state.side_mut(SideId::P1).team[1].hp = 0;
            let messages = end_turn_100(&mut b);
            let (winner, expected) = if loses {
                (Some(SideId::P2), vec![Message::BerryCycling { loser: SideId::P1 }])
            } else {
                (None, vec![])
            };
            assert_eq!(messages, expected);
            assert_eq!(b.result().map(|r| (r.winner, r.reason)), Some((winner, EndReason::EndlessBattle)));
        }
    }

    #[test]
    fn the_clause_needs_both_sides_stale_one_by_a_foe_and_no_way_out() {
        let cycler = || blissey().with_item(i::LEPPA_BERRY);
        // A format without the clause.
        let mut b = battle(Format::gen9_singles(), vec![cycler()], vec![blissey()]);
        heal_each_other(&mut b);
        assert_eq!(end_turn_100(&mut b), []);
        assert!(!b.is_over());

        // One side stale.
        let mut b = battle(Format::gen9_ou(), vec![cycler()], vec![blissey()]);
        turn(&mut b, act(TACKLE), act(TACKLE));
        turn(&mut b, act(SPLASH), act(HEAL_PULSE));
        assert_eq!([stale(&b, SideId::P1), stale(&b, SideId::P2)], [Staleness::External, Staleness::None]);
        assert_eq!(end_turn_100(&mut b), []);
        assert!(!b.is_over());

        // Both stale, neither by a foe.
        let mut b = battle(Format::gen9_ou(), vec![cycler()], vec![blissey()]);
        for side in SideId::BOTH {
            b.state.side_mut(side).team[0].endless.set_staleness(Staleness::Internal);
        }
        assert_eq!(end_turn_100(&mut b), []);
        assert!(!b.is_over());

        // Both sides can switch to a Pokémon that isn't stale...
        let bench = || PokemonSet::new(s::SNORLAX).with_ability(a::IMMUNITY).with_moves(&[m::SPLASH]);
        let mut b = battle(Format::gen9_ou(), vec![cycler(), bench()], vec![blissey(), bench()]);
        heal_each_other(&mut b);
        assert_eq!(end_turn_100(&mut b), []);
        assert!(!b.is_over());
        // ...until one is trapped.
        b.state.side_mut(SideId::P2).team[0].volatiles.add(crate::state::Volatiles::TRAPPED);
        b.state.turn = 100;
        turn(&mut b, act(SPLASH), act(SPLASH));
        assert_eq!(b.result().map(|r| (r.winner, r.reason)), Some((Some(SideId::P2), EndReason::EndlessBattle)));
    }

    /// `pendingStaleness`: a restorative berry() a Pokémon got back and
    /// then ate leaves it stale; the berry() it started with doesn't.
    #[test]
    fn a_berry_got_back_and_eaten_is_internal_staleness() {
        let cycler = || blissey().with_item(i::ORAN_BERRY);
        let mut b = battle(Format::gen9_ou(), vec![cycler()], vec![blissey()]);
        let mon = |b: &Battle| b.state.side(SideId::P1).team[0];
        // Hurt below half: the Oran Berry it started with is eaten.
        b.state.side_mut(SideId::P1).team[0].hp = 100;
        turn(&mut b, act(SPLASH), act(TACKLE));
        assert_eq!((mon(&b).item, mon(&b).endless.staleness()), (None, Staleness::None));
        // Recycle (above half HP, where the berry stays): pending.
        let max = mon(&b).max_hp();
        b.state.side_mut(SideId::P1).team[0].hp = max;
        turn(&mut b, act(3), act(SPLASH));
        assert_eq!(mon(&b).item, Some(i::ORAN_BERRY));
        assert_eq!(mon(&b).endless.pending_staleness(), Staleness::Internal);
        assert_eq!(mon(&b).endless.staleness(), Staleness::None);
        // Eaten again: stale.
        b.state.side_mut(SideId::P1).team[0].hp = 100;
        turn(&mut b, act(SPLASH), act(TACKLE));
        assert_eq!(mon(&b).item, None);
        assert_eq!(mon(&b).endless.pending_staleness(), Staleness::None);
        assert_eq!(mon(&b).endless.staleness(), Staleness::Internal);
        assert!(mon(&b).endless.set_berry() && mon(&b).endless.set_cycle());

        // A berry got back and then taken (`takeItem`: Knock Off) is no
        // longer pending.
        let knocker = PokemonSet::new(s::SNORLAX).with_ability(a::IMMUNITY).with_moves(&[m::KNOCK_OFF, m::SPLASH]);
        let mut b = battle(Format::gen9_ou(), vec![cycler()], vec![knocker]);
        turn(&mut b, act(SPLASH), act(0));
        assert_eq!((mon(&b).item, mon(&b).endless.pending_staleness()), (None, Staleness::None));
        // (Knock Off leaves no `lastItem`: nothing to Recycle. A berry
        // given back another way is pending again.)
        let p1 = &mut b.state.side_mut(SideId::P1).team[0];
        (p1.last_item, p1.hp) = (Some(i::ORAN_BERRY), max);
        turn(&mut b, act(3), act(1));
        assert_eq!((mon(&b).item, mon(&b).endless.pending_staleness()), (Some(i::ORAN_BERRY), Staleness::Internal));
        turn(&mut b, act(SPLASH), act(0));
        assert_eq!((mon(&b).item, mon(&b).endless.pending_staleness()), (None, Staleness::None));
        assert_eq!(mon(&b).endless.staleness(), Staleness::None);
    }

    /// Bug Bite and Fling with a Leppa Berry; Skill Swap's volatile
    /// staleness, which leaving the field ends.
    #[test]
    fn a_foe_s_leppa_berry_and_ability_are_external_staleness() {
        let biter = || PokemonSet::new(s::SNORLAX).with_ability(a::IMMUNITY).with_moves(&[m::BUG_BITE, m::SPLASH]);
        let holder = || blissey().with_item(i::LEPPA_BERRY);
        let mut b = battle(Format::gen9_ou(), vec![biter()], vec![holder()]);
        turn(&mut b, act(0), act(SPLASH));
        // The Pokémon that lost the berry().
        assert_eq!([stale(&b, SideId::P1), stale(&b, SideId::P2)], [Staleness::None, Staleness::External]);

        let flinger = || {
            PokemonSet::new(s::SNORLAX)
                .with_ability(a::IMMUNITY)
                .with_item(i::LEPPA_BERRY)
                .with_moves(&[m::FLING, m::SPLASH])
        };
        let mut b = battle(Format::gen9_ou(), vec![flinger()], vec![blissey()]);
        turn(&mut b, act(0), act(SPLASH));
        // The foe that ate it.
        assert_eq!([stale(&b, SideId::P1), stale(&b, SideId::P2)], [Staleness::None, Staleness::External]);

        let swapper = || PokemonSet::new(s::SNORLAX).with_ability(a::IMMUNITY).with_moves(&[m::SKILL_SWAP, m::SPLASH]);
        let bench = || PokemonSet::new(s::SNORLAX).with_ability(a::THICK_FAT).with_moves(&[m::SPLASH]);
        let mut b = battle(Format::gen9_ou(), vec![swapper()], vec![blissey(), bench()]);
        turn(&mut b, act(0), act(SPLASH));
        let target = b.state.side(SideId::P2).team[0];
        assert_eq!(target.endless.volatile_staleness(), Staleness::External);
        assert_eq!((target.endless.staleness(), target.endless.stale()), (Staleness::None, Staleness::External));
        assert_eq!(stale(&b, SideId::P1), Staleness::None);
        turn(&mut b, act(1), Choice::single(SlotAction::Switch(1)));
        assert!(b.state.side(SideId::P2).team.iter().all(|p| p.endless.stale() == Staleness::None));
    }
}
