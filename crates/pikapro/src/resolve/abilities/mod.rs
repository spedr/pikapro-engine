//! Abilities: dispatch to the module that implements each one, and the
//! helpers every ability needs.
//!
//! # Adding an ability
//!
//! 1. Read the ability in Showdown's `data/abilities.ts` (and
//!    `data/mods/champions/abilities.ts`). Its callbacks and their
//!    `on<Callback>Order` / `Priority` / `SubOrder` are already in the
//!    generated data (`AbilityData::handlers`), so the engine already
//!    collects and sorts its handlers at every hook; only the behaviour is
//!    missing.
//! 2. Assign it a module in [`owner`] (`switchin.rs`: switch-in, field,
//!    forme and ability-suppressing abilities, with the forme changers in
//!    `switchin/forme_abilities.rs` and the stat and power modifiers from
//!    the field in `switchin/modifier_abilities.rs`; `damage.rs`: damage, stat
//!    and type modifiers; `reactive.rs`: immunities and reactive effects;
//!    `misc.rs`: the rest; `items/consumed.rs`: abilities about items) and
//!    add one arm per callback to that module's `run` function:
//!    `(abilities::X, Event::BasePower, On) => ...`.
//!    The arm gets the running event ([`EventCtx`]: relay, source, effect,
//!    move data, `chain_modify`) and the holder's position
//!    (`effectState.target`), and returns what the callback returns
//!    (`None` for `undefined`, `Some(Relay::False)` to stop, a number to
//!    replace the relay). An `onStart` also runs on switching in: Showdown
//!    turns it into the `SwitchIn` handler (`getCallback`), and the
//!    dispatcher passes `Event::Start`.
//! 3. Use the helpers here and in `items` (`has_ability`, `show`,
//!    `set_ability`, `has_item`, `use_item`, ...), `events::run_event` for
//!    the events the callback runs itself, and the `BattleEvent` variants
//!    for messages (`Cause` for `[from]` / `[of]`). Per-ability state goes
//!    in `AbilityState` (renewed on switching in and by `set_ability`) or
//!    `PokemonState` (the whole battle); a volatile (Flash Fire, Slow
//!    Start) takes a bit of the module's range (`events::volatile_owner`)
//!    and its handlers go in the module's `volatile_spec`.
//! 4. Add it to `IMPLEMENTED_ABILITIES` in `pikapro-conformance`, fuzz,
//!    record fixtures.

pub(crate) mod damage;
pub(crate) mod misc;
pub(crate) mod reactive;
pub(crate) mod switchin;

use pikapro_data::gen9::{abilities as a, items as i};
use pikapro_data::{AbilityFlags, AbilityId, BoostId, Event, Stat};

use super::events::{self, Effect, EventCtx, Relay, Target};
use super::{actor, items};
use crate::battle::Battle;
use crate::event::{AbilityNote, BattleEvent, Cause, EffectName};
use crate::state::{Position, SideId, Volatiles, Weather};

/// Which module implements an ability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// [`switchin`].
    Switchin,
    /// [`damage`].
    Damage,
    /// [`reactive`].
    Reactive,
    /// [`misc`].
    Misc,
    /// [`items::consumed`].
    Consumed,
    /// No Ability.
    None,
}

