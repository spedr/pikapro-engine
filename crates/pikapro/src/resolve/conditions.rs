//! Substitute, trapping and residual volatiles.
//!
//! Showdown references: the `substitute`, `shedtail`, `block`, `meanlook`,
//! `spiritshackle`, `jawlock`, `noretreat`, `fairylock`, `leechseed`,
//! `yawn`, `curse`, `perishsong`, `destinybond`, `attract`, `torment`,
//! `saltcure`, `syrupbomb`, `throatchop`, `healblock`, `imprison`,
//! `glaiverush`, `endure`, `smackdown` and `magnetrise` moves and their
//! conditions in `data/moves.ts` (Salt Cure's change in
//! `data/mods/champions/moves.ts`); `trapped` and `partiallytrapped` in
//! `data/conditions.ts`; `Pokemon.addVolatile` / `removeVolatile` /
//! `tryTrap` / `isGrounded` / `copyVolatileFrom` / `clearVolatile` in
//! `sim/pokemon.ts`; `Battle.endTurn`'s `DisableMove` and `TrapPokemon`
//! events, `faintMessages`' `Faint` event, `spreadDamage`'s `Damage` event
//! and `heal`'s `TryHeal` event in `sim/battle.ts`; `tryPrimaryHitEvent` in
//! `sim/battle-actions.ts`.
//!
//! Contract (hooks called at Showdown's call sites):
//! - `volatiles::add_volatile` routes these IDs here: [`flag_of`],
//!   [`restart`], [`status_immune`], [`start`]; [`remove`] is their
//!   `removeVolatile` (with `onEnd`).
//! - handlers: [`before_move_handlers`], [`residual_handlers`],
//!   [`disabled_by`] and [`disable_move_handlers`] (`DisableMove`),
//!   [`trap_pokemon_event`] (`TrapPokemon`), [`source_left_update`]
//!   (`Update`).
//! - move pipeline: [`handles`], [`modify_move`] (Curse), [`try_move`],
//!   [`prepare_hit`], [`try_hit`], [`try_immunity`], [`try_primary_hit`]
//!   (a substitute taking the hit), [`has_on_hit`] / [`on_hit`], [`secondary_on_hit`], [`hit_field`]
//!   (Perish Song), [`add_fairy_lock`], [`move_aborted`].
//! - damage and healing: [`endure_damage`] (Endure), [`on_move_ko`] and
//!   [`on_faint`] (Destiny Bond), [`heal_block_try_heal`] (Heal Block),
//!   [`glaive_rush_source_damage`] and [`glaive_rush_accuracy`] (Glaive Rush).
//! - switching: [`switch_out`] / [`switched_in`] (linked traps, Shed
//!   Tail's substitute).
//! - requests: [`is_trapped`], [`request_target`] (Curse).
//!
//! State: presence bits in [`Volatiles`] (bits 16..=35 and Octolock's
//! 124), durations and
//! sources in [`VolatileCounters`], Fairy Lock in
//! [`FieldState::fairy_lock_turns`](crate::state::FieldState).
//!
//! Showdown's `trapper` volatile (the other end of a `trapped` link) is not
//! stored: `trapped` records its trapper, and the link is released when the
//! trapper leaves the field ([`switch_out`]), which is all `trapper` does.

use pikapro_data::Type;
use pikapro_data::gen9::{abilities, moves as ids};
use pikapro_data::{MoveCategory, MoveData, MoveFlags, MoveId, MoveTarget, StatBoosts, StatusId};

use super::handlers::{Callback, Handler, HandlerFn, Handlers, Holder, sub_order};
use super::manipulation::has_type;
use super::queue::speed_sort;
use super::targeting::Targets;
use super::{HealSource, actor, apply_heal, boosts, direct_damage, field, moves, status, switching, volatiles};
use crate::battle::Battle;
use crate::event::{Actor, BattleEvent, FailNote, VolatileEffect};
use crate::format::Mechanics;
use crate::state::{PokemonState, Position, PseudoWeather, SideId, Status, VolatileCounters, Volatiles};
use crate::team::Gender;

/// A Pokémon by side and stable [`PokemonState::id`] (Showdown's effect
/// `source`, which follows the Pokémon, not its slot).
type MonKey = (SideId, u8);

fn key(battle: &Battle, pos: Position) -> MonKey {
    (pos.side, battle.state.side(pos.side).team[pos.slot as usize].id)
}

/// Where `key` stands, if it is active (Showdown's `isActive`: in a slot,
/// its faint not reported yet).
fn active_at(battle: &Battle, key: MonKey) -> Option<Position> {
    let side = battle.state.side(key.0);
    let i = side.position_of(key.1)?;
    (i < battle.format.active_per_side() && side.active[i].is_some()).then(|| Position::new(key.0, i as u8))
}

/// The Pokémon `key` as an event names it: by its slot when active, by its
/// side otherwise (`p1: Name`).
fn actor_of(battle: &Battle, key: MonKey) -> Actor {
    match active_at(battle, key) {
        Some(pos) => actor(battle, pos),
        None => {
            let side = battle.state.side(key.0);
            // A benched Pokémon keeps its Illusion.
            let seen = side.position_of(key.1).map(|i| &side.team[i]);
            let species = seen.map_or(Default::default(), |m| super::abilities::switchin::seen_species(side, m));
            let id = seen.map_or(key.1, |m| super::abilities::switchin::seen_id(side, m));
            Actor { pos: Position::new(key.0, 0), species, active: false, id }
        }
    }
}

fn mon(battle: &Battle, pos: Position) -> &PokemonState {
    battle.state.at(pos).expect("volatile holder is active")
}

fn mon_mut(battle: &mut Battle, pos: Position) -> &mut PokemonState {
    battle.state.at_mut(pos).expect("volatile holder is active")
}

fn has(battle: &Battle, pos: Position, flag: u128) -> bool {
    battle.state.at(pos).is_some_and(|m| m.volatiles.has(flag))
}

fn emit_start(battle: &mut Battle, pos: Position, effect: VolatileEffect, of: Option<Position>) {
    let (actor, of) = (actor(battle, pos), of.map(|p| actor(battle, p)));
    battle.emit(BattleEvent::EffectStart { actor, effect, of, from: None });
}

/// Status moves whose effect this module implements beyond their data.
pub(crate) fn handles(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::SUBSTITUTE
            | ids::SHED_TAIL
            | ids::BLOCK
            | ids::MEAN_LOOK
            | ids::NO_RETREAT
            | ids::FAIRY_LOCK
            | ids::LEECH_SEED
            | ids::YAWN
            | ids::CURSE
            | ids::PERISH_SONG
            | ids::DESTINY_BOND
            | ids::ATTRACT
            | ids::TORMENT
            | ids::IMPRISON
            | ids::ENDURE
            | ids::MAGNET_RISE
            // Its `octolock` volatile (Champions).
            | ids::OCTOLOCK
    )
}

/// The volatile flag of a Showdown condition ID implemented here.
pub(crate) fn flag_of(id: &str) -> Option<u128> {
    Some(match id {
        "substitute" => Volatiles::SUBSTITUTE,
        "partiallytrapped" => Volatiles::PARTIALLY_TRAPPED,
        "trapped" => Volatiles::TRAPPED,
        "noretreat" => Volatiles::NO_RETREAT,
        "leechseed" => Volatiles::LEECH_SEED,
        "yawn" => Volatiles::YAWN,
        "curse" => Volatiles::CURSE,
        "perishsong" => Volatiles::PERISH_SONG,
        "destinybond" => Volatiles::DESTINY_BOND,
        "attract" => Volatiles::ATTRACT,
        "torment" => Volatiles::TORMENT,
        "saltcure" => Volatiles::SALT_CURE,
        "syrupbomb" => Volatiles::SYRUP_BOMB,
        "throatchop" => Volatiles::THROAT_CHOP,
        "healblock" => Volatiles::HEAL_BLOCK,
        "imprison" => Volatiles::IMPRISON,
        "glaiverush" => Volatiles::GLAIVE_RUSH,
        "endure" => Volatiles::ENDURE,
        "smackdown" => Volatiles::SMACK_DOWN,
        "magnetrise" => Volatiles::MAGNET_RISE,
        "octolock" => Volatiles::OCTOLOCK,
        _ => return None,
    })
}

/// `addVolatile` on a Pokémon that already has the volatile: its
/// `onRestart`, or failure without one. Heal Block's and Smack Down's
/// return nothing, which counts as success.
pub(crate) fn restart(battle: &mut Battle, target: Position, flag: u128) -> Option<bool> {
    match flag {
        Volatiles::HEAL_BLOCK => {
            // Psychic Noise restarts it quietly (without extending it).
            if battle.active_move.is_some_and(|(_, m)| m != ids::PSYCHIC_NOISE) {
                let actor = actor(battle, target);
                battle.emit(BattleEvent::MoveFail { user: actor, move_id: ids::HEAL_BLOCK, note: FailNote::None });
            }
            Some(true)
        }
        // `onRestart`: knocks a Pokémon using Fly or Bounce down again.
        Volatiles::SMACK_DOWN => {
            if super::multiturn::knock_down(battle, target) {
                emit_start(battle, target, VolatileEffect::SmackDown, None);
            }
            Some(true)
        }
        _ => Some(false),
    }
}

/// `runStatusImmunity(id)` in `addVolatile`: Ghost types can't be trapped
/// (the type chart's `trapped` immunity). Silent.
pub(crate) fn status_immune(battle: &Battle, target: Position, flag: u128) -> bool {
    flag == Volatiles::TRAPPED && has_type(mon(battle, target), Type::Ghost)
}

