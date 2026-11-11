//! Moves that take, swap, use or change items and abilities.
//!
//! Showdown references (`data/moves.ts`): Knock Off, Thief, Covet, Trick,
//! Switcheroo, Recycle, Bug Bite, Pluck, Incinerate, Fling, Belch, Stuff
//! Cheeks, Poltergeist, Corrosive Gas, Skill Swap, Role Play, Entrainment,
//! Simple Beam, Worry Seed, Gastro Acid, Doodle, Teatime; `battle.skillSwap`
//! in `sim/battle.ts`.
//!
//! The item and ability lifecycle they use: `items::take_item`,
//! `items::set_item`, `items::eat_item`, `abilities::set_ability`, and the
//! `BattleEvent::ItemEnd` / `ItemShow` / `AbilityShow` messages.
//!
//! Contract (hooks in the shared pipeline, each at Showdown's call site):
//! - [`handles`]: status moves implemented here.
//! - [`on_try`], [`prepare_hit`], [`try_immunity`], [`try_hit`], [`on_hit`],
//!   [`after_hit`], [`hit_field`]: the moves' `onTry`, `onPrepareHit`,
//!   `onTryImmunity`, `onTryHit`, `onHit`, `onAfterHit` and `onHitField`.
//! - [`on_base_power`]: Knock Off's `onBasePower`.
//! - [`own_disable_move`]: the moves' `onDisableMove` (Belch, Stuff Cheeks;
//!   Champions: Fake Out, First Impression).
//! - [`add_volatile`]: Gastro Acid's volatile.

use pikapro_data::gen9::{abilities as a, items as i, moves as ids};
use pikapro_data::{AbilityFlags, AbilityId, Event, ItemId, MoveData, MoveId, Secondary, StatBoosts, StatusId};

use super::abilities::{self, SetAbilityKind};
use super::events::{self, Effect, EventCtx, Relay, Target};
use super::items::{self, consumed};
use super::moves::Outcome;
use super::{actor, boosts, status};
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, EffectName, FailNote, ItemEndNote, LineArg};
use crate::format::{Format, Mechanics};
use crate::state::{PokemonState, Position, Status, Volatiles};

/// Status moves whose effects this module implements beyond their data.
pub(crate) fn handles(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::TRICK
            | ids::SWITCHEROO
            | ids::RECYCLE
            | ids::STUFF_CHEEKS
            | ids::CORROSIVE_GAS
            | ids::SKILL_SWAP
            | ids::ROLE_PLAY
            | ids::ENTRAINMENT
            | ids::SIMPLE_BEAM
            | ids::WORRY_SEED
            | ids::GASTRO_ACID
            | ids::DOODLE
            | ids::TEATIME
    )
}

fn ability_of(battle: &Battle, pos: Position) -> AbilityId {
    battle.state.side(pos.side).team[pos.slot as usize].ability
}

fn flags(battle: &Battle, ability: AbilityId) -> AbilityFlags {
    battle.format.dex.ability(ability).flags
}

fn item_of(battle: &Battle, pos: Position) -> Option<ItemId> {
    battle.state.side(pos.side).team.get(pos.slot as usize).and_then(|m| m.item)
}

/// Whether `move_id`'s own `onDisableMove` disables it for `mon`: Belch
/// until a berry was eaten, Stuff Cheeks without a berry. The Champions
/// mod removes both (the moves just fail) and gives Fake Out and First
/// Impression one instead (`data/mods/champions/moves.ts`: disabled once
/// the Pokémon has acted since it came in, `pokemon.activeMoveActions`).
/// The move request reads it (`volatiles::request_shows_usable`), for a
/// fainted Pokémon too.
pub(crate) fn own_disable_move(format: &Format, mon: &PokemonState, move_id: MoveId) -> bool {
    if format.mechanics == Mechanics::Champions {
        return matches!(move_id, ids::FAKE_OUT | ids::FIRST_IMPRESSION) && mon.active_move_actions > 0;
    }
    match move_id {
        ids::BELCH => !mon.ate_berry,
        ids::STUFF_CHEEKS => !mon.item.is_some_and(|it| format.dex.item(it).is_berry),
        _ => false,
    }
}

