//! Damage formula callbacks.
//!
//! The move callbacks that feed Showdown's `getDamage` / `modifyDamage`
//! (`sim/battle-actions.ts`) and the moves' own events around them
//! (`data/moves.ts`):
//!
//! - `onModifyType` / `onModifyMove` (Weather Ball, Tera Blast, Present,
//!   Shell Side Arm, Expanding Force, Hurricane, ...): the pipeline copies
//!   the move's data (Showdown's `ActiveMove`) and [`modify_move`] edits it
//!   before the `|move|` line.
//! - `onModifyPriority` (Grassy Glide): [`modify_priority`].
//! - `onTryMove` (Echoed Voice, Double Shock) and `onTry` (Aura Wheel,
//!   Hyperspace Fury): [`try_move`], [`on_try`].
//! - `onTryImmunity` (Endeavor, Dream Eater): [`try_immunity`].
//! - `basePowerCallback`: [`base_power_callback`]; `onBasePower`:
//!   [`on_base_power`]; `damageCallback`: [`damage_callback`].
//! - `willCrit`, `overrideOffensivePokemon`, `overrideOffensiveStat`,
//!   `overrideDefensiveStat`, `ignoreDefensive`, `ignoreEvasion`,
//!   `onEffectiveness`: [`will_crit`], [`uses_target_attack`],
//!   [`offensive_stat`], [`defensive_stat`], [`ignores_defensive`],
//!   [`ignores_evasion`], [`run_effectiveness`].
//! - `onDamage` (False Swipe): [`own_damage_spec`].
//! - `selfBoost`, `onMoveFail` (crash damage), `onAfterMoveSecondarySelf`
//!   (Fell Stinger): [`after_use_move`]; `mindBlownRecoil` /
//!   `chloroblastRecoil`: [`recoil`]; `self.chance` (Diamond Storm):
//!   [`self_chance`].
//!
//! It also keeps the history those callbacks read (Showdown's
//! `hurtThisTurn`, `attackedBy`, `timesAttacked`, `statsLoweredThisTurn`,
//! `moveThisTurnResult`, `newlySwitched`, `lastSuccessfulMoveThisTurn`,
//! `faintedLastTurn`, `totalFainted`) and the Fury Cutter and Echoed Voice
//! conditions.

use pikapro_data::gen9::{moves as ids, species};
use pikapro_data::{
    Event, Fraction, MoveCategory, MoveData, MoveFlags, MoveId, MoveTarget, SpeciesId, Stat, StatBoosts, Type,
};

use super::events::{self, Effect, EventCtx, Relay, Target};
use super::handlers::{Callback, Handler, Handlers, Holder, sub_order, vol};
use super::manipulation;
use super::queue::MonRef;
use super::status::poisoned;
use super::targeting;
use super::{DamageSource, actor, apply_damage, boosts, field};
use crate::battle::Battle;
use crate::event::{BattleEvent, FailNote};
use crate::format::Mechanics;
use crate::state::{MoveResult, PokemonState, Position, PseudoWeather, SideId, Status, Terrain, Volatiles, Weather};
use crate::stats::apply_stage;
use crate::typing;
use crate::util::StackVec;

// ---------------------------------------------------------------------------
// Helpers.

/// The Pokémon in `pos`'s slot, fainted or not (a move's target can be a
/// Pokémon that fainted earlier in the turn).
fn mon(battle: &Battle, pos: Position) -> &PokemonState {
    &battle.state.side(pos.side).team[pos.slot as usize]
}

/// Showdown's `pokemon.effectiveWeather()` for the Pokémon at `pos`, from
/// `move_id`'s own callback (Cloud Nine / Air Lock, Utility Umbrella hiding
/// sun and rain, and Mega Sol making it sun).
fn weather(battle: &Battle, pos: Position, move_id: MoveId) -> Option<Weather> {
    super::mega::pokemon_weather(battle, pos, super::mega::WeatherAsker::Move(move_id))
}

fn sunny(battle: &Battle, pos: Position, move_id: MoveId) -> bool {
    matches!(weather(battle, pos, move_id), Some(Weather::Sun | Weather::HarshSun))
}

fn rainy(battle: &Battle, pos: Position, move_id: MoveId) -> bool {
    matches!(weather(battle, pos, move_id), Some(Weather::Rain | Weather::HeavyRain))
}

/// `getStat(stat, false, true)`: boosted, unmodified.
fn boosted_stat(mon: &PokemonState, stat: Stat) -> u32 {
    apply_stage(mon.stats[stat], mon.boosts.stat(stat)) as u32
}

/// `getStat('spe')`: boosted and modified (`ModifyBoost`, `ModifySpe`), as
/// in `mon_action_speed` before Trick Room.
fn speed(battle: &mut Battle, pos: Position) -> u32 {
    super::events::get_stat(battle, pos, Stat::Spe, false, false)
}

/// `pokemon.getWeight()` in hectograms: `runEvent('ModifyWeight')`
/// (Float Stone, Heavy Metal, Light Metal), at least 1.
/// (Autotomize's weight loss: a Past move in Gen 9 and Champions.)
fn weight(battle: &mut Battle, pos: Position) -> u32 {
    let m = mon(battle, pos);
    // Transform copies `weighthg`.
    let base = battle.format.dex.species(super::tera::species_of(m)).weight_hg as u32;
    let event = pikapro_data::Event::ModifyWeight;
    let effect = super::events::Effect::None;
    super::events::run_modify(battle, event, pos, None, effect, base, None, None).max(1)
}

fn has_status(m: &PokemonState) -> bool {
    m.status != Status::None
}

/// The index of a Pokémon in `damaged_by` bit sets.
pub(crate) fn attacker_bit(side: SideId, id: u8) -> u16 {
    1 << (6 * side.index() + id as usize)
}

/// Status moves whose callbacks this module implements.
pub(crate) fn handles(move_id: MoveId) -> bool {
    move_id == ids::GROWTH
}

// ---------------------------------------------------------------------------
// `onModifyType` / `onModifyMove` / `onModifyPriority`.

/// The move's own `onModifyType` and `onModifyMove` (Showdown's
/// `singleEvent('ModifyType')` / `singleEvent('ModifyMove')` in
/// `useMoveInner`), applied to `data`, the active move. Returns the target:
/// a move whose target type changed is re-aimed (`getRandomTarget`, which
/// can draw).
pub(crate) fn modify_move(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    data: &mut MoveData,
    target: Option<Position>,
) -> Option<Position> {
    let base_target = data.target;
    modify_type(battle, user, move_id, data);
    on_modify_move(battle, user, move_id, data, target);
    if data.target != base_target {
        return targeting::get_random_target(battle, user, data.target);
    }
    target
}

