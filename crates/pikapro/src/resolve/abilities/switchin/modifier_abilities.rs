//! The abilities of `switchin.rs` that change stats or power from the
//! field: Protosynthesis, Quark Drive, the Ruin abilities, the auras and
//! Aura Break, Orichalcum Pulse, Hadron Engine, Slow Start, Truant, As One
//! and Costar.
//!
//! Showdown references: these abilities in `data/abilities.ts`, and
//! `pokemon.getBestStat`.

use pikapro_data::gen9::abilities as a;
use pikapro_data::{AbilityId, Event, EventPrefix, MoveCategory, StatBoosts, Type};

use super::{ability_state, ability_state_mut, mon, mon_mut};
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName, LineArg};
use crate::resolve::events::{self, Effect, EventCtx, Relay, Spec};
use crate::resolve::handlers::{Handler, Holder};
use crate::resolve::{abilities, actor, boosts, field, manipulation, queue};
use crate::state::{MoveResult, Position, Terrain, Volatiles, Weather};

/// Runs `ability`'s callback for `ev.event` / `ev.prefix` on its holder.
/// `None` for an ability or callback this file does not handle.
pub(super) fn run(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Option<Relay>> {
    use EventPrefix::{Any, Foe, On, Source};
    let relay = match (ability, ev.event, ev.prefix) {
        // Protosynthesis and Quark Drive.
        (a::PROTOSYNTHESIS, Event::Start | Event::WeatherChange, On) => {
            let sunny = abilities::effective_weather(battle) == Some(Weather::Sun);
            paradox_change(battle, holder, Volatiles::PROTOSYNTHESIS, sunny);
            None
        }
        (a::QUARK_DRIVE, Event::Start | Event::TerrainChange, On) => {
            let electric = battle.state.field.terrain == Some(Terrain::Electric);
            paradox_change(battle, holder, Volatiles::QUARK_DRIVE, electric);
            None
        }
        (a::PROTOSYNTHESIS | a::QUARK_DRIVE, Event::End, On) => {
            // `delete pokemon.volatiles[...]` (no `onEnd`), then a silent
            // `-end` whether or not it was there.
            let flag = if ability == a::PROTOSYNTHESIS { Volatiles::PROTOSYNTHESIS } else { Volatiles::QUARK_DRIVE };
            events::delete_volatile_flag(battle, holder, flag);
            let name = if ability == a::PROTOSYNTHESIS { "Protosynthesis" } else { "Quark Drive" };
            end_line(battle, holder, name, true);
            None
        }
        // The Ruin abilities (`onStart`: `-ability` unless a Mold Breaker
        // move is being used against the holder).
        (a::SWORD_OF_RUIN | a::BEADS_OF_RUIN | a::TABLETS_OF_RUIN | a::VESSEL_OF_RUIN, Event::Start, On)
        | (a::DARK_AURA | a::FAIRY_AURA, Event::Start, On) => {
            if !abilities::suppressing_ability(battle, holder) {
                abilities::show(battle, holder, ability);
            }
            None
        }
        (a::SWORD_OF_RUIN, Event::ModifyDef, Any) => ruin(battle, ev, holder, ability, 1, true),
        (a::BEADS_OF_RUIN, Event::ModifySpD, Any) => ruin(battle, ev, holder, ability, 3, true),
        (a::TABLETS_OF_RUIN, Event::ModifyAtk, Any) => ruin(battle, ev, holder, ability, 0, false),
        (a::VESSEL_OF_RUIN, Event::ModifySpA, Any) => ruin(battle, ev, holder, ability, 2, false),
        // Dark Aura and Fairy Aura (`onAnyBasePower`, priority 20).
        (a::DARK_AURA | a::FAIRY_AURA, Event::BasePower, Any) => {
            let ty = if ability == a::DARK_AURA { Type::Dark } else { Type::Fairy };
            let data = *ev.move_data()?;
            let (attacker, defender) = (ev.target_pos()?, ev.source);
            if Some(attacker) == defender || data.category == MoveCategory::Status || data.ty != ty {
                return Some(None);
            }
            let booster = battle.move_state.aura_booster;
            if !booster.is_some_and(|p| abilities::has_ability(battle, p, ability)) {
                battle.move_state.aura_booster = Some(holder);
            }
            if battle.move_state.aura_booster != Some(holder) {
                return Some(None);
            }
            ev.chain(if battle.move_state.has_aura_break { 3072 } else { 5448 });
            None
        }
        (a::AURA_BREAK, Event::Start, On) => {
            abilities::show(battle, holder, ability);
            None
        }
        // `onAnyTryPrimaryHit`.
        (a::AURA_BREAK, Event::TryPrimaryHit, Any) => {
            let category = ev.move_data()?.category;
            if ev.target_pos() != ev.source && category != MoveCategory::Status {
                battle.move_state.has_aura_break = true;
            }
            None
        }
        // Orichalcum Pulse and Hadron Engine.
        (a::ORICHALCUM_PULSE, Event::Start, On) => {
            let effect = Effect::Ability(ability);
            if field::set_weather_from(battle, Weather::Sun, Some(holder), effect) == Some(true) {
                let target = actor(battle, holder);
                battle.emit(BattleEvent::ActivateSource { actor: target, ability });
            } else if abilities::effective_weather(battle) == Some(Weather::Sun) {
                abilities::activate(battle, holder, ability);
            }
            None
        }
        // `onModifyAtk` (priority 5).
        (a::ORICHALCUM_PULSE, Event::ModifyAtk, On) => {
            if matches!(abilities::pokemon_weather(battle, holder), Some(Weather::Sun | Weather::HarshSun)) {
                ev.chain(5461);
            }
            None
        }
        (a::HADRON_ENGINE, Event::Start, On) => {
            if !field::set_terrain_from(battle, Terrain::Electric, holder, Effect::Ability(ability))
                && battle.state.field.terrain == Some(Terrain::Electric)
            {
                abilities::activate(battle, holder, ability);
            }
            None
        }
        // `onModifySpA` (priority 5).
        (a::HADRON_ENGINE, Event::ModifySpA, On) => {
            if battle.state.field.terrain == Some(Terrain::Electric) {
                ev.chain(5461);
            }
            None
        }
        // Slow Start.
        (a::SLOW_START, Event::Start, On) => {
            let target = actor(battle, holder);
            let effect = Some(EffectName::Ability(ability));
            let what = LineArg::Text("");
            battle.emit(BattleEvent::EffectStartEnd {
                actor: target,
                start: true,
                what,
                effect,
                cause: None,
                silent: false,
            });
            ability_state_mut(battle, holder).slow_start = 5;
            None
        }
        // `onResidual` (order 28, sub-order 2).
        (a::SLOW_START, Event::Residual, On) => {
            let m = mon(battle, holder);
            let counter = m.volatile_counters.ability_state.slow_start;
            if m.active_turns > 0 && counter > 0 {
                ability_state_mut(battle, holder).slow_start = counter - 1;
                if counter == 1 {
                    end_line(battle, holder, "Slow Start", false);
                }
            }
            None
        }
        // `onModifyAtk` (priority 5) and `onModifySpe`.
        (a::SLOW_START, Event::ModifyAtk | Event::ModifySpe, On) => {
            if ability_state(battle, holder).slow_start > 0 {
                ev.chain_modify(1, 2);
            }
            None
        }
        (a::SLOW_START, Event::End, On) => {
            if !mon(battle, holder).being_called_back {
                end_line(battle, holder, "Slow Start", true);
            }
            None
        }
        // Truant.
        (a::TRUANT, Event::Start, On) => {
            events::remove_volatile_flag(battle, holder, Volatiles::TRUANT);
            let m = mon(battle, holder);
            let moved = m.volatile_counters.move_this_turn != MoveResult::Undefined;
            if m.active_turns > 0 && (moved || !queue::will_move(battle, holder)) {
                events::add_volatile_flag(battle, holder, Volatiles::TRUANT, "truant", None, Effect::Ability(ability));
            }
            None
        }
        // `onBeforeMove` (priority 9).
        (a::TRUANT, Event::BeforeMove, On) => {
            if events::remove_volatile_flag(battle, holder, Volatiles::TRUANT) {
                let target = actor(battle, holder);
                let effect = EffectName::Ability(ability);
                battle.emit(BattleEvent::CantFrom { actor: target, effect, move_id: None, of: None });
                return Some(Some(Relay::False));
            }
            events::add_volatile_flag(battle, holder, Volatiles::TRUANT, "truant", None, Effect::Ability(ability));
            None
        }
        // As One: Unnerve, and Chilling Neigh or Grim Neigh.
        (a::AS_ONE_GLASTRIER | a::AS_ONE_SPECTRIER, Event::Start, On) => {
            if !ability_state(battle, holder).unnerved {
                // `-ability|<pokemon>|As One`, then `-ability|<pokemon>|Unnerve`.
                let target = actor(battle, holder);
                battle.emit(BattleEvent::AbilityNamed { actor: target, name: "As One" });
                abilities::show(battle, holder, a::UNNERVE);
                ability_state_mut(battle, holder).unnerved = true;
            }
            None
        }
        (a::AS_ONE_GLASTRIER | a::AS_ONE_SPECTRIER, Event::End, On) => {
            ability_state_mut(battle, holder).unnerved = false;
            None
        }
        (a::AS_ONE_GLASTRIER | a::AS_ONE_SPECTRIER, Event::TryEatItem, Foe) => {
            Some(Relay::from_bool(!ability_state(battle, holder).unnerved))
        }
        (a::AS_ONE_GLASTRIER | a::AS_ONE_SPECTRIER, Event::AfterFaint, Source) => {
            if ev.effect.is_move() {
                let length = ev.relay.num().unwrap_or(0).clamp(0, 6) as i8;
                let (stages, neigh) = if ability == a::AS_ONE_GLASTRIER {
                    ([length, 0, 0, 0, 0, 0, 0], a::CHILLING_NEIGH)
                } else {
                    ([0, 0, length, 0, 0, 0, 0], a::GRIM_NEIGH)
                };
                boosts::boost_from(
                    battle,
                    holder,
                    StatBoosts::new(stages),
                    Some(holder),
                    Effect::Ability(neigh),
                    false,
                    false,
                );
            }
            None
        }
        // Costar (`onStart`, switching in with priority -2).
        (a::COSTAR, Event::Start, On) => {
            costar(battle, holder);
            None
        }
        _ => return None,
    };
    Some(relay)
}

// ---------------------------------------------------------------------------
// Helpers.

/// `-end|<pokemon>|<name>`, `[silent]` with `silent`.
fn end_line(battle: &mut Battle, pos: Position, name: &'static str, silent: bool) {
    let target = actor(battle, pos);
    let what = LineArg::Text(name);
    battle.emit(BattleEvent::EffectStartEnd { actor: target, start: false, what, effect: None, cause: None, silent });
}

// ---------------------------------------------------------------------------
// Protosynthesis and Quark Drive.

/// Protosynthesis's `onWeatherChange` / Quark Drive's `onTerrainChange`:
/// the volatile starts in the sun (Electric Terrain), and ends without it
/// unless Booster Energy started it.
fn paradox_change(battle: &mut Battle, holder: Position, flag: u128, active: bool) {
    let m = mon(battle, holder);
    if active {
        add_paradox(battle, holder, flag, false);
    } else if !(m.volatiles.has(flag) && m.volatile_counters.paradox_booster) {
        remove_paradox(battle, holder, flag);
    }
}

/// `pokemon.addVolatile('protosynthesis' | 'quarkdrive')`: the condition's
/// `onStart` (`-activate`, `[fromitem]` for Booster Energy, then
/// `-start|<pokemon>|protosynthesis<stat>`). Returns whether it started.
fn add_paradox(battle: &mut Battle, holder: Position, flag: u128, from_booster: bool) -> bool {
    let (id, ability) = if flag == Volatiles::PROTOSYNTHESIS {
        ("protosynthesis", a::PROTOSYNTHESIS)
    } else {
        ("quarkdrive", a::QUARK_DRIVE)
    };
    let effect =
        if from_booster { Effect::Item(pikapro_data::gen9::items::BOOSTER_ENERGY) } else { Effect::Ability(ability) };
    if !events::add_volatile_flag(battle, holder, flag, id, None, effect) {
        return false;
    }
    let target = actor(battle, holder);
    let args = [from_booster.then_some(LineArg::Text("[fromitem]")), None];
    battle.emit(BattleEvent::ActivateEffect { actor: target, effect: EffectName::Ability(ability), args, of: None });
    // `getBestStat(false, true)`.
    let best = abilities::best_stat(battle, holder, false) as u8;
    let c = &mut mon_mut(battle, holder).volatile_counters;
    c.paradox_booster = from_booster;
    c.paradox_stat = best;
    const PROTO: [&str; 5] =
        ["protosynthesisatk", "protosynthesisdef", "protosynthesisspa", "protosynthesisspd", "protosynthesisspe"];
    const QUARK: [&str; 5] = ["quarkdriveatk", "quarkdrivedef", "quarkdrivespa", "quarkdrivespd", "quarkdrivespe"];
    let names = if flag == Volatiles::PROTOSYNTHESIS { PROTO } else { QUARK };
    let what = LineArg::Text(names[best as usize]);
    battle.emit(BattleEvent::EffectStartEnd {
        actor: target,
        start: true,
        what,
        effect: None,
        cause: None,
        silent: false,
    });
    true
}

/// `pokemon.removeVolatile(...)`: its `onEnd` (`-end|<pokemon>|Protosynthesis`).
/// A Pokémon knocked out whose faint is still queued (Perish Song at 0,
/// then Electric Terrain ending in the same residual) keeps it silently
/// (`removeVolatile`'s `if (!this.hp)`, in [`events::remove_volatile_flag`]).
fn remove_paradox(battle: &mut Battle, holder: Position, flag: u128) {
    if events::remove_volatile_flag(battle, holder, flag) {
        let name = if flag == Volatiles::PROTOSYNTHESIS { "Protosynthesis" } else { "Quark Drive" };
        end_line(battle, holder, name, false);
    }
}

/// Booster Energy's `addVolatile('protosynthesis' | 'quarkdrive')` (after
/// it used the item): the volatile of
/// the holder's paradox ability starts with `fromBooster`. Returns whether
/// it started.
pub(crate) fn booster_energy(battle: &mut Battle, pos: Position, quark_drive: bool) -> bool {
    let flag = if quark_drive { Volatiles::QUARK_DRIVE } else { Volatiles::PROTOSYNTHESIS };
    add_paradox(battle, pos, flag, true)
}

/// The paradox volatile's `onModify<Stat>` (Attack and Special Attack
/// priority 5, the defences 6): 5325/4096 for its best stat, 1.5x for
/// Speed, unless the holder's ability is suppressed.
fn paradox_modify(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    let stat = match ev.event {
        Event::ModifyAtk => 0,
        Event::ModifyDef => 1,
        Event::ModifySpA => 2,
        Event::ModifySpD => 3,
        _ => 4,
    };
    if mon(battle, pos).volatile_counters.paradox_stat != stat || abilities::ignoring_ability(battle, pos) {
        return None;
    }
    if stat == 4 { ev.chain_modify(3, 2) } else { ev.chain_modify(5325, 4096) }
}

/// The handlers of this file's volatiles.
pub(super) fn volatile_spec(flag: u128, event: Event, prefix: EventPrefix) -> Option<Spec> {
    if prefix != EventPrefix::On || !matches!(flag, Volatiles::PROTOSYNTHESIS | Volatiles::QUARK_DRIVE) {
        return None;
    }
    let spec = Spec::relay(paradox_modify);
    Some(match event {
        Event::ModifyAtk | Event::ModifySpA => spec.priority(5),
        Event::ModifyDef | Event::ModifySpD => spec.priority(6),
        Event::ModifySpe => spec,
        _ => return None,
    })
}

/// The events of [`volatile_spec`].
pub(super) const fn volatile_events(flag: u128) -> u128 {
    match flag {
        Volatiles::PROTOSYNTHESIS | Volatiles::QUARK_DRIVE => {
            const {
                pikapro_data::EventSet::of(&[
                    Event::ModifyAtk,
                    Event::ModifyDef,
                    Event::ModifySpA,
                    Event::ModifySpD,
                    Event::ModifySpe,
                ])
                .0
            }
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// The Ruin abilities.

/// A Ruin ability's `onAny<Modify stat>`: 0.75x the stat of every other
/// Pokémon for a move, once per move (`move.ruined<stat>` names the holder
/// that applies; Sword and Beads of Ruin replace one that lost its
/// ability). `index`: Attack 0, Defense 1, Special Attack 2, Special
/// Defense 3.
fn ruin(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
    index: usize,
    recheck: bool,
) -> Option<Relay> {
    ev.move_data()?;
    let stat_holder = ev.target_pos()?;
    if abilities::has_ability(battle, stat_holder, ability) {
        return None;
    }
    let ruined = battle.move_state.ruined[index];
    let keep = match ruined {
        Some(p) if recheck => abilities::has_ability(battle, p, ability),
        Some(_) => true,
        None => false,
    };
    if !keep {
        battle.move_state.ruined[index] = Some(holder);
    }
    if battle.move_state.ruined[index] != Some(holder) {
        return None;
    }
    ev.chain_modify(3, 4)
}

// ---------------------------------------------------------------------------
// Costar.

/// Costar's `onStart`: the holder copies its ally's stat stages, Dragon
/// Cheer and Focus Energy (`-copyboost|<holder>|<ally>|[from] ability:
/// Costar`).
fn costar(battle: &mut Battle, holder: Position) {
    let ally = (0..battle.format.active_per_side())
        .map(|slot| Position::new(holder.side, slot as u8))
        .find(|&p| p != holder && events::is_active(battle, p) && mon(battle, p).hp > 0);
    let Some(ally) = ally else { return };
    let ally_state = *mon(battle, ally);
    mon_mut(battle, holder).boosts = ally_state.boosts;
    // Showdown also copies Laser Focus and G-Max Chi Strike: both are Past
    // in Gen 9 and Champions, so no Pokémon can have them.
    manipulation::transform_copy(battle, holder, &ally_state);
    let (source, target) = (actor(battle, holder), actor(battle, ally));
    battle.emit(BattleEvent::CopyBoostFrom { source, target, cause: Cause::ability(a::COSTAR) });
}