/// The move's `onTry` (`singleEvent('Try')`): false fails it (`-fail`).
pub(crate) fn on_try(battle: &mut Battle, user: Position, target: Position, move_id: MoveId) -> bool {
    match move_id {
        ids::BELCH => battle.state.at(user).is_some_and(|m| m.ate_berry),
        ids::STUFF_CHEEKS => item_of(battle, user).is_some_and(|it| battle.format.dex.item(it).is_berry),
        ids::POLTERGEIST => item_of(battle, target).is_some(),
        _ => true,
    }
}

/// What Fling throws, as a secondary effect.
static FLING_BRN: [Secondary; 1] = [fling_secondary(Some(StatusId::Burn), None)];
static FLING_PAR: [Secondary; 1] = [fling_secondary(Some(StatusId::Paralysis), None)];
static FLING_PSN: [Secondary; 1] = [fling_secondary(Some(StatusId::Poison), None)];
static FLING_TOX: [Secondary; 1] = [fling_secondary(Some(StatusId::Toxic), None)];
static FLING_FLINCH: [Secondary; 1] = [fling_secondary(None, Some("flinch"))];

/// `move.secondaries.push({status})` / `({volatileStatus})`: no chance
/// (always), but still a `random(100)` roll.
const fn fling_secondary(status: Option<StatusId>, volatile_status: Option<&'static str>) -> Secondary {
    Secondary { chance: 100, status, volatile_status, boosts: None, self_boosts: None, unmodelled: &[] }
}

/// The move's own `onPrepareHit` (Fling), after its `onTry`: `Err(())`
/// fails the move (`-fail`); `Ok(Some(data))` is the active move changed by
/// it (Fling's power and secondaries).
#[inline(always)]
pub(crate) fn prepare_hit(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
) -> Result<Option<MoveData>, ()> {
    if move_id != ids::FLING {
        return Ok(None);
    }
    fling_prepare_hit(battle, user, data)
}

/// [`prepare_hit`] for Fling.
fn fling_prepare_hit(battle: &mut Battle, user: Position, data: &MoveData) -> Result<Option<MoveData>, ()> {
    let move_id = ids::FLING;
    // `onPrepareHit`: the item must be usable (Klutz, Magic Room), not
    // stuck to its holder (`TakeItem`), and flingable.
    if items::ignoring_item_for_fling(battle, user) {
        return Err(());
    }
    let item = item_of(battle, user).ok_or(())?;
    let taken = events::single_event_item(
        battle,
        Event::TakeItem,
        user,
        item,
        Some(user),
        Effect::Move(move_id),
        Relay::Item(item),
    );
    if !taken.is_truthy() {
        return Err(());
    }
    let item_data = battle.format.dex.item(item);
    let power = item_data.fling_power.ok_or(())?;
    let mut active = *data;
    active.base_power = power;
    if item_data.is_berry {
        if abilities::has_ability(battle, user, a::CUD_CHEW) {
            let effect = Effect::Move(move_id);
            events::single_event_ability(
                battle,
                Event::EatItem,
                user,
                a::CUD_CHEW,
                Some(user),
                effect,
                Relay::Item(item),
            );
        }
    } else {
        let secondaries: &'static [Secondary] = match item {
            i::FLAME_ORB => &FLING_BRN,
            i::LIGHT_BALL => &FLING_PAR,
            i::POISON_BARB => &FLING_PSN,
            i::TOXIC_ORB => &FLING_TOX,
            i::KINGS_ROCK | i::RAZOR_FANG => &FLING_FLINCH,
            _ => &[],
        };
        if !secondaries.is_empty() {
            active.secondaries = secondaries;
        }
    }
    battle.move_state.fling = Some(item);
    events::add_volatile_flag(battle, user, Volatiles::FLING, "fling", None, Effect::Move(move_id));
    Ok(Some(active))
}