/// After `runEvent('ModifyType')` / `runEvent('ModifyMove')`, Showdown
/// compares the target type with the original again, so a move whose
/// target type changed draws a second random target.
pub(crate) fn retarget(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    target: Option<Position>,
) -> Option<Position> {
    if data.target != battle.format.dex.move_(move_id).target {
        return targeting::get_random_target(battle, user, data.target);
    }
    target
}

/// The move's own `onModifyType` ([`typing::own_move_type`], with the
/// user's facts from the state), and Tera Starstorm's category.
fn modify_type(battle: &Battle, user: Position, move_id: MoveId, data: &mut MoveData) {
    if !typing::has_own_type(move_id) {
        return;
    }
    let m = mon(battle, user);
    // Terapagos-Stellar's is Stellar, and physical when terastallized
    // with more Attack than Special Attack.
    if move_id == ids::TERA_STARSTORM
        && super::tera::species_of(m) == species::TERAPAGOS_STELLAR
        && m.terastallized
        && boosted_stat(m, Stat::Atk) > boosted_stat(m, Stat::SpA)
    {
        data.category = MoveCategory::Physical;
    }
    let facts = move_user(battle, user, move_id);
    if let Ok(Some(ty)) = typing::own_move_type(battle.format.dex, move_id, &facts) {
        data.ty = ty;
    }
}

/// What [`typing`]'s rules read of the Pokémon at `pos` using `move_id`,
/// all known, from the state: its species (the one it transformed into),
/// `getTypes()`, Tera type, effective item and ability, `isGrounded`,
/// Electrify, its effective weather for the move, the terrain.
pub(crate) fn move_user(battle: &Battle, pos: Position, move_id: MoveId) -> typing::MoveUser {
    let m = mon(battle, pos);
    typing::MoveUser {
        species: super::tera::species_of(m),
        types: manipulation::all_types(m),
        tera: m.terastallized.then_some(m.tera_type),
        item: Some(super::items::effective_item(battle, pos)),
        ability: typing::AbilityFact::Known(super::abilities::effective_ability(battle, pos)),
        grounded: Some(field::is_grounded(battle, pos)),
        electrified: m.volatiles.has(Volatiles::ELECTRIFY),
        weather: Some(weather(battle, pos, move_id)),
        terrain: battle.state.field.terrain,
    }
}

fn on_modify_move(battle: &mut Battle, user: Position, move_id: MoveId, data: &mut MoveData, target: Option<Position>) {
    let m = mon(battle, user);
    match move_id {
        // `move.multihit = move.allies.length`.
        ids::BEAT_UP => {
            let allies = beat_up_allies(battle, user);
            let n = allies.len() as u8;
            battle.move_state.beat_up = allies;
            data.multihit = Some((n, n));
        }
        ids::EXPANDING_FORCE => {
            if battle.state.field.terrain == Some(Terrain::Psychic) && field::is_grounded(battle, user) {
                data.target = MoveTarget::AllAdjacentFoes;
            }
        }
        ids::TERA_BLAST => {
            if m.terastallized && boosted_stat(m, Stat::Atk) > boosted_stat(m, Stat::SpA) {
                data.category = MoveCategory::Physical;
            }
            // Stellar: `move.self = {boosts: {atk: -1, spa: -1}}`.
            if m.terastallized && m.tera_type == Type::Stellar {
                data.self_boosts = Some(StatBoosts::new([-1, 0, -1, 0, 0, 0, 0]));
            }
        }
        ids::TERA_STARSTORM => {
            if super::tera::species_of(m) == species::TERAPAGOS_STELLAR {
                data.target = MoveTarget::AllAdjacentFoes;
            }
        }
        ids::TERRAIN_PULSE => {
            if battle.state.field.terrain.is_some() && field::is_grounded(battle, user) {
                data.base_power *= 2;
            }
        }
        ids::WEATHER_BALL => {
            if matches!(
                weather(battle, user, ids::WEATHER_BALL),
                Some(
                    Weather::Sun
                        | Weather::HarshSun
                        | Weather::Rain
                        | Weather::HeavyRain
                        | Weather::Sand
                        | Weather::Snow
                )
            ) {
                data.base_power *= 2;
            }
        }
        // `field.isWeather`: not per Pokémon.
        ids::BLIZZARD => {
            if super::abilities::effective_weather(battle) == Some(Weather::Snow) {
                data.accuracy = None;
            }
        }
        // `target?.effectiveWeather()`.
        ids::HURRICANE | ids::THUNDER => {
            let Some(t) = target else { return };
            if rainy(battle, t, move_id) {
                data.accuracy = None;
            } else if sunny(battle, t, move_id) {
                data.accuracy = Some(50);
            }
        }
        ids::BLEAKWIND_STORM | ids::SANDSEAR_STORM | ids::WILDBOLT_STORM => {
            let Some(t) = target else { return };
            if rainy(battle, t, move_id) {
                data.accuracy = None;
            }
        }
        ids::GROWTH => {
            // `onModifyMove`: `pokemon.effectiveWeather()` in sun or harsh
            // sun (Mega Sol's sun for the move included) makes it +2 / +2.
            if sunny(battle, user, ids::GROWTH) {
                data.boosts = Some(StatBoosts::new([2, 0, 2, 0, 0, 0, 0]));
            }
        }
        ids::PRESENT => {
            let rand = battle.rng.random(10);
            if rand < 2 {
                // `infiltrates` (through a substitute) is in
                // `conditions::try_primary_hit`.
                data.heal = Some(Fraction { numerator: 1, denominator: 4 });
            } else if rand < 6 {
                data.base_power = 40;
            } else if rand < 9 {
                data.base_power = 80;
            } else {
                data.base_power = 120;
            }
        }
        ids::PHOTON_GEYSER => {
            if boosted_stat(m, Stat::Atk) > boosted_stat(m, Stat::SpA) {
                data.category = MoveCategory::Physical;
            }
        }
        ids::SHELL_SIDE_ARM => {
            let Some(t) = target else { return };
            // `getStat(stat, false, true)`: boosted, unmodified (under
            // Wonder Room a defence takes the other one's stage).
            let stat = |battle: &mut Battle, pos, s| super::events::get_stat(battle, pos, s, false, true);
            let base = (2 * m.level as u32 / 5 + 2) * 90;
            let (atk, spa) = (stat(battle, user, Stat::Atk), stat(battle, user, Stat::SpA));
            let (def, spd) = (stat(battle, t, Stat::Def), stat(battle, t, Stat::SpD));
            let physical = base * atk / def.max(1) / 50;
            let special = base * spa / spd.max(1) / 50;
            if physical > special || (physical == special && battle.rng.chance(1, 2)) {
                data.category = MoveCategory::Physical;
                data.flags = data.flags | MoveFlags::CONTACT;
            }
        }
        _ => {}
    }
}

