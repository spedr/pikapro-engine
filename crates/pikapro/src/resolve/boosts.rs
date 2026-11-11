//! Stat stages.
//!
//! Showdown references: `Battle.boost` in `sim/battle.ts` (`ChangeBoost`,
//! capping, `TryBoost`, `-boost` / `-unboost` by effect type,
//! `AfterEachBoost`, `AfterBoost`), `Pokemon.getCappedBoost` / `boostBy` in
//! `sim/pokemon.ts`, `boostTable` in `Pokemon.getStat`.
//!
//! Contract:
//! - [`boost`]: stat-stage changes from a move or condition. Used by
//!   `runMoveEffects` (a move's `boosts`), self drops and secondaries
//!   (`secondaries.rs`). Returns whether any stage changed.
//! - [`boost_from`]: the same with its effect named (an ability's or item's
//!   boost: `-ability|...|boost`, `[from] item: X`).

use pikapro_data::{BoostId, Event, MoveId, StatBoosts};

use super::events::{self, Boosts, Effect, EventCtx, Relay, Target};
use super::power;
use crate::battle::Battle;
use crate::event::{AbilityNote, BattleEvent, Cause};
use crate::state::{Position, SideId};

/// Showdown's `Battle.boost` for a move or a condition: applies `boosts`
/// to `target` in the boosts' key order, one message per stat.
///
/// A stat already at ±6 gets a `-boost|...|0` / `-unboost|...|0` line,
/// except for secondary effects and self effects (`is_secondary` /
/// `is_self`), where it is silent. Returns whether any stage changed
/// (Showdown's `success`; its `0`/`false`/`null` failures are all `false`).
pub(crate) fn boost(
    battle: &mut Battle,
    target: Position,
    boosts: StatBoosts,
    source: Option<Position>,
    is_secondary: bool,
    is_self: bool,
) -> bool {
    let effect = super::move_or_condition_effect(battle);
    boost_from(battle, target, boosts, source, effect, is_secondary, is_self)
}

/// BattleActions.hitStepStealBoosts: announce, boost the user (with its
/// ChangeBoost/TryBoost handlers), then zero just the target's positive
/// stages. It precedes damage and TryPrimaryHit, including Substitute.
pub(crate) fn steal_positive_boosts(battle: &mut Battle, user: Position, target: Position, move_id: MoveId) {
    let Some(mon) = battle.state.at(target) else { return };
    let mut stolen = StatBoosts::default();
    for id in BoostId::ALL {
        stolen.stages[id as usize] = mon.boosts.get(id).max(0);
    }
    if stolen.stages.iter().all(|&n| n == 0) {
        return;
    }
    battle.attr_last_move(crate::event::MoveAttr::Still);
    let (source, target_actor) = (super::actor(battle, user), super::actor(battle, target));
    battle.emit(BattleEvent::ClearPositiveBoost { target: target_actor, source, move_id });
    // Showdown calls boost without an effect here: it inherits the
    // current event's effect (the truthy empty effect object normally,
    // or the move calling useMove). Both announce ordinary boost lines.
    let effect = battle.order.source_effect.filter(|&m| super::order::calls_moves(battle.format.dex, m));
    let effect = effect.map_or(Effect::Condition(events::Condition::Other), Effect::Move);
    boost_from(battle, user, stolen, Some(user), effect, false, false);
    if let Some(mon) = battle.state.at_mut(target) {
        for (id, &n) in BoostId::ALL.iter().zip(&stolen.stages) {
            if n > 0 {
                *stage_mut(&mut mon.boosts, *id) = 0;
            }
        }
    }
    battle.emit(BattleEvent::Anim { user: source, move_id, target: target_actor });
}

