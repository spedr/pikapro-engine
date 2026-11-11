//! [`PublicTracker::apply`]: what each event tells a spectator.
//!
//! One `match` over [`BattleEvent`], without allocation. Every variant is
//! listed, so a new engine event has to be decided on here. The rules
//! follow what the protocol line shows (`pikapro-protocol`'s renderer): a
//! field the line doesn't show (`Switch::team_index`, a status's hidden
//! counters, exact HP under [`HpReport::Percent`]) is never read.
//!
//! Who an effect belongs to, in Showdown's lines:
//! * `[from] <effect>|[of] <pokemon>` on `-damage`, `-status`,
//!   `-curestatus`, `-boost`/`-unboost`, `-immune`, `-fail`, `-clearboost`,
//!   `-start` and `-formechange`: the `[of]` Pokémon's, else the line's
//!   (Rough Skin and Rocky Helmet name their holder with `[of]`, Life Orb
//!   and Moxie don't).
//! * `-heal|<pokemon>|...|[from] ability: Water Absorb|[of] <attacker>`:
//!   the healed Pokémon's; `[of]` is the attacker (but Hospitality's `[of]`
//!   is its holder).
//! * `-item|<pokemon>|<item>|[from] ability: X|[of] <other>`: the line's
//!   Pokémon's (Magician, Pickpocket, Harvest), except Frisk (`[of]` is
//!   the frisker, the line's Pokémon the item's holder).
//! * `-activate`, `-block`, `cant|<pokemon>|ability: X`: the line's.
//! * `-weather` and `-fieldstart` with `[from] ability: X|[of] <holder>`.

use pikapro::battle::move_max_pp;
use pikapro::event::{
    AbilityNote, Actor, CantReason, Cause, DamageSource, EffectName, FieldEffect, HealSource, Hint, ItemEndNote,
    LineArg, ManipulationVolatile, MoveAnim, MoveAttr, MoveFrom, Volatile, VolatileEffect,
};
use pikapro::state::{SideCondition, Terrain};
use pikapro::util::StackVec;
use pikapro::{BattleEvent, Gender, MAX_ACTIVE, MAX_TEAM, SideId, Status};
use pikapro_data::gen9::{abilities, items, moves, species};
use pikapro_data::{AbilityId, BoostId, ItemId, MoveFlags, MoveId, MoveTarget, SpeciesId, Type, TypeSet};
use pikapro_protocol::{HpHint, details_species, hp_shown, preview_wildcard};

use crate::mon::{AbilityState, HpView, ItemLoss, ItemState, MonFlags, MonView, OwnItem, RevealedMove, StatusKind};
use crate::side::{
    ActiveView, CantKind, ConditionView, FutureView, MoveOutcome, PublicVolatiles, PublicVolatiles as V,
    SlotConditions, TurnAction, TurnRecord, TurnTarget, taken_by,
};
use crate::tracker::{HpReport, ItemSwap, Outcome, Phase, PublicTracker, fill_from_sheet, single_ability};

/// Volatiles that last the rest of the turn (`duration: 1`: cleared at
/// `|upkeep`, or at `|turn|`).
const THIS_TURN: u64 =
    V::PROTECT | V::ENDURE | V::ROOST | V::HELPING_HAND | V::CENTER_OF_ATTENTION | V::ELECTRIFY | V::FOCUSING;

/// What Baton Pass does not pass on (Showdown's `noCopy` conditions, and
/// what belongs to the Pokémon itself: its types, its ability's
/// conditions, its move locks).
const NO_BATON_PASS: u64 = V::ENCORE
    | V::DISABLE
    | V::TRANSFORMED
    | V::MIMIC
    | V::ATTRACT
    | V::DESTINY_BOND
    | V::GLAIVE_RUSH
    | V::IMPRISON
    | V::SALT_CURE
    | V::SMACK_DOWN
    | V::SYRUP_BOMB
    | V::TORMENT
    | V::YAWN
    | V::TRAPPED
    | V::DEFENSE_CURL
    | V::LOCK_ON
    | V::MINIMIZE
    | V::STOCKPILE
    | V::COMMANDING
    | V::COMMANDED
    | V::FLASH_FIRE
    | V::PARADOX_BOOST
    | V::SLOW_START
    | V::NEUTRALIZING_GAS
    | V::TYPE_CHANGE
    | V::TYPE_ADD
    | V::MUST_RECHARGE
    | V::CHARGING
    | V::LOCKED_MOVE
    | V::FOCUSING
    | THIS_TURN;

/// The volatiles whose durations count the residuals
/// ([`ObservedCounters`](crate::ObservedCounters)).
const TIMED: u64 = V::CONFUSION
    | V::TAUNT
    | V::ENCORE
    | V::DISABLE
    | V::YAWN
    | V::HEAL_BLOCK
    | V::THROAT_CHOP
    | V::MAGNET_RISE
    | V::PARTIALLY_TRAPPED
    | V::UPROAR
    | V::SLOW_START
    | V::SYRUP_BOMB;

/// Moves whose success Showdown's `stall` counter tracks.
fn is_stall_move(m: MoveId) -> bool {
    matches!(
        m,
        moves::PROTECT
            | moves::DETECT
            | moves::ENDURE
            | moves::KINGS_SHIELD
            | moves::SPIKY_SHIELD
            | moves::BANEFUL_BUNKER
            | moves::OBSTRUCT
            | moves::SILK_TRAP
            | moves::BURNING_BULWARK
            | moves::WIDE_GUARD
            | moves::QUICK_GUARD
    )
}

