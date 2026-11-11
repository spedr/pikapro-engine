//! Moves stopped before they hit: `TryHit` (the absorbing abilities,
//! Bulletproof, Soundproof, Good as Gold, Overcoat, Telepathy, Wonder
//! Guard, Sturdy's OHKO immunity, Magic Bounce), `TryHitSide` (Magic
//! Bounce, Sap Sipper, Soundproof), `TryMove` (Armor Tail, Dazzling,
//! Queenly Majesty, Damp) and `RedirectTarget` (Lightning Rod, Storm
//! Drain).

use pikapro_data::gen9::{abilities as a, moves};
use pikapro_data::{AbilityId, BoostId, MoveCategory, MoveData, MoveFlags, MoveTarget, StatBoosts, Type};

use super::{boost, immune, still};
use crate::battle::Battle;
use crate::event::{BattleEvent, EffectName, MoveFrom};
use crate::resolve::events::{self, Effect, EventCtx, Relay};
use crate::resolve::{actor, heal_by, manipulation, moves as pipeline, multiturn, order, power, targeting};
use crate::state::{MoveResult, Position, Volatiles};

/// `onTryHit(target, source, move)` of the holder (the event's target).
pub(super) fn try_hit(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    use crate::resolve::boosts::one_boost as one;
    use BoostId::{Atk, Def, SpA, Spe};
    let source = ev.source?;
    let data = *ev.move_data()?;
    let move_id = ev.effect.move_id()?;
    let other = source != holder;
    match ability {
        // Heal a quarter (`-immune` when that heals nothing), then `null`.
        a::VOLT_ABSORB => absorb_heal(battle, holder, source, ability, other && data.ty == Type::Electric),
        a::WATER_ABSORB | a::DRY_SKIN => absorb_heal(battle, holder, source, ability, other && data.ty == Type::Water),
        a::EARTH_EATER => absorb_heal(battle, holder, source, ability, other && data.ty == Type::Ground),
        // `boost(...)` from the event (source: the attacker), `-immune`
        // when nothing changed, then `null`.
        a::LIGHTNING_ROD => {
            absorb_boost(battle, holder, Some(source), ability, other && data.ty == Type::Electric, one(SpA, 1))
        }
        a::STORM_DRAIN => {
            absorb_boost(battle, holder, Some(source), ability, other && data.ty == Type::Water, one(SpA, 1))
        }
        a::MOTOR_DRIVE => {
            absorb_boost(battle, holder, Some(source), ability, other && data.ty == Type::Electric, one(Spe, 1))
        }
        a::SAP_SIPPER => {
            absorb_boost(battle, holder, Some(source), ability, other && data.ty == Type::Grass, one(Atk, 1))
        }
        a::WELL_BAKED_BODY => {
            absorb_boost(battle, holder, Some(source), ability, other && data.ty == Type::Fire, one(Def, 2))
        }
        // `boost({atk: 1}, target, target)`.
        a::WIND_RIDER => {
            let wind = other && data.flags.contains(MoveFlags::WIND);
            absorb_boost(battle, holder, Some(holder), ability, wind, one(Atk, 1))
        }
        a::FLASH_FIRE => {
            if !(other && data.ty == Type::Fire) {
                return None;
            }
            // `move.accuracy = true`.
            battle.move_state.accuracy_true = true;
            // `addVolatile('flashfire')`: `-start|<holder>|ability: Flash
            // Fire`; already up, `-immune`.
            let effect = Effect::Ability(ability);
            if events::add_volatile_flag(battle, holder, Volatiles::FLASH_FIRE, "flashfire", Some(source), effect) {
                let actor = actor(battle, holder);
                battle.emit(BattleEvent::EffectStartEnd {
                    actor,
                    start: true,
                    what: crate::event::LineArg::Text(""),
                    effect: Some(EffectName::Ability(ability)),
                    cause: None,
                    silent: false,
                });
            } else {
                immune(battle, holder, ability);
            }
            Some(Relay::Null)
        }
        a::BULLETPROOF => immune_if(battle, holder, ability, data.flags.contains(MoveFlags::BULLET)),
        a::SOUNDPROOF => immune_if(battle, holder, ability, other && data.flags.contains(MoveFlags::SOUND)),
        a::GOOD_AS_GOLD => immune_if(battle, holder, ability, other && data.category == MoveCategory::Status),
        // Priority 1: a powder move against a Pokémon that is not already
        // immune to powder (`dex.getImmunity('powder')`: Grass types).
        a::OVERCOAT => {
            let grass = super::mon(battle, holder).is_some_and(|m| manipulation::has_type(m, Type::Grass));
            immune_if(battle, holder, ability, data.flags.contains(MoveFlags::POWDER) && other && !grass)
        }
        a::OBLIVIOUS => {
            let blocked = matches!(move_id, moves::ATTRACT | moves::CAPTIVATE | moves::TAUNT);
            immune_if(battle, holder, ability, blocked)
        }
        a::STURDY => immune_if(battle, holder, ability, data.ohko),
        // An ally's damaging move: `-activate`, `null`.
        a::TELEPATHY => {
            if other && source.side == holder.side && data.category != MoveCategory::Status {
                super::super::activate(battle, holder, ability);
                return Some(Relay::Null);
            }
            None
        }
        a::WONDER_GUARD => wonder_guard(battle, holder, source, move_id, &data),
        a::MAGIC_BOUNCE => {
            // Priority 1: a reflectable move is used back at its user.
            if !other
                || battle.move_state.has_bounced
                || !data.flags.contains(MoveFlags::REFLECTABLE)
                || multiturn::is_semi_invulnerable(battle, holder)
            {
                return None;
            }
            bounce(battle, holder, source, move_id);
            Some(Relay::Null)
        }
        _ => None,
    }
}

