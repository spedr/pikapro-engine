//! Damage variants and the hit loop's extras.
//!
//! Showdown references (`sim/battle-actions.ts`): `hitStepAccuracy` (OHKO
//! accuracy), `hitStepMoveHitLoop` (multi-hit, `-hitcount`, recoil),
//! `getDamage`'s early returns (`ohko`, `damage`, `damageCallback`),
//! `runMoveEffects` (`heal`), `applyRecoilDamage`; `spreadDamage` in
//! `sim/battle.ts` (drain); `selfdestruct` in `useMoveInner`.
//!
//! Contract (called by the move pipeline in `moves.rs`):
//! - [`hit_step_accuracy`]: the accuracy hit step for one target.
//! - [`self_destruct`]: `selfdestruct: 'always'` before the hit steps.
//! - [`hit_count`]: how many times the move hits (multi-hit draws here).
//! - [`multi_accuracy`]: the per-hit accuracy check of `multiaccuracy` moves.
//! - [`base_power`]: simple base power callbacks of multi-hit moves.
//! - [`fixed_damage`]: damage that bypasses the formula, if any.
//! - [`drain`]: heal the user after damage to one target.
//! - [`heal_move`]: the `heal` effect of `runMoveEffects`.
//! - [`after_hit_loop`]: `-hitcount`, recoil, Struggle recoil and `-ohko`.

use pikapro_data::gen9::moves as ids;
use pikapro_data::gen9::{abilities, species};
use pikapro_data::{BoostId, Event, Fraction, MoveCategory, MoveData, MoveId, MoveTarget, Type};

use super::events::{self, Relay};
use super::manipulation::has_type;
use super::{DamageSource, HealSource, actor, apply_damage, apply_heal, moves};
use crate::battle::Battle;
use crate::event::BattleEvent;
use crate::state::{PokemonState, Position};