impl PublicTracker {
    /// Updates the record with one event. Events the protocol doesn't show
    /// (`Rng`, `Poke` without team preview) or that say nothing a client
    /// keeps change nothing.
    pub fn apply(&mut self, event: &BattleEvent) {
        use BattleEvent as E;
        match *event {
            // Setup.
            E::Poke { side, species, level, gender } => {
                if self.config.team_preview {
                    self.preview(side, details_species(species), level, gender);
                }
            }
            E::TeamPreview { .. } => self.phase = Phase::TeamPreview,
            E::TeamSize { side, size } => self.sides[side.index()].team_size = size,
            E::Start => {
                self.phase = Phase::Battle;
                self.crown_at_start();
            }
            E::Turn { turn } => self.new_turn(turn),
            E::Upkeep => self.upkeep(),
            // A foreseen attack whose target fainted: it resolved.
            E::Hint { hint: Hint::FutureMoveMissed { fainted: true, .. } } => {
                for side in &mut self.sides {
                    for slot in 0..MAX_ACTIVE {
                        let gone = side.slots[slot].row.is_none_or(|r| side.mons[r as usize].is_fainted());
                        if gone && side.future[slot].turns.saturating_add(1) >= FutureView::TURNS {
                            side.future[slot] = FutureView::default();
                        }
                    }
                }
            }
            E::Hint { .. } => {}
            // `|`: the turn's actions or residuals begin; no move is going on.
            E::Blank => {
                // The residuals begin, unless a `|t:|` follows (a turn or a
                // resumed turn begins).
                self.ctx.residual = true;
                self.ctx.item_ended = None;
                self.ctx.ability_lost = None;
                self.stall_kept();
                // A Dancer copy that never showed (confusion stopped it)
                // ends with its action.
                self.ctx.dancing = None;
                self.ctx.pressure = None;
                self.ctx.released = None;
                self.ctx.guard_again = None;
                self.ctx.mover = None;
                self.ctx.move_id = None;
                self.ctx.move_target = None;
                self.ctx.contact = None;
                self.commit_target();
                self.ctx.acting = None;
                self.ctx.moving = None;
                self.ctx.slot_made = None;
            }
            E::Win { side } => {
                self.command_outlives_the_end();
                self.outcome = Some(Outcome::Win(side));
                self.phase = Phase::Over;
            }
            E::Tie => {
                self.command_outlives_the_end();
                self.outcome = Some(Outcome::Tie);
                self.phase = Phase::Over;
            }

            // Switches and identity.
            E::Switch { actor, level, hp, max_hp, status, tera, from, shown, gender, .. } => {
                let species = details_species(shown.unwrap_or(actor.species));
                self.switch_in(actor, species, level, gender, hp, max_hp, status, tera, from, false);
            }
            E::Drag { actor, level, hp, max_hp, status, tera, shown, gender, .. } => {
                let species = details_species(shown.unwrap_or(actor.species));
                self.switch_in(actor, species, level, gender, hp, max_hp, status, tera, None, true);
            }
            E::Replace { actor, species, level, gender } => {
                self.replace(actor, details_species(species), level, gender)
            }
            E::DetailsChange { actor, species, level, tera, gender, .. } => {
                let species = details_species(species);
                let dex = self.config.dex;
                if let Some(mon) = self.mon_mut(actor) {
                    mon.species = species;
                    mon.details_species = species;
                    mon.level = level;
                    mon.gender = gender;
                    mon.flags.set(MonFlags::FORME_HIDDEN, false);
                    if let Some(t) = tera {
                        mon.tera_type = Some(t);
                        mon.flags.set(MonFlags::TERASTALLIZED, true);
                    }
                }
                // `formeChange` → `setSpecies` → `setType(species.types,
                // true)`: a permanent forme change (Mega Evolution) ends a
                // type change too, as a temporary one does, even while
                // transformed (4,550,222: a transformed Tyranitar's Mega
                // Evolution gives it Tyranitar-Mega's types).
                if let Some(av) = self.active_mut(actor) {
                    av.types = None;
                    av.added_type = None;
                    av.volatiles.clear(V::TYPE_CHANGE | V::TYPE_ADD);
                }
                // A permanent forme change (Mega Evolution, a Tera forme)
                // sets the forme's ability, its own from then on
                // (`formeChange`'s `setAbility` and `baseAbility`).
                if let Some(a) = single_ability(dex, species) {
                    if let Some(mon) = self.mon_mut(actor) {
                        mon.ability = AbilityState::Known(a);
                        mon.base_ability = AbilityState::Known(a);
                    }
                    if let Some(av) = self.active_mut(actor) {
                        av.ability_changed = false;
                    }
                }
            }
            E::FormeChange { actor, species, cause, .. } => {
                self.forme_change(actor, species);
                if let Some(c) = cause {
                    self.reveal_cause(actor, c);
                }
            }
            E::FormeChangeSilent { actor, species, cause } => {
                self.forme_change(actor, species);
                self.reveal_cause(actor, cause);
            }
            E::MegaEvolve { actor, item, .. } => {
                self.sides[actor.pos.side.index()].mega_used = true;
                if let Some(mon) = self.mon_mut(actor) {
                    mon.flags.set(MonFlags::MEGA, true);
                    if let Some(item) = item {
                        mon.item = ItemState::Known(item);
                    }
                }
            }
            E::Terastallize { actor, ty } => {
                self.sides[actor.pos.side.index()].tera_used = true;
                let dex = self.config.dex;
                let mut transformed = false;
                if let Some(av) = self.active_mut(actor) {
                    av.added_type = None;
                    av.volatiles.clear(V::TYPE_ADD);
                    transformed = av.volatiles.has(V::TRANSFORMED);
                }
                if let Some(mon) = self.mon_mut(actor) {
                    mon.flags.set(MonFlags::TERASTALLIZED, true);
                    mon.tera_type = Some(ty);
                    // A Morpeko Terastallized in its Hangry forme keeps it
                    // (`terastallize`: its `baseSpecies` becomes the forme),
                    // unless transformed.
                    if !transformed && dex.species(mon.species).base_species == species::MORPEKO {
                        mon.details_species = mon.species;
                    }
                }
            }
            E::Transform { actor, target } => self.transform(actor, target),
            E::TransformFrom { actor, target, cause } => {
                self.reveal_cause(actor, cause);
                self.transform(actor, target);
            }

            // Moves.
            E::Move { user, move_id, target, from } => {
                self.use_move(user, move_id, target, from);
            }
            E::MoveNull { user, move_id, from } => {
                self.use_move(user, move_id, None, from);
            }
            E::Anim { user, move_id, .. } => {
                // A charge the weather or Power Herb skipped: the move goes
                // off at once.
                if let Some(av) = self.active_mut(user)
                    && av.volatiles.has(V::CHARGING)
                    && av.locked_move == Some(move_id)
                {
                    av.volatiles.clear(V::CHARGING);
                    av.locked_move = None;
                }
            }
            E::Prepare { user, move_id } => {
                if move_id != moves::CHILLY_RECEPTION
                    && let Some(av) = self.active_mut(user)
                {
                    av.volatiles.set(V::CHARGING);
                    av.locked_move = Some(move_id);
                    self.ctx.lock_residuals[user.pos.side.index()][user.pos.slot as usize % MAX_ACTIVE] = 0;
                }
            }
            E::MustRecharge { actor } => {
                self.set_volatile(actor, V::MUST_RECHARGE);
                self.ctx.lock_residuals[actor.pos.side.index()][actor.pos.slot as usize % MAX_ACTIVE] = 0;
            }
            E::Cant { actor, reason } => {
                self.cant(actor);
                self.cant_action(
                    actor,
                    match reason {
                        CantReason::Flinch => CantKind::Flinch,
                        CantReason::Recharge => CantKind::Recharge,
                        CantReason::Taunt(_) => CantKind::Taunt,
                        CantReason::Disable(_) => CantKind::Disable,
                    },
                );
                match reason {
                    CantReason::Recharge => {
                        if let Some(av) = self.active_mut(actor) {
                            av.volatiles.clear(V::MUST_RECHARGE);
                        }
                    }
                    CantReason::Taunt(m) | CantReason::Disable(m) => {
                        if self.chosen(actor, m) {
                            self.reveal_move(actor, m);
                        }
                    }
                    CantReason::Flinch => {}
                }
                self.stop_dance(actor);
            }
            E::StatusCant { actor, status } => {
                self.cant(actor);
                self.cant_action(
                    actor,
                    match status {
                        Status::Paralysis => CantKind::Paralysis,
                        Status::Sleep { .. } => CantKind::Sleep,
                        Status::Freeze { .. } => CantKind::Freeze,
                        _ => CantKind::Other,
                    },
                );
                // One attempt per `cant` line (`slp.onBeforeMove`, and
                // Champions' `frz.onBeforeMove`, which counts its attempts
                // down: `startTime = 3`). A Sleep Talk or Snore turn shows
                // its `cant|slp` too, then runs the move (`sleepUsable`):
                // that move isn't another attempt.
                let kind = StatusKind::of(status);
                if matches!(kind, StatusKind::Sleep | StatusKind::Freeze)
                    && let Some(mon) = self.mon_mut(actor)
                    && mon.status == kind
                {
                    mon.status_turns = mon.status_turns.saturating_add(1);
                }
                self.stop_dance(actor);
            }
            E::LostFocus { actor } => {
                self.cant(actor);
                self.cant_action(actor, CantKind::Other);
            }
            E::CantNoPp { actor, move_id } => {
                self.cant(actor);
                self.cant_action(actor, CantKind::Other);
                if let Some(move_id) = move_id {
                    self.reveal_move(actor, move_id);
                }
            }
            E::GravityCant { actor, move_id } => {
                self.cant(actor);
                self.cant_action(actor, CantKind::Other);
                if self.chosen(actor, move_id) {
                    self.reveal_move(actor, move_id);
                }
                self.stop_dance(actor);
            }
            E::EffectCant { actor, move_id, effect } => {
                self.cant(actor);
                let kind = if effect == VolatileEffect::Attract { CantKind::Attract } else { CantKind::Other };
                self.cant_action(actor, kind);
                if let Some(m) = move_id
                    && self.chosen(actor, m)
                {
                    self.reveal_move(actor, m);
                }
                self.stop_dance(actor);
            }
            E::CantFrom { actor, effect, of, .. } => {
                self.reveal_effect(actor, effect);
                match of {
                    // Damp, Dazzling: the holder stops `[of]`'s move after
                    // its `|move|` line.
                    Some(user) => {
                        if let Some(av) = self.active_mut(user) {
                            av.moved_this_turn = true;
                        }
                        self.user_failed(user, MoveOutcome::Fail);
                    }
                    // Truant: its own move.
                    None => {
                        self.cant(actor);
                        self.cant_action(actor, CantKind::Other);
                        self.stop_dance(actor);
                    }
                }
            }
            E::ChargeStart { actor, .. } => self.set_volatile(actor, V::FOCUSING),
            E::FatigueConfusion { actor } => {
                if let Some(av) = self.active_mut(actor) {
                    av.volatiles.clear(V::LOCKED_MOVE);
                    av.locked_move = None;
                    av.counters.locked_turns = 0;
                    av.volatiles.set(V::CONFUSION);
                    av.counters.confusion_turns = 0;
                    av.counters.confusion_checks = 0;
                }
            }
            E::UproarStart { actor, upkeep } => {
                if !upkeep && let Some(av) = self.active_mut(actor) {
                    av.volatiles.set(V::UPROAR);
                    av.counters.uproar_turns = 0;
                }
            }
            E::UproarEnd { actor } => {
                if let Some(av) = self.active_mut(actor) {
                    av.volatiles.clear(V::UPROAR | V::LOCKED_MOVE);
                    av.locked_move = None;
                    av.counters.locked_turns = 0;
                }
            }
            E::FutureMoveStart { user, move_id } => {
                // Aimed at the target of the user's `|move|` line: its slot
                // (named without one, a Pokémon that fainted: the slot it
                // fainted in), in singles the one slot (an Illusion user
                // that fainted in disguise is named as itself, which no
                // slot showed).
                let singles = self.config.active == 1;
                if let (Some(mover), Some(target)) = (self.ctx.mover, self.ctx.move_target)
                    && same_mon(mover, user)
                    && let slot = if target.active || !singles { target.pos.slot } else { 0 }
                    && (slot as usize) < MAX_ACTIVE
                {
                    self.sides[target.pos.side.index()].future[slot as usize] =
                        FutureView { move_id: Some(move_id), turns: 0 };
                }
            }
            E::FutureMoveEnd { target, .. } => {
                if let Some(f) = self.sides[target.pos.side.index()].future.get_mut(target.pos.slot as usize) {
                    *f = FutureView::default();
                }
            }
            E::ProtectStart { actor, .. } => {
                self.set_volatile(actor, V::PROTECT);
                self.protected(actor);
            }
            E::ProtectionBroken { target, .. } => {
                self.clear_volatile(target, V::PROTECT);
                // Gen 6+: `stall` goes too (`hitStepBreakProtect`).
                if let Some(av) = self.active_mut(target) {
                    av.counters.protect_chain = 0;
                    (av.protected, av.protect_pending) = (false, false);
                }
                // `hitStepBreakProtect` removes the target side's Wide and
                // Quick Guard too, silently.
                let side = &mut self.sides[target.pos.side.index()];
                for c in [SideCondition::WideGuard, SideCondition::QuickGuard] {
                    side.conditions[c as usize] = Default::default();
                }
            }
            E::GuardStart { actor, condition } => {
                let c = &mut self.sides[actor.pos.side.index()].conditions[condition as usize];
                *c = ConditionView { layers: 1, turns: 0 };
                self.protected(actor);
            }
            E::MoveSingleTurn { target, move_id, .. } => match move_id {
                moves::INSTRUCT => {
                    self.ctx.instructed = slot_of(target).map(|(si, slot)| (si as u8, slot as u8));
                }
                moves::HELPING_HAND => self.set_volatile(target, V::HELPING_HAND),
                moves::FOLLOW_ME | moves::RAGE_POWDER => self.set_volatile(target, V::CENTER_OF_ATTENTION),
                _ => {}
            },
            E::SingleTurn { actor, move_id } => match move_id {
                moves::ROOST => self.set_volatile(actor, V::ROOST),
                moves::ELECTRIFY => self.set_volatile(actor, V::ELECTRIFY),
                _ => {}
            },
            // Gravity grounds it: Magnet Rise ends, a Fly or Bounce comes
            // down.
            E::MoveActivate { actor, move_id: moves::GRAVITY, .. } => {
                if let Some(av) = self.active_mut(actor) {
                    av.volatiles.clear(V::MAGNET_RISE);
                    if matches!(av.locked_move, Some(moves::FLY | moves::BOUNCE)) {
                        av.volatiles.clear(V::CHARGING);
                        av.locked_move = None;
                    }
                }
            }
            // A guard blocked the acting move for `actor` (`-activate|<target>|move: Wide Guard`).
            E::MoveActivate { actor, move_id: moves::WIDE_GUARD | moves::QUICK_GUARD, .. } => {
                self.acting_failed(actor, MoveOutcome::Protected);
            }
            // Psychic Terrain stopped a priority move against `target`.
            E::TerrainActivate { target, terrain: Terrain::Psychic } => {
                self.acting_failed(target, MoveOutcome::Protected);
            }
            // `-activate|<user>|move: Trick|[of] <target>` as the channel
            // reads it (the engine's is `ActivateEffect`).
            E::MoveActivate { actor, move_id: moves::TRICK | moves::SWITCHEROO, of: Some(of) } => {
                self.start_swap(actor, of)
            }
            E::MoveActivate { actor, move_id: moves::SPEED_SWAP, of: Some(of) } => {
                // Each one's Speed stat isn't its own until it leaves the
                // field (`ActiveView::speed_swapped`).
                for x in [actor, of] {
                    if let Some((si, slot)) = self.slot_index(x)
                        && let Some(av) = self.sides[si].slots.get_mut(slot)
                    {
                        av.speed_swapped = true;
                    }
                }
            }
            E::MoveActivate { actor, move_id: moves::GUARD_SPLIT | moves::POWER_SPLIT, of: Some(of) } => {
                for x in [actor, of] {
                    if let Some(av) = self.active_mut(x) {
                        av.stats_split = true;
                    }
                }
            }
            E::MoveActivate { actor, move_id, of } => {
                if matches!(move_id, moves::LOCK_ON | moves::MIND_READER)
                    && of.is_some()
                    && let Some(av) = self.active_mut(actor)
                {
                    av.volatiles.set(V::LOCK_ON);
                    av.counters.lock_on_turns = 0;
                }
            }
            E::ActivateMove { target, move_id, about, amount } => match (move_id, about) {
                (moves::SKETCH, Some(m)) => {
                    if let Some(mon) = self.mon_mut(target) {
                        mon.sketched = Some(m);
                    }
                }
                (moves::SPITE, Some(m)) => self.reveal_move_pp(target, m, amount),
                _ => {}
            },
            E::PpDrain { target, move_id, amount } => self.reveal_move_pp(target, move_id, amount),
            E::MimicStart { actor, move_id } => {
                if let Some(av) = self.active_mut(actor) {
                    av.volatiles.set(V::MIMIC);
                    av.mimic_move = Some(move_id);
                }
            }
            E::Swap { actor, slot, .. } => self.ally_switch(actor, slot),

            // HP and status.
            E::Damage { target, hp, max_hp, status, source } => {
                if hp == 0 {
                    // The weather's residual runs an `Update` right after
                    // its damage (`eachEvent('Weather')`).
                    self.dondozo_down(target, matches!(source, DamageSource::Weather(_)));
                }
                let before = self.active(target).map(|a| a.hp);
                self.hp_report(target, hp, max_hp, status);
                let cause = match source {
                    // HP the acting Pokémon pays for its own move
                    // (Substitute, Belly Drum, Curse: `directDamage`).
                    DamageSource::Move if self.acting_is(target) => taken_by::RECOIL,
                    DamageSource::Move => taken_by::MOVE,
                    DamageSource::Status(_) | DamageSource::Weather(_) => taken_by::RESIDUAL,
                    DamageSource::Recoil | DamageSource::MoveRecoil | DamageSource::MoveCondition(_) => {
                        taken_by::RECOIL
                    }
                    DamageSource::Effect(Cause { effect: EffectName::Item(_), .. }) => taken_by::ITEM,
                    DamageSource::Effect(_)
                    | DamageSource::Confusion
                    | DamageSource::MoveEffect { .. }
                    | DamageSource::Species(_) => taken_by::OTHER,
                };
                self.record_hp_loss(target, before, cause);
                if source == DamageSource::Move {
                    self.sticky_barb(target);
                }
                match source {
                    DamageSource::Effect(cause) => self.reveal_cause(target, cause),
                    // Hurt in its confusion: its move is aborted.
                    DamageSource::Confusion => {
                        self.cant(target);
                        self.cant_action(target, CantKind::Confusion);
                        self.stop_dance(target);
                    }
                    DamageSource::Status(Status::Toxic { .. }) => self.toxic_tick(target),
                    _ => {}
                }
            }
            E::Heal { target, hp, max_hp, status, source } => {
                self.hp_report(target, hp, max_hp, status);
                if let HealSource::Effect(cause) = source {
                    // `[of]` names the attacker (Water Absorb): the effect
                    // is the healed Pokémon's; but Hospitality heals an ally
                    // and names its holder.
                    let holder = match (cause.effect, cause.of) {
                        (EffectName::Ability(abilities::HOSPITALITY), Some(of)) => of,
                        _ => target,
                    };
                    self.reveal_effect(holder, cause.effect);
                    // Poison Heal heals instead of toxic's damage: the
                    // counter went up all the same.
                    if cause.effect == EffectName::Ability(abilities::POISON_HEAL) {
                        self.toxic_tick(target);
                    }
                }
            }
            E::HealFromSilent { target, hp, max_hp, status, cause } => {
                self.hp_report(target, hp, max_hp, status);
                self.reveal_effect(target, cause.effect);
            }
            E::EffectDamage { target, hp, max_hp, status, .. }
            | E::ConditionDamage { target, hp, max_hp, status, .. } => {
                let before = self.active(target).map(|a| a.hp);
                self.hp_report(target, hp, max_hp, status);
                self.record_hp_loss(target, before, taken_by::RESIDUAL);
            }
            E::SetHp { target, hp, max_hp, status, .. } => {
                let before = self.active(target).map(|a| a.hp);
                // Pain Split: the user's own share is what its move cost it.
                let cause = if self.acting_is(target) { taken_by::RECOIL } else { taken_by::MOVE };
                self.hp_report(target, hp, max_hp, status);
                self.record_hp_loss(target, before, cause);
            }
            E::WishHeal { target, hp, max_hp, status, .. } => {
                if let Some((si, slot)) = slot_of(target) {
                    self.sides[si].slot_conditions[slot].wish = None;
                }
                self.hp_report(target, hp, max_hp, status);
            }
            E::EffectHeal { target, hp, max_hp, status, .. }
            | E::ManipulationHeal { target, hp, max_hp, status, .. }
            | E::HealSilent { target, hp, max_hp, status } => self.hp_report(target, hp, max_hp, status),
            E::MoveHeal { target, hp, max_hp, status, move_id } => {
                match move_id {
                    moves::REVIVAL_BLESSING => self.revive(target),
                    // A healing wish came true: it ends.
                    moves::HEALING_WISH | moves::LUNAR_DANCE => {
                        if let Some((si, slot)) = slot_of(target) {
                            let c = &mut self.sides[si].slot_conditions[slot];
                            if move_id == moves::HEALING_WISH {
                                c.healing_wish = false;
                            } else {
                                c.lunar_dance = false;
                            }
                        }
                        // Lunar Dance restores every move's PP too
                        // (`lunardance.onSwap`: `moveSlot.pp = moveSlot.maxpp`).
                        if move_id == moves::LUNAR_DANCE
                            && let Some(mon) = self.mon_mut(target)
                        {
                            for m in mon.moves.iter_mut() {
                                m.pp_spent = 0;
                            }
                        }
                    }
                    _ => {}
                }
                self.hp_report(target, hp, max_hp, status);
            }
            E::StatusStart { target, status, from_move } => {
                self.set_status(target, StatusKind::of(status));
                if let Some(mon) = self.mon_mut(target) {
                    let rest = mon.status == StatusKind::Sleep && from_move == Some(moves::REST);
                    mon.flags.set(MonFlags::SLEEP_FROM_REST, rest);
                }
            }
            E::StatusFrom { target, status, cause } => {
                self.set_status(target, StatusKind::of(status));
                self.reveal_cause(target, cause);
            }
            E::CureStatus { target, .. } => self.set_status(target, StatusKind::None),
            E::CureStatusFrom { target, cause, .. } => {
                self.set_status(target, StatusKind::None);
                self.reveal_cause(target, cause);
            }
            E::Faint { actor } => self.faint(actor),

            // Stat stages.
            E::Boost { target, stat, amount } => self.boost(target, stat, stages(amount)),
            E::Unboost { target, stat, amount } => self.boost(target, stat, -stages(amount)),
            E::BoostFrom { target, stat, amount, unboost, cause } => {
                self.boost(target, stat, if unboost { -stages(amount) } else { stages(amount) });
                self.reveal_cause(target, cause);
            }
            E::SetBoost { target, stat, stage, .. } => self.set_boost(target, stat, stage),
            E::SetBoostFrom { target, stat, stage, cause } => {
                self.set_boost(target, stat, stage);
                self.reveal_cause(target, cause);
            }
            E::ClearBoost { target } => self.map_boosts(target, |_| 0),
            E::ClearBoostFrom { target, cause } => {
                self.map_boosts(target, |_| 0);
                self.reveal_cause(target, cause);
            }
            E::ClearNegativeBoost { target, .. } => self.map_boosts(target, |b| b.max(0)),
            E::ClearPositiveBoost { target, .. } => self.map_boosts(target, |b| b.min(0)),
            E::InvertBoost { target } => self.map_boosts(target, |b| -b),
            E::ClearAllBoost => {
                for side in &mut self.sides {
                    for slot in &mut side.slots {
                        slot.boosts = [0; 7];
                    }
                }
            }
            E::CopyBoost { source, target } => self.copy_boosts(source, target),
            E::CopyBoostFrom { source, target, cause } => {
                self.copy_boosts(source, target);
                self.reveal_cause(source, Cause { of: None, ..cause });
            }
            E::SwapBoost { source, target, move_id } => {
                let stats: &[BoostId] = match move_id {
                    moves::GUARD_SWAP => &[BoostId::Def, BoostId::SpD],
                    moves::POWER_SWAP => &[BoostId::Atk, BoostId::SpA],
                    _ => &BoostId::ALL,
                };
                if let (Some((ss, sl)), Some((ts, tl))) = (slot_of(source), slot_of(target)) {
                    for &stat in stats {
                        let i = stat as usize;
                        let a = self.sides[ss].slots[sl].boosts[i];
                        let b = self.sides[ts].slots[tl].boosts[i];
                        self.sides[ss].slots[sl].boosts[i] = b;
                        self.sides[ts].slots[tl].boosts[i] = a;
                    }
                }
            }

            // Volatiles.
            E::VolatileStart { actor, volatile, move_id } => {
                let last = self.active_mut(actor).and_then(|av| av.last_move);
                let will_move = self.will_move(actor);
                let moving = self.moving_is(actor);
                if let Some(av) = self.active_mut(actor) {
                    match volatile {
                        Volatile::Confusion => {
                            av.volatiles.set(V::CONFUSION);
                            av.counters.confusion_turns = 0;
                            av.counters.confusion_checks = 0;
                        }
                        Volatile::Taunt => {
                            av.volatiles.set(V::TAUNT);
                            av.counters.taunt_turns = 0;
                            av.counters.taunt_long = av.turns_active > 0 && !will_move;
                        }
                        Volatile::Encore => {
                            av.volatiles.set(V::ENCORE);
                            av.counters.encore_turns = 0;
                            av.counters.encore_long = !will_move;
                            av.encore_move = move_id.or(last);
                        }
                        Volatile::Disable => {
                            av.volatiles.set(V::DISABLE);
                            av.counters.disable_turns = 0;
                            av.counters.disable_short = will_move || moving;
                            av.disabled_move = move_id;
                        }
                    }
                }
            }
            E::VolatileEnd { actor, volatile } => {
                if let Some(av) = self.active_mut(actor) {
                    match volatile {
                        Volatile::Confusion => av.volatiles.clear(V::CONFUSION),
                        Volatile::Taunt => av.volatiles.clear(V::TAUNT),
                        Volatile::Encore => {
                            av.volatiles.clear(V::ENCORE);
                            av.encore_move = None;
                        }
                        Volatile::Disable => {
                            av.volatiles.clear(V::DISABLE);
                            av.disabled_move = None;
                        }
                    }
                }
            }
            // Confusion's check before a move (`confusion.onBeforeMove`:
            // `time--`, then `-activate` unless that ended it): what ends
            // confusion is its checks (2 to 5), not turns.
            E::VolatileActivate { actor, volatile: Volatile::Confusion } => {
                if let Some(av) = self.active_mut(actor)
                    && av.volatiles.has(V::CONFUSION)
                {
                    av.counters.confusion_checks = av.counters.confusion_checks.saturating_add(1);
                }
            }
            E::VolatileActivate { .. } => {}
            E::EffectStart { actor, effect, of, .. } => self.effect_start(actor, effect, of),
            E::EffectEnd { actor, effect, .. } | E::EffectEndFrom { actor, effect, .. } => {
                self.effect_end(actor, effect)
            }
            E::EffectActivate { .. } => {}
            E::PerishCount { actor, count, .. } => {
                if let Some(av) = self.active_mut(actor) {
                    av.volatiles.set(V::PERISH_SONG);
                    av.counters.perish = count;
                }
            }
            E::ManipulationStart { actor, volatile, .. } => {
                if let Some(av) = self.active_mut(actor) {
                    if let ManipulationVolatile::Stockpile(n) = volatile {
                        av.counters.stockpile = n;
                    }
                    av.volatiles.set(manipulation_bit(volatile));
                }
            }
            E::ManipulationEnd { actor, volatile, .. } => {
                if let Some(av) = self.active_mut(actor) {
                    if let ManipulationVolatile::Stockpile(_) = volatile {
                        av.counters.stockpile = 0;
                    }
                    av.volatiles.clear(manipulation_bit(volatile));
                }
            }
            E::StartFrom { actor, what, arg, cause } => {
                match (what, arg) {
                    ("Attract", _) => self.set_volatile(actor, V::ATTRACT),
                    ("confusion", _) => {
                        if let Some(av) = self.active_mut(actor) {
                            av.volatiles.set(V::CONFUSION);
                            av.counters.confusion_turns = 0;
                            av.counters.confusion_checks = 0;
                        }
                    }
                    ("Disable", Some(LineArg::Move(m))) => {
                        // Cursed Body disables the move its holder's
                        // attacker is using now: one turn short.
                        let short = self.will_move(actor) || self.moving_is(actor);
                        if let Some(av) = self.active_mut(actor) {
                            av.volatiles.set(V::DISABLE);
                            av.counters.disable_turns = 0;
                            av.counters.disable_short = short;
                            av.disabled_move = Some(m);
                        }
                    }
                    ("Charge", _) => self.set_volatile(actor, V::CHARGE),
                    ("typechange", Some(LineArg::Type(t))) => self.change_types(actor, TypeSet::mono(t)),
                    _ => {}
                }
                self.reveal_cause(actor, cause);
            }
            E::EffectStartEnd { actor, start, what, effect, cause, .. } => {
                self.effect_start_end(actor, start, what, effect);
                if let Some(c) = cause {
                    self.reveal_cause(actor, c);
                }
            }
            E::AbilityEnd { actor } => self.set_volatile(actor, V::ABILITY_SUPPRESSED),

            // Types.
            E::TypeChange { target, types, silent } => {
                if silent {
                    self.set_types(target, types);
                } else {
                    self.change_types(target, types);
                }
                self.note_silent_types(target, silent);
            }
            E::TypeChangeSilent { actor, types } => {
                self.set_types(actor, types);
                self.note_silent_types(actor, true);
            }
            E::TypeChangeFrom { target, types, cause } => {
                self.change_types(target, types);
                self.reveal_cause(target, cause);
            }
            E::TypeCopy { target, of } => {
                let copied = slot_of(of).and_then(|(s, l)| {
                    let side = &self.sides[s];
                    let (_, mon) = side.active(l)?;
                    Some((self.types_of(mon, &side.slots[l]), side.slots[l].added_type))
                });
                if let Some((types, added)) = copied {
                    // Reflect Type: `target.getTypes(true).filter(type =>
                    // type !== '???')`, Normal if only an added type is
                    // left (with nothing at all it fails, showing no line):
                    // a Rotom's `???/Ghost` after Double Shock.
                    let mut kept =
                        [Some(types.primary), types.secondary].into_iter().flatten().filter(|&t| t != Type::Unknown);
                    let types = match (kept.next(), kept.next()) {
                        (Some(a), b) => TypeSet { primary: a, secondary: b },
                        (None, _) => TypeSet::mono(Type::Normal),
                    };
                    self.set_types(target, types);
                    if let Some(av) = self.active_mut(target) {
                        av.added_type = added;
                        av.volatiles.set(if added.is_some() { V::TYPE_ADD } else { 0 });
                    }
                }
            }
            E::TypeAdd { target, ty, .. } => {
                if let Some(av) = self.active_mut(target) {
                    av.added_type = Some(ty);
                    av.volatiles.set(V::TYPE_ADD);
                }
            }

            // Abilities.
            E::AbilityShow { actor, ability, note, cause } => self.ability_show(actor, ability, note, cause),
            E::AbilityNamed { actor, .. } => self.ctx.as_one = Some(actor),
            E::AbilityReplaced { holder, ability, target, old } => {
                self.reveal_ability(holder, ability);
                self.change_ability(target, Some(old), AbilityState::Known(ability));
            }
            E::SkillSwap { source, target, abilities } => self.skill_swap(source, target, abilities),
            E::SkillSwapActivate { source, abilities, of } => self.skill_swap(source, of, abilities),
            E::ActivateSource { actor, ability } => self.reveal_ability(actor, ability),
            E::ActivateEffect { actor, effect, args, of } => {
                if effect == EffectName::Ability(abilities::DANCER) {
                    self.ctx.dancing = Some(actor);
                }
                if effect == EffectName::Ability(abilities::CUD_CHEW) {
                    self.ctx.cud_chew = Some(actor);
                }
                if matches!(effect, EffectName::Ability(abilities::EMERGENCY_EXIT | abilities::WIMP_OUT)) {
                    self.exiting(actor);
                }
                // Storm Drain or Lightning Rod drew the move (it shows only
                // when it changed the target): a charged move's release
                // named it, not the charge's target.
                if matches!(effect, EffectName::Ability(abilities::STORM_DRAIN | abilities::LIGHTNING_ROD))
                    && let Some((side, id, m)) = self.ctx.released.take()
                    && let Some(mover) = self.ctx.mover.filter(|u| u.pos.side.index() == side as usize && u.id == id)
                    && let Some(mon) = self.mon_mut(mover)
                    && let Some(r) = mon.moves.iter_mut().find(|r| r.id == m)
                {
                    r.pp_spent = r.pp_spent.saturating_sub(1);
                }
                self.activate_effect(actor, effect, args, of)
            }
            // Aroma Veil, Sweet Veil, Flower Veil protect an ally and name
            // their holder with `[of]`.
            E::Block { actor, effect, of, .. } => {
                self.reveal_effect(of.unwrap_or(actor), effect);
                if effect == EffectName::Item(items::ABILITY_SHIELD)
                    && let Some(a) = self.ctx.shielded.take()
                {
                    self.reveal_ability(actor, a);
                }
            }
            E::ImmuneFrom { target, cause, .. } => {
                self.reveal_cause(target, cause);
                self.announce_effect(target, IMMUNE);
                self.acting_failed(target, MoveOutcome::Immune);
            }
            E::FailFrom { target, cause, .. } | E::FailBy { target, cause } => self.reveal_cause(target, cause),
            E::WeatherFail { source, effect, .. } => {
                if let EffectName::Ability(a) = effect {
                    self.reveal_ability(source, a);
                }
            }

            // Items.
            E::ItemShow { actor, item, cause } => self.item_show(actor, item, cause),
            // The Micle Berry's `-enditem` without `[eat]` is its boost
            // used up, after the berry was eaten (maybe a flung one): not
            // the held item.
            E::ItemEnd { item: items::MICLE_BERRY, note: ItemEndNote::None, from: None, .. } => {}
            // Cud Chew eats again the berry eaten last turn (one a Fling
            // fed it, too), not what it holds.
            E::ItemEnd { actor, item, note: ItemEndNote::Eat, .. }
                if self.ctx.cud_chew.take().is_some_and(|c| same_mon(c, actor)) =>
            {
                // Nor is the berry its effect names (championsvgc
                // 4,543,710: a Choice Scarf Trick gave it).
                self.ctx.chewed = Some((actor, item));
            }
            // A resist berry's `[weaken]` line comes after its `[eat]`, and
            // after Symbiosis may have passed the holder a new item: it
            // says nothing new.
            E::ItemEnd { actor, item, note: ItemEndNote::Weaken, .. }
                if self
                    .mon(actor)
                    .is_some_and(|m| m.item != ItemState::Known(item) && m.item != ItemState::Unknown) => {}
            E::ItemEnd { actor, item, note, from, .. } => {
                if matches!(item, items::EJECT_BUTTON | items::EJECT_PACK) {
                    self.exiting(actor);
                }
                let how = match (note, from) {
                    (ItemEndNote::Eat | ItemEndNote::Weaken, _) => ItemLoss::Eaten,
                    (ItemEndNote::StealEat(_), _) => ItemLoss::Stolen,
                    (_, Some(EffectName::Move(moves::KNOCK_OFF))) => ItemLoss::KnockedOff,
                    (_, Some(EffectName::Move(moves::THIEF | moves::COVET))) => ItemLoss::Stolen,
                    (_, Some(EffectName::Move(moves::TRICK | moves::SWITCHEROO))) => ItemLoss::Traded,
                    (_, Some(EffectName::Move(moves::INCINERATE))) => ItemLoss::Incinerated,
                    (_, Some(EffectName::Move(moves::FLING))) => ItemLoss::Flung,
                    (_, Some(EffectName::Ability(abilities::MAGICIAN | abilities::PICKPOCKET))) => ItemLoss::Stolen,
                    (_, Some(_)) => ItemLoss::Other,
                    (ItemEndNote::Gem | ItemEndNote::GemMove(_) | ItemEndNote::None | ItemEndNote::Silent, None) => {
                        ItemLoss::Consumed
                    }
                };
                if how == ItemLoss::Traded {
                    // Trick's `-enditem`: it held `item` before the trade.
                    self.swap_before(actor, item);
                }
                if let Some(mon) = self.mon_mut(actor) {
                    mon.item = ItemState::Gone { item: Some(item), how };
                }
                // Only an item used up can be announced as received after
                // it (`item_show`); a transfer's `-enditem` is not that. The
                // victim's `-enditem` of Thief comes between the thief's
                // using up what it stole and its `-item` (Showdown's
                // `setItem` runs the item's `onStart` first): it leaves the
                // thief's note alone (a White Herb).
                if matches!(how, ItemLoss::Consumed | ItemLoss::Eaten) {
                    self.ctx.item_ended = Some((actor, item));
                } else if self.ctx.item_ended.is_some_and(|(a, _)| same_mon(a, actor)) {
                    self.ctx.item_ended = None;
                }
            }
            // Fling's cleanup after its item was used up already (a flung
            // Blunder Policy that missed): the item's own `-enditem` said
            // it was gone.
            E::FlingNoItem { .. } => {}

            // Field and sides.
            E::Weather { weather, upkeep } => {
                if upkeep {
                    if self.field.weather.is_some() {
                        self.field.weather_turns = self.field.weather_turns.saturating_add(1);
                    }
                } else {
                    self.field.weather = weather;
                    self.field.weather_turns = 0;
                    self.field.weather_setter = weather.and(self.acting_row());
                    self.field.weather_setter_lost = self.item_lost(self.field.weather_setter);
                    self.field.weather_setter_item = self.item_then(self.field.weather_setter);
                }
            }
            E::WeatherFrom { weather, cause } => {
                self.field.weather = Some(weather);
                self.field.weather_turns = 0;
                self.field.weather_setter = cause.of.and_then(|of| self.actor_row(&of));
                self.field.weather_setter_lost = self.item_lost(self.field.weather_setter);
                self.field.weather_setter_item = self.item_then(self.field.weather_setter);
                if let Some(of) = cause.of {
                    self.reveal_effect(of, cause.effect);
                }
            }
            E::FieldStart { effect, .. } => {
                let setter = self.acting_row();
                self.field_start(effect, setter);
            }
            E::FieldStartFrom { effect, cause } => {
                let setter = cause.of.and_then(|of| self.actor_row(&of));
                self.field_start(effect, setter);
                if let Some(of) = cause.of {
                    self.reveal_effect(of, cause.effect);
                }
            }
            E::FieldEnd { effect } => self.field_end(effect),
            E::MagicRoomEnd { .. } => self.field.magic_room = None,
            E::FieldActivate { effect } => {
                if effect == VolatileEffect::FairyLock {
                    self.field.fairy_lock = true;
                    self.field.fairy_lock_turns = 0;
                }
            }
            E::SideStart { side, condition } => {
                let max = match condition {
                    SideCondition::Spikes => 3,
                    SideCondition::ToxicSpikes => 2,
                    _ => 1,
                };
                let setter = self.acting_row().filter(|&(s, _)| s == side).map(|(_, row)| row);
                let lost = self.item_lost(setter.map(|row| (side, row)));
                let then = self.item_then(setter.map(|row| (side, row)));
                let s = &mut self.sides[side.index()];
                let c = &mut s.conditions[condition as usize];
                if c.layers == 0 {
                    c.turns = 0;
                    if let Some(i) = crate::side::SCREENS.iter().position(|&x| x == condition) {
                        s.screen_setters[i] = setter;
                        s.screen_setter_lost[i] = lost;
                        s.screen_setter_items[i] = then;
                    }
                }
                c.layers = (c.layers + 1).min(max);
            }
            E::SideEnd { side, condition } | E::HazardEnd { side, condition, .. } => {
                let s = &mut self.sides[side.index()];
                s.conditions[condition as usize] = ConditionView::default();
                if let Some(i) = crate::side::SCREENS.iter().position(|&x| x == condition) {
                    s.screen_setters[i] = None;
                }
            }
            E::ToxicSpikesAbsorbed { side, .. } => {
                self.sides[side.index()].conditions[SideCondition::ToxicSpikes as usize] = ConditionView::default();
            }
            E::SwapSideConditions => {
                // The setters' rows are on the side they came from.
                for s in &mut self.sides {
                    s.screen_setters = [None; 3];
                }
                let [a, b] = [self.sides[0].conditions, self.sides[1].conditions];
                for c in SideCondition::ALL {
                    if !matches!(c, SideCondition::WideGuard | SideCondition::QuickGuard) {
                        self.sides[0].conditions[c as usize] = b[c as usize];
                        self.sides[1].conditions[c as usize] = a[c as usize];
                    }
                }
            }

            // The acting move's outcome (the turn records).
            E::Crit { target } => {
                if let Some(r) = self.acting_record(target) {
                    r.crit = true;
                }
            }
            E::SuperEffective { target, level } | E::Resisted { target, level } => {
                // Scarlet/Violet doesn't show the level.
                let level = if self.config.mechanics == pikapro::Mechanics::Champions { level.max(1) } else { 1 };
                let sign = if matches!(event, E::SuperEffective { .. }) { 1 } else { -1 };
                self.announce_effect(target, sign * level.min(2) as i8);
            }
            E::Immune { target } | E::OhkoImmune { target } => {
                self.announce_effect(target, IMMUNE);
                self.acting_failed(target, MoveOutcome::Immune);
            }
            E::HitCount { target, hits } => {
                if let Some(r) = self.acting_record(target) {
                    r.hits = r.hits.max(hits);
                }
            }
            E::Miss { user, .. } => self.user_failed(user, MoveOutcome::Miss),
            E::Protected { target } => self.acting_failed(target, MoveOutcome::Protected),
            E::Fail { user } | E::NoTarget { user } | E::MoveFail { user, .. } | E::WeatherMoveFail { user, .. } => {
                // A healing wish that failed isn't made.
                if let Some((maker, before)) = self.ctx.slot_made.take()
                    && same_mon(maker, user)
                    && let Some((si, slot)) = slot_of(user)
                {
                    self.sides[si].slot_conditions[slot] = before;
                }
                // A protection that failed its `stall` roll ends the chain at
                // once (`onStallMove` removes the volatile); so does a
                // second guard that `willAct` stopped (`guard_again`).
                // Or `willAct` failed it (nothing acts after it this turn:
                // an Instructed Protect after one that worked), which leaves
                // `stall` alone: decided at the next action or the
                // residuals (`ApplyCtx::stall`).
                let again = self.ctx.guard_again.take().is_some_and(|g| same_mon(g, user));
                let at = slot_of(user).map(|(si, slot)| (si as u8, slot as u8));
                let before = self.ctx.stall.take().filter(|b| !b.4 && Some((b.0, b.1)) == at);
                if let Some(av) = self.active_mut(user)
                    && (av.protect_pending || again)
                    && let Some((si, slot)) = at
                {
                    let (chain, protected) = before.map_or((av.counters.protect_chain, av.protected), |b| (b.2, b.3));
                    av.protect_pending = false;
                    av.protected = false;
                    av.counters.protect_chain = 0;
                    self.ctx.stall = Some((si, slot, chain, protected, true));
                }
                self.user_failed(user, MoveOutcome::Fail);
            }
            E::HealFail { target } | E::StatusFail { target, .. } => self.acting_failed(target, MoveOutcome::Fail),
            // Nothing a client keeps.
            E::Ohko
            | E::StruggleStart { .. }
            | E::TerrainActivate { .. }
            | E::UproarPreventsSleep { .. }
            | E::PledgeWaiting { .. }
            | E::PledgeCombine
            | E::Nothing
            | E::WeatherActivate { .. }
            | E::ZBroken { .. }
            | E::MoveFieldActivate { .. }
            | E::Player { .. }
            | E::Message { .. }
            | E::Rng { .. } => {}
            // A physical Shell Side Arm makes contact (its `onModifyMove`).
            E::MoveAttr { attr: MoveAttr::Anim(MoveAnim::ShellSideArm(category)) } => {
                self.ctx.contact = Some(category == pikapro_data::MoveCategory::Physical);
            }
            // `[still]` blanks the line's target.
            E::MoveAttr { attr: MoveAttr::Still } => {
                if let Some((_, _, t)) = &mut self.ctx.acting_target {
                    *t = TurnTarget::None;
                }
            }
            // `[notarget]`: the move had no target (`useMoveInner` returns
            // before Pressure, or `getMoveTargets` found none), so
            // Pressure took no PP.
            E::MoveAttr { attr: MoveAttr::NoTarget } => {
                self.ctx.pressure = None;
                if let Some((side, id, m, extra)) = self.ctx.pressure_taken.take()
                    && let Some(mover) = self.ctx.mover.filter(|u| u.pos.side.index() == side as usize && u.id == id)
                    && let Some(mon) = self.mon_mut(mover)
                    && let Some(r) = mon.moves.iter_mut().find(|r| r.id == m)
                {
                    r.pp_spent = r.pp_spent.saturating_sub(extra);
                }
            }
            // `[spread] <slots>`: the move hit several targets, whatever its
            // dex target (Expanding Force in Psychic Terrain). Only foes
            // among them make it a spread move: Howl and Jungle Healing
            // show `[spread]` for the user and its ally.
            E::MoveAttr { attr: MoveAttr::Spread(slots) } => {
                self.spread_pressure();
                if let Some((si, _, t)) = &mut self.ctx.acting_target
                    && slots.slots.iter().take(slots.len as usize).any(|p| p.side.index() != *si as usize)
                {
                    *t = TurnTarget::Spread;
                }
            }
            E::MoveAttr { .. } => {}
            // `|t:|` after `|`: a turn, or a turn resumed after a decision.
            E::Timestamp => {
                self.ctx.residual = false;
                self.ctx.uncommanded = [0; 2];
            }
        }
    }

