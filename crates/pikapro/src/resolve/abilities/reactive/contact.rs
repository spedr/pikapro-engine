//! Reactions to being hit: `DamagingHit` (contact effects, boosts on being
//! hit, Mummy, Cursed Body, ...), `onSourceDamagingHit` (Poison Touch,
//! Toxic Chain), `AfterMoveSecondary` (Berserk, Anger Shell, Color
//! Change), `Hit` (Anger Point, Own Tempo), `Damage` (Sturdy, Berserk),
//! and the switch-in and field reactions of Wind Rider, Wind Power, Dry
//! Skin, Comatose and Pastel Veil.

use pikapro_data::gen9::{abilities as a, items as i, moves};
use pikapro_data::{
    AbilityFlags, AbilityId, BoostId, Event, MoveCategory, MoveFlags, StatBoosts, StatusId, Type, TypeSet,
};

use super::{active_move_id, boost, mon, show};
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName, LineArg};
use crate::format::Mechanics;
use crate::resolve::abilities::{self as abilities, SetAbilityKind};
use crate::resolve::boosts::one_boost;
use crate::resolve::events::{self, Effect, EventCtx, Relay};
use crate::resolve::{actor, boosts, damage_by, field, heal_by, items, manipulation, order, queue, status, volatiles};
use crate::state::{Position, SideCondition, Volatiles, Weather};