/// Grassy Glide's `onModifyPriority`: the change to the move's priority.
pub(crate) fn modify_priority(battle: &Battle, user: Position, move_id: MoveId) -> i8 {
    let grassy = battle.state.field.terrain == Some(Terrain::Grassy);
    i8::from(move_id == ids::GRASSY_GLIDE && grassy && field::is_grounded(battle, user))
}

/// Beat Up's `move.allies`: the user, and every team member that has not
/// fainted and has no status, in team order.
fn beat_up_allies(battle: &Battle, user: Position) -> StackVec<SpeciesId, 6> {
    let side = battle.state.side(user.side);
    let mut allies = StackVec::new();
    for (i, ally) in side.team.iter().enumerate() {
        if i == user.slot as usize || (!ally.is_fainted() && ally.status == Status::None) {
            // `set.species` (a forme ability's forme is not it).
            allies.push(ally.set_species);
        }
    }
    allies
}

// ---------------------------------------------------------------------------
// `onTryMove`, `onTry`, `onTryImmunity`.

/// The move's own `onTryMove` (after the `|move|` line). Returns false to
/// stop the move (a `null` result).
pub(crate) fn try_move(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    match move_id {
        ids::ECHOED_VOICE => {
            add_echoed_voice(battle, user);
            true
        }
        ids::DOUBLE_SHOCK | ids::BURN_UP => {
            let ty = if move_id == ids::DOUBLE_SHOCK { Type::Electric } else { Type::Fire };
            if manipulation::has_type(mon(battle, user), ty) {
                return true;
            }
            fail_move(battle, user, move_id, false);
            set_move_result(battle, user, MoveResult::Null);
            false
        }
        _ => true,
    }
}

/// The move's own `onTry` (Showdown's `singleEvent('Try')` in
/// `trySpreadMoveHit`). Returns false to stop the move; the failure is
/// already announced.
pub(crate) fn on_try(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    let m = mon(battle, user);
    match move_id {
        // `pokemon.species`: a Pokémon transformed into Morpeko can use it.
        ids::AURA_WHEEL if battle.format.dex.species(super::tera::species_of(m)).base_species != species::MORPEKO => {
            fail_move(battle, user, move_id, false);
            battle.hint(crate::event::Hint::OnlyForme(move_id));
            false
        }
        ids::HYPERSPACE_FURY if super::tera::species_of(m) != species::HOOPA_UNBOUND => {
            let forme = super::tera::species_of(m) == species::HOOPA;
            battle.hint(crate::event::Hint::OnlyForme(move_id));
            fail_move(battle, user, move_id, forme);
            false
        }
        _ => true,
    }
}

/// `attrLastMove('[still]')`, then `-fail|user|move: X`.
fn fail_move(battle: &mut Battle, user: Position, move_id: MoveId, forme: bool) {
    battle.flush_rng_trace();
    battle.attr_last_move(crate::event::MoveAttr::Still);
    let user = actor(battle, user);
    battle.emit(BattleEvent::MoveFail { user, move_id, note: if forme { FailNote::Forme } else { FailNote::None } });
}

/// The move's own `onTryImmunity` for one target (`hitStepTryImmunity`):
/// false makes the target immune (`-immune`).
pub(crate) fn try_immunity(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    let ok = match move_id {
        ids::ENDEAVOR => mon(battle, user).hp < mon(battle, target).hp,
        // Comatose counts as asleep.
        ids::DREAM_EATER => {
            matches!(mon(battle, target).status, Status::Sleep { .. })
                || super::abilities::has_ability(battle, target, pikapro_data::gen9::abilities::COMATOSE)
        }
        _ => true,
    };
    if !ok {
        let target = actor(battle, target);
        battle.emit(BattleEvent::Immune { target });
    }
    ok
}

// ---------------------------------------------------------------------------
// `getDamage`.

/// Whether the move has a `basePowerCallback` (the Tera 60 BP floor skips
/// variable-power moves whose data base power is 0 or 150).
pub(crate) fn has_base_power_callback(data: &MoveData) -> bool {
    data.unmodelled.contains(&"basePowerCallback")
}