/// The module that implements `id`.
pub(crate) fn owner(id: AbilityId) -> Owner {
    match id {
        // Switch-in, field, forme and ability-suppressing abilities.
        a::AIR_LOCK
        | a::ANTICIPATION
        | a::AS_ONE_GLASTRIER
        | a::AS_ONE_SPECTRIER
        | a::AURA_BREAK
        | a::BATTLE_BOND
        | a::BEADS_OF_RUIN
        | a::CLOUD_NINE
        | a::COMMANDER
        | a::COSTAR
        | a::CURIOUS_MEDICINE
        | a::DARK_AURA
        | a::DAUNTLESS_SHIELD
        | a::DELTA_STREAM
        | a::DESOLATE_LAND
        | a::DISGUISE
        | a::DOWNLOAD
        | a::DRIZZLE
        | a::DROUGHT
        | a::ELECTRIC_SURGE
        | a::EMBODY_ASPECT_CORNERSTONE
        | a::EMBODY_ASPECT_HEARTHFLAME
        | a::EMBODY_ASPECT_TEAL
        | a::EMBODY_ASPECT_WELLSPRING
        | a::FAIRY_AURA
        | a::FLOWER_GIFT
        | a::FORECAST
        | a::FOREWARN
        | a::FRISK
        | a::GRASSY_SURGE
        | a::GULP_MISSILE
        | a::HADRON_ENGINE
        | a::HOSPITALITY
        | a::HUNGER_SWITCH
        | a::ICE_FACE
        | a::ILLUSION
        | a::IMPOSTER
        | a::INTIMIDATE
        | a::INTREPID_SWORD
        | a::KLUTZ
        | a::MIMICRY
        | a::MISTY_SURGE
        | a::MOLD_BREAKER
        | a::MULTITYPE
        | a::NATURAL_CURE
        | a::NEUTRALIZING_GAS
        | a::ORICHALCUM_PULSE
        | a::POWER_CONSTRUCT
        | a::PRESSURE
        | a::PRIMORDIAL_SEA
        | a::PROTOSYNTHESIS
        | a::PSYCHIC_SURGE
        | a::QUARK_DRIVE
        | a::RKS_SYSTEM
        | a::REGENERATOR
        | a::SAND_STREAM
        | a::SCHOOLING
        | a::SCREEN_CLEANER
        | a::SHIELDS_DOWN
        | a::SLOW_START
        | a::SNOW_WARNING
        | a::STANCE_CHANGE
        | a::SUPERSWEET_SYRUP
        | a::SUPREME_OVERLORD
        | a::SWORD_OF_RUIN
        | a::TABLETS_OF_RUIN
        | a::TERA_SHELL
        | a::TERA_SHIFT
        | a::TERAFORM_ZERO
        | a::TERAVOLT
        | a::TRACE
        | a::TRUANT
        | a::TURBOBLAZE
        | a::UNNERVE
        | a::VESSEL_OF_RUIN
        | a::ZEN_MODE
        | a::ZERO_TO_HERO => Owner::Switchin,
        // Damage, stat and type modifiers.
        a::ADAPTABILITY
        | a::AERILATE
        | a::ANALYTIC
        // Lucario-Mega-Z's (Champions Reg M-C).
        | a::AURA_GUARD
        | a::BATTERY
        | a::BATTLE_ARMOR
        | a::BLAZE
        | a::CHLOROPHYLL
        | a::CORROSION
        | a::DEFEATIST
        | a::DRAGONS_MAW
        | a::DRAGONIZE
        | a::EELEVATE
        | a::FILTER
        | a::FIRE_MANE
        | a::FLARE_BOOST
        | a::FLUFFY
        | a::FRIEND_GUARD
        | a::FUR_COAT
        | a::GALVANIZE
        | a::GORILLA_TACTICS
        | a::GRASS_PELT
        | a::GUTS
        | a::HEATPROOF
        | a::HEAVY_METAL
        | a::HUGE_POWER
        | a::HUSTLE
        | a::ICE_SCALES
        | a::INFILTRATOR
        | a::IRON_FIST
        | a::LIGHT_METAL
        | a::LIQUID_VOICE
        | a::LONG_REACH
        | a::MARVEL_SCALE
        | a::MEGA_LAUNCHER
        | a::MEGA_SOL
        | a::MERCILESS
        | a::MINDS_EYE
        | a::MINUS
        | a::MULTISCALE
        | a::NEUROFORCE
        | a::NORMALIZE
        | a::OVERGROW
        | a::PARENTAL_BOND
        | a::PIERCING_DRILL
        | a::PIXILATE
        | a::PLUS
        | a::POWER_SPOT
        | a::PRISM_ARMOR
        | a::PUNK_ROCK
        | a::PURE_POWER
        | a::QUICK_FEET
        | a::RECKLESS
        | a::REFRIGERATE
        | a::RIVALRY
        | a::ROCKY_PAYLOAD
        | a::SAND_FORCE
        | a::SAND_RUSH
        | a::SCRAPPY
        | a::SERENE_GRACE
        | a::SHADOW_SHIELD
        | a::SHARPNESS
        | a::SHEER_FORCE
        | a::SHELL_ARMOR
        | a::SKILL_LINK
        | a::SLUSH_RUSH
        | a::SNIPER
        | a::SOLAR_POWER
        | a::SOLID_ROCK
        | a::STAKEOUT
        | a::STEELWORKER
        | a::STEELY_SPIRIT
        | a::STENCH
        | a::STRONG_JAW
        | a::SUPER_LUCK
        | a::SURGE_SURFER
        | a::SWARM
        | a::SWIFT_SWIM
        | a::TECHNICIAN
        | a::THICK_FAT
        | a::TINTED_LENS
        | a::TORRENT
        | a::TOUGH_CLAWS
        | a::TOXIC_BOOST
        | a::TRANSISTOR
        | a::UNAWARE
        | a::UNSEEN_FIST
        | a::WATER_BUBBLE => Owner::Damage,
        // Immunities, contact and reactive effects, status and boost protection.
        a::AFTERMATH
        | a::ANGER_POINT
        | a::ANGER_SHELL
        | a::ARMOR_TAIL
        | a::AROMA_VEIL
        | a::BERSERK
        | a::BIG_PECKS
        | a::BULLETPROOF
        | a::CLEAR_BODY
        | a::COLOR_CHANGE
        | a::COMATOSE
        | a::COMPETITIVE
        | a::CONTRARY
        | a::COTTON_DOWN
        | a::CURSED_BODY
        | a::CUTE_CHARM
        | a::DAMP
        | a::DAZZLING
        | a::DEFIANT
        | a::DRY_SKIN
        | a::EARTH_EATER
        | a::EFFECT_SPORE
        | a::ELECTROMORPHOSIS
        | a::FLAME_BODY
        | a::FLASH_FIRE
        | a::FLOWER_VEIL
        | a::FULL_METAL_BODY
        | a::GOOD_AS_GOLD
        | a::GOOEY
        | a::GUARD_DOG
        | a::HYPER_CUTTER
        | a::IMMUNITY
        | a::INNARDS_OUT
        | a::INNER_FOCUS
        | a::INSOMNIA
        | a::IRON_BARBS
        | a::JUSTIFIED
        | a::KEEN_EYE
        | a::LEAF_GUARD
        | a::LEVITATE
        | a::LIGHTNING_ROD
        | a::LIMBER
        | a::LINGERING_AROMA
        | a::MAGIC_BOUNCE
        | a::MAGMA_ARMOR
        | a::MIRROR_ARMOR
        | a::MOTOR_DRIVE
        | a::MUMMY
        | a::OBLIVIOUS
        | a::OVERCOAT
        | a::OWN_TEMPO
        | a::PASTEL_VEIL
        | a::PERISH_BODY
        | a::POISON_POINT
        | a::POISON_PUPPETEER
        | a::POISON_TOUCH
        | a::PURIFYING_SALT
        | a::QUEENLY_MAJESTY
        | a::RATTLED
        | a::ROUGH_SKIN
        | a::SAND_SPIT
        | a::SAP_SIPPER
        | a::SEED_SOWER
        | a::SHIELD_DUST
        | a::SIMPLE
        | a::SOUNDPROOF
        | a::SPICY_SPRAY
        | a::STAMINA
        | a::STATIC
        | a::STEADFAST
        | a::STEAM_ENGINE
        | a::STORM_DRAIN
        | a::STURDY
        | a::SUCTION_CUPS
        | a::SWEET_VEIL
        | a::SYNCHRONIZE
        | a::TANGLING_HAIR
        | a::TELEPATHY
        | a::THERMAL_EXCHANGE
        | a::TOXIC_CHAIN
        | a::TOXIC_DEBRIS
        | a::VITAL_SPIRIT
        | a::VOLT_ABSORB
        | a::WANDERING_SPIRIT
        | a::WATER_ABSORB
        | a::WATER_COMPACTION
        | a::WATER_VEIL
        | a::WEAK_ARMOR
        | a::WELL_BAKED_BODY
        | a::WHITE_SMOKE
        | a::WIND_POWER
        | a::WIND_RIDER
        | a::WONDER_GUARD => Owner::Reactive,
        // Residual, priority, accuracy, KO and miscellaneous.
        a::ARENA_TRAP
        | a::BAD_DREAMS
        | a::BEAST_BOOST
        | a::CHILLING_NEIGH
        | a::COMPOUND_EYES
        | a::DANCER
        | a::EARLY_BIRD
        | a::EMERGENCY_EXIT
        | a::GALE_WINGS
        | a::GRIM_NEIGH
        | a::HEALER
        | a::HONEY_GATHER
        | a::HYDRATION
        | a::ICE_BODY
        | a::ILLUMINATE
        | a::LIBERO
        | a::LIQUID_OOZE
        | a::MAGIC_GUARD
        | a::MAGNET_PULL
        | a::MOODY
        | a::MOXIE
        | a::MYCELIUM_MIGHT
        | a::NO_GUARD
        | a::OPPORTUNIST
        | a::POISON_HEAL
        | a::POWER_OF_ALCHEMY
        | a::PRANKSTER
        | a::PROPELLER_TAIL
        | a::PROTEAN
        | a::QUICK_DRAW
        | a::RAIN_DISH
        | a::RECEIVER
        | a::ROCK_HEAD
        | a::RUN_AWAY
        | a::SAND_VEIL
        | a::SHADOW_TAG
        | a::SHED_SKIN
        | a::SNOW_CLOAK
        | a::SOUL_HEART
        | a::SPEED_BOOST
        | a::STALL
        | a::STALWART
        | a::TANGLED_FEET
        | a::TRIAGE
        | a::VICTORY_STAR
        | a::WIMP_OUT
        | a::WONDER_SKIN => Owner::Misc,
        // Berries, consumed items, item and ability transfer.
        a::BALL_FETCH
        | a::CHEEK_POUCH
        | a::CUD_CHEW
        | a::GLUTTONY
        | a::HARVEST
        | a::MAGICIAN
        | a::PICKPOCKET
        | a::PICKUP
        | a::RIPEN
        | a::STICKY_HOLD
        | a::SYMBIOSIS
        | a::UNBURDEN => Owner::Consumed,
        _ => Owner::None,
    }
}

