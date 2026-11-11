//! Weather, terrain, pseudo-weather, side and slot conditions.
//!
//! Showdown references: `sim/field.ts` (`setWeather`, `setTerrain`,
//! `clearTerrain`, `addPseudoWeather`, `removePseudoWeather`), `sim/side.ts`
//! (`addSideCondition`, `removeSideCondition`, `addSlotCondition`), the
//! conditions in `data/conditions.ts` and `data/moves.ts` (`sunnyday`,
//! `raindance`, `sandstorm`, `snowscape`, the terrains, `trickroom`,
//! `gravity`, `magicroom`, `wonderroom`, `tailwind`, `reflect`,
//! `lightscreen`, `auroraveil`, `safeguard`, `mist`, `wideguard`,
//! `quickguard`, ...).
//!
//! Related modules: `hazards.rs` (entry hazards and the
//! moves that clear them), `slots.rs` (Wish, Healing Wish, Lunar Dance,
//! Revival Blessing), `pledges.rs` (the pledge combinations).
//!
//! Contract:
//! - [`handles`], [`try_move`], [`prepare_hit`], [`run_field_effects`]:
//!   the moves this module implements; field and side moves go
//!   through Showdown's `tryMoveHit`.
//! - [`move_try_hit`], [`on_hit`], [`after_hit`]: the moves' own `onTryHit`,
//!   `onHit` and `onAfterHit` callbacks, called by the move pipeline.
//! - setters called by `runMoveEffects` for a move's `weather`, `terrain`,
//!   `pseudo_weather`, `side_condition`, `slot_condition`.
//! - [`residual_handlers`], [`before_move_handlers`]: handlers for the
//!   `Residual` and `BeforeMove` events (with Showdown's orders).
//! - [`try_hit`]: `TryHit` handlers of priority 4 (Wide Guard, Quick Guard,
//!   Psychic Terrain).
//! - damage/stat modifiers used by the damage formula and speed:
//!   [`terrain_base_power`] (terrains), [`weather_modify_damage`]
//!   (`WeatherModifyDamage`), [`screen_modify_damage`] (`ModifyDamage`:
//!   screens), [`sand_modify_spd`] / [`snow_modify_def`] (sand SpD, snow
//!   Def), [`side_modify_spe`] (Tailwind, swamp), [`defensive_stat`] (Wonder
//!   Room), [`gravity_modify_accuracy`] (Gravity). Modifiers return
//!   4096-based chain modifiers.
//! - Trick Room's speed inversion lives in `mon_action_speed`.
//! - [`safeguard_status`], [`terrain_status`], [`mist_try_boost`]: the
//!   `SetStatus`, `TryAddVolatile` and `TryBoost` handlers (Safeguard, Mist,
//!   Misty and Electric Terrain), collected by the event registry.
//! - [`modify_move`], [`delta_stream_effectiveness`], [`break_guards`]:
//!   Gravity's and Wonder Room's `ModifyMove`, Strong Winds' `Effectiveness`,
//!   Feint breaking the guards.
//! - [`end_turn`]: Gravity's `DisableMove`.

use pikapro_data::gen9::{abilities, moves as ids};
use pikapro_data::{MoveCategory, MoveData, MoveFlags, MoveId, MoveTarget, Stat, StatusId, Type};

use super::events::{Effect, EventCtx, Relay};
use super::handlers::{Callback, Handler, Handlers, Holder, sub_order};
use super::manipulation::has_type;
use super::moves::Outcome;
use super::{
    DamageSource, HealSource, actor, apply_heal, conditions, each_event, each_update, hazards, pledges, slots,
};
use crate::battle::Battle;
use crate::event::{Actor, BattleEvent, Cause, FieldEffect};
use crate::state::{Position, PseudoWeather, SideCondition, SideId, Terrain, Volatiles, Weather};
use crate::stats::modify;
use crate::util::StackVec;

/// Moves whose effect this module implements (their conditions and
/// callbacks).
pub(crate) fn handles(move_id: MoveId) -> bool {
    [
        ids::SUNNY_DAY,
        ids::RAIN_DANCE,
        ids::SANDSTORM,
        ids::SNOWSCAPE,
        ids::ELECTRIC_TERRAIN,
        ids::GRASSY_TERRAIN,
        ids::MISTY_TERRAIN,
        ids::PSYCHIC_TERRAIN,
        ids::TRICK_ROOM,
        ids::TAILWIND,
        ids::REFLECT,
        ids::LIGHT_SCREEN,
        ids::AURORA_VEIL,
        ids::SAFEGUARD,
        ids::MIST,
        ids::GRAVITY,
        ids::MAGIC_ROOM,
        ids::WONDER_ROOM,
        ids::HAZE,
        ids::WIDE_GUARD,
        ids::QUICK_GUARD,
        ids::MAGNETIC_FLUX,
        ids::HAPPY_HOUR,
        ids::COURT_CHANGE,
    ]
    .contains(&move_id)
        || hazards::handles(move_id)
        || slots::handles(move_id)
}

/// The move's `onTry` (Showdown's `singleEvent('Try')`): false fails it.
#[inline]
pub(crate) fn try_move(battle: &Battle, move_id: MoveId) -> bool {
    match move_id {
        // Aurora Veil only works in snow (or hail, which Gen 9 lacks).
        // `field.isWeather(['hail', 'snowscape'])` (Cloud Nine, Air Lock).
        ids::AURORA_VEIL => super::abilities::effective_weather(battle) == Some(Weather::Snow),
        // `!!this.queue.willAct()`.
        ids::WIDE_GUARD | ids::QUICK_GUARD => battle.queue.will_act(),
        // `!this.field.isTerrain('')`.
        ids::STEEL_ROLLER => battle.state.field.terrain.is_some(),
        _ => true,
    }
}

/// The move's `onPrepareHit` when it can stop the move without failing
/// (`null`): a pledge waiting for its ally's pledge. False ends the move
/// silently.
#[inline(always)]
pub(crate) fn prepare_hit(battle: &mut Battle, user: Position, move_id: MoveId) -> bool {
    pledges::prepare_hit(battle, user, move_id)
}

/// The field part of Showdown's `runMoveEffects` for one target, in its
/// order (side condition, slot condition, weather, terrain,
/// pseudo-weather, then `onHitField` / `onHitSide`). Returns what the
/// move did (`combineResults`, whose priorities [`Outcome`]'s order
/// follows: `Undefined` if it has none of these effects; `Null` if its
/// only effect returned `null`, a weather a primal weather stopped).
pub(crate) fn run_field_effects(battle: &mut Battle, target: Position, user: Position, data: &MoveData) -> Outcome {
    let mut did = Outcome::Undefined;
    let mut combine = |result: Outcome| did = did.max(result);
    if let Some(c) = data.side_condition {
        combine(Outcome::from_bool(add_side_condition(battle, target.side, c, user)));
    }
    if let Some(c) = data.slot_condition {
        combine(Outcome::from_bool(add_slot_condition(battle, target, c, user)));
    }
    if let Some(w) = data.weather {
        combine(set_weather(battle, w, user));
    }
    if let Some(t) = data.terrain {
        combine(Outcome::from_bool(set_terrain(battle, t, user)));
    }
    if let Some(p) = data.pseudo_weather {
        combine(Outcome::from_bool(add_pseudo_weather(battle, p, user)));
    }
    // `onHitField` / `onHitSide` (`singleEvent` turns `undefined` into true).
    let move_id = battle.active_move.map(|(_, m)| m);
    if let Some(result) = move_id.and_then(|m| on_hit_field(battle, target, user, m)) {
        combine(Outcome::from_bool(result));
    }
    did
}

/// A field or side move's `onHitField` / `onHitSide`, if it has one.
/// Returning nothing counts as success (`singleEvent`'s default).
fn on_hit_field(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Option<bool> {
    match move_id {
        ids::HAZE => {
            haze(battle);
            Some(true)
        }
        ids::COURT_CHANGE => Some(court_change(battle, user)),
        // `source.addVolatile('stall')`, even if the guard was already up.
        ids::WIDE_GUARD | ids::QUICK_GUARD => {
            super::protect::add_stall(battle, user);
            Some(true)
        }
        // Only allies (`side.allies()`, the user too) with Plus or Minus
        // are affected. (Showdown also skips allies behind Max Guard:
        // Dynamax is not in Gen 9 or Champions.)
        ids::MAGNETIC_FLUX => {
            let side = target.side;
            let mut targets = crate::util::StackVec::<Position, 2>::new();
            for slot in 0..battle.format.active_per_side() {
                let p = Position::new(side, slot as u8);
                let alive = battle.state.at(p).is_some_and(|m| m.hp > 0);
                let plus_minus = super::abilities::has_ability(battle, p, abilities::PLUS)
                    || super::abilities::has_ability(battle, p, abilities::MINUS);
                if alive && plus_minus {
                    targets.push(p);
                }
            }
            if targets.is_empty() {
                return Some(false);
            }
            let mut did = false;
            for &p in targets.iter() {
                let b = pikapro_data::StatBoosts::new([0, 1, 0, 1, 0, 0, 0]);
                did = super::boosts::boost(battle, p, b, Some(user), false, true) || did;
            }
            Some(did)
        }
        _ => None,
    }
}

/// The move's own `onTryHit` (`singleEvent('TryHit')` on the first target
/// in `spreadMoveHit`). False stops the hit.
pub(crate) fn move_try_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> bool {
    match move_id {
        // Brick Break and co. shatter the target side's screens first.
        ids::BRICK_BREAK | ids::PSYCHIC_FANGS | ids::RAGING_BULL => {
            for condition in [SideCondition::Reflect, SideCondition::LightScreen, SideCondition::AuroraVeil] {
                remove_side_condition(battle, target.side, condition);
            }
            true
        }
        _ => slots::try_hit(battle, target, user, move_id),
    }
}

/// The move's `onHit` for one target (`runMoveEffects`), if it has one.
/// Returning nothing counts as success (`singleEvent`'s default).
pub(crate) fn on_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) -> Option<bool> {
    match move_id {
        ids::STEEL_ROLLER => {
            clear_terrain(battle);
            Some(true)
        }
        // A combination's side condition (`move.sideCondition`, or
        // `move.self.sideCondition` for the rainbow). Damage already
        // counts as success.
        ids::FIRE_PLEDGE | ids::GRASS_PLEDGE | ids::WATER_PLEDGE => {
            pledges::on_hit(battle, target, user);
            None
        }
        _ => hazards::on_hit(battle, target, user, move_id),
    }
}

