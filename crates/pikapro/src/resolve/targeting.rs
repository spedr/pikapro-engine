//! Target selection, ported from Showdown's `validTargetLoc`, `getAtLoc`,
//! `getRandomTarget`, `getTarget` (`sim/battle.ts`) and `getMoveTargets`
//! (`sim/pokemon.ts`).
//!
//! Showdown addresses targets by *location* relative to the user: `n > 0` is
//! the foe in slot `n - 1`, `n < 0` is the user's side slot `-n - 1`, `0` is
//! "no choice". Several of these functions draw from the RNG (`randomFoe`
//! samples even from a single candidate), and they are called from queue
//! resolution, speed calculation and move execution, so each call site
//! matters for conformance.

use pikapro_data::{MoveData, MoveFlags, MoveTarget};

use crate::battle::Battle;
use crate::choice::Target;
use crate::state::{Position, Volatiles};
use crate::util::StackVec;

/// A target location relative to the user (Showdown's `targetLoc`).
pub(crate) type Loc = i8;

/// Up to 4: field moves (`all`) list every active Pokémon.
pub(crate) type Targets = StackVec<Position, 4>;

/// The location of a player's chosen [`Target`].
pub(crate) fn loc_of_choice(target: Option<Target>) -> Loc {
    match target {
        Some(Target::Foe(n)) => n as Loc + 1,
        Some(Target::Ally(n)) => -(n as Loc + 1),
        None => 0,
    }
}

/// Showdown's `getLocOf`.
pub(crate) fn loc_of(user: Position, target: Position) -> Loc {
    let n = target.slot as Loc + 1;
    if target.side == user.side { -n } else { n }
}

/// Whether slot `pos` has ever been filled. The Pokémon there stays in
/// place after fainting until it is replaced, as in Showdown's `side.active`.
fn held(battle: &Battle, pos: Position) -> bool {
    (pos.slot as usize) < battle.format.active_per_side()
        && (pos.slot as usize) < battle.state.side(pos.side).team.len()
}

/// Whether the Pokémon in slot `pos` has no HP. A slot a stale switch left
/// (`OrderState::stale_active`) still holds the fainted Pokémon that
/// moved out of it, though its team position now holds a benched one.
#[inline(always)]
pub(crate) fn is_fainted(battle: &Battle, pos: Position) -> bool {
    battle.state.side(pos.side).team[pos.slot as usize].is_fainted()
        || battle.order.stale_slots & 1 << (pos.side.index() * 2 + pos.slot as usize) != 0
}

fn alive(battle: &Battle, pos: Position) -> bool {
    held(battle, pos) && !is_fainted(battle, pos)
}

/// Showdown's `getAtLoc`: the Pokémon at a location, fainted or not.
pub(super) fn at_loc(battle: &Battle, user: Position, loc: Loc) -> Option<Position> {
    if loc == 0 {
        return None;
    }
    let side = if loc < 0 { user.side } else { user.side.foe() };
    let pos = Position::new(side, loc.unsigned_abs() - 1);
    held(battle, pos).then_some(pos)
}

/// Live Pokémon on `side`, left to right (Showdown's `side.allies()`).
fn live_on(battle: &Battle, side: crate::state::SideId) -> Targets {
    (0..battle.format.active_per_side() as u8).map(|s| Position::new(side, s)).filter(|&p| alive(battle, p)).collect()
}

/// Showdown's `adjacentAllies()`: live allies other than the user (every
/// ally is adjacent in singles and doubles).
pub(super) fn adjacent_allies(battle: &Battle, user: Position) -> Targets {
    if is_fainted(battle, user) {
        return Targets::new();
    }
    let mut out = live_on(battle, user.side);
    out.retain(|&p| p != user);
    out
}

/// Showdown's `validTargetLoc`.
#[inline]
pub(crate) fn valid_target_loc(battle: &Battle, loc: Loc, user: Position, target_type: MoveTarget) -> bool {
    valid_target_loc_in(battle.format.active_per_side() as i8, loc, user.slot, target_type)
}