    /// Applies every event in order.
    pub fn apply_all<'a>(&mut self, events: impl IntoIterator<Item = &'a BattleEvent>) {
        for e in events {
            self.apply(e);
        }
    }

    // -----------------------------------------------------------------
    // Lookups.

    /// The HP a report shows under this tracker's [`HpReport`].
    pub(crate) fn shown(&self, hp: u16, max_hp: u16) -> HpView {
        match self.config.hp {
            HpReport::Exact => HpView { hp, max: max_hp, hint: HpHint::None },
            HpReport::Percent => {
                let (pct, hint) = hp_shown(self.config.mechanics, hp, max_hp);
                HpView { hp: pct as u16, max: 100, hint }
            }
        }
    }

    /// The row events name `actor` with, created on first sight.
    fn row_for(&mut self, actor: Actor) -> Option<u8> {
        let s = &mut self.sides[actor.pos.side.index()];
        if let Some(row) = s.row_of(actor.id) {
            return Some(row);
        }
        if s.mons.is_full() || actor.id as usize >= MAX_TEAM {
            return None;
        }
        let row = s.mons.len() as u8;
        s.mons.push(MonView::default());
        s.row_of_id[actor.id as usize] = row;
        Some(row)
    }

    /// The record events name `actor` with, to change it (see
    /// [`ApplyCtx::touch`](crate::tracker::ApplyCtx::touch): every change
    /// to a record is counted).
    pub(crate) fn mon_mut(&mut self, actor: Actor) -> Option<&mut MonView> {
        let s = &mut self.sides[actor.pos.side.index()];
        let row = s.row_of(actor.id)?;
        let mon = s.mons.get_mut(row as usize)?;
        Some(mon)
    }

    fn mon(&self, actor: Actor) -> Option<&MonView> {
        let s = &self.sides[actor.pos.side.index()];
        s.mons.get(s.row_of(actor.id)? as usize)
    }

    /// The slot of an actor on the field whose slot shows it.
    fn active_mut(&mut self, actor: Actor) -> Option<&mut ActiveView> {
        let (s, slot) = slot_of(actor)?;
        let side = &mut self.sides[s];
        let row = side.row_of(actor.id)?;
        let av = &mut side.slots[slot];
        (av.row == Some(row)).then_some(av)
    }

    fn active(&self, actor: Actor) -> Option<&ActiveView> {
        let (s, slot) = slot_of(actor)?;
        let side = &self.sides[s];
        let row = side.row_of(actor.id)?;
        let av = &side.slots[slot];
        (av.row == Some(row)).then_some(av)
    }

    fn set_volatile(&mut self, actor: Actor, bit: u64) {
        if let Some(av) = self.active_mut(actor) {
            av.volatiles.set(bit);
        }
    }

    fn clear_volatile(&mut self, actor: Actor, bit: u64) {
        if let Some(av) = self.active_mut(actor) {
            av.volatiles.clear(bit);
        }
    }

    // -----------------------------------------------------------------
    // Setup and turns.

    /// `|poke|`: the next row of `side`, whose events will name it with
    /// the id equal to its row (the registration index).
    fn preview(&mut self, side: SideId, species: SpeciesId, level: u8, gender: Gender) {
        let (dex, mechanics) = (self.config.dex, self.config.mechanics);
        let s = &mut self.sides[side.index()];
        if s.mons.is_full() {
            return;
        }
        let row = s.mons.len() as u8;
        let wild = preview_wildcard(dex, mechanics, species);
        let mut flags = MonFlags(MonFlags::PREVIEWED);
        flags.set(MonFlags::FORME_HIDDEN, wild.is_some());
        let shown = wild.unwrap_or(species);
        let mut mon = MonView { species: shown, details_species: shown, level, gender, flags, ..MonView::default() };
        if wild.is_none()
            && let Some(a) = single_ability(dex, shown)
        {
            mon.ability = AbilityState::Known(a);
            mon.base_ability = AbilityState::Known(a);
        }
        if let Some(entry) = self.ots[side.index()].and_then(|sheet| sheet.mons.get(row as usize).copied()) {
            let started = matches!(self.phase, Phase::Battle | Phase::Over);
            fill_from_sheet(dex, &mut mon, &entry, started, true);
        }
        s.mons.push(mon);
        s.row_of_id[row as usize] = row;
    }

    /// A protection failed and nothing acted after it: `willAct` failed
    /// it, before any roll (Protect's `onPrepareHit`, Wide Guard's
    /// `onTry`), and Showdown's `stall` (`duration: 2`) is as it was: the
    /// chain goes on, and survives the residuals if a protection of the
    /// same Pokémon worked this turn.
    #[inline]
    fn stall_kept(&mut self) {
        if let Some((si, slot, chain, protected, true)) = self.ctx.stall {
            self.ctx.stall = None;
            let av = &mut self.sides[si as usize].slots[slot as usize];
            av.counters.protect_chain = chain;
            av.protected = protected;
        }
    }

    fn new_turn(&mut self, turn: u16) {
        self.ctx.stall = None;
        self.ctx.residual = false;
        self.ctx.uncommanded = [0; 2];
        self.turn = turn;
        // The turn records: this turn's becomes the last.
        self.commit_target();
        self.ctx.actions = 0;
        self.ctx.moves_begun = false;
        self.ctx.upkept = false;
        self.ctx.exiting = [[false; MAX_ACTIVE]; 2];
        self.ctx.acting = None;
        self.ctx.moving = None;
        self.ctx.instructed = None;
        for side in &mut self.sides {
            side.last_turn = side.this_turn;
            side.this_turn = [TurnRecord::default(); MAX_ACTIVE];
        }
        for side in &mut self.sides {
            for slot in &mut side.slots {
                if slot.row.is_none() {
                    continue;
                }
                slot.turns_active = slot.turns_active.saturating_add(1);
                slot.moved_this_turn = false;
                slot.volatiles.clear(THIS_TURN);
                slot.protected = false;
                slot.protect_pending = false;
            }
        }
    }

    /// `|upkeep`: the end of the turn's residuals. Durations count them;
    /// what lasts a turn is over (Roost, Protect, Helping Hand: `duration:
    /// 1`).
    fn upkeep(&mut self) {
        // Slow Start counts down in its holder's residuals (`onResidual`),
        // which a suppressed ability skips.
        let gas = self.sides.iter().any(|s| s.slots.iter().any(|av| av.volatiles.has(V::SLOW_START)))
            && self.neutralizing_gas();
        self.stall_kept();
        self.ctx.chewed = None;
        self.ctx.upkept = true;
        self.commit_target();
        self.ctx.acting = None;
        self.ctx.moving = None;
        for side in &mut self.sides {
            for av in &mut side.slots {
                av.volatiles.clear(THIS_TURN);
                // Showdown's `stall` (`duration: 2`) ends in the residuals
                // of a turn without a protection that worked.
                if !av.protected {
                    av.counters.protect_chain = 0;
                }
                av.protected = false;
                av.protect_pending = false;
            }
        }
        let f = &mut self.field;
        if f.terrain.is_some() {
            f.terrain_turns = f.terrain_turns.saturating_add(1);
        }
        for t in [&mut f.trick_room, &mut f.gravity, &mut f.magic_room, &mut f.wonder_room].into_iter().flatten() {
            *t = t.saturating_add(1);
        }
        if f.fairy_lock {
            f.fairy_lock_turns = f.fairy_lock_turns.saturating_add(1);
            if f.fairy_lock_turns >= 2 {
                f.fairy_lock = false;
                f.fairy_lock_turns = 0;
            }
        }
        for (side, residuals) in self.sides.iter_mut().zip(&mut self.ctx.lock_residuals) {
            for (av, n) in side.slots.iter_mut().zip(residuals) {
                // The volatiles' durations count the residuals, as the
                // engine's do (`duration`, counted down in `fieldEvent('Residual')`),
                // so that between them and the next `|turn|` (a
                // replacement for a Pokémon that fainted in them) they
                // agree with the field's and the sides'.
                if av.row.is_some() && av.volatiles.has(TIMED) {
                    let v = av.volatiles;
                    let c = &mut av.counters;
                    let tick = |on: bool, n: &mut u8| {
                        if on {
                            *n = n.saturating_add(1);
                        }
                    };
                    tick(v.has(V::CONFUSION), &mut c.confusion_turns);
                    tick(v.has(V::TAUNT), &mut c.taunt_turns);
                    tick(v.has(V::ENCORE), &mut c.encore_turns);
                    tick(v.has(V::DISABLE), &mut c.disable_turns);
                    tick(v.has(V::YAWN), &mut c.yawn_turns);
                    tick(v.has(V::HEAL_BLOCK), &mut c.heal_block_turns);
                    tick(v.has(V::THROAT_CHOP), &mut c.throat_chop_turns);
                    tick(v.has(V::MAGNET_RISE), &mut c.magnet_rise_turns);
                    tick(v.has(V::PARTIALLY_TRAPPED), &mut c.partial_trap_turns);
                    tick(v.has(V::UPROAR), &mut c.uproar_turns);
                    let suppressed = gas || v.has(V::ABILITY_SUPPRESSED);
                    tick(v.has(V::SLOW_START) && !suppressed, &mut c.slow_start_turns);
                    tick(v.has(V::SYRUP_BOMB), &mut c.syrup_bomb_turns);
                }
                // A charge or a recharge whose turn was skipped.
                if av.volatiles.has(V::CHARGING | V::MUST_RECHARGE) {
                    *n = n.saturating_add(1);
                    if *n >= 2 {
                        if av.volatiles.has(V::CHARGING) {
                            av.locked_move = None;
                        }
                        av.volatiles.clear(V::CHARGING | V::MUST_RECHARGE);
                    }
                }
                // Lock-On lasts two turns' residuals (`duration: 2`).
                if av.volatiles.has(V::LOCK_ON) {
                    av.counters.lock_on_turns = av.counters.lock_on_turns.saturating_add(1);
                    if av.counters.lock_on_turns >= 2 {
                        av.volatiles.clear(V::LOCK_ON);
                    }
                }
            }
            // (`conditions` is indexed by every `SideCondition`: one pass
            // over the array, then the guards.)
            for view in &mut side.conditions {
                view.turns = view.turns.saturating_add(u8::from(view.layers > 0));
            }
            // A single turn's.
            side.conditions[SideCondition::WideGuard as usize] = ConditionView::default();
            side.conditions[SideCondition::QuickGuard as usize] = ConditionView::default();
            // A Wish resolves in the residuals of the turn after it was
            // made (silently at full HP).
            for c in &mut side.slot_conditions {
                if let Some(n) = c.wish {
                    c.wish = (n + 1 < SlotConditions::WISH_TURNS).then_some(n + 1);
                }
            }
            for f in &mut side.future {
                if f.move_id.is_some() {
                    f.turns = f.turns.saturating_add(1);
                    // It resolved in this residual: it hit (`-end`), or
                    // missed with a hint that names no slot.
                    if f.turns >= FutureView::TURNS {
                        *f = FutureView::default();
                    }
                }
            }
        }
    }

    /// A field effect starts; `setter` is who started it, if the log says.
    fn field_start(&mut self, effect: FieldEffect, setter: Option<(SideId, u8)>) {
        let lost = self.item_lost(setter);
        let then = self.item_then(setter);
        let f = &mut self.field;
        match effect {
            FieldEffect::Terrain(t) => {
                f.terrain = Some(t);
                f.terrain_turns = 0;
                f.terrain_setter = setter;
                f.terrain_setter_lost = lost;
                f.terrain_setter_item = then;
            }
            FieldEffect::TrickRoom => f.trick_room = Some(0),
            FieldEffect::Gravity => f.gravity = Some(0),
            FieldEffect::MagicRoom => f.magic_room = Some(0),
            FieldEffect::WonderRoom => f.wonder_room = Some(0),
        }
    }

    fn field_end(&mut self, effect: FieldEffect) {
        let f = &mut self.field;
        match effect {
            FieldEffect::Terrain(t) => {
                if f.terrain == Some(t) {
                    f.terrain = None;
                    f.terrain_turns = 0;
                    f.terrain_setter = None;
                }
            }
            FieldEffect::TrickRoom => f.trick_room = None,
            FieldEffect::Gravity => f.gravity = None,
            FieldEffect::MagicRoom => f.magic_room = None,
            FieldEffect::WonderRoom => f.wonder_room = None,
        }
    }

    // -----------------------------------------------------------------
    // Switching.

    /// A Pokémon entering `actor.pos` (switch, drag): its identity, HP and
    /// status, and a fresh slot (Baton Pass and Shed Tail pass some of the
    /// old one on).
    #[allow(clippy::too_many_arguments)]
    fn switch_in(
        &mut self,
        actor: Actor,
        species: SpeciesId,
        level: u8,
        gender: Gender,
        hp: u16,
        max_hp: u16,
        status: Status,
        tera: Option<Type>,
        from: Option<MoveId>,
        drag: bool,
    ) {
        // A commanded Dondozo switched out (its switch was queued before
        // Tatsugiri came in): the `Update` after the switch ends the
        // command.
        if let Some((si, slot)) = slot_of(actor)
            && self.sides[si].slots[slot].volatiles.has(V::COMMANDED)
        {
            self.end_command(si);
        }
        let shown = self.shown(hp, max_hp);
        let Some(row) = self.row_for(actor) else { return };
        let (si, slot) = (actor.pos.side.index(), actor.pos.slot as usize);
        if slot >= MAX_ACTIVE {
            return;
        }
        let previous = self.sides[si].slots[slot];
        self.leave_slot(actor.pos.side, slot);
        self.ctx.silent_types[si][slot] = false;
        self.ctx.called_lock[si][slot] = None;
        let side = &mut self.sides[si];
        side.entered[slot] = side.mons[row as usize];
        side.entered_first[slot] = !side.mons[row as usize].is_revealed();
        let mut fresh = ActiveView { row: Some(row), ..ActiveView::default() };
        match from {
            Some(moves::BATON_PASS) => {
                fresh.boosts = previous.boosts;
                fresh.volatiles = PublicVolatiles(previous.volatiles.0 & !NO_BATON_PASS);
                // Octolock has no `noCopy`: it passes, trapping.
                if previous.octolocked {
                    fresh.octolocked = true;
                    fresh.volatiles.set(V::TRAPPED);
                }
                // Leech Seed and a partial trap pass with their source
                // (`copyVolatileFrom` copies the `effectState`).
                fresh.leech_seed_slot = previous.leech_seed_slot;
                fresh.partial_trap = previous.partial_trap;
                let (p, c) = (previous.counters, &mut fresh.counters);
                c.confusion_turns = p.confusion_turns;
                c.confusion_checks = p.confusion_checks;
                c.taunt_turns = p.taunt_turns;
                c.taunt_long = p.taunt_long;
                c.perish = p.perish;
                c.protect_chain = p.protect_chain;
                c.heal_block_turns = p.heal_block_turns;
                c.throat_chop_turns = p.throat_chop_turns;
                c.magnet_rise_turns = p.magnet_rise_turns;
                c.partial_trap_turns = p.partial_trap_turns;
            }
            Some(moves::SHED_TAIL) => fresh.volatiles = PublicVolatiles(previous.volatiles.0 & V::SUBSTITUTE),
            _ => {}
        }
        side.slots[slot] = fresh;
        self.commit_target();
        // The slot's turn record starts again: a switch, chosen (the
        // switches come before any move, in a slot that hasn't acted, not
        // after its Pokémon was sent out) or forced.
        let chosen = !drag
            && from.is_none()
            && self.turn > 0
            && !self.ctx.residual
            && !self.ctx.upkept
            && !self.ctx.moves_begun
            && self.sides[si].this_turn[slot].action == TurnAction::None
            && !self.ctx.exiting[si][slot];
        let order = if chosen {
            self.ctx.actions = self.ctx.actions.saturating_add(1);
            self.ctx.actions
        } else {
            0
        };
        let side = &mut self.sides[si];
        side.this_turn[slot] =
            TurnRecord { action: TurnAction::Switch, forced: !chosen, order, ..TurnRecord::default() };
        side.last_turn[slot] = TurnRecord::default();
        self.ctx.exiting[si][slot] = false;
        if self.ctx.acting == Some((si, slot)) {
            self.ctx.acting = None;
        }
        if self.ctx.moving == Some((si as u8, slot as u8)) {
            self.ctx.moving = None;
        }
        self.switch_in_identity(actor, species, level, gender);
        let side = &mut self.sides[si];
        let mon = &mut side.mons[row as usize];
        if mon.flags.has(MonFlags::FAINTED) {
            // Fainted as far as the log showed: an Illusion that fainted
            // under this Pokémon's name. The faint was the other's.
            mon.flags.set(MonFlags::FAINTED, false);
        }
        mon.flags.set(MonFlags::ACTIVE, true);
        mon.hp = shown;
        let kind = StatusKind::of(status);
        if kind != mon.status || kind == StatusKind::Toxic {
            // The toxic counter restarts on each switch-in; sleep's doesn't.
            mon.status = kind;
            mon.status_turns = 0;
        }
        side.slots[slot].hp = shown;
        side.slots[slot].status = kind;
        // The details say whether it is Terastallized.
        if let Some(t) = tera {
            mon.tera_type = Some(t);
        }
        mon.flags.set(MonFlags::TERASTALLIZED, tera.is_some());
        // Open Team Sheets without team preview: fill the row once seen.
        if !self.config.team_preview
            && let Some(sheet) = self.ots[si]
        {
            let dex = self.config.dex;
            // Its item only when first seen: the view tracks it from there.
            let first = self.sides[si].entered_first[slot];
            let mon = &mut self.sides[si].mons[row as usize];
            if let Some(entry) =
                sheet.mons.iter().find(|e| dex.species(e.species).base_species == dex.species(mon.species).base_species)
            {
                fill_from_sheet(dex, mon, entry, true, first);
            }
        }
    }

    /// The identity a switch-in or `|replace|` shows for `actor`.
    fn switch_in_identity(&mut self, actor: Actor, species: SpeciesId, level: u8, gender: Gender) -> Option<u8> {
        let row = self.row_for(actor)?;
        self.identity_at(actor.pos.side.index(), row, species, level, gender);
        Some(row)
    }

    /// [`PublicTracker::switch_in_identity`] for the record in `row` of
    /// side `si`.
    fn identity_at(&mut self, si: usize, row: u8, species: SpeciesId, level: u8, gender: Gender) {
        let dex = self.config.dex;
        let side = &mut self.sides[si];
        let mon = &mut side.mons[row as usize];
        if !mon.flags.has(MonFlags::REVEALED) {
            mon.flags.set(MonFlags::REVEALED, true);
            list_revealed(&mut side.reveal_order, row);
        }
        mon.flags.set(MonFlags::FORME_HIDDEN, false);
        mon.species = species;
        mon.details_species = species;
        mon.level = level;
        mon.gender = gender;
        if let Some(a) = single_ability(dex, species) {
            mon.base_ability = AbilityState::Known(a);
            mon.ability = AbilityState::Known(a);
        }
    }

    /// The Pokémon in `side`'s `slot` leaves it (another comes in): what
    /// lasts only while it is out is gone, and what it held on others ends
    /// (Block's and Mean Look's trap, Commander).
    fn leave_slot(&mut self, side: SideId, slot: usize) {
        let s = &mut self.sides[side.index()];
        let Some(old) = s.slots[slot].row else { return };
        // An Illusion may show it in the other slot too: then it stays.
        // Otherwise it leaves the field, even when the one coming in shows
        // its row: an Illusion disguised as the Pokémon it replaces
        // (Illusion copies the last Pokémon in the party, which the switch
        // just made the one leaving). The row shows the disguise at once,
        // but what lasted only while its Pokémon was out (a changed
        // ability, a temporary forme) is gone, and `entered` keeps the
        // record benched for the `|replace|` that restores it.
        if let Some(o) = (0..MAX_ACTIVE).find(|&o| o != slot && s.slots[o].row == Some(old)) {
            // The one left shows the record's HP and status.
            let (hp, status) = (s.slots[o].hp, s.slots[o].status);
            let mon = &mut s.mons[old as usize];
            set_status_from(mon, status);
            mon.hp = hp;
        } else {
            let mon = &mut s.mons[old as usize];
            mon.flags.set(MonFlags::ACTIVE, false);
            leave_field(mon);
        }
        self.release_links(side, slot);
    }

    /// What ends when the Pokémon in `side`'s `slot` leaves the field.
    fn release_links(&mut self, side: SideId, slot: usize) {
        for s in &mut self.sides {
            for av in &mut s.slots {
                if av.trapped_by == Some((side, slot as u8)) {
                    if !av.octolocked {
                        av.volatiles.clear(V::TRAPPED);
                    }
                    av.trapped_by = None;
                }
            }
        }
        // Commander: Dondozo stays `commanded` (trapped, boosted) when
        // Tatsugiri leaves: nothing removes that volatile but its own
        // switch-out. Tatsugiri stops commanding at the next `Update` that
        // finds no Dondozo beside it (`end_command`).
    }

    /// Commander's `onUpdate` once its commanded Dondozo is gone: no
    /// ally, so Tatsugiri stops commanding, silently. Returns the slots it
    /// ended.
    fn end_command(&mut self, side: usize) -> u8 {
        let mut ended = 0;
        let s = &mut self.sides[side];
        for (slot, av) in s.slots.iter_mut().enumerate() {
            // A Tatsugiri that lost Commander (Doodle) has no `onUpdate`:
            // it commands on.
            let ability = av.row.and_then(|r| s.mons.get(r as usize)).and_then(|m| m.ability.known());
            if av.volatiles.has(V::COMMANDING) && ability.is_none_or(|a| a == abilities::COMMANDER) {
                av.volatiles.clear(V::COMMANDING);
                ended |= 1 << slot;
            }
        }
        ended
    }

    /// The battle ended right after a commanded Dondozo fainted:
    /// `runAction` returns before the `Update` that would have ended the
    /// command.
    fn command_outlives_the_end(&mut self) {
        for (side, ended) in self.sides.iter_mut().zip(core::mem::take(&mut self.ctx.uncommanded)) {
            for (slot, av) in side.slots.iter_mut().enumerate() {
                if ended & (1 << slot) != 0 {
                    av.volatiles.set(V::COMMANDING);
                }
            }
        }
    }

    /// `|replace|`: the Illusion in `actor.pos` broke. What the disguise's
    /// record learnt since the switch-in was the Pokémon behind it's: it
    /// moves there, and the disguise's record goes back to what it was
    /// (poke-env's `end_illusion`), benched and marked
    /// [`MonFlags::WAS_DISGUISE`].
    fn replace(&mut self, actor: Actor, species: SpeciesId, level: u8, gender: Gender) {
        let (si, slot) = (actor.pos.side.index(), actor.pos.slot as usize);
        let disguise = self.sides[si].slots.get(slot).and_then(|s| s.row);
        let Some(row) = self.row_for(actor) else { return };
        if slot >= MAX_ACTIVE || disguise.is_none_or(|d| d == row) {
            self.switch_in_identity(actor, species, level, gender);
            return;
        }
        self.unmask_at(si, slot, row, species, level, gender);
    }

    /// The record as a `|replace|` would leave it had the Illusion of the
    /// Pokémon in `side`'s `slot` broken now, that Pokémon being the one in
    /// `row` (its details: `species`, `level`, `gender`): what its
    /// disguise's record learnt since it came in moves to `row`, the
    /// disguise's record goes back to what it was, and the slot shows `row`.
    ///
    /// For a reader that knows the Illusion (a player's own Pokémon, which
    /// its request names) or assumes one: the log itself breaks an Illusion
    /// with `|replace|`. Nothing happens if the slot is empty or already shows
    /// `row`, or `row` isn't a record.
    pub fn unmask(&mut self, side: SideId, slot: usize, row: u8, species: SpeciesId, level: u8, gender: Gender) {
        let si = side.index();
        let shown = self.sides[si].slots.get(slot).and_then(|s| s.row);
        if shown.is_none_or(|d| d == row) || row as usize >= self.sides[si].mons.len() {
            return;
        }
        self.unmask_at(si, slot, row, details_species(species), level, gender);
    }

    /// [`PublicTracker::replace`] of the Pokémon in `row`, its disguise
    /// shown in side `si`'s `slot` (another row).
    fn unmask_at(&mut self, si: usize, slot: usize, row: u8, species: SpeciesId, level: u8, gender: Gender) {
        let Some(disguise) = self.sides[si].slots[slot].row.filter(|&d| d != row) else { return };
        let dex = self.config.dex;
        let side = &mut self.sides[si];
        let mut shown = side.mons[disguise as usize];
        set_status_from(&mut shown, side.slots[slot].status);
        shown.hp = side.slots[slot].hp;
        let mut before = side.entered[slot];
        if before.level == 0 {
            // A record the disguise created (no team preview: the
            // disguise's Pokémon hadn't been seen): Illusion copies a party
            // member, so who it showed is a fact about the team, but
            // nothing else is.
            before = MonView {
                species: shown.details_species,
                details_species: shown.details_species,
                level: shown.level,
                gender: shown.gender,
                ..MonView::default()
            };
            if let Some(a) = single_ability(dex, before.species) {
                (before.ability, before.base_ability) = (AbilityState::Known(a), AbilityState::Known(a));
            }
        }
        let mon = &mut side.mons[disguise as usize];
        *mon = before;
        mon.flags.set(MonFlags::WAS_DISGUISE, true);
        // Off the field, unless the other slot shows it (below).
        mon.flags.set(MonFlags::ACTIVE, false);
        let mut kept = before;
        let mut shared = false;
        // The disguise itself came in meanwhile, in the other slot: it is
        // on the field, with what that slot showed.
        if let Some(other) = (0..MAX_ACTIVE).find(|&o| o != slot && side.slots[o].row == Some(disguise)) {
            shared = true;
            let (hp, status) = (side.slots[other].hp, side.slots[other].status);
            let first = side.entered_first[slot];
            let mon = &mut side.mons[disguise as usize];
            // A record the disguise created keeps who that slot showed.
            if first {
                (mon.species, mon.details_species) = (shown.species, shown.details_species);
                (mon.level, mon.gender) = (shown.level, shown.gender);
            }
            // A Mega Evolution or a `detailschange` the shared record
            // showed was the other slot's: an Illusion's details don't
            // change while it is up (Terastallization keeps them). That
            // slot keeps its species, ability and item (the Mega Stone),
            // and they don't move to the Pokémon behind the Illusion.
            let mega = shown.flags.has(MonFlags::MEGA) && !before.flags.has(MonFlags::MEGA);
            if mega || shown.details_species != before.details_species {
                (mon.species, mon.details_species) = (shown.species, shown.details_species);
                (mon.ability, mon.base_ability, mon.item) = (shown.ability, shown.base_ability, shown.item);
                mon.own_item = shown.own_item;
                mon.flags.set(MonFlags::MEGA, shown.flags.has(MonFlags::MEGA));
                (kept.ability, kept.base_ability, kept.item) = (shown.ability, shown.base_ability, shown.item);
                kept.own_item = shown.own_item;
            }
            // An ability a line changed in the other slot alone (Entrainment,
            // Skill Swap) is that slot's Pokémon's, the disguise's; it
            // doesn't move to the Illusion user (Entrainment's Multiscale
            // on the Glalie a Zoroark showed beside it, which an ally then
            // transformed into).
            if side.slots[other].ability_changed && !side.slots[slot].ability_changed {
                (mon.ability, mon.base_ability) = (shown.ability, shown.base_ability);
                (kept.ability, kept.base_ability) = (shown.ability, shown.base_ability);
            }
            // So is a temporary forme of the same species (Minior-Meteor); a
            // species the disguise showed for a transformed ally is another
            // one.
            if dex.species(shown.species).base_species == dex.species(mon.species).base_species {
                mon.species = shown.species;
            }
            mon.flags.set(MonFlags::ACTIVE, true);
            mon.flags.set(MonFlags::FAINTED, shown.flags.has(MonFlags::FAINTED));
            mon.flags.set(MonFlags::REVEALED, true);
            set_status_from(mon, status);
            mon.hp = hp;
        }
        // `reveal_order` lists exactly the revealed records.
        let revealed = side.mons[disguise as usize].is_revealed();
        match side.reveal_order.iter().position(|&r| r == disguise) {
            Some(i) if !revealed => {
                side.reveal_order.remove(i);
            }
            None if revealed => list_revealed(&mut side.reveal_order, disguise),
            _ => {}
        }
        let mon = &mut side.mons[row as usize];
        moved_facts(mon, &shown, &kept, shared);
        side.slots[slot].row = Some(row);
        side.entered[slot] = *mon;
        side.entered_first[slot] = !mon.is_revealed();
        if core::mem::take(&mut self.ctx.silent_types[si][slot]) {
            let av = &mut self.sides[si].slots[slot];
            av.types = None;
            av.volatiles.clear(V::TYPE_CHANGE);
        }
        self.identity_at(si, row, species, level, gender);
        let mon = &mut self.sides[si].mons[row as usize];
        mon.flags.set(MonFlags::ACTIVE, true);
    }

    /// Ally Switch: the Pokémon in `actor`'s slot and the one in `to` swap
    /// places, with what lasts while they stay in.
    fn ally_switch(&mut self, actor: Actor, to: u8) {
        let Some((si, from)) = slot_of(actor) else { return };
        let to = to as usize;
        if to >= MAX_ACTIVE || to == from {
            return;
        }
        let side = actor.pos.side;
        let s = &mut self.sides[si];
        s.slots.swap(from, to);
        s.entered.swap(from, to);
        s.entered_first.swap(from, to);
        // The turn records follow the Pokémon (a slot's healing wishes stay).
        s.this_turn.swap(from, to);
        s.last_turn.swap(from, to);
        self.ctx.exiting[si].swap(from, to);
        // The acting move's record (the Ally Switch's own) and its pending
        // target move with it: the target stays as its line named it.
        let swapped = |b: usize| {
            if b == from {
                to
            } else if b == to {
                from
            } else {
                b
            }
        };
        if let Some((a, b)) = self.ctx.acting
            && a == si
        {
            self.ctx.acting = Some((a, swapped(b)));
        }
        if let Some((a, b)) = self.ctx.moving
            && usize::from(a) == si
        {
            self.ctx.moving = Some((a, swapped(usize::from(b)) as u8));
        }
        if let Some((a, b, t)) = self.ctx.acting_target
            && a as usize == si
        {
            self.ctx.acting_target = Some((a, swapped(b as usize) as u8, t));
        }
        self.ctx.silent_types[si].swap(from, to);
        // So do a recharge's or a charge's residuals and a called lock (a
        // commanding Tatsugiri's skipped recharge ends in its new slot).
        self.ctx.lock_residuals[si].swap(from, to);
        // A charge aimed at a place relative to its user (an ally's) may
        // now point at the user itself, and the release draw a random foe:
        // the charges' targets are in doubt.
        self.ctx.charge_pressure[si] = [None; MAX_ACTIVE];
        self.ctx.called_lock[si].swap(from, to);
        let (a, b) = (Some((side, from as u8)), Some((side, to as u8)));
        for s in &mut self.sides {
            for av in &mut s.slots {
                if av.trapped_by == a {
                    av.trapped_by = b;
                } else if av.trapped_by == b {
                    av.trapped_by = a;
                }
            }
        }
    }

    /// A commanded Dondozo is down: an `Update` in the move it fainted
    /// in, or after the residuals it fainted in, ends the command; the latter
    /// only if the battle goes on, unless `updated`.
    fn dondozo_down(&mut self, actor: Actor, updated: bool) {
        if self.active(actor).is_some_and(|av| av.volatiles.has(V::COMMANDED)) {
            let si = actor.pos.side.index();
            let ended = self.end_command(si);
            if self.ctx.residual && !updated {
                self.ctx.uncommanded[si] |= ended;
            }
        }
    }

    fn faint(&mut self, actor: Actor) {
        self.dondozo_down(actor, false);
        let side = &mut self.sides[actor.pos.side.index()];
        let Some(row) = side.row_of(actor.id) else { return };
        let mon = &mut side.mons[row as usize];
        // Each `|faint|` is another Pokémon's, even when an Illusion made
        // the log name one twice.
        mon.flags.set(MonFlags::FAINTED, true);
        side.fainted = side.fainted.saturating_add(1);
        mon.hp.hp = 0;
        mon.hp.hint = HpHint::None;
        if mon.hp.max == 0 {
            mon.hp.max = 100;
        }
        mon.status = StatusKind::None;
        mon.status_turns = 0;
        mon.flags.set(MonFlags::TERASTALLIZED, false);
        // An Illusion that fainted disguised as the Pokémon in the other
        // slot: that one is still on the field, in its temporary forme.
        let slot = slot_of(actor).map_or(usize::MAX, |(_, s)| s);
        if !(0..MAX_ACTIVE).any(|o| o != slot && side.slots[o].row == Some(row)) {
            leave_field(&mut side.mons[row as usize]);
        }
        let mon = &side.mons[row as usize];
        let hp = mon.hp;
        if let Some((si, slot)) = slot_of(actor)
            && side.slots[slot].row == Some(row)
        {
            self.release_links(actor.pos.side, slot);
            self.sides[si].slots[slot] = ActiveView { row: Some(row), hp, ..ActiveView::default() };
        }
    }

    /// Whether the move a `cant` line names is one `actor` chose from its
    /// own slots: not a dance Dancer copied (`-activate|<mon>|ability:
    /// Dancer` just before), nor a move Magic Bounce or Magic Coat sent back
    /// (the last move line's, used on `actor`).
    fn chosen(&mut self, actor: Actor, move_id: MoveId) -> bool {
        if self.ctx.dancing.take().is_some_and(|d| same_mon(d, actor)) {
            return false;
        }
        let bounced = self.ctx.move_id == Some(move_id)
            && self.ctx.mover.is_some_and(|u| !same_mon(u, actor))
            && self.ctx.move_target.is_some_and(|t| same_mon(t, actor));
        // Nor the move one it just used called (`|move|<mon>|Metronome`,
        // `cant|<mon>|move: Gravity|Bounce`).
        let called = self.ctx.mover.is_some_and(|u| same_mon(u, actor))
            && self.ctx.move_id.is_some_and(|c| {
                c != move_id
                    && matches!(
                        c,
                        moves::METRONOME
                            | moves::COPYCAT
                            | moves::ASSIST
                            | moves::MIRROR_MOVE
                            | moves::ME_FIRST
                            | moves::NATURE_POWER
                    )
            });
        // Nor a lock-in move it called, which Disable can name (its
        // `lastMove`).
        let locked_call = slot_of(actor).is_some_and(|(si, slot)| self.ctx.called_lock[si][slot] == Some(move_id));
        !bounced && !called && !locked_call
    }

    /// Revival Blessing brought `target` back.
    fn revive(&mut self, target: Actor) {
        let side = &mut self.sides[target.pos.side.index()];
        // A revival takes a faint back even when the log counted it on
        // another record (an Illusion user fainting in disguise), or on none:
        // without team preview such an Illusion user has no record.
        side.fainted = side.fainted.saturating_sub(1);
        let Some(row) = side.row_of(target.id) else { return };
        side.mons[row as usize].flags.set(MonFlags::FAINTED, false);
    }

    // -----------------------------------------------------------------
    // Moves.

    fn use_move(&mut self, user: Actor, move_id: MoveId, target: Option<Actor>, from: Option<MoveFrom>) {
        self.ctx.item_ended = None;
        self.ctx.swap = None;
        self.ctx.pressure = None;
        self.ctx.pressure_taken = None;
        self.ctx.released = None;
        // Something acts after a failed protection: its `willAct` held, so
        // the `stall` roll failed it.
        self.ctx.stall = None;
        self.ctx.ability_lost = None;
        // A dance Dancer copied that failed at once is shown without its
        // `[from]` (`|move|<dancer>|Feather Dance||[still]`): still not its own.
        let dancing = self.ctx.dancing.take().is_some_and(|d| same_mon(d, user));
        let from = if dancing && from.is_none() { Some(MoveFrom::Ability(abilities::DANCER)) } else { from };
        self.ctx.mover = Some(user);
        self.ctx.move_id = Some(move_id);
        self.ctx.move_target = target;
        self.ctx.jaw_lock_pending = false;
        self.ctx.contact = None;
        // The slot that shows the user, looked up once (nothing below moves
        // a Pokémon but an ability's reveal, after which it is looked up
        // again).
        let mut at = self.slot_index(user);
        // A Round moved up by an ally's Round, or a pledge combined with
        // an ally's, names that move with `[from]` but is its own.
        // A charged move released without `[from]lockedmove` (it failed at
        // once) is the charge's, not a new use: one a Metronome called
        // isn't its own.
        let released = from.is_none()
            && at.is_some_and(|(si, slot)| {
                let av = &self.sides[si].slots[slot];
                av.volatiles.has(V::CHARGING) && av.locked_move == Some(move_id)
            });
        // So is a lock-in move a Metronome called, continued (gen9singles
        // 4,536,897: `|move|<mon>|Outrage||[still]`, `-fail`: a Choice
        // lock into the Metronome failing it). One that names its target
        // is a new use: a continuation shows `[from]lockedmove`, and the
        // lock may have ended silently (its fatigue confusion doesn't
        // show on a confused Pokémon).
        let lock = slot_of(user).map(|(si, slot)| &mut self.ctx.called_lock[si][slot]);
        let continued = match (lock, from) {
            (Some(l), None) => {
                let same = *l == Some(move_id) && target.is_none();
                if !same {
                    *l = None;
                }
                same
            }
            (Some(l), Some(MoveFrom::Move(caller))) => {
                let lock_in = matches!(
                    move_id,
                    moves::OUTRAGE
                        | moves::THRASH
                        | moves::PETAL_DANCE
                        | moves::RAGING_FURY
                        | moves::UPROAR
                        | moves::ROLLOUT
                        | moves::ICE_BALL
                );
                *l = (lock_in && caller != moves::SLEEP_TALK).then_some(move_id);
                false
            }
            _ => false,
        };
        let from = if released || continued { Some(MoveFrom::LockedMove) } else { from };
        let own = match from {
            None => true,
            Some(MoveFrom::Move(by)) => {
                by == moves::ROUND && move_id == moves::ROUND
                    || matches!(by, moves::FIRE_PLEDGE | moves::GRASS_PLEDGE | moves::WATER_PLEDGE)
                        && matches!(move_id, moves::FIRE_PLEDGE | moves::GRASS_PLEDGE | moves::WATER_PLEDGE)
            }
            Some(_) => false,
        };
        let locked = from == Some(MoveFrom::LockedMove);
        self.record_move(user, at, move_id, target, from, own || locked);
        if let Some(MoveFrom::Ability(a)) = from {
            // Dancer, Magic Bounce: the user's ability.
            self.reveal_ability(user, a);
            at = self.slot_index(user);
        }
        let reflected =
            matches!(from, Some(MoveFrom::Ability(abilities::MAGIC_BOUNCE) | MoveFrom::Move(moves::MAGIC_COAT)));
        let mut transformed = false;
        let mut mimicked = None;
        let stall_move = is_stall_move(move_id);
        if let Some((si, slot)) = at {
            let av = &mut self.sides[si].slots[slot];
            // Showdown's `lastMove`: a move from its own slots, or its
            // lock-in's, not one it called. A release shown without
            // `[from]lockedmove` is a Choice lock (or Gorilla Tactics)
            // failing it in `onBeforeMove`, before `moveUsed`.
            if (own || locked) && !released {
                av.last_move = Some(move_id);
                if move_id != moves::STRUGGLE {
                    av.choice_move = Some(move_id);
                }
                av.item_received = false;
            }
            av.moved_this_turn = true;
            transformed = av.volatiles.has(V::TRANSFORMED);
            mimicked = av.mimic_move.filter(|_| av.volatiles.has(V::MIMIC));
            // Glaive Rush's drawback ends at its user's next move; Destiny
            // Bond too (a second Destiny Bond in a row fails, its
            // `onPrepareHit`; a new one shows `-singlemove` again). Both
            // end in `onBeforeMove`, which a move Magic Bounce or Magic
            // Coat sends back skips (`useMove`, not `runMove`).
            if !reflected {
                av.volatiles.clear(V::GLAIVE_RUSH | V::DESTINY_BOND);
            }
            if locked {
                if av.volatiles.has(V::CHARGING) {
                    av.volatiles.clear(V::CHARGING);
                    av.locked_move = None;
                } else {
                    av.volatiles.set(V::LOCKED_MOVE);
                    av.locked_move = Some(move_id);
                    av.counters.locked_turns = av.counters.locked_turns.saturating_add(1);
                }
            } else if own {
                av.volatiles.clear(V::LOCKED_MOVE | V::CHARGING);
                av.locked_move = None;
                av.counters.locked_turns = 0;
            }
            // A protection counts for the chain however it was used (Sleep
            // Talk calls one too).
            if stall_move && !locked {
                av.protect_pending = true;
            }
            match move_id {
                moves::MINIMIZE => av.volatiles.set(V::MINIMIZE),
                moves::DEFENSE_CURL => av.volatiles.set(V::DEFENSE_CURL),
                _ => {}
            }
        }
        self.ctx.guard_again = None;
        if stall_move {
            self.guard_again(user, move_id, locked);
        }
        if !own || move_id == moves::STRUGGLE {
            if let (Some(MoveFrom::Move(caller)), false) = (from, own) {
                // A move called by one with PP (Sleep Talk): Pressure takes
                // the caller's (a transformed caller's is a copied move).
                if !transformed && self.config.dex.move_(caller).pp > 0 {
                    self.pressure_spend(user, caller, move_id, target, 0, 0);
                }
                // Sleep Talk calls one of the user's own moves: it is
                // revealed, without a use.
                if caller == moves::SLEEP_TALK {
                    self.reveal_move(user, move_id);
                }
            }
            // A charged move released (`[from]lockedmove`) whose charge
            // turn hid its target (`[still]`): Pressure took its PP then,
            // if the Pokémon in the slot the release names (it aims at a
            // place: `twoturnmove`'s `targetLoc`) had it at the time. With
            // the other foe down, the release may have gone to another
            // foe than the charge (`runMove` draws a new one when its
            // target fainted), and a redirection may have drawn it (Follow
            // Me, Rage Powder; Storm Drain and Lightning Rod show it after:
            // `-activate`, which takes the PP back): left in doubt.
            if locked
                && !transformed
                && let Some((si, slot)) = slot_of(user)
                && let Some((id, m, pressure)) = self.ctx.charge_pressure[si][slot].take()
                && id == user.id
                && m == move_id
                && let Some(t) = target.filter(|t| t.pos.side != user.pos.side && (t.pos.slot as usize) < MAX_ACTIVE)
                && pressure & (1 << t.pos.slot) != 0
                && self.side(t.pos.side).active(1 - t.pos.slot as usize).is_some_and(|(_, m)| !m.is_fainted())
                && !self.sides[t.pos.side.index()].slots.iter().any(|s| s.volatiles.has(V::CENTER_OF_ATTENTION))
            {
                self.spend(user, move_id, 0, 1);
                self.ctx.released = Some((user.pos.side.index() as u8, user.id, move_id));
            }
            return;
        }
        if transformed {
            if let Some(av) = self.active_mut(user)
                && !av.copied_moves.contains(&move_id)
                && !av.copied_moves.is_full()
            {
                av.copied_moves.push(move_id);
            }
            return;
        }
        if mimicked == Some(move_id) || self.mon(user).is_some_and(|m| m.sketched == Some(move_id)) {
            return;
        }
        self.pressure_spend(user, move_id, move_id, target, 1, 1);
        // In doubles, a charge turn hides its target (`[still]`, from
        // `twoturnmove.onStart`): the release names it (the chosen place,
        // `lastMoveTargetLoc`), if the charge went there. With a foe down,
        // the charge may have gone to another foe (`getMoveTargets` draws
        // one for a fainted target), and Follow Me or Rage Powder may have
        // drawn it (`RedirectTarget`): left in doubt.
        if target.is_none()
            && self.config.active > 1
            && self.config.dex.move_(move_id).flags.contains(MoveFlags::CHARGE)
            && let Some((si, slot)) = slot_of(user)
        {
            let foe = self.side(user.pos.side.foe());
            let standing = (0..MAX_ACTIVE).all(|s| {
                foe.active(s).is_some_and(|(_, m)| !m.is_fainted())
                    && !foe.slots[s].volatiles.has(V::CENTER_OF_ATTENTION)
            });
            let (pressure, _) = self.pressure(user, move_id, None);
            self.ctx.charge_pressure[si][slot] = standing.then_some((user.id, move_id, pressure));
        }
    }

    /// `user` used `move_id` from its slot `charged`'s PP (the move itself,
    /// or the move that called it: Sleep Talk) `uses` times, spending `pp`
    /// plus what Pressure takes as far as the log shows; the Pressure foes
    /// it could still take PP for, should the move turn out to hit more
    /// than its line names (`[spread]`), are kept in
    /// [`ApplyCtx::pressure`](crate::tracker::ApplyCtx).
    fn pressure_spend(
        &mut self,
        user: Actor,
        charged: MoveId,
        move_id: MoveId,
        target: Option<Actor>,
        uses: u8,
        pp: u8,
    ) {
        let (all, taken) = self.pressure(user, move_id, target);
        if all == 0 {
            // No Pressure to count (the context's were cleared with the
            // move line).
            if uses > 0 || pp > 0 {
                self.spend(user, charged, uses, pp);
            }
            return;
        }
        let extra = (all & taken).count_ones() as u8;
        if uses > 0 || pp > 0 || extra > 0 {
            self.spend(user, charged, uses, pp + extra);
        }
        let side = user.pos.side.index() as u8;
        self.ctx.pressure_taken = (extra > 0).then_some((side, user.id, charged, extra));
        self.ctx.pressure = (all & !taken != 0).then_some((side, user.id, charged, all & !taken));
    }

    /// `[spread]` on a move whose dex target is a single Pokémon: it hit
    /// every adjacent foe (Expanding Force in Psychic Terrain, whose
    /// `onModifyMove` makes it `allAdjacentFoes`), and Pressure took PP for
    /// each (`getMoveTargets`' `pressureTargets` are its targets before any
    /// of them is protected or immune, which `[spread]` leaves out).
    fn spread_pressure(&mut self) {
        let (Some((side, id, charged, rest)), Some(mover), Some(m)) =
            (self.ctx.pressure.take(), self.ctx.mover, self.ctx.move_id)
        else {
            return;
        };
        let single = matches!(
            self.config.dex.move_(m).target,
            MoveTarget::Normal | MoveTarget::Any | MoveTarget::AdjacentFoe | MoveTarget::RandomNormal
        );
        if mover.pos.side.index() == side as usize && mover.id == id && single {
            self.spend(mover, charged, 0, rest.count_ones() as u8);
        }
    }

    /// The foe slots whose Pressure takes PP from `user`'s move as far as
    /// the log shows (Showdown's `getMoveTargets`, `pressureTargets`, and
    /// Pressure's `onDeductPP`), as masks of the foe's slots: the ones
    /// whose Pokémon is known to have Pressure (not fainted, not
    /// suppressed), and the move's targets:
    ///
    /// - every foe for a spread or field-wide move, or a `mustpressure`
    ///   one (none for another move on the foe's side: Spikes), unless the
    ///   target it names is off the field;
    /// - the target its line names; with Dragon Darts (`smartTarget`), the
    ///   target's ally too, while both are standing (`getSmartTargets`);
    /// - a target `[still]` hid (a failed move, a charge turn): in singles
    ///   the one foe, for a move aimed at a foe. In doubles it stays in
    ///   doubt, but for a charged move, whose release names it.
    fn pressure(&self, user: Actor, move_id: MoveId, target: Option<Actor>) -> (u8, u8) {
        if !self.side(user.pos.side.foe()).mons.iter().any(|m| m.ability == AbilityState::Known(abilities::PRESSURE)) {
            return (0, 0);
        }
        if self.neutralizing_gas() {
            return (0, 0);
        }
        let data = self.config.dex.move_(move_id);
        let must = data.flags.contains(MoveFlags::MUST_PRESSURE);
        if data.target == MoveTarget::FoeSide && !must {
            return (0, 0);
        }
        // A named target that isn't on the field (a fainted ally, shown
        // with `[notarget]`): no targets, so no Pressure either, even for a
        // `mustpressure` move (`getMoveTargets` returns before it).
        if target.is_some_and(|t| !t.active) && !data.flags.contains(MoveFlags::FUTURE_MOVE) {
            return (0, 0);
        }
        let foe = user.pos.side.foe();
        let side = self.side(foe);
        let mut pressure = 0u8;
        let mut standing = 0u8;
        for slot in 0..self.config.active as usize {
            let Some((_, mon)) = side.active(slot) else { continue };
            if mon.is_fainted() {
                continue;
            }
            standing |= 1 << slot;
            if mon.ability == AbilityState::Known(abilities::PRESSURE)
                && !side.slots[slot].volatiles.has(V::ABILITY_SUPPRESSED)
            {
                pressure |= 1 << slot;
            }
        }
        // Counter, Mirror Coat and Metal Burst (`scripted`) aim at a foe
        // too.
        let aimed = matches!(
            data.target,
            MoveTarget::Normal
                | MoveTarget::Any
                | MoveTarget::AdjacentFoe
                | MoveTarget::RandomNormal
                | MoveTarget::Scripted
        );
        let targets =
            if must || matches!(data.target, MoveTarget::All | MoveTarget::AllAdjacent | MoveTarget::AllAdjacentFoes) {
                standing
            } else {
                match target {
                    Some(t) if t.pos.side == foe && (t.pos.slot as usize) < MAX_ACTIVE => {
                        let named = 1u8 << t.pos.slot;
                        if move_id == moves::DRAGON_DARTS && named & standing != 0 && standing.count_ones() == 2 {
                            standing
                        } else {
                            named
                        }
                    }
                    None if self.config.active == 1 && aimed => standing,
                    _ => 0,
                }
            };
        (pressure, targets)
    }

    /// Neutralizing Gas is up: its holder isn't transformed nor under
    /// Gastro Acid (Showdown's `ignoringAbility`).
    fn neutralizing_gas(&self) -> bool {
        self.sides.iter().any(|s| {
            s.slots.iter().any(|av| {
                av.volatiles.has(V::NEUTRALIZING_GAS) && !av.volatiles.has(V::TRANSFORMED | V::ABILITY_SUPPRESSED)
            })
        })
    }

    /// `user` used `move_id` from its own slot `uses` times, spending `pp`.
    /// PP spent stop at the move's maximum: past zero, nothing is taken
    /// (`deductPP`'s floor; Pressure against a move out of PP).
    fn spend(&mut self, user: Actor, move_id: MoveId, uses: u8, pp: u8) {
        let Some(mon) = self.mon_mut(user) else { return };
        let spent = match mon.moves.iter().position(|m| m.id == move_id) {
            Some(i) => {
                let m = &mut mon.moves[i];
                m.uses = m.uses.saturating_add(uses);
                m.pp_spent = m.pp_spent.saturating_add(pp);
                m.pp_spent
            }
            None if !mon.moves.is_full() => {
                mon.moves.push(RevealedMove { id: move_id, uses, pp_spent: pp });
                pp
            }
            None => return,
        };
        // Every maximum is 8 or more (5 PP with every PP Up), but the
        // `noPPBoosts` moves' (1 PP): only past that is it worked out.
        if spent > 8 || spent > 1 && matches!(move_id, moves::SKETCH | moves::REVIVAL_BLESSING | moves::STRUGGLE) {
            let max = move_max_pp(self.config.mechanics, self.config.dex.move_(move_id));
            if spent > max
                && let Some(mon) = self.mon_mut(user)
                && let Some(m) = mon.moves.iter_mut().find(|m| m.id == move_id)
            {
                m.pp_spent = max;
            }
        }
    }

    /// `actor` has `move_id` in its move slots (it tried to use it, or
    /// an effect named it), with no use.
    fn reveal_move(&mut self, actor: Actor, move_id: MoveId) {
        if move_id == moves::STRUGGLE {
            return;
        }
        if let Some(av) = self.active_mut(actor) {
            if av.volatiles.has(V::TRANSFORMED) {
                if !av.copied_moves.contains(&move_id) && !av.copied_moves.is_full() {
                    av.copied_moves.push(move_id);
                }
                return;
            }
            if av.volatiles.has(V::MIMIC) && av.mimic_move == Some(move_id) {
                return;
            }
        }
        if self.mon(actor).is_some_and(|m| m.sketched == Some(move_id)) {
            return;
        }
        self.spend(actor, move_id, 0, 0);
    }

    /// An effect took `amount` PP from `actor`'s `move_id` (Eerie Spell,
    /// Spite).
    fn reveal_move_pp(&mut self, actor: Actor, move_id: MoveId, amount: u8) {
        let transformed = self.active(actor).is_some_and(|av| av.volatiles.has(V::TRANSFORMED));
        // Mimic's copy isn't its own move either (Eerie Spell on a Mimic'd
        // Taunt).
        let mimicked =
            self.active(actor).is_some_and(|av| av.volatiles.has(V::MIMIC) && av.mimic_move == Some(move_id));
        self.reveal_move(actor, move_id);
        // Nor is Sketch's copy kept with the record's moves.
        let sketched = self.mon(actor).is_some_and(|m| m.sketched == Some(move_id));
        if !transformed && !mimicked && !sketched {
            self.spend(actor, move_id, 0, amount);
        }
    }

    /// A Leppa Berry gave `move_id` PP back (`leppaberry.onEat`: 10, 20
    /// with Ripen, up to its maximum), from the holder's own move slots
    /// (not a transformed Pokémon's copies or Mimic's move).
    fn leppa(&mut self, actor: Actor, move_id: MoveId) {
        let copied = self.active(actor).is_some_and(|av| {
            av.volatiles.has(V::TRANSFORMED) || av.volatiles.has(V::MIMIC) && av.mimic_move == Some(move_id)
        });
        let Some(mon) = self.mon_mut(actor) else { return };
        if copied || mon.sketched == Some(move_id) {
            return;
        }
        let added = if mon.ability == AbilityState::Known(abilities::RIPEN) { 20 } else { 10 };
        if let Some(m) = mon.moves.iter_mut().find(|m| m.id == move_id) {
            m.pp_spent = m.pp_spent.saturating_sub(added);
        }
    }

    /// Sticky Barb moves to an attacker that makes contact and holds
    /// nothing, without a line: a hit from one not known to hold something
    /// leaves the target's Sticky Barb in doubt (or gone, and the
    /// attacker's, when the attacker is known to hold nothing).
    fn sticky_barb(&mut self, target: Actor) {
        let Some(user) = self.ctx.mover.filter(|u| u.pos.side != target.pos.side || u.id != target.id) else {
            return;
        };
        // Magic Room suppresses its `onHit` (`ignoringItem`). (The cheap checks
        // first: what follows only reads.)
        if self.mon(target).is_none_or(|m| m.item != ItemState::Known(items::STICKY_BARB))
            || self.field.magic_room.is_some()
        {
            return;
        }
        // Shell Side Arm shows its category only against a foe.
        let contact = match (self.ctx.contact, self.ctx.move_id) {
            (Some(c), _) => Some(c),
            (None, Some(moves::SHELL_SIDE_ARM)) => None,
            (None, m) => Some(m.is_some_and(|m| self.config.dex.move_(m).flags.contains(MoveFlags::CONTACT))),
        };
        // Long Reach makes no contact: an attacker that may have it leaves
        // the barb in doubt.
        let contact = match (contact, self.mon(user).map(|m| (m.ability, m.species))) {
            (Some(true), Some((AbilityState::Known(a), _))) => Some(a != abilities::LONG_REACH),
            (Some(true), Some((AbilityState::Unknown, sp))) => {
                let a = self.config.dex.species(sp).abilities;
                let may = [Some(a.primary), a.secondary, a.hidden, a.special].contains(&Some(abilities::LONG_REACH));
                if may { None } else { Some(true) }
            }
            (c, _) => c,
        };
        if contact == Some(false) {
            return;
        }
        let user_item = self.mon(user).map_or(ItemState::Unknown, |m| m.item);
        match user_item {
            ItemState::NoItem | ItemState::Gone { .. } if contact.is_none() => {
                for x in [target, user] {
                    if let Some(mon) = self.mon_mut(x) {
                        mon.item = ItemState::Unknown;
                    }
                }
            }
            ItemState::Known(_) => {}
            ItemState::NoItem | ItemState::Gone { .. } => {
                if let Some(mon) = self.mon_mut(target) {
                    mon.item = ItemState::Gone { item: Some(items::STICKY_BARB), how: ItemLoss::Traded };
                }
                // `stickybarb.onHit`: `source.setItem(item)`.
                self.receive_item(user, None, false);
                if let Some(mon) = self.mon_mut(user) {
                    mon.item = ItemState::Known(items::STICKY_BARB);
                }
            }
            ItemState::Unknown => {
                if let Some(mon) = self.mon_mut(target) {
                    mon.item = ItemState::Unknown;
                }
            }
        }
    }

    /// Toxic's residual went off: its counter rises, to at most 15.
    fn toxic_tick(&mut self, target: Actor) {
        if let Some(mon) = self.mon_mut(target)
            && mon.status == StatusKind::Toxic
        {
            mon.status_turns = mon.status_turns.saturating_add(1).min(15);
        }
    }

    /// A `cant` line: `actor` tried to move this turn and couldn't.
    fn cant(&mut self, actor: Actor) {
        // An action after a failed protection (`ApplyCtx::stall`).
        self.ctx.stall = None;
        if let Some(av) = self.active_mut(actor) {
            av.moved_this_turn = true;
            av.volatiles.clear(V::GLAIVE_RUSH);
            // A charge or a rampage stopped: the lock ends with the turn;
            // Destiny Bond ends too (`onMoveAborted`).
            av.volatiles.clear(V::CHARGING | V::LOCKED_MOVE | V::DESTINY_BOND);
            av.locked_move = None;
        }
    }

    /// A `cant` line or a confusion self-hit of `actor` ends the Dancer
    /// copy it was about to make: no `|move|` line follows, and its own
    /// move later in the turn is its own again (Showdown's `runMove` for
    /// the copy returns at `BeforeMove`).
    fn stop_dance(&mut self, actor: Actor) {
        if self.ctx.dancing.is_some_and(|d| same_mon(d, actor)) {
            self.ctx.dancing = None;
        }
    }

    // -----------------------------------------------------------------
    // The turn records.

    /// The slot `actor` is in, if the slot shows it.
    fn slot_index(&self, actor: Actor) -> Option<(usize, usize)> {
        self.active(actor)?;
        slot_of(actor)
    }

    /// A `|move|` line: the slot's action if it hasn't acted this turn
    /// (`action`: a move from its own slots or a lock-in), the move a move
    /// it just used called, or neither (Dancer, Magic Bounce, a second
    /// move). Also a Wish, Healing Wish or Lunar Dance for its slot.
    /// `at` is the user's [`PublicTracker::slot_index`].
    fn record_move(
        &mut self,
        user: Actor,
        at: Option<(usize, usize)>,
        move_id: MoveId,
        target: Option<Actor>,
        from: Option<MoveFrom>,
        action: bool,
    ) {
        self.ctx.moves_begun = true;
        self.ctx.slot_made = None;
        self.commit_target();
        let instructed = self.ctx.instructed.take();
        debug_assert_eq!(at, self.slot_index(user));
        let Some((si, slot)) = at else {
            self.ctx.acting = None;
            self.ctx.moving = None;
            return;
        };
        // A move Instruct queued: the Pokémon moves now (`activePokemon`),
        // but it isn't its turn's action (which may still be queued: Showdown's
        // `queue.willMove`), so it sets nothing in its record.
        let instructed = instructed == Some((si as u8, slot as u8));
        let action = action && !instructed;
        // Any move of its own (its action, one it called, a lock-in, one
        // Instruct queued); Dancer's copy and Magic Bounce's reflection run
        // as external moves.
        self.ctx.moving = (!matches!(from, Some(MoveFrom::Ability(_)))).then_some((si as u8, slot as u8));
        let data = self.config.dex.move_(move_id);
        let spread =
            self.config.active > 1 && matches!(data.target, MoveTarget::AllAdjacent | MoveTarget::AllAdjacentFoes);
        let aimed = match target {
            _ if spread => TurnTarget::Spread,
            // A Pokémon named without a slot (fainted, off the field) is no
            // slot's.
            Some(t) if t.active && (t.pos.slot as usize) < MAX_ACTIVE => {
                TurnTarget::Slot { side: t.pos.side, slot: t.pos.slot }
            }
            _ => TurnTarget::None,
        };
        let r = &mut self.sides[si].this_turn[slot];
        let called = match from {
            Some(MoveFrom::Move(caller)) => {
                !action
                    && r.action == TurnAction::Move
                    && r.move_id == Some(caller)
                    && self.ctx.acting == Some((si, slot))
            }
            _ => false,
        };
        // A sleeping Pokémon's `cant|slp` comes before the move it can use
        // asleep (Sleep Talk, Snore): that move is its action, in the
        // cant's place.
        let asleep_move = r.action == TurnAction::Cant && r.cant == CantKind::Sleep;
        let new_action = action && (r.action == TurnAction::None || asleep_move);
        if new_action {
            let order = if asleep_move {
                r.order
            } else {
                self.ctx.actions = self.ctx.actions.saturating_add(1);
                self.ctx.actions
            };
            *r = TurnRecord {
                action: TurnAction::Move,
                order,
                move_id: Some(move_id),
                outcome: MoveOutcome::Hit,
                // What it took before it moved stays.
                taken: r.taken,
                taken_by: r.taken_by,
                ..TurnRecord::default()
            };
        } else if called {
            let caller = r.move_id;
            *r = TurnRecord { move_id: Some(move_id), called_by: caller, target: TurnTarget::None, ..*r };
            (r.outcome, r.crit, r.effect, r.hits, r.dealt) =
                (MoveOutcome::Hit, false, [0; MAX_ACTIVE], 0, [0; MAX_ACTIVE]);
        } else {
            self.ctx.acting = None;
            self.slot_condition(user, si, slot, move_id);
            return;
        }
        self.ctx.acting = Some((si, slot));
        self.ctx.acting_hit = false;
        self.ctx.acting_failed = false;
        self.ctx.acting_target = Some((si as u8, slot as u8, aimed));
        self.slot_condition(user, si, slot, move_id);
    }

    /// The acting move is over: its target goes into its record (none if
    /// its line turned out `[still]`).
    fn commit_target(&mut self) {
        if let Some((si, slot, t)) = self.ctx.acting_target.take() {
            self.sides[si as usize].this_turn[slot as usize].target = t;
        }
    }

    /// Wish, Healing Wish and Lunar Dance wait in their user's slot; a
    /// `-fail` right after undoes it.
    fn slot_condition(&mut self, user: Actor, si: usize, slot: usize, move_id: MoveId) {
        let c = &mut self.sides[si].slot_conditions[slot];
        let before = *c;
        match move_id {
            moves::WISH => c.wish = c.wish.or(Some(0)),
            moves::HEALING_WISH => c.healing_wish = true,
            moves::LUNAR_DANCE => c.lunar_dance = true,
            _ => return,
        }
        self.ctx.slot_made = Some((user, before));
    }

    /// [`PublicTracker::acting_failed`] for a line that names the move's
    /// user (`-miss`, `-fail`): only the acting slot's.
    fn user_failed(&mut self, user: Actor, outcome: MoveOutcome) {
        if self.ctx.acting.is_some() && self.slot_index(user) == self.ctx.acting {
            self.acting_failed(user, outcome);
        }
    }

    /// The side and row of the Pokémon whose move is running, if any.
    #[inline]
    fn acting_row(&self) -> Option<(SideId, u8)> {
        let (si, slot) = self.ctx.acting?;
        Some((SideId::BOTH[si], self.sides[si].slots[slot].row?))
    }

    /// Whether the Pokémon in `row` of `side` has lost its item now (a
    /// field or side condition's setter as it starts it: `Gone`).
    fn item_lost(&self, setter: Option<(SideId, u8)>) -> bool {
        setter
            .and_then(|(side, row)| self.sides[side.index()].mons.get(row as usize))
            .is_some_and(|m| matches!(m.item, ItemState::Gone { .. }))
    }

    /// The item of the Pokémon in `row` of `side` as the log shows it now
    /// (a field or side condition's setter as it starts it); `Unknown`
    /// without a setter.
    fn item_then(&self, setter: Option<(SideId, u8)>) -> ItemState {
        setter
            .and_then(|(side, row)| self.sides[side.index()].mons.get(row as usize))
            .map_or(ItemState::Unknown, |m| m.item)
    }

    /// The side and row of the Pokémon an event names, once seen.
    fn actor_row(&self, actor: &Actor) -> Option<(SideId, u8)> {
        Some((actor.pos.side, self.sides[actor.pos.side.index()].row_of(actor.id)?))
    }

    /// Showdown's `queue.willMove(pokemon)` when a volatile starts on
    /// `actor` (Taunt, Encore and Disable read it for their duration): its
    /// turn's move action is still in the queue. It has one when it was on
    /// the field when the turn's actions were chosen (`turns_active` counts
    /// the `|turn|` lines since it entered), the residuals haven't begun,
    /// its slot shows no action of its own yet (a `|move|`, a `cant`; a
    /// Dancer copy, a move Magic Bounce sent back or one Instruct queued
    /// isn't the slot's action), it isn't using its action now, and it
    /// isn't inside a Dondozo (Commander: no action). A Pokémon Instruct
    /// made move before its own action still has that action queued.
    fn will_move(&self, actor: Actor) -> bool {
        let Some((si, slot)) = slot_of(actor) else { return false };
        let Some(av) = self.active(actor) else { return false };
        !self.ctx.residual
            && av.turns_active > 0
            && !av.volatiles.has(V::COMMANDING)
            && self.sides[si].this_turn[slot].action == TurnAction::None
            && !self.acting_is(actor)
    }

    /// Whether `actor` is using a move now, its action or one Instruct
    /// queued (Showdown's `pokemon === this.activePokemon && this.activeMove
    /// && !this.activeMove.isExternal`, which Disable reads).
    fn moving_is(&self, actor: Actor) -> bool {
        self.ctx.moving.is_some() && slot_of(actor).map(|(si, slot)| (si as u8, slot as u8)) == self.ctx.moving
    }

    /// `actor` is the Pokémon whose action the lines are about (the user
    /// of the acting move).
    fn acting_is(&self, actor: Actor) -> bool {
        self.ctx.acting.is_some() && slot_of(actor) == self.ctx.acting
    }

    /// The record of the slot acting now, if `actor` is on the field (the
    /// user, or one of its targets).
    fn acting_record(&mut self, actor: Actor) -> Option<&mut TurnRecord> {
        let (si, slot) = self.ctx.acting?;
        self.active(actor)?;
        Some(&mut self.sides[si].this_turn[slot])
    }

    /// The acting move showed `outcome` for `actor`, one of its targets:
    /// the move's outcome, unless it damaged a target or showed another
    /// failure first.
    fn acting_failed(&mut self, actor: Actor, outcome: MoveOutcome) {
        let (hit, failed) = (self.ctx.acting_hit, self.ctx.acting_failed);
        if let Some(r) = self.acting_record(actor)
            && !hit
            && !failed
        {
            r.outcome = outcome;
            self.ctx.acting_failed = true;
        }
    }

    /// The log announced an effectiveness against `target`.
    fn announce_effect(&mut self, target: Actor, effect: i8) {
        let Some((si, _)) = self.ctx.acting else { return };
        let slot = target.pos.slot as usize;
        if target.pos.side.index() != si
            && slot < MAX_ACTIVE
            && let Some(r) = self.acting_record(target)
        {
            r.effect[slot] = effect;
        }
    }

    /// `target` lost HP (its slot showed `before`, and now shows its record's
    /// HP): the turn records of its slot, and of the slot whose move hit it.
    fn record_hp_loss(&mut self, target: Actor, before: Option<HpView>, cause: u8) {
        let Some(before) = before else { return };
        let Some((ti, tslot)) = slot_of(target) else { return };
        // `before` came from this slot (`active`): it still shows `target`.
        let now = self.sides[ti].slots[tslot].hp;
        let points = |h: HpView| if h.max == 0 { 100 } else { u32::from(h.hp) * 100 / u32::from(h.max) };
        let lost = points(before).saturating_sub(points(now)).min(100) as u8;
        if lost == 0 {
            return;
        }
        let r = &mut self.sides[ti].this_turn[tslot];
        r.taken = r.taken.saturating_add(lost).min(100);
        r.taken_by |= cause;
        if cause != taken_by::MOVE {
            return;
        }
        if let Some((si, slot)) = self.ctx.acting
            && si != ti
        {
            let r = &mut self.sides[si].this_turn[slot];
            r.dealt[tslot] = r.dealt[tslot].saturating_add(lost).min(100);
            r.hits = r.hits.max(1);
            r.outcome = MoveOutcome::Hit;
            self.ctx.acting_hit = true;
        }
    }

    /// A `cant` line (or confusion's self-hit): the slot's action, if it
    /// hasn't acted this turn.
    fn cant_action(&mut self, actor: Actor, kind: CantKind) {
        self.ctx.moves_begun = true;
        self.commit_target();
        // A Dancer copy that `onBeforeMove` stopped (`runMove`'s
        // `BeforeMove`: a confusion self-hit, paralysis, a flinch...) isn't
        // the slot's action.
        if self.ctx.dancing.is_some_and(|d| same_mon(d, actor)) {
            return;
        }
        let Some((si, slot)) = self.slot_index(actor) else { return };
        if self.ctx.acting == Some((si, slot)) {
            self.ctx.acting = None;
        }
        self.ctx.moving = None;
        let r = &mut self.sides[si].this_turn[slot];
        if r.action == TurnAction::None {
            self.ctx.actions = self.ctx.actions.saturating_add(1);
            *r = TurnRecord { action: TurnAction::Cant, order: self.ctx.actions, cant: kind, ..*r };
        }
    }

    /// An Eject Button, Eject Pack, Emergency Exit or Wimp Out sends
    /// `actor` out: its replacement is forced.
    fn exiting(&mut self, actor: Actor) {
        if let Some((si, slot)) = self.slot_index(actor) {
            self.ctx.exiting[si][slot] = true;
        }
    }

    /// A protection while its effect is already up: a Wide Guard or Quick
    /// Guard while its guard is up on the user's side (a second user this
    /// turn), or a Protect or Endure its user repeats in the turn it
    /// worked (Instruct). `addSideCondition` or `addVolatile` fails with
    /// no line, but `onHitSide` or `onHit` adds `stall` all the same, and
    /// `runMoveEffects` combines the two into a success (Showdown's
    /// `battle-actions.js`). Nothing shows it, so the move line decides:
    /// the chain goes on at once. Only `willAct`, or Protect's and
    /// Endure's `stall` roll, can still stop it, with a `-fail`
    /// ([`ApplyCtx::guard_again`](crate::tracker::ApplyCtx)).
    fn guard_again(&mut self, user: Actor, move_id: MoveId, locked: bool) {
        let up = match move_id {
            moves::WIDE_GUARD => {
                self.sides[user.pos.side.index()].conditions[SideCondition::WideGuard as usize].layers > 0
            }
            moves::QUICK_GUARD => {
                self.sides[user.pos.side.index()].conditions[SideCondition::QuickGuard as usize].layers > 0
            }
            moves::ENDURE => self.active(user).is_some_and(|av| av.volatiles.has(V::ENDURE)),
            m if is_stall_move(m) => self.active(user).is_some_and(|av| av.volatiles.has(V::PROTECT)),
            _ => return,
        };
        let Some(av) = self.active(user).filter(|av| !locked && up && av.protect_pending) else { return };
        // The chain as it was, for a `-fail` after (`ApplyCtx::stall`).
        self.ctx.stall =
            slot_of(user).map(|(si, slot)| (si as u8, slot as u8, av.counters.protect_chain, av.protected, false));
        self.protected(user);
        self.ctx.guard_again = Some(user);
    }

    /// A protection of `actor` worked (Protect, Endure, Wide Guard, ...).
    fn protected(&mut self, actor: Actor) {
        if let Some(av) = self.active_mut(actor)
            && av.protect_pending
        {
            av.protect_pending = false;
            av.protected = true;
            // Showdown's `stall` counter stops at `counterMax` (3^6).
            av.counters.protect_chain = (av.counters.protect_chain + 1).min(6);
        }
    }

    fn transform(&mut self, actor: Actor, target: Actor) {
        let Some((ts, tl)) = slot_of(target) else { return };
        let side = &self.sides[ts];
        let Some((row, tmon)) = side.active(tl) else { return };
        let tav = side.slots[tl];
        // `transformInto` copies the types before Terastallization.
        let types = tav.types.unwrap_or_else(|| self.config.dex.species(tmon.species).types);
        let (species, ability) = (tmon.species, tmon.ability);
        let copied: StackVec<MoveId, 4> = if tav.volatiles.has(V::TRANSFORMED) {
            tav.copied_moves
        } else {
            tmon.moves.iter().filter(|m| Some(m.id) != tmon.sketched).map(|m| m.id).take(4).collect()
        };
        if let Some(av) = self.active_mut(actor) {
            // The copied move slots replace Mimic's.
            av.volatiles.clear(V::MIMIC);
            av.mimic_move = None;
            av.volatiles.set(V::TRANSFORMED);
            // `transformInto` (Gen 6+) removes the user's crit volatiles
            // silently and copies the target's (their `[silent]` starts come
            // before the `-transform` line).
            let crit = V::FOCUS_ENERGY | V::DRAGON_CHEER;
            av.volatiles.clear(crit);
            av.volatiles.set(tav.volatiles.0 & crit);
            av.transformed = Some(crate::side::TransformView { side: target.pos.side, row, species });
            av.boosts = tav.boosts;
            av.types = Some(types);
            av.added_type = tav.added_type;
            av.copied_moves = copied;
        }
        self.change_ability(actor, None, ability);
    }

    // -----------------------------------------------------------------
    // HP, status, stages.

    fn hp_report(&mut self, target: Actor, hp: u16, max_hp: u16, status: Status) {
        let shown = self.shown(hp, max_hp);
        // `0 fnt` shows no status (the Pokémon keeps it until it faints).
        let kind = if hp == 0 { StatusKind::None } else { StatusKind::of(status) };
        if let Some(mon) = self.mon_mut(target) {
            mon.hp = shown;
            if kind != mon.status {
                mon.status = kind;
                mon.status_turns = 0;
            }
        }
        if let Some(av) = self.active_mut(target) {
            av.hp = shown;
            av.status = kind;
        }
    }

    fn set_status(&mut self, target: Actor, kind: StatusKind) {
        if let Some(mon) = self.mon_mut(target)
            && mon.status != kind
        {
            mon.status = kind;
            mon.status_turns = 0;
            mon.flags.set(MonFlags::SLEEP_FROM_REST, false);
        }
        if let Some(av) = self.active_mut(target) {
            av.status = kind;
        }
        // Falling asleep ends a rampage (`lockedmove`'s `onResidual`).
        if kind == StatusKind::Sleep
            && let Some(av) = self.active_mut(target)
        {
            av.volatiles.clear(V::LOCKED_MOVE);
            av.locked_move = None;
        }
    }

    fn boost(&mut self, target: Actor, stat: BoostId, by: i8) {
        if let Some(av) = self.active_mut(target) {
            let b = &mut av.boosts[stat as usize];
            *b = b.saturating_add(by).clamp(-6, 6);
        }
    }

    fn set_boost(&mut self, target: Actor, stat: BoostId, stage: i8) {
        if let Some(av) = self.active_mut(target) {
            av.boosts[stat as usize] = stage.clamp(-6, 6);
        }
    }

    fn map_boosts(&mut self, target: Actor, f: impl Fn(i8) -> i8) {
        if let Some(av) = self.active_mut(target) {
            for b in &mut av.boosts {
                *b = f(*b);
            }
        }
    }

    /// `source` copies `target`'s stages (Psych Up, Costar), and its
    /// Focus Energy and Dragon Cheer: its own go silently, `-start
    /// ...|[silent]` lines give it the target's.
    fn copy_boosts(&mut self, source: Actor, target: Actor) {
        const CRIT: u64 = V::FOCUS_ENERGY | V::DRAGON_CHEER;
        if let Some((boosts, crit)) = self.active(target).map(|av| (av.boosts, av.volatiles.0 & CRIT))
            && let Some(av) = self.active_mut(source)
        {
            av.boosts = boosts;
            av.volatiles.clear(CRIT);
            av.volatiles.set(crit);
        }
    }

    /// `-formechange` (a temporary forme): its species, and the forme's
    /// types (`setSpecies` resets a type change).
    fn forme_change(&mut self, actor: Actor, species: SpeciesId) {
        if let Some(mon) = self.mon_mut(actor) {
            mon.species = species;
        }
        if let Some(av) = self.active_mut(actor)
            && !av.volatiles.has(V::TRANSFORMED)
        {
            av.types = None;
            av.added_type = None;
            av.volatiles.clear(V::TYPE_CHANGE | V::TYPE_ADD);
        }
    }

    /// Whether a slot's types were last set by a `[silent]` line: an
    /// Illusion's disguise showing its apparent types, which a `|replace|`
    /// takes back.
    fn note_silent_types(&mut self, target: Actor, silent: bool) {
        if let Some((s, l)) = slot_of(target) {
            self.ctx.silent_types[s][l] = silent;
        }
    }

    /// An announced type change (Soak, Double Shock, Mimicry, ...):
    /// `setType`, which fails on a Terastallized Pokémon (its types stay,
    /// which Transform copies); Double Shock and Burn Up announce it all
    /// the same (an ally transformed into a Slowking Terastallized
    /// Electric after its Double Shock).
    fn change_types(&mut self, target: Actor, types: TypeSet) {
        if self.mon(target).is_some_and(|m| m.flags.has(MonFlags::TERASTALLIZED)) {
            return;
        }
        self.set_types(target, types);
    }

    fn set_types(&mut self, target: Actor, types: TypeSet) {
        if let Some(av) = self.active_mut(target) {
            av.types = Some(types);
            av.added_type = None;
            av.volatiles.set(V::TYPE_CHANGE);
            av.volatiles.clear(V::TYPE_ADD);
        }
    }

    // -----------------------------------------------------------------
    // Volatiles.

    fn effect_start(&mut self, actor: Actor, effect: VolatileEffect, of: Option<Actor>) {
        let (mover, move_target) = (self.ctx.mover, self.ctx.move_target);
        // A partial trap's source: its line's `[of]`, as a side and row.
        let source = of.and_then(|o| self.sides[o.pos.side.index()].row_of(o.id).map(|r| (o.pos.side, r)));
        match effect {
            VolatileEffect::FairyLock => {
                self.field.fairy_lock = true;
                self.field.fairy_lock_turns = 0;
                return;
            }
            VolatileEffect::Endure => {
                self.set_volatile(actor, V::ENDURE);
                self.protected(actor);
                return;
            }
            _ => {}
        }
        let Some(av) = self.active_mut(actor) else { return };
        let c = &mut av.counters;
        match effect {
            VolatileEffect::Substitute => av.volatiles.set(V::SUBSTITUTE),
            VolatileEffect::PartiallyTrapped(m) => {
                av.volatiles.set(V::PARTIALLY_TRAPPED);
                c.partial_trap_turns = 0;
                av.partial_trap = source.map(|(side, row)| (side, row, m));
            }
            VolatileEffect::Trapped => {
                // Trapped by the move's user; Jaw Lock traps its user too,
                // by its target (never by itself: see `jaw_lock_partner`).
                let own = mover.is_some_and(|m| same_mon(m, actor));
                let by = if own { move_target } else { mover };
                if own && by.is_none() {
                    self.jaw_lock_user(actor);
                    return;
                }
                av.volatiles.set(V::TRAPPED);
                av.trapped_by = by.and_then(|b| slot_of(b).map(|(_, l)| (b.pos.side, l as u8)));
                if !own && move_target.is_none() {
                    self.jaw_lock_partner(actor);
                }
            }
            VolatileEffect::NoRetreat => av.volatiles.set(V::NO_RETREAT),
            VolatileEffect::LeechSeed => {
                // Seeded by the move's user, in its slot then (`addVolatile`:
                // `sourceSlot = source.getSlot()`).
                av.volatiles.set(V::LEECH_SEED);
                av.leech_seed_slot = mover.and_then(|m| slot_of(m).map(|(_, l)| (m.pos.side, l as u8)));
            }
            VolatileEffect::Yawn => {
                av.volatiles.set(V::YAWN);
                c.yawn_turns = 0;
            }
            VolatileEffect::Curse => av.volatiles.set(V::CURSE),
            VolatileEffect::PerishSong => av.volatiles.set(V::PERISH_SONG),
            VolatileEffect::DestinyBond => av.volatiles.set(V::DESTINY_BOND),
            VolatileEffect::Attract => av.volatiles.set(V::ATTRACT),
            VolatileEffect::Torment => av.volatiles.set(V::TORMENT),
            VolatileEffect::SaltCure => av.volatiles.set(V::SALT_CURE),
            VolatileEffect::SyrupBomb => {
                av.volatiles.set(V::SYRUP_BOMB);
                c.syrup_bomb_turns = 0;
            }
            VolatileEffect::ThroatChop => {
                av.volatiles.set(V::THROAT_CHOP);
                c.throat_chop_turns = 0;
            }
            VolatileEffect::HealBlock => {
                av.volatiles.set(V::HEAL_BLOCK);
                c.heal_block_turns = 0;
            }
            VolatileEffect::Imprison => av.volatiles.set(V::IMPRISON),
            VolatileEffect::GlaiveRush => av.volatiles.set(V::GLAIVE_RUSH),
            VolatileEffect::SmackDown => {
                // It also ends Magnet Rise and a Fly or Bounce in the air.
                av.volatiles.set(V::SMACK_DOWN);
                av.volatiles.clear(V::MAGNET_RISE | V::CHARGING);
            }
            VolatileEffect::MagnetRise => {
                av.volatiles.set(V::MAGNET_RISE);
                c.magnet_rise_turns = 0;
            }
            // Octolock (`-start|<target>|move: Octolock|[of] <user>`): it
            // keeps the target in while its user is on the field
            // (`onTrapPokemon`), and lasts until its `-end` at a residual
            // after the user left, so it isn't the user's to release
            // (`trapped_by`).
            VolatileEffect::Octolock => {
                av.volatiles.set(V::TRAPPED);
                av.octolocked = true;
            }
            VolatileEffect::Endure | VolatileEffect::FairyLock => {}
        }
    }

    /// Jaw Lock's `onHit` traps its user, then its target, each by the
    /// other (`addVolatile('trapped', ..., 'trapper')`: each one's trap
    /// ends with the other's `clearVolatile`). When `[still]` blanked the
    /// move line's target (a `-fail` later in the move: `|move|<user>|Jaw
    /// Lock||[still]`), the user's line names no partner. It is the only
    /// other Pokémon on the field if there is one; otherwise the user waits
    /// for its target's line (`jaw_lock_partner`). If none follows (the
    /// target was already trapped, a Ghost, or knocked out by the hit), the
    /// view can't tell whose leaving frees the user, so it doesn't mark it
    /// trapped rather than keep a trap nothing would release.
    #[cold]
    #[inline(never)]
    fn jaw_lock_user(&mut self, user: Actor) {
        let mut others = self.sides.iter().enumerate().flat_map(|(s, side)| {
            side.slots.iter().enumerate().filter_map(move |(slot, av)| {
                let shown = av.row.is_some_and(|r| side.mons.get(r as usize).is_some_and(|m| !m.is_fainted()));
                (shown && (s, slot) != (user.pos.side.index(), user.pos.slot as usize)).then_some((s, slot))
            })
        });
        let only = match (others.next(), others.next()) {
            (Some(o), None) => Some(o),
            _ => None,
        };
        match only {
            Some((s, slot)) => {
                if let Some(av) = self.active_mut(user) {
                    av.volatiles.set(V::TRAPPED);
                    av.trapped_by = Some((SideId::BOTH[s], slot as u8));
                }
            }
            None => self.ctx.jaw_lock_pending = true,
        }
    }

    /// A `trapped` line during a Jaw Lock whose line named no target: the
    /// target's, so it is the partner of a user whose own line came first. A
    /// user already trapped showed no line (`addVolatile` fails silently) and
    /// stays trapped by whoever trapped it.
    #[cold]
    #[inline(never)]
    fn jaw_lock_partner(&mut self, target: Actor) {
        let Some(user) = self.ctx.mover.filter(|_| self.ctx.move_id == Some(moves::JAW_LOCK)) else { return };
        if !core::mem::take(&mut self.ctx.jaw_lock_pending) {
            return;
        }
        let Some((_, slot)) = slot_of(target) else { return };
        if let Some(av) = self.active_mut(user) {
            av.volatiles.set(V::TRAPPED);
            av.trapped_by = Some((target.pos.side, slot as u8));
        }
    }

    fn effect_end(&mut self, actor: Actor, effect: VolatileEffect) {
        let Some(av) = self.active_mut(actor) else { return };
        let bit = match effect {
            VolatileEffect::Substitute => V::SUBSTITUTE,
            VolatileEffect::PartiallyTrapped(_) => {
                av.partial_trap = None;
                V::PARTIALLY_TRAPPED
            }
            VolatileEffect::Trapped => {
                av.trapped_by = None;
                if av.octolocked { 0 } else { V::TRAPPED }
            }
            VolatileEffect::NoRetreat => V::NO_RETREAT,
            VolatileEffect::LeechSeed => {
                av.leech_seed_slot = None;
                V::LEECH_SEED
            }
            VolatileEffect::Yawn => V::YAWN,
            VolatileEffect::Curse => V::CURSE,
            VolatileEffect::PerishSong => {
                av.counters.perish = 0;
                V::PERISH_SONG
            }
            VolatileEffect::DestinyBond => V::DESTINY_BOND,
            VolatileEffect::Attract => V::ATTRACT,
            VolatileEffect::Torment => V::TORMENT,
            VolatileEffect::SaltCure => V::SALT_CURE,
            VolatileEffect::SyrupBomb => V::SYRUP_BOMB,
            VolatileEffect::ThroatChop => V::THROAT_CHOP,
            VolatileEffect::HealBlock => V::HEAL_BLOCK,
            VolatileEffect::Imprison => V::IMPRISON,
            VolatileEffect::GlaiveRush => V::GLAIVE_RUSH,
            VolatileEffect::Endure => V::ENDURE,
            VolatileEffect::SmackDown => V::SMACK_DOWN,
            VolatileEffect::MagnetRise => V::MAGNET_RISE,
            // Unless a Block-like trap by a Pokémon still on the field holds
            // it too.
            VolatileEffect::Octolock => {
                av.octolocked = false;
                if av.trapped_by.is_some() { 0 } else { V::TRAPPED }
            }
            VolatileEffect::FairyLock => 0,
        };
        av.volatiles.clear(bit);
    }

    /// `-start` / `-end` lines of abilities and items
    /// ([`BattleEvent::EffectStartEnd`]).
    fn effect_start_end(&mut self, actor: Actor, start: bool, what: LineArg, effect: Option<EffectName>) {
        // An ability's condition ending (`-end|<pokemon>|ability: Flash
        // Fire|[silent]`) may be because the ability just changed.
        if let (Some(e), true) = (effect, start) {
            self.reveal_effect(actor, e);
        }
        match (effect, what) {
            (Some(EffectName::Ability(abilities::FLASH_FIRE)), _) => {
                if start {
                    self.set_volatile(actor, V::FLASH_FIRE);
                } else {
                    self.clear_volatile(actor, V::FLASH_FIRE);
                }
            }
            (Some(EffectName::Ability(abilities::SLOW_START)), _) => {
                if let Some(av) = self.active_mut(actor) {
                    if start {
                        av.volatiles.set(V::SLOW_START);
                        av.counters.slow_start_turns = 0;
                    } else {
                        av.volatiles.clear(V::SLOW_START);
                    }
                }
            }
            (Some(EffectName::Ability(abilities::NEUTRALIZING_GAS)), _) => {
                if !start {
                    self.clear_volatile(actor, V::NEUTRALIZING_GAS);
                }
            }
            (Some(EffectName::Move(moves::ATTRACT)), _) => {
                if !start {
                    self.clear_volatile(actor, V::ATTRACT);
                }
            }
            (None, LineArg::Text(text)) => match text {
                "Attract" if start => self.set_volatile(actor, V::ATTRACT),
                // Mimicry's reset, after `setType`, which ends an added
                // type too (Trick-or-Treat).
                "typechange" if !start => {
                    if let Some(av) = self.active_mut(actor) {
                        av.types = None;
                        av.added_type = None;
                        av.volatiles.clear(V::TYPE_CHANGE | V::TYPE_ADD);
                    }
                }
                "Illusion" if !start => self.set_base_ability(actor, abilities::ILLUSION),
                "Slow Start" if !start => self.clear_volatile(actor, V::SLOW_START),
                _ => {
                    if let Some(stat) = paradox_stat(text) {
                        if let Some(av) = self.active_mut(actor) {
                            av.volatiles.set(V::PARADOX_BOOST);
                            av.boosted_stat = Some(stat);
                        }
                    } else if let Some(n) = text.strip_prefix("fallen") {
                        if let Some(av) = self.active_mut(actor) {
                            av.counters.fallen = if start { n.parse().unwrap_or(0) } else { 0 };
                        }
                    } else if !start
                        && matches!(text, "Protosynthesis" | "Quark Drive")
                        && let Some(av) = self.active_mut(actor)
                    {
                        av.volatiles.clear(V::PARADOX_BOOST);
                        av.boosted_stat = None;
                    }
                }
            },
            _ => {}
        }
    }

    // -----------------------------------------------------------------
    // Abilities and items.

    /// `holder`'s current ability is `ability`; if it hasn't changed since
    /// it came in, that is its own too.
    fn reveal_ability(&mut self, holder: Actor, ability: AbilityId) {
        // As One announces itself with Unnerve, and boosts with Chilling
        // Neigh or Grim Neigh: those are As One's.
        let part_of = |a: AbilityState| match a {
            AbilityState::Known(abilities::AS_ONE_GLASTRIER) => {
                matches!(ability, abilities::UNNERVE | abilities::CHILLING_NEIGH)
            }
            AbilityState::Known(abilities::AS_ONE_SPECTRIER) => {
                matches!(ability, abilities::UNNERVE | abilities::GRIM_NEIGH)
            }
            _ => false,
        };
        if self.mon(holder).is_some_and(|m| part_of(m.ability)) {
            return;
        }
        let changed = self.active(holder).is_some_and(|av| av.ability_changed);
        // Its own ability acting after it changed (Poison Touch's poison
        // lands after Wandering Spirit took it): not its ability now.
        if changed
            && self.mon(holder).is_some_and(|m| {
                m.base_ability == AbilityState::Known(ability) && m.ability != AbilityState::Known(ability)
            })
        {
            return;
        }
        // Nor one it had until a line just took it, a copied one included
        // (a transformed Toxic Chain's poison after Lingering Aroma).
        if self.ctx.ability_lost == Some((holder.pos.side.index() as u8, holder.id, ability)) {
            return;
        }
        let on_field = self.active(holder).is_some();
        if let Some(mon) = self.mon_mut(holder) {
            mon.ability = AbilityState::Known(ability);
            if !changed || !on_field {
                mon.base_ability = AbilityState::Known(ability);
            }
        }
    }

    /// `holder`'s own ability is `ability` (a permanent forme change, the
    /// Illusion that broke); its current one too unless it changed.
    fn set_base_ability(&mut self, holder: Actor, ability: AbilityId) {
        let changed = self.active(holder).is_some_and(|av| av.ability_changed);
        if let Some(mon) = self.mon_mut(holder) {
            mon.base_ability = AbilityState::Known(ability);
            if !changed {
                mon.ability = AbilityState::Known(ability);
            }
        }
    }

    /// `holder`'s ability became `now` (Skill Swap, Mummy, Transform, ...);
    /// `old`, when the line names it, is the one it had.
    fn change_ability(&mut self, holder: Actor, old: Option<AbilityId>, now: AbilityState) {
        self.ctx.ability_lost = old.map(|a| (holder.pos.side.index() as u8, holder.id, a));
        let changed = self.active(holder).is_some_and(|av| av.ability_changed);
        if let Some(mon) = self.mon_mut(holder) {
            if let (Some(old), false) = (old, changed) {
                mon.base_ability = AbilityState::Known(old);
            }
            mon.ability = now;
        }
        if let Some(av) = self.active_mut(holder) {
            av.ability_changed = true;
        }
    }

    /// A line's effect is `holder`'s item. A berry's or a used item's
    /// effect is often shown after its `-enditem` (`-heal|...|[from] item:
    /// Sitrus Berry`): an item the log just showed gone stays gone.
    fn reveal_item(&mut self, holder: Actor, item: ItemId) {
        // An item used up can still show its effect after `-enditem`; one
        // given away (Trick) or taken can come back unannounced (a Sticky
        // Barb moves on contact without a line), so its effect shows it held again.
        let used_up = |how: ItemLoss| {
            matches!(
                how,
                ItemLoss::Consumed | ItemLoss::Eaten | ItemLoss::Flung | ItemLoss::Incinerated | ItemLoss::KnockedOff
            )
        };
        if self.ctx.chewed.is_some_and(|(c, i)| same_mon(c, holder) && i == item) {
            return;
        }
        // Nor one it just used up, whose effect shows after Symbiosis gave
        // it another.
        if self.ctx.item_ended.is_some_and(|(c, i)| same_mon(c, holder) && i == item) {
            return;
        }
        if let Some(mon) = self.mon_mut(holder)
            && !matches!(mon.item, ItemState::Gone { item: Some(i), how } if i == item && used_up(how))
        {
            mon.item = ItemState::Known(item);
        }
    }

    fn reveal_effect(&mut self, holder: Actor, effect: EffectName) {
        match effect {
            EffectName::Ability(a) => self.reveal_ability(holder, a),
            // Fling, Bug Bite and Pluck make another Pokémon's item act on
            // the target or the user (`-boost|<target>|...|[from] item:
            // Liechi Berry` before the thrower's `-enditem`).
            // Only a berry is eaten by Bug Bite or Pluck: the user's Life Orb
            // still shows its own.
            EffectName::Item(i)
                if !(self.ctx.move_id == Some(moves::FLING)
                    || matches!(self.ctx.move_id, Some(moves::BUG_BITE | moves::PLUCK))
                        && self.config.dex.item(i).is_berry) =>
            {
                self.reveal_item(holder, i)
            }
            EffectName::Item(_) | EffectName::Move(_) => {}
        }
    }

    /// `[from] <effect>|[of] <pokemon>` on a line about `target`: the
    /// effect is the `[of]` Pokémon's, else `target`'s.
    fn reveal_cause(&mut self, target: Actor, cause: Cause) {
        self.reveal_effect(cause.of.unwrap_or(target), cause.effect);
    }

    fn ability_show(&mut self, actor: Actor, ability: AbilityId, note: AbilityNote, cause: Option<Cause>) {
        if ability == abilities::UNNERVE && self.ctx.as_one.take().is_some_and(|a| same_mon(a, actor)) {
            return;
        }
        // Ability Shield stopping Receiver or Power of Alchemy shows the
        // effect on the ally that fainted (`onSetAbility`'s `-ability|
        // <source>|<effect>`), then `-block|<holder>|item: Ability
        // Shield`: the ability is the holder's.
        if matches!(ability, abilities::RECEIVER | abilities::POWER_OF_ALCHEMY)
            && cause.is_none()
            && self.mon(actor).is_some_and(|m| m.is_fainted())
        {
            self.ctx.shielded = Some(ability);
            return;
        }
        match (note, cause) {
            // `setAbility`: `-ability|<pokemon>|<new>|<old>|[from] <effect>|[of] <source>`.
            (AbilityNote::Replaced(old), Some(cause)) => {
                if let EffectName::Ability(own) = cause.effect {
                    // Trace, Receiver, Power of Alchemy: its own ability did it.
                    self.reveal_ability(actor, own);
                }
                self.change_ability(actor, Some(old), AbilityState::Known(ability));
                // Trace, Role Play, Entrainment, Doodle copy the `[of]`
                // Pokémon's ability (Receiver's `[of]` has fainted).
                if let Some(of) = cause.of
                    && !matches!(cause.effect, EffectName::Ability(abilities::RECEIVER | abilities::POWER_OF_ALCHEMY))
                {
                    self.reveal_ability(of, ability);
                }
                if ability == abilities::NEUTRALIZING_GAS {
                    self.set_volatile(actor, V::NEUTRALIZING_GAS);
                }
            }
            _ => {
                self.reveal_ability(actor, ability);
                if let Some(c) = cause {
                    self.reveal_cause(actor, c);
                }
                if ability == abilities::NEUTRALIZING_GAS {
                    self.set_volatile(actor, V::NEUTRALIZING_GAS);
                }
            }
        }
    }

    fn skill_swap(&mut self, source: Actor, target: Actor, shown: Option<(AbilityId, AbilityId)>) {
        match shown {
            // `-activate|<source>|Skill Swap|<target's>|<source's>|[of] <target>`.
            Some((theirs, ours)) => {
                self.change_ability(source, Some(ours), AbilityState::Known(theirs));
                self.change_ability(target, Some(theirs), AbilityState::Known(ours));
            }
            // Between allies nothing is shown: they swap what was known.
            None => {
                let s = self.mon(source).map_or(AbilityState::Unknown, |m| m.ability);
                let t = self.mon(target).map_or(AbilityState::Unknown, |m| m.ability);
                self.change_ability(source, None, t);
                self.change_ability(target, None, s);
            }
        }
    }

    /// `receiver` got another Pokémon's item, before its record shows it
    /// ([`MonView::own_item`]): its own item is `own` (what a trade's other
    /// line showed it held), else the one its record shows it held or lost
    /// (not after `used`: an item it used up at once replaced that). It
    /// holds it from now (a Choice item locks it only from its next move).
    fn receive_item(&mut self, receiver: Actor, own: Option<ItemId>, used: bool) {
        if let Some(mon) = self.mon_mut(receiver)
            && mon.own_item == OwnItem::Held
        {
            let shown = if used { None } else { OwnItem::Held.of(mon.item) };
            mon.own_item = OwnItem::Moved(own.or(shown));
        }
        if let Some(av) = self.active_mut(receiver) {
            av.item_received = true;
            av.choice_move = None;
        }
    }

    /// Trick or Switcheroo between `user` and `target` (`-activate|<user>|
    /// move: Trick|[of] <target>`): their items' lines follow.
    fn start_swap(&mut self, user: Actor, target: Actor) {
        let held = |a| self.mon(a).is_none_or(|m| m.own_item == OwnItem::Held);
        self.ctx.swap = Some(ItemSwap { pair: [user, target], held: [held(user), held(target)], before: [None, None] });
    }

    /// A Trick's `-item|<receiver>|<item>`: the other Pokémon held `item`
    /// before the trade, which is its own if it held its own (and, if its
    /// line came first, it got its new item already: its own is now
    /// known). Returns what `receiver` held before, if the other line
    /// came first.
    fn swap_received(&mut self, receiver: Actor, item: ItemId) -> Option<ItemId> {
        let mut swap = self.ctx.swap?;
        let k = swap.pair.iter().position(|&a| same_mon(a, receiver))?;
        let giver = swap.pair[1 - k];
        swap.before[1 - k] = Some(item);
        self.ctx.swap = Some(swap);
        if swap.held[1 - k]
            && let Some(mon) = self.mon_mut(giver)
            && matches!(mon.own_item, OwnItem::Moved(_))
        {
            mon.own_item = OwnItem::Moved(Some(item));
        }
        swap.before[k]
    }

    /// A Trick's `-enditem|<pokemon>|<item>`: it held `item` before the
    /// trade (and got nothing back).
    fn swap_before(&mut self, actor: Actor, item: ItemId) {
        if let Some(swap) = self.ctx.swap.as_mut()
            && let Some(k) = swap.pair.iter().position(|&a| same_mon(a, actor))
        {
            swap.before[k] = Some(item);
        }
    }

    fn item_show(&mut self, actor: Actor, item: ItemId, cause: Option<Cause>) {
        // Trick's and Switcheroo's `-item` comes after the received item
        // already acted (a White Herb used at once): it is gone.
        // So does Magician's and Pickpocket's (a White Herb stolen and used
        // at once), and Thief's and Covet's; the victim lost it all the
        // same.
        let traded = cause.is_some_and(|c| {
            matches!(
                c.effect,
                EffectName::Move(moves::TRICK | moves::SWITCHEROO | moves::THIEF | moves::COVET)
                    | EffectName::Ability(abilities::MAGICIAN | abilities::PICKPOCKET)
            )
        });
        let used = traded && self.ctx.item_ended.is_some_and(|(a, i)| same_mon(a, actor) && i == item);
        // Another's item came to it: Trick, Switcheroo (`trick.onHit`,
        // `switcheroo.onHit`), Thief, Covet (`onAfterHit`), Bestow
        // (`bestow.onHit`), Magician, Pickpocket, Pickup (`data/moves.js`,
        // `data/abilities.js`; each `setItem`s what another's `takeItem`
        // gave, or Pickup another's `lastItem`). What it holds is no longer
        // its own item.
        let received = cause.is_some_and(|c| {
            matches!(
                c.effect,
                EffectName::Move(moves::TRICK | moves::SWITCHEROO | moves::THIEF | moves::COVET | moves::BESTOW)
                    | EffectName::Ability(abilities::MAGICIAN | abilities::PICKPOCKET | abilities::PICKUP)
            )
        });
        if received {
            // A trade's other line may have shown what it held before.
            let swapped = cause.is_some_and(|c| matches!(c.effect, EffectName::Move(moves::TRICK | moves::SWITCHEROO)));
            let own = if swapped { self.swap_received(actor, item) } else { None };
            self.receive_item(actor, own, used);
        }
        // `-item`: it holds `item` now, whatever it held before.
        if !used && let Some(mon) = self.mon_mut(actor) {
            mon.item = ItemState::Known(item);
        }
        let Some(cause) = cause else { return };
        match cause.effect {
            EffectName::Ability(a) => {
                let holder = if a == abilities::FRISK { cause.of.unwrap_or(actor) } else { actor };
                self.reveal_ability(holder, a);
                if matches!(a, abilities::MAGICIAN | abilities::PICKPOCKET)
                    && let Some(of) = cause.of
                    && let Some(mon) = self.mon_mut(of)
                {
                    mon.item = ItemState::Gone { item: Some(item), how: ItemLoss::Stolen };
                }
            }
            EffectName::Move(moves::THIEF | moves::COVET) => {
                if let Some(of) = cause.of
                    && let Some(mon) = self.mon_mut(of)
                {
                    mon.item = ItemState::Gone { item: Some(item), how: ItemLoss::Stolen };
                }
            }
            EffectName::Move(_) | EffectName::Item(_) => {}
        }
    }

    fn activate_effect(&mut self, actor: Actor, effect: EffectName, args: [Option<LineArg>; 2], of: Option<Actor>) {
        let consumed = args.contains(&Some(LineArg::Text("[consumed]")));
        // Fling, Bug Bite and Pluck: another's item acts on this Pokémon.
        let borrowed = matches!(self.ctx.move_id, Some(moves::FLING | moves::BUG_BITE | moves::PLUCK));
        match effect {
            EffectName::Item(item) if borrowed => {
                if let (Some(LineArg::Move(m)), true) = (args[0], consumed) {
                    self.reveal_move(actor, m);
                    if item == items::LEPPA_BERRY {
                        self.leppa(actor, m);
                    }
                }
            }
            EffectName::Item(item) => {
                // Its `-enditem` came first, and Symbiosis may have passed
                // it another since (Custap Berry).
                let ended = self.ctx.item_ended.is_some_and(|(c, i)| same_mon(c, actor) && i == item);
                if consumed {
                    if !ended
                        && let Some(mon) = self.mon_mut(actor)
                        && !matches!(mon.item, ItemState::Gone { .. })
                    {
                        mon.item = ItemState::Gone { item: Some(item), how: ItemLoss::Eaten };
                    }
                } else {
                    self.reveal_item(actor, item);
                }
                // Leppa Berry names the move it restores (`[consumed]`);
                // Safety Goggles the attacker's.
                if let (Some(LineArg::Move(m)), true) = (args[0], consumed) {
                    self.reveal_move(actor, m);
                    if item == items::LEPPA_BERRY {
                        self.leppa(actor, m);
                    }
                }
            }
            EffectName::Ability(a) => {
                self.reveal_ability(actor, a);
                match a {
                    abilities::FOREWARN => {
                        if let (Some(LineArg::Move(m)), Some(of)) = (args[0], of) {
                            self.reveal_move(of, m);
                        }
                    }
                    abilities::SYMBIOSIS => {
                        if let (Some(LineArg::Item(item)), Some(of)) = (args[0], of) {
                            if let Some(mon) = self.mon_mut(actor) {
                                mon.item = ItemState::Gone { item: Some(item), how: ItemLoss::Traded };
                            }
                            // `symbiosis.onAllyAfterUseItem`: the ally that
                            // used up its own item gets this one. The line
                            // names it, so it is known even when it is the
                            // item the ally just ate (two Maranga Berries:
                            // `reveal_item` keeps that one gone).
                            self.receive_item(of, None, false);
                            if let Some(mon) = self.mon_mut(of) {
                                mon.item = ItemState::Known(item);
                            }
                        }
                    }
                    abilities::COMMANDER => {
                        if let Some(of) = of {
                            self.set_volatile(actor, V::COMMANDING);
                            self.set_volatile(of, V::COMMANDED);
                        }
                    }
                    _ => {}
                }
            }
            EffectName::Move(moves::POLTERGEIST) => {
                if let Some(LineArg::Item(item)) = args[0] {
                    self.reveal_item(actor, item);
                }
            }
            // `-activate|<user>|move: Trick|[of] <target>` (Switcheroo's
            // too): their items' lines follow.
            EffectName::Move(moves::TRICK | moves::SWITCHEROO) => {
                if let Some(of) = of {
                    self.start_swap(actor, of);
                }
            }
            EffectName::Move(_) => {}
        }
    }
}

