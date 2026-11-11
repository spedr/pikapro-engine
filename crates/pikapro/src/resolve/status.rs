//! Non-volatile status: burn, paralysis, poison, toxic, sleep, freeze.
//!
//! Showdown references: `brn`, `par`, `slp`, `frz`, `psn`, `tox` in
//! `data/conditions.ts` (and `data/mods/champions/conditions.ts` for the
//! Champions paralysis/sleep/freeze changes, behind `Mechanics::Champions`);
//! `Pokemon.trySetStatus` / `setStatus` / `cureStatus` in `sim/pokemon.ts`.
//!
//! Contract:
//! - [`try_set_status`]: inflict a status (type immunities, already
//!   statused, `-status` / `-fail` messages). Called by `runMoveEffects`
//!   for a move's primary `status` and by secondaries.
//! - [`before_move_handlers`]: `onBeforeMove` for par (priority 1), slp and
//!   frz (priority 10).
//! - [`residual_handlers`]: `onResidual` for brn (order 10), psn and tox
//!   (order 9).
//! - [`par_modify_spe`]: `onModifySpe` (paralysis halves Speed).
//! - [`modify_move`], [`frz_damaging_hit`], [`frz_after_move_secondary`]:
//!   the ways a frozen Pokémon thaws besides the `BeforeMove` roll.
//! - [`switch_in`]: `tox`'s `onSwitchIn` (the toxic counter restarts).
//!
//! Burn's damage halving is already applied in the damage formula.

use pikapro_data::gen9::moves as ids;
use pikapro_data::{Event, MoveCategory, MoveData, MoveFlags, MoveId, StatusId, Type};

use super::events::{Effect, EventCtx, Relay};
use super::handlers::{Callback, Handler, Handlers, Holder, sub_order};
use super::manipulation::has_type;
use super::moves::fail_still;
use super::{DamageSource, actor, apply_damage};
use crate::battle::Battle;
use crate::event::BattleEvent;
use crate::format::Mechanics;
use crate::state::{PokemonState, Position, SideId, Status};

/// Moves whose status effect needs code beyond their data: Thunder Wave's
/// `ignoreImmunity: false` (the move pipeline checks its type immunity).
pub(crate) fn handles(move_id: MoveId) -> bool {
    move_id == ids::THUNDER_WAVE
}

/// Gen 8+: Toxic used by a Poison type cannot miss (`hitStepAccuracy`).
pub(crate) fn always_hits(battle: &Battle, user: Position, move_id: MoveId) -> bool {
    move_id == ids::TOXIC && battle.state.at(user).is_some_and(|m| has_type(m, Type::Poison))
}

/// Which status a [`Status`] is, ignoring its counters.
pub(crate) fn kind(status: Status) -> Option<StatusId> {
    Some(match status {
        Status::None => return None,
        Status::Burn => StatusId::Burn,
        Status::Paralysis => StatusId::Paralysis,
        Status::Poison => StatusId::Poison,
        Status::Toxic { .. } => StatusId::Toxic,
        Status::Sleep { .. } => StatusId::Sleep,
        Status::Freeze { .. } => StatusId::Freeze,
    })
}

/// Showdown's `Pokemon.trySetStatus` → `setStatus`: tries to inflict
/// `status` on `target`. Returns whether it was applied.
///
/// `is_secondary` means the effect doing it has no `status` of its own
/// (secondaries, Yawn, Toxic Spikes, ...), so failures are silent; for a
/// primary status move (`is_secondary == false`) they are reported with
/// `-fail` / `-immune`. `source` defaults to the target, as in Showdown.
/// The effect (Showdown's `sourceEffect`) is the move being used, if any;
/// abilities and items call [`set_status_from`].
///
/// Sleep shows `[from] move: X` when a move is being used
/// (`battle.active_move`), since Showdown's `sourceEffect` is then the move.
pub(crate) fn try_set_status(
    battle: &mut Battle,
    target: Position,
    status: StatusId,
    source: Option<Position>,
    is_secondary: bool,
) -> bool {
    let effect = super::move_or_condition_effect(battle);
    set_status_from(battle, target, status, source, effect, is_secondary)
}

