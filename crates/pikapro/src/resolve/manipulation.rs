//! Stat, HP and type manipulation.
//!
//! Showdown references: the moves' callbacks in `data/moves.ts` (Belly
//! Drum, Pain Split, Soak, Heal Bell, ...) and their `condition`s (Aqua
//! Ring, Ingrain, Charge, Stockpile, Roost, ...); `Pokemon.setType`,
//! `addType`, `getTypes`, `hasType`, `setBoost`, `cureStatus`, `sethp`,
//! `deductPP` in `sim/pokemon.ts`; `Battle.heal` / `directDamage` /
//! `boost` in `sim/battle.ts`.
//!
//! Contract (hooks in the shared pipeline, each at Showdown's call site):
//! - [`handles`]: status moves implemented here.
//! - [`on_try`], [`try_hit`], [`on_hit`]: the moves' `onTry`, `onTryHit`
//!   and `onHit` (`singleEvent('Try' | 'TryHit' | 'Hit')`), returning an
//!   [`Outcome`] like Showdown's callbacks; [`skips_data_boosts`] is the
//!   `delete move.boosts` of Clangorous Soul and Fillet Away.
//! - [`self_destruct_if_hit`]: `selfdestruct: 'ifHit'` (Memento).
//! - [`secondary_on_hit`]: the `onHit` of secondaries (Tri Attack, ...).
//! - [`modify_type`], [`move_aborted`], [`record_hit_targets`],
//!   [`after_move`]: `ModifyType` (Electrify), `MoveAborted` and
//!   `AfterMove` (Charge, Spit Up, Sparkling Aria).
//! - [`add_volatile`]: the volatiles of this module, dispatched from
//!   `volatiles::add_volatile`; [`residual_handlers`] their end of turn.
//! - Damage hooks: [`base_power`] (Spit Up), [`charge_base_power`]
//!   (Charge), [`crit_ratio_handler`] (Focus Energy, Dragon Cheer),
//!   [`tar_shot_effectiveness`] (Tar Shot), [`minimize_source_damage`] and
//!   [`minimize_accuracy`] (Minimize), [`lock_on_accuracy`] (Lock-On).
//! - Types: [`has_type`], [`pre_tera_has_type`]; `PokemonState::types` is
//!   Showdown's `getTypes(true)` (Roost's filter included), kept in sync by
//!   [`refresh_types`]; the underlying `pokemon.types` and `addedType` live
//!   in `VolatileCounters`.
//! - Ingrain: [`ingrain_drag_out`], [`grounded`].
//! - [`end_turn`]: `statsRaisedThisTurn` resets at the start of a turn.

use pikapro_data::gen9::moves as ids;
use pikapro_data::gen9::species;
use pikapro_data::{BoostId, Effectiveness, MoveData, MoveFlags, MoveId, Stat, StatBoosts, StatusId, Type, TypeSet};

use super::boosts::{one_boost, stage_mut};
use super::events::{EventCtx, Relay};
use super::handlers::{Callback, Handler, HandlerFn, Handlers, Holder, sub_order, vol};
use super::moves::{Outcome, fail_still};
use super::queue::ActionKind;
use super::{HealSource, actor, apply_heal, boosts, direct_damage, field, status, volatiles};
use crate::battle::Battle;
use crate::event::{Actor, BattleEvent, FailNote, ManipulationVolatile};
use crate::format::Mechanics;
use crate::state::{PokemonState, Position, SideId, Status, Terrain, Volatiles, Weather};
use crate::stats::{apply_stage, modify};
use crate::util::StackVec;

/// Status moves whose effects this module implements beyond their data.
pub(crate) fn handles(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::ACUPRESSURE
            | ids::BELLY_DRUM
            | ids::CLANGOROUS_SOUL
            | ids::FILLET_AWAY
            | ids::TAKE_HEART
            | ids::PAIN_SPLIT
            | ids::PSYCH_UP
            | ids::PSYCHO_SHIFT
            | ids::HEART_SWAP
            | ids::GUARD_SPLIT
            | ids::GUARD_SWAP
            | ids::POWER_SPLIT
            | ids::POWER_SWAP
            | ids::SPEED_SWAP
            | ids::TOPSY_TURVY
            | ids::REST
            | ids::HEAL_BELL
            | ids::HEAL_PULSE
            | ids::FLORAL_HEALING
            | ids::JUNGLE_HEALING
            | ids::LUNAR_BLESSING
            | ids::MOONLIGHT
            | ids::MORNING_SUN
            | ids::SYNTHESIS
            | ids::SHORE_UP
            | ids::STRENGTH_SAP
            | ids::SOAK
            | ids::MAGIC_POWDER
            | ids::CONVERSION
            | ids::CONVERSION_2
            | ids::REFLECT_TYPE
            | ids::FORESTS_CURSE
            | ids::TRICK_OR_TREAT
            | ids::MEMENTO
            | ids::SPLASH
            | ids::CELEBRATE
            | ids::DARK_VOID
            | ids::AQUA_RING
            | ids::INGRAIN
            | ids::CHARGE
            | ids::FOCUS_ENERGY
            | ids::DRAGON_CHEER
            | ids::ROOST
            | ids::TAR_SHOT
            | ids::MINIMIZE
            | ids::DEFENSE_CURL
            | ids::STOCKPILE
            | ids::SWALLOW
            | ids::POWER_TRICK
            | ids::POWER_SHIFT
            | ids::ELECTRIFY
            | ids::LOCK_ON
    )
}

fn alive(battle: &Battle, pos: Position) -> Option<&PokemonState> {
    battle.state.at(pos).filter(|m| !m.is_fainted())
}

fn mon_mut(battle: &mut Battle, pos: Position) -> &mut PokemonState {
    battle.state.at_mut(pos).expect("Pokémon is active")
}

// ---------------------------------------------------------------------------
// Types.

/// Refreshes `PokemonState::types` (Showdown's `getTypes(true)`): the Tera
/// type once terastallized (not Stellar), otherwise `pokemon.types`
/// without Flying while roosting (Roost's `onType`), `Normal` if nothing is
/// left.
pub(crate) fn refresh_types(mon: &mut PokemonState) {
    if let Some(types) = mon.tera_types() {
        mon.types = types;
        return;
    }
    let types = mon.volatile_counters.types.unwrap_or(mon.base_types);
    mon.types = if mon.volatiles.has(Volatiles::ROOST) {
        match (types.primary, types.secondary) {
            (Type::Flying, Some(t)) | (t, Some(Type::Flying)) => TypeSet::mono(t),
            (Type::Flying, None) => TypeSet::mono(Type::Normal),
            _ => types,
        }
    } else {
        types
    };
}

/// Showdown's `hasType`: `getTypes()`, including the added type.
pub(crate) fn has_type(mon: &PokemonState, ty: Type) -> bool {
    mon.types.contains(ty) || (!mon.terastallized && mon.volatile_counters.added_type == Some(ty))
}

/// Whether `getTypes(false, true)` (the types before Terastallization)
/// includes `ty`: STAB also counts these.
pub(crate) fn pre_tera_has_type(mon: &PokemonState, ty: Type) -> bool {
    let types = if mon.terastallized { mon.volatile_counters.types.unwrap_or(mon.base_types) } else { mon.types };
    types.contains(ty) || mon.volatile_counters.added_type == Some(ty)
}

/// The types of `getTypes()`: up to two, plus the added type.
pub(crate) fn all_types(mon: &PokemonState) -> [Option<Type>; 3] {
    let added = if mon.terastallized { None } else { mon.volatile_counters.added_type };
    [Some(mon.types.primary), mon.types.secondary, added]
}

/// Showdown's `runImmunity` for a move of type `ty` against the Pokémon at
/// `pos`: Ground moves hit grounded Pokémon (`isGrounded`: Ingrain, a
/// roosting Flying type); other types check every type of `getTypes()`.
pub(crate) fn type_immune(battle: &Battle, pos: Position, ty: Type) -> bool {
    if ty == Type::Ground {
        return !field::is_grounded(battle, pos);
    }
    let Some(mon) = battle.state.at(pos) else { return false };
    all_types(mon).into_iter().flatten().any(|t| ty.against(t) == Effectiveness::Immune)
}

/// `getTypes().join()` is exactly `ty` (Soak, Magic Powder).
pub(crate) fn is_exactly(mon: &PokemonState, ty: Type) -> bool {
    mon.types == TypeSet::mono(ty) && (mon.terastallized || mon.volatile_counters.added_type.is_none())
}

/// `setType`'s refusals: Arceus and Silvally (by species number) and
/// terastallized Pokémon.
fn can_set_type(battle: &Battle, mon: &PokemonState) -> bool {
    // `this.species`: the species it transformed into.
    let num = battle.format.dex.species(super::tera::species_of(mon)).num;
    num != 493 && num != 773 && !mon.terastallized
}

/// Showdown's `setType` (not enforced): the new types, and no added type.
pub(crate) fn set_type(battle: &mut Battle, pos: Position, types: TypeSet) -> bool {
    if !can_set_type(battle, battle.state.at(pos).expect("Pokémon is active")) {
        return false;
    }
    let mon = mon_mut(battle, pos);
    mon.volatile_counters.types = Some(types);
    mon.volatile_counters.added_type = None;
    mon.volatile_counters.apparent_types = None;
    // `apparentType = types`: a copied Tera type no longer shows either, nor
    // one an Illusion revealed.
    mon.volatile_counters.apparent_type_stale = false;
    mon.illusion_apparent = None;
    // `setType` makes the types known.
    mon.volatile_counters.type_unknown = false;
    refresh_types(mon);
    true
}