/// Volt Absorb, Water Absorb, Dry Skin, Earth Eater: `if (!this.heal(
/// target.baseMaxhp / 4)) -immune`, `null`.
fn absorb_heal(
    battle: &mut Battle,
    holder: Position,
    source: Position,
    ability: AbilityId,
    applies: bool,
) -> Option<Relay> {
    if !applies {
        return None;
    }
    let amount = crate::resolve::hp_fraction(super::mon(battle, holder)?.max_hp() as u32, 1, 4);
    if heal_by(battle, holder, amount, Some(source), Effect::Ability(ability)).is_none() {
        immune(battle, holder, ability);
    }
    Some(Relay::Null)
}

/// Lightning Rod, Storm Drain, Motor Drive, Sap Sipper, Well-Baked Body,
/// Wind Rider: `if (!this.boost(...)) -immune`, `null`.
fn absorb_boost(
    battle: &mut Battle,
    holder: Position,
    source: Option<Position>,
    ability: AbilityId,
    applies: bool,
    boosts: StatBoosts,
) -> Option<Relay> {
    if !applies {
        return None;
    }
    if !boost(battle, holder, boosts, source, ability) {
        immune(battle, holder, ability);
    }
    Some(Relay::Null)
}

/// `-immune|<holder>|[from] ability: X` and `null` when `blocked`.
fn immune_if(battle: &mut Battle, holder: Position, ability: AbilityId, blocked: bool) -> Option<Relay> {
    if !blocked {
        return None;
    }
    immune(battle, holder, ability);
    Some(Relay::Null)
}

/// Wonder Guard's `onTryHit`: a damaging move (not Struggle) that is not
/// super effective (`runEffectiveness(move) <= 0`) or that the holder is
/// immune to (`runImmunity`) is stopped, silently for a `smartTarget` move
/// (which stops being one).
fn wonder_guard(
    battle: &mut Battle,
    holder: Position,
    source: Position,
    move_id: pikapro_data::MoveId,
    data: &MoveData,
) -> Option<Relay> {
    if holder == source || data.category == MoveCategory::Status || move_id == moves::STRUGGLE {
        return None;
    }
    // Showdown also lets a Sky Drop user through (`!source.volatiles
    // ['skydrop']`): Sky Drop is Past in Gen 9 and Champions.
    let blocked = power::run_effectiveness(battle, holder, move_id, data) <= 0
        || !events::run_immunity(battle, holder, data.ty, false);
    if !blocked {
        return None;
    }
    if order::smart_target(battle) {
        battle.order.smart_target = false;
    } else {
        immune(battle, holder, a::WONDER_GUARD);
    }
    Some(Relay::Null)
}