/// Runs `ability`'s callback for `ev.event` / `ev.prefix`. `holder` is the
/// Pokémon with the ability (Showdown's `effectState.target`). Returns the
/// callback's result (`None`: `undefined`).
pub(crate) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, ability: AbilityId) -> Option<Relay> {
    match owner(ability) {
        Owner::Switchin => switchin::run(battle, ev, holder, ability),
        Owner::Damage => damage::run(battle, ev, holder, ability),
        Owner::Reactive => reactive::run(battle, ev, holder, ability),
        Owner::Misc => misc::run(battle, ev, holder, ability),
        Owner::Consumed => items::consumed::run_ability(battle, ev, holder, ability),
        Owner::None => None,
    }
}

/// Showdown's `pokemon.getBestStat(unboosted, true)`: the highest stat
/// other than HP, without modifiers (Wonder Room swaps the defences) and
/// with its stage unless `unboosted`. The first one wins ties.
pub(crate) fn best_stat(battle: &mut Battle, pos: Position, unboosted: bool) -> BoostId {
    const STATS: [(Stat, BoostId); 5] = [
        (Stat::Atk, BoostId::Atk),
        (Stat::Def, BoostId::Def),
        (Stat::SpA, BoostId::SpA),
        (Stat::SpD, BoostId::SpD),
        (Stat::Spe, BoostId::Spe),
    ];
    let mut best = (BoostId::Atk, 0);
    for (stat, id) in STATS {
        let value = events::get_stat(battle, pos, stat, unboosted, true);
        if value > best.1 {
            best = (id, value);
        }
    }
    best.0
}