/// Whether the move has a `self.onHit` ported here (Burn Up, Double
/// Shock, Psycho Shift): `selfDrops` runs it, and like any `self` effect it keeps
/// `runMoveEffects` and `getSpreadDamage` from failing the move loudly.
pub(crate) fn has_self_on_hit(move_id: MoveId) -> bool {
    matches!(move_id, ids::BURN_UP | ids::DOUBLE_SHOCK | ids::PSYCHO_SHIFT)
}

/// Burn Up's and Double Shock's `self.onHit` (`moveHit(source, source,
/// move, moveData.self, false, true)` from `selfDrops`, once per target the
/// hit didn't fail on, with no draws):
/// `pokemon.setType(pokemon.getTypes(true).map(type => type === 'Fire' ?
/// '???' : type))`, then `-start|<user>|typechange|<getTypes().join('/')>|
/// [from] move: X`. The line comes even when `setType` refuses (a
/// Terastallized Pokémon, Arceus, Silvally) and then shows the types
/// unchanged; a successful `setType` drops the added type.
pub(crate) fn self_on_hit(battle: &mut Battle, user: Position, move_id: MoveId) {
    // Showdown's Psycho Shift `self.onHit` (data/moves.ts): cure only
    // after the primary status succeeded, through the usual selfDrops.
    if move_id == ids::PSYCHO_SHIFT {
        super::status::cure_status(battle, user);
        return;
    }
    let removed = match move_id {
        ids::BURN_UP => Type::Fire,
        ids::DOUBLE_SHOCK => Type::Electric,
        _ => return,
    };
    let Some(mon) = battle.state.at(user) else { return };
    // `getTypes(true)`: `PokemonState::types`.
    let map = |t: Type| if t == removed { Type::Unknown } else { t };
    let types = TypeSet { primary: map(mon.types.primary), secondary: mon.types.secondary.map(map) };
    set_type(battle, user, types);
    let mon = battle.state.at(user).expect("checked above");
    // `getTypes()`: at most two here (a refused `setType` leaves an added
    // type only on a mono-typed Arceus or Silvally; Terastallizing clears
    // it).
    let mut shown = all_types(mon).into_iter().flatten();
    let primary = shown.next().expect("getTypes() is never empty");
    let types = TypeSet { primary, secondary: shown.next() };
    let target = actor(battle, user);
    let cause = crate::event::Cause::move_(move_id);
    battle.emit(BattleEvent::TypeChangeFrom { target, types, cause });
}

// ---------------------------------------------------------------------------
// Stats, HP and status helpers.

/// Showdown's `battle.heal` with a move as the effect: at least 1 HP,
/// nothing at full HP. Returns the HP restored.
fn heal(battle: &mut Battle, pos: Position, amount: u32) -> u16 {
    apply_heal(battle, pos, amount.max(1), HealSource::Move)
}

/// `battle.heal` logged `[silent]` (Rest) or `[from] <volatile>`.
fn heal_from(battle: &mut Battle, pos: Position, amount: u32, from: Option<ManipulationVolatile>) -> u16 {
    // `TryHeal`: Heal Block, Big Root.
    let effect = match from {
        Some(ManipulationVolatile::AquaRing) => super::events::Effect::volatile(Volatiles::AQUA_RING),
        Some(_) => super::events::Effect::volatile(Volatiles::INGRAIN),
        None => battle.active_move.map_or(super::events::Effect::None, |(_, m)| super::events::Effect::Move(m)),
    };
    let Some(amount) = super::try_heal(battle, pos, amount.max(1), None, effect) else { return 0 };
    let target = actor(battle, pos);
    let Some(mon) = battle.state.at_mut(pos).filter(|m| !m.is_fainted()) else { return 0 };
    let healed = amount.max(1).min((mon.max_hp() - mon.hp) as u32) as u16;
    if healed == 0 {
        return 0;
    }
    mon.hp += healed;
    let (hp, max_hp, status) = (mon.hp, mon.max_hp(), mon.status);
    battle.emit(BattleEvent::ManipulationHeal { target, hp, max_hp, status, from });
    healed
}

/// The Pokémon at team position `index` of `side`, named as Showdown would
/// (`p1a: X` on the field, `p1: X` otherwise).
fn team_actor(battle: &Battle, side: SideId, index: usize) -> Actor {
    if index < battle.format.active_per_side() {
        return actor(battle, Position::new(side, index as u8));
    }
    // A benched Pokémon keeps its Illusion.
    let s = battle.state.side(side);
    let species = super::abilities::switchin::seen_species(s, &s.team[index]);
    let id = super::abilities::switchin::seen_id(s, &s.team[index]);
    Actor { pos: Position::new(side, index as u8), species, active: false, id }
}

/// Showdown's `cureStatus` for the Pokémon at team position `index`:
/// `-curestatus|...|[msg]`. Returns whether there was a status to cure.
fn cure_status(battle: &mut Battle, side: SideId, index: usize) -> bool {
    let mon = &battle.state.side(side).team[index];
    if mon.is_fainted() || mon.status == Status::None {
        return false;
    }
    let (status, target) = (mon.status, team_actor(battle, side, index));
    battle.emit(BattleEvent::CureStatus { target, status, from_move: None });
    battle.state.side_mut(side).team[index].status = Status::None;
    true
}

// ---------------------------------------------------------------------------
// Move callbacks.

/// The move's `onTry` (`singleEvent('Try')` in `trySpreadMoveHit`).
/// `Undefined` lets the move go on; `Failed` fails it with `-fail`; `Null`
/// fails it silently (the callback logged its own message).
pub(super) fn on_try(battle: &mut Battle, user: Position, move_id: MoveId) -> Outcome {
    let Some(&mon) = alive(battle, user) else { return Outcome::Undefined };
    let (hp, max) = (mon.hp as u32, mon.max_hp() as u32);
    match move_id {
        ids::CLANGOROUS_SOUL if 100 * hp <= 33 * max || max == 1 => Outcome::Failed,
        ids::FILLET_AWAY if 2 * hp <= max || max == 1 => Outcome::Failed,
        ids::REST => {
            use pikapro_data::gen9::abilities as a;
            let has = |battle: &Battle, x| super::abilities::has_ability(battle, user, x);
            if matches!(mon.status, Status::Sleep { .. }) || has(battle, a::COMATOSE) {
                return Outcome::Failed;
            }
            if hp == max {
                let target = actor(battle, user);
                battle.emit(BattleEvent::HealFail { target });
                return Outcome::Null;
            }
            // `-fail|<user>|[from] ability: Insomnia|[of] <user>`.
            for ability in [a::INSOMNIA, a::VITAL_SPIRIT] {
                if has(battle, ability) {
                    let target = actor(battle, user);
                    let cause = crate::event::Cause::ability(ability).of(target);
                    battle.emit(BattleEvent::FailBy { target, cause });
                    return Outcome::Null;
                }
            }
            Outcome::Undefined
        }
        ids::DARK_VOID => {
            // `source.species.baseSpecies === 'Darkrai' || move.hasBounced`:
            // the species it transformed into; a move bounced back by Magic
            // Bounce works.
            let darkrai = battle.format.dex.species(super::tera::species_of(&mon)).base_species == species::DARKRAI;
            if darkrai || battle.move_state.has_bounced {
                return Outcome::Undefined;
            }
            let user_actor = actor(battle, user);
            battle.emit(BattleEvent::MoveFail { user: user_actor, move_id, note: FailNote::None });
            battle.hint(crate::event::Hint::OnlyForme(move_id));
            Outcome::Null
        }
        ids::STOCKPILE if mon.volatiles.has(Volatiles::STOCKPILE) && mon.volatile_counters.stockpile_layers >= 3 => {
            Outcome::Failed
        }
        ids::SPIT_UP | ids::SWALLOW if !mon.volatiles.has(Volatiles::STOCKPILE) => Outcome::Failed,
        // (Splash under Gravity never gets here: Gravity's `onBeforeMove`
        // stops every move with the `gravity` flag, `field.rs`.)
        _ => Outcome::Undefined,
    }
}

/// The move's own `onTryHit` (`singleEvent('TryHit')` on the first target
/// in `spreadMoveHit`). Returns false if the hit fails (logging `-fail`
/// when the callback returned `false`).
pub(crate) fn try_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    let outcome = match move_id {
        // `if (!this.boost(move.boosts)) return null; delete move.boosts;`
        ids::CLANGOROUS_SOUL | ids::FILLET_AWAY => {
            let Some(b) = battle.format.dex.move_(move_id).boosts else { return true };
            if boosts::boost(battle, target, b, Some(user), false, false) { Outcome::Undefined } else { Outcome::Null }
        }
        ids::SPLASH => {
            battle.emit(BattleEvent::Nothing);
            Outcome::Undefined
        }
        ids::CELEBRATE => {
            let a = actor(battle, target);
            battle.emit(BattleEvent::MoveActivate { actor: a, move_id, of: None });
            Outcome::Undefined
        }
        ids::ELECTRIFY => {
            let Some(mon) = battle.state.at(target) else { return true };
            if !super::queue::will_move(battle, target) && mon.active_turns > 0 {
                Outcome::Failed
            } else {
                Outcome::Undefined
            }
        }
        ids::LOCK_ON if battle.state.at(user).is_some_and(|m| m.volatiles.has(Volatiles::LOCK_ON)) => Outcome::Failed,
        // Psycho Shift's `onTryHit` (data/moves.ts). The move pipeline
        // copies this status into its ActiveMove after this callback.
        ids::PSYCHO_SHIFT if battle.state.at(user).is_none_or(|m| m.status == Status::None) => Outcome::Failed,
        _ => Outcome::Undefined,
    };
    match outcome {
        Outcome::Failed => {
            fail_still(battle, user);
            false
        }
        Outcome::Null | Outcome::NotFail => false,
        _ => true,
    }
}