/// Showdown's `setStatus(status, source, sourceEffect)`: fails on a
/// Pokémon that already has a status; then (unless Corrosion poisons) the
/// type immunity and the `Immunity` event (`runStatusImmunity`: sun stops
/// freezing; Insomnia, Limber, ...), the `SetStatus` event (Uproar,
/// Safeguard, the terrains, Leaf Guard, ...), the status's `onStart`
/// (`-status`, `[from] ability: X|[of] <source>` or `[from] item: X`), and
/// the `AfterSetStatus` event (Synchronize, Lum Berry, ...).
pub(crate) fn set_status_from(
    battle: &mut Battle,
    target: Position,
    status: StatusId,
    source: Option<Position>,
    effect: Effect,
    is_secondary: bool,
) -> bool {
    // `Pokemon.setStatus`: `if (!this.hp) return false;
    // if (!this.isActive && status) return false;`. A Pokémon off the field
    // (at its team position: a Future Sight user that left, hit back by
    // Spicy Spray) is refused, silently.
    if target.slot as usize >= battle.format.active_per_side() {
        return false;
    }
    let Some(mon) = battle.state.at(target) else { return false };
    if mon.is_fainted() {
        return false;
    }
    // `trySetStatus` passes the current status if there is one, so any
    // status fails against an already statused Pokémon.
    if let Some(current) = kind(mon.status) {
        if !is_secondary {
            if current == status {
                let target_actor = actor(battle, target);
                battle.emit(BattleEvent::StatusFail { target: target_actor, status: mon.status });
            } else {
                fail_still(battle, source.unwrap_or(target));
            }
        }
        return false;
    }
    // `runStatusImmunity` (toxic checks poison's immunity): the type
    // immunity, then the `Immunity` event, where sun and Desolate Land's
    // harsh sun block freezing. Corrosion poisons anything.
    let corrosion = matches!(status, StatusId::Poison | StatusId::Toxic)
        && source.is_some_and(|s| super::abilities::has_ability(battle, s, pikapro_data::gen9::abilities::CORROSION));
    if !corrosion {
        let immune = match status {
            StatusId::Burn => &[Type::Fire][..],
            StatusId::Paralysis => &[Type::Electric],
            StatusId::Freeze => &[Type::Ice],
            StatusId::Poison | StatusId::Toxic => &[Type::Poison, Type::Steel],
            StatusId::Sleep => &[],
        };
        let id = match status {
            StatusId::Burn => "brn",
            StatusId::Paralysis => "par",
            StatusId::Freeze => "frz",
            StatusId::Poison | StatusId::Toxic => "psn",
            StatusId::Sleep => "slp",
        };
        let immune =
            immune.iter().any(|&t| has_type(mon, t)) || super::events::immunity_event(battle, target, id) != Some(true);
        if immune {
            if !is_secondary {
                battle.emit(BattleEvent::Immune { target: actor(battle, target) });
            }
            return false;
        }
    }
    // `SetStatus`: Pokémon, side, terrain and rule handlers share one
    // sort (Champions Trick Room makes Pokémon slower than speed zero).
    let mut ev = super::events::EventCtx::new(
        Event::SetStatus,
        super::events::Target::Pokemon(target),
        source.or(Some(target)),
        effect,
        super::events::Relay::Status(status),
    );
    if !super::events::run_event(battle, &mut ev, None).is_truthy() {
        return false;
    }

    // `onStart`: the message, then the status's own state.
    let from_move = match (status, effect) {
        (StatusId::Sleep, Effect::Move(m)) => Some(m),
        _ => None,
    };
    let champions = battle.format.mechanics == Mechanics::Champions;
    let placeholder = match status {
        StatusId::Burn => Status::Burn,
        StatusId::Paralysis => Status::Paralysis,
        StatusId::Poison => Status::Poison,
        StatusId::Toxic => Status::Toxic { counter: 0 },
        StatusId::Sleep => Status::Sleep { turns: 0 },
        // The Champions mod thaws a Pokémon after 3 turns at most.
        StatusId::Freeze => Status::Freeze { turns: if champions { 3 } else { 0 } },
    };
    let target_actor = actor(battle, target);
    match effect {
        Effect::Ability(a) => {
            let of = actor(battle, source.unwrap_or(target));
            let cause = crate::event::Cause::ability(a).of(of);
            battle.emit(BattleEvent::StatusFrom { target: target_actor, status: placeholder, cause });
        }
        // `slp`'s `onStart` names only abilities and moves.
        Effect::Item(i) if status != StatusId::Sleep => {
            let cause = crate::event::Cause::item(i);
            battle.emit(BattleEvent::StatusFrom { target: target_actor, status: placeholder, cause });
        }
        _ => battle.emit(BattleEvent::StatusStart { target: target_actor, status: placeholder, from_move }),
    }
    let status_value = match placeholder {
        // `startTime = random(2, 5)`; the Champions mod uses `sample([2, 3, 3])`.
        Status::Sleep { .. } => {
            let turns = if champions { *battle.rng.sample(&[2, 3, 3]) } else { battle.rng.random_range(2, 5) as u8 };
            Status::Sleep { turns }
        }
        other => other,
    };
    let mon = &mut battle.state.side_mut(target.side).team[target.slot as usize];
    mon.status = status_value;
    if status == StatusId::Sleep {
        // `this.statusState.source = source`; only whose side it was on
        // matters (Sleep Clause Mod).
        mon.sleep_source_ally = source.is_none_or(|s| s.side == target.side);
    }
    // `frz`'s `onStart`: Shaymin-Sky reverts to Land Forme
    // (`target.species.name === 'Shaymin-Sky' && target.baseSpecies.baseSpecies
    // === 'Shaymin'`: the species it transformed into, its own base species).
    let shaymin = |m: &crate::state::PokemonState| {
        use pikapro_data::gen9::species as s;
        super::tera::species_of(m) == s::SHAYMIN_SKY
            && battle.format.dex.species(m.base_species).base_species == s::SHAYMIN
    };
    if status == StatusId::Freeze && battle.state.at(target).is_some_and(shaymin) {
        let effect = Effect::Status(StatusId::Freeze);
        super::forme::forme_change(battle, target, pikapro_data::gen9::species::SHAYMIN, effect, true, false);
    }
    // `runEvent('AfterSetStatus', target, source, sourceEffect, status)`.
    let mut ev = super::events::EventCtx::new(
        Event::AfterSetStatus,
        super::events::Target::Pokemon(target),
        source.or(Some(target)),
        effect,
        super::events::Relay::Status(status),
    );
    super::events::run_event(battle, &mut ev, None).is_truthy()
}