/// `onAllyTryHitSide(target, source, move)`: a side move's first target
/// is on the holder's side.
pub(super) fn ally_try_hit_side(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    let source = ev.source?;
    let target = ev.target_pos()?;
    let data = *ev.move_data()?;
    let move_id = ev.effect.move_id()?;
    match ability {
        // Magic Bounce: a foe's reflectable side move (Stealth Rock, ...)
        // is used back by the holder.
        a::MAGIC_BOUNCE => {
            if target.side == source.side
                || battle.move_state.has_bounced
                || !data.flags.contains(MoveFlags::REFLECTABLE)
                || multiturn::is_semi_invulnerable(battle, target)
            {
                return None;
            }
            bounce(battle, holder, source, move_id);
            battle.move_state.has_bounced = true;
            Some(Relay::Null)
        }
        // Sap Sipper: an ally's Grass side move raises the holder's Attack.
        a::SAP_SIPPER => {
            if source == holder || target.side != source.side {
                return None;
            }
            if data.ty == Type::Grass {
                boost(battle, holder, StatBoosts::new([1, 0, 0, 0, 0, 0, 0]), Some(source), ability);
            }
            None
        }
        // Soundproof only announces itself.
        a::SOUNDPROOF => {
            if data.flags.contains(MoveFlags::SOUND) {
                immune(battle, holder, ability);
            }
            None
        }
        _ => None,
    }
}

/// Magic Bounce's `this.actions.useMove(newMove, bouncer, {target:
/// source})`: a new active move with `hasBounced` (no Prankster boost),
/// the reflected move's priority and `[from] ability: Magic Bounce`. The
/// reflected move's own state comes back afterwards (Showdown's hit steps
/// go on with their own `move` object).
fn bounce(battle: &mut Battle, bouncer: Position, target: Position, move_id: pikapro_data::MoveId) {
    let saved =
        (battle.move_state, battle.order.source_effect, battle.order.smart_target, battle.order.priority_override);
    let dex_data = *battle.format.dex.move_(move_id);
    // `move.priority = this.battle.activeMove.priority`.
    let priority = order::active_priority(battle, &dex_data);
    // `useMove`: `moveThisTurnResult` starts over.
    if let Some(m) = battle.state.at_mut(bouncer) {
        m.volatile_counters.move_this_turn = MoveResult::Undefined;
    }
    battle.active_move = Some((bouncer, move_id));
    order::set_active_move(battle, move_id, None, Some(priority));
    let from = Some(MoveFrom::Ability(a::MAGIC_BOUNCE));
    // `useMoveInner` runs the new active move's `ModifyType` events.
    let did = pipeline::use_move(battle, bouncer, move_id, &dex_data, Some(target), from);
    // Showdown's Magic Bounce.onTryHit/ onAllyTryHitSide do not restore
    // battle.activeMove after useMove. BattleActions.runMove subsequently
    // passes that reflected ActiveMove to AfterMove (Charge reads its
    // modified type), while the original hit steps keep their own move.
    battle.order.called_move = Some(order::CalledMove {
        ty: battle.move_state.ty,
        has_sheer_force: battle.move_state.has_sheer_force,
        ignore_ability: battle.move_state.ignore_ability,
    });
    if let Some(m) = battle.state.at_mut(bouncer)
        && m.volatile_counters.move_this_turn == MoveResult::Undefined
    {
        m.volatile_counters.move_this_turn = if did { MoveResult::Succeeded } else { MoveResult::Failed };
    }
    let (state, source_effect, smart_target, priority_override) = saved;
    battle.move_state = crate::resolve::MoveState { active_target: battle.move_state.active_target, ..state };
    battle.order.source_effect = source_effect;
    battle.order.smart_target = smart_target;
    battle.order.priority_override = priority_override;
}