/// `onDamagingHit(damage, target, source, move)` of the holder (the hit
/// Pokémon).
pub(super) fn damaging_hit(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    let data = *ev.move_data()?;
    let move_id = ev.effect.move_id()?;
    let damage = ev.value().max(0) as u32;
    let effect = Effect::Ability(ability);
    // The attacker may have left the field (a future attack): first the
    // reactions that don't need it.
    match ability {
        // Stat boosts on being hit (`this.boost(...)`: the attacker is the
        // source).
        a::JUSTIFIED if data.ty == Type::Dark => {
            boost(battle, holder, one_boost(BoostId::Atk, 1), ev.source, ability);
        }
        a::RATTLED if matches!(data.ty, Type::Dark | Type::Bug | Type::Ghost) => {
            boost(battle, holder, one_boost(BoostId::Spe, 1), ev.source, ability);
        }
        a::STAMINA => {
            boost(battle, holder, one_boost(BoostId::Def, 1), ev.source, ability);
        }
        a::STEAM_ENGINE if matches!(data.ty, Type::Water | Type::Fire) => {
            boost(battle, holder, one_boost(BoostId::Spe, 6), ev.source, ability);
        }
        a::THERMAL_EXCHANGE if data.ty == Type::Fire => {
            boost(battle, holder, one_boost(BoostId::Atk, 1), ev.source, ability);
        }
        a::WATER_COMPACTION if data.ty == Type::Water => {
            boost(battle, holder, one_boost(BoostId::Def, 2), ev.source, ability);
        }
        // `boost({def: -1, spe: 2}, target, target)`.
        a::WEAK_ARMOR if data.category == MoveCategory::Physical => {
            boost(battle, holder, StatBoosts::new([0, -1, 0, 0, 2, 0, 0]), Some(holder), ability);
        }
        // The field changes (`setWeather` / `setTerrain` from the holder).
        a::SAND_SPIT => {
            field::set_weather_from(battle, Weather::Sand, Some(holder), effect);
        }
        a::SEED_SOWER => {
            field::set_terrain_from(battle, crate::state::Terrain::Grassy, holder, effect);
        }
        // Every other active Pokémon's Speed drops.
        a::COTTON_DOWN => {
            let mut activated = false;
            let active: crate::util::StackVec<Position, 4> = abilities::all_active(battle).collect();
            for &p in active.iter() {
                if p == holder {
                    continue;
                }
                if !activated {
                    show(battle, holder, ability);
                    activated = true;
                }
                boosts::boost_from(battle, p, one_boost(BoostId::Spe, -1), Some(holder), effect, true, false);
            }
        }
        // `addVolatile('charge')` (order 1).
        a::ELECTROMORPHOSIS => charge(battle, holder, ev.source, ability),
        a::WIND_POWER if data.flags.contains(MoveFlags::WIND) => charge(battle, holder, ev.source, ability),
        _ => {}
    }
    let source = ev.source?;
    let contact = |battle: &mut Battle, announce: bool| {
        abilities::makes_contact(battle, &data, source, holder, announce.then_some(EffectName::Ability(ability)))
    };
    match ability {
        // Contact: 30% to inflict a status on the attacker.
        a::STATIC | a::FLAME_BODY | a::POISON_POINT => {
            let status = match ability {
                a::STATIC => StatusId::Paralysis,
                a::FLAME_BODY => StatusId::Burn,
                _ => StatusId::Poison,
            };
            if contact(battle, false) && battle.rng.chance(3, 10) {
                status::set_status_from(battle, source, status, Some(holder), effect, true);
            }
        }
        // Contact, unless the attacker is immune to powder: `random(100)`
        // below 11 sleep, 21 paralysis, 30 poison.
        a::EFFECT_SPORE => {
            if contact(battle, false) && powder_ok(battle, source) {
                let r = battle.rng.random(100);
                let status = match r {
                    0..11 => Some(StatusId::Sleep),
                    11..21 => Some(StatusId::Paralysis),
                    21..30 => Some(StatusId::Poison),
                    _ => None,
                };
                if let Some(s) = status {
                    status::set_status_from(battle, source, s, Some(holder), effect, true);
                }
            }
        }
        // Contact: 30% to infatuate the attacker.
        a::CUTE_CHARM => {
            if contact(battle, false) && battle.rng.chance(3, 10) {
                volatiles::add_volatile_from(battle, source, "attract", Some(holder), true, effect);
            }
        }
        // 30% to disable the move (not a future move or Struggle), unless
        // the attacker already has a disabled move.
        a::CURSED_BODY => {
            let disabled = mon(battle, source).is_some_and(|m| m.volatiles.has(Volatiles::DISABLE));
            if !disabled
                && !data.flags.contains(MoveFlags::FUTURE_MOVE)
                && move_id != moves::STRUGGLE
                && battle.rng.chance(3, 10)
            {
                volatiles::add_volatile_from(battle, source, "disable", Some(holder), true, effect);
            }
        }
        // Contact: `-ability`, then the attacker's Speed drops (a
        // secondary boost from the holder).
        a::GOOEY | a::TANGLING_HAIR => {
            if contact(battle, true) {
                show(battle, holder, ability);
                boosts::boost_from(battle, source, one_boost(BoostId::Spe, -1), Some(holder), effect, true, false);
            }
        }
        // Contact: an eighth of the attacker's max HP.
        a::ROUGH_SKIN | a::IRON_BARBS => {
            if contact(battle, true) {
                let amount = crate::resolve::hp_fraction(mon(battle, source)?.max_hp() as u32, 1, 8);
                damage_by(battle, source, amount, Some(holder), effect);
            }
        }
        // Knocked out by contact: a quarter of the attacker's max HP.
        a::AFTERMATH => {
            if mon(battle, holder)?.hp == 0 && contact(battle, true) {
                let amount = crate::resolve::hp_fraction(mon(battle, source)?.max_hp() as u32, 1, 4);
                damage_by(battle, source, amount, Some(holder), effect);
            }
        }
        // Knocked out: the damage it took (this hit, and the earlier hits
        // of the move unless it is `smartTarget`).
        a::INNARDS_OUT => {
            if mon(battle, holder)?.hp == 0 {
                let mut amount = damage;
                if !order::smart_target(battle) {
                    amount += battle.move_state.total_damage;
                }
                damage_by(battle, source, amount, Some(holder), effect);
            }
        }
        // Contact (announcing Protective Pads against a foe): the
        // attacker's ability becomes the holder's.
        a::MUMMY | a::LINGERING_AROMA => {
            let theirs = mon(battle, source)?.ability;
            if battle.format.dex.ability(theirs).flags.contains(AbilityFlags::CANT_SUPPRESS) || theirs == ability {
                return None;
            }
            if contact(battle, source.side != holder.side) {
                let name = Some(EffectName::Ability(ability));
                abilities::set_ability(battle, source, ability, Some(holder), name, SetAbilityKind::Normal);
            }
        }
        a::WANDERING_SPIRIT => {
            if contact(battle, false) {
                skill_swap(battle, source, holder);
            }
        }
        // Contact: both get Perish Song's count, unless the attacker has
        // one.
        a::PERISH_BODY => {
            let perishing = mon(battle, source).is_some_and(|m| m.volatiles.has(Volatiles::PERISH_SONG));
            if !contact(battle, false) || perishing {
                return None;
            }
            show(battle, holder, ability);
            volatiles::add_volatile_from(battle, source, "perishsong", Some(source), true, effect);
            volatiles::add_volatile_from(battle, holder, "perishsong", Some(source), true, effect);
        }
        // A physical move sets Toxic Spikes (up to two layers) on the
        // attacker's side (the foe's, if an ally attacked).
        a::TOXIC_DEBRIS => {
            let side = if source.side == holder.side { source.side.foe() } else { source.side };
            let layers = battle.state.side(side).conditions.turns(SideCondition::ToxicSpikes);
            if data.category == MoveCategory::Physical && layers < 2 {
                super::super::activate(battle, holder, ability);
                field::add_side_condition(battle, side, "toxicspikes", holder);
            }
        }
        // Any hit tries to burn the attacker (`source.trySetStatus('brn',
        // target)`; no `-immune` for a Fire type).
        a::SPICY_SPRAY => {
            status::set_status_from(battle, source, StatusId::Burn, Some(holder), effect, true);
        }
        _ => {}
    }
    None
}