/// The volatile's `durationCallback` / `duration` and `onStart`, once
/// `TryAddVolatile` let it through. `Some(false)` means `onStart` failed;
/// `None` that the volatile is not added at all (`null`). The caller sets
/// the presence bit on success.
/// `from_ability`: the ability that added it (Cute Charm's Attract says
/// so).
pub(crate) fn start(
    battle: &mut Battle,
    target: Position,
    flag: u128,
    source: Option<Position>,
    from_ability: Option<crate::event::Cause>,
) -> Option<bool> {
    let source = source.unwrap_or(target);
    let move_id = battle.active_move.map(|(_, m)| m);
    match flag {
        Volatiles::SUBSTITUTE => {
            let from = (move_id == Some(ids::SHED_TAIL)).then_some(ids::SHED_TAIL);
            let a = actor(battle, target);
            battle.emit(BattleEvent::EffectStart { actor: a, effect: VolatileEffect::Substitute, of: None, from });
            let m = mon_mut(battle, target);
            // `effectState.hp = Math.floor(target.maxhp / 4)`.
            m.volatile_counters.substitute_hp_halves = 2 * (m.max_hp() / 4);
            // A substitute frees the Pokémon from a partial trap (silently).
            if m.volatiles.has(Volatiles::PARTIALLY_TRAPPED) {
                m.volatiles.clear(Volatiles::PARTIALLY_TRAPPED);
                let effect = VolatileEffect::PartiallyTrapped(m.volatile_counters.partially_trapped_move);
                battle.emit(BattleEvent::EffectEnd { actor: a, effect, silent: true });
            }
        }
        Volatiles::PARTIALLY_TRAPPED => {
            let Some(m) = move_id else { return Some(false) };
            // `durationCallback`: Grip Claw makes it 8 turns.
            let duration = match super::items::held::binding_duration(battle, source) {
                Some(d) => d,
                None => battle.rng.random_range(5, 7) as u8,
            };
            let (a, of) = (actor(battle, target), actor(battle, source));
            let effect = VolatileEffect::PartiallyTrapped(m);
            battle.emit(BattleEvent::EffectStart { actor: a, effect, of: Some(of), from: None });
            let by = key(battle, source);
            // `boundDivisor`: Binding Band.
            let divisor = super::items::held::binding_divisor(battle, source) as u8;
            let c = &mut mon_mut(battle, target).volatile_counters;
            (c.partially_trapped, c.partially_trapped_move, c.partially_trapped_by) = (duration, m, by);
            c.bound_divisor = divisor;
        }
        Volatiles::TRAPPED => {
            emit_start(battle, target, VolatileEffect::Trapped, None);
            let by = key(battle, source);
            // `source.addVolatile('trapper', this, sourceEffect)` unless it
            // already traps someone: `runStatusImmunity` and the
            // `TryAddVolatile` event on the trapper (nothing stops it, but
            // two Veil abilities' tie draws).
            let trapping = SideId::BOTH.into_iter().any(|side| {
                battle.state.side(side).team.iter().enumerate().any(|(i, m)| {
                    (side, i as u8) != (target.side, target.slot)
                        && !m.is_fainted()
                        && m.volatiles.has(Volatiles::TRAPPED)
                        && m.volatile_counters.trapped_by == by
                        && active_at(battle, (side, m.id)).is_some()
                })
            });
            if !trapping {
                let _ = super::events::immunity_event(battle, source, "trapper");
                let effect = move_id.map_or(super::events::Effect::None, super::events::Effect::Move);
                let mut ev = super::events::EventCtx::new(
                    pikapro_data::Event::TryAddVolatile,
                    super::events::Target::Pokemon(source),
                    Some(target),
                    effect,
                    super::events::Relay::Id("trapper"),
                );
                super::events::run_event(battle, &mut ev, None);
            }
            mon_mut(battle, target).volatile_counters.trapped_by = by;
        }
        Volatiles::NO_RETREAT => emit_start(battle, target, VolatileEffect::NoRetreat, None),
        Volatiles::LEECH_SEED => {
            emit_start(battle, target, VolatileEffect::LeechSeed, None);
            mon_mut(battle, target).volatile_counters.leech_seed_slot = source;
        }
        Volatiles::YAWN => {
            let counters = &mut mon_mut(battle, target).volatile_counters;
            counters.yawn = 2;
            counters.yawn_source = source;
            emit_start(battle, target, VolatileEffect::Yawn, Some(source));
        }
        Volatiles::CURSE => emit_start(battle, target, VolatileEffect::Curse, Some(source)),
        // Perish Song's `onHitField` announces it.
        Volatiles::PERISH_SONG => mon_mut(battle, target).volatile_counters.perish_song = 4,
        Volatiles::DESTINY_BOND => emit_start(battle, target, VolatileEffect::DestinyBond, None),
        Volatiles::ATTRACT => {
            // Cute Charm passes `from_ability`; Destiny Knot uses `attract_from`.
            if !opposite_genders(battle, target, source) {
                return Some(false);
            }
            // `runEvent('Attract', pokemon, source)` (Oblivious, Aroma Veil,
            // Destiny Knot). The volatile is present during its `onStart`.
            mon_mut(battle, target).volatiles.add(Volatiles::ATTRACT);
            if !super::run_mon_event_from(battle, pikapro_data::Event::Attract, target, Some(source)).is_truthy() {
                mon_mut(battle, target).volatiles.clear(Volatiles::ATTRACT);
                return Some(false);
            }
            match from_ability {
                // Cute Charm: `-start|<target>|Attract|[from] ability: Cute
                // Charm|[of] <source>`.
                Some(cause) => {
                    let a = actor(battle, target);
                    battle.emit(BattleEvent::StartFrom { actor: a, what: "Attract", arg: None, cause });
                }
                None => emit_start(battle, target, VolatileEffect::Attract, None),
            }
            let by = key(battle, source);
            mon_mut(battle, target).volatile_counters.attract_by = by;
        }
        Volatiles::TORMENT => emit_start(battle, target, VolatileEffect::Torment, None),
        Volatiles::SALT_CURE => emit_start(battle, target, VolatileEffect::SaltCure, None),
        Volatiles::SYRUP_BOMB => {
            let by = key(battle, source);
            let c = &mut mon_mut(battle, target).volatile_counters;
            (c.syrup_bomb, c.syrup_bomb_by) = (4, by);
            emit_start(battle, target, VolatileEffect::SyrupBomb, None);
        }
        Volatiles::THROAT_CHOP => {
            mon_mut(battle, target).volatile_counters.throat_chop = 2;
            emit_start(battle, target, VolatileEffect::ThroatChop, None);
        }
        Volatiles::HEAL_BLOCK => {
            // `durationCallback`: 2 turns from Psychic Noise. (Persistent's 7
            // turns: a CAP ability.)
            let turns = if move_id == Some(ids::PSYCHIC_NOISE) { 2 } else { 5 };
            mon_mut(battle, target).volatile_counters.heal_block = turns;
            emit_start(battle, target, VolatileEffect::HealBlock, None);
        }
        Volatiles::IMPRISON => emit_start(battle, target, VolatileEffect::Imprison, None),
        Volatiles::GLAIVE_RUSH => emit_start(battle, target, VolatileEffect::GlaiveRush, None),
        Volatiles::ENDURE => emit_start(battle, target, VolatileEffect::Endure, None),
        Volatiles::SMACK_DOWN => {
            let m = *mon(battle, target);
            // `hasAbility(['levitate', 'eelevate'])`.
            let mut applies = has_type(&m, Type::Flying)
                || super::abilities::has_ability(battle, target, abilities::LEVITATE)
                || super::abilities::has_ability(battle, target, abilities::EELEVATE);
            // Already grounded by Iron Ball, Ingrain or Gravity.
            if super::items::has_item(battle, target, pikapro_data::gen9::items::IRON_BALL)
                || m.volatiles.has(Volatiles::INGRAIN)
                || super::gravity(battle)
            {
                applies = false;
            }
            // Knocking down a Fly or Bounce user.
            if super::multiturn::knock_down(battle, target) {
                applies = true;
            }
            // (Telekinesis would end too: a Past move in Gen 9 and Champions.)
            if m.volatiles.has(Volatiles::MAGNET_RISE) {
                applies = true;
                // `delete pokemon.volatiles['magnetrise']`: no `-end`.
                mon_mut(battle, target).volatiles.clear(Volatiles::MAGNET_RISE);
            }
            if !applies {
                return Some(false);
            }
            emit_start(battle, target, VolatileEffect::SmackDown, None);
        }
        Volatiles::MAGNET_RISE => {
            mon_mut(battle, target).volatile_counters.magnet_rise = 5;
            emit_start(battle, target, VolatileEffect::MagnetRise, None);
        }
        // Octolock's `onStart` (`data/moves.ts` octolock): `-start|<target>|
        // move: Octolock|[of] <source>`; `effectState.source` is the user.
        Volatiles::OCTOLOCK => {
            emit_start(battle, target, VolatileEffect::Octolock, Some(source));
            let by = key(battle, source);
            mon_mut(battle, target).volatile_counters.octolock_by = by;
        }
        _ => return Some(false),
    }
    Some(true)
}

/// Showdown's `removeVolatile` for these volatiles: `onEnd` (the `-end`
/// message, Yawn's sleep, Perish Song's faint). Returns whether it was
/// present. A fainted Pokémon keeps its volatiles (`if (!this.hp)`).
pub(crate) fn remove(battle: &mut Battle, pos: Position, flag: u128) -> bool {
    let Some(m) = battle.state.at_mut(pos) else { return false };
    if m.is_fainted() || !m.volatiles.has(flag) {
        return false;
    }
    m.volatiles.clear(flag);
    if flag == Volatiles::SUBSTITUTE {
        // Its HP goes with it (so equal positions compare equal).
        m.volatile_counters.substitute_hp_halves = 0;
    }
    let counters = m.volatile_counters;
    let effect = match flag {
        Volatiles::SUBSTITUTE => VolatileEffect::Substitute,
        Volatiles::PARTIALLY_TRAPPED => VolatileEffect::PartiallyTrapped(counters.partially_trapped_move),
        Volatiles::YAWN => VolatileEffect::Yawn,
        Volatiles::ATTRACT => VolatileEffect::Attract,
        Volatiles::TORMENT => VolatileEffect::Torment,
        Volatiles::SALT_CURE => VolatileEffect::SaltCure,
        Volatiles::SYRUP_BOMB => VolatileEffect::SyrupBomb,
        Volatiles::THROAT_CHOP => VolatileEffect::ThroatChop,
        Volatiles::HEAL_BLOCK => VolatileEffect::HealBlock,
        Volatiles::MAGNET_RISE => VolatileEffect::MagnetRise,
        Volatiles::PERISH_SONG => {
            // `onEnd`: `-start|...|perish0`, then it faints.
            let actor = actor(battle, pos);
            battle.emit(BattleEvent::PerishCount { actor, count: 0, silent: false });
            super::faint(battle, pos);
            return true;
        }
        // No `onEnd`.
        _ => return true,
    };
    let actor = actor(battle, pos);
    battle.emit(BattleEvent::EffectEnd { actor, effect, silent: false });
    if flag == Volatiles::YAWN {
        yawn_sleep(battle, pos);
    }
    true
}