/// Showdown's `Battle.boost(boost, target, source, effect, isSecondary,
/// isSelf)`: `ChangeBoost`, capping, `TryBoost`, then each stat in the
/// object's key order (its message depends on `effect`: an item's says
/// `[from] item: X`, an ability's is preceded once by `-ability|<target>|<ability>|boost`
/// unless `is_secondary`), `AfterEachBoost` for each change, then
/// `AfterBoost`. Returns whether any stage changed.
pub(crate) fn boost_from(
    battle: &mut Battle,
    target: Position,
    boosts: StatBoosts,
    source: Option<Position>,
    effect: Effect,
    is_secondary: bool,
    is_self: bool,
) -> bool {
    // `!target?.hp`, `!target.isActive`.
    let Some(mon) = battle.state.at(target) else { return false };
    if mon.is_fainted() {
        return false;
    }
    // Gen 6+: nothing happens once the target's foes are all gone.
    if foe_pokemon_left(battle, target.side) == 0 {
        return false;
    }
    let mut relay = Boosts::new(boosts);
    relay = boost_event(battle, Event::ChangeBoost, target, source, effect, relay);
    relay = capped(battle, target, relay);
    relay = boost_event(battle, Event::TryBoost, target, source, effect, relay);
    let mut success = false;
    let mut boosted = is_secondary;
    for (id, requested) in relay.iter() {
        let Some(mon) = battle.state.at_mut(target) else { break };
        let stage = stage_mut(&mut mon.boosts, id);
        // `boostBy`.
        let delta = (*stage + requested).clamp(-6, 6) - *stage;
        *stage += delta;
        let unboost = requested < 0 || *stage == -6;
        let amount = delta.unsigned_abs();
        // (Made only for the log.)
        let target_actor = super::log_actor(battle, target);
        let plain = |battle: &mut Battle| {
            battle.emit_for(target_actor, |target| {
                if unboost {
                    BattleEvent::Unboost { target, stat: id, amount }
                } else {
                    BattleEvent::Boost { target, stat: id, amount }
                }
            });
        };
        if delta != 0 {
            success = true;
            match effect {
                Effect::None => {}
                // Belly Drum's `boost({atk: 12})`: `-setboost`.
                Effect::Move(pikapro_data::gen9::moves::BELLY_DRUM) => {
                    let stage = battle.state.at(target).map_or(0, |m| m.boosts.atk);
                    let move_id = pikapro_data::gen9::moves::BELLY_DRUM;
                    battle.emit_for(target_actor, |target| BattleEvent::SetBoost {
                        target,
                        stat: BoostId::Atk,
                        stage,
                        move_id,
                    });
                }
                Effect::Item(item) => battle.emit_for(target_actor, |target| BattleEvent::BoostFrom {
                    target,
                    stat: id,
                    amount,
                    unboost,
                    cause: Cause::item(item),
                }),
                // Anger Point sets Attack to +6 (`-setboost`).
                Effect::Ability(pikapro_data::gen9::abilities::ANGER_POINT) => {
                    let stage = battle.state.at(target).map_or(0, |m| m.boosts.atk);
                    let cause = Cause::ability(pikapro_data::gen9::abilities::ANGER_POINT);
                    battle.emit_for(target_actor, |target| BattleEvent::SetBoostFrom {
                        target,
                        stat: BoostId::Atk,
                        stage,
                        cause,
                    });
                }
                Effect::Ability(ability) => {
                    if !boosted {
                        let note = AbilityNote::Boost;
                        battle.emit_for(target_actor, |actor| BattleEvent::AbilityShow {
                            actor,
                            ability,
                            note,
                            cause: None,
                        });
                        boosted = true;
                    }
                    plain(battle);
                }
                _ => plain(battle),
            }
            // (The one-stat object only when a handler may read it.)
            if !events::filtered(battle, Event::AfterEachBoost)
                && !events::quiet(battle, Event::AfterEachBoost, target, source)
            {
                let mut one = Boosts { changes: StatBoosts::default(), keys: 0 };
                one.set(id, requested);
                boost_event(battle, Event::AfterEachBoost, target, source, effect, one);
            }
        } else if effect.is_ability() {
            if is_secondary || is_self {
                plain(battle);
            }
        } else if !is_secondary && !is_self {
            plain(battle);
        }
    }
    boost_event(battle, Event::AfterBoost, target, source, effect, relay);
    if success {
        let raised = relay.iter().any(|(_, v)| v > 0);
        let lowered = relay.iter().any(|(_, v)| v < 0);
        if let Some(m) = battle.state.at_mut(target)
            && raised
        {
            m.volatile_counters.stats_raised_this_turn = true;
        }
        power::on_boost(battle, target, lowered);
    }
    success
}

/// `runEvent(event, target, source, effect, boost)` with a boosts object;
/// returns the (possibly changed) object. Inlined: the event folds in
/// [`events::run_event`]'s test.
#[inline(always)]
fn boost_event(
    battle: &mut Battle,
    event: Event,
    target: Position,
    source: Option<Position>,
    effect: Effect,
    boosts: Boosts,
) -> Boosts {
    let mut ev = EventCtx::new(event, Target::Pokemon(target), source, effect, Relay::Boosts(boosts));
    match events::run_event(battle, &mut ev, None) {
        Relay::Boosts(b) => b,
        // A handler stopped the event: nothing is left to apply.
        _ => Boosts { changes: StatBoosts::default(), keys: 0 },
    }
}

/// Showdown's `getCappedBoost`: each named change limited so the stage
/// stays in `-6..=6` (still named if that makes it 0); changes of 0 are
/// dropped.
fn capped(battle: &Battle, target: Position, boosts: Boosts) -> Boosts {
    let Some(mon) = battle.state.at(target) else { return boosts };
    let mut out = Boosts { changes: StatBoosts { stages: [0; 7], order: boosts.changes.order }, keys: 0 };
    for (id, delta) in boosts.iter() {
        if delta == 0 {
            continue;
        }
        let stage = mon.boosts.get(id);
        out.changes.stages[id as usize] = (stage + delta).clamp(-6, 6) - stage;
        out.keys |= 1 << id as u8;
    }
    out
}

/// Showdown's `side.foePokemonLeft()`: `pokemonLeft` only drops when a faint
/// is reported (`faintMessages`), so a Pokémon knocked out by the current
/// move still counts while its faint is queued (in `SideState::active`
/// and the faint queue; the one being reported has left the queue).
fn foe_pokemon_left(battle: &Battle, side: SideId) -> usize {
    let foe = battle.state.side(side.foe());
    let queued = |i: usize| {
        foe.is_active(i as u8) && battle.faint_queue.iter().any(|f| f.pos == Position::new(side.foe(), i as u8))
    };
    (0..foe.team.len()).filter(|&i| !foe.team[i].is_fainted() || queued(i)).count()
}

/// The stage of `id` in `boosts`.
pub(crate) fn stage_mut(boosts: &mut crate::state::Boosts, id: BoostId) -> &mut i8 {
    match id {
        BoostId::Atk => &mut boosts.atk,
        BoostId::Def => &mut boosts.def,
        BoostId::SpA => &mut boosts.spa,
        BoostId::SpD => &mut boosts.spd,
        BoostId::Spe => &mut boosts.spe,
        BoostId::Accuracy => &mut boosts.accuracy,
        BoostId::Evasion => &mut boosts.evasion,
    }
}

/// A boost of `n` stages to `id` alone.
pub(crate) fn one_boost(id: BoostId, n: i8) -> StatBoosts {
    let mut stages = [0; 7];
    stages[id as usize] = n;
    StatBoosts::new(stages)
}