/// The move's `onAfterHit` on one damaged target (`spreadMoveHit`, after
/// `DamagingHit`). The caller checks the user's HP (Showdown only calls it
/// while the user has HP, except in the Champions mod).
pub(crate) fn after_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) {
    match move_id {
        ids::ICE_SPINNER => {
            clear_terrain(battle);
        }
        _ => hazards::after_hit(battle, target, user, move_id),
    }
}

/// The move's `onAfterSubDamage` after it hit a substitute:
/// the same effects as `onAfterHit`, except that Steel Roller clears the
/// terrain even if its user has fainted.
/// Sheer Force stops the hazard moves' effects (`hazards::after_hit`).
/// (Shell Side Arm's hint is in `conditions.rs`.)
pub(crate) fn after_sub_damage(battle: &mut Battle, target: Position, user: Position, move_id: MoveId) {
    if move_id == ids::STEEL_ROLLER {
        clear_terrain(battle);
        return;
    }
    // Ceaseless Edge's and Stone Axe's `onAfterSubDamage` keep
    // `!move.hasSheerForce && source.hp` (`onAfterHit` has no such check).
    let fainted = battle.state.at(user).is_none_or(|m| m.hp == 0);
    if matches!(move_id, ids::CEASELESS_EDGE | ids::STONE_AXE) && fainted {
        return;
    }
    after_hit(battle, target, user, move_id);
}

/// `onModifyType`: Raging Bull takes the type of the user's Paldean Tauros
/// breed; a pledge combination takes the combination's
/// ([`pledges::modify_move`]). The type it gives the move for this use, if
/// any.
pub(crate) fn modify_type(battle: &Battle, user: Position, move_id: MoveId) -> Option<Type> {
    let mon = battle.state.at(user)?;
    if move_id != ids::RAGING_BULL {
        return pledges::modify_move(battle, user);
    }
    // `switch (pokemon.species.name)`: the species it transformed into
    // (the rule reads nothing else).
    let facts = crate::typing::MoveUser::new(battle.format.dex, super::tera::species_of(mon));
    crate::typing::own_move_type(battle.format.dex, move_id, &facts).ok().flatten()
}

fn weather_of(id: &str) -> Option<Weather> {
    // Showdown IDs are case-insensitive (`toID`); the data keeps the move's
    // spelling ("RainDance", "sunnyday").
    [
        ("sunnyday", Weather::Sun),
        ("raindance", Weather::Rain),
        ("sandstorm", Weather::Sand),
        ("snowscape", Weather::Snow),
    ]
    .into_iter()
    .find(|(name, _)| id.eq_ignore_ascii_case(name))
    .map(|(_, w)| w)
}

fn terrain_of(id: &str) -> Option<Terrain> {
    [
        ("electricterrain", Terrain::Electric),
        ("grassyterrain", Terrain::Grassy),
        ("mistyterrain", Terrain::Misty),
        ("psychicterrain", Terrain::Psychic),
    ]
    .into_iter()
    .find(|(name, _)| id.eq_ignore_ascii_case(name))
    .map(|(_, t)| t)
}

pub(crate) fn side_condition_of(id: &str) -> Option<SideCondition> {
    [
        ("reflect", SideCondition::Reflect),
        ("lightscreen", SideCondition::LightScreen),
        ("auroraveil", SideCondition::AuroraVeil),
        ("tailwind", SideCondition::Tailwind),
        ("safeguard", SideCondition::Safeguard),
        ("mist", SideCondition::Mist),
        ("wideguard", SideCondition::WideGuard),
        ("quickguard", SideCondition::QuickGuard),
        ("stealthrock", SideCondition::StealthRock),
        ("spikes", SideCondition::Spikes),
        ("toxicspikes", SideCondition::ToxicSpikes),
        ("stickyweb", SideCondition::StickyWeb),
        ("firepledge", SideCondition::FirePledge),
        ("grasspledge", SideCondition::GrassPledge),
        ("waterpledge", SideCondition::WaterPledge),
    ]
    .into_iter()
    .find(|(name, _)| id.eq_ignore_ascii_case(name))
    .map(|(_, c)| c)
}

fn pseudo_weather_of(id: &str) -> Option<PseudoWeather> {
    [
        ("trickroom", PseudoWeather::TrickRoom),
        ("gravity", PseudoWeather::Gravity),
        ("magicroom", PseudoWeather::MagicRoom),
        ("wonderroom", PseudoWeather::WonderRoom),
    ]
    .into_iter()
    .find(|(name, _)| id.eq_ignore_ascii_case(name))
    .map(|(_, p)| p)
}

/// The effect running now (Showdown's `battle.effect`, the default source
/// effect of `setWeather`, `setTerrain`, `addVolatile`, ...): the move being
/// used.
pub(crate) fn current_effect(battle: &Battle) -> Effect {
    match super::active_dex_move(battle) {
        Some((_, m)) => Effect::Move(m),
        None => Effect::None,
    }
}

/// Sunny Day's and Desolate Land's `onImmunity`: no freezing in the sun
/// (unless the Pokémon holds a Utility Umbrella): the Pokémon's
/// `effectiveWeather()` must be the handler's weather. While a Mega Sol
/// Pokémon uses a move, that is sun (so Desolate Land's no longer is).
pub(crate) fn sun_immunity(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let pos = ev.target_pos()?;
    let Effect::Weather(weather) = h.effect else { return None };
    if super::mega::pokemon_weather(battle, pos, super::mega::WeatherAsker::Weather) != Some(weather) {
        return None;
    }
    (ev.relay == Relay::Id("frz")).then_some(Relay::False)
}

/// `primordialsea`'s and `desolateland`'s `onTryMove` (priority 1,
/// `data/conditions.js`): a damaging Fire move in Heavy Rain, or Water move
/// in Extreme Sun, fails (`-fail|<user>|<move>|[from] Primordial Sea`,
/// `[still]`) and the event returns `null`. It reads the field's weather,
/// not the user's `effectiveWeather` (Utility Umbrella doesn't help), and
/// `runEvent` skips it while Cloud Nine or Air Lock suppresses weather.
pub(crate) fn primal_weather_try_move(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Effect::Weather(weather) = h.effect else { return None };
    let blocked = match weather {
        Weather::HeavyRain => Type::Fire,
        Weather::HarshSun => Type::Water,
        _ => return None,
    };
    let data = ev.move_data()?;
    if data.ty != blocked || data.category == MoveCategory::Status {
        return None;
    }
    let move_id = ev.effect.move_id()?;
    let user = actor(battle, ev.target_pos()?);
    battle.emit(BattleEvent::WeatherMoveFail { user, move_id, weather });
    battle.attr_last_move(crate::event::MoveAttr::Still);
    Some(Relay::Null)
}

/// Showdown's `field.setWeather` from a move (`runMoveEffects`): `null`
/// ([`Outcome::Null`]) when the `SetWeather` event stopped it.
pub(crate) fn set_weather(battle: &mut Battle, id: &'static str, source: Position) -> Outcome {
    let Some(weather) = weather_of(id) else { return Outcome::Failed };
    let effect = current_effect(battle);
    match set_weather_from(battle, weather, Some(source), effect) {
        Some(ok) => Outcome::from_bool(ok),
        None => Outcome::Null,
    }
}

/// Showdown's `field.setWeather(weather, source, sourceEffect)`: fails
/// (`Some(false)`) if that weather is already up; the `SetWeather` event
/// can stop it (`None`, Showdown's `null`: the primal weathers). It lasts 5
/// turns (8 with the matching rock: `items::held::weather_duration`; the
/// primal weathers until they end),
/// starts with `-weather` (`[from] ability: X|[of] <source>` when an
/// ability set it), then `eachEvent('WeatherChange', sourceEffect)`.
pub(crate) fn set_weather_from(
    battle: &mut Battle,
    weather: Weather,
    source: Option<Position>,
    effect: Effect,
) -> Option<bool> {
    // Gen 6+: an ability can't restart its weather either.
    if battle.state.field.weather == Some(weather) {
        return Some(false);
    }
    if let Some(source) = source {
        let mut ev = super::events::EventCtx::new(
            pikapro_data::Event::SetWeather,
            super::events::Target::Pokemon(source),
            Some(source),
            Effect::Weather(weather),
            Relay::True,
        );
        let result = super::events::run_event(battle, &mut ev, None);
        if !result.is_truthy() {
            // A primal weather stops it with `false`.
            if result == Relay::False {
                super::abilities::switchin::set_weather_failed(battle, source, effect);
            }
            return None;
        }
    }
    let turns = match weather {
        Weather::HarshSun | Weather::HeavyRain | Weather::StrongWinds => 0,
        _ => super::items::held::weather_duration(battle, source, weather),
    };
    // `weatherState.source` (the primal weathers).
    let weather_source = source.and_then(|s| battle.state.at(s).map(|m| (s.side, m.id)));
    let field = &mut battle.state.field;
    field.weather = Some(weather);
    field.weather_turns = turns;
    field.weather_source = weather_source;
    battle.conditions_changed();
    // The weather's `onFieldStart`.
    match (effect, source) {
        (Effect::Ability(ability), Some(source)) => {
            let cause = Cause::ability(ability).of(actor(battle, source));
            battle.emit(BattleEvent::WeatherFrom { weather, cause });
        }
        _ => battle.emit(BattleEvent::Weather { weather: Some(weather), upkeep: false }),
    }
    // `eachEvent('WeatherChange')`: the speed sort draws on ties.
    each_event(battle, Some(pikapro_data::Event::WeatherChange), effect);
    Some(true)
}

/// Showdown's `field.setTerrain` from a move (`runMoveEffects`).
pub(crate) fn set_terrain(battle: &mut Battle, id: &'static str, source: Position) -> bool {
    let Some(terrain) = terrain_of(id) else { return false };
    let effect = current_effect(battle);
    set_terrain_from(battle, terrain, source, effect)
}

