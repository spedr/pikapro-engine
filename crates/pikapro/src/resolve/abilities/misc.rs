//! Residual, priority, accuracy, KO and miscellaneous abilities.
//!
//! Showdown references: the abilities in `data/abilities.ts` that
//! `abilities::owner` assigns to [`Owner::Misc`](super::Owner::Misc)
//! (Speed Boost, Prankster, Moxie, No Guard,
//! Magic Guard, the trapping abilities, Emergency Exit, Protean, ...), and
//! the Champions mod's Healer (`data/mods/champions/abilities.ts`).
//!
//! Adding an ability: see `abilities/mod.rs`. One arm per callback in
//! [`run`].
//!
//! Some of these abilities are checked by the engine where Showdown checks
//! them rather than through a callback; the hooks below are called there:
//! - [`prankster_immunity`] (`hitStepTryImmunity`) and
//!   [`begin_move_action`] (`move.pranksterBoosted`, from `runMove`);
//! - [`tracks_target`] (`getTarget`: Stalwart, Propeller Tail);
//! - [`early_bird`] (`slp`'s `onBeforeMove`).
//!
//! Scarlet/Violet's Run Away and Honey Gather have no effect in a battle
//! between trainers.

use pikapro_data::gen9::{abilities as a, items as i, moves as m};
use pikapro_data::{AbilityFlags, AbilityId, BoostId, Event, EventPrefix, MoveCategory, MoveFlags, MoveId, MoveTarget};
use pikapro_data::{Stat, StatBoosts, Type};

use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName, LineArg};
use crate::format::Mechanics;
use crate::resolve::boosts::one_boost;
use crate::resolve::events::{self, Boosts, Condition, Effect, EventCtx, Relay};
use crate::resolve::{actor, boosts, damage_by, field, heal_by, hp_fraction, manipulation, status, switching};
use crate::state::{Position, Status, SwitchFlag, Volatiles, Weather};

use super::has_ability;