/// Armor Tail, Dazzling, Queenly Majesty's `onFoeTryMove(target, source,
/// move)`: a foe's move with priority aimed at the holder's side (or at
/// everyone, but for Perish Song, Flower Shield and Rototiller) is stopped
/// (`cant|<holder>|ability: X|<move>|[of] <user>`).
pub(super) fn foe_try_move(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    let user = ev.target_pos()?;
    let data = *ev.move_data()?;
    let move_id = ev.effect.move_id()?;
    let all_except = matches!(move_id, moves::PERISH_SONG | moves::FLOWER_SHIELD | moves::ROTOTILLER);
    if data.target == MoveTarget::FoeSide || (data.target == MoveTarget::All && !all_except) {
        return None;
    }
    let toward_holder = ev.source.is_some_and(|t| t.side == holder.side) || data.target == MoveTarget::All;
    if toward_holder && order::active_priority(battle, &data) > 0 {
        cant(battle, holder, ability, move_id, user);
        return Some(Relay::False);
    }
    None
}

/// Damp's `onAnyTryMove`: the self-destructing moves can't be used.
pub(super) fn damp_try_move(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    let user = ev.target_pos()?;
    let move_id = ev.effect.move_id()?;
    if matches!(move_id, moves::EXPLOSION | moves::MIND_BLOWN | moves::MISTY_EXPLOSION | moves::SELF_DESTRUCT) {
        cant(battle, holder, a::DAMP, move_id, user);
        return Some(Relay::False);
    }
    None
}

/// `attrLastMove('[still]')`, `cant|<holder>|ability: X|<move>|[of] <user>`.
fn cant(battle: &mut Battle, holder: Position, ability: AbilityId, move_id: pikapro_data::MoveId, user: Position) {
    still(battle);
    let (holder, of) = (actor(battle, holder), actor(battle, user));
    battle.emit(BattleEvent::CantFrom {
        actor: holder,
        effect: EffectName::Ability(ability),
        move_id: Some(move_id),
        of: Some(of),
    });
}

/// Lightning Rod and Storm Drain's `onAnyRedirectTarget(target, source,
/// source2, move)`: a move of their type (not a pledge combination) the
/// holder could be targeted by goes to the holder (`-activate` if that
/// changes the target), and stops being `smartTarget`.
pub(super) fn redirect_target(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Relay> {
    let data = *ev.move_data()?;
    let user = ev.target_pos()?;
    let Relay::Pokemon(target) = ev.relay else { return None };
    let ty = if ability == a::LIGHTNING_ROD { Type::Electric } else { Type::Water };
    if data.ty != ty || data.flags.contains(MoveFlags::PLEDGE_COMBO) {
        return None;
    }
    let redirect = match data.target {
        MoveTarget::RandomNormal | MoveTarget::AdjacentFoe => MoveTarget::Normal,
        t => t,
    };
    if !targeting::valid_target_loc(battle, targeting::loc_of(user, holder), user, redirect) {
        return None;
    }
    battle.order.smart_target = false;
    if holder != target {
        super::super::activate(battle, holder, ability);
    }
    Some(Relay::Pokemon(holder))
}

/// Flash Fire's `onEnd`: `removeVolatile('flashfire')` (`-end|<holder>|ability:
/// Flash Fire|[silent]`).
pub(super) fn flash_fire_end(battle: &mut Battle, holder: Position) -> Option<Relay> {
    // `removeVolatile` does nothing without HP.
    if events::remove_volatile_flag(battle, holder, Volatiles::FLASH_FIRE) {
        let actor = actor(battle, holder);
        battle.emit(BattleEvent::EffectStartEnd {
            actor,
            start: false,
            what: crate::event::LineArg::Text(""),
            effect: Some(EffectName::Ability(a::FLASH_FIRE)),
            cause: None,
            silent: true,
        });
    }
    None
}
