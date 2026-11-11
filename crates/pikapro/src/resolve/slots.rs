//! Slot conditions: Wish, Healing Wish, Lunar Dance and Revival Blessing.
//!
//! Showdown references: `side.addSlotCondition` / `removeSlotCondition`
//! (`sim/side.ts`); the `wish`, `healingwish`, `lunardance` and
//! `revivalblessing` moves and conditions (`data/moves.ts`); Revival
//! Blessing's switch request and `revivalblessing` action (`chooseSwitch`
//! in `sim/side.ts`, `runAction` and its switch-request step in
//! `sim/battle.ts`).
//!
//! Slot conditions belong to an active position, not to the Pokémon in it
//! (`SideConditions::slots`). Their handlers are collected with the
//! Pokémon's in that slot ([`Holder::Slot`]) and run even while it is
//! fainted.

use pikapro_data::MoveId;
use pikapro_data::gen9::moves as ids;

use super::handlers::{Callback, Handler, Handlers, Holder, sub_order};
use super::moves::fail_still;
use super::queue::{self, Action, ActionKind, MonRef};
use super::{actor, switching};
use crate::battle::Battle;
use crate::event::{Actor, BattleEvent};
use crate::state::{Position, SlotCondition, Status, Wish};

/// Status moves implemented here.
pub(crate) fn handles(move_id: MoveId) -> bool {
    matches!(move_id, ids::WISH | ids::HEALING_WISH | ids::LUNAR_DANCE | ids::REVIVAL_BLESSING)
}

fn slot_condition_of(id: &str) -> Option<SlotCondition> {
    [
        ("wish", SlotCondition::Wish),
        ("healingwish", SlotCondition::HealingWish),
        ("lunardance", SlotCondition::LunarDance),
        ("revivalblessing", SlotCondition::RevivalBlessing),
    ]
    .into_iter()
    .find(|(name, _)| id.eq_ignore_ascii_case(name))
    .map(|(_, c)| c)
}

/// Showdown's `getOverflowedTurnCount()`: `this.trunc(this.turn - 1, 8)`.
/// Custom games bind `Math.trunc`, which ignores the bit count, so there
/// the count never wraps at 256: a Wish made on turn 256 heals on turn 257
/// despite its hint, and one made on turn 512 gives no hint
/// (`Format::cartridge_overflow`).
pub(crate) fn overflowed_turn(battle: &Battle) -> u16 {
    let turn = battle.state.turn.saturating_sub(1);
    if battle.format.cartridge_overflow { turn % 256 } else { turn }
}

/// The move's `onTryHit`: Healing Wish and Lunar Dance fail with nothing to
/// switch in, Revival Blessing with nobody fainted. False stops the move
/// (after `-fail`).
pub(crate) fn try_hit(battle: &mut Battle, _target: Position, user: Position, move_id: MoveId) -> bool {
    let ok = match move_id {
        ids::HEALING_WISH | ids::LUNAR_DANCE => switching::can_switch(battle, user.side) > 0,
        ids::REVIVAL_BLESSING => battle.state.side(user.side).team.iter().any(|m| m.is_fainted()),
        _ => true,
    };
    if !ok {
        fail_still(battle, user);
    }
    ok
}

/// Showdown's `side.addSlotCondition`: fails if the slot already has it
/// (none of these has `onRestart`).
pub(crate) fn add_slot_condition(battle: &mut Battle, target: Position, id: &'static str, source: Position) -> bool {
    let Some(condition) = slot_condition_of(id) else { return false };
    let turn = overflowed_turn(battle);
    let Some(user) = battle.state.at(source).copied() else { return false };
    let slots = &mut battle.state.side_mut(target.side).conditions.slots[target.slot as usize];
    if slots.has(condition) {
        return false;
    }
    if condition == SlotCondition::Wish {
        // `onStart`: half the wisher's max HP, healed at the end of the
        // next turn. `heal` truncates it, and heals at least 1.
        slots.wish = Wish {
            starting_turn: turn,
            hp: super::hp_fraction(u32::from(user.max_hp()), 1, 2) as u16,
            wisher: user.species,
            wisher_id: user.id,
        };
    }
    slots.order.push(condition);
    battle.conditions_changed();
    // Wish's `onStart` hint.
    if condition == SlotCondition::Wish && turn == 255 {
        battle.hint(crate::event::Hint::WishTurnLimit(battle.state.turn));
    }
    true
}

fn remove(battle: &mut Battle, pos: Position, condition: SlotCondition) -> bool {
    let slots = &mut battle.state.side_mut(pos.side).conditions.slots[pos.slot as usize];
    let had = slots.has(condition);
    slots.order.retain(|&c| c != condition);
    slots.targeted &= !(1 << condition as u8);
    battle.conditions_changed();
    had
}