/// The move's `basePowerCallback`, before `clampIntRange(bp, 1)`; `None`
/// if this module does not implement one for the move. `data` is the active
/// move (`move.basePower` there).
pub(crate) fn base_power_callback(
    battle: &mut Battle,
    user: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
    hit: u8,
) -> Option<u32> {
    let base = data.base_power as u32;
    let u = mon(battle, user);
    let t = mon(battle, target);
    let doubled = |cond: bool| if cond { base * 2 } else { base };
    Some(match move_id {
        ids::ACROBATICS => doubled(u.item.is_none()),
        ids::ASSURANCE => doubled(t.volatile_counters.hurt_this_turn),
        ids::AVALANCHE => {
            let bit = attacker_bit(target.side, t.id);
            doubled(u.volatile_counters.damaged_by & bit != 0)
        }
        ids::BEAT_UP => {
            // `move.allies.shift()`: one ally per hit, from the list its
            // `onModifyMove` fixed.
            let ally = battle.move_state.beat_up.get(hit as usize - 1).copied().unwrap_or(u.species);
            5 + battle.format.dex.species(ally).base_stats[Stat::Atk] as u32 / 10
        }
        ids::CRUSH_GRIP | ids::HARD_PRESS => {
            let max = if move_id == ids::CRUSH_GRIP { 120 } else { 100 };
            let ratio = t.hp as u32 * 4096 / t.max_hp() as u32;
            ((max * (100 * ratio) + 2048 - 1) / 4096 / 100).max(1)
        }
        // `move.basePower * pokemon.hp / pokemon.maxhp`: a fraction, which
        // `clampIntRange` floors to at least 1.
        ids::DRAGON_ENERGY | ids::ERUPTION | ids::WATER_SPOUT => (base * u.hp as u32 / u.max_hp() as u32).max(1),
        ids::ECHOED_VOICE => {
            let field = battle.state.field;
            if field.echoed_voice_turns > 0 { base * field.echoed_voice_multiplier as u32 } else { base }
        }
        ids::ELECTRO_BALL => {
            let target_speed = speed(battle, target);
            let ratio = if target_speed == 0 { 0 } else { speed(battle, user) / target_speed };
            [40, 60, 80, 120, 150][ratio.min(4) as usize]
        }
        ids::FLAIL | ids::REVERSAL => {
            let ratio = (u.hp as u32 * 48 / u.max_hp() as u32).max(1);
            match ratio {
                0..2 => 200,
                2..5 => 150,
                5..10 => 100,
                10..17 => 80,
                17..33 => 40,
                _ => 20,
            }
        }
        ids::FURY_CUTTER => fury_cutter(battle, user, data, hit),
        ids::GRASS_KNOT | ids::LOW_KICK => match weight(battle, target) {
            2000.. => 120,
            1000.. => 100,
            500.. => 80,
            250.. => 60,
            100.. => 40,
            _ => 20,
        },
        ids::HEAT_CRASH | ids::HEAVY_SLAM => {
            let (own, other) = (weight(battle, user), weight(battle, target));
            match own {
                w if w >= other * 5 => 120,
                w if w >= other * 4 => 100,
                w if w >= other * 3 => 80,
                w if w >= other * 2 => 60,
                _ => 40,
            }
        }
        ids::GYRO_BALL => {
            let user_speed = speed(battle, user);
            if user_speed == 0 { 1 } else { (25 * speed(battle, target) / user_speed + 1).min(150) }
        }
        // Comatose counts as a status.
        ids::HEX | ids::INFERNAL_PARADE => doubled(
            has_status(t) || super::abilities::has_ability(battle, target, pikapro_data::gen9::abilities::COMATOSE),
        ),
        ids::LAST_RESPECTS => 50 + 50 * battle.state.side(user.side).total_fainted as u32,
        ids::PAYBACK => {
            let will_move = super::queue::will_move(battle, target);
            doubled(t.volatile_counters.settled && !will_move)
        }
        ids::POWER_TRIP | ids::STORED_POWER => {
            let b = u.boosts;
            let positive: i32 =
                [b.atk, b.def, b.spa, b.spd, b.spe, b.accuracy, b.evasion].iter().map(|&s| s.max(0) as i32).sum();
            base + 20 * positive as u32
        }
        ids::RAGE_FIST => (50 + 50 * times_attacked(battle, u) as u32).min(350),
        ids::RISING_VOLTAGE => {
            let grounded = battle.state.field.terrain == Some(Terrain::Electric) && field::is_grounded(battle, target);
            // `if (!source.isAlly(target)) this.hint(...)`.
            if grounded && user.side != target.side {
                battle.hint(crate::event::Hint::GroundedDoubled(move_id));
            }
            doubled(grounded)
        }
        ids::STOMPING_TANTRUM | ids::TEMPER_FLARE => doubled(u.volatile_counters.move_last_turn == MoveResult::Failed),
        // 100 when Terastallized into Stellar.
        ids::TERA_BLAST if u.terastallized && u.tera_type == Type::Stellar => 100,
        ids::TERA_BLAST => base,
        _ => return None,
    })
}

/// Fury Cutter's `basePowerCallback`: (re)starts the `furycutter`
/// volatile, which doubles the power on each consecutive use up to 160;
/// only on the first hit (`move.hit === 1`: Parental Bond's second hit
/// keeps the power).
fn fury_cutter(battle: &mut Battle, user: Position, data: &MoveData, hit: u8) -> u32 {
    // A new `furycutter` runs `TryAddVolatile` first (its ties draw).
    if !battle.state.side(user.side).team[user.slot as usize].volatiles.has(Volatiles::FURY_CUTTER) {
        super::events::try_add_volatile(battle, user, "furycutter");
    }
    let m = &mut battle.state.side_mut(user.side).team[user.slot as usize];
    let c = &mut m.volatile_counters;
    if m.volatiles.has(Volatiles::FURY_CUTTER) && hit != 1 {
        // Parental Bond's second hit.
    } else if m.volatiles.has(Volatiles::FURY_CUTTER) {
        // `onRestart`.
        if c.fury_cutter < 4 {
            c.fury_cutter <<= 1;
        }
    } else {
        // `onStart`.
        m.volatiles.add(Volatiles::FURY_CUTTER);
        c.fury_cutter = 1;
    }
    c.fury_cutter_duration = 2;
    (data.base_power as u32 * c.fury_cutter as u32).clamp(1, 160)
}

fn times_attacked(battle: &Battle, m: &PokemonState) -> u8 {
    match battle.format.mechanics {
        Mechanics::ScarletViolet => m.record.times_attacked,
        Mechanics::Champions => m.volatile_counters.times_attacked,
    }
}