// ---------------------------------------------------------------------------
// Suppression.

/// Showdown's `pokemon.ignoringAbility()`: the ability of the Pokémon at
/// `pos` does nothing (it is not active, Gastro Acid, Neutralizing Gas, a
/// transformed Pokémon's `notransform` ability). `cantsuppress` abilities
/// and Ability Shield keep it.
pub(crate) fn ignoring_ability(battle: &Battle, pos: Position) -> bool {
    let side = battle.state.side(pos.side);
    let Some(mon) = side.team.get(pos.slot as usize) else { return true };
    // Gen 5+: `!this.isActive` (also a Pokémon revived in its slot by
    // Revival Blessing until it switches back in: its Regenerator doesn't
    // heal it as its `instaswitch` calls it back).
    if !events::is_active(battle, pos) || super::slots::off_field(battle, pos) {
        return true;
    }
    // Without Gastro Acid or Transform, only a working Neutralizing Gas
    // can suppress it (every test below then gives false).
    if !mon.volatiles.has(Volatiles::GASTRO_ACID | Volatiles::TRANSFORMED) && !neutralizing_gas(battle) {
        return false;
    }
    let flags = battle.format.dex.ability(mon.ability).flags;
    if flags.contains(AbilityFlags::NO_TRANSFORM) && mon.volatiles.has(Volatiles::TRANSFORMED) {
        return true;
    }
    if flags.contains(AbilityFlags::CANT_SUPPRESS) {
        return false;
    }
    if mon.volatiles.has(Volatiles::GASTRO_ACID) {
        return true;
    }
    if mon.ability == a::NEUTRALIZING_GAS || items::has_item(battle, pos, i::ABILITY_SHIELD) {
        return false;
    }
    !mon.volatiles.has(Volatiles::COMMANDING) && neutralizing_gas(battle)
}

