//! Immunities, contact and reactive effects, status and boost protection.
//!
//! Showdown references: the abilities in `data/abilities.ts` that
//! `abilities::owner` assigns to [`Owner::Reactive`](super::Owner::Reactive)
//! (Levitate, Static, the absorbing abilities,
//! Clear Body, Lightning Rod, Magic Bounce, Sturdy, Wonder Guard, ...), and
//! the `champions` mod's Berserk, Anger Shell and Spicy Spray
//! (`data/mods/champions/abilities.ts`).
//!
//! Adding an ability: see `abilities/mod.rs`. One arm per callback in
//! [`run`]; this module's volatiles (bits 88..=99) get their handlers in
//! [`volatile_spec`]. The callbacks live in three modules:
//!
//! - [`immunity`]: moves stopped before they hit (`TryHit`, `TryHitSide`,
//!   `TryMove`, `RedirectTarget`): the absorbing abilities, Magic Bounce,
//!   Wonder Guard, Sturdy, Dazzling and the like.
//! - [`contact`]: reactions to being hit (`DamagingHit`,
//!   `AfterMoveSecondary`, `Hit`, `Damage`): contact effects, boosts on
//!   being hit, Berserk, Color Change, Mummy, ...
//! - [`guard`]: status, volatile and stat-change protection (`SetStatus`,
//!   `TryAddVolatile`, `Immunity`, `Update`, `TryBoost`, `ChangeBoost`,
//!   `AfterEachBoost`, ...).

mod contact;
mod guard;
mod immunity;

use super::{effect_has_status, show};

use pikapro_data::gen9::abilities as a;
use pikapro_data::{AbilityId, Event, EventPrefix, MoveId};

use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName};
use crate::resolve::actor;
use crate::resolve::events::{Effect, EventCtx, Relay, Spec};
use crate::resolve::handlers::{Handler, Holder};
use crate::state::{PokemonState, Position, Volatiles};