/// [`TurnRecord::effect`] of `-immune`.
const IMMUNE: i8 = -3;

/// The stages a boost line's amount changes (at most 12: -6 to +6; an
/// event's `u8` is never read as negative).
fn stages(amount: u8) -> i8 {
    amount.min(12) as i8
}

/// A record takes the status another slot showed for it (an Illusion shows
/// one Pokémon in both slots): the turns counted under another status
/// don't carry over.
fn set_status_from(mon: &mut MonView, status: StatusKind) {
    if mon.status != status {
        mon.status_turns = 0;
        mon.flags.set(MonFlags::SLEEP_FROM_REST, false);
    }
    mon.status = status;
}

/// Lists `row` in a side's `reveal_order`, once (a hostile channel can
/// reveal a record again: stray `|replace|` lines).
fn list_revealed(order: &mut StackVec<u8, MAX_TEAM>, row: u8) {
    if !order.contains(&row) && !order.is_full() {
        order.push(row);
    }
}

/// What a record keeps when its Pokémon leaves the field (Showdown's
/// `clearVolatile`): its species and ability go back to its own.
fn leave_field(mon: &mut MonView) {
    mon.species = mon.details_species;
    mon.ability = mon.base_ability;
}

/// The facts a disguise's record learnt since `before` (its record when
/// the Illusion came in), given to the Pokémon behind it. `shared`: the
/// disguise's Pokémon was on the field too, in the other slot.
fn moved_facts(mon: &mut MonView, shown: &MonView, before: &MonView, shared: bool) {
    mon.hp = shown.hp;
    // Its status, and the attempts it made asleep: a sleep it already had
    // (it can't wake on the bench) goes on counting the attempts its own
    // record saw before, and Rest's still is; the disguise's record counted
    // those made since the switch-in (from 0 when the switch-in changed its
    // status, else from what it had). Otherwise the status came since, with
    // what the disguise's record showed of it.
    let asleep = |m: &MonView| m.status == StatusKind::Sleep;
    if asleep(mon) && asleep(shown) {
        let since =
            if asleep(before) { shown.status_turns.saturating_sub(before.status_turns) } else { shown.status_turns };
        mon.status_turns = mon.status_turns.saturating_add(since);
    } else {
        mon.status = shown.status;
        mon.status_turns = shown.status_turns;
        let rest = asleep(shown) && shown.flags.has(MonFlags::SLEEP_FROM_REST);
        mon.flags.set(MonFlags::SLEEP_FROM_REST, rest);
    }
    for m in shown.moves.iter() {
        let (uses, pp) = before.revealed(m.id).map_or((0, 0), |b| (b.uses, b.pp_spent));
        let known_before = before.revealed(m.id).is_some();
        // Unused since, with PP given back (a Lunar Dance or Leppa Berry
        // the Pokémon behind the disguise took): not its move.
        if m.uses <= uses && m.pp_spent <= pp && known_before {
            continue;
        }
        let (du, dp) = (m.uses.saturating_sub(uses), m.pp_spent.saturating_sub(pp));
        match mon.moves.iter().position(|x| x.id == m.id) {
            Some(i) => {
                let x = &mut mon.moves[i];
                x.uses = x.uses.saturating_add(du);
                x.pp_spent = x.pp_spent.saturating_add(dp);
            }
            None if !mon.moves.is_full() => mon.moves.push(RevealedMove { id: m.id, uses: du, pp_spent: dp }),
            None => {}
        }
    }
    if shown.item != before.item {
        mon.item = shown.item;
    }
    if shown.own_item != before.own_item {
        mon.own_item = shown.own_item;
    }
    if shown.ability != before.ability {
        mon.ability = shown.ability;
    }
    if shown.base_ability != before.base_ability {
        mon.base_ability = shown.base_ability;
    }
    // Whether it is Terastallized is the slot's: the switch-in's details
    // showed the Illusion user's (`tera:`), and `-terastallize` since
    // (Illusion keeps the disguise's species, not its Tera). A record
    // that was Terastallized before (an earlier disguise) says nothing.
    // `|replace|` doesn't show it (`getUpdatedDetails`). Unless the record was
    // shared with the Pokémon it shows, whose Tera it may be.
    let tera_before = shared && before.flags.has(MonFlags::TERASTALLIZED);
    if shown.flags.has(MonFlags::TERASTALLIZED) && !tera_before {
        mon.flags.set(MonFlags::TERASTALLIZED, true);
        mon.tera_type = shown.tera_type;
    }
}