/// The move's own `onBasePower` as a 4096-based chain modifier (4096 =
/// none). It runs after the terrains' handlers (priority 6); Fickle Beam
/// draws here.
pub(crate) fn on_base_power(
    battle: &mut Battle,
    user: Position,
    target: Position,
    move_id: MoveId,
    data: &MoveData,
) -> u32 {
    let (u, t) = (*mon(battle, user), *mon(battle, target));
    let terrain = battle.state.field.terrain;
    let boost = |cond: bool, modifier: u32| if cond { modifier } else { 4096 };
    match move_id {
        ids::BARB_BARRAGE | ids::VENOSHOCK => boost(poisoned(&t), 8192),
        ids::BRINE => boost(t.hp as u32 * 2 <= t.max_hp() as u32, 8192),
        ids::COLLISION_COURSE | ids::ELECTRO_DRIFT => {
            let super_effective = run_effectiveness(battle, target, move_id, data) > 0;
            boost(super_effective, 5461)
        }
        ids::FACADE => boost(!matches!(u.status, Status::None | Status::Sleep { .. }), 8192),
        ids::FICKLE_BEAM => {
            if battle.rng.chance(3, 10) {
                // `attrLastMove('[anim] Fickle Beam All Out')`.
                battle.attr_last_move(crate::event::MoveAttr::Anim(crate::event::MoveAnim::FickleBeamAllOut));
                let actor = actor(battle, user);
                battle.emit(BattleEvent::MoveActivate { actor, move_id, of: None });
                8192
            } else {
                4096
            }
        }
        ids::FUSION_BOLT => boost(battle.state.field.last_successful_move == Some(ids::FUSION_FLARE), 8192),
        ids::FUSION_FLARE => boost(battle.state.field.last_successful_move == Some(ids::FUSION_BOLT), 8192),
        ids::GRAV_APPLE => boost(super::gravity(battle), 6144),
        ids::LASH_OUT => boost(u.volatile_counters.stats_lowered_this_turn, 8192),
        ids::MISTY_EXPLOSION => boost(terrain == Some(Terrain::Misty) && field::is_grounded(battle, user), 6144),
        ids::PSYBLADE => boost(terrain == Some(Terrain::Electric), 6144),
        ids::RETALIATE => boost(battle.state.side(user.side).fainted_last_turn, 8192),
        ids::EXPANDING_FORCE => boost(terrain == Some(Terrain::Psychic) && field::is_grounded(battle, user), 6144),
        // Knock Off.
        _ => super::itemmoves::on_base_power(battle, user, target, move_id),
    }
}

/// `damageCallback`s implemented here.
pub(crate) fn damage_callback(battle: &Battle, user: Position, target: Position, move_id: MoveId) -> Option<u32> {
    match move_id {
        // `target.getUndynamaxedHP() - pokemon.hp`; `onTryImmunity` made
        // sure it is positive.
        ids::ENDEAVOR => Some((mon(battle, target).hp as u32).saturating_sub(mon(battle, user).hp as u32)),
        _ => None,
    }
}

/// `willCrit: true`: the move always crits and skips the crit roll.
pub(crate) fn will_crit(move_id: MoveId) -> bool {
    matches!(
        move_id,
        ids::FLOWER_TRICK | ids::FROST_BREATH | ids::SURGING_STRIKES | ids::WICKED_BLOW | ids::STORM_THROW
    )
}

/// `overrideOffensivePokemon: 'target'` (Foul Play): the target's attacking
/// stat and stages.
pub(crate) fn uses_target_attack(move_id: MoveId) -> bool {
    move_id == ids::FOUL_PLAY
}

/// `overrideOffensiveStat` (Body Press: Defense).
pub(crate) fn offensive_stat(move_id: MoveId) -> Option<Stat> {
    (move_id == ids::BODY_PRESS).then_some(Stat::Def)
}

/// `overrideDefensiveStat` (Psyshock, Psystrike, Secret Sword: Defense).
pub(crate) fn defensive_stat(move_id: MoveId) -> Option<Stat> {
    matches!(move_id, ids::PSYSHOCK | ids::PSYSTRIKE | ids::SECRET_SWORD).then_some(Stat::Def)
}

/// `ignoreDefensive: true`: the target's defensive stages are ignored.
pub(crate) fn ignores_defensive(move_id: MoveId) -> bool {
    matches!(move_id, ids::DARKEST_LARIAT | ids::SACRED_SWORD)
}

/// `ignoreEvasion: true`: the target's evasion stages are ignored.
pub(crate) fn ignores_evasion(move_id: MoveId) -> bool {
    matches!(move_id, ids::DARKEST_LARIAT | ids::SACRED_SWORD)
}

/// Showdown's `target.runEffectiveness(move)`: for each of the target's
/// types (an added type included), the chart's exponent, the move's own
/// `onEffectiveness` (Freeze-Dry, Flying Press), then the `Effectiveness`
/// event (Tar Shot, Delta Stream, abilities), summed. An immune type counts
/// as neutral here (`getEffectiveness`); type immunity is `runImmunity`.
pub(crate) fn run_effectiveness(battle: &mut Battle, target: Position, move_id: MoveId, data: &MoveData) -> i8 {
    let defender = mon(battle, target);
    let mut total = 0;
    // A Stellar move against a Terastallized target.
    let types = if typing::stellar_effective(data.ty, defender.terastallized) {
        total = 1;
        [None; 3]
    } else {
        manipulation::all_types(defender)
    };
    for defend in types.into_iter().flatten() {
        // `getEffectiveness`, then the move's own `onEffectiveness`.
        let own = typing::type_mod(move_id, data.ty, defend);
        // `runEvent('Effectiveness', this, type, move, typeMod)`.
        let mut ev = EventCtx::new(
            Event::Effectiveness,
            Target::Pokemon(target),
            None,
            Effect::Move(move_id),
            Relay::Num(own as i32),
        )
        .with_move(data);
        ev.ty = Some(defend);
        total += events::run_event(battle, &mut ev, None).num().unwrap_or(0) as i8;
    }
    // Tera Shell (Terapagos-Terastal) makes a full-HP hit resisted here,
    // after the types.
    super::abilities::switchin::tera_shell(battle, target, move_id, data, total)
}

/// The move's own `onBasePower` as a relay handler (`runEvent`'s
/// `onEffect`: it sorts with the other `BasePower` handlers). Solar Beam's
/// is in `multiturn`.
pub(crate) fn own_base_power(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    let (Some(move_id), Some(user), Some(target)) = (ev.effect.move_id(), ev.target_pos(), ev.source) else {
        return None;
    };
    let data = ev.move_data()?;
    let modifier = match on_base_power(battle, user, target, move_id, data) {
        4096 => super::multiturn::own_base_power(battle, user, move_id),
        m => m,
    };
    if modifier != 4096 {
        ev.chain(modifier);
    }
    None
}

/// Whether Facade ignores the burn's halving (Gen 6+).
pub(crate) fn ignores_burn(move_id: MoveId) -> bool {
    move_id == ids::FACADE
}

// ---------------------------------------------------------------------------
// Damage and the hit loop.

/// The move's own `onDamage` handler (the `Damage` event's `onEffect`):
/// False Swipe's and Hold Back's (priority -20).
pub(crate) fn own_damage_spec(move_id: MoveId) -> Option<super::events::Spec> {
    match move_id {
        ids::FALSE_SWIPE | ids::HOLD_BACK => Some(super::events::Spec::relay(false_swipe_damage).priority(-20)),
        _ => None,
    }
}