/// Whether an active Pokémon's Neutralizing Gas is working (not itself
/// suppressed by Gastro Acid, transformed, or ending).
fn neutralizing_gas(battle: &Battle) -> bool {
    any_active(battle, |m, _| {
        m.ability == a::NEUTRALIZING_GAS
            && !m.volatiles.has(Volatiles::GASTRO_ACID | Volatiles::TRANSFORMED)
            && !m.volatile_counters.ability_state.ending
    })
}

/// `all_active(battle).any(...)` on each active Pokémon and its position,
/// as a plain loop (the iterator's `flat_map` doesn't compile to one).
#[inline(always)]
fn any_active(battle: &Battle, mut test: impl FnMut(&crate::state::PokemonState, Position) -> bool) -> bool {
    for side in SideId::BOTH {
        let s = battle.state.side(side);
        for slot in 0..battle.format.active_per_side() {
            if let Some(m) = s.active(slot)
                && test(m, Position::new(side, slot as u8))
            {
                return true;
            }
        }
    }
    false
}

/// The active Pokémon whose faint has not been reported (Showdown's
/// `getAllActive()`), P1 then P2, left to right.
pub(crate) fn all_active(battle: &Battle) -> impl Iterator<Item = Position> + '_ {
    SideId::BOTH.into_iter().flat_map(move |side| {
        (0..battle.format.active_per_side() as u8)
            .map(move |slot| Position::new(side, slot))
            .filter(move |&p| events::is_active(battle, p))
    })
}

/// Showdown's `battle.checkMoveMakesContact(move, attacker, defender,
/// announcePads)`: the move (its data after `ModifyMove`: Long Reach,
/// Punching Glove) makes contact, unless the attacker holds Protective
/// Pads (with `announce`, `-activate` for the defender's effect, the one
/// running now, then for the pads).
pub(crate) fn makes_contact(
    battle: &mut Battle,
    data: &pikapro_data::MoveData,
    attacker: Position,
    defender: Position,
    announce: Option<crate::event::EffectName>,
) -> bool {
    if !data.flags.contains(pikapro_data::MoveFlags::CONTACT) {
        return false;
    }
    if super::items::has_item(battle, attacker, pikapro_data::gen9::items::PROTECTIVE_PADS) {
        if let Some(effect) = announce {
            let d = actor(battle, defender);
            battle.emit(BattleEvent::ActivateEffect { actor: d, effect, args: [None, None], of: None });
            let a = actor(battle, attacker);
            let pads = crate::event::EffectName::Item(pikapro_data::gen9::items::PROTECTIVE_PADS);
            battle.emit(BattleEvent::ActivateEffect { actor: a, effect: pads, args: [None, None], of: None });
        }
        return false;
    }
    true
}

/// Showdown's `pokemon.hasAbility(id)`: it has the ability and it is not
/// suppressed.
#[inline]
pub(crate) fn has_ability(battle: &Battle, pos: Position, id: AbilityId) -> bool {
    battle.state.side(pos.side).team.get(pos.slot as usize).is_some_and(|m| m.ability == id)
        && !ignoring_ability(battle, pos)
}