/// The side index and slot of an actor standing in a slot.
pub(crate) fn slot_of(actor: Actor) -> Option<(usize, usize)> {
    let slot = actor.pos.slot as usize;
    (actor.active && slot < MAX_ACTIVE).then_some((actor.pos.side.index(), slot))
}

/// Whether two actors name the same Pokémon: the same slot for Pokémon
/// on the field (an Illusion can show one name in both slots), else the
/// same name.
fn same_mon(a: Actor, b: Actor) -> bool {
    if a.active && b.active { a.pos == b.pos } else { a.pos.side == b.pos.side && a.id == b.id }
}

fn manipulation_bit(v: ManipulationVolatile) -> u64 {
    match v {
        ManipulationVolatile::AquaRing => V::AQUA_RING,
        ManipulationVolatile::Ingrain => V::INGRAIN,
        ManipulationVolatile::Charge => V::CHARGE,
        ManipulationVolatile::FocusEnergy => V::FOCUS_ENERGY,
        ManipulationVolatile::DragonCheer => V::DRAGON_CHEER,
        ManipulationVolatile::TarShot => V::TAR_SHOT,
        ManipulationVolatile::Stockpile(_) => V::STOCKPILE,
        ManipulationVolatile::PowerTrick => V::POWER_TRICK,
        ManipulationVolatile::PowerShift => V::POWER_SHIFT,
    }
}

/// The stat of a `protosynthesis<stat>` / `quarkdrive<stat>` line.
fn paradox_stat(text: &str) -> Option<BoostId> {
    let stat = text.strip_prefix("protosynthesis").or_else(|| text.strip_prefix("quarkdrive"))?;
    Some(match stat {
        "atk" => BoostId::Atk,
        "def" => BoostId::Def,
        "spa" => BoostId::SpA,
        "spd" => BoostId::SpD,
        "spe" => BoostId::Spe,
        _ => return None,
    })
}