/// Clangorous Soul and Fillet Away apply their boosts in `onTryHit` and
/// delete them, so `runMoveEffects` must not apply them again.
pub(crate) fn skips_data_boosts(move_id: MoveId) -> bool {
    matches!(move_id, ids::CLANGOROUS_SOUL | ids::FILLET_AWAY)
}

/// The move's `onHit` (`singleEvent('Hit')` in `runMoveEffects`), or `None`
/// if it has none here. `target` is the Pokémon hit, `user` the move's user.
pub(super) fn on_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Option<Outcome> {
    use Outcome::{Failed, NotFail, Succeeded};
    Some(match move_id {
        ids::ACUPRESSURE => acupressure(battle, target, user),
        ids::BELLY_DRUM => belly_drum(battle, target),
        ids::CLANGOROUS_SOUL => {
            let max = mon_mut(battle, target).max_hp() as u32;
            direct_damage(battle, target, super::hp_fraction(max, 33, 100));
            Succeeded
        }
        ids::FILLET_AWAY => {
            let max = mon_mut(battle, target).max_hp() as u32;
            direct_damage(battle, target, super::hp_fraction(max, 1, 2));
            Succeeded
        }
        ids::TAKE_HEART => {
            let boosted =
                boosts::boost(battle, target, StatBoosts::new([0, 0, 1, 1, 0, 0, 0]), Some(user), false, false);
            Outcome::from_bool(cure_status(battle, target.side, target.slot as usize) || boosted)
        }
        ids::PAIN_SPLIT => pain_split(battle, target, user),
        ids::PSYCH_UP => psych_up(battle, target, user),
        ids::HEART_SWAP | ids::GUARD_SWAP | ids::POWER_SWAP => swap_boosts(battle, target, user, move_id),
        ids::GUARD_SPLIT | ids::POWER_SPLIT => split_stats(battle, target, user, move_id),
        ids::SPEED_SWAP => {
            let target_spe = mon_mut(battle, target).stats[Stat::Spe];
            let user_spe = mon_mut(battle, user).stats[Stat::Spe];
            mon_mut(battle, target).stats[Stat::Spe] = user_spe;
            mon_mut(battle, user).stats[Stat::Spe] = target_spe;
            let (a, of) = (actor(battle, user), actor(battle, target));
            battle.emit(BattleEvent::MoveActivate { actor: a, move_id, of: Some(of) });
            Succeeded
        }
        ids::TOPSY_TURVY => {
            let boosts = &mut mon_mut(battle, target).boosts;
            let mut changed = false;
            for id in BoostId::ALL {
                let stage = stage_mut(boosts, id);
                changed |= *stage != 0;
                *stage = -*stage;
            }
            if !changed {
                return Some(Failed);
            }
            let a = actor(battle, target);
            battle.emit(BattleEvent::InvertBoost { target: a });
            Succeeded
        }
        ids::REST => rest(battle, target),
        ids::HEAL_BELL => {
            let a = actor(battle, user);
            battle.emit(BattleEvent::MoveActivate { actor: a, move_id, of: None });
            // Soundproof and Good as Gold allies (on the field, their
            // ability not ignored by the move) are immune.
            let mut success = false;
            for index in 0..battle.state.side(target.side).team.len() {
                let ally = Position::new(target.side, index as u8);
                if ally != user && !super::abilities::suppressing_ability(battle, ally) {
                    use pikapro_data::gen9::abilities as a;
                    let immune = [a::SOUNDPROOF, a::GOOD_AS_GOLD]
                        .into_iter()
                        .find(|&x| super::abilities::has_ability(battle, ally, x));
                    if let Some(ability) = immune {
                        let target = actor(battle, ally);
                        let cause = crate::event::Cause::ability(ability);
                        battle.emit(BattleEvent::ImmuneFrom { target, detail: None, cause });
                        continue;
                    }
                }
                success |= cure_status(battle, target.side, index);
            }
            Outcome::from_bool(success)
        }
        ids::HEAL_PULSE | ids::FLORAL_HEALING => {
            let max = mon_mut(battle, target).max_hp() as u32;
            let grassy = move_id == ids::FLORAL_HEALING && battle.state.field.terrain == Some(Terrain::Grassy);
            // Heal Pulse from Mega Launcher heals 3/4.
            let launcher = move_id == ids::HEAL_PULSE
                && super::abilities::has_ability(battle, user, pikapro_data::gen9::abilities::MEGA_LAUNCHER);
            let amount = if grassy {
                modify(max, 2732)
            } else if launcher {
                modify(max, 3072)
            } else {
                max.div_ceil(2)
            };
            if heal(battle, target, amount) > 0 {
                // `if (success && !target.isAlly(source)) target.staleness
                // = 'external';` (the Endless Battle Clause).
                if target.side != user.side {
                    super::endless::external(battle, target);
                }
                Succeeded
            } else {
                let a = actor(battle, target);
                battle.emit(BattleEvent::HealFail { target: a });
                NotFail
            }
        }
        ids::JUNGLE_HEALING | ids::LUNAR_BLESSING => {
            let max = mon_mut(battle, target).max_hp() as u32;
            let healed = heal(battle, target, modify(max, 1024)) > 0;
            Outcome::from_bool(cure_status(battle, target.side, target.slot as usize) || healed)
        }
        ids::MOONLIGHT | ids::MORNING_SUN | ids::SYNTHESIS | ids::SHORE_UP => {
            // `pokemon.effectiveWeather(undefined, true)` (Cloud Nine, Air
            // Lock, Utility Umbrella; Mega Sol's sun, announced);
            // Shore Up's `field.isWeather('sandstorm')`.
            let weather = if move_id == ids::SHORE_UP {
                None
            } else {
                super::mega::pokemon_weather_shown(battle, target, move_id)
            };
            let sand = super::abilities::effective_weather(battle) == Some(Weather::Sand);
            let factor = match (move_id, weather) {
                (ids::SHORE_UP, _) if sand => 2732,
                (ids::SHORE_UP, _) => 2048,
                (_, Some(Weather::Sun | Weather::HarshSun)) => 2732,
                (_, Some(Weather::Rain | Weather::HeavyRain | Weather::Sand | Weather::Snow)) => 1024,
                _ => 2048,
            };
            let max = mon_mut(battle, target).max_hp() as u32;
            if heal(battle, target, modify(max, factor)) > 0 {
                Succeeded
            } else {
                let a = actor(battle, target);
                battle.emit(BattleEvent::HealFail { target: a });
                NotFail
            }
        }
        ids::STRENGTH_SAP => {
            let mon = mon_mut(battle, target);
            if mon.boosts.atk == -6 {
                return Some(Failed);
            }
            // `getStat('atk', false, true)`: boosted, unmodified (Wonder
            // Room swaps only Def and SpD).
            let atk = apply_stage(mon.stats[Stat::Atk], mon.boosts.atk) as u32;
            let lowered = boosts::boost(battle, target, one_boost(BoostId::Atk, -1), Some(user), false, true);
            // `this.heal(atk, source, target)`: the target is the heal's
            // source (Liquid Ooze).
            let effect = super::events::Effect::Move(ids::STRENGTH_SAP);
            let healed =
                super::heal_inner(battle, user, atk.max(1), Some(target), effect, HealSource::Move).unwrap_or(0) > 0;
            Outcome::from_bool(healed || lowered)
        }
        ids::SOAK => {
            let mon = *mon_mut(battle, target);
            if is_exactly(&mon, Type::Water) || !set_type(battle, target, TypeSet::mono(Type::Water)) {
                let a = actor(battle, target);
                battle.emit(BattleEvent::Fail { user: a });
                return Some(Outcome::Null);
            }
            let a = actor(battle, target);
            battle.emit(BattleEvent::TypeChange { target: a, types: TypeSet::mono(Type::Water), silent: false });
            Succeeded
        }
        ids::MAGIC_POWDER => {
            let mon = *mon_mut(battle, target);
            if is_exactly(&mon, Type::Psychic) || !set_type(battle, target, TypeSet::mono(Type::Psychic)) {
                return Some(Failed);
            }
            let a = actor(battle, target);
            battle.emit(BattleEvent::TypeChange { target: a, types: TypeSet::mono(Type::Psychic), silent: false });
            Succeeded
        }
        ids::CONVERSION => {
            let mon = *mon_mut(battle, target);
            let ty = battle.format.dex.move_(mon.moves[0].id).ty;
            if has_type(&mon, ty) || !set_type(battle, target, TypeSet::mono(ty)) {
                return Some(Failed);
            }
            let a = actor(battle, target);
            battle.emit(BattleEvent::TypeChange { target: a, types: TypeSet::mono(ty), silent: false });
            Succeeded
        }
        ids::CONVERSION_2 => conversion_2(battle, target, user),
        ids::REFLECT_TYPE => {
            let source = *mon_mut(battle, user);
            // `source.species`: the species it transformed into.
            let num = battle.format.dex.species(super::tera::species_of(&source)).num;
            if num == 493 || num == 773 || source.terastallized {
                return Some(Failed);
            }
            let copied = *mon_mut(battle, target);
            // `target.getTypes(true).filter(type => type !== '???')`: with
            // nothing left, Normal if the target has an added type, or fail.
            let known = [Some(copied.types.primary), copied.types.secondary];
            let mut kept = known.into_iter().flatten().filter(|&t| t != Type::Unknown);
            let types = match (kept.next(), kept.next()) {
                (Some(a), b) => TypeSet { primary: a, secondary: b },
                (None, _) if copied.volatile_counters.added_type.is_some() => TypeSet::mono(Type::Normal),
                (None, _) => return Some(Failed),
            };
            let (a, of) = (actor(battle, user), actor(battle, target));
            battle.emit(BattleEvent::TypeCopy { target: a, of });
            set_type(battle, user, types);
            let counters = &mut mon_mut(battle, user).volatile_counters;
            counters.added_type = copied.volatile_counters.added_type;
            // `knownType`: the copy stays unseen unless the target is an
            // ally whose types are known; the end of the turn reveals it.
            let known = target.side == user.side && !copied.volatile_counters.type_unknown;
            counters.type_unknown = !known;
            if !known {
                // `oldApparentType`: what the user showed before this move
                // (still the pre-Reflect-Type types if it was used twice).
                counters.apparent_types = Some(source.volatile_counters.apparent_types.unwrap_or(source.types));
            }
            Succeeded
        }
        ids::FORESTS_CURSE | ids::TRICK_OR_TREAT => {
            let ty = if move_id == ids::FORESTS_CURSE { Type::Grass } else { Type::Ghost };
            let mon = mon_mut(battle, target);
            // `addType` fails on a terastallized Pokémon.
            if has_type(mon, ty) || mon.terastallized {
                return Some(Failed);
            }
            mon.volatile_counters.added_type = Some(ty);
            let a = actor(battle, target);
            battle.emit(BattleEvent::TypeAdd { target: a, ty, move_id: Some(move_id) });
            if move_id == ids::TRICK_OR_TREAT && battle.format.active_per_side() == 2 && target.slot == 1 {
                // A Ghost-type Curse queued by the target now aims at its
                // left ally slot.
                let id = mon_mut(battle, target).id;
                for action in battle.queue.as_mut_slice() {
                    if action.mon.is_some_and(|m| m.side == target.side && m.id == id)
                        && let ActionKind::Move { move_id: ids::CURSE, target_loc, .. } = &mut action.kind
                    {
                        *target_loc = -1;
                        break;
                    }
                }
            }
            Succeeded
        }
        ids::SWALLOW => {
            let mon = mon_mut(battle, target);
            let layers = mon.volatile_counters.stockpile_layers.clamp(1, 3);
            let amount = modify(mon.max_hp() as u32, [1024, 2048, 4096][layers as usize - 1]);
            let healed = heal(battle, target, amount) > 0;
            if !healed {
                let a = actor(battle, target);
                battle.emit(BattleEvent::HealFail { target: a });
            }
            remove_volatile(battle, target, Volatiles::STOCKPILE);
            if healed { Succeeded } else { NotFail }
        }
        ids::LOCK_ON => {
            let locked = mon_mut(battle, target).id;
            // `source.addVolatile('lockon', target)`: `TryAddVolatile`'s
            // ties draw.
            super::events::try_add_volatile(battle, user, "lockon");
            let mon = mon_mut(battle, user);
            mon.volatiles.add(Volatiles::LOCK_ON);
            mon.volatile_counters.lock_on = Some((target.side, locked));
            mon.volatile_counters.lock_on_turns = 2;
            let (a, of) = (actor(battle, user), actor(battle, target));
            battle.emit(BattleEvent::MoveActivate { actor: a, move_id, of: Some(of) });
            Succeeded
        }
        ids::CLEAR_SMOG => {
            mon_mut(battle, target).boosts = crate::state::Boosts::default();
            let a = actor(battle, target);
            battle.emit(BattleEvent::ClearBoost { target: a });
            Succeeded
        }
        _ => return None,
    })
}