/// False Swipe's `onDamage`: leaves the target at 1 HP.
fn false_swipe_damage(
    battle: &mut Battle,
    ev: &mut super::events::EventCtx<'_>,
    _: &super::handlers::Handler,
) -> Option<super::events::Relay> {
    let hp = battle.state.at(ev.target_pos()?).map_or(0, |m| m.hp as i32);
    match ev.relay {
        super::events::Relay::Num(damage) if damage >= hp => Some(super::events::Relay::Num(hp - 1)),
        _ => None,
    }
}

/// Records damage for Showdown's `hurtThisTurn` (set by `spreadDamage`,
/// not by `directDamage`, which Struggle's recoil uses).
pub(crate) fn on_damage_dealt(mon: &mut PokemonState, dealt: u16, source: DamageSource) {
    if source != DamageSource::Recoil {
        record_hurt(mon, dealt);
    }
}

/// `spreadDamage`'s `target.hurtThisTurn = target.hp`, for damage that
/// doesn't go through `apply_damage` (hazards, volatiles' residual damage).
pub(crate) fn record_hurt(mon: &mut PokemonState, dealt: u16) {
    if dealt != 0 {
        mon.volatile_counters.hurt_this_turn = mon.hp != 0;
        mon.volatile_counters.hurt_hp = mon.hp;
    }
}

/// The last `spreadMoveHit` of a move: the targets it did not fail on, and
/// the damage each took if the hit dealt damage (a number in Showdown).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LastHit {
    pub hits: StackVec<(Position, Option<u16>), 4>,
    /// The targets whose substitute took the hit (`null` in the returned
    /// targets, damage 0).
    pub substitute: StackVec<Position, 4>,
}

impl LastHit {
    /// `targetsCopy = targets.slice(0)` with the last hit's `moveDamage`,
    /// what the hit loop is left with when a later hit's accuracy check
    /// misses: every target, with the damage the last `spreadMoveHit` gave
    /// it (0 behind a substitute; not a number where it failed).
    pub(crate) fn all_targets(&self, targets: &[Position]) -> LastHit {
        let mut all = LastHit::default();
        for &t in targets {
            let damage = match self.hits.iter().find(|&&(p, _)| p == t) {
                Some(&(_, d)) => d,
                None if self.substitute.contains(&t) => Some(0),
                None => None,
            };
            all.hits.push((t, damage));
        }
        all
    }
}

/// After the hit loop (`hitStepMoveHitLoop`): `gotAttacked` and
/// `timesAttacked` for each target of the last hit.
pub(crate) fn record_attacks(battle: &mut Battle, user: Position, last: &LastHit, hits: u8) {
    let Some(attacker_id) = battle.state.side(user.side).team.get(user.slot as usize).map(|m| m.id) else { return };
    for &(t, damage) in last.hits.iter() {
        if t == user {
            continue;
        }
        got_attacked(battle, t, MonRef { side: user.side, id: attacker_id }, Some(user), damage, hits);
    }
}

/// Showdown's `gotAttacked` on `target` by `attacker` (from `attacker_pos`,
/// `None` for a Future Sight user that left the field), plus
/// `timesAttacked` when the hit dealt damage (a number).
///
/// `hitStepMoveHitLoop` calls it after `faintMessages`, whose
/// `clearVolatile(false)` has already reset a target the move knocked out:
/// what this writes stays on that Pokémon (Revival Blessing brings it back
/// with it; `switchIn` doesn't clear the incoming Pokémon).
pub(crate) fn got_attacked(
    battle: &mut Battle,
    target: Position,
    attacker: MonRef,
    attacker_pos: Option<Position>,
    damage: Option<u16>,
    hits: u8,
) {
    let bit = attacker_bit(attacker.side, attacker.id);
    let m = &mut battle.state.side_mut(target.side).team[target.slot as usize];
    // `attackedBy` (`knownType` reads it).
    m.volatile_counters.attacked_by |= bit;
    if let Some(d) = damage {
        // The entry `getLastDamagedBy(true)` reads (Metal Burst): the
        // pipeline records it with the damage, which a knocked-out target's
        // `clearVolatile` then erased.
        if m.is_fainted() && attacker.side != target.side {
            super::multiturn::remember_damage(m, attacker, attacker_pos, d);
        }
        m.record.times_attacked = m.record.times_attacked.saturating_add(hits);
        m.volatile_counters.times_attacked = m.volatile_counters.times_attacked.saturating_add(hits);
        if d > 0 {
            m.volatile_counters.damaged_by |= bit;
        }
    }
}

/// `mindBlownRecoil` (Steel Beam: `[from] steelbeam`) and
/// `chloroblastRecoil` (`[from] Recoil`): half the user's max HP, rounded.
pub(crate) fn recoil(battle: &Battle, user: Position, move_id: MoveId) -> Option<(u32, DamageSource)> {
    let source = match move_id {
        ids::STEEL_BEAM => DamageSource::MoveCondition(move_id),
        ids::CHLOROBLAST => DamageSource::MoveRecoil,
        _ => return None,
    };
    let max_hp = battle.state.at(user)?.max_hp() as u32;
    Some((max_hp.div_ceil(2), source))
}

/// Diamond Storm's `self.chance`: the chance, out of 100, that the move's
/// `self` boosts apply after `selfDrops`' roll.
pub(crate) fn self_chance(battle: &Battle) -> u32 {
    // Serene Grace doubles `move.self.chance` (`ModifyMove`).
    let chance = if battle.active_move.is_some_and(|(_, m)| m == ids::DIAMOND_STORM) { 50 } else { 100 };
    chance << battle.move_state.chance_doublings
}