/// Runs `ability`'s callback for `ev.event` / `ev.prefix` on its holder.
///
/// Levitate has no callback: `field::grounded` (`isGrounded`) and
/// `events::run_immunity` (`-immune|...|[from] ability: Levitate`) check
/// it with `has_ability`.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, ability: AbilityId) -> Option<Relay> {
    use EventPrefix::{Ally, Any, Foe, On, Source};
    match (ability, ev.event, ev.prefix) {
        // Moves stopped before they hit.
        (_, Event::TryHit, On) => immunity::try_hit(battle, ev, holder, ability),
        (a::MAGIC_BOUNCE | a::SAP_SIPPER | a::SOUNDPROOF, Event::TryHitSide, Ally) => {
            immunity::ally_try_hit_side(battle, ev, holder, ability)
        }
        (a::ARMOR_TAIL | a::DAZZLING | a::QUEENLY_MAJESTY, Event::TryMove, Foe) => {
            immunity::foe_try_move(battle, ev, holder, ability)
        }
        (a::DAMP, Event::TryMove, Any) => immunity::damp_try_move(battle, ev, holder),
        (a::LIGHTNING_ROD | a::STORM_DRAIN, Event::RedirectTarget, Any) => {
            immunity::redirect_target(battle, ev, holder, ability)
        }
        (a::FLASH_FIRE, Event::End, On) => immunity::flash_fire_end(battle, holder),

        // Reactions to being hit.
        (_, Event::DamagingHit, On) => contact::damaging_hit(battle, ev, holder, ability),
        (a::POISON_TOUCH | a::TOXIC_CHAIN, Event::DamagingHit, Source) => {
            contact::source_damaging_hit(battle, ev, holder, ability)
        }
        (a::BERSERK | a::ANGER_SHELL | a::COLOR_CHANGE, Event::AfterMoveSecondary, On) => {
            contact::after_move_secondary(battle, ev, holder, ability)
        }
        (a::ANGER_POINT | a::OWN_TEMPO, Event::Hit, On) => contact::hit(battle, ev, holder, ability),
        (a::STURDY | a::BERSERK | a::ANGER_SHELL, Event::Damage, On) => contact::damage(battle, ev, holder, ability),
        (a::DAMP, Event::Damage, Any) => {
            // `onAnyDamage`: Aftermath's damage is stopped.
            (ev.effect == Effect::Ability(a::AFTERMATH)).then_some(Relay::False)
        }
        (a::BERSERK | a::ANGER_SHELL, Event::TryEatItem, On) => contact::try_eat_item(battle, ev, holder),
        (a::STEADFAST, Event::Flinch, On) => {
            // `onFlinch`: `boost({spe: 1})` (no source: `runEvent('Flinch',
            // pokemon)`).
            boost(battle, holder, pikapro_data::StatBoosts::new([0, 0, 0, 0, 1, 0, 0]), ev.source, ability);
            None
        }
        (a::WIND_RIDER | a::WIND_POWER, Event::SideConditionStart, On) => {
            contact::side_condition_start(battle, ev, holder, ability)
        }
        (a::WIND_RIDER | a::COMATOSE | a::PASTEL_VEIL, Event::Start, On) => contact::start(battle, holder, ability),
        (a::PASTEL_VEIL, Event::SwitchIn, Any) => contact::start(battle, holder, ability),
        (a::DRY_SKIN, Event::Weather, On) => contact::dry_skin_weather(battle, ev, holder),
        (a::DRY_SKIN, Event::BasePower, Source) => {
            // `onSourceBasePower` (priority 17): Fire moves 1.25x.
            let fire = ev.move_data().is_some_and(|d| d.ty == pikapro_data::Type::Fire);
            if fire { ev.chain_modify(5, 4) } else { None }
        }
        (a::PURIFYING_SALT, Event::ModifyAtk | Event::ModifySpA, Source) => {
            // `onSourceModifyAtk` / `onSourceModifySpA`: Ghost moves 0.5x.
            let ghost = ev.move_data().is_some_and(|d| d.ty == pikapro_data::Type::Ghost);
            if ghost { ev.chain_modify(1, 2) } else { None }
        }
        (a::KEEN_EYE, Event::ModifyMove, On) => {
            // `onModifyMove`: `move.ignoreEvasion = true`.
            battle.move_state.ignore_evasion = true;
            None
        }
        (a::SUCTION_CUPS | a::GUARD_DOG, Event::DragOut, On) => {
            // `onDragOut` (priority 1): `-activate`, `null`.
            super::activate(battle, holder, ability);
            Some(Relay::Null)
        }

        // Status, volatile and stat-change protection.
        (_, Event::SetStatus, On | Ally) => guard::set_status(battle, ev, holder, ability),
        (_, Event::TryAddVolatile, On | Ally) => guard::try_add_volatile(battle, ev, holder, ability),
        (a::MAGMA_ARMOR | a::OVERCOAT | a::OBLIVIOUS, Event::Immunity, On) => guard::immunity(ev, ability),
        (_, Event::Update, _) => guard::update(battle, holder, ability),
        (_, Event::TryBoost, On | Ally) => guard::try_boost(battle, ev, holder, ability),
        (a::CONTRARY | a::SIMPLE, Event::ChangeBoost, On) => guard::change_boost(ev, ability),
        (a::COMPETITIVE | a::DEFIANT, Event::AfterEachBoost, On) => {
            guard::after_each_boost(battle, ev, holder, ability)
        }
        (a::RATTLED, Event::AfterBoost, On) => guard::rattled_after_boost(battle, ev, holder),
        (a::SYNCHRONIZE, Event::AfterSetStatus, On) => guard::synchronize(battle, ev, holder),
        (a::POISON_PUPPETEER, Event::AfterSetStatus, Any) => guard::poison_puppeteer(battle, ev, holder),
        (a::SHIELD_DUST, Event::ModifySecondaries, On) => guard::shield_dust(battle, ev),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers.

/// The Pokémon at `pos`.
fn mon(battle: &Battle, pos: Position) -> Option<&PokemonState> {
    battle.state.side(pos.side).team.get(pos.slot as usize)
}

/// Showdown's `battle.boost(boosts, target, source, <the ability>)`: the
/// ability's stat changes (`-ability|<target>|<ability>|boost` first).
fn boost(
    battle: &mut Battle,
    target: Position,
    boosts: pikapro_data::StatBoosts,
    source: Option<Position>,
    ability: AbilityId,
) -> bool {
    crate::resolve::boosts::boost_from(battle, target, boosts, source, Effect::Ability(ability), false, false)
}

/// `-immune|<target>|[from] ability: X`.
fn immune(battle: &mut Battle, target: Position, ability: AbilityId) {
    let target = actor(battle, target);
    battle.emit(BattleEvent::ImmuneFrom { target, detail: None, cause: Cause::ability(ability) });
}

/// `-block|<target>|ability: X|[of] <holder>`.
fn block(battle: &mut Battle, target: Position, ability: AbilityId, holder: Position) {
    let (target, of) = (actor(battle, target), actor(battle, holder));
    battle.emit(BattleEvent::Block { actor: target, effect: EffectName::Ability(ability), arg: None, of: Some(of) });
}

/// `attrLastMove('[still]')`: the `|move|` line loses its target.
fn still(battle: &mut Battle) {
    battle.flush_rng_trace();
    battle.attr_last_move(crate::event::MoveAttr::Still);
}

/// The active move's ID, if it is a dex move.
fn active_move_id(battle: &Battle) -> Option<MoveId> {
    crate::resolve::active_dex_move(battle).map(|(_, m)| m)
}

// ---------------------------------------------------------------------------
// Volatiles (bits 88..=99).

/// This module's volatiles' handlers (bits 88..=99).
pub(crate) fn volatile_spec(flag: u128, event: Event, prefix: EventPrefix) -> Option<Spec> {
    match (flag, event, prefix) {
        // `flashfire`'s `onModifyAtk` / `onModifySpA` (priority 5).
        (Volatiles::FLASH_FIRE, Event::ModifyAtk | Event::ModifySpA, EventPrefix::On) => {
            Some(Spec::relay(flash_fire_modify).priority(5))
        }
        _ => None,
    }
}

/// The events [`volatile_spec`] handles for `flag` (any prefix). Keep it in
/// step with `volatile_spec` (a test checks them).
pub(crate) const fn volatile_events(flag: u128) -> u128 {
    match flag {
        Volatiles::FLASH_FIRE => const { pikapro_data::EventSet::of(&[Event::ModifyAtk, Event::ModifySpA]).0 },
        _ => 0,
    }
}

/// `flashfire`: the holder's Fire moves get 1.5x, while it still has
/// Flash Fire (`attacker.hasAbility('flashfire')`).
fn flash_fire_modify(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    let fire = ev.move_data().is_some_and(|d| d.ty == pikapro_data::Type::Fire);
    if fire && super::has_ability(battle, pos, a::FLASH_FIRE) { ev.chain_modify(3, 2) } else { None }
}