fn acupressure(battle: &mut Battle, target: Position, user: Position) -> Outcome {
    let mon = mon_mut(battle, target);
    let (mut stats, mut n) = ([BoostId::Atk; 7], 0);
    for id in BoostId::ALL {
        if *stage_mut(&mut mon.boosts, id) < 6 {
            stats[n] = id;
            n += 1;
        }
    }
    if n == 0 {
        return Outcome::Failed;
    }
    let stat = *battle.rng.sample(&stats[..n]);
    boosts::boost(battle, target, one_boost(stat, 2), Some(user), false, false);
    Outcome::Succeeded
}

fn belly_drum(battle: &mut Battle, target: Position) -> Outcome {
    let mon = mon_mut(battle, target);
    let (hp, max) = (mon.hp as u32, mon.max_hp() as u32);
    if 2 * hp <= max || mon.boosts.atk >= 6 || max == 1 {
        return Outcome::Failed;
    }
    direct_damage(battle, target, super::hp_fraction(max, 1, 2));
    // `boost({atk: 12})`, announced as `-setboost` (Contrary makes it -6).
    let effect = super::events::Effect::Move(ids::BELLY_DRUM);
    super::boosts::boost_from(battle, target, one_boost(BoostId::Atk, 12), Some(target), effect, false, false);
    Outcome::Succeeded
}

fn pain_split(battle: &mut Battle, target: Position, user: Position) -> Outcome {
    let (target_hp, user_hp) = (mon_mut(battle, target).hp as u32, mon_mut(battle, user).hp as u32);
    let average = ((target_hp + user_hp) / 2).max(1);
    for (pos, silent) in [(target, true), (user, false)] {
        let mon = mon_mut(battle, pos);
        // `sethp`: nothing for a fainted Pokémon, capped at max HP.
        if mon.hp > 0 {
            mon.hp = average.min(mon.max_hp() as u32) as u16;
        }
        let (hp, max_hp, status) = (mon.hp, mon.max_hp(), mon.status);
        let a = actor(battle, pos);
        battle.emit(BattleEvent::SetHp { target: a, hp, max_hp, status, silent });
    }
    Outcome::Succeeded
}

fn psych_up(battle: &mut Battle, target: Position, user: Position) -> Outcome {
    let copied = *mon_mut(battle, target);
    mon_mut(battle, user).boosts = copied.boosts;
    // Crit-ratio volatiles are copied too (removed first, silently).
    for flag in [Volatiles::DRAGON_CHEER, Volatiles::FOCUS_ENERGY] {
        remove_volatile(battle, user, flag);
    }
    for flag in [Volatiles::DRAGON_CHEER, Volatiles::FOCUS_ENERGY] {
        if copied.volatiles.has(flag) {
            add(battle, user, flag, true);
            if flag == Volatiles::DRAGON_CHEER {
                mon_mut(battle, user).volatile_counters.dragon_cheer_dragon =
                    copied.volatile_counters.dragon_cheer_dragon;
            }
        }
    }
    let (source, of) = (actor(battle, user), actor(battle, target));
    battle.emit(BattleEvent::CopyBoost { source, target: of });
    Outcome::Succeeded
}

fn swap_boosts(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Outcome {
    let stats: &[BoostId] = match move_id {
        ids::GUARD_SWAP => &[BoostId::Def, BoostId::SpD],
        ids::POWER_SWAP => &[BoostId::Atk, BoostId::SpA],
        _ => &BoostId::ALL,
    };
    for &id in stats {
        let a = *stage_mut(&mut mon_mut(battle, target).boosts, id);
        let b = *stage_mut(&mut mon_mut(battle, user).boosts, id);
        *stage_mut(&mut mon_mut(battle, target).boosts, id) = b;
        *stage_mut(&mut mon_mut(battle, user).boosts, id) = a;
    }
    let (source, of) = (actor(battle, user), actor(battle, target));
    battle.emit(BattleEvent::SwapBoost { source, target: of, move_id });
    Outcome::Succeeded
}

fn split_stats(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Outcome {
    let stats = if move_id == ids::GUARD_SPLIT { [Stat::Def, Stat::SpD] } else { [Stat::Atk, Stat::SpA] };
    for stat in stats {
        let average = (mon_mut(battle, target).stats[stat] as u32 + mon_mut(battle, user).stats[stat] as u32) / 2;
        mon_mut(battle, target).stats[stat] = average as u16;
        mon_mut(battle, user).stats[stat] = average as u16;
    }
    let (a, of) = (actor(battle, user), actor(battle, target));
    battle.emit(BattleEvent::MoveActivate { actor: a, move_id, of: Some(of) });
    Outcome::Succeeded
}

/// Rest's `onHit`: `setStatus('slp')` (replacing any other status), three
/// turns of sleep, then a full heal.
fn rest(battle: &mut Battle, pos: Position) -> Outcome {
    // `setStatus`: no type is immune to sleep; the `SetStatus` event.
    // Electric Terrain announces blocking a move without secondaries;
    // Misty Terrain only one with a `status`.
    // `runStatusImmunity('slp')`, then the `SetStatus` event: Uproar's
    // `onAnySetStatus` (its own `-fail`, `null`) and the
    // abilities' and items' (Purifying Salt, Leaf Guard, Sweet Veil, Flower
    // Veil, Shields Down, ...), sorted by speed.
    if super::events::immunity_event(battle, pos, "slp") != Some(true) {
        return Outcome::Failed;
    }
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::SetStatus,
        super::events::Target::Pokemon(pos),
        Some(pos),
        super::events::Effect::Move(ids::REST),
        super::events::Relay::Status(StatusId::Sleep),
    );
    match super::events::run_event(battle, &mut ev, None) {
        super::events::Relay::Null => return Outcome::Null,
        r if !r.is_truthy() => return Outcome::Failed,
        _ => {}
    }
    let target = actor(battle, pos);
    let placeholder = Status::Sleep { turns: 0 };
    battle.emit(BattleEvent::StatusStart { target, status: placeholder, from_move: Some(ids::REST) });
    // slp's `onStart` draws the duration, which Rest then overrides.
    match battle.format.mechanics {
        Mechanics::Champions => {
            battle.rng.sample(&[2, 3, 3]);
        }
        Mechanics::ScarletViolet => {
            battle.rng.random_range(2, 5);
        }
    }
    let mon = mon_mut(battle, pos);
    mon.status = Status::Sleep { turns: 3 };
    // `setStatus('slp', source, move)`: its own source.
    mon.sleep_source_ally = true;
    let max = mon.max_hp() as u32;
    // `runEvent('AfterSetStatus', ...)` (Lum Berry, Chesto Berry).
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::AfterSetStatus,
        super::events::Target::Pokemon(pos),
        Some(pos),
        super::events::Effect::Move(ids::REST),
        Relay::Status(StatusId::Sleep),
    );
    super::events::run_event(battle, &mut ev, None);
    heal_from(battle, pos, max, None);
    Outcome::Succeeded
}