/// The end of `useMoveInner` after the move's hit: `selfBoost` on success,
/// `MoveFail` (crash damage) on failure, then `AfterMoveSecondarySelf`.
/// `target` is the last of the move's targets. Returns `hit`.
pub(crate) fn after_use_move(
    battle: &mut Battle,
    user: Position,
    move_id: MoveId,
    data: &MoveData,
    target: Option<Position>,
    hit: bool,
) -> bool {
    let hp_before = battle.state.at(user).map_or(0, |m| m.hp);
    if hit {
        // `selfBoost`: `moveHit(pokemon, pokemon, move, move.selfBoost, false, true)`.
        let self_boost = match move_id {
            ids::CLANGING_SCALES => Some(StatBoosts::new([0, -1, 0, 0, 0, 0, 0])),
            ids::SCALE_SHOT => Some(StatBoosts::new([0, -1, 0, 0, 1, 0, 0])),
            _ => None,
        };
        if let Some(b) = self_boost {
            boosts::boost(battle, user, b, Some(user), false, true);
        }
    } else {
        move_fail(battle, user, move_id, data);
        if target != Some(user) && data.category != MoveCategory::Status {
            super::moves::emergency_exit_at_any_hp(battle, user, Some(user), hp_before);
        }
        return false;
    }
    // `AfterMoveSecondarySelf`, unless Sheer Force dropped the secondaries
    // or the move hits later (Future Sight): the move's own, then
    // `runEvent` (Life Orb, Shell Bell).
    if super::moves::sheer_force(battle, user) || data.flags.contains(MoveFlags::FUTURE_MOVE) {
        return true;
    }
    let hp_before = battle.state.at(user).map_or(0, |m| m.hp);
    match move_id {
        ids::FELL_STINGER => {
            if target.is_none_or(|t| battle.state.side(t.side).team[t.slot as usize].is_fainted()) {
                boosts::boost(battle, user, StatBoosts::new([3, 0, 0, 0, 0, 0, 0]), Some(user), false, false);
            }
        }
        // Order Up boosts while Commanded.
        ids::ORDER_UP => super::abilities::switchin::order_up(battle, user),
        // Relic Song's `onAfterMoveSecondarySelf`: Meloetta (not
        // transformed) switches between its Aria and Pirouette formes
        // (`formeChange(..., this.effect, false, '0', '[msg]')`).
        ids::RELIC_SONG => {
            let m = &battle.state.side(user.side).team[user.slot as usize];
            let dex = battle.format.dex;
            if dex.species(m.base_species).base_species == species::MELOETTA && !m.volatiles.has(Volatiles::TRANSFORMED)
            {
                let forme = if m.species == species::MELOETTA_PIROUETTE {
                    species::MELOETTA
                } else {
                    species::MELOETTA_PIROUETTE
                };
                super::forme::forme_change(battle, user, forme, Effect::Move(ids::RELIC_SONG), false, true);
            }
        }
        _ => {}
    }
    // Also for a user at 0 HP (recoil): it is still active until its faint
    // is reported (Magician steals as it faints).
    if battle.state.at(user).is_some() {
        events::move_event(battle, Event::AfterMoveSecondarySelf, user, target, move_id, data, Relay::True);
    }
    if target != Some(user) && data.category != MoveCategory::Status {
        super::moves::emergency_exit_at_any_hp(battle, user, Some(user), hp_before);
    }
    true
}

/// The move's own `onMoveFail`: crash damage (High Jump Kick, Axe Kick,
/// Supercell Slam: half the base max HP, rounded down; Steel Beam: half the
/// max HP, rounded, unless multi-hit).
fn move_fail(battle: &mut Battle, user: Position, move_id: MoveId, data: &MoveData) {
    let Some(max_hp) = battle.state.at(user).map(|m| m.max_hp() as u32) else { return };
    let amount = match move_id {
        ids::HIGH_JUMP_KICK | ids::AXE_KICK | ids::SUPERCELL_SLAM => super::hp_fraction(max_hp, 1, 2),
        // `move.multihit`, also Parental Bond's (set before the hit).
        ids::STEEL_BEAM if data.multihit.is_none() && !battle.move_state.parental_bond => max_hp.div_ceil(2),
        _ => return,
    };
    apply_damage(battle, user, amount, DamageSource::MoveCondition(move_id));
}

// ---------------------------------------------------------------------------
// Move results, turn bookkeeping, faints, boosts.

/// Sets Showdown's `moveThisTurnResult` for the Pokémon at `pos`.
pub(crate) fn set_move_result(battle: &mut Battle, pos: Position, result: MoveResult) {
    if let Some(m) = battle.state.at_mut(pos) {
        m.volatile_counters.move_this_turn = result;
    }
}

/// `useMove`: `moveThisTurnResult` starts `undefined`.
pub(crate) fn begin_use_move(battle: &mut Battle, user: Position) {
    set_move_result(battle, user, MoveResult::Undefined);
}

/// After `useMove` in `runMove`: `moveThisTurnResult` takes the move's
/// result unless the move set it, and `lastSuccessfulMoveThisTurn` is
/// updated.
pub(crate) fn end_use_move(battle: &mut Battle, user: Position, move_id: MoveId, did_something: bool) {
    // The user may have fainted (its counters were then cleared) or left.
    if let Some(m) = battle.state.at_mut(user) {
        let c = &mut m.volatile_counters;
        if c.move_this_turn == MoveResult::Undefined {
            c.move_this_turn = if did_something { MoveResult::Succeeded } else { MoveResult::Failed };
        }
    }
    battle.state.field.last_successful_move = did_something.then_some(move_id);
}

/// Showdown's `endTurn` bookkeeping for this module, run after the turn
/// counter moves on.
pub(crate) fn end_turn(battle: &mut Battle) {
    let first_turn = battle.state.turn == 1;
    battle.state.field.last_successful_move = None;
    let active = battle.format.active_per_side();
    for side in &mut battle.state.sides {
        for m in side.team.iter_mut().take(active) {
            let c = &mut m.volatile_counters;
            c.settled = true;
            c.move_last_turn = c.move_this_turn;
            c.move_this_turn = MoveResult::Undefined;
            if !first_turn {
                c.stats_lowered_this_turn = false;
                c.hurt_this_turn = false;
                c.hurt_hp = 0;
                m.used_item_this_turn = false;
            }
            c.damaged_by = 0;
        }
        side.fainted_last_turn = side.fainted_this_turn;
        side.fainted_this_turn = false;
    }
}

/// `faintMessages` for a Pokémon of `side`: `totalFainted` and
/// `faintedThisTurn`.
pub(crate) fn on_faint(battle: &mut Battle, side: SideId) {
    let s = battle.state.side_mut(side);
    if s.total_fainted < 100 {
        s.total_fainted += 1;
    }
    s.fainted_this_turn = true;
}

/// `boost`: `statsLoweredThisTurn` once a stage fell.
pub(crate) fn on_boost(battle: &mut Battle, target: Position, lowered: bool) {
    if lowered && let Some(m) = battle.state.at_mut(target) {
        m.volatile_counters.stats_lowered_this_turn = true;
    }
}

