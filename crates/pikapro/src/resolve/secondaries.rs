//! Self effects and secondary effects of a hit.
//!
//! Showdown references (`sim/battle-actions.ts`): `selfDrops` (a move's
//! `self` effects: `self.boosts`, `self.volatileStatus`) and `secondaries`
//! (one `random(100)` per secondary per target, then `moveHit` with the
//! secondary's status / volatile / boosts / self).
//!
//! Contract: called by `spread_move_hit` after `runMoveEffects`, in
//! Showdown's order. Status and volatiles go through
//! `status::try_set_status` and `volatiles::add_volatile`; boosts through
//! `boosts::boost`.
//!
//! Showdown runs these effects through a nested `moveHit` (`spreadMoveHit`
//! with the effect as `moveData`). For the effects modelled here that nested
//! hit reduces to `runMoveEffects` (boosts, then status, then volatile)
//! followed by the effect's own `self` boosts, which is what this module
//! does directly.

use pikapro_data::{Event, MoveData, MoveId, Secondary};

use super::targeting::Targets;
use super::{boosts, conditions, events, manipulation, pledges, status, volatiles};
use crate::battle::Battle;
use crate::state::Position;

/// Showdown's `selfDrops` for a move's primary `self` effects (`isSecondary`
/// false).
///
/// With `self.boosts`, each remaining target draws `random(100)` (the
/// effect has no chance, so it always applies) and, unless the move is
/// multi-hit, sets `move.selfDropped` so a spread move drops the user's
/// stats once. Without boosts (`self.volatileStatus` only), the effect runs
/// once per target and draws nothing, as does a `self.onHit` (Burn Up,
/// Double Shock: `manipulation::self_on_hit`).
pub(crate) fn self_drops(battle: &mut Battle, targets: &Targets, user: Position, move_id: MoveId, data: &MoveData) {
    if data.self_boosts.is_none() && data.self_volatile.is_none() && !manipulation::has_self_on_hit(move_id) {
        return;
    }
    for _ in targets.iter() {
        if data.self_boosts.is_some() {
            // `self.chance` (Diamond Storm) is not in the data.
            if battle.rng.random(100) < super::power::self_chance(battle) {
                self_hit(battle, user, move_id, data);
            }
            // (Parental Bond's `move.multihit = 2` is in `data` already.)
            if data.multihit.is_none() {
                // `move.selfDropped`.
                break;
            }
        } else {
            self_hit(battle, user, move_id, data);
        }
    }
}

/// `moveHit(source, source, move, moveData.self, false, true)`: the self
/// effect's `runMoveEffects` (boosts, then volatile, then its `onHit`).
fn self_hit(battle: &mut Battle, user: Position, move_id: MoveId, data: &MoveData) {
    if let Some(b) = data.self_boosts {
        boosts::boost(battle, user, b, Some(user), false, true);
    }
    if let Some(v) = data.self_volatile {
        volatiles::add_volatile(battle, user, v, Some(user), false);
    }
    manipulation::self_on_hit(battle, user, move_id);
}

/// Showdown's `secondaries`: for each target (fainted or not), each
/// secondary draws `random(100)` and applies when the roll is below its
/// chance.
pub(crate) fn secondaries(battle: &mut Battle, targets: &Targets, user: Position, data: &MoveData) {
    // King's Rock's and Razor Fang's flinch comes after the
    // move's own secondaries.
    let extra = battle.move_state.added_flinch.then_some(&ADDED_FLINCH);
    if data.secondaries.is_empty() && extra.is_none() {
        return;
    }
    let n = (data.secondaries.len() + extra.is_some() as usize).min(8);
    let all = ((1u16 << n) - 1) as u8;
    for &t in targets.iter() {
        // `runEvent('ModifySecondaries', target, source, moveData,
        // secondaries)`: Shield Dust and Covert Cloak keep only the `self`
        // ones. Showdown filters the array it then rolls, so bit `i` is the
        // `i`-th secondary [`ordered`] yields (the roll order), not
        // `data.secondaries[i]`: with Fling the pushed flinch is bit 0.
        let kept = match super::active_dex_move(battle) {
            Some((_, move_id)) => {
                let relay = events::Relay::Secondaries(all);
                match events::move_event(battle, Event::ModifySecondaries, t, Some(user), move_id, data, relay) {
                    events::Relay::Secondaries(m) => m,
                    _ => all,
                }
            }
            None => all,
        };
        for (i, (secondary, own)) in ordered(battle, data, extra).enumerate() {
            if i < 8 && kept & (1 << i) == 0 {
                continue;
            }
            let roll = battle.rng.random(100);
            if roll < chance(battle, user, secondary, own) {
                secondary_hit(battle, t, user, secondary);
            }
        }
    }
}

/// The `ModifySecondaries` mask of the secondaries that affect the user
/// (`secondaries.filter(effect => !!effect.self)`: Shield Dust's and
/// Covert Cloak's), bit `i` for the `i`-th one [`secondaries`] rolls. The
/// bits follow [`ordered`], not `data.secondaries`, so the mask stays
/// right when a pushed flinch comes first (Fling) or a handler keeps some
/// of the move's own secondaries.
pub(crate) fn with_self(battle: &Battle, data: &MoveData) -> u8 {
    let extra = battle.move_state.added_flinch.then_some(&ADDED_FLINCH);
    ordered(battle, data, extra)
        .take(8)
        .enumerate()
        .filter(|(_, (secondary, _))| secondary.self_boosts.is_some())
        .fold(0, |mask, (i, _)| mask | 1 << i)
}