/// The move's `onTryImmunity` (`hitStepTryImmunity`): false makes the
/// target immune (`-immune`).
pub(crate) fn try_immunity(battle: &mut Battle, target: Position, _user: Position, move_id: MoveId) -> bool {
    let immune = match move_id {
        ids::TRICK | ids::SWITCHEROO => abilities::has_ability(battle, target, a::STICKY_HOLD),
        ids::WORRY_SEED => matches!(ability_of(battle, target), a::TRUANT | a::INSOMNIA),
        _ => false,
    };
    if immune {
        let t = actor(battle, target);
        battle.emit(BattleEvent::Immune { target: t });
    }
    !immune
}

/// The move's own `onTryHit` (on the first target in `spreadMoveHit`).
/// Returns false if the hit fails (`-fail` when the callback returned
/// `false`).
pub(crate) fn try_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    let (target_ability, user_ability) = (ability_of(battle, target), ability_of(battle, user));
    let outcome = match move_id {
        ids::ROLE_PLAY => {
            let fails = target_ability == user_ability
                || flags(battle, target_ability).contains(AbilityFlags::FAIL_ROLE_PLAY)
                || flags(battle, user_ability).contains(AbilityFlags::CANT_SUPPRESS);
            if fails { Outcome::Failed } else { Outcome::Undefined }
        }
        ids::ENTRAINMENT => {
            let fails = target == user
                || target_ability == user_ability
                || flags(battle, target_ability).contains(AbilityFlags::CANT_SUPPRESS)
                || target_ability == a::TRUANT
                || flags(battle, user_ability).contains(AbilityFlags::NO_ENTRAIN);
            if fails { Outcome::Failed } else { Outcome::Undefined }
        }
        ids::SIMPLE_BEAM => {
            let fails = flags(battle, target_ability).contains(AbilityFlags::CANT_SUPPRESS)
                || matches!(target_ability, a::SIMPLE | a::TRUANT);
            if fails { Outcome::Failed } else { Outcome::Undefined }
        }
        ids::WORRY_SEED if flags(battle, target_ability).contains(AbilityFlags::CANT_SUPPRESS) => Outcome::Failed,
        ids::GASTRO_ACID => {
            if flags(battle, target_ability).contains(AbilityFlags::CANT_SUPPRESS) {
                Outcome::Failed
            } else if items::has_item(battle, target, i::ABILITY_SHIELD) {
                let t = actor(battle, target);
                let effect = EffectName::Item(i::ABILITY_SHIELD);
                battle.emit(BattleEvent::Block { actor: t, effect, arg: None, of: None });
                Outcome::Null
            } else {
                Outcome::Undefined
            }
        }
        ids::POLTERGEIST => {
            if let Some(item) = item_of(battle, target) {
                let t = actor(battle, target);
                battle.emit(BattleEvent::ActivateEffect {
                    actor: t,
                    effect: EffectName::Move(move_id),
                    args: [Some(LineArg::Item(item)), None],
                    of: None,
                });
            }
            Outcome::Undefined
        }
        _ => Outcome::Undefined,
    };
    match outcome {
        Outcome::Failed => {
            super::moves::fail_still(battle, user);
            false
        }
        Outcome::Null | Outcome::NotFail => false,
        _ => true,
    }
}

/// Knock Off's `onBasePower` as a 4096-based modifier: 1.5x against a
/// target holding an item that can be removed (the item's own
/// `TakeItem`).
pub(crate) fn on_base_power(battle: &mut Battle, _user: Position, target: Position, move_id: MoveId) -> u32 {
    if move_id != ids::KNOCK_OFF {
        return 4096;
    }
    let Some(item) = item_of(battle, target) else { return 4096 };
    let effect = Effect::Move(move_id);
    let relay =
        events::single_event_item(battle, Event::TakeItem, target, item, Some(target), effect, Relay::Item(item));
    if relay.is_truthy() { 6144 } else { 4096 }
}