/// The rule sorts at speed zero, sub-order five, after terrains and
/// side conditions, before a Pokémon with negative stored speed.
pub(crate) fn sleep_clause_set_status(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    if !matches!(ev.relay, Relay::Status(StatusId::Sleep)) {
        return None;
    }
    let target = ev.target_pos()?;
    sleep_clause_blocks(battle, target, ev.source.unwrap_or(target)).then_some(Relay::False)
}

/// Sleep Clause Mod's `onSetStatus` (`data/rulesets.js`): true blocks.
/// A move used by an ally doesn't count; the source is `source` (the
/// target itself if it has none). `battle.hint` follows the message.
fn sleep_clause_blocks(battle: &mut Battle, target: Position, source: Position) -> bool {
    if !battle.format.sleep_clause_mod || source.side == target.side {
        return false;
    }
    let asleep =
        |m: &crate::state::PokemonState| m.hp > 0 && matches!(m.status, Status::Sleep { .. }) && !m.sleep_source_ally;
    if !battle.state.side(target.side).team.iter().any(asleep) {
        return false;
    }
    battle.emit(BattleEvent::Message { message: crate::event::Message::SleepClauseMod });
    battle.hint(crate::event::Hint::SleepClauseMod);
    true
}

/// Showdown's `cureStatus`: `-curestatus|...|[msg]` and the status is gone.
pub(crate) fn cure_status(battle: &mut Battle, pos: Position) {
    let Some(mon) = battle.state.at(pos) else { return };
    if mon.is_fainted() || mon.status == Status::None {
        return;
    }
    let (status, target) = (mon.status, actor(battle, pos));
    battle.emit(BattleEvent::CureStatus { target, status, from_move: None });
    battle.state.at_mut(pos).expect("checked above").status = Status::None;
}