/// [`valid_target_loc`] with `num_slots` active Pokémon per side, for a
/// user in `user_slot` (a `const fn`: the legal actions' table of
/// doubles' targets, [`crate::legal`], is built from it).
#[inline]
pub(crate) const fn valid_target_loc_in(num_slots: i8, loc: Loc, user_slot: u8, target_type: MoveTarget) -> bool {
    if loc == 0 {
        return true;
    }
    let source_loc = -(user_slot as i8 + 1);
    if loc.abs() > num_slots {
        return false;
    }
    let is_self = source_loc == loc;
    let is_foe = loc > 0;
    let across = -(num_slots + 1 - loc);
    let is_adjacent = if loc > 0 { (across - source_loc).abs() <= 1 } else { (loc - source_loc).abs() == 1 };
    match target_type {
        MoveTarget::RandomNormal | MoveTarget::Scripted | MoveTarget::Normal => is_adjacent,
        MoveTarget::AdjacentAlly => is_adjacent && !is_foe,
        MoveTarget::AdjacentAllyOrSelf => is_adjacent && !is_foe || is_self,
        MoveTarget::AdjacentFoe => is_adjacent && is_foe,
        MoveTarget::Any => !is_self,
        _ => false,
    }
}

/// Showdown's `getRandomTarget`.
pub(crate) fn get_random_target(battle: &mut Battle, user: Position, target_type: MoveTarget) -> Option<Position> {
    match target_type {
        MoveTarget::User
        | MoveTarget::All
        | MoveTarget::AllySide
        | MoveTarget::AllyTeam
        | MoveTarget::AdjacentAllyOrSelf => return Some(user),
        MoveTarget::AdjacentAlly => {
            if battle.format.active_per_side() == 1 {
                return None;
            }
            let allies = adjacent_allies(battle, user);
            return if allies.is_empty() { None } else { Some(*battle.rng.sample(&allies)) };
        }
        _ => {}
    }
    let first_foe = Position::new(user.side.foe(), 0);
    if battle.format.active_per_side() == 1 {
        return held(battle, first_foe).then_some(first_foe);
    }
    // `randomFoe() || foe.active[0]`: sampling draws even from one foe.
    if let Some(foe) = sample_live(battle, user.side.foe()) {
        return Some(foe);
    }
    held(battle, first_foe).then_some(first_foe)
}

/// `sample(live_on(side))` (`randomFoe()`) without building the list: the
/// same draw (`random(n)`, even for one), and none without a live Pokémon.
fn sample_live(battle: &mut Battle, side: crate::state::SideId) -> Option<Position> {
    let mut live = [Position::new(side, 0); crate::state::MAX_ACTIVE];
    let mut n = 0;
    for slot in 0..battle.format.active_per_side().min(crate::state::MAX_ACTIVE) as u8 {
        let pos = Position::new(side, slot);
        if alive(battle, pos) {
            live[n] = pos;
            n += 1;
        }
    }
    (n > 0).then(|| live[battle.rng.random(n as u32) as usize])
}

/// Showdown's `getTarget`: the Pokémon a move at `loc` is aimed at.
pub(crate) fn get_target(battle: &mut Battle, user: Position, target_type: MoveTarget, loc: Loc) -> Option<Position> {
    // `tracksTarget` (Stalwart, Propeller Tail) and `smartTarget` moves are
    // handled first by `order::get_target`, which knows the move.
    let self_loc = -(user.slot as Loc + 1);
    // Aiming at its own slot fails, except while locked into a two-turn
    // move or Rollout. A future move aimed there targets the
    // user (`order::get_target`, which knows the move).
    let locked =
        || battle.state.at(user).is_some_and(|m| m.volatiles.has(Volatiles::TWO_TURN_MOVE | Volatiles::ROLLOUT));
    if matches!(target_type, MoveTarget::AdjacentAlly | MoveTarget::Any | MoveTarget::Normal)
        && loc == self_loc
        && !locked()
    {
        return None;
    }
    if target_type != MoveTarget::RandomNormal
        && valid_target_loc(battle, loc, user, target_type)
        && let Some(target) = at_loc(battle, user, loc)
    {
        if !is_fainted(battle, target) {
            return Some(target);
        }
        if target.side == user.side {
            return Some(if target_type == MoveTarget::AdjacentAllyOrSelf { user } else { target });
        }
    }
    get_random_target(battle, user, target_type)
}