/// The ability of the Pokémon at `pos`, unless it is suppressed.
pub(crate) fn effective_ability(battle: &Battle, pos: Position) -> Option<AbilityId> {
    let mon = battle.state.side(pos.side).team.get(pos.slot as usize)?;
    (!ignoring_ability(battle, pos)).then_some(mon.ability)
}

/// Showdown's `battle.suppressingAbility(target)`: the move being used
/// ignores the target's ability (Mold Breaker's `move.ignoreAbility`),
/// unless the target is its user or holds Ability Shield.
pub(crate) fn suppressing_ability(battle: &Battle, target: Position) -> bool {
    let Some((user, _)) = battle.active_move else { return false };
    let ignores = battle.order.called_move.map_or(battle.move_state.ignore_ability, |m| m.ignore_ability);
    ignores && events::is_active(battle, user) && user != target && !items::has_item(battle, target, i::ABILITY_SHIELD)
}

/// Showdown's `field.suppressingWeather()`: an active Pokémon's Air Lock or
/// Cloud Nine is working.
pub(crate) fn suppressing_weather(battle: &Battle) -> bool {
    let dex = battle.format.dex;
    any_active(battle, |m, p| {
        dex.ability(m.ability).suppress_weather
            && !m.volatile_counters.ability_state.ending
            && !ignoring_ability(battle, p)
    })
}

/// Showdown's `field.effectiveWeather()`: the weather, unless Air Lock or
/// Cloud Nine suppresses it.
pub(crate) fn effective_weather(battle: &Battle) -> Option<Weather> {
    let weather = battle.state.field.weather?;
    (!suppressing_weather(battle)).then_some(weather)
}

/// Showdown's `pokemon.effectiveWeather()`: the effective weather, where
/// Utility Umbrella hides sun and rain from its holder.
pub(crate) fn pokemon_weather(battle: &Battle, pos: Position) -> Option<Weather> {
    let weather = effective_weather(battle)?;
    let umbrella = crate::typing::umbrella_hides(weather) && items::has_item(battle, pos, i::UTILITY_UMBRELLA);
    (!umbrella).then_some(weather)
}

// ---------------------------------------------------------------------------
// Lifecycle.

/// A new `abilityState` for the Pokémon at `pos` (switching in,
/// `setAbility`): the next `effectOrder` and fresh ability data.
pub(crate) fn new_ability_state(battle: &mut Battle, pos: Position) {
    super::order::new_ability_state(battle, pos);
    if let Some(mon) = battle.state.side_mut(pos.side).team.get_mut(pos.slot as usize) {
        mon.volatile_counters.ability_state = Default::default();
    }
}

/// How [`set_ability`] was called (Showdown's `isFromFormeChange` /
/// `isTransform`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SetAbilityKind {
    /// Skill Swap, Role Play, Entrainment, Trace, Mummy, ...: `SetAbility`
    /// runs and the change is announced.
    #[default]
    Normal,
    /// A forme change (`isFromFormeChange`): no checks, no message.
    FormeChange,
    /// Transform (`isTransform`): no `SetAbility`, no message, no `Start`
    /// if the ability stays the same.
    Transform,
}

/// The [`Effect`] an [`EffectName`] names.
pub(crate) fn effect_of(name: EffectName) -> Effect {
    match name {
        EffectName::Ability(x) => Effect::Ability(x),
        EffectName::Item(x) => Effect::Item(x),
        EffectName::Move(x) => Effect::Move(x),
    }
}