/// Showdown's `hitStepAccuracy` for one target: OHKO immunity and accuracy,
/// accuracy/evasion stages, then the accuracy roll. Returns whether the
/// target is hit; logs `-immune` / `-miss` otherwise.
pub(crate) fn hit_step_accuracy(
    battle: &mut Battle,
    user: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
) -> bool {
    let attacker = battle.state.at(user).expect("user is active");
    let defender = battle.state.at(target).expect("target is active");
    let acc = if data.ohko && super::multiturn::is_semi_invulnerable(battle, target) {
        // A semi-invulnerable target (reached through No Guard or Lock-On)
        // keeps the move's own accuracy, without the level check.
        data.accuracy.map(u32::from)
    } else if data.ohko {
        let ice = move_id == ids::SHEER_COLD;
        let mut acc = if ice && !has_type(attacker, Type::Ice) { 20 } else { 30 };
        if attacker.level >= defender.level && !(ice && has_type(defender, Type::Ice)) {
            acc += (attacker.level - defender.level) as u32;
        } else {
            let target_actor = actor(battle, target);
            battle.emit(BattleEvent::OhkoImmune { target: target_actor });
            return false;
        }
        Some(acc)
    } else {
        // `runEvent('ModifyAccuracy', target, pokemon, move, accuracy)`
        // (Gravity, Compound Eyes, Hustle, Sand Veil, BrightPowder, ...),
        // then the stages (after `ModifyBoost`: Unaware).
        battle.move_state.active_target = Some(target);
        // `move.accuracy = true` set by a target's ability (Flash Fire) makes
        // the move hit the rest of its targets.
        let relay = match data.accuracy {
            Some(a) if !battle.move_state.accuracy_true => Relay::Num(a as i32),
            _ => Relay::True,
        };
        match events::move_event(battle, Event::ModifyAccuracy, target, Some(user), move_id, data, relay) {
            Relay::Num(acc) => {
                let mut stage = 0i8;
                if !battle.move_state.ignore_accuracy {
                    let b = battle.state.at(user).expect("user is active").boosts;
                    stage = events::boost_stage(battle, user, b, BoostId::Accuracy).clamp(-6, 6);
                }
                // `ignoreEvasion` (Sacred Sword; Keen Eye) skips the target's
                // evasion.
                if !super::power::ignores_evasion(move_id) && !battle.move_state.ignore_evasion {
                    let b = battle.state.at(target).expect("target is active").boosts;
                    stage =
                        (stage - events::boost_stage(battle, target, b, BoostId::Evasion).clamp(-6, 6)).clamp(-6, 6);
                }
                Some(accuracy_with_stage(acc.max(0) as u32, stage))
            }
            _ => None,
        }
    };
    // Toxic from a Poison type (Gen 8+) and a self-targeting status move
    // hit without the `Accuracy` event. Otherwise it runs even for a move
    // that can't miss (its tied handlers draw): Minimize, Lock-On, Glaive
    // Rush, No Guard, ...
    // A self-targeting status move used by a semi-invulnerable Pokémon
    // (a Dancer copy mid-Fly, let through by No Guard) runs it.
    let self_status = data.target == MoveTarget::User
        && data.category == MoveCategory::Status
        && !super::multiturn::is_semi_invulnerable(battle, target);
    let acc = if self_status || super::status::always_hits(battle, user, move_id) {
        None
    } else {
        let relay = acc.map_or(Relay::True, |a| Relay::Num(a as i32));
        match events::move_event(battle, Event::Accuracy, target, Some(user), move_id, data, relay) {
            Relay::Num(n) => Some(n.max(0) as u32),
            Relay::True => None,
            _ => Some(0),
        }
    };
    match acc {
        None => true,
        Some(acc) if battle.rng.chance(acc, 100) => true,
        miss => {
            // A `smartTarget` move's first miss is silent and
            // turns `smartTarget` off.
            if !super::order::smart_target_missed(battle) {
                let (user_actor, target_actor) = (actor(battle, user), actor(battle, target));
                super::moves::attr_miss(battle);
                battle.emit(BattleEvent::Miss { user: user_actor, target: target_actor });
            }
            // Blunder Policy.
            if !data.ohko && miss.is_some() {
                super::items::consumed::after_miss(battle, user);
            }
            false
        }
    }
}

/// Showdown's accuracy stage table: `[1, 4/3, 5/3, 2, 7/3, 8/3, 3]`.
fn accuracy_with_stage(acc: u32, stage: i8) -> u32 {
    const NUM: [u32; 7] = [3, 4, 5, 6, 7, 8, 9];
    if stage >= 0 { acc * NUM[stage as usize] / 3 } else { acc * 3 / NUM[(-stage) as usize] }
}

/// `selfdestruct: 'always'` (Explosion): the user faints as it uses the
/// move, before any hit step (`useMoveInner`). Moves that only faint the
/// user on a hit (`'ifHit'`) list `selfdestruct` as unmodelled.
#[inline(always)]
pub(crate) fn self_destruct(battle: &mut Battle, user: Position, data: &MoveData) {
    if data.self_destruct && !data.unmodelled.contains(&"selfdestruct") {
        super::faint(battle, user);
    }
}

/// Number of hits (`move.multihit`). Showdown draws for ranged counts: 2-5
/// hits from its Gen 5+ distribution, other ranges uniformly.
pub(crate) fn hit_count(battle: &mut Battle, user: Position, data: &MoveData) -> u8 {
    const TWO_TO_FIVE: [u8; 20] = [2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 5, 5, 5];
    let hits = match data.multihit {
        None => 1,
        Some((2, 5)) => {
            let hits = *battle.rng.sample(&TWO_TO_FIVE);
            // Loaded Dice can reroll a low count.
            return super::items::held::multihit_count(battle, user, hits);
        }
        Some((min, max)) if min == max => min,
        Some((min, max)) => battle.rng.random_range(min as u32, max as u32 + 1) as u8,
    };
    // Loaded Dice: Population Bomb's 10 hits.
    if hits == 10 { super::items::held::multihit_count(battle, user, hits) } else { hits }
}