/// Showdown's `field.setTerrain(terrain, source, sourceEffect)`: fails if
/// that terrain is already up; otherwise lasts 5 turns (8 with Terrain
/// Extender: `items::held::terrain_duration`), starts with
/// `-fieldstart` (`[from] ability: X|[of] <source>` when an ability set it),
/// then `eachEvent('TerrainChange', sourceEffect)`.
pub(crate) fn set_terrain_from(battle: &mut Battle, terrain: Terrain, source: Position, effect: Effect) -> bool {
    if battle.state.field.terrain == Some(terrain) {
        return false;
    }
    let turns = super::items::held::terrain_duration(battle, source);
    let field = &mut battle.state.field;
    field.terrain = Some(terrain);
    field.terrain_turns = turns;
    battle.conditions_changed();
    match effect {
        Effect::Ability(ability) => {
            let cause = Cause::ability(ability).of(actor(battle, source));
            battle.emit(BattleEvent::FieldStartFrom { effect: FieldEffect::Terrain(terrain), cause });
        }
        _ => battle.emit(BattleEvent::FieldStart { effect: FieldEffect::Terrain(terrain), of: None }),
    }
    // `eachEvent('TerrainChange')`.
    each_event(battle, Some(pikapro_data::Event::TerrainChange), effect);
    true
}

/// Showdown's `field.clearTerrain`: the terrain's `FieldEnd` message, then
/// `eachEvent('TerrainChange')`. False if there was no terrain.
pub(crate) fn clear_terrain(battle: &mut Battle) -> bool {
    let field = &mut battle.state.field;
    let Some(terrain) = field.terrain else { return false };
    field.terrain = None;
    field.terrain_turns = 0;
    battle.conditions_changed();
    battle.emit(BattleEvent::FieldEnd { effect: FieldEffect::Terrain(terrain) });
    each_event(battle, Some(pikapro_data::Event::TerrainChange), Effect::None);
    true
}

fn pseudo_weather_turns(battle: &mut Battle, pw: PseudoWeather) -> &mut u8 {
    let field = &mut battle.state.field;
    match pw {
        PseudoWeather::TrickRoom => &mut field.trick_room_turns,
        PseudoWeather::Gravity => &mut field.gravity_turns,
        PseudoWeather::MagicRoom => &mut field.magic_room_turns,
        PseudoWeather::WonderRoom => &mut field.wonder_room_turns,
        PseudoWeather::FairyLock => &mut field.fairy_lock_turns,
        PseudoWeather::EchoedVoice => &mut field.echoed_voice_turns,
    }
}

/// A pseudo-weather started by another module (Fairy Lock, Echoed Voice)
/// joins `field.pseudoWeather`, whose key order is how handlers are
/// collected.
pub(crate) fn pseudo_weather_started(battle: &mut Battle, pw: PseudoWeather) {
    battle.conditions_changed();
    let field = &mut battle.state.field;
    if !field.pseudo_order.contains(&pw) {
        field.pseudo_order.push(pw);
    }
}

/// A pseudo-weather ended by another module leaves `field.pseudoWeather`.
pub(crate) fn pseudo_weather_ended(battle: &mut Battle, pw: PseudoWeather) {
    battle.conditions_changed();
    let field = &mut battle.state.field;
    field.pseudo_order.retain(|&p| p != pw);
    field.pseudo_targeted &= !(1 << pw as u8);
}

/// Showdown's `field.addPseudoWeather`. Using Trick Room, Magic Room or
/// Wonder Room again ends it (`onFieldRestart`), which counts as success;
/// Gravity fails.
pub(crate) fn add_pseudo_weather(battle: &mut Battle, id: &'static str, source: Position) -> bool {
    // Fairy Lock (conditions.rs).
    // (Persistent, which lengthens these, is a CAP ability.)
    if id.eq_ignore_ascii_case("fairylock") {
        return conditions::add_fairy_lock(battle, source);
    }
    let Some(pw) = pseudo_weather_of(id) else { return false };
    if *pseudo_weather_turns(battle, pw) > 0 {
        if pw == PseudoWeather::Gravity {
            return false;
        }
        remove_pseudo_weather(battle, pw);
        return true;
    }
    *pseudo_weather_turns(battle, pw) = 5;
    battle.state.field.pseudo_order.push(pw);
    battle.conditions_changed();
    let of = actor(battle, source);
    match pw {
        PseudoWeather::TrickRoom => {
            battle.emit(BattleEvent::FieldStart { effect: FieldEffect::TrickRoom, of: Some(of) });
        }
        PseudoWeather::Gravity => {
            battle.emit(BattleEvent::FieldStart { effect: FieldEffect::Gravity, of: None });
            // `onFieldStart` grounds every active Pokémon in the air: Fly and
            // Bounce end (cancelling the move), Magnet Rise too, each with
            // `-activate|<pokemon>|move: Gravity`. (Sky Drop and Telekinesis,
            // which it ends too, are Past moves in Gen 9 and Champions.)
            for side in SideId::BOTH {
                for slot in 0..battle.format.active_per_side() {
                    let pos = Position::new(side, slot as u8);
                    if battle.state.at(pos).is_none() {
                        continue;
                    }
                    let mut applies = super::multiturn::knock_down(battle, pos);
                    let mon = battle.state.at_mut(pos).expect("checked above");
                    if mon.volatiles.has(Volatiles::MAGNET_RISE) {
                        mon.volatiles.clear(Volatiles::MAGNET_RISE);
                        applies = true;
                    }
                    if applies {
                        let actor = actor(battle, pos);
                        battle.emit(BattleEvent::MoveActivate { actor, move_id: ids::GRAVITY, of: None });
                    }
                }
            }
        }
        PseudoWeather::MagicRoom => {
            let mon = battle.state.at(source).map(|m| m.id);
            battle.state.field.magic_room_source = mon.map(|id| (source.side, id));
            battle.emit(BattleEvent::FieldStart { effect: FieldEffect::MagicRoom, of: Some(of) });
            // `onFieldStart` then runs each active Pokémon's item's `End`
            // (`singleEvent`), which `ignoringItem()` already suppresses:
            // Magic Room is up. Items are ignored while it lasts.
        }
        PseudoWeather::WonderRoom => {
            battle.emit(BattleEvent::FieldStart { effect: FieldEffect::WonderRoom, of: Some(of) });
        }
        PseudoWeather::FairyLock | PseudoWeather::EchoedVoice => {
            unreachable!("started by conditions.rs and power.rs")
        }
    }
    pseudo_weather_change(battle, pw, source);
    true
}

/// The end of `field.addPseudoWeather` once a pseudo-weather started
/// (`onFieldStart` succeeded): `runEvent('PseudoWeatherChange', source,
/// source, status)` (Room Service; with tied holders, a draw).
pub(crate) fn pseudo_weather_change(battle: &mut Battle, pw: PseudoWeather, source: Position) {
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::PseudoWeatherChange,
        super::events::Target::Pokemon(source),
        Some(source),
        Effect::PseudoWeather(pw),
        Relay::True,
    );
    super::events::run_event(battle, &mut ev, None);
}

/// Showdown's `field.removePseudoWeather`: `onFieldEnd`, then it is gone.
fn remove_pseudo_weather(battle: &mut Battle, pw: PseudoWeather) {
    *pseudo_weather_turns(battle, pw) = 0;
    battle.state.field.pseudo_order.retain(|&p| p != pw);
    battle.state.field.pseudo_targeted &= !(1 << pw as u8);
    battle.conditions_changed();
    let event = match pw {
        PseudoWeather::TrickRoom => BattleEvent::FieldEnd { effect: FieldEffect::TrickRoom },
        PseudoWeather::Gravity => BattleEvent::FieldEnd { effect: FieldEffect::Gravity },
        PseudoWeather::WonderRoom => BattleEvent::FieldEnd { effect: FieldEffect::WonderRoom },
        PseudoWeather::MagicRoom => {
            let source = battle.state.field.magic_room_source.take();
            match source.and_then(|(side, id)| mon_actor(battle, side, id)) {
                Some(of) => BattleEvent::MagicRoomEnd { of },
                None => BattleEvent::FieldEnd { effect: FieldEffect::MagicRoom },
            }
        }
        PseudoWeather::FairyLock | PseudoWeather::EchoedVoice => {
            unreachable!("ended by conditions.rs and power.rs")
        }
    };
    battle.emit(event);
}

/// The Pokémon with stable `id` on `side` as Showdown names it: with its
/// slot while it is active, without once it has left the field.
pub(crate) fn mon_actor(battle: &Battle, side: SideId, id: u8) -> Option<Actor> {
    let s = battle.state.side(side);
    let position = s.position_of(id)?;
    let active = position < battle.format.active_per_side() && s.active[position].is_some();
    let slot = if active { position as u8 } else { 0 };
    // Illusion shows its disguise.
    let species = super::abilities::switchin::seen_species(s, &s.team[position]);
    let id = super::abilities::switchin::seen_id(s, &s.team[position]);
    Some(Actor { pos: Position::new(side, slot), species, active, id })
}

/// Showdown's `side.addSideCondition`: fails if the condition is already
/// up, except that Spikes and Toxic Spikes add a layer (`onSideRestart`).
pub(crate) fn add_side_condition(battle: &mut Battle, side: SideId, id: &'static str, source: Position) -> bool {
    let Some(condition) = side_condition_of(id) else { return false };
    // The screens' `durationCallback`: Light Clay.
    let screen_turns = match condition {
        SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil => {
            super::items::held::screen_duration(battle, source)
        }
        _ => 5,
    };
    let conditions = &mut battle.state.side_mut(side).conditions;
    let current = conditions.turns(condition);
    if current > 0 {
        let max_layers = match condition {
            SideCondition::Spikes => 3,
            SideCondition::ToxicSpikes => 2,
            _ => return false,
        };
        if current >= max_layers {
            return false;
        }
        *conditions.turns_mut(condition) += 1;
        battle.emit(BattleEvent::SideStart { side, condition });
        return true;
    }
    *conditions.turns_mut(condition) = match condition {
        SideCondition::Tailwind
        | SideCondition::FirePledge
        | SideCondition::GrassPledge
        | SideCondition::WaterPledge => 4,
        SideCondition::Reflect | SideCondition::LightScreen | SideCondition::AuroraVeil => screen_turns,
        SideCondition::Safeguard | SideCondition::Mist => 5,
        // Guards last the turn; hazards have one layer.
        _ => 1,
    };
    conditions.order.push(condition);
    battle.conditions_changed();
    // `onSideStart`.
    if matches!(condition, SideCondition::WideGuard | SideCondition::QuickGuard) {
        let actor = actor(battle, source);
        battle.emit(BattleEvent::GuardStart { actor, condition });
    } else {
        battle.emit(BattleEvent::SideStart { side, condition });
    }
    // `runEvent('SideConditionStart', side, source, status)` (Wind Rider,
    // Wind Power).
    let mut ev = super::events::EventCtx::new(
        pikapro_data::Event::SideConditionStart,
        super::events::Target::Side(side),
        Some(source),
        Effect::Side(condition),
        Relay::True,
    );
    super::events::run_event(battle, &mut ev, None);
    true
}