/// Showdown's `getMoveTargets`: every Pokémon the move will try to hit,
/// plus the target to show on the `|move|` line if it changed
/// (`retargetLastMove`).
pub(crate) fn get_move_targets(
    battle: &mut Battle,
    user: Position,
    data: &MoveData,
    target: Position,
) -> (Targets, Option<Position>) {
    let mut targets = Targets::new();
    let foe = user.side.foe();
    match data.target {
        MoveTarget::All | MoveTarget::FoeSide | MoveTarget::AllySide | MoveTarget::AllyTeam => {
            // Field and side moves: `alliesAndSelf()` and/or `foes(true)`.
            if !matches!(data.target, MoveTarget::FoeSide) {
                live_on(battle, user.side).iter().for_each(|&p| targets.push(p));
            }
            if !matches!(data.target, MoveTarget::AllySide | MoveTarget::AllyTeam) {
                (0..battle.format.active_per_side() as u8)
                    .map(|s| Position::new(foe, s))
                    .filter(|&p| held(battle, p))
                    .for_each(|p| targets.push(p));
            }
            let retarget = targets.last().copied().filter(|_| !targets.contains(&target));
            (targets, retarget)
        }
        MoveTarget::AllAdjacent | MoveTarget::AllAdjacentFoes => {
            if data.target == MoveTarget::AllAdjacent {
                adjacent_allies(battle, user).iter().for_each(|&p| targets.push(p));
            }
            live_on(battle, foe).iter().for_each(|&p| targets.push(p));
            let retarget = targets.last().copied().filter(|_| !targets.contains(&target));
            (targets, retarget)
        }
        MoveTarget::Allies => (live_on(battle, user.side), None),
        _ => {
            let selected = target;
            let mut target = target;
            if is_fainted(battle, target) && target.side != user.side {
                match get_random_target(battle, user, data.target) {
                    Some(t) => target = t,
                    None => return (targets, None),
                }
            }
            // `RedirectTarget` (Follow Me and Rage Powder;
            // Lightning Rod and Storm Drain; Counter and Mirror Coat) in
            // doubles, then `getSmartTargets` (Dragon Darts).
            let (target, second) = super::order::redirect_and_smart_targets(battle, user, data, target);
            // A future attack (Future Sight) may aim at a fainted ally's slot.
            if is_fainted(battle, target) && !data.flags.contains(MoveFlags::FUTURE_MOVE) {
                return (targets, None);
            }
            targets.push(target);
            if let Some(second) = second {
                targets.push(second);
            }
            (targets, (selected != target).then_some(target))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SideId;
    use crate::testing::sample_team;
    use crate::{BattleOptions, Format, Seed};

    fn battle(format: Format) -> Battle {
        let teams = [sample_team(&format), sample_team(&format)];
        Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap()
    }

    #[test]
    fn locations() {
        let user = Position::new(SideId::P1, 1);
        assert_eq!(loc_of(user, Position::new(SideId::P2, 0)), 1);
        assert_eq!(loc_of(user, Position::new(SideId::P1, 0)), -1);
        assert_eq!(loc_of_choice(Some(Target::Foe(1))), 2);
        assert_eq!(loc_of_choice(Some(Target::Ally(0))), -1);

        let b = battle(Format::gen9_doubles());
        let valid = |loc, t| valid_target_loc(&b, loc, user, t);
        assert!(valid(1, MoveTarget::Normal) && valid(2, MoveTarget::Normal) && valid(-1, MoveTarget::Normal));
        assert!(!valid(-2, MoveTarget::Normal), "self is not an adjacent target");
        assert!(valid(-2, MoveTarget::AdjacentAllyOrSelf));
        assert!(!valid(1, MoveTarget::AdjacentAlly));
        assert!(!valid(3, MoveTarget::Any));
        assert!(!valid(1, MoveTarget::AllAdjacentFoes), "spread moves never have a valid location");
    }

    #[test]
    fn random_targets_draw_only_in_doubles() {
        let mut singles = battle(Format::gen9_singles());
        let before = singles.rng.clone();
        let t = get_random_target(&mut singles, Position::new(SideId::P1, 0), MoveTarget::AllAdjacentFoes);
        assert_eq!(t, Some(Position::new(SideId::P2, 0)));
        assert_eq!(singles.rng, before);

        let mut doubles = battle(Format::gen9_doubles());
        let before = doubles.rng.clone();
        let t = get_random_target(&mut doubles, Position::new(SideId::P1, 0), MoveTarget::AllAdjacentFoes);
        assert_eq!(t.map(|p| p.side), Some(SideId::P2));
        assert_ne!(doubles.rng, before);
        // Self-targeting never draws.
        let before = doubles.rng.clone();
        let user = Position::new(SideId::P1, 0);
        assert_eq!(get_random_target(&mut doubles, user, MoveTarget::User), Some(user));
        assert_eq!(doubles.rng, before);
    }
}