pub(crate) fn before_move_handlers<const N: usize>(
    battle: &Battle,
    user: Position,
    handlers: &mut super::handlers::SpillList<N>,
) {
    let Some(mon) = battle.state.at(user) else { return };
    let (priority, run): (i32, fn(&mut Battle, &Handler) -> bool) = match mon.status {
        Status::Paralysis => (1, par_before_move),
        Status::Sleep { .. } => (10, slp_before_move),
        Status::Freeze { .. } => (10, frz_before_move),
        _ => return,
    };
    handlers.push(Handler {
        priority,
        speed: mon.speed,
        sub_order: sub_order::STATUS,
        holder: Holder::Pokemon(user),
        callback: Callback::Plain(run),
        ..Handler::default()
    });
}

/// The holder of a status handler, if it still has `status`
/// (Showdown skips handlers whose status changed since they were collected).
fn holder_with(battle: &Battle, handler: &Handler, status: StatusId) -> Option<Position> {
    let Holder::Pokemon(pos) = handler.holder else { return None };
    battle.state.at(pos).filter(|m| kind(m.status) == Some(status)).map(|_| pos)
}

/// `cant|<pokemon>|<status>`: the status stops the move.
fn cant(battle: &mut Battle, pos: Position) -> bool {
    let status = battle.state.at(pos).expect("holder is active").status;
    let actor = actor(battle, pos);
    battle.emit(BattleEvent::StatusCant { actor, status });
    false
}

/// `par`'s `onBeforeMove`: fully paralysed with chance 1/4 (1/8 in the
/// Champions mod).
fn par_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let Some(pos) = holder_with(battle, handler, StatusId::Paralysis) else { return true };
    let denominator = if battle.format.mechanics == Mechanics::Champions { 8 } else { 4 };
    if battle.rng.chance(1, denominator) {
        return cant(battle, pos);
    }
    true
}

/// `slp`'s `onBeforeMove`: count down, wake at 0; `sleepUsable` moves
/// (Sleep Talk, Snore) are used while asleep.
fn slp_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let Some(pos) = holder_with(battle, handler, StatusId::Sleep) else { return true };
    // Early Bird counts down twice.
    let extra = super::abilities::misc::early_bird(battle, pos);
    let mon = battle.state.at_mut(pos).expect("holder is active");
    let Status::Sleep { turns } = &mut mon.status else { unreachable!("checked by holder_with") };
    *turns = turns.saturating_sub(1 + extra);
    if *turns == 0 {
        cure_status(battle, pos);
        return true;
    }
    cant(battle, pos);
    battle.active_move.is_some_and(|(_, m)| m == ids::SLEEP_TALK || m == ids::SNORE)
}

/// `frz`'s `onBeforeMove`: `defrost` moves thaw the user (in
/// [`modify_move`]); otherwise it thaws with chance 1/5. The Champions mod
/// thaws after 3 turns at most, and with chance 1/4 before that.
fn frz_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let Some(pos) = holder_with(battle, handler, StatusId::Freeze) else { return true };
    if let Some((_, move_id)) = battle.active_move {
        let data = battle.format.dex.move_(move_id);
        let user = battle.state.at(pos).expect("holder is active");
        // Burn Up only defrosts a Fire-type user.
        if data.flags.contains(MoveFlags::DEFROST) && (move_id != ids::BURN_UP || has_type(user, Type::Fire)) {
            return true;
        }
    }
    let thawed = match battle.format.mechanics {
        Mechanics::ScarletViolet => battle.rng.chance(1, 5),
        Mechanics::Champions => {
            let mon = battle.state.at_mut(pos).expect("holder is active");
            let Status::Freeze { turns } = &mut mon.status else { unreachable!("checked by holder_with") };
            *turns = turns.saturating_sub(1);
            *turns == 0 || battle.rng.chance(1, 4)
        }
    };
    if thawed {
        cure_status(battle, pos);
        return true;
    }
    cant(battle, pos)
}

/// `frz`'s `onModifyMove` (runs before the `|move|` line): a frozen user of
/// a `defrost` move thaws.
pub(crate) fn modify_move(battle: &mut Battle, user: Position, move_id: MoveId, data: &MoveData) {
    let Some(mon) = battle.state.at(user) else { return };
    if matches!(mon.status, Status::Freeze { .. }) && data.flags.contains(MoveFlags::DEFROST) {
        let (status, target) = (mon.status, actor(battle, user));
        battle.emit(BattleEvent::CureStatus { target, status, from_move: Some(move_id) });
        battle.state.at_mut(user).expect("checked above").status = Status::None;
    }
}