/// `source.runStatusImmunity('powder')`: Grass types, then the `Immunity`
/// event (Overcoat, Safety Goggles).
fn powder_ok(battle: &mut Battle, pos: Position) -> bool {
    let grass = mon(battle, pos).is_some_and(|m| manipulation::has_type(m, Type::Grass));
    !grass && events::immunity_event(battle, pos, "powder") == Some(true)
}

/// `pokemon.addVolatile('charge')` from Electromorphosis or Wind Power:
/// `-start|<holder>|Charge|<active move>|[from] ability: X`, also when
/// Charge is already up (`onRestart`).
fn charge(battle: &mut Battle, holder: Position, source: Option<Position>, ability: AbilityId) {
    let Some(m) = mon(battle, holder) else { return };
    if m.hp == 0 {
        return;
    }
    let restart = m.volatiles.has(Volatiles::CHARGE);
    let effect = Effect::Ability(ability);
    if restart || events::add_volatile_flag(battle, holder, Volatiles::CHARGE, "charge", source, effect) {
        let actor = actor(battle, holder);
        let arg = active_move_id(battle).map(LineArg::Move);
        battle.emit(BattleEvent::StartFrom { actor, what: "Charge", arg, cause: Cause::ability(ability) });
    }
}

/// Showdown's `battle.skillSwap(source, target)` (Wandering Spirit): the
/// two trade abilities, announced with `-activate|<source>|Skill Swap`.
fn skill_swap(battle: &mut Battle, source: Position, target: Position) {
    if !events::is_active(battle, source) || !events::is_active(battle, target) {
        return;
    }
    let (Some(s), Some(t)) = (mon(battle, source), mon(battle, target)) else { return };
    let (theirs, ours) = (s.ability, t.ability);
    let dex = battle.format.dex;
    if dex.ability(theirs).flags.contains(AbilityFlags::FAIL_SKILL_SWAP)
        || dex.ability(ours).flags.contains(AbilityFlags::FAIL_SKILL_SWAP)
    {
        return;
    }
    // `runEvent('SetAbility')` on the target, then the source (the
    // effect is the `skillswap` condition).
    let effect = Effect::Move(moves::SKILL_SWAP);
    for (pos, ability) in [(target, theirs), (source, ours)] {
        let mut ev = EventCtx::new(
            Event::SetAbility,
            events::Target::Pokemon(pos),
            Some(source),
            effect,
            Relay::Ability(ability),
        );
        if !events::run_event(battle, &mut ev, None).is_truthy() {
            return;
        }
    }
    let abilities_shown = (source.side != target.side).then_some((ours, theirs));
    let (s_actor, t_actor) = (actor(battle, source), actor(battle, target));
    battle.emit(BattleEvent::SkillSwapActivate { source: s_actor, abilities: abilities_shown, of: t_actor });
    // `Battle.skillSwap`'s hint, which Wandering Spirit calls too.
    if abilities_shown.is_none() {
        battle.hint(crate::event::Hint::SkillSwapAllies);
    }
    events::single_event_ability(battle, Event::End, source, theirs, None, Effect::None, Relay::True);
    events::single_event_ability(battle, Event::End, target, ours, None, Effect::None, Relay::True);
    battle.state.side_mut(source.side).team[source.slot as usize].ability = ours;
    battle.state.side_mut(target.side).team[target.slot as usize].ability = theirs;
    abilities::new_ability_state(battle, source);
    abilities::new_ability_state(battle, target);
    crate::resolve::endless::ability_swapped(battle, source, target);
    events::single_event_ability(battle, Event::Start, target, theirs, None, Effect::None, Relay::True);
    events::single_event_ability(battle, Event::Start, source, ours, None, Effect::None, Relay::True);
}