/// Showdown's `side.addSlotCondition`.
pub(crate) fn add_slot_condition(battle: &mut Battle, target: Position, id: &'static str, source: Position) -> bool {
    slots::add_slot_condition(battle, target, id, source)
}

/// Showdown's `side.removeSideCondition`: `onSideEnd` (the `-sideend`
/// message of the conditions that have one), then it is gone. Returns
/// whether it was up.
pub(crate) fn remove_side_condition(battle: &mut Battle, side: SideId, condition: SideCondition) -> bool {
    let conditions = &mut battle.state.side_mut(side).conditions;
    if conditions.turns(condition) == 0 {
        return false;
    }
    *conditions.turns_mut(condition) = 0;
    conditions.order.retain(|&c| c != condition);
    battle.conditions_changed();
    let silent = condition.is_hazard() || matches!(condition, SideCondition::WideGuard | SideCondition::QuickGuard);
    if !silent {
        battle.emit(BattleEvent::SideEnd { side, condition });
    }
    true
}

/// Gravity is up (`field.pseudoWeather.gravity`).
#[inline]
pub(crate) fn gravity(battle: &Battle) -> bool {
    battle.state.field.gravity_turns > 0
}

/// Showdown's `pokemon.isGrounded()` (as a boolean: Levitate's `null` is
/// false). Works for a Pokémon that has left the field too (by its team
/// position), whose ability and item then do nothing.
pub(crate) fn is_grounded(battle: &Battle, pos: Position) -> bool {
    grounded(battle, pos, false) == Some(true)
}

/// Showdown's `pokemon.isGrounded(negateImmunity)`: `Some(true)` on the
/// ground, `Some(false)` in the air, `None` for Levitate (which
/// `runImmunity` announces).
pub(crate) fn grounded(battle: &Battle, pos: Position, negate_immunity: bool) -> Option<bool> {
    let Some(mon) = battle.state.side(pos.side).team.get(pos.slot as usize) else { return Some(false) };
    // Gravity, Ingrain and Smack Down ground it.
    let gravity = gravity(battle);
    let ingrain = super::manipulation::grounded(mon);
    let smack_down = mon.volatiles.has(Volatiles::SMACK_DOWN);
    // Roost's type loss is in `types`.
    let flying = !negate_immunity && has_type(mon, Type::Flying);
    // The rule (`typing::ground`) reads the item and Levitate only past
    // what grounds it first: they aren't looked up when they can't matter.
    let early = gravity || ingrain || smack_down;
    // Only Iron Ball and Air Balloon count (`typing::ground`): another item
    // acts as none, so whether it is ignored (Klutz, Embargo, Magic Room)
    // isn't asked.
    let item = match mon.item {
        Some(pikapro_data::gen9::items::IRON_BALL | pikapro_data::gen9::items::AIR_BALLOON) if !early => {
            super::items::effective_item(battle, pos)
        }
        _ => None,
    };
    // `hasAbility(['levitate', 'eelevate'])`, unless the
    // move being used ignores it. (Telekinesis would lift it too: a Past
    // move in Gen 9 and Champions.)
    let levitate = !early
        && !flying
        && item != Some(pikapro_data::gen9::items::IRON_BALL)
        && (super::abilities::has_ability(battle, pos, abilities::LEVITATE)
            || super::abilities::has_ability(battle, pos, abilities::EELEVATE))
        && !super::abilities::suppressing_ability(battle, pos);
    let facts = crate::typing::GroundFacts {
        gravity,
        ingrain,
        smack_down,
        magnet_rise: mon.volatiles.has(Volatiles::MAGNET_RISE),
        flying,
        item: Some(item),
        levitate: Some(levitate),
    };
    match crate::typing::ground(&facts) {
        Some(crate::typing::Ground::Grounded) => Some(true),
        Some(crate::typing::Ground::Levitating) => None,
        _ => Some(false),
    }
}

/// Safeguard's `onSetStatus` / `onTryAddVolatile`, sorted together with
/// the Pokémon's handlers. Champions Trick Room gives Pokémon negative
/// stored speeds, so this speed-zero side condition runs before them.
pub(crate) fn safeguard_status(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    let target = ev.target_pos()?;
    let source = ev.source?;
    if ev.effect == Effect::None || source == target {
        return None;
    }
    match ev.relay {
        Relay::Status(_) if ev.effect == Effect::volatile(Volatiles::YAWN) => return None,
        Relay::Status(_) => {}
        Relay::Id("confusion" | "yawn") => {}
        _ => return None,
    }
    if ev.effect.is_move() && battle.move_state.infiltrates && source.side != target.side {
        return None;
    }
    let synchronize = ev.event == pikapro_data::Event::SetStatus
        && ev.effect == Effect::Condition(super::events::Condition::Synchronize);
    if synchronize || ev.effect.is_move() && !super::events::effect_has_secondaries(battle, ev.effect) {
        let target_actor = actor(battle, target);
        battle.emit(BattleEvent::MoveActivate { actor: target_actor, move_id: ids::SAFEGUARD, of: None });
    }
    Some(Relay::Null)
}

/// Misty and Electric Terrain's `onSetStatus` / `onTryAddVolatile`.
/// Terrain has sub-order zero; its speed is zero, just like Safeguard's.
pub(crate) fn terrain_status(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let target = ev.target_pos()?;
    let Effect::Terrain(terrain) = h.effect else { return None };
    let yawn = ev.effect == Effect::volatile(Volatiles::YAWN);
    let move_without_secondaries = ev.effect.is_move() && !super::events::effect_has_secondaries(battle, ev.effect);
    let (announce, result) = match (terrain, ev.relay) {
        (Terrain::Misty, Relay::Status(_)) => {
            (super::abilities::effect_has_status(battle, ev.effect) || yawn, Relay::False)
        }
        (Terrain::Electric, Relay::Status(StatusId::Sleep)) => (yawn || move_without_secondaries, Relay::False),
        (Terrain::Misty, Relay::Id("confusion")) => (move_without_secondaries, Relay::Null),
        (Terrain::Electric, Relay::Id("yawn")) => (true, Relay::Null),
        _ => return None,
    };
    if !is_grounded(battle, target) || super::multiturn::is_semi_invulnerable(battle, target) {
        return None;
    }
    if announce {
        let target_actor = actor(battle, target);
        battle.emit(BattleEvent::TerrainActivate { target: target_actor, terrain });
    }
    Some(result)
}

/// Mist's `onTryBoost` (a side condition's handler of the `TryBoost`
/// event, on the capped changes): drops caused by someone else are
/// removed, with `-activate` unless the effect has secondaries (a
/// secondary effect's drop). An infiltrating move goes through.
pub(crate) fn mist_try_boost(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    let target = ev.target_pos()?;
    let source = ev.source?;
    // `effect.infiltrates`: the active move's (Infiltrator), not Sticky
    // Web's.
    let active = battle.active_move.is_some_and(|(_, m)| ev.effect.move_id() == Some(m));
    if active && battle.move_state.infiltrates && source.side != target.side {
        return None;
    }
    if source == target {
        return None;
    }
    let Relay::Boosts(mut boosts) = ev.relay else { return None };
    let mut blocked = false;
    for id in pikapro_data::BoostId::ALL {
        if boosts.get(id).is_some_and(|v| v < 0) {
            boosts.delete(id);
            blocked = true;
        }
    }
    ev.relay = Relay::Boosts(boosts);
    if blocked && !super::events::effect_has_secondaries(battle, ev.effect) {
        let target_actor = actor(battle, target);
        battle.emit(BattleEvent::MoveActivate { actor: target_actor, move_id: ids::MIST, of: None });
    }
    None
}

/// Wide Guard's and Quick Guard's `onTryHit` for one target: a spread move
/// or a priority move that Protect would stop is blocked (`NOT_FAIL`).
#[inline]
pub(crate) fn guard_try_hit(battle: &mut Battle, target: Position, data: &MoveData) -> bool {
    // No side condition, no guard (the common case, inlined).
    battle.state.side(target.side).conditions.order.is_empty() || guards_try_hit(battle, target, data)
}

/// [`guard_try_hit`] with side conditions up.
fn guards_try_hit(battle: &mut Battle, target: Position, data: &MoveData) -> bool {
    let conditions = battle.state.side(target.side).conditions;
    // Both are `onTryHitPriority: 4` side conditions, run in the order
    // they started.
    for &condition in conditions.order.iter() {
        let blocks = match condition {
            SideCondition::WideGuard => matches!(data.target, MoveTarget::AllAdjacent | MoveTarget::AllAdjacentFoes),
            // `move.priority`: the action's, after `ModifyPriority`; an
            // encored or called move keeps the chosen move's.
            SideCondition::QuickGuard => super::order::active_priority(battle, data) > 0,
            _ => false,
        };
        // `checkMoveBypassesProtect`: `runEvent('HitProtect')` (Piercing
        // Drill, the Champions mod's Unseen Fist) can let it through.
        if !blocks || !data.flags.contains(MoveFlags::PROTECT) {
            continue;
        }
        if let Some((user, move_id)) = super::active_dex_move(battle) {
            let relay = super::events::Relay::True;
            let event = pikapro_data::Event::HitProtect;
            if !super::events::move_event(battle, event, user, Some(target), move_id, data, relay).is_truthy() {
                continue;
            }
        }
        let move_id = if condition == SideCondition::WideGuard { ids::WIDE_GUARD } else { ids::QUICK_GUARD };
        let target_actor = actor(battle, target);
        battle.emit(BattleEvent::MoveActivate { actor: target_actor, move_id, of: None });
        if let Some((user, _)) = battle.active_move {
            super::multiturn::protected_locked_move(battle, user);
        }
        return false;
    }
    true
}

/// `hitStepBreakProtect` (Feint and co.) removes the guards on `side`.
/// Returns whether there was any.
pub(crate) fn break_guards(battle: &mut Battle, side: SideId) -> bool {
    // (Showdown also removes Crafty Shield and Mat Block: Past moves in Gen 9
    // and Champions.)
    let quick = remove_side_condition(battle, side, SideCondition::QuickGuard);
    remove_side_condition(battle, side, SideCondition::WideGuard) || quick
}