/// The move's secondaries in Showdown's order, each with whether it is the
/// move's own (not a pushed flinch): the pushed flinch (Stench, King's
/// Rock, Razor Fang: `onModifyMove`) comes after the move's own ones, but
/// before the one Fling pushes in its `onPrepareHit`, which runs later.
fn ordered<'a>(
    battle: &Battle,
    data: &'a MoveData,
    extra: Option<&'a Secondary>,
) -> impl Iterator<Item = (&'a Secondary, bool)> + 'a {
    let fling = battle.move_state.fling.is_some();
    let (before, after) = if fling { (extra, None) } else { (None, extra) };
    let before = before.into_iter().map(|s| (s, false));
    let own = data.secondaries.iter().map(|s| (s, true));
    before.chain(own).chain(after.into_iter().map(|s| (s, false)))
}

/// The secondary's chance after `ModifyMove`: the rainbow (Water Pledge)
/// and Serene Grace double it. The pushed flinch (not `own`: Stench, King's Rock, Razor Fang) is pushed
/// after the rainbow's `onModifyMove` (priority 0) and before Serene
/// Grace's (-2).
fn chance(battle: &Battle, user: Position, secondary: &Secondary, own: bool) -> u32 {
    let chance = (secondary.chance as u32) << battle.move_state.chance_doublings;
    let flinch = secondary.volatile_status == Some("flinch");
    if own { pledges::secondary_chance(battle, user, chance, flinch) } else { chance }
}

/// `secondaries` for a target whose substitute took the hit (Showdown's
/// `null` target): no `ModifySecondaries` handler of the target runs, each
/// secondary still rolls, and its nested `moveHit` has no target, so only
/// its `self` boosts apply.
pub(crate) fn substitute_secondaries(battle: &mut Battle, user: Position, data: &MoveData) {
    let extra = battle.move_state.added_flinch.then_some(&ADDED_FLINCH);
    for (secondary, own) in ordered(battle, data, extra) {
        let roll = battle.rng.random(100);
        if roll < chance(battle, user, secondary, own)
            && let Some(b) = secondary.self_boosts
        {
            boosts::boost(battle, user, b, Some(user), true, true);
        }
    }
}

/// The flinch that Stench, King's Rock and Razor Fang push onto a move's
/// secondaries (`onModifyMove`, priority -1: after the rainbow's, before
/// Serene Grace's).
pub(crate) const ADDED_FLINCH: Secondary = Secondary {
    chance: 10,
    status: None,
    volatile_status: Some("flinch"),
    boosts: None,
    self_boosts: None,
    unmodelled: &[],
};

/// `moveHit(target, source, move, secondary, true, false)`: the secondary's
/// `runMoveEffects` on `target`, then its `self` boosts through `selfDrops`
/// (with `isSecondary`, so no roll).
fn secondary_hit(battle: &mut Battle, target: Position, user: Position, secondary: &Secondary) {
    // A target knocked out by this hit is still passed in: `boost`,
    // `trySetStatus` and `addVolatile` each fail on a Pokémon at 0 HP.
    if let Some(b) = secondary.boosts {
        boosts::boost(battle, target, b, Some(user), true, false);
    }
    if let Some(s) = secondary.status {
        status::try_set_status(battle, target, s, Some(user), true);
    }
    if let Some(v) = secondary.volatile_status {
        volatiles::add_volatile(battle, target, v, Some(user), true);
    }
    // The secondary's `onHit` (Spirit Shackle, Throat Chop).
    if !secondary.unmodelled.is_empty() {
        conditions::secondary_on_hit(battle, target, user);
    }
    // Showdown skips `selfDrops` when the secondary's other effects all
    // failed; no Gen 9 secondary has both `self` and other effects.
    if let Some(b) = secondary.self_boosts {
        boosts::boost(battle, user, b, Some(user), true, true);
    }
    // The secondary's `onHit` (Tri Attack, Dire Claw, ...).
    if !secondary.unmodelled.is_empty() {
        super::manipulation::secondary_on_hit(battle, target, user);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Battle, BattleOptions, Format, PokemonSet, Seed, Team};
    use pikapro_data::gen9::{abilities as a, items, moves, species as s};

    /// Shield Dust's and Covert Cloak's mask follows the roll order: a
    /// `self` secondary is bit 0 when it rolls first and bit 1 after
    /// Fling's pushed flinch (which rolls before the move's own).
    #[test]
    fn with_self_follows_the_roll_order() {
        let set = |sp| PokemonSet::new(sp).with_ability(a::NO_ABILITY).with_moves(&[moves::SPLASH]);
        let teams = [Team::new(vec![set(s::MACHAMP)]), Team::new(vec![set(s::MACHAMP)])];
        let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(1))).unwrap();
        // Power-Up Punch: one secondary, `self` (+1 Attack).
        let data = *b.format.dex.move_(moves::POWER_UP_PUNCH);
        assert!(data.secondaries.len() == 1 && data.secondaries[0].self_boosts.is_some());
        let rolled = |b: &Battle| {
            let extra = b.move_state.added_flinch.then_some(&ADDED_FLINCH);
            ordered(b, &data, extra).map(|(s, own)| (s.self_boosts.is_some(), own)).collect::<Vec<_>>()
        };
        assert_eq!(with_self(&b, &data), 0b1);
        // King's Rock's flinch rolls after the move's own secondaries.
        b.move_state.added_flinch = true;
        assert_eq!(rolled(&b), [(true, true), (false, false)]);
        assert_eq!(with_self(&b, &data), 0b01);
        // Fling (a King's Rock): its flinch is pushed before the move's own.
        b.move_state.fling = Some(items::KINGS_ROCK);
        assert_eq!(rolled(&b), [(false, false), (true, true)]);
        assert_eq!(with_self(&b, &data), 0b10);
    }
}