fn conversion_2(battle: &mut Battle, target: Position, user: Position) -> Outcome {
    // `target.lastMoveUsed.type` (after `ModifyType`); Struggle's type is
    // `???`, which resists nothing.
    let Some((last, attack)) = mon_mut(battle, target).volatile_counters.last_move_used else {
        return Outcome::Failed;
    };
    if last == ids::STRUGGLE {
        return Outcome::Failed;
    }
    // `dex.types.names()`: alphabetical (Stellar resists nothing).
    const NAMES: [Type; 18] = [
        Type::Bug,
        Type::Dark,
        Type::Dragon,
        Type::Electric,
        Type::Fairy,
        Type::Fighting,
        Type::Fire,
        Type::Flying,
        Type::Ghost,
        Type::Grass,
        Type::Ground,
        Type::Ice,
        Type::Normal,
        Type::Poison,
        Type::Psychic,
        Type::Rock,
        Type::Steel,
        Type::Water,
    ];
    let source = *mon_mut(battle, user);
    let (mut possible, mut n) = ([Type::Normal; 18], 0);
    for ty in NAMES {
        if !has_type(&source, ty) && matches!(attack.against(ty), Effectiveness::Immune | Effectiveness::Exp(-1)) {
            possible[n] = ty;
            n += 1;
        }
    }
    if n == 0 {
        return Outcome::Failed;
    }
    let ty = *battle.rng.sample(&possible[..n]);
    if !set_type(battle, user, TypeSet::mono(ty)) {
        return Outcome::Failed;
    }
    let a = actor(battle, user);
    battle.emit(BattleEvent::TypeChange { target: a, types: TypeSet::mono(ty), silent: false });
    Outcome::Succeeded
}

/// `selfdestruct: 'ifHit'` (Memento): after the move's effects on a target
/// it did not fail against, the user faints.
pub(crate) fn self_destruct_if_hit(battle: &mut Battle, user: Position, data: &MoveData) {
    if data.self_destruct && data.unmodelled.contains(&"selfdestruct") {
        super::faint(battle, user);
    }
}