/// Runs `ability`'s callback for `ev.event` / `ev.prefix` on its holder.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, ability: AbilityId) -> Option<Relay> {
    use EventPrefix::{Ally, Any, Foe, On, Source};
    match (ability, ev.event, ev.prefix) {
        // -------------------------------------------------------------------
        // Accuracy and evasion.

        // `onSourceModifyAccuracy` (priority -1): 5325/4096.
        (a::COMPOUND_EYES, Event::ModifyAccuracy, Source) => {
            if matches!(ev.relay, Relay::Num(_)) {
                ev.chain(5325);
            }
            None
        }
        // `onModifyAccuracy` (priority -1): 3277/4096 in sand / snow.
        (a::SAND_VEIL | a::SNOW_CLOAK, Event::ModifyAccuracy, On) => {
            let weather = if ability == a::SAND_VEIL { Weather::Sand } else { Weather::Snow };
            if matches!(ev.relay, Relay::Num(_)) && super::effective_weather(battle) == Some(weather) {
                ev.chain(3277);
            }
            None
        }
        // `onModifyAccuracy` (priority -1): half against a confused holder.
        (a::TANGLED_FEET, Event::ModifyAccuracy, On) => {
            let confused = battle.state.at(holder).is_some_and(|m| m.volatiles.has(Volatiles::CONFUSION));
            if matches!(ev.relay, Relay::Num(_)) && confused {
                ev.chain_modify(1, 2);
            }
            None
        }
        // `onModifyAccuracy` (priority 10): a status move's accuracy is 50.
        (a::WONDER_SKIN, Event::ModifyAccuracy, On) => {
            let status = ev.move_data().is_some_and(|d| d.category == MoveCategory::Status);
            (status && matches!(ev.relay, Relay::Num(_))).then_some(Relay::Num(50))
        }
        // `onAnyModifyAccuracy` (priority -1): 4506/4096 for the holder's
        // and its allies' moves.
        (a::VICTORY_STAR, Event::ModifyAccuracy, Any) => {
            if ev.source.is_some_and(|s| s.side == holder.side) && matches!(ev.relay, Relay::Num(_)) {
                ev.chain(4506);
            }
            None
        }
        // `onAnyInvulnerability` (priority 1): a move by or against the
        // holder reaches a semi-invulnerable target (`0`).
        (a::NO_GUARD, Event::Invulnerability, Any) => no_guard(ev, holder).then_some(Relay::Num(0)),
        // `onAnyAccuracy`: a move by or against the holder can't miss.
        (a::NO_GUARD, Event::Accuracy, Any) => no_guard(ev, holder).then_some(Relay::True),
        // `onTryBoost`: other Pokémon can't lower its accuracy.
        (a::ILLUMINATE, Event::TryBoost, On) => illuminate_try_boost(battle, ev, holder),
        // `onModifyMove`: the target's evasion is ignored.
        (a::ILLUMINATE, Event::ModifyMove, On) => {
            battle.move_state.ignore_evasion = true;
            None
        }

        // -------------------------------------------------------------------
        // Priority.

        // `onModifyPriority`: +1 for a Flying move at full HP.
        (a::GALE_WINGS, Event::ModifyPriority, On) => {
            let flying = ev.move_data().is_some_and(|d| d.ty == Type::Flying);
            let full = battle.state.at(holder).is_some_and(|m| m.hp == m.max_hp());
            (flying && full).then(|| Relay::Num(ev.value() + 1))
        }
        // `onModifyPriority`: +1 for a status move (`move.pranksterBoosted`:
        // see [`begin_move_action`]).
        (a::PRANKSTER, Event::ModifyPriority, On) => {
            let status = ev.move_data().is_some_and(|d| d.category == MoveCategory::Status);
            status.then(|| Relay::Num(ev.value() + 1))
        }
        // `onModifyPriority`: +3 for a healing move.
        (a::TRIAGE, Event::ModifyPriority, On) => {
            let heal = ev.move_data().is_some_and(|d| d.flags.contains(MoveFlags::HEAL));
            heal.then(|| Relay::Num(ev.value() + 3))
        }
        // `onFractionalPriority` (priority -1): a damaging move goes first
        // in its bracket 30% of the time (`0.1`, in tenths here). Recharging
        // (no move data: no category) counts.
        (a::QUICK_DRAW, Event::FractionalPriority, On) => {
            let damaging = ev.move_data().is_none_or(|d| d.category != MoveCategory::Status);
            if damaging && battle.rng.chance(3, 10) {
                super::activate(battle, holder, ability);
                return Some(Relay::Num(1));
            }
            None
        }
        // `onFractionalPriority: -0.1`.
        (a::STALL, Event::FractionalPriority, On) => Some(Relay::Num(-1)),
        // `onFractionalPriority` (priority -1): status moves go last in
        // their bracket; `onModifyMove`: and ignore abilities.
        (a::MYCELIUM_MIGHT, Event::FractionalPriority, On) => {
            let status = ev.move_data().is_some_and(|d| d.category == MoveCategory::Status);
            status.then_some(Relay::Num(-1))
        }
        (a::MYCELIUM_MIGHT, Event::ModifyMove, On) => {
            if ev.move_data().is_some_and(|d| d.category == MoveCategory::Status) {
                battle.move_state.ignore_ability = true;
            }
            None
        }
        // `onModifyMove` (priority 1): `move.tracksTarget` (no redirection;
        // `getTarget` is [`tracks_target`]).
        (a::STALWART | a::PROPELLER_TAIL, Event::ModifyMove, On) => {
            battle.move_state.tracks_target = ev.move_data().is_some_and(|d| d.target != MoveTarget::Scripted);
            None
        }

        // -------------------------------------------------------------------
        // End of the turn.

        // `onResidual` (order 28, sub-order 2): +1 Speed after the first turn.
        (a::SPEED_BOOST, Event::Residual, On) => {
            if battle.state.at(holder).is_some_and(|m| m.active_turns > 0) {
                let spe = one_boost(BoostId::Spe, 1);
                boosts::boost_from(battle, holder, spe, ev.source, Effect::Ability(ability), false, false);
            }
            None
        }
        (a::MOODY, Event::Residual, On) => moody(battle, holder),
        (a::HEALER, Event::Residual, On) => healer(battle, holder),
        // `onResidual` (order 5, sub-order 3): cured in rain.
        (a::HYDRATION, Event::Residual, On) => {
            let statused = battle.state.at(holder).is_some_and(|m| m.status != Status::None);
            let rain = matches!(super::pokemon_weather(battle, holder), Some(Weather::Rain | Weather::HeavyRain));
            if statused && rain {
                super::activate(battle, holder, ability);
                status::cure_status(battle, holder);
            }
            None
        }
        // `onResidual` (order 5, sub-order 3): cured 33% of the time.
        (a::SHED_SKIN, Event::Residual, On) => {
            let statused = battle.state.at(holder).is_some_and(|m| m.hp > 0 && m.status != Status::None);
            if statused && battle.rng.chance(33, 100) {
                super::activate(battle, holder, ability);
                status::cure_status(battle, holder);
            }
            None
        }
        // `onResidual` (order 28, sub-order 2): sleeping foes lose 1/8.
        (a::BAD_DREAMS, Event::Residual, On) => {
            if battle.state.at(holder).is_none_or(|m| m.hp == 0) {
                return None;
            }
            for foe in super::foes(battle, holder).iter().copied() {
                let Some(mon) = battle.state.at(foe) else { continue };
                if matches!(mon.status, Status::Sleep { .. }) || has_ability(battle, foe, a::COMATOSE) {
                    let amount = hp_fraction(u32::from(mon.max_hp()), 1, 8);
                    damage_by(battle, foe, amount, Some(holder), Effect::Ability(ability));
                }
            }
            None
        }
        // `onWeather`: heals 1/16 in snow.
        (a::ICE_BODY, Event::Weather, On) => {
            if ev.effect == Effect::Weather(Weather::Snow) {
                let amount = hp_fraction(u32::from(battle.state.at(holder)?.max_hp()), 1, 16);
                heal_by(battle, holder, amount, None, Effect::Ability(ability));
            }
            None
        }
        // `onWeather`: heals 1/16 in rain (not under Utility Umbrella).
        (a::RAIN_DISH, Event::Weather, On) => {
            let Effect::Weather(weather) = ev.effect else { return None };
            if super::pokemon_weather(battle, holder) == Some(weather)
                && matches!(weather, Weather::Rain | Weather::HeavyRain)
            {
                let amount = hp_fraction(u32::from(battle.state.at(holder)?.max_hp()), 1, 16);
                heal_by(battle, holder, amount, None, Effect::Ability(ability));
            }
            None
        }
        // `onImmunity`: no sandstorm damage; Ice Body's and Snow Cloak's
        // `hail` immunity has nothing to block in Gen 9.
        (a::SAND_VEIL, Event::Immunity, On) => (ev.relay == Relay::Id("sandstorm")).then_some(Relay::False),
        (a::SNOW_CLOAK | a::ICE_BODY, Event::Immunity, On) => (ev.relay == Relay::Id("hail")).then_some(Relay::False),

        // -------------------------------------------------------------------
        // Damage.

        // `onDamage`: only moves damage the holder; an ability's damage is
        // announced with `-activate|<source>|ability: X`.
        (a::MAGIC_GUARD, Event::Damage, On) => {
            if ev.effect.is_move() || ev.effect == Effect::None {
                return None;
            }
            if let (Effect::Ability(x), Some(source)) = (ev.effect, ev.source) {
                super::activate(battle, source, x);
            }
            Some(Relay::False)
        }
        // `onDamage` (priority 1): poison heals 1/8 instead.
        (a::POISON_HEAL, Event::Damage, On) => {
            if !matches!(ev.effect, Effect::Status(pikapro_data::StatusId::Poison | pikapro_data::StatusId::Toxic)) {
                return None;
            }
            let amount = hp_fraction(u32::from(battle.state.at(holder)?.max_hp()), 1, 8);
            heal_by(battle, holder, amount, None, Effect::Ability(ability));
            Some(Relay::False)
        }
        // `onDamage`: no recoil (Struggle's is `strugglerecoil`).
        (a::ROCK_HEAD, Event::Damage, On) => (ev.effect == Effect::Condition(Condition::Recoil)).then_some(Relay::Null),
        // `onSourceTryHeal`: draining the holder hurts instead.
        (a::LIQUID_OOZE, Event::TryHeal, Source) => {
            let oozes = match ev.effect {
                Effect::Condition(Condition::Drain) => true,
                Effect::Volatile(bit) => 1u128 << bit == Volatiles::LEECH_SEED,
                Effect::Move(mv) => mv == m::STRENGTH_SAP,
                _ => false,
            };
            if !oozes {
                return None;
            }
            let target = ev.target_pos()?;
            let amount = ev.value().max(0) as u32;
            damage_by(battle, target, amount, Some(holder), Effect::Ability(ability));
            Some(Relay::Num(0))
        }

        // -------------------------------------------------------------------
        // Knock-outs.

        // `onSourceAfterFaint`: +1 Attack (Special Attack, the best stat)
        // per Pokémon a move knocked out.
        (a::MOXIE | a::CHILLING_NEIGH | a::GRIM_NEIGH | a::BEAST_BOOST, Event::AfterFaint, Source) => {
            if !ev.effect.is_move() {
                return None;
            }
            let stat = match ability {
                a::GRIM_NEIGH => BoostId::SpA,
                a::BEAST_BOOST => super::best_stat(battle, holder, true),
                _ => BoostId::Atk,
            };
            let amount = ev.value().clamp(0, 6) as i8;
            boosts::boost_from(
                battle,
                holder,
                one_boost(stat, amount),
                ev.source,
                Effect::Ability(ability),
                false,
                false,
            );
            None
        }
        // `onAnyFaint` (priority 1): +1 Special Attack.
        (a::SOUL_HEART, Event::Faint, Any) => {
            let spa = one_boost(BoostId::SpA, 1);
            boosts::boost_from(battle, holder, spa, ev.source, Effect::Ability(ability), false, false);
            None
        }
        // `onAllyFaint`: takes the fainted ally's ability.
        (a::RECEIVER | a::POWER_OF_ALCHEMY, Event::Faint, Ally) => {
            let fainted = ev.target_pos()?;
            if battle.state.at(holder).is_none_or(|m| m.hp == 0) {
                return None;
            }
            let taken = battle.state.side(fainted.side).team[fainted.slot as usize].ability;
            let flags = battle.format.dex.ability(taken).flags;
            if flags.contains(AbilityFlags::NO_RECEIVER) || taken == a::NO_ABILITY {
                return None;
            }
            let effect = Some(EffectName::Ability(ability));
            super::set_ability(battle, holder, taken, Some(fainted), effect, super::SetAbilityKind::Normal);
            None
        }

        // -------------------------------------------------------------------
        // Trapping (`endTurn`'s `TrapPokemon`; `maybeTrapped` is not
        // modelled, so the `MaybeTrapPokemon` handlers only take part in the
        // sort).

        // `onFoeTrapPokemon`: adjacent foes without Shadow Tag.
        (a::SHADOW_TAG, Event::TrapPokemon, Foe) => {
            let foe = ev.target_pos()?;
            if !has_ability(battle, foe, a::SHADOW_TAG) && adjacent(battle, foe, holder) {
                events::try_trap(battle, foe);
            }
            None
        }
        // `onFoeTrapPokemon`: adjacent grounded foes.
        (a::ARENA_TRAP, Event::TrapPokemon, Foe) => {
            let foe = ev.target_pos()?;
            if adjacent(battle, foe, holder) && field::grounded(battle, foe, false) == Some(true) {
                events::try_trap(battle, foe);
            }
            None
        }
        // `onFoeTrapPokemon`: adjacent Steel-type foes.
        (a::MAGNET_PULL, Event::TrapPokemon, Foe) => {
            let foe = ev.target_pos()?;
            let steel = battle.state.at(foe).is_some_and(|m| manipulation::has_type(m, Type::Steel));
            if steel && adjacent(battle, foe, holder) {
                events::try_trap(battle, foe);
            }
            None
        }
        (a::SHADOW_TAG | a::ARENA_TRAP | a::MAGNET_PULL, Event::MaybeTrapPokemon, Foe) => None,
        // Champions' Run Away (`data/mods/champions/abilities.ts`; only its
        // dex gives it these handlers): like Shed Shell, `onTrapPokemon`
        // (priority -10) sets `trapped = false`. `maybeTrapped` isn't
        // modelled: the `onMaybeTrapPokemon` arm only takes part in the
        // handler sort.
        (a::RUN_AWAY, Event::TrapPokemon, On) => {
            events::release_trap(battle, holder);
            None
        }
        (a::RUN_AWAY, Event::MaybeTrapPokemon, On) => None,

        // -------------------------------------------------------------------
        // Switching out.

        // `onEmergencyExit`: at half HP, the holder switches out (every
        // other switch flag is dropped). Champions' override
        // (`data/mods/champions/abilities.ts`) sets only the
        // holder's flag, so a U-turn user still switches too.
        (a::EMERGENCY_EXIT | a::WIMP_OUT, Event::EmergencyExit, On) => {
            let mon = battle.state.at(holder)?;
            if switching::can_switch(battle, holder.side) == 0
                || mon.force_switch_flag
                || mon.switch_flag != SwitchFlag::None
            {
                return None;
            }
            let champions = battle.format.mechanics == Mechanics::Champions;
            for side in crate::state::SideId::BOTH.into_iter().filter(|_| !champions) {
                let s = battle.state.side_mut(side);
                let n = battle.format.active_per_side().min(s.team.len());
                for mon in s.team[..n].iter_mut() {
                    mon.switch_flag = SwitchFlag::None;
                }
            }
            // Showdown's `switchFlag = true` (no move to name), stored as
            // `Fainted`: the same `true` as a faint's or an Eject Button's
            // `SwitchFlag::True` (`SwitchFlag::is_true`).
            battle.state.at_mut(holder)?.switch_flag = SwitchFlag::Fainted;
            super::activate(battle, holder, ability);
            None
        }

        // -------------------------------------------------------------------
        // Type change.

        // `onPrepareHit`: once per switch-in, the user becomes the type of
        // the move it is about to use.
        (a::PROTEAN | a::LIBERO, Event::PrepareHit, On) => protean(battle, ev, holder, ability),

        // -------------------------------------------------------------------
        // Opportunist.

        // `onFoeAfterBoost`: remembers the foe's raises.
        (a::OPPORTUNIST, Event::AfterBoost, Foe) => {
            if matches!(ev.effect, Effect::Ability(a::OPPORTUNIST) | Effect::Item(i::MIRROR_HERB)) {
                return None;
            }
            let Relay::Boosts(boost) = ev.relay else { return None };
            let mon = battle.state.at_mut(holder)?;
            let mut stored = mon
                .volatile_counters
                .ability_state
                .opportunist
                .unwrap_or(StatBoosts { stages: [0; 7], order: BoostId::ALL });
            let mut known = Boosts { changes: stored, keys: stored_keys(&stored) };
            for (id, n) in boost.iter() {
                if n > 0 {
                    let total = known.get(id).unwrap_or(0).saturating_add(n);
                    known.set(id, total);
                }
            }
            stored = known.changes;
            mon.volatile_counters.ability_state.opportunist = Some(stored);
            None
        }
        // `onAnySwitchIn` (priority -3), `onAnyAfterMega`, `onAnyAfterMove`,
        // `onAnyAfterTerastallization`, `onResidual` (order 29): copies them.
        (a::OPPORTUNIST, Event::SwitchIn | Event::AfterMega | Event::AfterMove | Event::AfterTerastallization, Any)
        | (a::OPPORTUNIST, Event::Residual, On) => {
            let stored = battle.state.at_mut(holder)?.volatile_counters.ability_state.opportunist.take()?;
            boosts::boost_from(battle, holder, stored, ev.source, Effect::Ability(ability), false, false);
            None
        }
        (a::OPPORTUNIST, Event::End, On) => {
            if let Some(mon) = battle.state.side_mut(holder.side).team.get_mut(holder.slot as usize) {
                mon.volatile_counters.ability_state.opportunist = None;
            }
            None
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hooks at Showdown's call sites.

/// `runMove` from a move action: records Showdown's `move.pranksterBoosted`
/// for the move the Pokémon at `user` chose (`move_id`, before Encore
/// replaces it). It was set by Prankster's `onModifyPriority` when the
/// action's priority was last computed (right before the action), which
/// happens exactly when the holder's Prankster works and the move is a
/// status move. Moves the Pokémon calls (Sleep Talk, Metronome) inherit it
/// (`useMove`); nobody else's move does.
pub(crate) fn begin_move_action(battle: &mut Battle, user: Position, move_id: MoveId) {
    let boosted = battle.format.dex.moves.get(move_id.0 as usize).is_some_and(|d| d.category == MoveCategory::Status)
        && has_ability(battle, user, a::PRANKSTER);
    for side in crate::state::SideId::BOTH {
        for mon in battle.state.side_mut(side).team.iter_mut() {
            mon.prankster_boosted = false;
        }
    }
    if let Some(mon) = battle.state.at_mut(user) {
        mon.prankster_boosted = boosted;
    }
}

/// `hitStepTryImmunity`'s Prankster check (Gen 7+): a move boosted by the
/// user's Prankster fails against a Dark-type foe (`-immune`). Returns
/// whether the target can be hit.
pub(crate) fn prankster_immunity(
    battle: &mut Battle,
    target: Position,
    user: Position,
    status: Option<pikapro_data::StatusId>,
) -> bool {
    let boosted = battle.state.at(user).is_some_and(|m| m.prankster_boosted);
    if !boosted || target.side == user.side || !has_ability(battle, user, a::PRANKSTER) {
        return true;
    }
    let Some(m) = battle.state.at(target).filter(|m| manipulation::has_type(m, Type::Dark)) else { return true };
    // The hint, unless the move's status could not affect the target
    // anyway (`dex.getImmunity(move.status, target)`), which Illusion hides.
    use pikapro_data::StatusId;
    let status_immune = status.is_some_and(|s| {
        let types: &[Type] = match s {
            StatusId::Burn => &[Type::Fire],
            StatusId::Paralysis => &[Type::Electric],
            StatusId::Freeze => &[Type::Ice],
            StatusId::Poison | StatusId::Toxic => &[Type::Poison, Type::Steel],
            StatusId::Sleep => &[],
        };
        types.iter().any(|&t| manipulation::has_type(m, t))
    });
    if m.illusion.is_some() || !status_immune {
        battle.hint(crate::event::Hint::DarkPrankster);
    }
    let t = actor(battle, target);
    battle.emit(BattleEvent::Immune { target: t });
    false
}

/// `getTarget`: a Pokémon with Stalwart or Propeller Tail follows its
/// original target (`tracksTarget`).
pub(crate) fn tracks_target(battle: &Battle, user: Position) -> bool {
    has_ability(battle, user, a::STALWART) || has_ability(battle, user, a::PROPELLER_TAIL)
}

/// Dancer, after `runMove`'s `AfterMove` events: when a dance move (the
/// active move, which may have been called) did something, every other
/// active Pokémon with Dancer that is not semi-invulnerable copies it,
/// slowest (stored Speed) first and, on ties, the latest to gain its
/// ability first. Before each, faints are reported (the battle may end);
/// a fainted Dancer is skipped. Each announces `-activate|<dancer>|ability:
/// Dancer` and uses the move as an `externalMove` (`[from] ability:
/// Dancer`), aimed at the original target if that is its foe and the user
/// its ally, otherwise at the user.
pub(crate) fn dancer(battle: &mut Battle, user: Position, did_something: bool) {
    let Some((_, move_id)) = battle.active_move else { return };
    let dance = battle.format.dex.moves.get(move_id.0 as usize).is_some_and(|d| d.flags.contains(MoveFlags::DANCE));
    if !dance || !did_something {
        return;
    }
    let mut dancers = [(Position::new(crate::state::SideId::P1, 0), 0u16, 0u16); 4];
    let mut n = 0;
    for p in super::all_active(battle) {
        if p != user && has_ability(battle, p, a::DANCER) && !crate::resolve::multiturn::is_semi_invulnerable(battle, p)
        {
            let mon = &battle.state.side(p.side).team[p.slot as usize];
            dancers[n] = (p, mon.stats[Stat::Spe], mon.volatile_counters.ability_order);
            n += 1;
        }
    }
    if n == 0 {
        return;
    }
    // `dancers.sort(...)` (stable): Speed ascending, then `effectOrder`
    // descending.
    let dancers = &mut dancers[..n];
    dancers.sort_by(|x, y| x.1.cmp(&y.1).then(y.2.cmp(&x.2)));
    let first_target = battle.move_state.active_target.unwrap_or(user);
    for &(dancer, _, _) in dancers.iter() {
        // Showdown BattleActions#runMove checks faintMessages' return
        // value here. Once the battle has already ended, faintMessages
        // returns undefined, so a remaining Dancer still runs (and can
        // log a failed copy after the win line).
        if !battle.is_over() && crate::resolve::faint_messages(battle, true) {
            break;
        }
        if battle.state.at(dancer).is_none_or(|m| m.is_fainted()) {
            continue;
        }
        super::activate(battle, dancer, a::DANCER);
        let target = if first_target.side != dancer.side && user.side == dancer.side { first_target } else { user };
        let loc = crate::resolve::targeting::loc_of(dancer, target);
        let options = crate::resolve::order::RunMoveOptions { external: true, ..Default::default() };
        crate::resolve::moves::run_move(battle, dancer, move_id, loc, options);
    }
}

/// `slp`'s `onBeforeMove`: Early Bird counts the sleep down twice. Returns
/// the extra turns to count down.
pub(crate) fn early_bird(battle: &Battle, pos: Position) -> u8 {
    u8::from(has_ability(battle, pos, a::EARLY_BIRD))
}

// ---------------------------------------------------------------------------
// Callbacks.

/// No Guard's condition: the move is used by or against the holder.
fn no_guard(ev: &EventCtx<'_>, holder: Position) -> bool {
    ev.effect.is_move() && (ev.source == Some(holder) || ev.target_pos() == Some(holder))
}

/// Illuminate's `onTryBoost`: an accuracy drop from another Pokémon is
/// removed, with `-fail|<holder>|unboost|accuracy|[from] ability:
/// Illuminate|[of] <holder>` unless the effect is a move with secondaries.
fn illuminate_try_boost(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    if ev.source.is_some() && ev.source == ev.target_pos() {
        return None;
    }
    let Relay::Boosts(mut boost) = ev.relay else { return None };
    if boost.get(BoostId::Accuracy).is_none_or(|n| n >= 0) {
        return None;
    }
    boost.delete(BoostId::Accuracy);
    if !events::effect_has_secondaries(battle, ev.effect) {
        let target = actor(battle, holder);
        battle.emit(BattleEvent::FailFrom {
            target,
            what: LineArg::Text("unboost"),
            detail: Some(LineArg::Text("accuracy")),
            cause: Cause::ability(a::ILLUMINATE).of(target),
        });
    }
    Some(Relay::Boosts(boost))
}

/// Moody's `onResidual` (order 28, sub-order 2): a random stat below +6
/// rises by 2 (`sample`), then another one above -6 falls by 1 (`sample`);
/// accuracy and evasion are left out.
fn moody(battle: &mut Battle, holder: Position) -> Option<Relay> {
    const STATS: [BoostId; 5] = [BoostId::Atk, BoostId::Def, BoostId::SpA, BoostId::SpD, BoostId::Spe];
    let stages = battle.state.at(holder)?.boosts;
    let pick = |battle: &mut Battle, keep: &dyn Fn(BoostId) -> bool| {
        let mut list = [BoostId::Atk; 5];
        let mut n = 0;
        for s in STATS.into_iter().filter(|&s| keep(s)) {
            list[n] = s;
            n += 1;
        }
        (n > 0).then(|| *battle.rng.sample(&list[..n]))
    };
    let plus = pick(battle, &|s| stages.get(s) < 6);
    let minus = pick(battle, &|s| stages.get(s) > -6 && Some(s) != plus);
    let mut boost = Boosts { changes: StatBoosts::default(), keys: 0 };
    if let Some(s) = plus {
        boost.set(s, 2);
    }
    if let Some(s) = minus {
        boost.set(s, -1);
    }
    boosts::boost_from(battle, holder, boost.changes, Some(holder), Effect::Ability(a::MOODY), false, false);
    None
}

/// Healer's `onResidual` (order 5, sub-order 3): each adjacent ally with a
/// status is cured 30% of the time (50% in the Champions mod), announced by
/// `-activate|<holder>|ability: Healer`.
fn healer(battle: &mut Battle, holder: Position) -> Option<Relay> {
    let (num, den) = if battle.format.mechanics == Mechanics::Champions { (1, 2) } else { (3, 10) };
    let mut allies = crate::util::StackVec::<Position, 2>::new();
    for slot in 0..battle.format.active_per_side() {
        let p = Position::new(holder.side, slot as u8);
        if p != holder && battle.state.at(p).is_some_and(|m| m.hp > 0) && adjacent(battle, p, holder) {
            allies.push(p);
        }
    }
    for ally in allies.iter().copied() {
        let statused = battle.state.at(ally).is_some_and(|m| m.status != Status::None);
        if statused && battle.rng.chance(num, den) {
            super::activate(battle, holder, a::HEALER);
            status::cure_status(battle, ally);
        }
    }
    None
}

/// Protean's and Libero's `onPrepareHit`: unless the type already changed
/// since switching in, or the move was bounced, is a future move or calls
/// another move, the user whose types are not exactly the move's type
/// becomes that type (`setType`, which fails once terastallized):
/// `-start|<user>|typechange|<type>|[from] ability: X`.
fn protean(battle: &mut Battle, ev: &EventCtx<'_>, holder: Position, ability: AbilityId) -> Option<Relay> {
    let mon = battle.state.at(holder)?;
    if mon.volatile_counters.ability_state.type_changed {
        return None;
    }
    let data = ev.move_data()?;
    let move_id = ev.effect.move_id()?;
    // Struggle's type is `???`; a move bounced by Magic Bounce
    // (`move.hasBounced`) doesn't count.
    if data.flags.contains(MoveFlags::FUTURE_MOVE)
        || data.unmodelled.contains(&"callsMove")
        || move_id == m::STRUGGLE
        || battle.move_state.has_bounced
    {
        return None;
    }
    // `type !== '???'`: a typeless Revelation Dance.
    let ty = data.ty;
    if ty == pikapro_data::Type::Unknown || manipulation::is_exactly(mon, ty) {
        return None;
    }
    if !manipulation::set_type(battle, holder, pikapro_data::TypeSet::mono(ty)) {
        return None;
    }
    battle.state.at_mut(holder)?.volatile_counters.ability_state.type_changed = true;
    let target = actor(battle, holder);
    battle.emit(BattleEvent::TypeChangeFrom {
        target,
        types: pikapro_data::TypeSet::mono(ty),
        cause: Cause::ability(ability),
    });
    None
}

// ---------------------------------------------------------------------------
// Helpers.

/// Showdown's `pokemon.isAdjacent(other)` for two active Pokémon in singles
/// or doubles: any two different ones with HP left.
fn adjacent(battle: &Battle, a: Position, b: Position) -> bool {
    a != b && battle.state.at(a).is_some_and(|m| m.hp > 0) && battle.state.at(b).is_some_and(|m| m.hp > 0)
}

/// The stats a stored boosts object names (its non-zero ones).
fn stored_keys(boosts: &StatBoosts) -> u8 {
    boosts.iter().fold(0, |keys, (id, _)| keys | 1 << id as u8)
}