/// The move's `onAfterHit` on a damaged target (the user still has HP):
/// Knock Off removes the target's item, Thief and Covet steal it.
pub(crate) fn after_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) {
    match move_id {
        ids::KNOCK_OFF => {
            if let Some(item) = items::take_item(battle, target, None) {
                let (t, u) = (actor(battle, target), actor(battle, user));
                battle.emit(BattleEvent::ItemEnd {
                    actor: t,
                    item,
                    note: ItemEndNote::None,
                    from: Some(EffectName::Move(move_id)),
                    of: Some(u),
                });
            }
        }
        ids::THIEF | ids::COVET => {
            let user_mon = battle.state.side(user.side).team[user.slot as usize];
            if user_mon.item.is_some() || user_mon.volatiles.has(Volatiles::GEM) {
                return;
            }
            let Some(item) = items::take_item(battle, target, Some(user)) else { return };
            let effect = Effect::Move(move_id);
            let kept =
                events::single_event_item(battle, Event::TakeItem, user, item, Some(target), effect, Relay::Item(item));
            if !kept.is_truthy() || !items::set_item(battle, user, Some(item), None, Effect::None) {
                consumed::restore_item(battle, target, item);
                return;
            }
            let (t, u) = (actor(battle, target), actor(battle, user));
            if move_id == ids::THIEF {
                battle.emit(BattleEvent::ItemEnd {
                    actor: t,
                    item,
                    note: ItemEndNote::Silent,
                    from: Some(EffectName::Move(move_id)),
                    of: Some(u),
                });
            }
            battle.emit(BattleEvent::ItemShow { actor: u, item, cause: Some(Cause::move_(move_id).of(t)) });
        }
        _ => {}
    }
}