/// The `onHit` of a secondary effect (Showdown runs it through `moveHit`
/// with the secondary as the effect, so status failures are silent and
/// sleep names no move).
pub(crate) fn secondary_on_hit(battle: &mut Battle, target: Position, user: Position) {
    let Some((_, move_id)) = battle.active_move else { return };
    let Some(mon) = battle.state.at(target) else { return };
    match move_id {
        ids::ALLURING_VOICE if mon.volatile_counters.stats_raised_this_turn => {
            volatiles::add_volatile(battle, target, "confusion", Some(user), true);
        }
        ids::BURNING_JEALOUSY if mon.volatile_counters.stats_raised_this_turn => {
            status::try_set_status(battle, target, StatusId::Burn, Some(user), true);
        }
        ids::DIRE_CLAW | ids::TRI_ATTACK => {
            let choices = if move_id == ids::DIRE_CLAW {
                [StatusId::Poison, StatusId::Paralysis, StatusId::Sleep]
            } else {
                [StatusId::Burn, StatusId::Paralysis, StatusId::Freeze]
            };
            // `this.sample(...)` then `target.trySetStatus(status, source)`
            // (Champions too: no `-fail` for an already-statused target).
            let status = *battle.rng.sample(&choices);
            // The effect is the secondary, not the move: no `[from] move:`.
            // The move stays active (Mold Breaker suppresses Sweet Veil).
            let effect = super::events::Effect::Condition(super::events::Condition::Other);
            status::set_status_from(battle, target, status, Some(user), effect, true);
        }
        ids::EERIE_SPELL => {
            if mon.is_fainted() {
                return;
            }
            let Some(last) = mon.last_move else { return };
            let Some(slot) = mon.moves.iter().position(|s| s.id == last) else { return };
            let pp = mon.moves[slot].pp;
            if pp == 0 {
                return;
            }
            let amount = pp.min(3);
            mon_mut(battle, target).moves[slot].pp -= amount;
            let a = actor(battle, target);
            battle.emit(BattleEvent::PpDrain { target: a, move_id: last, amount });
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Around the move.

/// `ModifyType` (Electrify): the move becomes Electric (except Struggle).
/// ([`typing::electrify_type`](crate::typing::electrify_type)).
pub(crate) fn modify_type(battle: &Battle, user: Position, move_id: MoveId, data: &mut MoveData) {
    if battle.state.at(user).is_some_and(|m| m.volatiles.has(Volatiles::ELECTRIFY))
        && let Some(ty) = crate::typing::electrify_type(move_id)
    {
        data.ty = ty;
    }
}

/// `MoveAborted` (the move was stopped before being used): Charge ends if
/// the move is Electric.
pub(crate) fn move_aborted(battle: &mut Battle, user: Position, move_id: MoveId) {
    if move_id == super::volatiles::RECHARGE {
        return;
    }
    let ty = battle.format.dex.move_(move_id).ty;
    if ty == Type::Electric && move_id != ids::CHARGE {
        remove_volatile(battle, user, Volatiles::CHARGE);
    }
}

/// Records Showdown's `move.hitTargets` for the move's `AfterMove`
/// (Sparkling Aria) and Magician. Each `ActiveMove` has its own: a move
/// that called another (Metronome, Copycat, Sleep Talk) finishes its hit
/// after the called one, which is `battle.activeMove` from then on, so it
/// leaves the called move's targets in place.
pub(crate) fn record_hit_targets(battle: &mut Battle, user: Position, move_id: MoveId, targets: &[Position]) {
    if battle.active_move.is_some_and(|(_, m)| m != move_id) {
        return;
    }
    let mask = targets.iter().fold(0u8, |m, t| m | 1 << (t.side.index() * 2 + t.slot as usize));
    if let Some(mon) = battle.state.at_mut(user) {
        mon.record.hit_targets = Some(mask);
    }
}

/// `AfterMove`: the move's own (`singleEvent`: Spit Up, Sparkling Aria),
/// then the user's volatiles' (`runEvent`: Charge).
pub(crate) fn after_move(battle: &mut Battle, user: Position, move_id: MoveId) {
    let hit_targets = battle.state.at_mut(user).and_then(|m| m.record.hit_targets.take());
    match move_id {
        ids::SPIT_UP => {
            remove_volatile(battle, user, Volatiles::STOCKPILE);
        }
        ids::SPARKLING_ARIA => {
            let hit_order = core::mem::take(&mut battle.order.hit_order);
            sparkling_aria(battle, user, hit_targets, hit_order);
        }
        _ => {}
    }
}

/// Charge's `onAfterMove` (a handler of `runEvent('AfterMove')`): it ends
/// after an Electric move other than Charge.
pub(crate) fn charge_after_move(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    let data = ev.move_data()?;
    if data.ty == Type::Electric && ev.effect.move_id() != Some(ids::CHARGE) {
        remove_volatile(battle, pos, Volatiles::CHARGE);
    }
    None
}

/// Sparkling Aria's `onAfterMove`: hit targets other than the user that
/// got its volatile (or any, if it hit several) are cured of their burn,
/// in `move.hitTargets`' order: the move's target order (the user's side,
/// then its foes), or the user's Magician's speed order (`hit_order`,
/// packed as `OrderState::hit_order` is).
fn sparkling_aria(battle: &mut Battle, user: Position, hit_targets: Option<u8>, hit_order: u16) {
    // `source.fainted` is set by `faintMessages`, which empties the slot: a
    // user Life Orb just knocked out still cures. `move.hasSheerForce` ends
    // the volatiles like a fainted user.
    let user_down = battle.state.at(user).is_none() || battle.move_state.has_sheer_force;
    let Some(mask) = hit_targets.filter(|_| !user_down) else {
        for side in SideId::BOTH {
            for mon in battle.state.side_mut(side).team.iter_mut() {
                mon.volatile_counters.sparkling_aria = false;
            }
        }
        return;
    };
    let many = mask.count_ones() > 1;
    let at = |code: u16| Position::new(if code < 2 { SideId::P1 } else { SideId::P2 }, (code % 2) as u8);
    let order: StackVec<Position, 4> = if hit_order != 0 && at((hit_order & 15) - 1) == user {
        (1..4).map(|i| hit_order >> (4 * i) & 15).take_while(|&n| n != 0).map(|n| at(n - 1)).collect()
    } else {
        let slots = 0..battle.format.active_per_side() as u8;
        let in_order = [user.side, user.side.foe()]
            .into_iter()
            .flat_map(|side| slots.clone().map(move |s| Position::new(side, s)));
        in_order.filter(|p| mask & 1 << (p.side.index() * 2 + p.slot as usize) != 0).collect()
    };
    for &pos in order.iter() {
        if pos == user {
            continue;
        }
        // `pokemon.isActive`: a Pokémon that fainted has left.
        let Some(mon) = battle.state.at_mut(pos) else { continue };
        let removed = mon.hp > 0 && core::mem::take(&mut mon.volatile_counters.sparkling_aria);
        if (removed || many) && mon.status == Status::Burn {
            cure_status(battle, pos.side, pos.slot as usize);
        }
    }
}

// ---------------------------------------------------------------------------
// Volatiles.

/// One of this module's volatiles: its flag, or Sparkling Aria's, which
/// has no handlers and is kept as a counter
/// (`VolatileCounters::sparkling_aria`) instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnVolatile {
    Flag(u128),
    SparklingAria,
}

impl OwnVolatile {
    /// Whether `mon` has it.
    pub(crate) fn present(self, mon: &PokemonState) -> bool {
        match self {
            OwnVolatile::Flag(flag) => mon.volatiles.has(flag),
            OwnVolatile::SparklingAria => mon.volatile_counters.sparkling_aria,
        }
    }
}

/// One of this module's volatiles, by Showdown condition ID.
pub(crate) fn volatile_of(id: &str) -> Option<OwnVolatile> {
    Some(OwnVolatile::Flag(match id {
        "aquaring" => Volatiles::AQUA_RING,
        "ingrain" => Volatiles::INGRAIN,
        "charge" => Volatiles::CHARGE,
        "focusenergy" => Volatiles::FOCUS_ENERGY,
        "dragoncheer" => Volatiles::DRAGON_CHEER,
        "roost" => Volatiles::ROOST,
        "tarshot" => Volatiles::TAR_SHOT,
        "minimize" => Volatiles::MINIMIZE,
        "defensecurl" => Volatiles::DEFENSE_CURL,
        "stockpile" => Volatiles::STOCKPILE,
        "powertrick" => Volatiles::POWER_TRICK,
        "powershift" => Volatiles::POWER_SHIFT,
        "electrify" => Volatiles::ELECTRIFY,
        "sparklingaria" => return Some(OwnVolatile::SparklingAria),
        _ => return None,
    }))
}

/// `addVolatile` for one of this module's volatiles ([`volatile_of`]),
/// on a Pokémon that has not fainted. Returns `addVolatile`'s result
/// (`None` for `null`).
pub(crate) fn add_volatile(battle: &mut Battle, target: Position, id: &str) -> Option<bool> {
    match volatile_of(id) {
        None => Some(false),
        // Sparkling Aria's has no handlers: a flag is enough.
        Some(OwnVolatile::SparklingAria) => {
            let counters = &mut mon_mut(battle, target).volatile_counters;
            let added = !counters.sparkling_aria;
            counters.sparkling_aria = true;
            Some(added)
        }
        Some(OwnVolatile::Flag(flag)) => add(battle, target, flag, false),
    }
}

/// Transform copies Dragon Cheer and Focus Energy (Gen 6+):
/// the user's own are removed, then the target's are added (`[silent]`),
/// Dragon Cheer keeping the target's `hasDragonType`.
/// (Showdown copies Laser Focus too, a Past move in Gen 9 and Champions.)
pub(crate) fn transform_copy(battle: &mut Battle, user: Position, into: &PokemonState) {
    mon_mut(battle, user).volatiles.clear(Volatiles::DRAGON_CHEER | Volatiles::FOCUS_ENERGY);
    for flag in [Volatiles::DRAGON_CHEER, Volatiles::FOCUS_ENERGY] {
        if into.volatiles.has(flag) {
            add(battle, user, flag, true);
            if flag == Volatiles::DRAGON_CHEER {
                mon_mut(battle, user).volatile_counters.dragon_cheer_dragon =
                    into.volatile_counters.dragon_cheer_dragon;
            }
        }
    }
}

/// `addVolatile`: `onRestart` if present (else fail), otherwise `onStart`.
/// `copied`: a volatile copied by Psych Up or Transform (`[silent]` start).
fn add(battle: &mut Battle, target: Position, flag: u128, copied: bool) -> Option<bool> {
    let mon = *mon_mut(battle, target);
    if mon.volatiles.has(flag) {
        return match flag {
            Volatiles::CHARGE => {
                start_message(battle, target, ManipulationVolatile::Charge, false);
                Some(true)
            }
            Volatiles::MINIMIZE | Volatiles::DEFENSE_CURL => None,
            Volatiles::STOCKPILE => Some(stockpile_layer(battle, target)),
            Volatiles::POWER_TRICK | Volatiles::POWER_SHIFT => {
                remove_volatile(battle, target, flag);
                Some(true)
            }
            _ => Some(false),
        };
    }
    let started = match flag {
        Volatiles::AQUA_RING => start_message(battle, target, ManipulationVolatile::AquaRing, false),
        Volatiles::INGRAIN => start_message(battle, target, ManipulationVolatile::Ingrain, false),
        Volatiles::CHARGE => start_message(battle, target, ManipulationVolatile::Charge, false),
        Volatiles::FOCUS_ENERGY if mon.volatiles.has(Volatiles::DRAGON_CHEER) => false,
        Volatiles::FOCUS_ENERGY => start_message(battle, target, ManipulationVolatile::FocusEnergy, copied),
        Volatiles::DRAGON_CHEER if mon.volatiles.has(Volatiles::FOCUS_ENERGY) => false,
        Volatiles::DRAGON_CHEER => {
            start_message(battle, target, ManipulationVolatile::DragonCheer, copied);
            let mon = mon_mut(battle, target);
            mon.volatile_counters.dragon_cheer_dragon = has_type(mon, Type::Dragon);
            true
        }
        Volatiles::ROOST => {
            // A terastallized Pokémon keeps its type (Showdown only hints).
            // Stellar: `getTypes` runs the `Type` event, where this Roost
            // (already added) filters Flying out, so never.
            if mon.terastallized {
                if mon.tera_type != Type::Stellar && has_type(&mon, Type::Flying) {
                    battle.hint(crate::event::Hint::TeraRoost);
                }
                false
            } else {
                let a = actor(battle, target);
                battle.emit(BattleEvent::SingleTurn { actor: a, move_id: ids::ROOST });
                let mon = mon_mut(battle, target);
                mon.volatiles.add(flag);
                refresh_types(mon);
                true
            }
        }
        Volatiles::TAR_SHOT if mon.terastallized => false,
        Volatiles::TAR_SHOT => start_message(battle, target, ManipulationVolatile::TarShot, false),
        Volatiles::MINIMIZE | Volatiles::DEFENSE_CURL => true,
        Volatiles::STOCKPILE => {
            let mon = mon_mut(battle, target);
            let counters = &mut mon.volatile_counters;
            (counters.stockpile_layers, counters.stockpile_def, counters.stockpile_spd) = (0, 0, 0);
            mon.volatiles.add(flag);
            stockpile_layer(battle, target)
        }
        Volatiles::POWER_TRICK | Volatiles::POWER_SHIFT => {
            let v = if flag == Volatiles::POWER_TRICK {
                ManipulationVolatile::PowerTrick
            } else {
                ManipulationVolatile::PowerShift
            };
            start_message(battle, target, v, false);
            swap_attack_defense(mon_mut(battle, target));
            true
        }
        Volatiles::ELECTRIFY => {
            let a = actor(battle, target);
            battle.emit(BattleEvent::SingleTurn { actor: a, move_id: ids::ELECTRIFY });
            true
        }
        _ => false,
    };
    if started {
        mon_mut(battle, target).volatiles.add(flag);
    }
    Some(started)
}

fn start_message(battle: &mut Battle, pos: Position, volatile: ManipulationVolatile, silent: bool) -> bool {
    let a = actor(battle, pos);
    battle.emit(BattleEvent::ManipulationStart { actor: a, volatile, silent });
    true
}

fn swap_attack_defense(mon: &mut PokemonState) {
    let stats = &mut mon.stats;
    let (atk, def) = (stats[Stat::Atk], stats[Stat::Def]);
    stats[Stat::Atk] = def;
    stats[Stat::Def] = atk;
}

/// Stockpile's `onStart` / `onRestart`: one more layer, `+1` Def and SpD,
/// remembering which stages actually rose.
fn stockpile_layer(battle: &mut Battle, pos: Position) -> bool {
    let mon = mon_mut(battle, pos);
    if mon.volatile_counters.stockpile_layers >= 3 {
        return false;
    }
    mon.volatile_counters.stockpile_layers += 1;
    let layers = mon.volatile_counters.stockpile_layers;
    let before = mon.boosts;
    start_message(battle, pos, ManipulationVolatile::Stockpile(layers), false);
    boosts::boost(battle, pos, StatBoosts::new([0, 1, 0, 1, 0, 0, 0]), Some(pos), false, false);
    let mon = mon_mut(battle, pos);
    if mon.boosts.def != before.def {
        mon.volatile_counters.stockpile_def -= 1;
    }
    if mon.boosts.spd != before.spd {
        mon.volatile_counters.stockpile_spd -= 1;
    }
    true
}

/// Showdown's `removeVolatile` for this module's volatiles, with their
/// `onEnd`. Returns whether it was present.
fn remove_volatile(battle: &mut Battle, pos: Position, flag: u128) -> bool {
    let Some(mon) = battle.state.at_mut(pos).filter(|m| !m.is_fainted()) else { return false };
    if !mon.volatiles.has(flag) {
        return false;
    }
    let counters = mon.volatile_counters;
    match flag {
        Volatiles::CHARGE => {
            let a = actor(battle, pos);
            let volatile = ManipulationVolatile::Charge;
            battle.emit(BattleEvent::ManipulationEnd { actor: a, volatile, silent: true });
        }
        Volatiles::STOCKPILE => {
            let (def, spd) = (counters.stockpile_def, counters.stockpile_spd);
            if def != 0 || spd != 0 {
                boosts::boost(battle, pos, StatBoosts::new([0, def, 0, spd, 0, 0, 0]), Some(pos), false, false);
            }
            let a = actor(battle, pos);
            let volatile = ManipulationVolatile::Stockpile(counters.stockpile_layers);
            battle.emit(BattleEvent::ManipulationEnd { actor: a, volatile, silent: false });
            let layers = -(counters.stockpile_layers as i8);
            if def != layers || spd != layers {
                battle.emit(BattleEvent::Hint { hint: crate::event::Hint::StockpileTracking });
            }
        }
        Volatiles::POWER_TRICK | Volatiles::POWER_SHIFT => {
            let a = actor(battle, pos);
            let volatile = if flag == Volatiles::POWER_TRICK {
                ManipulationVolatile::PowerTrick
            } else {
                ManipulationVolatile::PowerShift
            };
            battle.emit(BattleEvent::ManipulationEnd { actor: a, volatile, silent: false });
            swap_attack_defense(mon_mut(battle, pos));
        }
        _ => {}
    }
    let mon = mon_mut(battle, pos);
    mon.volatiles.clear(flag);
    match flag {
        Volatiles::ROOST => refresh_types(mon),
        Volatiles::STOCKPILE => {
            let c = &mut mon.volatile_counters;
            (c.stockpile_layers, c.stockpile_def, c.stockpile_spd) = (0, 0, 0);
        }
        Volatiles::LOCK_ON => mon.volatile_counters.lock_on = None,
        _ => {}
    }
    true
}

/// End-of-turn handlers (`fieldEvent('Residual')`): Aqua Ring (order 6)
/// and Ingrain (7) heal; Roost (25), Electrify and Lock-On count down
/// their `duration`.
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            let Some(mon) = battle.state.at(pos) else { continue };
            // None of the table's volatiles: nothing to collect.
            const ANY: u128 = Volatiles::AQUA_RING
                | Volatiles::INGRAIN
                | Volatiles::ROOST
                | Volatiles::ELECTRIFY
                | Volatiles::LOCK_ON;
            if !mon.volatiles.has(ANY) {
                continue;
            }
            let base = Handler {
                speed: mon.speed,
                sub_order: sub_order::CONDITION,
                holder: Holder::Pokemon(pos),
                ..Handler::default()
            };
            let v = mon.volatiles;
            // A `static`, read in place, not built for every Pokémon.
            static ENTRIES: [(u128, Option<u8>, HandlerFn); 5] = [
                (Volatiles::AQUA_RING, Some(6), aqua_ring_residual),
                (Volatiles::INGRAIN, Some(7), ingrain_residual),
                (Volatiles::ROOST, Some(25), end_residual),
                (Volatiles::ELECTRIFY, None, end_residual),
                (Volatiles::LOCK_ON, None, lock_on_residual),
            ];
            debug_assert_eq!(ENTRIES.iter().fold(0, |a, e| a | e.0), ANY);
            for &(flag, order, run) in &ENTRIES {
                if v.has(flag) {
                    handlers.push(Handler { volatile_bit: vol(flag), order, callback: Callback::Plain(run), ..base });
                }
            }
        }
    }
}