// ---------------------------------------------------------------------------
// Echoed Voice and Fury Cutter conditions.

/// `field.addPseudoWeather('echoedvoice')` (`Field.addPseudoWeather`, from
/// the move's `onTryMove`, whose event's target is the user): starts with
/// multiplier 1 for 2 turns, then runs `PseudoWeatherChange`; restarting
/// it in a later turn raises the multiplier (up to 5).
fn add_echoed_voice(battle: &mut Battle, user: Position) {
    let field = &mut battle.state.field;
    if field.echoed_voice_turns == 0 {
        // `onFieldStart`.
        field.echoed_voice_turns = 2;
        field.echoed_voice_multiplier = 1;
        super::field::pseudo_weather_started(battle, PseudoWeather::EchoedVoice);
        super::field::pseudo_weather_change(battle, PseudoWeather::EchoedVoice, user);
    } else if field.echoed_voice_turns != 2 {
        // `onFieldRestart`.
        field.echoed_voice_turns = 2;
        if field.echoed_voice_multiplier < 5 {
            field.echoed_voice_multiplier += 1;
        }
    }
}

/// End-of-turn handlers: Fury Cutter's duration (no `onResidual`, so
/// unordered). Echoed Voice's is collected with the pseudo-weathers
/// (`field::residual_handlers`).
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let pos = Position::new(side, slot as u8);
            let Some(m) = battle.state.at(pos) else { continue };
            if m.volatiles.has(Volatiles::FURY_CUTTER) {
                handlers.push(Handler {
                    speed: m.speed,
                    sub_order: sub_order::CONDITION,
                    holder: Holder::Pokemon(pos),
                    volatile_bit: vol(Volatiles::FURY_CUTTER),
                    callback: Callback::Plain(tick_fury_cutter),
                    ..Handler::default()
                });
            }
        }
    }
}

/// Echoed Voice's duration (collected by `field::residual_handlers` among
/// the pseudo-weathers).
pub(crate) fn tick_echoed_voice(battle: &mut Battle, _handler: &Handler) -> bool {
    let field = &mut battle.state.field;
    field.echoed_voice_turns = field.echoed_voice_turns.saturating_sub(1);
    if field.echoed_voice_turns == 0 {
        super::field::pseudo_weather_ended(battle, PseudoWeather::EchoedVoice);
        // An expired duration skips `faintMessages` (`run_residual`).
        return false;
    }
    true
}

fn tick_fury_cutter(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Pokemon(pos) = handler.holder else { return true };
    let Some(m) = battle.state.at_mut(pos) else { return true };
    if !m.volatiles.has(Volatiles::FURY_CUTTER) {
        return true;
    }
    let c = &mut m.volatile_counters;
    c.fury_cutter_duration = c.fury_cutter_duration.saturating_sub(1);
    if c.fury_cutter_duration == 0 {
        m.volatiles.clear(Volatiles::FURY_CUTTER);
        // An expired duration skips `faintMessages` (`run_residual`).
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use pikapro_data::TypeSet;

    use super::*;
    use crate::testing::sample_team;
    use crate::{BattleOptions, Format, Seed};

    fn battle() -> Battle {
        let format = Format::gen9_singles();
        let teams = [sample_team(&format), sample_team(&format)];
        Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap()
    }

    #[test]
    fn echoed_voice_grows_once_per_turn() {
        let mut b = battle();
        add_echoed_voice(&mut b, Position::new(SideId::P1, 0));
        add_echoed_voice(&mut b, Position::new(SideId::P1, 0));
        assert_eq!((b.state.field.echoed_voice_turns, b.state.field.echoed_voice_multiplier), (2, 1));
        for expected in 2..=5 {
            tick_echoed_voice(&mut b, &Handler::default());
            add_echoed_voice(&mut b, Position::new(SideId::P1, 0));
            assert_eq!(b.state.field.echoed_voice_multiplier, expected);
        }
        tick_echoed_voice(&mut b, &Handler::default());
        add_echoed_voice(&mut b, Position::new(SideId::P1, 0));
        assert_eq!(b.state.field.echoed_voice_multiplier, 5, "capped at 5");
        tick_echoed_voice(&mut b, &Handler::default());
        tick_echoed_voice(&mut b, &Handler::default());
        assert_eq!(b.state.field.echoed_voice_turns, 0);
    }

    #[test]
    fn variable_base_powers() {
        let mut b = battle();
        let (user, target) = (Position::new(SideId::P1, 0), Position::new(SideId::P2, 0));
        let data = |id| *Format::gen9_singles().dex.move_(id);
        let foe = b.state.at_mut(target).unwrap();
        foe.hp = foe.max_hp() / 2;
        // Crush Grip at half HP: 120 * 2048/4096, rounded as Showdown does.
        assert_eq!(base_power_callback(&mut b, user, target, ids::CRUSH_GRIP, &data(ids::CRUSH_GRIP), 1), Some(60));
        let me = b.state.at_mut(user).unwrap();
        me.hp = 1;
        assert_eq!(base_power_callback(&mut b, user, target, ids::FLAIL, &data(ids::FLAIL), 1), Some(200));
        assert_eq!(base_power_callback(&mut b, user, target, ids::ERUPTION, &data(ids::ERUPTION), 1), Some(1));
        // Fury Cutter doubles on consecutive uses, up to 160.
        let fc = data(ids::FURY_CUTTER);
        let powers: Vec<_> =
            (0..4).map(|_| base_power_callback(&mut b, user, target, ids::FURY_CUTTER, &fc, 1)).collect();
        assert_eq!(powers, [Some(40), Some(80), Some(160), Some(160)]);
    }

    #[test]
    fn freeze_dry_and_flying_press_effectiveness() {
        let mut b = battle();
        let target = Position::new(SideId::P2, 0);
        b.state.at_mut(target).unwrap().types = TypeSet::dual(Type::Water, Type::Grass);
        let dex = Format::gen9_singles().dex;
        let freeze_dry = *dex.move_(ids::FREEZE_DRY);
        assert_eq!(run_effectiveness(&mut b, target, ids::FREEZE_DRY, &freeze_dry), 2);
        let flying_press = *dex.move_(ids::FLYING_PRESS);
        // Fighting: neutral on both; Flying: neutral on Water, 2x on Grass.
        assert_eq!(run_effectiveness(&mut b, target, ids::FLYING_PRESS, &flying_press), 1);
    }
}