/// The move's `onHit` (`singleEvent('Hit')` in `runMoveEffects`), or
/// `None` if it has none here. `target` is the Pokémon hit, `user` the
/// move's user.
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Option<Outcome> {
    Some(match move_id {
        ids::TRICK | ids::SWITCHEROO => trick(battle, target, user, move_id),
        ids::RECYCLE => {
            let mon = battle.state.at(target)?;
            let Some(item) = mon.last_item.filter(|_| mon.item.is_none()) else { return Some(Outcome::Failed) };
            battle.state.side_mut(target.side).team[target.slot as usize].last_item = None;
            let t = actor(battle, target);
            battle.emit(BattleEvent::ItemShow { actor: t, item, cause: Some(Cause::move_(move_id)) });
            items::set_item(battle, target, Some(item), Some(user), Effect::Move(move_id));
            Outcome::Undefined
        }
        ids::BUG_BITE | ids::PLUCK => {
            let item = item_of(battle, target).filter(|&it| battle.format.dex.item(it).is_berry);
            let user_up = battle.state.at(user).is_some_and(|m| m.hp > 0);
            if let (true, Some(item)) = (user_up, item)
                && items::take_item(battle, target, Some(user)).is_some()
            {
                let (t, u) = (actor(battle, target), actor(battle, user));
                battle.emit(BattleEvent::ItemEnd {
                    actor: t,
                    item,
                    note: ItemEndNote::StealEat(move_id),
                    from: None,
                    of: Some(u),
                });
                // `if (item.id === 'leppaberry') target.staleness =
                // 'external'`: the Pokémon that lost the berry.
                eat_for(battle, user, user, item, move_id, target);
            }
            Outcome::Undefined
        }
        ids::INCINERATE => {
            let item = item_of(battle, target).filter(|&it| {
                let d = battle.format.dex.item(it);
                d.is_berry || d.is_gem
            });
            if let Some(item) = item
                && items::take_item(battle, target, Some(user)).is_some()
            {
                let t = actor(battle, target);
                battle.emit(BattleEvent::ItemEnd {
                    actor: t,
                    item,
                    note: ItemEndNote::None,
                    from: Some(EffectName::Move(move_id)),
                    of: None,
                });
            }
            Outcome::Undefined
        }
        ids::FLING => {
            let item = battle.move_state.fling?;
            fling_hit(battle, target, user, item);
            Outcome::Undefined
        }
        ids::STUFF_CHEEKS => {
            if !boosts::boost(battle, target, StatBoosts::new([0, 2, 0, 0, 0, 0, 0]), Some(target), false, false) {
                return Some(Outcome::Null);
            }
            items::eat_item(battle, target, true, Some(target), Effect::Move(move_id));
            Outcome::Undefined
        }
        ids::CORROSIVE_GAS => {
            match items::take_item(battle, target, Some(user)) {
                Some(item) => {
                    let (t, u) = (actor(battle, target), actor(battle, user));
                    battle.emit(BattleEvent::ItemEnd {
                        actor: t,
                        item,
                        note: ItemEndNote::None,
                        from: Some(EffectName::Move(move_id)),
                        of: Some(u),
                    });
                }
                None => {
                    let t = actor(battle, target);
                    battle.emit(BattleEvent::MoveFail { user: t, move_id, note: FailNote::None });
                }
            }
            Outcome::Undefined
        }
        ids::SKILL_SWAP => skill_swap(battle, user, target),
        ids::ROLE_PLAY => {
            let ability = ability_of(battle, target);
            set(battle, user, ability, Some(target), move_id)
        }
        ids::ENTRAINMENT => {
            let ability = ability_of(battle, user);
            let result = set(battle, target, ability, Some(user), move_id);
            // `if (!oldAbility) return oldAbility; if
            // (!target.isAlly(source)) target.volatileStaleness =
            // 'external';`
            if result == Outcome::Undefined {
                super::endless::ability_given(battle, target, user);
            }
            result
        }
        ids::SIMPLE_BEAM => set(battle, target, a::SIMPLE, None, move_id),
        ids::WORRY_SEED => {
            // `if (!oldAbility) return oldAbility;`: Ability Shield's `null`
            // leaves it asleep.
            let result = set(battle, target, a::INSOMNIA, None, move_id);
            if result == Outcome::Undefined
                && battle.state.at(target).is_some_and(|m| matches!(m.status, Status::Sleep { .. }))
            {
                status::cure_status(battle, target);
            }
            result
        }
        ids::DOODLE => doodle(battle, target, user, move_id),
        _ => return None,
    })
}

/// `setAbility(ability, source, move)`: `false` (or the `SetAbility`
/// event's failure) fails the move.
fn set(battle: &mut Battle, pos: Position, ability: AbilityId, source: Option<Position>, move_id: MoveId) -> Outcome {
    let effect = Some(EffectName::Move(move_id));
    // `setAbility` is `false` for a `cantsuppress` ability or a fainted
    // target, and the `SetAbility` event's `null` (Ability Shield's
    // `-block`) otherwise: `null` doesn't fail the move.
    let old = ability_of(battle, pos);
    let can = !flags(battle, ability).contains(AbilityFlags::CANT_SUPPRESS)
        && !flags(battle, old).contains(AbilityFlags::CANT_SUPPRESS)
        && battle.state.side(pos.side).team[pos.slot as usize].hp > 0;
    match abilities::set_ability(battle, pos, ability, source, effect, SetAbilityKind::Normal) {
        Some(_) => Outcome::Undefined,
        None if can => Outcome::Null,
        None => Outcome::Failed,
    }
}