/// Whether the slot at `pos` waits for a Revival Blessing choice.
pub(crate) fn reviving(battle: &Battle, pos: Position) -> bool {
    battle.state.side(pos.side).conditions.slots[pos.slot as usize].has(SlotCondition::RevivalBlessing)
}

/// `selfdestruct: 'ifHit'` (Healing Wish, Lunar Dance): the user faints
/// once the move has hit (`runMoveEffects`).
pub(crate) fn self_destruct_if_hit(battle: &mut Battle, user: Position, move_id: MoveId) {
    if !matches!(move_id, ids::HEALING_WISH | ids::LUNAR_DANCE) {
        return;
    }
    super::faint(battle, user);
}

/// End-of-turn handlers of the slot conditions at `pos`: Wish (order 4).
pub(crate) fn residual_handlers(battle: &Battle, pos: Position, speed: i32, handlers: &mut Handlers) {
    let slots = &battle.state.side(pos.side).conditions.slots[pos.slot as usize];
    for &condition in slots.order.iter() {
        if condition == SlotCondition::Wish {
            handlers.push(Handler {
                effect: super::events::Effect::Slot(condition),
                order: Some(4),
                speed,
                sub_order: sub_order::SLOT_CONDITION,
                holder: Holder::Slot(pos),
                callback: Callback::Plain(wish_residual),
                ..Handler::default()
            });
        }
        // Revival Blessing's `duration: 1`: left unused (a commanded
        // Dondozo can't switch out), it ends at the end of
        // the turn.
        if condition == SlotCondition::RevivalBlessing {
            handlers.push(Handler {
                effect: super::events::Effect::Slot(condition),
                speed,
                sub_order: sub_order::SLOT_CONDITION,
                holder: Holder::Slot(pos),
                callback: Callback::Plain(revival_blessing_residual),
                ..Handler::default()
            });
        }
    }
}

/// Revival Blessing's duration runs out: it ends (no `faintMessages`).
fn revival_blessing_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Slot(pos) = handler.holder else { return true };
    remove(battle, pos, SlotCondition::RevivalBlessing);
    false
}

/// Wish's `onResidual`: from the turn after it was made, it ends, healing
/// the Pokémon in the slot (`onEnd`) unless it has fainted.
fn wish_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Slot(pos) = handler.holder else { return true };
    let slots = battle.state.side(pos.side).conditions.slots[pos.slot as usize];
    if !slots.has(SlotCondition::Wish) || overflowed_turn(battle) <= slots.wish.starting_turn {
        return true;
    }
    remove(battle, pos, SlotCondition::Wish);
    // `onEnd`: `this.heal(hp, target, target)` unless it has fainted.
    if battle.state.at(pos).is_none_or(|m| m.is_fainted()) {
        return true;
    }
    // `runEvent('TryHeal')` (Heal Block stops it), then nothing (and no
    // message) at full HP or off the field.
    let effect = super::events::Effect::Slot(SlotCondition::Wish);
    let Some(amount) = super::try_heal(battle, pos, slots.wish.hp as u32, Some(pos), effect) else { return true };
    if off_field(battle, pos) {
        return true;
    }
    let Some(mon) = battle.state.at_mut(pos).filter(|m| m.hp < m.max_hp()) else { return true };
    mon.hp = (mon.hp as u32 + amount).min(mon.max_hp() as u32) as u16;
    let (hp, max_hp, status) = (mon.hp, mon.max_hp(), mon.status);
    let target = actor(battle, pos);
    let (wisher, wisher_id) = (slots.wish.wisher, slots.wish.wisher_id);
    battle.emit(BattleEvent::WishHeal { target, hp, max_hp, status, wisher, wisher_id });
    true
}

/// `SwitchIn` handlers of the slot conditions at `pos` (Healing Wish,
/// Lunar Dance), in the order they were made.
pub(crate) fn switch_in_handlers(battle: &Battle, pos: Position, speed: i32, handlers: &mut Handlers) {
    let slots = &battle.state.side(pos.side).conditions.slots[pos.slot as usize];
    for (i, &condition) in slots.order.iter().enumerate() {
        if matches!(condition, SlotCondition::HealingWish | SlotCondition::LunarDance) {
            handlers.push(Handler {
                speed,
                sub_order: sub_order::SLOT_CONDITION,
                effect_order: i as u32,
                holder: Holder::Slot(pos),
                callback: Callback::Plain(healing_wish_switch_in),
                arg: condition as u8,
                ..Handler::default()
            });
        }
    }
}