/// The handler's holder, if it still has the handler's volatile.
fn holder(battle: &Battle, handler: &Handler) -> Option<Position> {
    let Holder::Pokemon(pos) = handler.holder else { return None };
    alive(battle, pos).filter(|m| m.volatiles.has(handler.volatile())).map(|_| pos)
}

fn aqua_ring_residual(battle: &mut Battle, handler: &Handler) -> bool {
    if let Some(pos) = holder(battle, handler) {
        let max = mon_mut(battle, pos).max_hp() as u32;
        heal_from(battle, pos, super::hp_fraction(max, 1, 16), Some(ManipulationVolatile::AquaRing));
    }
    true
}

fn ingrain_residual(battle: &mut Battle, handler: &Handler) -> bool {
    if let Some(pos) = holder(battle, handler) {
        let max = mon_mut(battle, pos).max_hp() as u32;
        heal_from(battle, pos, super::hp_fraction(max, 1, 16), Some(ManipulationVolatile::Ingrain));
    }
    true
}

/// `duration: 1`: a one-turn volatile ends (silently). Returns false, like
/// every expiring duration (`run_residual` then skips `faintMessages`).
fn end_residual(battle: &mut Battle, handler: &Handler) -> bool {
    if let Some(pos) = holder(battle, handler) {
        remove_volatile(battle, pos, handler.volatile());
    }
    false
}

/// Lock-On's `duration: 2`. The countdown runs even at 0 HP (a faint not
/// yet reported), and its expiry skips `faintMessages`; a volatile already
/// gone is skipped too (its state is no longer the Pokémon's).
fn lock_on_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Pokemon(pos) = handler.holder else { return true };
    let Some(mon) = battle.state.at_mut(pos).filter(|m| m.volatiles.has(Volatiles::LOCK_ON)) else { return false };
    let counters = &mut mon.volatile_counters;
    counters.lock_on_turns = counters.lock_on_turns.saturating_sub(1);
    if counters.lock_on_turns == 0 {
        remove_volatile(battle, pos, Volatiles::LOCK_ON);
        return false;
    }
    true
}

/// `endTurn`, for each active Pokémon: `statsRaisedThisTurn` resets (not
/// before turn 1), and Gen 7+ reveals types that changed unseen
/// (`apparentType`).
pub(crate) fn end_turn(battle: &mut Battle, pos: Position) {
    let first = battle.state.turn == 1;
    // The Pokémon on the field, for pruning `attackedBy`.
    let mut active = 0u16;
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            if let Some(m) = battle.state.side(side).active(slot) {
                active |= super::power::attacker_bit(side, m.id);
            }
        }
    }
    let (side, slot) = (pos.side, pos.slot as usize);
    let Some(mon) = battle.state.side_mut(side).active_mut(slot) else { return };
    if !first {
        mon.volatile_counters.stats_raised_this_turn = false;
    }
    // `if (pokemon.getLastAttackedBy()) pokemon.knownType = true`,
    // then attackers that left the field drop out of `attackedBy`.
    if mon.volatile_counters.attacked_by != 0 {
        mon.volatile_counters.type_unknown = false;
    }
    mon.volatile_counters.attacked_by &= active;
    // An Illusion shows the types of the Pokémon it looks
    // like (`pokemon.illusion || pokemon`).
    if mon.illusion.is_some() {
        super::abilities::switchin::illusion_type_reveal(battle, pos);
        return;
    }
    // A Terastallized Pokémon keeps its `apparentType` (Transform copies it).
    if mon.terastallized {
        return;
    }
    let Some(apparent) = mon.volatile_counters.apparent_types.take() else { return };
    if apparent == mon.types {
        return;
    }
    // One reveal: `apparentType` now matches (the check in
    // `order::end_turn` is the same one).
    mon.volatile_counters.apparent_type_stale = false;
    let (types, added) = (mon.types, mon.volatile_counters.added_type);
    let target = actor(battle, Position::new(side, slot as u8));
    battle.emit(BattleEvent::TypeChange { target, types, silent: true });
    if let Some(ty) = added {
        battle.emit(BattleEvent::TypeAdd { target, ty, move_id: None });
    }
}

// ---------------------------------------------------------------------------
// Damage, accuracy and switching hooks.

/// Spit Up's `basePowerCallback`: 100 per Stockpile layer.
pub(crate) fn base_power(attacker: &PokemonState, move_id: MoveId) -> Option<u16> {
    (move_id == ids::SPIT_UP).then(|| 100 * attacker.volatile_counters.stockpile_layers as u16)
}

/// Charge's `onBasePower` (priority 9): Electric moves double.
pub(crate) fn charge_base_power(_: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    if ev.move_data().is_some_and(|d| d.ty == Type::Electric) {
        return ev.chain_modify(2, 1);
    }
    None
}

/// Focus Energy's `onModifyCritRatio` (+2) and Dragon Cheer's (+1, +2 for
/// a Dragon type when it started).
pub(crate) fn crit_ratio_handler(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    let mon = &battle.state.side(pos.side).team[pos.slot as usize];
    let bonus = match h.volatile() {
        Volatiles::FOCUS_ENERGY => 2,
        _ if mon.volatile_counters.dragon_cheer_dragon => 2,
        _ => 1,
    };
    Some(Relay::Num(ev.value() + bonus))
}

/// Minimize's `onSourceModifyDamage`: `minimize` moves deal double damage.
pub(crate) fn minimize_source_damage(_: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    if ev.move_data().is_some_and(|d| d.flags.contains(MoveFlags::MINIMIZE)) {
        return ev.chain_modify(2, 1);
    }
    None
}