/// Poison Touch and Toxic Chain's `onSourceDamagingHit(damage, target,
/// source, move)`: the holder attacked `target`.
pub(super) fn source_damaging_hit(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    let target = ev.target_pos()?;
    let data = *ev.move_data()?;
    if abilities::has_ability(battle, target, a::SHIELD_DUST) || items::has_item(battle, target, i::COVERT_CLOAK) {
        return None;
    }
    let effect = Effect::Ability(ability);
    match ability {
        // `checkMoveMakesContact(move, target, source)`: the target's
        // Protective Pads count.
        a::POISON_TOUCH => {
            if abilities::makes_contact(battle, &data, target, holder, None) && battle.rng.chance(3, 10) {
                status::set_status_from(battle, target, StatusId::Poison, Some(holder), effect, true);
            }
        }
        a::TOXIC_CHAIN => {
            if battle.rng.chance(3, 10) {
                status::set_status_from(battle, target, StatusId::Toxic, Some(holder), effect, true);
            }
        }
        _ => {}
    }
    None
}

/// `onAfterMoveSecondary(target, source, move)` of the holder (a target
/// of the move).
pub(super) fn after_move_secondary(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    let data = *ev.move_data()?;
    let move_id = ev.effect.move_id()?;
    match ability {
        // Berserk: falling to half HP or below from above it raises Special
        // Attack; Anger Shell: Attack, Special Attack and Speed up, Defense
        // and Special Defense down.
        a::BERSERK | a::ANGER_SHELL => {
            if let Some(m) = battle.state.at_mut(holder) {
                m.volatile_counters.ability_state.checked = Some(true);
            }
            // `!source || source === target`: a future attack's user off the
            // field (no position here) is still a source.
            let m = mon(battle, holder)?;
            let total = battle.move_state.total_damage;
            if ev.source == Some(holder) || m.hp == 0 || total == 0 {
                return None;
            }
            // `move.multihit` (and not `smartTarget`, for Berserk): the
            // whole move's damage; otherwise `getLastAttackedBy().damage`.
            let multihit = data.multihit.is_some() && (ability == a::ANGER_SHELL || !order::smart_target(battle));
            let index = holder.side.index() * 2 + (holder.slot as usize & 1);
            let damage = if multihit { total } else { battle.move_state.last_damage[index] as u32 };
            let (hp, max) = (m.hp as u32, m.max_hp() as u32);
            if 2 * hp <= max && 2 * (hp + damage) > max {
                let boosts = if ability == a::BERSERK {
                    one_boost(BoostId::SpA, 1)
                } else {
                    // `{atk: 1, spa: 1, spe: 1, def: -1, spd: -1}`.
                    StatBoosts::ordered([1, -1, 1, -1, 1, 0, 0], [0, 2, 4, 1, 3, 5, 6])
                };
                boost(battle, holder, boosts, Some(holder), ability);
            }
            None
        }
        // Color Change: the holder takes the type of a damaging move that
        // hit it.
        a::COLOR_CHANGE => {
            let m = mon(battle, holder)?;
            if m.hp == 0 || !events::is_active(battle, holder) {
                return None;
            }
            let ty = data.ty;
            // `type !== '???'`: Struggle, a typeless Revelation Dance.
            if data.category == MoveCategory::Status
                || move_id == moves::STRUGGLE
                || ty == pikapro_data::Type::Unknown
                || manipulation::has_type(m, ty)
            {
                return None;
            }
            if !manipulation::set_type(battle, holder, TypeSet::mono(ty)) {
                return Some(Relay::False);
            }
            let actor = actor(battle, holder);
            let cause = Cause::ability(ability);
            battle.emit(BattleEvent::StartFrom { actor, what: "typechange", arg: Some(LineArg::Type(ty)), cause });
            // A right-hand Pokémon's queued Curse is re-aimed at its left.
            if battle.format.active_per_side() == 2 && holder.slot == 1 {
                let id = mon(battle, holder)?.id;
                let me = queue::MonRef { side: holder.side, id };
                if let Some(action) = battle
                    .queue
                    .as_mut_slice()
                    .iter_mut()
                    .find(|q| matches!(q.kind, queue::ActionKind::Move { .. }) && q.mon == Some(me))
                    && let queue::ActionKind::Move { move_id: moves::CURSE, target_loc, .. } = &mut action.kind
                {
                    *target_loc = -1;
                }
            }
            None
        }
        _ => None,
    }
}