/// The accuracy check before each hit after the first of a `multiaccuracy`
/// move (Population Bomb, Triple Axel), from `hitStepMoveHitLoop`. Returns
/// whether the hit lands; a miss silently ends the move.
pub(crate) fn multi_accuracy(
    battle: &mut Battle,
    user: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
    hit: u8,
) -> bool {
    // Skill Link and Loaded Dice delete `move.multiaccuracy`.
    if hit == 1
        || !matches!(move_id, ids::POPULATION_BOMB | ids::TRIPLE_AXEL | ids::TRIPLE_KICK)
        || battle.move_state.no_multiaccuracy
    {
        return true;
    }
    let Some(acc) = data.accuracy else { return true };
    // `accuracy *= boostTable[boost]` for the user's accuracy, then
    // `/= boostTable[boost]` for the target's evasion (both after
    // `ModifyBoost`), as exact fractions: the table is `(3 + n) / 3` for
    // stage `n`.
    let attacker_boosts = battle.state.at(user).expect("user is active").boosts;
    let defender_boosts = battle.state.at(target).expect("target is active").boosts;
    let accuracy = if battle.move_state.ignore_accuracy {
        0
    } else {
        events::boost_stage(battle, user, attacker_boosts, BoostId::Accuracy).clamp(-6, 6)
    };
    let evasion = if battle.move_state.ignore_evasion || super::power::ignores_evasion(move_id) {
        0
    } else {
        events::boost_stage(battle, target, defender_boosts, BoostId::Evasion).clamp(-6, 6)
    };
    // Accuracy 90 (every such move) gives the same comparisons as
    // Showdown's floating-point table at every stage combination.
    let (mut num, mut den) = (acc as u32, 1u32);
    let mut scale = |stage: i8, up: bool| {
        let n = 3 + stage.unsigned_abs() as u32;
        if up {
            (num, den) = (num * n, den * 3);
        } else {
            (num, den) = (num * 3, den * n);
        }
    };
    if accuracy != 0 {
        scale(accuracy, accuracy > 0);
    }
    if evasion != 0 {
        scale(evasion, evasion < 0);
    }
    // BattleActions.hitStepMoveHitLoop always runs ModifyAccuracy,
    // including its handler-sort draws. Battle.runEvent applies the final
    // chain modifier only to a nonnegative integer relay. These physical
    // multiaccuracy moves' handlers only chain modifiers (Wonder Skin's
    // numeric override is status-only), so use a numeric relay to run the
    // handlers, retaining the exact fraction when it is not an integer.
    let relay = Relay::Num((num / den) as i32);
    match events::move_event(battle, Event::ModifyAccuracy, target, Some(user), move_id, data, relay) {
        Relay::Num(n) if num % den == 0 => (num, den) = (n.max(0) as u32, 1),
        Relay::Num(_) => {}
        _ => return true,
    }
    // `runEvent('Accuracy')`: Minimize, Lock-On, Glaive Rush, No Guard:
    // `true`.
    let relay = Relay::Num((num / den) as i32);
    match events::move_event(battle, Event::Accuracy, target, Some(user), move_id, data, relay) {
        Relay::True => return true,
        Relay::Num(n) if num % den == 0 => (num, den) = (n.max(0) as u32, 1),
        _ => {}
    }
    // `randomChance(accuracy, 100)`: `random(100) < accuracy`.
    battle.rng.random(100) * den < num
}