/// Minimize's `onAccuracy`: a `minimize` move (Stomp, Body Slam, ...)
/// can't miss.
pub(crate) fn minimize_accuracy(_: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    if ev.move_data().is_some_and(|d| d.flags.contains(MoveFlags::MINIMIZE)) {
        return Some(Relay::True);
    }
    None
}

/// Lock-On's `onSourceAccuracy`: the holder's moves against the Pokémon it
/// locked on to can't miss.
pub(crate) fn lock_on_accuracy(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Holder::Pokemon(holder) = h.holder else { return None };
    let (source, target) = (ev.source?, ev.target_pos()?);
    let locked = battle.state.at(holder)?.volatile_counters.lock_on?;
    let target_id = battle.state.at(target)?.id;
    (ev.move_data().is_some() && source == holder && locked == (target.side, target_id)).then_some(Relay::True)
}

/// Tar Shot's `onEffectiveness` (priority -2): a Fire move is one step more
/// effective, once (on the target's first type).
pub(crate) fn tar_shot_effectiveness(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    if ev.move_data().is_none_or(|d| d.ty != Type::Fire) {
        return None;
    }
    let target = ev.target_pos()?;
    let first = battle.state.side(target.side).team.get(target.slot as usize)?.types.primary;
    if ev.ty != Some(first) {
        return None;
    }
    Some(Relay::Num(ev.value() + 1))
}

/// Ingrain's `onDragOut`: `-activate|...|move: Ingrain` and the Pokémon
/// stays (`null`).
pub(crate) fn ingrain_drag_out(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    let target = ev.target_pos()?;
    let a = actor(battle, target);
    battle.emit(BattleEvent::MoveActivate { actor: a, move_id: ids::INGRAIN, of: None });
    Some(super::events::Relay::Null)
}

/// `isGrounded`: Ingrain grounds the Pokémon.
pub(crate) fn grounded(mon: &PokemonState) -> bool {
    mon.volatiles.has(Volatiles::INGRAIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::BattleOptions;
    use crate::format::Format;
    use crate::rng::Seed;
    use crate::testing::sample_team;

    const P1: Position = Position::new(SideId::P1, 0);
    const P2: Position = Position::new(SideId::P2, 0);

    /// Singles, both leads Incineroar (Fire/Dark), no events so far.
    fn singles() -> Battle {
        let format = Format::gen9_singles();
        let teams = [sample_team(&format), sample_team(&format)];
        let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
        b.log.drain();
        b
    }

    #[test]
    fn soak_changes_types_until_switch_out() {
        let mut b = singles();
        assert_eq!(on_hit(&mut b, P2, P1, ids::SOAK), Some(Outcome::Succeeded));
        assert_eq!(b.state.at(P2).unwrap().types, TypeSet::mono(Type::Water));
        // Already pure Water: `-fail` on the target, then `null`.
        assert_eq!(on_hit(&mut b, P2, P1, ids::SOAK), Some(Outcome::Null));
        let mon = b.state.at_mut(P2).unwrap();
        mon.clear_volatile_state();
        assert_eq!(mon.types, mon.base_types);
    }

    #[test]
    fn burn_up_leaves_a_typeless_type() {
        let mut b = singles();
        // Incineroar (Fire/Dark) with Ghost added: `???/Dark`, and the
        // added type is gone.
        b.state.at_mut(P1).unwrap().volatile_counters.added_type = Some(Type::Ghost);
        self_on_hit(&mut b, P1, ids::BURN_UP);
        let mon = b.state.at(P1).unwrap();
        assert_eq!(mon.types, TypeSet::dual(Type::Unknown, Type::Dark));
        assert_eq!(mon.volatile_counters.added_type, None);
        assert!(!has_type(mon, Type::Fire));
        // `???` is neutral: Water hits only Dark now, neutrally.
        let data = *b.format.dex.move_(ids::WATER_GUN);
        assert_eq!(super::super::power::run_effectiveness(&mut b, P1, ids::WATER_GUN, &data), 0);
        assert!(matches!(
            b.log.drain().next_back(),
            Some(BattleEvent::TypeChangeFrom { types, .. }) if types == TypeSet::dual(Type::Unknown, Type::Dark)
        ));
        // Again: nothing left to remove, but the line comes anyway.
        self_on_hit(&mut b, P1, ids::BURN_UP);
        assert_eq!(b.state.at(P1).unwrap().types, TypeSet::dual(Type::Unknown, Type::Dark));
        assert_eq!(b.log.drain().len(), 1);
        // Terastallized: `setType` refuses, the line shows the Tera type.
        let mon = b.state.at_mut(P2).unwrap();
        mon.tera_type = Type::Fire;
        mon.terastallized = true;
        refresh_types(mon);
        self_on_hit(&mut b, P2, ids::BURN_UP);
        assert_eq!(b.state.at(P2).unwrap().types, TypeSet::mono(Type::Fire));
        assert!(matches!(
            b.log.drain().next_back(),
            Some(BattleEvent::TypeChangeFrom { types, .. }) if types == TypeSet::mono(Type::Fire)
        ));
    }

    #[test]
    fn added_types_and_tar_shot_change_effectiveness() {
        let mut b = singles();
        let eff = |b: &mut Battle, id: MoveId| {
            let data = *b.format.dex.move_(id);
            Effectiveness::Exp(super::super::power::run_effectiveness(b, P2, id, &data))
        };
        assert_eq!(eff(&mut b, ids::WATER_GUN), Effectiveness::Exp(1));
        assert_eq!(on_hit(&mut b, P2, P1, ids::FORESTS_CURSE), Some(Outcome::Succeeded));
        assert!(has_type(b.state.at(P2).unwrap(), Type::Grass));
        // Fire/Dark/Grass: Water is neutral, Flying super effective.
        assert_eq!(eff(&mut b, ids::WATER_GUN), Effectiveness::Exp(0));
        assert_eq!(eff(&mut b, ids::GUST), Effectiveness::Exp(1));
        b.state.at_mut(P2).unwrap().volatiles.add(Volatiles::TAR_SHOT);
        assert_eq!(eff(&mut b, ids::EMBER), Effectiveness::Exp(1));
        // Psychic can't hit the Dark type.
        assert!(type_immune(&b, P2, Type::Psychic));
    }

    #[test]
    fn roost_hides_flying_for_the_turn() {
        let mut mon = PokemonState { base_types: TypeSet::dual(Type::Normal, Type::Flying), ..PokemonState::default() };
        refresh_types(&mut mon);
        assert_eq!(mon.types, TypeSet::dual(Type::Normal, Type::Flying));
        mon.volatiles.add(Volatiles::ROOST);
        refresh_types(&mut mon);
        assert_eq!(mon.types, TypeSet::mono(Type::Normal));
        mon.base_types = TypeSet::mono(Type::Flying);
        refresh_types(&mut mon);
        assert_eq!(mon.types, TypeSet::mono(Type::Normal));
    }

    #[test]
    fn power_trick_and_splits_revert_on_switch_out() {
        let mut b = singles();
        let before = b.state.at(P1).unwrap().stats;
        assert_eq!(add_volatile(&mut b, P1, "powertrick"), Some(true));
        let mon = b.state.at(P1).unwrap();
        assert_eq!((mon.stats[Stat::Atk], mon.stats[Stat::Def]), (before[Stat::Def], before[Stat::Atk]));
        // Using it again ends it and swaps back.
        assert_eq!(add_volatile(&mut b, P1, "powertrick"), Some(true));
        assert_eq!(b.state.at(P1).unwrap().stats, before);
        on_hit(&mut b, P2, P1, ids::GUARD_SPLIT);
        let mon = b.state.at_mut(P1).unwrap();
        mon.clear_volatile_state();
        assert_eq!(mon.stats, before);
    }

    #[test]
    fn stockpile_layers_and_spit_up() {
        let mut b = singles();
        for layers in 1..=3 {
            assert_eq!(add_volatile(&mut b, P1, "stockpile"), Some(true));
            assert_eq!(b.state.at(P1).unwrap().volatile_counters.stockpile_layers, layers);
        }
        assert_eq!(add_volatile(&mut b, P1, "stockpile"), Some(false));
        assert_eq!(on_try(&mut b, P1, ids::STOCKPILE), Outcome::Failed);
        assert_eq!(base_power(b.state.at(P1).unwrap(), ids::SPIT_UP), Some(300));
        let mon = b.state.at(P1).unwrap();
        assert_eq!((mon.boosts.def, mon.boosts.spd), (3, 3));
        after_move(&mut b, P1, ids::SPIT_UP);
        let mon = b.state.at(P1).unwrap();
        assert_eq!((mon.boosts.def, mon.boosts.spd), (0, 0));
        assert!(!mon.volatiles.has(Volatiles::STOCKPILE));
        assert_eq!(on_try(&mut b, P1, ids::SPIT_UP), Outcome::Failed);
    }

    #[test]
    fn pain_split_averages_hp() {
        let mut b = singles();
        b.state.at_mut(P1).unwrap().hp = 10;
        let full = b.state.at(P2).unwrap().hp;
        pain_split(&mut b, P2, P1);
        let avg = (full + 10) / 2;
        assert_eq!((b.state.at(P1).unwrap().hp, b.state.at(P2).unwrap().hp), (avg, avg));
        let events = b.log.events();
        assert!(matches!(events, [BattleEvent::SetHp { silent: true, .. }, BattleEvent::SetHp { silent: false, .. }]));
    }
}
