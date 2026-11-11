//! Entry hazards and the moves that set or clear them.
//!
//! Showdown references (`data/moves.ts`): the side conditions of
//! `stealthrock`, `spikes`, `toxicspikes` and `stickyweb` (`onSideStart`,
//! `onSideRestart`, `onSwitchIn`); `rapidspin` and `mortalspin`
//! (`onAfterHit`), `defog` and `tidyup` (`onHit`), `ceaselessedge` and
//! `stoneaxe` (`onAfterHit`).
//!
//! Hazards are side conditions (`SideConditions`, set by
//! `field::add_side_condition`); their `onSwitchIn` handlers run through
//! `handlers::run_switch_in` from `runSwitch`.

use pikapro_data::gen9::moves as ids;
use pikapro_data::{Effectiveness, MoveId, StatBoosts, StatusId, Type};

use super::field::{self, condition_damage, is_grounded, remove_side_condition};
use super::handlers::{Callback, Handler, Handlers, Holder, sub_order};
use super::manipulation::has_type;
use super::{actor, boosts, status};
use crate::battle::Battle;
use crate::event::{BattleEvent, VolatileEffect};
use crate::state::{Position, SideCondition, SideId, Volatiles};

/// The hazards, in the order Rapid Spin, Defog and Tidy Up clear them.
const HAZARDS: [SideCondition; 4] =
    [SideCondition::Spikes, SideCondition::ToxicSpikes, SideCondition::StealthRock, SideCondition::StickyWeb];

/// Status moves implemented here beyond their data.
pub(crate) fn handles(move_id: MoveId) -> bool {
    matches!(move_id, ids::STEALTH_ROCK | ids::SPIKES | ids::TOXIC_SPIKES | ids::STICKY_WEB | ids::DEFOG | ids::TIDY_UP)
}

/// The `onSwitchIn` handlers of the hazards on the side of the Pokémon at
/// `pos`, in the order they were set (their `effectOrder`).
pub(crate) fn switch_in_handlers(battle: &Battle, pos: Position, speed: i32, handlers: &mut Handlers) {
    let conditions = &battle.state.side(pos.side).conditions;
    for (i, &condition) in conditions.order.iter().enumerate() {
        if !condition.is_hazard() {
            continue;
        }
        handlers.push(Handler {
            speed,
            sub_order: sub_order::SIDE_CONDITION,
            effect_order: i as u32,
            holder: Holder::SideOn(pos),
            callback: Callback::Plain(hazard_switch_in),
            arg: condition as u8,
            ..Handler::default()
        });
    }
}