/// The berry `item` eaten by `eater` for `source`'s move (Bug Bite, Pluck,
/// Fling): its `Eat` callback, `EatItem`, and `ateBerry` if it has an
/// `onEat`.
///
/// A Leppa Berry eaten this way leaves `stale` with `staleness =
/// 'external'` (the Endless Battle Clause): Bug Bite's and Pluck's target,
/// which lost the berry, and the foe Fling hit, which ate it.
fn eat_for(battle: &mut Battle, eater: Position, source: Position, item: ItemId, move_id: MoveId, stale: Position) {
    let effect = Effect::Move(move_id);
    if events::single_event_item(battle, Event::Eat, eater, item, Some(source), effect, Relay::True).is_truthy() {
        let mut ev = EventCtx::new(Event::EatItem, Target::Pokemon(eater), Some(source), effect, Relay::Item(item));
        events::run_event(battle, &mut ev, None);
        if item == i::LEPPA_BERRY {
            super::endless::external(battle, stale);
        }
    }
    if consumed::has_on_eat(battle, item)
        && let Some(m) = battle.state.side_mut(eater.side).team.get_mut(eater.slot as usize)
    {
        m.ate_berry = true;
    }
}

/// Fling's `move.onHit`: a flung berry is eaten by the target; Mental Herb
/// and White Herb act on it.
fn fling_hit(battle: &mut Battle, target: Position, user: Position, item: ItemId) {
    if battle.format.dex.item(item).is_berry {
        eat_for(battle, target, user, item, ids::FLING, target);
        return;
    }
    match item {
        i::MENTAL_HERB => {
            if let Some(first) = consumed::mental_herb_first(battle, target) {
                consumed::mental_herb_cure(battle, target, first);
            }
        }
        i::WHITE_HERB => {
            let Some(mon) = battle.state.at_mut(target) else { return };
            let b = &mut mon.boosts;
            let mut activate = false;
            for stage in [&mut b.atk, &mut b.def, &mut b.spa, &mut b.spd, &mut b.spe, &mut b.accuracy, &mut b.evasion] {
                if *stage < 0 {
                    *stage = 0;
                    activate = true;
                }
            }
            if activate {
                let t = actor(battle, target);
                battle.emit(BattleEvent::ClearNegativeBoost { target: t, silent: true });
            }
        }
        _ => {}
    }
}