/// `onHit(target, source, move)` of the holder.
pub(super) fn hit(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, ability: AbilityId) -> Option<Relay> {
    let data = *ev.move_data()?;
    match ability {
        // A critical hit maxes Attack (`-setboost`).
        a::ANGER_POINT => {
            if mon(battle, holder)?.hp == 0 || !ev.effect.is_move() {
                return None;
            }
            if battle.move_state.hit_data(holder).crit {
                boost(battle, holder, one_boost(BoostId::Atk, 12), Some(holder), ability);
            }
        }
        // A move that confuses: `-immune|<holder>|confusion|[from] ability:
        // Own Tempo`.
        a::OWN_TEMPO => {
            if data.volatile_status == Some("confusion") {
                let target = actor(battle, holder);
                battle.emit(BattleEvent::ImmuneFrom {
                    target,
                    detail: Some(LineArg::Text("confusion")),
                    cause: Cause::ability(ability),
                });
            }
        }
        _ => {}
    }
    None
}

/// `onDamage(damage, target, source, effect)` of the holder.
pub(super) fn damage(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    match ability {
        // Priority -30: a move's damage that would knock it out from full
        // HP leaves 1 HP.
        a::STURDY => {
            let m = mon(battle, holder)?;
            let (hp, max) = (m.hp as i32, m.max_hp() as i32);
            if hp == max && ev.value() >= hp && ev.effect.is_move() {
                show(battle, holder, ability);
                return Some(Relay::Num(hp - 1));
            }
            None
        }
        // Berserk, Anger Shell: `checked` is false during a single-hit
        // move's damage (the healing berries wait for its
        // `AfterMoveSecondary`). Scarlet/Violet also exempt a Sheer Force
        // move. `effect.multihit` is the active move's: Parental Bond's
        // `onPrepareHit` sets it to 2.
        a::BERSERK | a::ANGER_SHELL => {
            let multihit = ev.effect.move_id().is_some_and(|m| {
                battle.format.dex.move_(m).multihit.is_some()
                    || (battle.move_state.parental_bond && battle.active_move.is_some_and(|(_, a)| a == m))
            });
            let sheer_force = battle.format.mechanics == Mechanics::ScarletViolet
                && battle.move_state.has_sheer_force
                && ev.source.is_some_and(|s| abilities::has_ability(battle, s, a::SHEER_FORCE));
            let during_move = ev.effect.is_move() && !multihit && !sheer_force;
            if let Some(m) = battle.state.at_mut(holder) {
                m.volatile_counters.ability_state.checked = Some(!during_move);
            }
            None
        }
        _ => None,
    }
}