/// A hazard's `onSwitchIn`.
fn hazard_switch_in(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::SideOn(pos) = handler.holder else { return true };
    let condition = SideCondition::ALL[handler.arg as usize];
    // A hazard cleared since the handlers were collected does nothing.
    let layers = battle.state.side(pos.side).conditions.turns(condition);
    let Some(mon) = battle.state.at(pos).copied() else { return true };
    if layers == 0 || mon.is_fainted() {
        return true;
    }
    // Heavy-Duty Boots keep every hazard but Toxic Spikes'
    // absorption away.
    let boots = super::items::held::hazard_immune(battle, pos);
    if boots && condition != SideCondition::ToxicSpikes {
        return true;
    }
    let foe = Position::new(pos.side.foe(), 0);
    match condition {
        // `maxhp * 2 ** typeMod / 8` with Stealth Rock's effectiveness.
        SideCondition::StealthRock => {
            // `runEffectiveness` over `getTypes()` (an added type counts).
            let type_mod = super::manipulation::all_types(&mon)
                .into_iter()
                .flatten()
                .map(|t| match Type::Rock.against(t) {
                    Effectiveness::Exp(n) => n,
                    Effectiveness::Immune => 0,
                })
                .sum::<i8>()
                .clamp(-6, 6);
            let max_hp = mon.max_hp() as u32;
            let amount = if type_mod >= 0 {
                super::hp_fraction(max_hp, 1 << type_mod, 8)
            } else {
                super::hp_fraction(max_hp, 1, 8 << -type_mod)
            };
            condition_damage(battle, pos, amount, condition);
        }
        SideCondition::Spikes if is_grounded(battle, pos) => {
            let amount = super::hp_fraction(mon.max_hp() as u32, [0, 3, 4, 6][layers.min(3) as usize], 24);
            condition_damage(battle, pos, amount, condition);
        }
        SideCondition::ToxicSpikes if is_grounded(battle, pos) => {
            if has_type(&mon, Type::Poison) {
                let of = actor(battle, pos);
                battle.emit(BattleEvent::ToxicSpikesAbsorbed { side: pos.side, of });
                remove_side_condition(battle, pos.side, condition);
            } else if !has_type(&mon, Type::Steel) && !boots {
                // `trySetStatus(..., pokemon.side.foe.active[0])`: the
                // condition is not a move, so failures are silent.
                let status = if layers >= 2 { StatusId::Toxic } else { StatusId::Poison };
                let effect = super::events::Effect::Side(SideCondition::ToxicSpikes);
                status::set_status_from(battle, pos, status, Some(foe), effect, true);
            }
        }
        SideCondition::StickyWeb if is_grounded(battle, pos) => {
            let target = actor(battle, pos);
            battle.emit(BattleEvent::MoveActivate { actor: target, move_id: ids::STICKY_WEB, of: None });
            // `boost({spe: -1}, pokemon, pokemon.side.foe.active[0],
            // getActiveMove('stickyweb'))`: the effect is Sticky Web, also
            // during a drag.
            let changes = StatBoosts::new([0, 0, 0, 0, -1, 0, 0]);
            boosts::boost_from(
                battle,
                pos,
                changes,
                Some(foe),
                super::events::Effect::Move(ids::STICKY_WEB),
                false,
                false,
            );
        }
        _ => {}
    }
    true
}

/// Clears `side`'s hazards (Rapid Spin's list order), logging each one.
/// Returns whether any was up.
fn clear_hazards(battle: &mut Battle, side: SideId, from: Option<MoveId>, of: Option<Position>) -> bool {
    let mut any = false;
    for condition in HAZARDS {
        if remove_side_condition(battle, side, condition) {
            let of = of.map(|p| actor(battle, p));
            battle.emit(BattleEvent::HazardEnd { side, condition, from, of });
            any = true;
        }
    }
    any
}

/// The move's `onHit` (`runMoveEffects`): Defog and Tidy Up. `None` for
/// other moves (`undefined`).
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Option<bool> {
    match move_id {
        ids::DEFOG => Some(defog(battle, target, user)),
        ids::TIDY_UP => Some(tidy_up(battle, user)),
        _ => None,
    }
}

/// Defog: lowers the target's evasion, clears the target side's screens,
/// Safeguard, Mist and hazards and the user side's hazards, and the terrain.
/// Only the evasion drop and cleared hazards count as success.
fn defog(battle: &mut Battle, target: Position, user: Position) -> bool {
    // No evasion drop through a substitute, unless the move infiltrates
    // (Infiltrator).
    let evasion = StatBoosts::new([0, 0, 0, 0, 0, 0, -1]);
    let behind_sub = battle.state.at(target).is_some_and(|m| m.volatiles.has(Volatiles::SUBSTITUTE))
        && !battle.move_state.infiltrates;
    let mut success = !behind_sub && boosts::boost(battle, target, evasion, Some(user), false, false);
    let screens = [
        SideCondition::Reflect,
        SideCondition::LightScreen,
        SideCondition::AuroraVeil,
        SideCondition::Safeguard,
        SideCondition::Mist,
    ];
    for condition in screens.into_iter().chain(HAZARDS) {
        if remove_side_condition(battle, target.side, condition) && condition.is_hazard() {
            let of = actor(battle, user);
            battle.emit(BattleEvent::HazardEnd { side: target.side, condition, from: Some(ids::DEFOG), of: Some(of) });
            success = true;
        }
    }
    success |= clear_hazards(battle, user.side, Some(ids::DEFOG), Some(user));
    field::clear_terrain(battle);
    success
}