/// Simple `basePowerCallback`s of multi-hit moves: Triple Axel and Triple
/// Kick grow with each hit; Water Shuriken is stronger from Ash-Greninja.
pub(crate) fn base_power(attacker: &PokemonState, move_id: MoveId, data: &MoveData, hit: u8) -> u16 {
    match move_id {
        ids::TRIPLE_AXEL => 20 * hit as u16,
        ids::TRIPLE_KICK => 10 * hit as u16,
        // `!pokemon.transformed`.
        ids::WATER_SHURIKEN
            if attacker.species == species::GRENINJA_ASH
                && attacker.ability == abilities::BATTLE_BOND
                && !attacker.volatiles.has(crate::state::Volatiles::TRANSFORMED) =>
        {
            data.base_power as u16 + 5
        }
        _ => data.base_power as u16,
    }
}

/// Damage that does not use the formula (`getDamage`'s early returns:
/// `ohko`, `damageCallback`, `damage`), or `None` to use the formula.
pub(crate) fn fixed_damage(
    battle: &mut Battle,
    user: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
) -> Option<u32> {
    let attacker = battle.state.at(user).expect("user is active");
    let defender = battle.state.at(target).expect("target is active");
    if data.ohko {
        return Some(defender.max_hp() as u32);
    }
    Some(match move_id {
        // `damageCallback`: half the target's HP, at least 1.
        ids::SUPER_FANG | ids::RUINATION => (defender.hp as u32 / 2).max(1),
        // `damageCallback`: the user's HP, and the user faints.
        ids::FINAL_GAMBIT => {
            let hp = attacker.hp as u32;
            super::faint(battle, user);
            hp
        }
        // `damage: 'level'`.
        ids::SEISMIC_TOSS | ids::NIGHT_SHADE => attacker.level as u32,
        // `damage: <number>`.
        ids::DRAGON_RAGE => 40,
        ids::SONIC_BOOM => 20,
        _ => return None,
    })
}

/// `Math.round(value * n / d)` for non-negative integers.
fn round_fraction(value: u32, f: Fraction) -> u32 {
    let (n, d) = (f.numerator as u32, f.denominator as u32);
    (2 * value * n + d) / (2 * d)
}

/// Drain after `dealt` damage to one target (`spreadDamage`, Gen 5+):
/// heals the user by `round(dealt * drain)`.
pub(crate) fn drain(battle: &mut Battle, user: Position, target: Position, dealt: u16, data: &MoveData) {
    let Some(ratio) = data.drain else { return };
    if dealt == 0 {
        return;
    }
    let of = actor(battle, target);
    apply_heal(battle, user, round_fraction(dealt as u32, ratio), HealSource::Drain { of });
}

/// The `heal` effect of `runMoveEffects` (Recover, Life Dew). Returns
/// whether it did something; at full HP it fails with `-fail|...|heal`.
pub(crate) fn heal_move(battle: &mut Battle, target: Position, data: &MoveData) -> bool {
    let Some(ratio) = data.heal else { return true };
    let Some(mon) = battle.state.at(target) else { return true };
    if mon.hp >= mon.max_hp() {
        let target_actor = actor(battle, target);
        battle.emit(BattleEvent::HealFail { target: target_actor });
        // `attrLastMove('[still]')`.
        battle.flush_rng_trace();
        battle.attr_last_move(crate::event::MoveAttr::Still);
        return false;
    }
    // `battle.heal`: `TryHeal` (Heal Block) stops it, `-fail` from the
    // user.
    let amount = round_fraction(mon.max_hp() as u32, ratio);
    if super::heal_move(battle, target, amount).is_none() {
        let user = battle.active_move.map_or(target, |(u, _)| u);
        moves::fail_still(battle, user);
        return false;
    }
    true
}