/// Showdown's `pokemon.setAbility(ability, source, sourceEffect)`.
/// `effect` is the cause of the change (the move or ability), announced
/// as `-ability|<pokemon>|<new>|<old>|[from] <effect>|[of] <source>` (Mummy
/// and Lingering Aroma: `-activate`). Returns the old ability, or `None` if
/// the change failed (`cantsuppress`, a `SetAbility` handler, no HP).
pub(crate) fn set_ability(
    battle: &mut Battle,
    pos: Position,
    ability: AbilityId,
    source: Option<Position>,
    effect: Option<EffectName>,
    kind: SetAbilityKind,
) -> Option<AbilityId> {
    let mon = battle.state.side(pos.side).team.get(pos.slot as usize)?;
    if mon.hp == 0 {
        return None;
    }
    let old = mon.ability;
    let dex = battle.format.dex;
    // `isFromFormeChange` (a forme change; Transform passes it too) skips
    // the `cantsuppress` check.
    if kind == SetAbilityKind::Normal
        && (dex.ability(ability).flags.contains(AbilityFlags::CANT_SUPPRESS)
            || dex.ability(old).flags.contains(AbilityFlags::CANT_SUPPRESS))
    {
        return None;
    }
    let source_effect = effect.map_or(Effect::None, effect_of);
    if kind == SetAbilityKind::Normal {
        let mut ev =
            EventCtx::new(Event::SetAbility, Target::Pokemon(pos), source, source_effect, Relay::Ability(ability));
        if !events::run_event(battle, &mut ev, None).is_truthy() {
            return None;
        }
    }
    events::single_event_ability(battle, Event::End, pos, old, source, Effect::None, Relay::True);
    battle.state.side_mut(pos.side).team[pos.slot as usize].ability = ability;
    super::events::note_held(battle, Some(ability), None);
    new_ability_state(battle, pos);
    if let (Some(effect), SetAbilityKind::Normal) = (effect, kind) {
        let target = actor(battle, pos);
        let source_actor = source.map(|s| actor(battle, s));
        match effect {
            // `-activate|<source>|ability: Mummy|<target>|[ability] <old>`.
            EffectName::Ability(holder_ability @ (a::MUMMY | a::LINGERING_AROMA)) => {
                let holder = source_actor.unwrap_or(target);
                battle.emit(BattleEvent::AbilityReplaced { holder, ability: holder_ability, target, old });
            }
            _ => {
                let cause = Cause { effect, of: source_actor };
                battle.emit(BattleEvent::AbilityShow {
                    actor: target,
                    ability,
                    note: AbilityNote::Replaced(old),
                    cause: Some(cause),
                });
            }
        }
    }
    if kind != SetAbilityKind::Transform || old != ability {
        events::single_event_ability(battle, Event::Start, pos, ability, source, Effect::None, Relay::True);
    }
    Some(old)
}

/// `-ability|<pokemon>|<ability>`: the Pokémon's ability announces itself
/// (Pressure, Mold Breaker, Unnerve, ...).
pub(crate) fn show(battle: &mut Battle, pos: Position, ability: AbilityId) {
    let actor = actor(battle, pos);
    battle.emit(BattleEvent::AbilityShow { actor, ability, note: AbilityNote::None, cause: None });
}

/// Showdown's `pokemon.foes()` (`adjacentFoes()` in doubles): the foes on
/// the field with HP left, left to right.
pub(crate) fn foes(battle: &Battle, pos: Position) -> crate::util::StackVec<Position, 2> {
    let mut out = crate::util::StackVec::new();
    let side = pos.side.foe();
    for slot in 0..battle.format.active_per_side() {
        let p = Position::new(side, slot as u8);
        if events::is_active(battle, p) && battle.state.side(side).team[slot].hp > 0 {
            out.push(p);
        }
    }
    out
}

/// `-activate|<pokemon>|ability: <ability>`.
pub(crate) fn activate(battle: &mut Battle, pos: Position, ability: AbilityId) {
    let actor = actor(battle, pos);
    let effect = EffectName::Ability(ability);
    battle.emit(BattleEvent::ActivateEffect { actor, effect, args: [None, None], of: None });
}

/// `effect?.status`: the effect inflicts a status of its own (a move with a
/// `status`, Synchronize passing one on as `{status, id: 'synchronize'}`),
/// so the abilities that refuse it announce it.
pub(crate) fn effect_has_status(battle: &Battle, effect: Effect) -> bool {
    match effect {
        // Psycho Shift's onTryHit sets the ActiveMove's status before
        // SetStatus; its static dex entry has no status.
        Effect::Move(m) => {
            m == pikapro_data::gen9::moves::PSYCHO_SHIFT
                || battle.format.dex.moves.get(m.0 as usize).is_some_and(|d| d.status.is_some())
        }
        Effect::Condition(events::Condition::Synchronize) => true,
        _ => false,
    }
}