/// `frz`'s `onDamagingHit`: a damaging Fire-type move (not Polar Flare)
/// thaws the target.
pub(crate) fn frz_damaging_hit(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    let target = ev.target_pos()?;
    let data = ev.move_data()?;
    // (Polar Flare, which doesn't thaw, is not in the Gen 9 dex.)
    if data.ty == Type::Fire && data.category != MoveCategory::Status {
        cure_status(battle, target);
    }
    None
}

/// `frz`'s `onAfterMoveSecondary`: moves with `thawsTarget` (Scald, ...)
/// thaw their targets.
pub(crate) fn frz_after_move_secondary(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    let target = ev.target_pos()?;
    if ev.move_data()?.unmodelled.contains(&"thawsTarget") {
        cure_status(battle, target);
    }
    None
}

/// `onResidual` for brn (order 10), psn and tox (order 9).
///
/// Like Showdown's `fieldEvent`, this includes a fainted Pokémon still in its
/// slot whose status `checkFainted` has not cleared yet: its handler is
/// skipped when run, but it takes part in the sort (and its ties).
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    for side in SideId::BOTH {
        let team = &battle.state.side(side).team;
        for slot in 0..battle.format.active_per_side().min(team.len()) {
            // (After a stale switch, the fainted Pokémon twice.)
            let index = super::field_index(battle, side, slot);
            let pos = Position::new(side, index as u8);
            let mon = &team[index];
            let (order, run): (u8, fn(&mut Battle, &Handler) -> bool) = match mon.status {
                Status::Burn => (10, brn_residual),
                Status::Poison => (9, psn_residual),
                Status::Toxic { .. } => (9, tox_residual),
                _ => continue,
            };
            handlers.push(Handler {
                order: Some(order),
                speed: mon.speed,
                sub_order: sub_order::STATUS,
                holder: Holder::Pokemon(pos),
                callback: Callback::Plain(run),
                ..Handler::default()
            });
        }
    }
}

/// Residual damage: `multiplier` times `1/denominator` of max HP (the
/// fraction at least 1).
fn residual_damage(battle: &mut Battle, pos: Position, denominator: u32, multiplier: u32) {
    let mon = battle.state.at(pos).expect("holder is active");
    let amount = super::hp_fraction(mon.max_hp() as u32, 1, denominator) * multiplier;
    let status = mon.status;
    apply_damage(battle, pos, amount, DamageSource::Status(status));
}

fn brn_residual(battle: &mut Battle, handler: &Handler) -> bool {
    if let Some(pos) = holder_with(battle, handler, StatusId::Burn) {
        residual_damage(battle, pos, 16, 1);
    }
    true
}

fn psn_residual(battle: &mut Battle, handler: &Handler) -> bool {
    if let Some(pos) = holder_with(battle, handler, StatusId::Poison) {
        residual_damage(battle, pos, 8, 1);
    }
    true
}

/// `tox`: the counter goes up (to 15 at most), then `counter/16` max HP.
fn tox_residual(battle: &mut Battle, handler: &Handler) -> bool {
    if let Some(pos) = holder_with(battle, handler, StatusId::Toxic) {
        let mon = battle.state.at_mut(pos).expect("holder is active");
        let Status::Toxic { counter } = &mut mon.status else { unreachable!("checked by holder_with") };
        *counter = (*counter + 1).min(15);
        let stage = *counter as u32;
        residual_damage(battle, pos, 16, stage);
    }
    true
}

/// `tox`'s `onSwitchIn`: the toxic counter restarts.
pub(crate) fn switch_in(mon: &mut PokemonState) {
    if let Status::Toxic { counter } = &mut mon.status {
        *counter = 0;
    }
}

/// `par`'s `onModifySpe` (priority -101): the modifiers so far are applied
/// (`finalModify`), then the Speed is halved, rounded down, unless the
/// Pokémon has Quick Feet.
pub(crate) fn par_modify_spe(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Holder::Pokemon(pos) = h.holder else { return None };
    let speed = ev.final_modify(ev.value().max(0) as u32);
    if super::abilities::has_ability(battle, pos, pikapro_data::gen9::abilities::QUICK_FEET) {
        return Some(Relay::Num(speed as i32));
    }
    Some(Relay::Num((speed * 50 / 100) as i32))
}