/// Yawn's `onEnd`: `trySetStatus('slp')`. Its failures are silent (Yawn
/// has no `status`), except that Electric and Misty Terrain announce
/// blocking it (`effect.id === 'yawn'`).
fn yawn_sleep(battle: &mut Battle, pos: Position) {
    let Some(m) = battle.state.at(pos) else { return };
    if m.is_fainted() || m.status != Status::None {
        return;
    }
    // `trySetStatus('slp', this.effectState.source)` with Yawn as the effect: one
    // `SetStatus` event, so Uproar's `onAnySetStatus` sorts before the
    // terrains; Misty Terrain announces Yawn, and Safeguard lets it through
    // (`effect.id === 'yawn'`, which no source reproduces). Failures are
    // otherwise silent (Yawn has no `status`).
    let source = Some(m.volatile_counters.yawn_source);
    status::set_status_from(
        battle,
        pos,
        StatusId::Sleep,
        source,
        super::events::Effect::volatile(Volatiles::YAWN),
        true,
    );
}

/// Showdown's `spreadDamage` for a volatile's damage: at least 1, after
/// the `Damage` event (Magic Guard), logged `[from]` the effect. Returns
/// the HP removed.
fn effect_damage(battle: &mut Battle, pos: Position, amount: u32, effect: VolatileEffect, of: Option<Position>) -> u16 {
    // `spreadDamage` names the source only if it is someone else (after Ally
    // Switch the seeder's slot can hold the seeded Pokémon itself).
    let of = of.filter(|&p| p != pos);
    if battle.state.at(pos).is_none_or(|m| m.is_fainted()) {
        return 0;
    }
    // `runEvent('Damage')` (Magic Guard).
    let effect_id = super::events::Effect::Condition(super::events::Condition::Other);
    let Some(amount) = super::damage_event(battle, pos, amount, of, effect_id) else { return 0 };
    let (target, of) = (actor(battle, pos), of.map(|p| actor(battle, p)));
    let Some(m) = battle.state.at_mut(pos) else { return 0 };
    if m.is_fainted() {
        return 0;
    }
    let dealt = amount.min(m.hp as u32) as u16;
    m.hp -= dealt;
    super::power::record_hurt(m, dealt);
    let (hp, max_hp, status, fainted) = (m.hp, m.max_hp(), m.status, m.is_fainted());
    battle.emit(BattleEvent::EffectDamage { target, hp, max_hp, status, effect, of });
    if fainted {
        super::queue_faint(battle, pos.into());
    }
    dealt
}

/// Showdown's `heal` for Leech Seed: `TryHeal` (Heal Block), then only an
/// active Pokémon below full HP heals, logged `[silent]`.
/// `source` is the seeded Pokémon (Liquid Ooze's `onSourceTryHeal`).
fn effect_heal(battle: &mut Battle, pos: Position, amount: u32, effect: VolatileEffect, source: Position) {
    let from = super::events::Effect::volatile(Volatiles::LEECH_SEED);
    let Some(amount) = super::try_heal(battle, pos, amount.max(1), Some(source), from) else { return };
    let target = actor(battle, pos);
    let Some(m) = battle.state.at_mut(pos) else { return };
    if m.is_fainted() || m.hp >= m.max_hp() {
        return;
    }
    let healed = amount.max(1).min((m.max_hp() - m.hp) as u32) as u16;
    m.hp += healed;
    let (hp, max_hp, status) = (m.hp, m.max_hp(), m.status);
    battle.emit(BattleEvent::EffectHeal { target, hp, max_hp, status, effect });
}

/// Attract's `pokemon.gender === 'M' && source.gender === 'F' || ...` (its
/// `onTryImmunity` and `onStart`; Captivate's `onTryImmunity`), through
/// the genders fixed when the battle was created.
pub(crate) fn opposite_genders(battle: &Battle, a: Position, b: Position) -> bool {
    matches!(
        (mon(battle, a).gender, mon(battle, b).gender),
        (Gender::Male, Gender::Female) | (Gender::Female, Gender::Male)
    )
}

// ---------------------------------------------------------------------------
// Move pipeline.

/// Curse's `onModifyMove` (the move's own `ModifyMove`, before the `|move|`
/// line): a non-Ghost user targets itself, and its `onTryHit` turns the
/// move into stat changes (no `volatileStatus`); a Ghost type aiming at an
/// ally curses a random foe instead. Returns the changed move, whose target
/// the caller then redraws (`getRandomTarget`) as Showdown does.
pub(crate) fn modify_move(
    battle: &Battle,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    target: Option<Position>,
) -> Option<MoveData> {
    if move_id != ids::CURSE {
        return None;
    }
    let mut data = *data;
    if !has_type(mon(battle, user), Type::Ghost) {
        // The volatile is dropped by `onTryHit`, after `PrepareHit` (where
        // Protean can make the user a Ghost type): see `moves.rs`.
        data.target = MoveTarget::User;
        return Some(data);
    }
    // Champions' `onModifyMove` leaves a Curse aimed at the user
    // itself alone (`source !== target`): `onHit` picks the foe.
    let champions = battle.format.mechanics == Mechanics::Champions;
    if target.is_some_and(|t| t.side == user.side && !(champions && t == user)) {
        data.target = MoveTarget::RandomNormal;
        return Some(data);
    }
    None
}

/// The target type the request offers for a move (`getMoves`): Curse from
/// a non-Ghost type targets the user (`getMoves` sets `target = 'self'`).
pub(crate) fn request_target(mon: &PokemonState, move_id: MoveId, target: MoveTarget) -> MoveTarget {
    match move_id {
        ids::CURSE if !has_type(mon, Type::Ghost) => MoveTarget::User,
        // Pollen Puff can't heal an ally under Heal Block, so the request
        // offers only foes.
        ids::POLLEN_PUFF if mon.volatiles.has(Volatiles::HEAL_BLOCK) => MoveTarget::AdjacentFoe,
        // Terapagos-Stellar's Tera Starstorm hits every adjacent foe.
        ids::TERA_STARSTORM if super::tera::species_of(mon) == pikapro_data::gen9::species::TERAPAGOS_STELLAR => {
            MoveTarget::AllAdjacentFoes
        }
        _ => target,
    }
}

/// The move's `onTry`. False fails it (`-fail`).
pub(crate) fn try_move(battle: &Battle, user: Position, move_id: MoveId) -> bool {
    let Some(m) = battle.state.at(user) else { return true };
    match move_id {
        ids::NO_RETREAT => !m.volatiles.has(Volatiles::NO_RETREAT),
        // Smack Down and Ingrain hold the user down. Under Gravity it is
        // stopped earlier (Gravity's `ModifyMove`).
        ids::MAGNET_RISE => !m.volatiles.has(Volatiles::SMACK_DOWN | Volatiles::INGRAIN),
        _ => true,
    }
}

/// The move's `onPrepareHit`. False fails it (`-fail`).
#[inline(always)]
pub(crate) fn prepare_hit(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    match move_id {
        // `!!willAct() && runEvent('StallMove')`: `stall`'s 1/3^n chance.
        ids::ENDURE => battle.queue.will_act() && super::protect::stall_move(battle, user),
        // Using Destiny Bond again right away fails (and ends it).
        ids::DESTINY_BOND => !remove(battle, user, Volatiles::DESTINY_BOND),
        _ => true,
    }
}

/// The move's own `onTryHit` on its first target. False stops the hit;
/// each failure logs its own message.
pub(crate) fn try_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    let fail = |battle: &mut Battle, pos: Position, move_id: MoveId, weak: bool| {
        let actor = actor(battle, pos);
        battle.emit(BattleEvent::MoveFail {
            user: actor,
            move_id,
            note: if weak { FailNote::Weak } else { FailNote::None },
        });
        false
    };
    let m = *mon(battle, target);
    match move_id {
        ids::SUBSTITUTE => {
            if m.volatiles.has(Volatiles::SUBSTITUTE) {
                return fail(battle, target, move_id, false);
            }
            if m.hp as u32 * 4 <= m.max_hp() as u32 || m.max_hp() == 1 {
                return fail(battle, target, move_id, true);
            }
            true
        }
        ids::SHED_TAIL => {
            if switching::can_switch(battle, user.side) == 0 || m.volatiles.has(Volatiles::COMMANDED) {
                let actor = actor(battle, target);
                battle.emit(BattleEvent::Fail { user: actor });
                return false;
            }
            if m.volatiles.has(Volatiles::SUBSTITUTE) {
                return fail(battle, target, move_id, false);
            }
            if m.hp <= m.max_hp().div_ceil(2) {
                return fail(battle, target, move_id, true);
            }
            true
        }
        // `target.status || !target.runStatusImmunity('slp')`.
        ids::YAWN if m.status != Status::None => {
            moves::fail_still(battle, user);
            false
        }
        ids::CURSE if has_type(mon(battle, user), Type::Ghost) && m.volatiles.has(Volatiles::CURSE) => {
            moves::fail_still(battle, user);
            false
        }
        _ => true,
    }
}