/// Haze's `onHitField`: `-clearallboost`, then every active Pokémon loses
/// its stat changes.
fn haze(battle: &mut Battle) {
    battle.emit(BattleEvent::ClearAllBoost);
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            if let Some(mon) = battle.state.at_mut(Position::new(side, slot as u8)).filter(|m| !m.is_fainted()) {
                mon.boosts = Default::default();
            }
        }
    }
}

/// Court Change's `onHitField`: the sides swap their conditions (except
/// the guards). Each side's swappable conditions are taken out, in order,
/// and added after the other side's remaining ones, keeping their state.
fn court_change(battle: &mut Battle, user: Position) -> bool {
    let swappable = |c: SideCondition| !matches!(c, SideCondition::WideGuard | SideCondition::QuickGuard);
    let [a, b] = [battle.state.sides[0].conditions, battle.state.sides[1].conditions];
    if !a.order.iter().chain(b.order.iter()).any(|&c| swappable(c)) {
        return false;
    }
    for (side, (mine, theirs)) in SideId::BOTH.into_iter().zip([(a, b), (b, a)]) {
        let conditions = &mut battle.state.side_mut(side).conditions;
        let mut order = StackVec::<SideCondition, 16>::new();
        for &c in mine.order.iter().filter(|&&c| !swappable(c)) {
            order.push(c);
        }
        for &c in theirs.order.iter().filter(|&&c| swappable(c)) {
            order.push(c);
        }
        for c in SideCondition::ALL {
            if swappable(c) {
                *conditions.turns_mut(c) = theirs.turns(c);
            }
        }
        conditions.order = order;
        battle.conditions_changed();
    }
    battle.emit(BattleEvent::SwapSideConditions);
    let user_actor = actor(battle, user);
    battle.emit(BattleEvent::MoveActivate { actor: user_actor, move_id: ids::COURT_CHANGE, of: None });
    true
}

/// End-of-turn handlers (`fieldEvent('Residual')`), collected in Showdown's
/// order: pseudo-weathers, weather and terrain; then per side its side
/// conditions (in insertion order) and per active Pokémon its slot
/// conditions and the side's and the field's handlers for it.
pub(crate) fn residual_handlers(battle: &Battle, handlers: &mut Handlers) {
    let field = battle.state.field;
    // Field-level handlers have no speed.
    for &pw in field.pseudo_order.iter() {
        let sub_order = match pw {
            PseudoWeather::TrickRoom => 1,
            PseudoWeather::Gravity => 2,
            PseudoWeather::WonderRoom => 5,
            PseudoWeather::MagicRoom => 6,
            // Durations without `onFieldResidualOrder`: unordered, with the
            // pseudo-weather's default sub-order (see `pseudo_targeted`).
            PseudoWeather::FairyLock | PseudoWeather::EchoedVoice => {
                let sub_order = if pseudo_targeted(battle, pw) { 5 } else { 2 };
                let run = if pw == PseudoWeather::FairyLock {
                    super::conditions::tick_fairy_lock
                } else {
                    super::power::tick_echoed_voice
                };
                handlers.push(Handler { sub_order, callback: Callback::Plain(run), ..Handler::default() });
                continue;
            }
        };
        handlers.push(Handler {
            order: Some(27),
            sub_order,
            callback: Callback::Plain(residual_pseudo_weather),
            arg: pw as u8,
            ..Handler::default()
        });
    }
    // `arg` is the weather or terrain collected; the terrain's `index` is
    // the turns it had left: one that replaced it during the residual has
    // a new state, and the old handler counts down the old one (see
    // `residual_terrain`).
    if let Some(w) = field.weather {
        let sub_order = sub_order::FIELD_CONDITION;
        let arg = w as u8;
        handlers.push(Handler {
            order: Some(1),
            sub_order,
            callback: Callback::Plain(residual_weather),
            arg,
            ..Handler::default()
        });
    }
    if let Some(t) = field.terrain {
        let (arg, index) = (t as u8, field.terrain_turns);
        handlers.push(Handler {
            order: Some(27),
            sub_order: 7,
            callback: Callback::Plain(residual_terrain),
            arg,
            index,
            ..Handler::default()
        });
    }
    for side in SideId::BOTH {
        let s = battle.state.side(side);
        for &condition in s.conditions.order.iter() {
            // `onSideResidualOrder` / `SubOrder`; hazards have no duration.
            let (order, sub_order) = match condition {
                SideCondition::Reflect => (Some(26), 1),
                SideCondition::LightScreen => (Some(26), 2),
                SideCondition::Safeguard => (Some(26), 3),
                SideCondition::Mist => (Some(26), 4),
                SideCondition::Tailwind => (Some(26), 5),
                SideCondition::WaterPledge => (Some(26), 7),
                SideCondition::FirePledge => (Some(26), 8),
                SideCondition::GrassPledge => (Some(26), 9),
                SideCondition::AuroraVeil => (Some(26), 10),
                SideCondition::WideGuard | SideCondition::QuickGuard => (None, sub_order::SIDE_CONDITION),
                _ => continue,
            };
            handlers.push(Handler {
                order,
                sub_order,
                holder: Holder::Side(side),
                callback: Callback::Plain(residual_side_condition),
                arg: condition as u8,
                ..Handler::default()
            });
        }
        // Per-Pokémon handlers take its speed. Fainted Pokémon still in
        // their slot take part in the sort.
        for slot in 0..battle.format.active_per_side().min(s.team.len()) {
            // (After a stale switch, the fainted Pokémon twice, with the
            // conditions of the slot it stands in.)
            let index = super::field_index(battle, side, slot);
            let pos = Position::new(side, index as u8);
            let speed = s.team[index].speed;
            slots::residual_handlers(battle, pos, speed, handlers);
            pledges::residual_handlers(battle, pos, speed, handlers);
            // Grassy Terrain's `onResidual`.
            if field.terrain == Some(Terrain::Grassy) {
                handlers.push(Handler {
                    order: Some(5),
                    sub_order: 2,
                    speed,
                    holder: Holder::FieldOn(pos),
                    callback: Callback::Plain(grassy_terrain_heal),
                    ..Handler::default()
                });
            }
        }
    }
}

/// `BeforeMove` handlers of the field: Gravity (priority 6) stops moves
/// that can't be used in it.
pub(crate) fn before_move_handlers<const N: usize>(battle: &Battle, handlers: &mut super::handlers::SpillList<N>) {
    if gravity(battle) {
        let sub_order = sub_order::FIELD_CONDITION;
        handlers.push(Handler {
            priority: 6,
            sub_order,
            callback: Callback::Plain(gravity_before_move),
            ..Handler::default()
        });
    }
}

/// The field's `ModifyMove` handlers (`runEvent('ModifyMove')` in
/// `useMoveInner`, after the user's own): Gravity and Wonder Room tie
/// (field holders), so with both up Showdown shuffles them. Gravity stops a
/// move with the `gravity` flag that got this far (one called by another
/// move). False stops the move before its `|move|` line.
pub(crate) fn modify_move(battle: &mut Battle, user: Position, move_id: MoveId, data: &MoveData) -> bool {
    let field = battle.state.field;
    // The handlers in collection order (the pseudo-weathers' key order).
    let mut order: StackVec<PseudoWeather, 2> = field
        .pseudo_order
        .iter()
        .copied()
        .filter(|&pw| matches!(pw, PseudoWeather::Gravity | PseudoWeather::WonderRoom))
        .collect();
    if order.len() == 2 {
        let sub_order = |pw: PseudoWeather| if pseudo_targeted(battle, pw) { 5 } else { 2 };
        if sub_order(order[0]) == sub_order(order[1]) {
            battle.rng.shuffle(&mut order);
        } else if sub_order(order[0]) > sub_order(order[1]) {
            order.swap(0, 1);
        }
    }
    // Wonder Room swaps `overrideOffensiveStat` Def/SpD (Body Press; the
    // damage calculation reads the swap; this writes the hint). The
    // rainbow's `onModifyMove` (a side handler, sub-order 4) never ties
    // with these.
    for &pw in order.iter() {
        set_pseudo_targeted(battle, pw);
        if pw == PseudoWeather::WonderRoom
            && let Some(stat) = super::power::offensive_stat(move_id)
        {
            let special = stat == pikapro_data::Stat::SpD;
            battle.hint(crate::event::Hint::WonderRoomDefense { move_id, special });
        }
        if pw == PseudoWeather::Gravity && data.flags.contains(MoveFlags::GRAVITY) {
            let user_actor = actor(battle, user);
            battle.emit(BattleEvent::GravityCant { actor: user_actor, move_id });
            return false;
        }
    }
    true
}

pub(crate) fn pseudo_targeted(battle: &Battle, pw: PseudoWeather) -> bool {
    battle.state.field.pseudo_targeted & (1 << pw as u8) != 0
}

/// `runEvent` ran one of the pseudo-weather's handlers: its effect state
/// now has a `target` (see `FieldState::pseudo_targeted`).
pub(crate) fn set_pseudo_targeted(battle: &mut Battle, pw: PseudoWeather) {
    battle.state.field.pseudo_targeted |= 1 << pw as u8;
}

/// Gravity's `onBeforeMove`: `cant|<user>|move: Gravity|<move>`.
fn gravity_before_move(battle: &mut Battle, _: &Handler) -> bool {
    set_pseudo_targeted(battle, PseudoWeather::Gravity);
    let Some((user, move_id)) = battle.active_move else { return true };
    // `recharge` (no data) and Struggle have no `gravity` flag.
    if move_id.0 as usize >= battle.format.dex.moves.len()
        || !battle.format.dex.move_(move_id).flags.contains(MoveFlags::GRAVITY)
    {
        return true;
    }
    let user_actor = actor(battle, user);
    battle.emit(BattleEvent::GravityCant { actor: user_actor, move_id });
    false
}

/// `endTurn`, after the volatiles' `DisableMove` handlers: Gravity's
/// `onDisableMove` (it never ties with them, so it adds no draw) disables
/// moves with the `gravity` flag. A pledge still waiting is forgotten.
pub(crate) fn end_turn(battle: &mut Battle) {
    for side in SideId::BOTH {
        battle.state.side_mut(side).conditions.pledge_combo = None;
    }
    if !gravity(battle) {
        return;
    }
    // `runEvent('DisableMove')` runs for every Pokémon in a slot, fainted
    // ones included (the field's handlers still apply to them).
    set_pseudo_targeted(battle, PseudoWeather::Gravity);
    let dex = battle.format.dex;
    for side in SideId::BOTH {
        for slot in 0..battle.format.active_per_side() {
            let Some(mon) = battle.state.side_mut(side).active_mut(slot) else { continue };
            for m in mon.moves.iter_mut() {
                if dex.move_(m.id).flags.contains(MoveFlags::GRAVITY) {
                    m.disabled = true;
                }
            }
        }
    }
}