/// Berserk and Anger Shell's `onTryEatItem`: a healing berry waits for
/// `checked`.
pub(super) fn try_eat_item(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    let Relay::Item(item) = ev.relay else { return None };
    let healing = matches!(
        item,
        i::AGUAV_BERRY
            | i::ENIGMA_BERRY
            | i::FIGY_BERRY
            | i::IAPAPA_BERRY
            | i::MAGO_BERRY
            | i::SITRUS_BERRY
            | i::WIKI_BERRY
            | i::ORAN_BERRY
            | i::BERRY_JUICE
    );
    if !healing {
        return None;
    }
    match mon(battle, holder)?.volatile_counters.ability_state.checked {
        Some(false) => Some(Relay::False),
        _ => None,
    }
}

/// Wind Rider and Wind Power's `onSideConditionStart`: Tailwind on the
/// holder's side.
pub(super) fn side_condition_start(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    if ev.effect != Effect::Side(SideCondition::Tailwind) {
        return None;
    }
    match ability {
        // `boost({atk: 1}, pokemon, pokemon)`.
        a::WIND_RIDER => {
            boost(battle, holder, one_boost(BoostId::Atk, 1), Some(holder), ability);
        }
        a::WIND_POWER => charge(battle, holder, ev.source, ability),
        _ => {}
    }
    None
}

/// `onStart` (switching in, or gaining the ability).
pub(super) fn start(battle: &mut Battle, holder: Position, ability: AbilityId) -> Option<Relay> {
    match ability {
        // Tailwind already up: Attack +1.
        a::WIND_RIDER => {
            if battle.state.side(holder.side).conditions.turns(SideCondition::Tailwind) > 0 {
                boost(battle, holder, one_boost(BoostId::Atk, 1), Some(holder), ability);
            }
        }
        a::COMATOSE => show(battle, holder, ability),
        // Pastel Veil (also on any switch-in): poisoned allies are cured.
        a::PASTEL_VEIL => {
            for slot in 0..battle.format.active_per_side() {
                let ally = Position::new(holder.side, slot as u8);
                if !events::is_active(battle, ally) || mon(battle, ally).is_none_or(|m| m.hp == 0) {
                    continue;
                }
                if status::poisoned(mon(battle, ally)?) {
                    super::super::activate(battle, holder, ability);
                    status::cure_status(battle, ally);
                }
            }
        }
        _ => {}
    }
    None
}

/// Dry Skin's `onWeather`: rain heals an eighth, sun hurts an eighth.
pub(super) fn dry_skin_weather(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    let Effect::Weather(weather) = ev.effect else { return None };
    if abilities::pokemon_weather(battle, holder) != Some(weather) {
        return None;
    }
    let amount = crate::resolve::hp_fraction(mon(battle, holder)?.max_hp() as u32, 1, 8);
    let effect = Effect::Ability(a::DRY_SKIN);
    match weather {
        Weather::Rain | Weather::HeavyRain => {
            heal_by(battle, holder, amount, None, effect);
        }
        Weather::Sun | Weather::HarshSun => {
            damage_by(battle, holder, amount, Some(holder), effect);
        }
        _ => {}
    }
    None
}