/// The move's `onTryImmunity` (the `TryImmunity` hit step): Leech Seed
/// doesn't affect Grass types, Attract and Captivate need opposite genders. False logs
/// `-immune`.
pub(crate) fn try_immunity(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    let immune = match move_id {
        ids::LEECH_SEED => has_type(mon(battle, target), Type::Grass),
        ids::ATTRACT | ids::CAPTIVATE => !opposite_genders(battle, target, user),
        // Octolock's `onTryImmunity`: `dex.getImmunity('trapped', target)`,
        // Ghost types.
        ids::OCTOLOCK => has_type(mon(battle, target), Type::Ghost),
        _ => false,
    };
    if immune {
        let target = actor(battle, target);
        battle.emit(BattleEvent::Immune { target });
    }
    !immune
}

/// The targets of a hit after [`try_primary_hit`].
pub(crate) struct PrimaryHit {
    /// Targets the move hits normally.
    pub hit: Targets,
    /// Targets whose substitute took the hit (`HIT_SUBSTITUTE`): the move's
    /// primary effects skip them, but they still count as hit, `selfDrops`
    /// run for them and their secondaries roll.
    pub substitute: Targets,
    /// A substitute blocked a status move (`null`): the hit still counts
    /// as not failed.
    pub blocked: bool,
}

/// Showdown's `tryPrimaryHitEvent`: Substitute's `onTryPrimaryHit` on each
/// target. A move that doesn't bypass it hits the substitute instead:
/// `damage` is the move's `getDamage` in half points (with its crit and
/// damage rolls and messages; Metal Burst's and Comeuppance's `damage *
/// 1.5` is not truncated), the substitute loses that much HP (and breaks
/// at 0), and recoil and drain apply to it; a status move fails.
pub(crate) fn try_primary_hit(
    battle: &mut Battle,
    targets: &Targets,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    mut damage: impl FnMut(&mut Battle, Position) -> Option<u32>,
) -> PrimaryHit {
    let mut out = PrimaryHit { hit: Targets::new(), substitute: Targets::new(), blocked: false };
    for &t in targets.iter() {
        // `runEvent('TryPrimaryHit', target, pokemon, moveData)`: the gems'
        // `onSourceTryPrimaryHit` (priority 0) before the substitute's (-1).
        let relay = super::events::Relay::True;
        let tried =
            super::events::move_event(battle, pikapro_data::Event::TryPrimaryHit, t, Some(user), move_id, data, relay);
        if !tried.is_truthy() {
            continue;
        }
        // Present's healing, Pollen Puff on an ally and Infiltrator set
        // `infiltrates`.
        let infiltrates = move_id == ids::PRESENT && data.heal.is_some()
            || move_id == ids::POLLEN_PUFF && t.side == user.side
            || battle.move_state.infiltrates;
        if t == user
            || !has(battle, t, Volatiles::SUBSTITUTE)
            || data.flags.contains(MoveFlags::BYPASS_SUB)
            || infiltrates
        {
            out.hit.push(t);
            continue;
        }
        if data.category == MoveCategory::Status {
            // `getDamage` is `undefined` for a status move.
            moves::fail_still(battle, user);
            out.blocked = true;
            continue;
        }
        // `getDamage` is `undefined` (a damage callback that does nothing):
        // `-fail` and `null`.
        let Some(halves) = damage(battle, t) else {
            moves::fail_still(battle, user);
            out.blocked = true;
            continue;
        };
        // `if (damage > sub.hp) damage = sub.hp; sub.hp -= damage`, exactly:
        // a substitute can be left at x.5 HP.
        let sub_halves = u32::from(mon(battle, t).volatile_counters.substitute_hp_halves);
        let halves = halves.min(sub_halves);
        let left = sub_halves - halves;
        mon_mut(battle, t).volatile_counters.substitute_hp_halves = left as u16;
        if left == 0 {
            if data.ohko {
                battle.emit(BattleEvent::Ohko);
            }
            remove(battle, t, Volatiles::SUBSTITUTE);
        } else {
            let actor = actor(battle, t);
            battle.emit(BattleEvent::EffectActivate { actor, effect: VolatileEffect::Substitute, of: None });
        }
        if halves > 0 {
            super::effects::apply_recoil(battle, user, move_id, data, halves);
        }
        if let Some(ratio) = data.drain {
            // `Math.ceil(damage * drain[0] / drain[1])`, of the exact damage.
            let (n, d) = (ratio.numerator as u32, ratio.denominator as u32);
            let of = actor(battle, t);
            apply_heal(battle, user, (halves * n).div_ceil(2 * d), HealSource::Drain { of });
        }
        // `singleEvent('AfterSubDamage')`: Stone Axe, Rapid Spin, Ice
        // Spinner, ..., Shell Side Arm's hint; then `runEvent`.
        if move_id == pikapro_data::gen9::moves::SHELL_SIDE_ARM && t.side != user.side {
            battle.hint(crate::event::Hint::ShellSideArm(data.category));
        }
        field::after_sub_damage(battle, t, user, move_id);
        // The relay carries whole points: Showdown passes the exact damage,
        // but no `onAfterSubDamage` handler reads it (Air Balloon, Rapid
        // Spin, Stone Axe, Flame Burst, ... use only the target and user).
        let relay = super::events::Relay::Num((halves / 2) as i32);
        super::events::move_event(battle, pikapro_data::Event::AfterSubDamage, t, Some(user), move_id, data, relay);
        out.substitute.push(t);
    }
    out
}

/// Moves with an `onHit` implemented by [`on_hit`].
pub(crate) fn has_on_hit(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::SUBSTITUTE | ids::SHED_TAIL | ids::BLOCK | ids::MEAN_LOOK | ids::JAW_LOCK | ids::CURSE | ids::ENDURE
    )
}

/// The move's `onHit` in `runMoveEffects` (after its volatile). Returns its
/// result; `undefined` counts as success.
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    match move_id {
        ids::SUBSTITUTE => {
            let max_hp = mon(battle, target).max_hp() as u32;
            direct_damage(battle, target, super::hp_fraction(max_hp, 1, 4));
        }
        ids::SHED_TAIL => {
            let max_hp = mon(battle, target).max_hp() as u32;
            direct_damage(battle, target, max_hp.div_ceil(2));
        }
        ids::BLOCK | ids::MEAN_LOOK => {
            return volatiles::add_volatile(battle, target, "trapped", Some(user), false) == Some(true);
        }
        ids::JAW_LOCK => {
            volatiles::add_volatile(battle, user, "trapped", Some(target), false);
            volatiles::add_volatile(battle, target, "trapped", Some(user), false);
        }
        ids::CURSE if battle.format.mechanics == Mechanics::Champions => {
            // Champions' `curse.onHit` (`data/mods/champions/moves.ts`): no
            // `volatileStatus`, no `self` boosts.
            if !has_type(mon(battle, user), Type::Ghost) {
                // `return !!this.boost({spe: -1, atk: 1, def: 1}, source,
                // source)`: a plain boost (no `selfDrops` roll), whose
                // unchanged stats are announced, and whose failure fails
                // the move.
                let b = StatBoosts::ordered([1, 1, 0, 0, -1, 0, 0], [4, 0, 1, 2, 3, 5, 6]);
                return boosts::boost(battle, user, b, Some(user), false, false);
            }
            let max_hp = mon(battle, user).max_hp() as u32;
            direct_damage(battle, user, super::hp_fraction(max_hp, 1, 2));
            // An ally target (the user itself, when it became a Ghost type
            // after its Curse was queued) curses a random foe instead:
            // `getRandomTarget(source, 'Curse')` (the dex move's `normal`).
            let mut target = target;
            if target.side == user.side {
                let Some(random) = super::targeting::get_random_target(battle, user, MoveTarget::Normal) else {
                    return false;
                };
                target = random;
            }
            // `delete target.volatiles['curse']; target.addVolatile('curse')`.
            if let Some(m) = battle.state.at_mut(target) {
                m.volatiles.clear(Volatiles::CURSE);
            }
            volatiles::add_volatile(battle, target, "curse", Some(user), false);
        }
        ids::CURSE => {
            if has_type(mon(battle, user), Type::Ghost) {
                let max_hp = mon(battle, user).max_hp() as u32;
                direct_damage(battle, user, super::hp_fraction(max_hp, 1, 2));
            } else {
                // The `self` effect `onTryHit` gave a non-Ghost user:
                // `selfDrops` rolls once, then `{spe: -1, atk: 1, def: 1}`.
                battle.rng.random(100);
                let b = StatBoosts::ordered([1, 1, 0, 0, -1, 0, 0], [4, 0, 1, 2, 3, 5, 6]);
                boosts::boost(battle, user, b, Some(user), false, true);
            }
        }
        ids::ENDURE => super::protect::add_stall(battle, user),
        _ => {}
    }
    true
}

/// A secondary effect's `onHit` (Spirit Shackle traps, Throat Chop), run
/// after its other effects.
pub(crate) fn secondary_on_hit(battle: &mut Battle, target: Position, user: Position) {
    match battle.active_move.map(|(_, m)| m) {
        Some(ids::SPIRIT_SHACKLE) if battle.state.at(user).is_some() => {
            volatiles::add_volatile(battle, target, "trapped", Some(user), true);
        }
        Some(ids::THROAT_CHOP) => {
            volatiles::add_volatile(battle, target, "throatchop", Some(user), true);
        }
        _ => {}
    }
}

/// The move's `onHitField` (Perish Song): every active Pokémon without a
/// perish count gets one. Returns false (the move fails) if none did.
pub(crate) fn hit_field(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    if move_id != ids::PERISH_SONG {
        return true;
    }
    let (mut result, mut message) = (false, false);
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            if battle.state.at(pos).is_none() {
                continue;
            }
            // `Invulnerability` (Fly, Dig, ...) logs `-miss`.
            if !super::multiturn::field_invulnerability(battle, pos, user, move_id) {
                result = true;
                continue;
            }
            // `runEvent('TryHit', pokemon, source, move) === null`: the active
            // move, with its type after `ModifyType`. Psychic Terrain's
            // handler comes first (priority 4) against a Prankster's or a
            // Metronome's priority Perish Song; then Soundproof, Good as Gold,
            // Water Absorb when Liquid Voice made it Water. (The guards never
            // stop Perish Song: no `protect` flag, not a spread target.)
            let mut data = *battle.format.dex.move_(move_id);
            data.ty = battle.move_state.ty.unwrap_or(data.ty);
            if !super::field::try_hit(battle, pos, user, move_id, &data) {
                result = true;
                continue;
            }
            let relay = super::events::Relay::True;
            if super::events::move_event(battle, pikapro_data::Event::TryHit, pos, Some(user), move_id, &data, relay)
                == super::events::Relay::Null
            {
                result = true;
                continue;
            }
            if battle.state.at(pos).is_some_and(|m| m.volatiles.has(Volatiles::PERISH_SONG)) {
                continue;
            }
            volatiles::add_volatile(battle, pos, "perishsong", Some(user), false);
            let actor = actor(battle, pos);
            battle.emit(BattleEvent::PerishCount { actor, count: 3, silent: true });
            result = true;
            message = true;
        }
    }
    if message {
        battle.emit(BattleEvent::FieldActivate { effect: VolatileEffect::PerishSong });
    }
    result
}