/// Gravity's `onModifyAccuracy`: numeric accuracy is multiplied by
/// 6840/4096 (before accuracy and evasion stages). Running it gives the
/// pseudo-weather's state a `target`.
pub(crate) fn gravity_modify_accuracy(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    set_pseudo_targeted(battle, PseudoWeather::Gravity);
    if let Relay::Num(_) = ev.relay {
        ev.chain(6840);
    }
    None
}

/// Wonder Room swaps the raw Defense and Special Defense a Pokémon defends
/// with (`calculateStat`); stat stages and modifiers still follow `stat`.
pub(crate) fn defensive_stat(battle: &Battle, stat: Stat) -> Stat {
    match stat {
        Stat::Def if battle.state.field.wonder_room_turns > 0 => Stat::SpD,
        Stat::SpD if battle.state.field.wonder_room_turns > 0 => Stat::Def,
        other => other,
    }
}

/// The weather's `onFieldResidual`: count down, then either end it or
/// announce it and run `eachEvent('Weather')` (sandstorm damage) followed by
/// `eachEvent('Update')`.
fn residual_weather(battle: &mut Battle, handler: &Handler) -> bool {
    // Gone, or replaced: skipped (no `faintMessages`). The weather's
    // handler runs first (order 1), so nothing in the residual replaces it
    // before it.
    let Some(weather) = battle.state.field.weather else { return false };
    if weather as u8 != handler.arg {
        return false;
    }
    let field = &mut battle.state.field;
    if field.weather_turns > 0 {
        field.weather_turns -= 1;
        if field.weather_turns == 0 {
            // `clearWeather`: `FieldEnd`, then `eachEvent('WeatherChange')`.
            field.weather = None;
            battle.conditions_changed();
            battle.emit(BattleEvent::Weather { weather: None, upkeep: false });
            each_event(battle, Some(pikapro_data::Event::WeatherChange), Effect::None);
            // Ran out: no `faintMessages` after it.
            return false;
        }
    }
    battle.emit(BattleEvent::Weather { weather: Some(weather), upkeep: true });
    // `eachEvent('Weather')` (sandstorm's damage, Dry Skin, Rain Dish, Ice
    // Body, Solar Power, ...), then `eachEvent('Update')`; sand and snow
    // only if `isWeather` (not under Air Lock).
    let checked = matches!(weather, Weather::Sand | Weather::Snow);
    if !checked || super::abilities::effective_weather(battle) == Some(weather) {
        each_event(battle, Some(pikapro_data::Event::Weather), Effect::Weather(weather));
        each_update(battle);
    }
    true
}

/// Sandstorm's `onWeather`: 1/16 of max HP, unless immune
/// (`runStatusImmunity('sandstorm')` in `spreadDamage`).
pub(crate) fn sand_weather(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    let pos = ev.target_pos()?;
    if slots::off_field(battle, pos) {
        return None;
    }
    let amount = super::hp_fraction(battle.state.at(pos)?.max_hp() as u32, 1, 16);
    let effect = Effect::Weather(Weather::Sand);
    super::spread_damage(battle, pos, amount, None, effect, DamageSource::Weather(Weather::Sand));
    None
}

/// `runStatusImmunity('sandstorm')`: Rock, Ground and Steel types, then the
/// `Immunity` event (Overcoat, Sand Veil, Safety Goggles; Dig and Dive).
pub(crate) fn weather_immune(battle: &mut Battle, pos: Position, weather: Weather) -> bool {
    let Some(mon) = battle.state.at(pos) else { return true };
    let (types, id): (&[Type], &'static str) = match weather {
        Weather::Sand => (&[Type::Rock, Type::Ground, Type::Steel], "sandstorm"),
        _ => return false,
    };
    types.iter().any(|&t| has_type(mon, t)) || super::events::immunity_event(battle, pos, id) != Some(true)
}

/// The terrain's duration (`fieldEvent('Residual')`, order 27).
fn residual_terrain(battle: &mut Battle, handler: &Handler) -> bool {
    let field = &mut battle.state.field;
    // Gone: skipped (no `faintMessages`).
    let Some(terrain) = field.terrain else { return false };
    if terrain as u8 != handler.arg {
        // Replaced during the residual (Seed Sower hit by Future Sight):
        // `fieldEvent` counts down the state it collected before checking
        // that it is still the terrain's, and at zero `clearTerrain` ends
        // whatever terrain is up.
        if handler.index == 1 {
            clear_terrain(battle);
        }
        return false;
    }
    if field.terrain_turns > 0 {
        field.terrain_turns -= 1;
        if field.terrain_turns == 0 {
            clear_terrain(battle);
            // Ran out: no `faintMessages` after it.
            return false;
        }
    }
    true
}

/// A pseudo-weather's duration; at zero it ends.
fn residual_pseudo_weather(battle: &mut Battle, handler: &Handler) -> bool {
    let pw = [PseudoWeather::TrickRoom, PseudoWeather::Gravity, PseudoWeather::MagicRoom, PseudoWeather::WonderRoom]
        [handler.arg as usize];
    let turns = pseudo_weather_turns(battle, pw);
    if *turns > 0 {
        *turns -= 1;
        if *turns == 0 {
            remove_pseudo_weather(battle, pw);
            // Ran out: no `faintMessages` after it.
            return false;
        }
    }
    true
}

/// A side condition's duration; at zero it ends (`removeSideCondition`).
fn residual_side_condition(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::Side(side) = handler.holder else { return true };
    let condition = SideCondition::ALL[handler.arg as usize];
    let turns = battle.state.side_mut(side).conditions.turns_mut(condition);
    if *turns > 0 {
        *turns -= 1;
        if *turns == 0 {
            *turns = 1;
            remove_side_condition(battle, side, condition);
            // Ran out: no `faintMessages` after it.
            return false;
        }
    }
    true
}

/// Grassy Terrain's `onResidual`: grounded Pokémon heal 1/16 of their max HP.
fn grassy_terrain_heal(battle: &mut Battle, handler: &Handler) -> bool {
    let Holder::FieldOn(pos) = handler.holder else { return true };
    // Semi-invulnerable Pokémon (Dig, Dive) are skipped.
    if battle.state.field.terrain != Some(Terrain::Grassy)
        || !is_grounded(battle, pos)
        || super::multiturn::is_semi_invulnerable(battle, pos)
    {
        return true;
    }
    let Some(mon) = battle.state.at(pos) else { return true };
    if slots::off_field(battle, pos) {
        return true;
    }
    let amount = super::hp_fraction(u32::from(mon.max_hp()), 1, 16);
    apply_heal(battle, pos, amount, HealSource::Terrain(Terrain::Grassy));
    true
}

/// `TryHit` handlers of priority 4 for one target, in Showdown's order: the
/// target side's guards, then Psychic Terrain (grounded Pokémon are
/// protected from the foes' priority moves). False blocks.
pub(crate) fn try_hit(battle: &mut Battle, target: Position, user: Position, move_id: MoveId, data: &MoveData) -> bool {
    if !guard_try_hit(battle, target, data) {
        return false;
    }
    // A called move has its caller's priority.
    let priority = super::order::active_priority(battle, data);
    if battle.state.field.terrain != Some(Terrain::Psychic) || priority <= 0 || data.target == MoveTarget::User {
        return true;
    }
    if super::multiturn::is_semi_invulnerable(battle, target) || target.side == user.side {
        return true;
    }
    if !is_grounded(battle, target) {
        // The hint, for a move whose own priority is positive
        // (`dex.moves.get(effect.id).priority`).
        if battle.format.dex.move_(move_id).priority > 0 {
            battle.hint(crate::event::Hint::PsychicTerrainAirborne);
        }
        return true;
    }
    let target_actor = actor(battle, target);
    battle.emit(BattleEvent::TerrainActivate { target: target_actor, terrain: Terrain::Psychic });
    false
}

/// The terrains' `onBasePower` (priority 6): Electric, Grassy and Psychic
/// Terrain boost their type's moves from a grounded user (not while it is
/// semi-invulnerable, except in Grassy Terrain); Grassy Terrain halves
/// Earthquake, Bulldoze and Magnitude and Misty Terrain Dragon moves
/// against a grounded target.
pub(crate) fn terrain_base_power(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let super::events::Effect::Terrain(terrain) = h.effect else { return None };
    let (attacker, defender) = (ev.target_pos()?, ev.source?);
    let ty = ev.move_data()?.ty;
    let move_id = ev.effect.move_id();
    let semi = |pos| super::multiturn::is_semi_invulnerable(battle, pos);
    let boosted = |t: Type| ty == t && is_grounded(battle, attacker);
    match terrain {
        Terrain::Electric if boosted(Type::Electric) && !semi(attacker) => ev.chain(5325),
        Terrain::Psychic if boosted(Type::Psychic) && !semi(attacker) => ev.chain(5325),
        Terrain::Grassy => {
            let weakened = [ids::EARTHQUAKE, ids::BULLDOZE, ids::MAGNITUDE];
            if move_id.is_some_and(|m| weakened.contains(&m)) && is_grounded(battle, defender) && !semi(defender) {
                ev.chain(2048);
            } else if boosted(Type::Grass) {
                ev.chain(5325);
            }
        }
        Terrain::Misty if ty == Type::Dragon && is_grounded(battle, defender) && !semi(defender) => ev.chain(2048),
        _ => {}
    }
    None
}

/// The weathers' `onWeatherModifyDamage` (a `priorityEvent`): sun boosts
/// Fire and weakens Water moves (and boosts Hydro Steam), rain the
/// reverse; harsh sun boosts Fire and heavy rain Water moves. The
/// defender's weather counts (Utility Umbrella); Hydro Steam checks the
/// attacker's. While a Mega Sol Pokémon uses a move, everyone's weather is
/// sun for a weather's handler (`pokemon.effectiveWeather()`).
pub(crate) fn weather_modify_damage(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    use super::mega::{WeatherAsker, pokemon_weather};
    let super::events::Effect::Weather(weather) = h.effect else { return None };
    if weather == Weather::Sun {
        return sun_modify_damage(battle, ev, WeatherAsker::Weather);
    }
    let defender = ev.source?;
    let ty = ev.move_data()?.ty;
    let defender_weather = pokemon_weather(battle, defender, WeatherAsker::Weather);
    match weather {
        Weather::Rain => {
            if defender_weather != Some(Weather::Rain) {
                return None;
            }
            match ty {
                Type::Water => ev.chain_modify(3, 2),
                Type::Fire => ev.chain_modify(1, 2),
                _ => None,
            }
        }
        Weather::HarshSun if defender_weather == Some(Weather::HarshSun) && ty == Type::Fire => ev.chain_modify(3, 2),
        Weather::HeavyRain if defender_weather == Some(Weather::HeavyRain) && ty == Type::Water => {
            ev.chain_modify(3, 2)
        }
        _ => None,
    }
}