/// Marks a [`swap_event`] handler (whose `effectOrder` is unused).
const SWAP: u32 = u32::MAX;

/// `runEvent('Swap', pokemon)` after `swapPosition` (Ally Switch): the
/// Healing Wish and Lunar Dance waiting in the Pokémon's new slot heal it.
/// A `Swap` handler has no `effectOrder`, so two of them tie.
pub(crate) fn swap_event(battle: &mut Battle, pos: Position) {
    let mut handlers = Handlers::new();
    switch_in_handlers(battle, pos, 0, &mut handlers);
    for h in handlers.iter_mut() {
        h.effect_order = SWAP;
    }
    queue::speed_sort(&mut handlers, &mut battle.rng, Handler::compare);
    for h in handlers.iter() {
        let condition = if h.arg == SlotCondition::LunarDance as u8 {
            SlotCondition::LunarDance
        } else {
            SlotCondition::HealingWish
        };
        // `runEvent` sets `effectState.target` to the holder.
        let slots = &mut battle.state.side_mut(pos.side).conditions.slots[pos.slot as usize];
        if slots.has(condition) {
            slots.targeted |= 1 << condition as u8;
        }
        healing_wish_switch_in(battle, h);
    }
}

/// Healing Wish's and Lunar Dance's `onSwap`: a Pokémon that needs it is
/// fully healed and cured (Lunar Dance also restores PP); otherwise the
/// wish waits for the next one.
fn healing_wish_switch_in(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Slot(pos) = handler.holder else { return true };
    let condition = if handler.arg == SlotCondition::LunarDance as u8 {
        SlotCondition::LunarDance
    } else {
        SlotCondition::HealingWish
    };
    if !battle.state.side(pos.side).conditions.slots[pos.slot as usize].has(condition) {
        return true;
    }
    // `fieldEvent('SwitchIn')` skips a state with a Pokémon `target`
    // (after sorting, so it still counts for ties).
    if handler.effect_order != SWAP
        && battle.state.side(pos.side).conditions.slots[pos.slot as usize].targeted & (1 << condition as u8) != 0
    {
        return true;
    }
    let lunar = condition == SlotCondition::LunarDance;
    let Some(mon) = battle.state.at_mut(pos).filter(|m| !m.is_fainted()) else { return true };
    let pp_missing = mon.moves.iter().any(|m| m.pp < m.max_pp);
    if mon.hp >= mon.max_hp() && mon.status == Status::None && !(lunar && pp_missing) {
        return true;
    }
    mon.hp = mon.max_hp();
    // `clearStatus`: no message.
    mon.status = Status::None;
    if lunar {
        for m in mon.moves.iter_mut() {
            m.pp = m.max_pp;
        }
    }
    let (hp, max_hp) = (mon.hp, mon.max_hp());
    let target = actor(battle, pos);
    let move_id = if lunar { ids::LUNAR_DANCE } else { ids::HEALING_WISH };
    battle.emit(BattleEvent::MoveHeal { target, hp, max_hp, status: Status::None, move_id });
    remove(battle, pos, condition);
    true
}

/// Whether the Pokémon at `pos` was revived in its slot and has not
/// switched back in yet (its own `instaswitch` is still queued, or is
/// calling it back): Showdown's `isActive` is false, so damage and healing
/// fail on it and its ability and item are ignored.
///
/// Inlined, the test that almost always answers: no Pokémon revived in
/// its slot.
#[inline(always)]
pub(crate) fn off_field(battle: &Battle, pos: Position) -> bool {
    battle.order.revived_in_slot && revived_off_field(battle, pos)
}

/// [`off_field`] with a Pokémon revived in its slot.
#[inline(never)]
fn revived_off_field(battle: &Battle, pos: Position) -> bool {
    if battle.order.revived_switching == Some(pos) || battle.order.revived_resolving == Some(pos) {
        return true;
    }
    let Some(mon) = battle.state.at(pos) else { return false };
    let mon = MonRef { side: pos.side, id: mon.id };
    battle
        .queue
        .as_slice()
        .iter()
        .any(|a| a.mon == Some(mon) && matches!(a.kind, ActionKind::InstaSwitch { target, .. } if target == mon))
}

/// Whether a Pokémon revived in its slot is waiting for its switch: its
/// `instaswitch` is queued, or being resolved (`revived_resolving`). The
/// event masks then depend on the queue ([`off_field`]).
pub(crate) fn revived_waiting(battle: &Battle) -> bool {
    battle.order.revived_in_slot
        && (battle.order.revived_resolving.is_some()
            || battle.queue.as_slice().iter().any(|a| matches!(a.kind, ActionKind::InstaSwitch { .. })))
}