/// `addPseudoWeather('fairylock')` (`Field.addPseudoWeather`): fails if it
/// is already up; otherwise lasts 2 turns (so it traps everyone at the
/// next request), then `runEvent('PseudoWeatherChange')` from `source`.
pub(crate) fn add_fairy_lock(battle: &mut Battle, source: Position) -> bool {
    if battle.state.field.fairy_lock_turns > 0 {
        return false;
    }
    battle.state.field.fairy_lock_turns = 2;
    field::pseudo_weather_started(battle, PseudoWeather::FairyLock);
    battle.emit(BattleEvent::FieldActivate { effect: VolatileEffect::FairyLock });
    field::pseudo_weather_change(battle, PseudoWeather::FairyLock, source);
    true
}

/// `MoveAborted` (a `BeforeMove` handler stopped the move): Destiny Bond
/// ends.
pub(crate) fn move_aborted(battle: &mut Battle, user: Position) {
    remove(battle, user, Volatiles::DESTINY_BOND);
}

// ---------------------------------------------------------------------------
// Damage, healing, fainting.

/// Endure's `onDamage` (priority -10): a move's damage leaves the Pokémon
/// at 1 HP.
pub(crate) fn endure_damage(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    h: &Handler,
) -> Option<super::events::Relay> {
    let pos = holder(h);
    let hp = battle.state.at(pos)?.hp as i32;
    match ev.relay {
        super::events::Relay::Num(damage) if ev.effect.is_move() && damage >= hp => {
            let actor = actor(battle, pos);
            battle.emit(BattleEvent::EffectActivate { actor, effect: VolatileEffect::Endure, of: None });
            Some(super::events::Relay::Num(hp - 1))
        }
        _ => None,
    }
}

/// Heal Block's `onTryHeal`: no healing. An ally's Pollen Puff is
/// announced (`cant|<user>|move: Heal Block|Pollen Puff`, the move line
/// losing its target) and stops silently (`null`).
pub(crate) fn heal_block_try_heal(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    let target = ev.target_pos()?;
    if let (Some(source), super::events::Effect::Move(ids::POLLEN_PUFF)) = (ev.source, ev.effect) {
        let m = battle.state.at(target)?;
        if source != target && m.hp != m.max_hp() {
            // `attrLastMove('[still]')`.
            battle.flush_rng_trace();
            battle.attr_last_move(crate::event::MoveAttr::Still);
            let actor = actor(battle, source);
            let effect = VolatileEffect::HealBlock;
            battle.emit(BattleEvent::EffectCant { actor, effect, move_id: Some(ids::POLLEN_PUFF) });
            return Some(super::events::Relay::Null);
        }
    }
    Some(super::events::Relay::False)
}

/// Pollen Puff's `onTryMove`: a user under Heal Block can't use it on an
/// ally (`attrLastMove('[still]')`, `cant|...|move: Heal Block|Pollen
/// Puff`). The request stops choosing an ally then, but Heal Block can
/// start after the choice.
pub(crate) fn pollen_puff_try_move(battle: &mut Battle, user: Position, move_id: MoveId, target: Position) -> bool {
    if move_id != ids::POLLEN_PUFF
        || target.side != user.side
        || target == user
        || !has(battle, user, Volatiles::HEAL_BLOCK)
    {
        return true;
    }
    battle.flush_rng_trace();
    battle.attr_last_move(crate::event::MoveAttr::Still);
    let actor = actor(battle, user);
    battle.emit(BattleEvent::EffectCant { actor, effect: VolatileEffect::HealBlock, move_id: Some(ids::POLLEN_PUFF) });
    false
}

/// Destiny Bond's `onFaint`.
pub(crate) fn destiny_bond_faint(battle: &mut Battle, h: &Handler) -> bool {
    on_faint(battle, holder(h));
    true
}

/// A move's damage knocked out the Pokémon at `pos`: remember who, for
/// Destiny Bond's `onFaint` (the faint's `source`, with the move as its
/// `effect`).
pub(crate) fn on_move_ko(battle: &mut Battle, pos: Position) {
    let Some((user, move_id)) = battle.active_move else { return };
    let future =
        move_id != volatiles::RECHARGE && battle.format.dex.move_(move_id).flags.contains(MoveFlags::FUTURE_MOVE);
    if user.side == pos.side || future || !has(battle, pos, Volatiles::DESTINY_BOND) {
        return;
    }
    let by = key(battle, user);
    mon_mut(battle, pos).volatile_counters.destiny_bond_ko_by = Some(by);
}

/// The `Faint` event of a Pokémon whose faint is being reported: Destiny
/// Bond takes the foe that knocked it out down with it.
fn on_faint(battle: &mut Battle, pos: Position) {
    let m = &battle.state.side(pos.side).team[pos.slot as usize];
    let Some(by) = m.volatile_counters.destiny_bond_ko_by.filter(|_| m.volatiles.has(Volatiles::DESTINY_BOND)) else {
        return;
    };
    let actor = actor(battle, pos);
    battle.emit(BattleEvent::EffectActivate { actor, effect: VolatileEffect::DestinyBond, of: None });
    if let Some(source) = active_at(battle, by) {
        super::faint(battle, source);
    }
}

/// Glaive Rush's `onSourceModifyDamage`: its user takes double damage
/// until its next move.
pub(crate) fn glaive_rush_source_damage(
    _: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    ev.chain_modify(2, 1)
}

/// Glaive Rush's `onAccuracy` as an `Accuracy` handler: `true`.
pub(crate) fn glaive_rush_accuracy(
    _: &mut Battle,
    _: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    Some(super::events::Relay::True)
}

// ---------------------------------------------------------------------------
// Switching.

/// A Pokémon leaves the field (switched, dragged or fainted): the traps it
/// set end silently (Showdown's `removeLinkedVolatiles` for `trapper`), and
/// Shed Tail hands its substitute to the replacement. Returns the
/// substitute's HP to pass on, in half points (`copyVolatileFrom(...,
/// 'shedtail')` copies its `effectState`).
pub(crate) fn switch_out(battle: &mut Battle, pos: Position, from: Option<MoveId>) -> Option<u16> {
    let leaving = key(battle, pos);
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let other = Position::new(side, slot as u8);
            if other == pos {
                continue;
            }
            if let Some(m) = battle.state.at_mut(other)
                && !m.is_fainted()
                && m.volatiles.has(Volatiles::TRAPPED)
                && m.volatile_counters.trapped_by == leaving
            {
                m.volatiles.clear(Volatiles::TRAPPED);
            }
        }
    }
    let m = &battle.state.side(pos.side).team[pos.slot as usize];
    (from == Some(ids::SHED_TAIL) && m.volatiles.has(Volatiles::SUBSTITUTE))
        .then_some(m.volatile_counters.substitute_hp_halves)
}

/// The replacement of a Shed Tail user gets its substitute (its HP in half
/// points).
pub(crate) fn switched_in(battle: &mut Battle, pos: Position, substitute: Option<u16>) {
    if let Some(hp) = substitute {
        let m = mon_mut(battle, pos);
        m.volatiles.add(Volatiles::SUBSTITUTE);
        m.volatile_counters.substitute_hp_halves = hp;
    }
}

// ---------------------------------------------------------------------------
// Requests.

/// Whether the Pokémon at `pos` can't switch out (the end of turn's
/// `TrapPokemon` event: `trapped`, `partiallytrapped` while its trapper is
/// active, No Retreat, Fairy Lock). Ghost types are never trapped
/// (`tryTrap`'s `runStatusImmunity('trapped')`).
pub(crate) fn is_trapped(battle: &Battle, pos: Position) -> bool {
    trapped(battle, pos, true)
}

/// Whether the Pokémon at `pos` is trapped with Showdown's `trapped
/// === true` rather than a foe ability's hidden `tryTrap(true)` (Shadow
/// Tag, Arena Trap, Magnet Pull): the last active Pokémon's request shows
/// only these as `trapped` (the hidden ones as `maybeTrapped`).
pub(crate) fn is_visibly_trapped(battle: &Battle, pos: Position) -> bool {
    trapped(battle, pos, false)
}

/// [`is_trapped`], with the foe abilities' trap (`m.trapped`, set by
/// `tryTrap`) counted only when `hidden`.
#[inline(always)]
fn trapped(battle: &Battle, pos: Position, hidden: bool) -> bool {
    let Some(m) = battle.state.at(pos) else { return false };
    // Commander's volatiles trap (priority -11, after Shed
    // Shell, without `tryTrap`).
    if m.volatiles.has(Volatiles::COMMANDING | Volatiles::COMMANDED) {
        return true;
    }
    // `tryTrap` checks the `trapped` immunity: Ghost types (including one
    // added by Trick-or-Treat) can't be trapped.
    if super::manipulation::has_type(m, Type::Ghost) {
        return false;
    }
    let v = m.volatiles;
    // Shed Shell's `onTrapPokemon` frees it; abilities trap it (`tryTrap`).
    if m.trap_released {
        return false;
    }
    // The foe abilities' `tryTrap` (Shadow Tag, Arena Trap, Magnet Pull),
    // counted only when `hidden`; then the volatiles' `onTrapPokemon`
    // (Mean Look's `trapped`, No Retreat, Ingrain), a
    // partial trap or Octolock whose source is still active, Fairy Lock.
    hidden && m.trapped
        || v.has(Volatiles::TRAPPED | Volatiles::NO_RETREAT | Volatiles::INGRAIN)
        || v.has(Volatiles::PARTIALLY_TRAPPED) && active_at(battle, m.volatile_counters.partially_trapped_by).is_some()
        || v.has(Volatiles::OCTOLOCK) && active_at(battle, m.volatile_counters.octolock_by).is_some()
        || battle.state.field.fairy_lock_turns > 0
}