/// Sunny Day's `onWeatherModifyDamage`, also called by Mega Sol's:
/// `asker` is the effect running it (Showdown's `battle.effect`, which
/// `pokemon.effectiveWeather()` reads).
pub(crate) fn sun_modify_damage(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    asker: super::mega::WeatherAsker,
) -> Option<Relay> {
    use super::mega::pokemon_weather;
    let (attacker, defender) = (ev.target_pos()?, ev.source?);
    let ty = ev.move_data()?.ty;
    if ev.effect.move_id() == Some(ids::HYDRO_STEAM) && pokemon_weather(battle, attacker, asker) == Some(Weather::Sun) {
        return ev.chain_modify(3, 2);
    }
    if pokemon_weather(battle, defender, asker) != Some(Weather::Sun) {
        return None;
    }
    match ty {
        Type::Fire => ev.chain_modify(3, 2),
        Type::Water => ev.chain_modify(1, 2),
        _ => None,
    }
}

/// Delta Stream's `onEffectiveness` (priority -1): a damaging move that is
/// super effective on the Flying type is neutral on it instead
/// (`-fieldactivate|Delta Stream`).
pub(crate) fn delta_stream_effectiveness(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    let data = ev.move_data()?;
    if data.category == MoveCategory::Status || ev.ty != Some(Type::Flying) || ev.value() <= 0 {
        return None;
    }
    battle.emit(BattleEvent::WeatherActivate { weather: Weather::StrongWinds });
    Some(Relay::Num(0))
}

/// Reflect's, Light Screen's and Aurora Veil's `onAnyModifyDamage` (side
/// conditions, collected from both sides for every hit, so they tie in the
/// handler sort): a physical (Reflect), special (Light Screen) or any
/// (Aurora Veil, unless the other screen covers the hit) move against a
/// Pokémon on the screen's side is weakened, unless it crits or infiltrates
/// (2732/4096 with more than one Pokémon per side, else 1/2).
pub(crate) fn screen_modify_damage(battle: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    let Holder::Side(side) = h.holder else { return None };
    let (source, target) = (ev.target_pos()?, ev.source?);
    if target == source || target.side != side {
        return None;
    }
    let category = ev.move_data()?.category;
    let c = &battle.state.side(side).conditions;
    let applies = match h.effect {
        super::events::Effect::Side(SideCondition::Reflect) => category == MoveCategory::Physical,
        super::events::Effect::Side(SideCondition::LightScreen) => category == MoveCategory::Special,
        super::events::Effect::Side(SideCondition::AuroraVeil) => {
            !(c.reflect > 0 && category == MoveCategory::Physical
                || c.light_screen > 0 && category == MoveCategory::Special)
        }
        _ => false,
    };
    if !applies || battle.move_state.hit_data(target).crit || battle.move_state.infiltrates {
        return None;
    }
    if battle.format.active_per_side() > 1 {
        ev.chain(2732);
        None
    } else {
        ev.chain_modify(1, 2)
    }
}

/// Sandstorm's `onModifySpD` (priority 10): a Rock type's Special Defense
/// is multiplied by 1.5 (`this.modify`, applied at once).
pub(crate) fn sand_modify_spd(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    weather_stat_boost(battle, ev, Weather::Sand, Type::Rock)
}

/// Snow's `onModifyDef` (priority 10): an Ice type's Defense is multiplied
/// by 1.5.
pub(crate) fn snow_modify_def(battle: &mut Battle, ev: &mut EventCtx<'_>, _: &Handler) -> Option<Relay> {
    weather_stat_boost(battle, ev, Weather::Snow, Type::Ice)
}

fn weather_stat_boost(battle: &mut Battle, ev: &mut EventCtx<'_>, weather: Weather, ty: Type) -> Option<Relay> {
    let pos = ev.target_pos()?;
    let mon = battle.state.side(pos.side).team.get(pos.slot as usize)?;
    // The Pokémon's `effectiveWeather()`: the field's (Utility Umbrella
    // hides neither), or sun while a Mega Sol Pokémon uses a move.
    let asker = super::mega::WeatherAsker::Weather;
    if !has_type(mon, ty) || super::mega::pokemon_weather(battle, pos, asker) != Some(weather) {
        return None;
    }
    Some(Relay::Num(modify(ev.value().max(0) as u32, 6144) as i32))
}

/// Tailwind's `onModifySpe` (`chainModify(2)`) and the swamp's (Grass
/// Pledge: `chainModify(0.25)`), side conditions of the Pokémon's side.
pub(crate) fn side_modify_spe(_: &mut Battle, ev: &mut EventCtx<'_>, h: &Handler) -> Option<Relay> {
    match h.effect {
        super::events::Effect::Side(SideCondition::Tailwind) => ev.chain_modify(2, 1),
        super::events::Effect::Side(SideCondition::GrassPledge) => ev.chain_modify(1, 4),
        _ => None,
    }
}