/// Showdown's `revivalblessing` action: the chosen fainted Pokémon comes
/// back with half its HP. One that fainted in an active slot (doubles) is
/// switched back in by an `instaswitch` added at the end of the queue.
pub(crate) fn revive(battle: &mut Battle, user: MonRef, target: MonRef) {
    let side = battle.state.side_mut(target.side);
    let Some(position) = side.position_of(target.id) else { return };
    let in_slot = position < battle.format.active_per_side();
    let mon = &mut battle.state.side_mut(target.side).team[position];
    // Fainting already ended its Terastallization (`faint_messages`).
    // `hp = 1; sethp(maxhp / 2)`.
    mon.status = Status::None;
    mon.hp = super::hp_fraction(u32::from(mon.max_hp()), 1, 2) as u16;
    let (hp, max_hp, species) = (mon.hp, mon.max_hp(), mon.species);
    if in_slot {
        // Showdown keeps it in its slot but off the field (`isActive` false,
        // no longer `fainted`) until the switch: it takes part in speed
        // sorts and can be targeted, but damage and healing fail, and its
        // ability and item are ignored (`off_field`). No move can come in
        // between: the `instaswitch` goes to the end of the queue, and
        // the Gen 8+ re-sort brings it first when a move is next; only the
        // residual (the user moved last) runs before it.
        battle.state.side_mut(target.side).active[position] = Some(position as u8);
        battle.order.revived_in_slot = true;
        let kind = ActionKind::InstaSwitch { target, from: None };
        // `addChoice` resolves the action (`getActionSpeed`) before queueing
        // it, with the Pokémon already off the field: its speed events find
        // only the field's handlers (`Battle.findEventHandlers` collects a
        // Pokémon's and its side's only for an `isActive` target).
        battle.order.revived_resolving = Some(Position::new(target.side, position as u8));
        queue::add_choice(battle, Action::new(kind, Some(target)));
        battle.order.revived_resolving = None;
    }
    let target_actor = Actor { pos: Position::new(target.side, 0), species, active: false, id: target.id };
    battle.emit(BattleEvent::MoveHeal {
        target: target_actor,
        hp,
        max_hp,
        status: Status::None,
        move_id: ids::REVIVAL_BLESSING,
    });
    let side = battle.state.side(user.side);
    if let Some(slot) = side.position_of(user.id).filter(|&p| p < battle.format.active_per_side()) {
        remove(battle, Position::new(user.side, slot as u8), SlotCondition::RevivalBlessing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::handlers::run_residual;
    use crate::state::SideId;
    use crate::testing::sample_team;
    use crate::{BattleOptions, Format, Seed};

    #[test]
    fn wish_heals_at_the_end_of_the_next_turn() {
        let format = Format::gen9_singles();
        let teams = [sample_team(&format), sample_team(&format)];
        let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
        let pos = Position::new(SideId::P1, 0);
        let max = b.state.at(pos).unwrap().max_hp();
        b.state.at_mut(pos).unwrap().hp = 10;
        assert!(add_slot_condition(&mut b, pos, "Wish", pos));
        assert!(!add_slot_condition(&mut b, pos, "Wish", pos), "one Wish per slot");
        run_residual(&mut b);
        assert_eq!(b.state.at(pos).unwrap().hp, 10);
        b.state.turn += 1;
        run_residual(&mut b);
        assert_eq!(b.state.at(pos).unwrap().hp, 10 + max / 2);
        assert!(b.state.side(SideId::P1).conditions.slots[0].order.is_empty());
    }

    /// A Wish made on turn 256 never resolves on the cartridge
    /// (`getOverflowedTurnCount` wraps at 256), but heals on turn 257 in
    /// Showdown's custom games (`Math.trunc`), which the Gen 9 presets
    /// follow.
    #[test]
    fn wish_on_turn_256_resolves_only_without_the_overflow() {
        for format in
            [Format::gen9_singles(), Format { team_preview: crate::TeamPreview::None, ..Format::champions_vgc() }]
        {
            let overflow = format.cartridge_overflow;
            let teams = [sample_team(&format), sample_team(&format)];
            let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
            let pos = Position::new(SideId::P1, 0);
            b.state.at_mut(pos).unwrap().hp = 10;
            b.state.turn = 256;
            assert!(add_slot_condition(&mut b, pos, "Wish", pos));
            for _ in 0..3 {
                b.state.turn += 1;
                run_residual(&mut b);
            }
            let pending = b.state.side(SideId::P1).conditions.slots[0].has(SlotCondition::Wish);
            assert_eq!(pending, overflow, "cartridge overflow: {overflow}");
        }
    }
}