// ---------------------------------------------------------------------------
// Event handlers.

fn holder(handler: &Handler) -> Position {
    match handler.holder {
        Holder::Pokemon(pos) => pos,
        _ => unreachable!("volatile handlers are held by a Pokémon"),
    }
}

fn pokemon_handler(battle: &Battle, pos: Position, volatile: u128) -> Handler {
    Handler {
        speed: mon(battle, pos).speed,
        sub_order: sub_order::CONDITION,
        holder: Holder::Pokemon(pos),
        volatile_bit: super::handlers::vol(volatile),
        ..Handler::default()
    }
}

fn foes(battle: &Battle, pos: Position) -> impl Iterator<Item = Position> + '_ {
    let side = pos.side.foe();
    (0..battle.format.active_per_side() as u8)
        .map(move |s| Position::new(side, s))
        .filter(|&p| battle.state.at(p).is_some())
}

/// `runEvent('ModifyMove')` on the user, but for the field's handlers
/// (`field::modify_move`, speed 0: they never tie with these): a frozen
/// user's `frz` handler, Throat Chop's and Heal Block's `onModifyMove`
/// (all three priority 0 and tied), then the user's ability and item
/// (`events::collect_into`), in one sort as `findEventHandlers` collects
/// them, so Stance Change (priority 1) runs before Throat Chop. Throat
/// Chop and Heal Block stop a sound or a healing move with `cant` and
/// `false`, so the rest of the event doesn't run; `BeforeMove` already
/// stopped the moves the user chose, so this only catches moves called by
/// other moves (Copycat, Sleep Talk, ...). Returns false when one did.
pub(crate) fn modify_move_event(
    battle: &mut Battle,
    user: Position,
    target: Option<Position>,
    move_id: MoveId,
    data: &mut MoveData,
) -> bool {
    let Some(m) = battle.state.at(user) else { return true };
    let mut handlers = super::handlers::MonHandlers::new();
    if matches!(m.status, Status::Freeze { .. }) {
        let holder = Holder::Pokemon(user);
        let callback = Callback::Plain(frz_modify_move as HandlerFn);
        handlers.push(Handler { speed: m.speed, sub_order: sub_order::STATUS, holder, callback, ..Handler::default() });
    }
    for v in m.volatiles.in_order() {
        if v == Volatiles::THROAT_CHOP || v == Volatiles::HEAL_BLOCK {
            handlers.push(Handler { callback: Callback::Plain(stop_modify_move), ..pokemon_handler(battle, user, v) });
        }
    }
    let event = pikapro_data::Event::ModifyMove;
    super::events::collect_into(battle, event, super::events::Target::Pokemon(user), target, &mut handlers);
    if handlers.is_empty() {
        return true;
    }
    speed_sort(&mut handlers, &mut battle.rng, Handler::compare);
    let effect = super::events::Effect::Move(move_id);
    let mut ev = super::events::EventCtx::new(
        event,
        super::events::Target::Pokemon(user),
        target,
        effect,
        super::events::Relay::True,
    )
    .with_move_mut(data);
    super::events::run_list(battle, &mut ev, &handlers).is_truthy()
}

/// `frz`'s `onModifyMove`: a `defrost` move thaws its user.
fn frz_modify_move(battle: &mut Battle, _: &Handler) -> bool {
    let (user, move_id) = battle.active_move.expect("ModifyMove runs for the active move");
    let data = *battle.format.dex.move_(move_id);
    super::status::modify_move(battle, user, move_id, &data);
    true
}

/// Throat Chop's and Heal Block's `onModifyMove`: a sound or healing move
/// can't be used (`!move.isZOrMaxPowered`: always so in Gen 9).
fn stop_modify_move(battle: &mut Battle, handler: &Handler) -> bool {
    let (_, move_id) = battle.active_move.expect("ModifyMove runs for the active move");
    let user = holder(handler);
    let flags = battle.format.dex.move_(move_id).flags;
    let (flag, effect, shown) = match handler.volatile() {
        Volatiles::THROAT_CHOP => (MoveFlags::SOUND, VolatileEffect::ThroatChop, None),
        _ => (MoveFlags::HEAL, VolatileEffect::HealBlock, Some(move_id)),
    };
    if !flags.contains(flag) {
        return true;
    }
    cant(battle, user, effect, shown)
}

/// `onBeforeMove` of the user's volatiles (Glaive Rush 100, Throat Chop and
/// Heal Block 6, Attract 2, Destiny Bond -1).
pub(crate) fn before_move_handlers<const N: usize>(
    battle: &Battle,
    user: Position,
    handlers: &mut super::handlers::SpillList<N>,
) {
    let Some(m) = battle.state.at(user) else { return };
    let own: [(u128, i32, HandlerFn); 5] = [
        (Volatiles::GLAIVE_RUSH, 100, glaive_rush_before_move),
        (Volatiles::THROAT_CHOP, 6, throat_chop_before_move),
        (Volatiles::HEAL_BLOCK, 6, heal_block_before_move),
        (Volatiles::ATTRACT, 2, attract_before_move),
        (Volatiles::DESTINY_BOND, -1, destiny_bond_before_move),
    ];
    for (flag, priority, run) in own {
        if m.volatiles.has(flag) {
            handlers.push(Handler { priority, callback: Callback::Plain(run), ..pokemon_handler(battle, user, flag) });
        }
    }
    // The foes' Imprison (`onFoeBeforeMove`) is a registry handler
    // (`events::volatile_spec`), collected after the user's own as
    // `findEventHandlers` does.
}

/// The move being used, unless it is the `recharge` pseudo-move.
fn active_move_data(battle: &Battle) -> Option<(MoveId, &'static MoveData)> {
    let (_, move_id) = battle.active_move.expect("BeforeMove runs for the active move");
    (move_id != volatiles::RECHARGE).then(|| (move_id, battle.format.dex.move_(move_id)))
}

fn cant(battle: &mut Battle, pos: Position, effect: VolatileEffect, move_id: Option<MoveId>) -> bool {
    let actor = actor(battle, pos);
    battle.emit(BattleEvent::EffectCant { actor, effect, move_id });
    false
}

/// Glaive Rush's drawback ends before the user's next move.
fn glaive_rush_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    remove(battle, holder(handler), Volatiles::GLAIVE_RUSH);
    true
}

/// Throat Chop: sound moves can't be used (called moves: its
/// `onModifyMove`, in [`modify_move_event`]).
fn throat_chop_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    match active_move_data(battle) {
        Some((_, data)) if data.flags.contains(MoveFlags::SOUND) => cant(battle, pos, VolatileEffect::ThroatChop, None),
        _ => true,
    }
}

/// Heal Block: healing moves can't be used (called moves: its
/// `onModifyMove`, in [`modify_move_event`]).
fn heal_block_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    match active_move_data(battle) {
        Some((move_id, data)) if data.flags.contains(MoveFlags::HEAL) => {
            cant(battle, pos, VolatileEffect::HealBlock, Some(move_id))
        }
        _ => true,
    }
}

/// Attract: `-activate`, then a 1/2 chance to be immobilized by love.
fn attract_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    if !has(battle, pos, Volatiles::ATTRACT) {
        return true;
    }
    let (actor, of) = (actor(battle, pos), actor_of(battle, mon(battle, pos).volatile_counters.attract_by));
    battle.emit(BattleEvent::EffectActivate { actor, effect: VolatileEffect::Attract, of: Some(of) });
    if battle.rng.chance(1, 2) {
        return cant(battle, pos, VolatileEffect::Attract, None);
    }
    true
}

/// Destiny Bond ends before any other move of its user.
fn destiny_bond_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    if battle.active_move.is_none_or(|(_, m)| m != ids::DESTINY_BOND) {
        remove(battle, holder(handler), Volatiles::DESTINY_BOND);
    }
    true
}

/// Imprison (held by a foe of the user): moves the holder also knows can't
/// be used.
pub(crate) fn imprison_before_move(battle: &mut Battle, handler: &Handler) -> bool {
    let (user, move_id) = battle.active_move.expect("BeforeMove runs for the active move");
    let holder = holder(handler);
    let Some(h) = battle.state.at(holder).filter(|h| h.volatiles.has(Volatiles::IMPRISON)) else { return true };
    if move_id == ids::STRUGGLE || move_id == volatiles::RECHARGE || !h.moves.iter().any(|s| s.id == move_id) {
        return true;
    }
    cant(battle, user, VolatileEffect::Imprison, Some(move_id))
}