/// Showdown's `battle.damage` from a side condition (`[from] Stealth
/// Rock`): at least 1 HP, logged with the condition's name. Returns the HP
/// removed.
pub(crate) fn condition_damage(battle: &mut Battle, pos: Position, amount: u32, condition: SideCondition) -> u16 {
    let target = actor(battle, pos);
    let off_field = slots::off_field(battle, pos);
    if off_field || battle.state.at(pos).is_none_or(|m| m.is_fainted()) {
        return 0;
    }
    // `runEvent('Damage')` (Magic Guard).
    let effect = super::events::Effect::Side(condition);
    let Some(amount) = super::damage_event(battle, pos, amount, None, effect) else { return 0 };
    let Some(mon) = battle.state.at_mut(pos) else { return 0 };
    let dealt = amount.min(mon.hp as u32) as u16;
    mon.hp -= dealt;
    super::power::record_hurt(mon, dealt);
    let (hp, max_hp, status, fainted) = (mon.hp, mon.max_hp(), mon.status, mon.is_fainted());
    battle.emit(BattleEvent::ConditionDamage { target, hp, max_hp, status, condition });
    if fainted {
        super::queue_faint(battle, pos.into());
    }
    dealt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::handlers::run_residual;
    use crate::testing::sample_team;
    use crate::{BattleOptions, Format, Seed};

    fn battle(format: Format) -> Battle {
        let teams = [sample_team(&format), sample_team(&format)];
        Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap()
    }

    fn prevention_battle(preset: &Format, ability: pikapro_data::AbilityId, trick_room: bool) -> Battle {
        use crate::format::TeamPreview;
        use crate::{PokemonSet, Team};
        use pikapro_data::gen9::{abilities as a, species as s};

        let mut format = preset.clone();
        format.team_size = (1, 6);
        format.team_preview = TeamPreview::None;
        let set = |species, ability| {
            let mut set = PokemonSet::new(species).with_ability(ability).with_moves(&[ids::SPLASH]);
            if format.stat_system == crate::format::StatSystem::StatPoints {
                set.investment = crate::Investment::StatPoints(pikapro_data::Stats::splat(0));
            }
            set
        };
        let teams = [
            Team::new(vec![set(s::BLISSEY, ability), set(s::MEW, a::NO_ABILITY)]),
            Team::new(vec![set(s::SNORLAX, a::NO_ABILITY), set(s::WOBBUFFET, a::NO_ABILITY)]),
        ];
        let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(60_000_000))).unwrap();
        b.state.field.trick_room_turns = if trick_room { 5 } else { 0 };
        super::super::update_speed(&mut b);
        b.log.drain();
        b
    }

    #[test]
    fn safeguard_silently_blocks_the_secondary_fling_adds() {
        use crate::state::SideCondition;
        use pikapro_data::gen9::{abilities as a, items as i};
        let target = Position::new(SideId::P1, 0);
        let source = Position::new(SideId::P2, 0);
        for preset in Format::presets() {
            for (item, status) in [
                (i::LIGHT_BALL, StatusId::Paralysis),
                (i::FLAME_ORB, StatusId::Burn),
                (i::POISON_BARB, StatusId::Poison),
                (i::TOXIC_ORB, StatusId::Toxic),
            ] {
                let mut b = prevention_battle(preset, a::NO_ABILITY, false);
                b.state.at_mut(source).unwrap().item = Some(item);
                b.active_move = Some((source, ids::FLING));
                let conditions = &mut b.state.side_mut(target.side).conditions;
                conditions.safeguard = 5;
                conditions.order.push(SideCondition::Safeguard);
                b.conditions_changed();
                let data = b.format.dex.move_(ids::FLING);
                let data = super::super::itemmoves::prepare_hit(&mut b, source, ids::FLING, data).unwrap().unwrap();
                assert_eq!(data.secondaries[0].status, Some(status));
                b.log.drain();
                b.rng.enable_trace();
                super::super::secondaries::secondaries(&mut b, &[target].into_iter().collect(), source, &data);
                b.flush_rng_trace();
                assert_eq!(b.state.at(target).unwrap().status, crate::state::Status::None);
                // Still roll the unconditional secondary; no Safeguard activation.
                assert!(
                    matches!(b.events(), [BattleEvent::Rng { bound: 100, .. }]),
                    "{} {item:?}: {:?}",
                    preset.id,
                    b.events()
                );
            }
        }
    }

    #[test]
    fn field_status_prevention_sorts_with_abilities_in_every_format() {
        use crate::format::Mechanics;
        use crate::state::SideCondition;
        use pikapro_data::gen9::abilities as a;
        let target = Position::new(SideId::P1, 0);
        let source = Position::new(SideId::P2, 0);
        let cases = [
            (StatusId::Paralysis, ids::STUN_SPORE, a::LIMBER),
            (StatusId::Sleep, ids::SPORE, a::INSOMNIA),
            (StatusId::Sleep, ids::SPORE, a::VITAL_SPIRIT),
            (StatusId::Poison, ids::POISON_POWDER, a::IMMUNITY),
            (StatusId::Toxic, ids::TOXIC, a::PASTEL_VEIL),
            (StatusId::Burn, ids::WILL_O_WISP, a::WATER_VEIL),
            (StatusId::Burn, ids::WILL_O_WISP, a::THERMAL_EXCHANGE),
            (StatusId::Paralysis, ids::STUN_SPORE, a::PURIFYING_SALT),
        ];
        for preset in Format::presets() {
            for trick_room in [false, true] {
                for (status, move_id, ability) in cases {
                    // Terrain and Safeguard can each preempt an ability.
                    for terrain in [Some(Terrain::Misty), Some(Terrain::Electric), None] {
                        if terrain == Some(Terrain::Electric) && status != StatusId::Sleep {
                            continue;
                        }
                        let mut b = prevention_battle(preset, ability, trick_room);
                        b.active_move = Some((source, move_id));
                        b.state.field.terrain = terrain;
                        if terrain.is_none() {
                            let conditions = &mut b.state.side_mut(target.side).conditions;
                            conditions.safeguard = 5;
                            conditions.order.push(SideCondition::Safeguard);
                        }
                        b.conditions_changed();
                        assert!(!super::super::status::try_set_status(&mut b, target, status, Some(source), false));
                        let field_first = trick_room && preset.mechanics == Mechanics::Champions;
                        assert!(
                            matches!(b.events(), [BattleEvent::ImmuneFrom { .. } | BattleEvent::Block { .. }])
                                != field_first,
                            "{} trick={trick_room} {ability:?} {terrain:?}: {:?}",
                            preset.id,
                            b.events()
                        );
                        if field_first {
                            assert!(matches!(
                                b.events(),
                                [BattleEvent::TerrainActivate { .. }
                                    | BattleEvent::MoveActivate { move_id: ids::SAFEGUARD, .. }]
                            ));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn field_volatile_prevention_sorts_with_abilities_in_every_format() {
        use crate::format::Mechanics;
        use crate::state::SideCondition;
        use pikapro_data::gen9::abilities as a;
        let target = Position::new(SideId::P1, 0);
        let source = Position::new(SideId::P2, 0);
        for preset in Format::presets() {
            for trick_room in [false, true] {
                for (terrain, id, move_id, ability) in [
                    (Some(Terrain::Misty), "confusion", ids::CONFUSE_RAY, a::OWN_TEMPO),
                    (None, "confusion", ids::CONFUSE_RAY, a::OWN_TEMPO),
                    (Some(Terrain::Electric), "yawn", ids::YAWN, a::INSOMNIA),
                    (None, "yawn", ids::YAWN, a::INSOMNIA),
                ] {
                    let mut b = prevention_battle(preset, ability, trick_room);
                    b.active_move = Some((source, move_id));
                    b.state.field.terrain = terrain;
                    if terrain.is_none() {
                        let conditions = &mut b.state.side_mut(target.side).conditions;
                        conditions.safeguard = 5;
                        conditions.order.push(SideCondition::Safeguard);
                    }
                    b.conditions_changed();
                    assert_eq!(super::super::volatiles::add_volatile(&mut b, target, id, Some(source), false), None);
                    let field_first = trick_room && preset.mechanics == Mechanics::Champions;
                    let announced_field = matches!(
                        b.events(),
                        [BattleEvent::TerrainActivate { .. }
                            | BattleEvent::MoveActivate { move_id: ids::SAFEGUARD, .. }]
                    );
                    assert_eq!(
                        announced_field,
                        field_first,
                        "{} trick={trick_room} {id} {terrain:?}: {:?}",
                        preset.id,
                        b.events()
                    );
                }
            }
        }
    }

    #[test]
    fn underground_weather_immunity_still_sorts_handlers() {
        use crate::{PokemonSet, Team};
        use pikapro_data::gen9::{abilities as a, species as s};

        for move_id in [ids::DIG, ids::DIVE] {
            let set = || PokemonSet::new(s::PIKACHU).with_ability(a::NO_ABILITY).with_moves(&[move_id]);
            let teams = [Team::new(vec![set()]), Team::new(vec![set()])];
            let mut b = Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(44_298_482))).unwrap();
            let user = Position::new(SideId::P1, 0);
            let mon = b.state.at_mut(user).unwrap();
            mon.volatiles.add(Volatiles::MAGNET_RISE);
            mon.volatiles.add(Volatiles::CHARGING);
            mon.volatile_counters.charge_move = move_id;
            let mut expected = b.rng.clone();
            expected.random(2);
            // Pokemon.runStatusImmunity sorts both onImmunity handlers
            // before Dig/Dive returns false, even though Magnet Rise
            // only answers Ground immunity.
            assert!(weather_immune(&mut b, user, Weather::Sand));
            assert_eq!(b.rng, expected, "{move_id:?}");
        }
    }

    #[test]
    fn weather_lasts_five_turns_and_fails_when_repeated() {
        let mut b = battle(Format::gen9_singles());
        let user = Position::new(SideId::P1, 0);
        assert_eq!(set_weather(&mut b, "RainDance", user), Outcome::Succeeded);
        assert_eq!(set_weather(&mut b, "raindance", user), Outcome::Failed, "same weather fails");
        for turn in 1..=5 {
            run_residual(&mut b);
            assert_eq!(b.state.field.weather.is_some(), turn < 5, "turn {turn}");
        }
        assert!(b.events().contains(&BattleEvent::Weather { weather: None, upkeep: false }));
    }

    #[test]
    fn side_conditions_and_trick_room() {
        let mut b = battle(Format::gen9_doubles());
        let user = Position::new(SideId::P2, 0);
        assert!(add_side_condition(&mut b, SideId::P2, "tailwind", user));
        assert!(add_side_condition(&mut b, SideId::P2, "reflect", user));
        assert!(!add_side_condition(&mut b, SideId::P2, "reflect", user));
        let c = b.state.side(SideId::P2).conditions;
        assert_eq!((c.tailwind, c.reflect), (4, 5));
        assert_eq!(&c.order[..], [SideCondition::Tailwind, SideCondition::Reflect]);
        assert!(add_pseudo_weather(&mut b, "trickroom", user));
        assert_eq!(b.state.field.trick_room_turns, 5);
        // Using Trick Room again ends it, and counts as success.
        assert!(add_pseudo_weather(&mut b, "trickroom", user));
        assert_eq!(b.state.field.trick_room_turns, 0);
        for _ in 0..4 {
            run_residual(&mut b);
        }
        let c = b.state.side(SideId::P2).conditions;
        assert_eq!((c.tailwind, c.reflect), (0, 1));
        assert_eq!(&c.order[..], [SideCondition::Reflect]);
    }

    #[test]
    fn spikes_stack_to_three_layers() {
        let mut b = battle(Format::gen9_singles());
        let user = Position::new(SideId::P1, 0);
        for layers in 1..=3 {
            assert!(add_side_condition(&mut b, SideId::P2, "spikes", user));
            assert_eq!(b.state.side(SideId::P2).conditions.spikes, layers);
        }
        assert!(!add_side_condition(&mut b, SideId::P2, "spikes", user));
        assert!(add_side_condition(&mut b, SideId::P2, "toxicspikes", user));
        assert!(add_side_condition(&mut b, SideId::P2, "toxicspikes", user));
        assert!(!add_side_condition(&mut b, SideId::P2, "toxicspikes", user));
        assert!(add_side_condition(&mut b, SideId::P2, "stealthrock", user));
        assert!(!add_side_condition(&mut b, SideId::P2, "stealthrock", user));
        // Hazards don't wear off.
        run_residual(&mut b);
        assert_eq!(b.state.side(SideId::P2).conditions.order.len(), 3);
    }

    #[test]
    fn court_change_swaps_conditions_but_not_guards() {
        let mut b = battle(Format::gen9_doubles());
        let (p1, p2) = (Position::new(SideId::P1, 0), Position::new(SideId::P2, 0));
        add_side_condition(&mut b, SideId::P1, "wideguard", p1);
        add_side_condition(&mut b, SideId::P1, "reflect", p1);
        add_side_condition(&mut b, SideId::P2, "spikes", p2);
        add_side_condition(&mut b, SideId::P2, "tailwind", p2);
        assert!(court_change(&mut b, p1));
        let (a, c) = (b.state.side(SideId::P1).conditions, b.state.side(SideId::P2).conditions);
        assert_eq!(&a.order[..], [SideCondition::WideGuard, SideCondition::Spikes, SideCondition::Tailwind]);
        assert_eq!((a.spikes, a.tailwind, a.reflect, a.wide_guard), (1, 4, 0, 1));
        assert_eq!(&c.order[..], [SideCondition::Reflect]);
        assert_eq!(c.reflect, 5);
    }

    #[test]
    fn gravity_grounds_and_boosts_accuracy() {
        use crate::resolve::events::{Effect, run_modify};
        let mut b = battle(Format::gen9_singles());
        let user = Position::new(SideId::P1, 0);
        let target = Position::new(SideId::P2, 0);
        let acc = |b: &mut Battle, a| {
            run_modify(b, pikapro_data::Event::ModifyAccuracy, target, Some(user), Effect::None, a, None, None)
        };
        assert_eq!(acc(&mut b, 70), 70);
        assert!(add_pseudo_weather(&mut b, "gravity", user));
        assert!(!add_pseudo_weather(&mut b, "gravity", user), "Gravity can't be restarted");
        assert_eq!(acc(&mut b, 70), 117);
        assert_eq!(acc(&mut b, 90), 150);
        assert!(is_grounded(&b, user));
    }

    #[test]
    fn screens_halve_damage_or_cut_it_to_two_thirds_in_doubles() {
        use crate::resolve::events::{Effect, run_modify};
        use pikapro_data::Event;
        for (format, expected) in [(Format::gen9_singles(), 50), (Format::gen9_doubles(), 67)] {
            let physical = *format.dex.move_(ids::EARTHQUAKE);
            let mut b = battle(format);
            let (user, target) = (Position::new(SideId::P1, 0), Position::new(SideId::P2, 0));
            add_side_condition(&mut b, SideId::P2, "reflect", target);
            let effect = Effect::Move(ids::EARTHQUAKE);
            let d = run_modify(&mut b, Event::ModifyDamage, user, Some(target), effect, 100, Some(&physical), None);
            assert_eq!(d, expected);
            b.move_state.hit_data_mut(target).crit = true;
            let d = run_modify(&mut b, Event::ModifyDamage, user, Some(target), effect, 100, Some(&physical), None);
            assert_eq!(d, 100, "crits bypass screens");
        }
    }
}