/// Whether the Pokémon is poisoned or badly poisoned.
pub(crate) fn poisoned(m: &PokemonState) -> bool {
    matches!(m.status, Status::Poison | Status::Toxic { .. })
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::moves as m;

    use super::*;
    use crate::battle::BattleOptions;
    use crate::format::Format;
    use crate::rng::Seed;
    use crate::state::Weather;
    use crate::testing::sample_team;

    const P1: Position = Position::new(SideId::P1, 0);
    const P2: Position = Position::new(SideId::P2, 0);

    /// Singles, both leads Incineroar (Fire/Dark), no events so far.
    fn battle(format: Format, seed: u64) -> Battle {
        let teams = [sample_team(&format), sample_team(&format)];
        let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(seed))).unwrap();
        b.log.drain();
        b
    }

    fn singles() -> Battle {
        battle(Format::gen9_singles(), 1)
    }

    fn status(b: &Battle, pos: Position) -> Status {
        b.state.at(pos).unwrap().status
    }

    #[test]
    fn type_immunity_is_silent_for_secondaries() {
        let mut b = singles();
        assert!(!try_set_status(&mut b, P2, StatusId::Burn, Some(P1), true));
        assert!(b.log.events().is_empty());
        assert!(!try_set_status(&mut b, P2, StatusId::Burn, Some(P1), false));
        assert!(matches!(b.log.events(), [BattleEvent::Immune { .. }]));
        assert_eq!(status(&b, P2), Status::None);
    }

    #[test]
    fn sun_prevents_freezing() {
        let mut b = singles();
        b.state.field.weather = Some(Weather::Sun);
        b.conditions_changed();
        assert!(!try_set_status(&mut b, P2, StatusId::Freeze, Some(P1), true));
        assert!(b.log.events().is_empty());
        b.state.field.weather = Some(Weather::Rain);
        b.conditions_changed();
        assert!(try_set_status(&mut b, P2, StatusId::Freeze, Some(P1), true));
    }

    #[test]
    fn already_statused() {
        let mut b = singles();
        assert!(try_set_status(&mut b, P2, StatusId::Paralysis, Some(P1), false));
        assert_eq!(status(&b, P2), Status::Paralysis);
        b.log.drain();
        // Same status: `-fail|target|par`; another one: `-fail|source`.
        assert!(!try_set_status(&mut b, P2, StatusId::Paralysis, Some(P1), false));
        assert!(matches!(b.log.events(), [BattleEvent::StatusFail { status: Status::Paralysis, .. }]));
        b.log.drain();
        assert!(!try_set_status(&mut b, P2, StatusId::Poison, Some(P1), false));
        assert!(matches!(
            b.log.events(),
            [BattleEvent::MoveAttr { attr: crate::event::MoveAttr::Still }, BattleEvent::Fail { user }]
                if user.pos == P1
        ));
        b.log.drain();
        assert!(!try_set_status(&mut b, P2, StatusId::Sleep, Some(P1), true));
        assert!(b.log.events().is_empty());
    }

    #[test]
    fn psycho_shift_cures_only_after_a_successful_primary_status() {
        use crate::{Choice, PokemonSet, SlotAction, Team};
        use pikapro_data::gen9::{abilities as a, species as s};

        for (initial, recipient, target_ability, succeeds) in [
            (Status::None, s::BLISSEY, a::NO_ABILITY, false),
            (Status::Burn, s::BLISSEY, a::NO_ABILITY, true),
            (Status::Burn, s::CHARIZARD, a::NO_ABILITY, false),
            (Status::Poison, s::TOXAPEX, a::NO_ABILITY, false),
            (Status::Paralysis, s::BLISSEY, a::LIMBER, false),
        ] {
            let user = PokemonSet::new(s::MEW).with_ability(a::NO_ABILITY).with_moves(&[m::PSYCHO_SHIFT]);
            let target = PokemonSet::new(recipient).with_ability(target_ability).with_moves(&[m::SPLASH]);
            let teams = [Team::new(vec![user]), Team::new(vec![target])];
            let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(45_000_100))).unwrap();
            b.state.at_mut(P1).unwrap().status = initial;
            for side in SideId::BOTH {
                b.submit(side, Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None })).unwrap();
            }
            assert_eq!(status(&b, P1), if succeeds { Status::None } else { initial });
            assert_eq!(status(&b, P2), if succeeds { initial } else { Status::None });
            if initial == Status::Paralysis {
                assert!(b.events().iter().any(|e| matches!(e, BattleEvent::ImmuneFrom { .. })));
            }
        }
    }

    #[test]
    fn sleep_duration() {
        for seed in 0..50 {
            let mut b = singles();
            b.rng = crate::rng::BattleRng::new(Seed::fast(seed));
            b.active_move = Some((P1, m::SPORE));
            assert!(try_set_status(&mut b, P2, StatusId::Sleep, Some(P1), false));
            let Status::Sleep { turns } = status(&b, P2) else { panic!() };
            assert!((2..=4).contains(&turns));
            assert!(matches!(b.log.events(), [BattleEvent::StatusStart { from_move: Some(m::SPORE), .. }]));

            let mut b = battle(Format::champions_vgc(), seed);
            let pos = Position::new(SideId::P2, 1);
            if b.state.at(pos).is_none() {
                continue;
            }
            assert!(try_set_status(&mut b, pos, StatusId::Sleep, None, true));
            let Status::Sleep { turns } = status(&b, pos) else { panic!() };
            assert!((2..=3).contains(&turns));
        }
    }

    #[test]
    fn residual_damage_and_toxic_counter() {
        let mut b = singles();
        let max = b.state.at(P2).unwrap().max_hp();
        b.state.at_mut(P2).unwrap().status = Status::Toxic { counter: 0 };
        let handler = Handler { holder: Holder::Pokemon(P2), ..Handler::default() };
        tox_residual(&mut b, &handler);
        tox_residual(&mut b, &handler);
        assert_eq!(b.state.at(P2).unwrap().hp, max - 3 * (max / 16));
        assert_eq!(status(&b, P2), Status::Toxic { counter: 2 });
        switch_in(b.state.at_mut(P2).unwrap());
        assert_eq!(status(&b, P2), Status::Toxic { counter: 0 });

        let mut b = singles();
        b.state.at_mut(P1).unwrap().status = Status::Burn;
        brn_residual(&mut b, &Handler { holder: Holder::Pokemon(P1), ..Handler::default() });
        assert_eq!(b.state.at(P1).unwrap().hp, max - max / 16);
        // A handler whose status is gone does nothing.
        psn_residual(&mut b, &Handler { holder: Holder::Pokemon(P1), ..Handler::default() });
        assert_eq!(b.state.at(P1).unwrap().hp, max - max / 16);
    }

    #[test]
    fn fire_moves_thaw() {
        let mut b = singles();
        b.state.at_mut(P1).unwrap().status = Status::Freeze { turns: 0 };
        let hit = |b: &mut Battle, move_id| {
            let data = pikapro_data::gen9::DEX.move_(move_id);
            let mut relays = [crate::resolve::events::Relay::Num(1)];
            let effect = crate::resolve::events::Effect::Move(move_id);
            let event = pikapro_data::Event::DamagingHit;
            crate::resolve::events::run_event_multi(b, event, &[P1], Some(P2), effect, &mut relays, Some(data));
        };
        hit(&mut b, m::KNOCK_OFF);
        assert!(matches!(status(&b, P1), Status::Freeze { .. }));
        hit(&mut b, m::FLAMETHROWER);
        assert_eq!(status(&b, P1), Status::None);
        assert!(matches!(b.log.events(), [BattleEvent::CureStatus { from_move: None, .. }]));

        // A frozen user of a `defrost` move thaws before moving.
        b.log.drain();
        b.state.at_mut(P1).unwrap().status = Status::Freeze { turns: 0 };
        modify_move(&mut b, P1, m::FLARE_BLITZ, pikapro_data::gen9::DEX.move_(m::FLARE_BLITZ));
        assert_eq!(status(&b, P1), Status::None);
        assert!(matches!(b.log.events(), [BattleEvent::CureStatus { from_move: Some(m::FLARE_BLITZ), .. }]));
    }
}