/// Tidy Up: clears the hazards on both sides (user's first), then raises
/// the user's Attack and Speed.
fn tidy_up(battle: &mut Battle, user: Position) -> bool {
    // Every active Pokémon's substitute ends first.
    let mut success = false;
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            success |= super::conditions::remove(battle, Position::new(side, slot as u8), Volatiles::SUBSTITUTE);
        }
    }
    for side in [user.side, user.side.foe()] {
        success |= clear_hazards(battle, side, None, None);
    }
    if success {
        let user_actor = actor(battle, user);
        battle.emit(BattleEvent::MoveActivate { actor: user_actor, move_id: ids::TIDY_UP, of: None });
    }
    let boosted = boosts::boost(battle, user, StatBoosts::new([1, 0, 0, 0, 1, 0, 0]), Some(user), false, true);
    boosted || success
}

/// `onAfterHit`: Rapid Spin and Mortal Spin clear
/// the user side's hazards; Ceaseless Edge and Stone Axe set Spikes and
/// Stealth Rock on the target's side.
pub(crate) fn after_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) {
    // `if (!move.hasSheerForce)`: Sheer Force removed these moves'
    // secondaries, and with them their hazard effects.
    if battle.move_state.has_sheer_force {
        return;
    }
    match move_id {
        ids::RAPID_SPIN | ids::MORTAL_SPIN => {
            // Leech Seed (no `onEnd`) ends with Rapid Spin's own message.
            if super::conditions::remove(battle, user, Volatiles::LEECH_SEED) {
                let of = actor(battle, user);
                let event = BattleEvent::EffectEndFrom { actor: of, effect: VolatileEffect::LeechSeed, move_id, of };
                battle.emit(event);
            }
            clear_hazards(battle, user.side, Some(move_id), Some(user));
            // Partial trapping's `onEnd` logs its `-end`.
            super::conditions::remove(battle, user, Volatiles::PARTIALLY_TRAPPED);
        }
        // `onAfterHit` (no `source.hp` check), then
        // `side.addSideCondition(...)` with the target as source. Only the
        // Champions mod calls `onAfterHit` after the user fainted (Rough
        // Skin), and then the hazard is set; `onAfterSubDamage` keeps the
        // check (`field::after_sub_damage`).
        ids::CEASELESS_EDGE | ids::STONE_AXE => {
            let hazard = if move_id == ids::CEASELESS_EDGE { "spikes" } else { "stealthrock" };
            field::add_side_condition(battle, user.side.foe(), hazard, target);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::handlers::run_switch_in;
    use crate::testing::sample_team;
    use crate::{BattleOptions, Format, Seed};

    #[test]
    fn stealth_rock_and_spikes_hurt_on_switch_in() {
        let format = Format::gen9_singles();
        let teams = [sample_team(&format), sample_team(&format)];
        let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
        let (user, pos) = (Position::new(SideId::P1, 0), Position::new(SideId::P2, 0));
        field::add_side_condition(&mut b, SideId::P2, "stealthrock", user);
        field::add_side_condition(&mut b, SideId::P2, "spikes", user);
        field::add_side_condition(&mut b, SideId::P2, "spikes", user);
        let max = b.state.at(pos).unwrap().max_hp();
        run_switch_in(&mut b, &[pos], &[user, pos]);
        // Incineroar is weak to Rock (1/4), then two layers of Spikes (4/24).
        assert_eq!(b.state.at(pos).unwrap().hp, max - max / 4 - 4 * max / 24);
        // Rapid Spin clears them, in its order.
        b.log.drain();
        after_hit(&mut b, user, pos, ids::RAPID_SPIN);
        let cleared: Vec<_> = b
            .events()
            .iter()
            .filter_map(|e| match e {
                BattleEvent::HazardEnd { condition, .. } => Some(*condition),
                _ => None,
            })
            .collect();
        assert_eq!(cleared, [SideCondition::Spikes, SideCondition::StealthRock]);
        assert!(b.state.side(SideId::P2).conditions.order.is_empty());
    }
}