/// Trick's and Switcheroo's `onHit`: the user and the target swap items.
fn trick(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Outcome {
    let (had_theirs, had_mine) = (item_of(battle, target).is_some(), item_of(battle, user).is_some());
    let yours = items::take_item(battle, target, Some(user));
    let mine = items::take_item(battle, user, None);
    let restore = |battle: &mut Battle| {
        if let Some(it) = yours {
            consumed::restore_item(battle, target, it);
        }
        if let Some(it) = mine {
            consumed::restore_item(battle, user, it);
        }
    };
    // `false` (a `TakeItem` handler kept it) or neither holds anything.
    if (had_theirs && yours.is_none()) || (had_mine && mine.is_none()) || (yours.is_none() && mine.is_none()) {
        restore(battle);
        return Outcome::Failed;
    }
    let effect = Effect::Move(move_id);
    let mine_kept = mine.is_none_or(|it| {
        events::single_event_item(battle, Event::TakeItem, target, it, Some(user), effect, Relay::Item(it)).is_truthy()
    });
    let yours_kept = mine_kept
        && yours.is_none_or(|it| {
            events::single_event_item(battle, Event::TakeItem, user, it, Some(target), effect, Relay::Item(it))
                .is_truthy()
        });
    if !mine_kept || !yours_kept {
        restore(battle);
        return Outcome::Failed;
    }
    let (u, t) = (actor(battle, user), actor(battle, target));
    battle.emit(BattleEvent::ActivateEffect {
        actor: u,
        effect: EffectName::Move(ids::TRICK),
        args: [None, None],
        of: Some(t),
    });
    let from = Some(EffectName::Move(move_id));
    match mine {
        Some(it) => {
            items::set_item(battle, target, Some(it), None, Effect::None);
            battle.emit(BattleEvent::ItemShow { actor: t, item: it, cause: Some(Cause::move_(move_id)) });
        }
        None => {
            let it = yours.expect("one of them held an item");
            battle.emit(BattleEvent::ItemEnd { actor: t, item: it, note: ItemEndNote::Silent, from, of: None });
        }
    }
    match yours {
        Some(it) => {
            items::set_item(battle, user, Some(it), None, Effect::None);
            battle.emit(BattleEvent::ItemShow { actor: u, item: it, cause: Some(Cause::move_(move_id)) });
        }
        None => {
            let it = mine.expect("one of them held an item");
            battle.emit(BattleEvent::ItemEnd { actor: u, item: it, note: ItemEndNote::Silent, from, of: None });
        }
    }
    Outcome::Undefined
}

/// Showdown's `battle.skillSwap(source, target)`.
fn skill_swap(battle: &mut Battle, source: Position, target: Position) -> Outcome {
    if !events::is_active(battle, source) || !events::is_active(battle, target) {
        return Outcome::Failed;
    }
    let (source_ability, target_ability) = (ability_of(battle, source), ability_of(battle, target));
    if flags(battle, source_ability).contains(AbilityFlags::FAIL_SKILL_SWAP)
        || flags(battle, target_ability).contains(AbilityFlags::FAIL_SKILL_SWAP)
    {
        return Outcome::Failed;
    }
    // `runEvent('SetAbility', ...)` with the `skillswap` condition.
    let effect = Effect::Condition(events::Condition::Other);
    for (pos, ability) in [(target, source_ability), (source, target_ability)] {
        let mut ev =
            EventCtx::new(Event::SetAbility, Target::Pokemon(pos), Some(source), effect, Relay::Ability(ability));
        match events::run_event(battle, &mut ev, None) {
            r if r.is_truthy() => {}
            Relay::Null => return Outcome::Null,
            _ => return Outcome::Failed,
        }
    }
    let (s, t) = (actor(battle, source), actor(battle, target));
    let shown = (source.side != target.side).then_some((target_ability, source_ability));
    battle.emit(BattleEvent::SkillSwap { source: s, target: t, abilities: shown });
    if shown.is_none() {
        battle.hint(crate::event::Hint::SkillSwapAllies);
    }
    events::single_event_ability(battle, Event::End, source, source_ability, None, Effect::None, Relay::True);
    events::single_event_ability(battle, Event::End, target, target_ability, None, Effect::None, Relay::True);
    battle.state.side_mut(source.side).team[source.slot as usize].ability = target_ability;
    battle.state.side_mut(target.side).team[target.slot as usize].ability = source_ability;
    abilities::new_ability_state(battle, source);
    abilities::new_ability_state(battle, target);
    super::endless::ability_swapped(battle, source, target);
    events::single_event_ability(battle, Event::Start, target, source_ability, None, Effect::None, Relay::True);
    events::single_event_ability(battle, Event::Start, source, target_ability, None, Effect::None, Relay::True);
    Outcome::Undefined
}

/// Doodle's `onHit`: the user and its ally copy the target's ability.
fn doodle(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Outcome {
    // `false`, then `null` if a change was stopped silently.
    let mut success: Option<bool> = Some(false);
    let ability = ability_of(battle, target);
    if !flags(battle, ability).contains(AbilityFlags::FAIL_ROLE_PLAY) {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(user.side, slot as u8);
            if !events::is_active(battle, pos) || battle.state.at(pos).is_none_or(|m| m.hp == 0) {
                continue;
            }
            let own = ability_of(battle, pos);
            if own == ability || flags(battle, own).contains(AbilityFlags::CANT_SUPPRESS) {
                continue;
            }
            let effect = Some(EffectName::Move(move_id));
            // `setAbility` is `false` for a `cantsuppress` ability and the
            // `SetAbility` event's `null` (Ability Shield's `-block`)
            // otherwise, which keeps the move from failing loudly.
            let can = !flags(battle, ability).contains(AbilityFlags::CANT_SUPPRESS);
            match abilities::set_ability(battle, pos, ability, None, effect, SetAbilityKind::Normal) {
                Some(_) => success = Some(true),
                None if can && success != Some(true) => success = None,
                None => {}
            }
        }
    }
    if success == Some(true) {
        return Outcome::Undefined;
    }
    battle.flush_rng_trace();
    if success == Some(false) {
        let u = actor(battle, user);
        battle.emit(BattleEvent::Fail { user: u });
    }
    battle.attr_last_move(crate::event::MoveAttr::Still);
    Outcome::NotFail
}

/// Teatime's `onHitField`: every active Pokémon holding a berry eats it.
/// Returns whether the move did something (it never fails loudly).
pub(crate) fn hit_field(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    if move_id != ids::TEATIME {
        return true;
    }
    // The active move, with its type after `ModifyType` (Galvanize makes
    // it Electric: Volt Absorb's `TryHit`).
    let mut data = *battle.format.dex.move_(move_id);
    data.ty = battle.move_state.ty.unwrap_or(data.ty);
    let mut eaters = crate::util::StackVec::<Position, 4>::new();
    for pos in abilities::all_active(battle).collect::<crate::util::StackVec<Position, 4>>().iter().copied() {
        // `runEvent('Invulnerability')` (a semi-invulnerable Pokémon is
        // missed), then `runEvent('TryHit')`.
        if !super::multiturn::field_invulnerability(battle, pos, user, move_id) {
            continue;
        }
        // One `TryHit` event, in the move pipeline's order: the guards and
        // Psychic Terrain (priority 4: a Prankster Teatime is blocked on a
        // grounded foe), Protect (3), then the abilities and items.
        let hit = super::field::try_hit(battle, pos, user, move_id, &data)
            && super::protect::try_hit(battle, pos, user, &data)
            && events::move_event(battle, Event::TryHit, pos, Some(user), move_id, &data, Relay::True).is_truthy();
        if hit && item_of(battle, pos).is_some_and(|it| battle.format.dex.item(it).is_berry) {
            eaters.push(pos);
        }
    }
    battle.emit(BattleEvent::MoveFieldActivate { move_id });
    if eaters.is_empty() {
        let u = actor(battle, user);
        battle.emit(BattleEvent::MoveFail { user: u, move_id, note: FailNote::None });
        battle.flush_rng_trace();
        battle.attr_last_move(crate::event::MoveAttr::Still);
        return true;
    }
    for &pos in eaters.iter() {
        items::eat_item(battle, pos, true, Some(target), Effect::Move(move_id));
    }
    true
}

/// `addVolatile` for this module's volatiles (Gastro Acid), or `None` for
/// another ID. Its `onStart` fails under Ability Shield; otherwise the
/// ability ends (`-endability`, its `End`).
pub(crate) fn add_volatile(
    battle: &mut Battle,
    target: Position,
    id: &str,
    source: Option<Position>,
) -> Option<Option<bool>> {
    if id != "gastroacid" {
        return None;
    }
    let effect = Effect::Move(ids::GASTRO_ACID);
    if !events::add_volatile_flag(battle, target, Volatiles::GASTRO_ACID, "gastroacid", source, effect) {
        return Some(Some(false));
    }
    if items::has_item(battle, target, i::ABILITY_SHIELD) {
        events::remove_volatile_flag(battle, target, Volatiles::GASTRO_ACID);
        return Some(Some(false));
    }
    let t = actor(battle, target);
    battle.emit(BattleEvent::AbilityEnd { actor: t });
    let ability = ability_of(battle, target);
    events::single_event_ability(battle, Event::End, target, ability, Some(target), Effect::None, Relay::True);
    Some(Some(true))
}