/// After all hits (`hitStepMoveHitLoop`, after `faintMessages`):
/// `-hitcount`, recoil (`applyRecoilDamage`), then `-ohko`.
pub(crate) fn after_hit_loop(
    battle: &mut Battle,
    user: Position,
    first_target: Position,
    move_id: MoveId,
    data: &MoveData,
    hits: u8,
    total_damage: u32,
) {
    // `smartTarget` moves (Dragon Darts) don't report their hits.
    if data.multihit.is_some() && !data.unmodelled.contains(&"smartTarget") {
        let target = actor(battle, first_target);
        battle.emit(BattleEvent::HitCount { target, hits });
    }
    if total_damage > 0 {
        apply_recoil(battle, user, move_id, data, 2 * total_damage);
    }
    if data.ohko && battle.state.side(first_target.side).team[first_target.slot as usize].is_fainted() {
        battle.emit(BattleEvent::Ohko);
    }
}

/// `applyRecoilDamage` for damage dealt given in half points (`halves`: a
/// substitute's damage can end in a half): Struggle's 1/4 of max HP,
/// `mindBlownRecoil` (Steel Beam) and `chloroblastRecoil`, or the move's
/// `recoil` fraction of the damage, rounded, at least 1. Then
/// `runEvent('EmergencyExit', pokemon, pokemon)` when the recoil took the
/// user to half HP or below.
pub(crate) fn apply_recoil(battle: &mut Battle, user: Position, move_id: MoveId, data: &MoveData, halves: u32) {
    let Some(mon) = battle.state.at(user) else { return };
    let hp_before = mon.hp;
    if move_id == ids::STRUGGLE {
        // `directDamage`: nothing for a user without HP.
        if mon.is_fainted() {
            return;
        }
        let recoil = ((mon.max_hp() as u32 + 2) / 4).max(1);
        apply_damage(battle, user, recoil, DamageSource::Recoil);
    } else if let Some((recoil, source)) = super::power::recoil(battle, user, move_id) {
        apply_damage(battle, user, recoil, source);
    } else if let Some(ratio) = data.recoil {
        // `clampIntRange(Math.round(damage * recoil[0] / recoil[1]), 1)`.
        let (n, d) = (ratio.numerator as u32, ratio.denominator as u32);
        let recoil = ((halves * n + d) / (2 * d)).max(1);
        apply_damage(battle, user, recoil, DamageSource::MoveRecoil);
    } else {
        return;
    }
    moves::emergency_exit_at_any_hp(battle, user, Some(user), hp_before);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BattleActions.hitStepMoveHitLoop runs ModifyAccuracy at 67.5%
    /// too: the two ability handlers tie, drawing before the hit roll.
    #[test]
    fn fractional_multiaccuracy_still_sorts_handlers() {
        use crate::{BattleOptions, Format, PokemonSet, Seed, SideId, Team};
        let set = |ability| PokemonSet::new(species::MEW).with_ability(ability).with_moves(&[ids::TRIPLE_AXEL]);
        let teams = [Team::new(vec![set(abilities::COMPOUND_EYES)]), Team::new(vec![set(abilities::SAND_VEIL)])];
        let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(49_000_100))).unwrap();
        let (user, target) = (Position::new(SideId::P1, 0), Position::new(SideId::P2, 0));
        b.state.at_mut(user).unwrap().boosts.accuracy = -1;
        let mut expected = b.rng.clone();
        expected.random(2);
        let roll = expected.random(100);
        let data = *b.format.dex.move_(ids::TRIPLE_AXEL);
        let hit = multi_accuracy(&mut b, user, target, ids::TRIPLE_AXEL, &data, 2);
        assert_eq!(hit, roll * 4 < 270);
        assert_eq!(b.rng, expected);
    }

    #[test]
    fn rounds_like_math_round() {
        let half = Fraction { numerator: 1, denominator: 2 };
        let third = Fraction { numerator: 33, denominator: 100 };
        assert_eq!(round_fraction(1, half), 1); // 0.5 rounds up
        assert_eq!(round_fraction(7, half), 4);
        assert_eq!(round_fraction(100, third), 33);
        assert_eq!(round_fraction(150, third), 50); // 49.5
        assert_eq!(round_fraction(1, third), 0);
    }
}