/// End-of-turn handlers: durations (Showdown's `fieldEvent('Residual')`
/// collects every volatile with a `duration`) and `onResidual`, with their
/// `onResidualOrder`. Fairy Lock's duration is collected with the
/// pseudo-weathers (`field::residual_handlers`).
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    // A `static`, read in place (a `const` looped over by value would be
    // copied onto the stack for every Pokémon).
    static RESIDUAL: [(u128, Option<u8>, HandlerFn); 12] = [
        (Volatiles::LEECH_SEED, Some(8), leech_seed_residual),
        (Volatiles::CURSE, Some(12), curse_residual),
        (Volatiles::PARTIALLY_TRAPPED, Some(13), partially_trapped_residual),
        (Volatiles::SALT_CURE, Some(13), salt_cure_residual),
        (Volatiles::SYRUP_BOMB, Some(14), syrup_bomb_residual),
        (Volatiles::OCTOLOCK, Some(14), octolock_residual),
        (Volatiles::MAGNET_RISE, Some(18), tick_magnet_rise),
        (Volatiles::HEAL_BLOCK, Some(20), tick_heal_block),
        (Volatiles::THROAT_CHOP, Some(22), tick_throat_chop),
        (Volatiles::YAWN, Some(23), tick_yawn),
        (Volatiles::PERISH_SONG, Some(24), perish_song_residual),
        (Volatiles::ENDURE, None, end_endure),
    ];
    // Every volatile of the table: a Pokémon with none of them has nothing
    // to collect.
    const ANY: u128 = Volatiles::LEECH_SEED
        | Volatiles::CURSE
        | Volatiles::PARTIALLY_TRAPPED
        | Volatiles::SALT_CURE
        | Volatiles::SYRUP_BOMB
        | Volatiles::OCTOLOCK
        | Volatiles::MAGNET_RISE
        | Volatiles::HEAL_BLOCK
        | Volatiles::THROAT_CHOP
        | Volatiles::YAWN
        | Volatiles::PERISH_SONG
        | Volatiles::ENDURE;
    debug_assert_eq!(RESIDUAL.iter().fold(0, |a, e| a | e.0), ANY);
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            let Some(m) = battle.state.at(pos) else { continue };
            if !m.volatiles.has(ANY) {
                continue;
            }
            for &(flag, order, run) in &RESIDUAL {
                if m.volatiles.has(flag) {
                    handlers.push(Handler {
                        order,
                        callback: Callback::Plain(run),
                        ..pokemon_handler(battle, pos, flag)
                    });
                }
            }
        }
    }
}

/// Counts a duration down; at zero the volatile ends (`removeVolatile`,
/// with its `onEnd`). Returns whether it is still up (so `onResidual` runs
/// and faints are reported after it).
fn tick(battle: &mut Battle, pos: Position, flag: u128, counter: fn(&mut VolatileCounters) -> &mut u8) -> bool {
    let Some(m) = battle.state.at_mut(pos) else { return false };
    if !m.volatiles.has(flag) {
        return false;
    }
    let turns = counter(&mut m.volatile_counters);
    *turns = turns.saturating_sub(1);
    if *turns == 0 {
        remove(battle, pos, flag);
        return false;
    }
    true
}

/// Fairy Lock's duration (collected by `field::residual_handlers` among the
/// pseudo-weathers).
pub(crate) fn tick_fairy_lock(battle: &mut Battle, _: &Handler) -> bool {
    // No `onFieldEnd`: it ends silently.
    let turns = &mut battle.state.field.fairy_lock_turns;
    *turns = turns.saturating_sub(1);
    if *turns == 0 {
        field::pseudo_weather_ended(battle, PseudoWeather::FairyLock);
        return false;
    }
    true
}

/// Leech Seed: 1/8 of the seeded Pokémon's max HP goes to whoever stands
/// in the seeder's slot now (nothing if that Pokémon has fainted).
fn leech_seed_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let Some(m) = battle.state.at(pos).filter(|m| m.volatiles.has(Volatiles::LEECH_SEED)) else { return false };
    let (slot, max_hp) = (m.volatile_counters.leech_seed_slot, m.max_hp() as u32);
    if battle.state.at(slot).is_none_or(|t| t.is_fainted()) {
        return true;
    }
    let dealt = effect_damage(battle, pos, super::hp_fraction(max_hp, 1, 8), VolatileEffect::LeechSeed, Some(slot));
    if dealt > 0 {
        effect_heal(battle, slot, dealt as u32, VolatileEffect::LeechSeed, pos);
    }
    true
}

/// Curse: 1/4 of max HP each turn.
fn curse_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let Some(m) = battle.state.at(pos).filter(|m| m.volatiles.has(Volatiles::CURSE)) else { return false };
    let amount = super::hp_fraction(m.max_hp() as u32, 1, 4);
    effect_damage(battle, pos, amount, VolatileEffect::Curse, None);
    true
}

/// `partiallytrapped`: counts down; ends silently once the trapper has
/// left, fainted or not been in for a turn; otherwise 1/8 of max HP.
fn partially_trapped_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    if !tick(battle, pos, Volatiles::PARTIALLY_TRAPPED, |c| &mut c.partially_trapped) {
        return false;
    }
    let m = mon(battle, pos);
    let (by, move_id, max_hp) =
        (m.volatile_counters.partially_trapped_by, m.volatile_counters.partially_trapped_move, m.max_hp() as u32);
    let effect = VolatileEffect::PartiallyTrapped(move_id);
    let trapper_up =
        active_at(battle, by).and_then(|p| battle.state.at(p)).is_some_and(|t| t.hp > 0 && t.active_turns > 0);
    if !trapper_up {
        mon_mut(battle, pos).volatiles.clear(Volatiles::PARTIALLY_TRAPPED);
        let actor = actor(battle, pos);
        battle.emit(BattleEvent::EffectEnd { actor, effect, silent: true });
        return true;
    }
    let divisor = match mon(battle, pos).volatile_counters.bound_divisor {
        0 => 8,
        d => d as u32,
    };
    effect_damage(battle, pos, super::hp_fraction(max_hp, 1, divisor), effect, None);
    true
}

/// Salt Cure: 1/8 of max HP, 1/4 for Water and Steel types (the Champions
/// mod halves both).
fn salt_cure_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let Some(m) = battle.state.at(pos).filter(|m| m.volatiles.has(Volatiles::SALT_CURE)) else { return false };
    let weak = has_type(m, Type::Water) || has_type(m, Type::Steel);
    let champions = battle.format.mechanics == Mechanics::Champions;
    let denominator = match (champions, weak) {
        (false, true) => 4,
        (false, false) | (true, true) => 8,
        (true, false) => 16,
    };
    let amount = super::hp_fraction(m.max_hp() as u32, 1, denominator);
    effect_damage(battle, pos, amount, VolatileEffect::SaltCure, None);
    true
}

/// Syrup Bomb: counts down, then lowers Speed by 1.
fn syrup_bomb_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    if !tick(battle, pos, Volatiles::SYRUP_BOMB, |c| &mut c.syrup_bomb) {
        return false;
    }
    // `effectState.source` is the Pokémon even if it has just fainted (its
    // `onUpdate` ends Syrup Bomb only afterwards), so Mist still sees
    // someone else's drop: its position, active or not.
    let (side, id) = mon(battle, pos).volatile_counters.syrup_bomb_by;
    let source = battle.state.side(side).position_of(id).map(|p| Position::new(side, p as u8));
    boosts::boost(battle, pos, StatBoosts::new([0, 0, 0, 0, -1, 0, 0]), source, false, false);
    true
}

fn tick_magnet_rise(battle: &mut Battle, handler: &Handler) -> bool {
    tick(battle, holder(handler), Volatiles::MAGNET_RISE, |c| &mut c.magnet_rise)
}

fn tick_heal_block(battle: &mut Battle, handler: &Handler) -> bool {
    tick(battle, holder(handler), Volatiles::HEAL_BLOCK, |c| &mut c.heal_block)
}

fn tick_throat_chop(battle: &mut Battle, handler: &Handler) -> bool {
    tick(battle, holder(handler), Volatiles::THROAT_CHOP, |c| &mut c.throat_chop)
}

/// Yawn: the Pokémon falls asleep when it runs out (in `remove`).
fn tick_yawn(battle: &mut Battle, handler: &Handler) -> bool {
    tick(battle, holder(handler), Volatiles::YAWN, |c| &mut c.yawn)
}

/// Perish Song: counts down and announces the count; at 0 the Pokémon
/// faints (in `remove`).
fn perish_song_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    if !tick(battle, pos, Volatiles::PERISH_SONG, |c| &mut c.perish_song) {
        return false;
    }
    let count = mon(battle, pos).volatile_counters.perish_song;
    let actor = actor(battle, pos);
    battle.emit(BattleEvent::PerishCount { actor, count, silent: false });
    true
}

/// Endure lasts one turn.
fn end_endure(battle: &mut Battle, handler: &Handler) -> bool {
    remove(battle, holder(handler), Volatiles::ENDURE);
    false
}

/// Volatiles with an `onDisableMove` handler implemented here.
pub(crate) const DISABLE_MOVE: u128 = Volatiles::TORMENT | Volatiles::HEAL_BLOCK | Volatiles::THROAT_CHOP;

/// The foes' `onFoeDisableMove` handlers (Imprison) in `runEvent('DisableMove',
/// pokemon)`, collected after the Pokémon's own.
pub(crate) fn disable_move_handlers<const N: usize>(
    battle: &Battle,
    pos: Position,
    handlers: &mut super::handlers::SpillList<N>,
) {
    for foe in foes(battle, pos) {
        if battle.state.at(foe).is_some_and(|f| f.volatiles.has(Volatiles::IMPRISON)) {
            handlers.push(pokemon_handler(battle, foe, Volatiles::IMPRISON));
        }
    }
}

/// Whether this module's own `DisableMove` handlers disable `move_id` for
/// the Pokémon at `pos`: Torment (the last move, unless Struggle), Heal
/// Block (healing moves), Throat Chop (sound moves).
pub(crate) fn disabled_by(battle: &Battle, pos: Position, move_id: MoveId) -> bool {
    let m = mon(battle, pos);
    let v = m.volatiles;
    let flags = battle.format.dex.move_(move_id).flags;
    v.has(Volatiles::TORMENT) && m.last_move == Some(move_id) && move_id != ids::STRUGGLE
        || v.has(Volatiles::HEAL_BLOCK) && flags.contains(MoveFlags::HEAL)
        || v.has(Volatiles::THROAT_CHOP) && flags.contains(MoveFlags::SOUND)
}

/// Whether a foe of the Pokémon at `pos` has Imprison: without one,
/// [`imprisoned`] is false for every move.
pub(crate) fn foe_imprisons(battle: &Battle, pos: Position) -> bool {
    foes(battle, pos).any(|f| mon(battle, f).volatiles.has(Volatiles::IMPRISON))
}

/// A foe's Imprison (`onFoeDisableMove`) disables the Pokémon's move
/// `index` if the foe knows it. Showdown's hidden disable: a choice can't
/// pick the move, but the last active Pokémon's request still shows it.
/// Never a Struggle in a move slot (`if (moveSlot.id === 'struggle')
/// continue`), as its `onFoeBeforeMove` lets Struggle through.
pub(crate) fn imprisoned(battle: &Battle, pos: Position, index: usize) -> bool {
    let move_id = mon(battle, pos).moves[index].id;
    if move_id == ids::STRUGGLE {
        return false;
    }
    foes(battle, pos).any(|f| {
        let f = mon(battle, f);
        f.volatiles.has(Volatiles::IMPRISON) && f.moves.iter().any(|s| s.id == move_id)
    })
}

/// Octolock's `onResidual` (`onResidualOrder: 14`): ends silently
/// (`-end|<target>|Octolock|[partiallytrapped]|[silent]`) once its source
/// has left, fainted or not been in for a turn; otherwise
/// `boost({def: -1, spd: -1}, pokemon, source, getActiveMove('octolock'))`.
fn octolock_residual(battle: &mut Battle, handler: &Handler) -> bool {
    let pos = holder(handler);
    let Some(m) = battle.state.at(pos).filter(|m| m.volatiles.has(Volatiles::OCTOLOCK)) else { return false };
    let by = m.volatile_counters.octolock_by;
    let source = active_at(battle, by).filter(|&p| battle.state.at(p).is_some_and(|t| t.hp > 0 && t.active_turns > 0));
    let Some(source) = source else {
        mon_mut(battle, pos).volatiles.clear(Volatiles::OCTOLOCK);
        let actor = actor(battle, pos);
        battle.emit(BattleEvent::EffectEnd { actor, effect: VolatileEffect::Octolock, silent: true });
        return true;
    };
    let drops = StatBoosts::new([0, -1, 0, -1, 0, 0, 0]);
    let effect = super::events::Effect::Move(ids::OCTOLOCK);
    boosts::boost_from(battle, pos, drops, Some(source), effect, false, false);
    true
}

/// Magnet Rise's `onImmunity(type)`: `false` for `'Ground'`. Showdown runs
/// the `Immunity` event only from `runStatusImmunity`, with a status or
/// condition id (`'par'`, `'trapped'`, `'sandstorm'`), never a type (a
/// Ground move's immunity is `isGrounded`, which reads the volatile), so
/// the handler never fires. It stays registered: it is collected and
/// sorted with the event's other handlers, as in Showdown, and a speed tie
/// among them draws.
pub(crate) fn magnet_rise_immunity(
    _: &mut Battle,
    _: &mut super::events::EventCtx<'_>,
    _: &Handler,
) -> Option<super::events::Relay> {
    None
}

/// The trapping conditions' `onTrapPokemon`: `pokemon.tryTrap()`, whose
/// `runStatusImmunity('trapped')` runs the `Immunity` event (its sort can
/// draw) unless the Pokémon is a Ghost; `partiallytrapped` only while its
/// trapper is active. Whether it is trapped is read from the state later
/// ([`is_trapped`]).
fn try_trap_handler(battle: &mut Battle, h: &Handler) -> bool {
    let pos = match h.holder {
        Holder::Pokemon(p) | Holder::FieldOn(p) => p,
        _ => return true,
    };
    let Some(m) = battle.state.at(pos) else { return true };
    if h.volatile() == Volatiles::PARTIALLY_TRAPPED
        && active_at(battle, m.volatile_counters.partially_trapped_by).is_none()
    {
        return true;
    }
    // Octolock's `onTrapPokemon`: `if (this.effectState.source?.isActive)`.
    if h.volatile() == Volatiles::OCTOLOCK && active_at(battle, m.volatile_counters.octolock_by).is_none() {
        return true;
    }
    if !has_type(m, Type::Ghost) {
        super::events::immunity_event(battle, pos, "trapped");
    }
    true
}

/// `runEvent('TrapPokemon', pokemon)` at the end of the turn. Whether the
/// Pokémon is trapped is read from the state when the request is built
/// ([`is_trapped`]); only the handler sort matters here, as it draws when
/// two of the Pokémon's own trapping volatiles tie.
pub(crate) fn trap_pokemon_event(battle: &mut Battle, pos: Position) {
    let Some(m) = battle.state.at_mut(pos) else { return };
    // `pokemon.trapped = pokemon.maybeTrapped = false`.
    m.trapped = false;
    m.trap_released = false;
    let m = battle.state.at(pos).expect("checked above");
    let mut handlers = super::handlers::MonHandlers::new();
    // Ingrain and Octolock have `onTrapPokemon` too.
    const TRAPS: u128 = Volatiles::TRAPPED
        | Volatiles::PARTIALLY_TRAPPED
        | Volatiles::NO_RETREAT
        | Volatiles::INGRAIN
        | Volatiles::OCTOLOCK;
    // In the order they were added (none: nothing to walk).
    if m.volatiles.has(TRAPS) {
        for v in m.volatiles.in_order() {
            if v & TRAPS != 0 {
                handlers
                    .push(Handler { callback: Callback::Plain(try_trap_handler), ..pokemon_handler(battle, pos, v) });
            }
        }
    }
    if battle.state.field.fairy_lock_turns > 0 {
        // Sorted with the sub-order its state has now; running it gives the
        // state a `target` (see `FieldState::pseudo_targeted`). A field
        // handler: speed 0.
        let sub_order = if field::pseudo_targeted(battle, PseudoWeather::FairyLock) { 5 } else { 2 };
        handlers.push(Handler {
            sub_order,
            holder: Holder::FieldOn(pos),
            callback: Callback::Plain(try_trap_handler),
            ..Handler::default()
        });
    }
    // The abilities' and items' (Shadow Tag, Arena Trap, Magnet Pull as
    // `onFoeTrapPokemon`; Shed Shell), sorted with them.
    let target = super::events::Target::Pokemon(pos);
    super::events::collect_into(battle, pikapro_data::Event::TrapPokemon, target, None, &mut handlers);
    speed_sort(&mut handlers, &mut battle.rng, Handler::compare);
    if battle.state.field.fairy_lock_turns > 0 {
        field::set_pseudo_targeted(battle, PseudoWeather::FairyLock);
    }
    let effect = super::events::Effect::None;
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::TrapPokemon,
        target,
        None,
        effect,
        super::events::Relay::True,
    );
    super::events::run_list(battle, &mut ev, &handlers);
    // `runEvent('MaybeTrapPokemon')` unless the Pokémon is a known Ghost.
    let m = mon(battle, pos);
    if m.volatile_counters.type_unknown || !has_type(m, Type::Ghost) {
        super::run_mon_event(battle, pikapro_data::Event::MaybeTrapPokemon, pos);
    }
}

/// `pokemon.addVolatile('attract', source, effect)` from an ability or an
/// item (Destiny Knot): `runStatusImmunity('attract')`,
/// `TryAddVolatile`, then `attract`'s `onStart` (genders, the `Attract`
/// event), announced `-start|<pokemon>|Attract|[from] <effect>|[of]
/// <source>`. Returns whether the Pokémon is now attracted.
pub(crate) fn attract_from(
    battle: &mut Battle,
    target: Position,
    source: Position,
    effect: crate::event::EffectName,
) -> bool {
    let Some(m) = battle.state.at(target) else { return false };
    if m.is_fainted() || m.volatiles.has(Volatiles::ATTRACT) {
        return false;
    }
    if super::events::immunity_event(battle, target, "attract") != Some(true) {
        return false;
    }
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::TryAddVolatile,
        super::events::Target::Pokemon(target),
        Some(source),
        super::abilities::effect_of(effect),
        super::events::Relay::Id("attract"),
    );
    if !super::events::run_event(battle, &mut ev, None).is_truthy() {
        return false;
    }
    if !opposite_genders(battle, target, source) {
        return false;
    }
    // The volatile is present during its `onStart`.
    mon_mut(battle, target).volatiles.add(Volatiles::ATTRACT);
    if !super::run_mon_event_from(battle, pikapro_data::Event::Attract, target, Some(source)).is_truthy() {
        mon_mut(battle, target).volatiles.clear(Volatiles::ATTRACT);
        return false;
    }
    let (actor, of) = (actor(battle, target), actor(battle, source));
    let cause = crate::event::Cause { effect, of: Some(of) };
    battle.emit(BattleEvent::EffectStartEnd {
        actor,
        start: true,
        what: crate::event::LineArg::Text("Attract"),
        effect: None,
        cause: Some(cause),
        silent: false,
    });
    let by = key(battle, source);
    let m = mon_mut(battle, target);
    m.volatile_counters.attract_by = by;
    true
}

/// Attract's and Syrup Bomb's `onUpdate` (the `Update` event): they end
/// once their source has left the field.
pub(crate) fn source_left_update(
    battle: &mut Battle,
    _: &mut super::events::EventCtx<'_>,
    h: &Handler,
) -> Option<super::events::Relay> {
    let pos = holder(h);
    let m = battle.state.at(pos).filter(|m| m.volatiles.has(h.volatile()))?;
    let source = match h.volatile() {
        Volatiles::ATTRACT => m.volatile_counters.attract_by,
        _ => m.volatile_counters.syrup_bomb_by,
    };
    if active_at(battle, source).is_none() {
        remove(battle, pos, h.volatile());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battle::BattleOptions;
    use crate::format::Format;
    use crate::rng::Seed;
    use crate::testing::sample_team;

    /// Imprison's `onFoeDisableMove` skips a Struggle in a move slot, even
    /// one the Imprison user has too.
    #[test]
    fn imprison_never_disables_a_struggle_slot() {
        let format = Format::gen9_singles();
        let teams = [sample_team(&format), sample_team(&format)];
        let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
        let (p1, p2) = (Position::new(SideId::P1, 0), Position::new(SideId::P2, 0));
        // The same lead on both sides: every move is shared.
        b.state.at_mut(p2).unwrap().volatiles.set(Volatiles::IMPRISON);
        assert!(imprisoned(&b, p1, 0));
        for pos in [p1, p2] {
            b.state.at_mut(pos).unwrap().moves[0].id = ids::STRUGGLE;
        }
        assert!(!imprisoned(&b, p1, 0));
        assert!(imprisoned(&b, p1, 1));
    }
}
