//! Damage, stat and type modifiers.
//!
//! Showdown references: the abilities in `data/abilities.ts` that
//! `abilities::owner` assigns to [`Owner::Damage`](super::Owner::Damage)
//! (Technician, Huge Power, Multiscale, the
//! type-changing abilities, Sheer Force, Parental Bond, the weather speed
//! abilities, ...), and the `champions` mod's overrides
//! (`data/mods/champions/abilities.ts`: Unseen Fist).
//!
//! Adding an ability: see `abilities/mod.rs`. One arm per callback in
//! [`run`].
//!
//! Several of these only exist on Mega Evolutions (Parental Bond,
//! Aerilate, Dragonize, Eelevate, Fire Mane, Piercing Drill, the Champions
//! mod's Unseen Fist); the conformance pool reaches them through Mega
//! Evolution.
//!
//! Mega Sol (Meganium-Mega) is in `resolve/mega.rs`: its
//! `onWeatherModifyDamage` goes with `pokemon.effectiveWeather()` returning
//! sun for any effect of a move or weather while its holder is the active
//! Pokémon (Growth, Weather Ball, Solar Beam, the rain's own damage
//! modifier, ...).
//!
//! Handler arguments follow Showdown's `runEvent` convention: an `On`
//! handler's event target is its holder; for `ModifyAtk` / `ModifySpA` /
//! `BasePower` / `ModifyDamage` / `ModifyCritRatio` / `ModifySTAB` the
//! event runs on the attacker with the defender as its source
//! (`ev.source`), so an attacker's ability reads the defender there and a
//! defender's `onSource...` handler is its own holder.

use pikapro_data::gen9::{abilities as a, moves as mv};
use pikapro_data::{AbilityId, BoostId, Event, EventPrefix, MoveCategory, MoveData, MoveFlags, MoveId, StatusId, Type};

use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, LineArg};
use crate::resolve::events::{self, Effect, EventCtx, Relay};
use crate::resolve::status::poisoned;
use crate::resolve::{actor, boosts, damage_by, manipulation, queue, status};
use crate::state::{PokemonState, Position, Status, Terrain, Weather};
use crate::team::Gender;

/// Runs `ability`'s callback for `ev.event` / `ev.prefix` on its holder.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, ability: AbilityId) -> Option<Relay> {
    use EventPrefix::{Ally, Any, On, Source};
    match (ability, ev.event, ev.prefix) {
        // ---------------------------------------------------------------
        // `BasePower` (the attacker's; `ev.source` is the defender).

        // `onBasePower` (priority 30): a move at 60 base power or less
        // (after the modifiers so far) gets 1.5x.
        (a::TECHNICIAN, Event::BasePower, On) => {
            let base_power = crate::stats::modify(ev.value().max(0) as u32, ev.modifier);
            if base_power <= 60 { ev.chain_modify(3, 2) } else { None }
        }
        // The -ate abilities and Normalize (priority 23): the move whose
        // type they changed (`move.typeChangerBoosted === this.effect`)
        // gets 4915/4096.
        (
            a::AERILATE | a::DRAGONIZE | a::GALVANIZE | a::NORMALIZE | a::PIXILATE | a::REFRIGERATE,
            Event::BasePower,
            On,
        ) => {
            if battle.move_state.type_changer == Some(ability) {
                ev.chain(4915);
            }
            None
        }
        // Iron Fist (23): punching moves, 4915/4096.
        (a::IRON_FIST, Event::BasePower, On) => flag_boost(ev, MoveFlags::PUNCH, 4915),
        // Reckless (23): moves with recoil or crash damage, 4915/4096.
        (a::RECKLESS, Event::BasePower, On) => {
            let data = ev.move_data()?;
            if data.recoil.is_some() || data.unmodelled.contains(&"hasCrashDamage") {
                ev.chain(4915);
            }
            None
        }
        // Rivalry (24): 1.25x against the same gender, 0.75x against the
        // other (neither genderless).
        (a::RIVALRY, Event::BasePower, On) => {
            let defender = ev.source?;
            match (gender(battle, holder), gender(battle, defender)) {
                (Some(x), Some(y)) if x == y => ev.chain_modify(5, 4),
                (Some(_), Some(_)) => ev.chain_modify(3, 4),
                _ => None,
            }
        }
        // Analytic (21): 5325/4096 when no other active Pokémon still has
        // a move to use this turn (`queue.willMove`).
        (a::ANALYTIC, Event::BasePower, On) => {
            let user = ev.target_pos()?;
            let moves_later = super::all_active(battle).filter(|&p| p != user).any(|p| queue::will_move(battle, p));
            if !moves_later {
                ev.chain(5325);
            }
            None
        }
        // Sand Force (21): Rock, Ground and Steel moves in a sandstorm
        // (`field.isWeather`), 5325/4096.
        (a::SAND_FORCE, Event::BasePower, On) => {
            let ty = ev.move_data()?.ty;
            if super::effective_weather(battle) == Some(Weather::Sand)
                && matches!(ty, Type::Rock | Type::Ground | Type::Steel)
            {
                ev.chain(5325);
            }
            None
        }
        // Sheer Force (21): a move whose secondaries it removed (or Electro
        // Shot, Order Up: `hasSheerForceBoost`), 5325/4096.
        (a::SHEER_FORCE, Event::BasePower, On) => {
            let boosted = ev.move_data()?.unmodelled.contains(&"hasSheerForceBoost");
            if battle.move_state.has_sheer_force || boosted {
                ev.chain(5325);
            }
            None
        }
        // Tough Claws (21): contact moves (`move.flags.contact`, after Long
        // Reach), 5325/4096.
        (a::TOUGH_CLAWS, Event::BasePower, On) => flag_boost(ev, MoveFlags::CONTACT, 5325),
        // Mega Launcher, Strong Jaw, Sharpness (19): pulse, bite and
        // slicing moves, 1.5x.
        (a::MEGA_LAUNCHER, Event::BasePower, On) => flag_boost(ev, MoveFlags::PULSE, 6144),
        (a::STRONG_JAW, Event::BasePower, On) => flag_boost(ev, MoveFlags::BITE, 6144),
        (a::SHARPNESS, Event::BasePower, On) => flag_boost(ev, MoveFlags::SLICING, 6144),
        // Flare Boost (19): special moves while burned, 1.5x.
        (a::FLARE_BOOST, Event::BasePower, On) => {
            let special = ev.move_data()?.category == MoveCategory::Special;
            if special && mon(battle, holder)?.status == Status::Burn { ev.chain_modify(3, 2) } else { None }
        }
        // Toxic Boost (19): physical moves while poisoned, 1.5x.
        (a::TOXIC_BOOST, Event::BasePower, On) => {
            let physical = ev.move_data()?.category == MoveCategory::Physical;
            if physical && poisoned(mon(battle, holder)?) { ev.chain_modify(3, 2) } else { None }
        }
        // Punk Rock (7): sound moves, 5325/4096.
        (a::PUNK_ROCK, Event::BasePower, On) => flag_boost(ev, MoveFlags::SOUND, 5325),
        // Battery and Power Spot (`onAllyBasePower`, 22): an ally's special
        // move, or any move of an ally, 5325/4096; Steely Spirit (22): the
        // holder's and its allies' Steel moves, 1.5x.
        (a::BATTERY, Event::BasePower, Ally) => {
            let special = ev.move_data()?.category == MoveCategory::Special;
            if ev.target_pos() != Some(holder) && special {
                ev.chain(5325);
            }
            None
        }
        (a::POWER_SPOT, Event::BasePower, Ally) => {
            if ev.target_pos() != Some(holder) {
                ev.chain(5325);
            }
            None
        }
        (a::STEELY_SPIRIT, Event::BasePower, Ally) => {
            if ev.move_data()?.ty == Type::Steel {
                ev.chain_modify(3, 2)
            } else {
                None
            }
        }

        // ---------------------------------------------------------------
        // `ModifyAtk` / `ModifySpA` (the attacker's; `ev.source` is the
        // defender) and the defender's `onSourceModifyAtk` / `SpA`.

        // `onModifyAtk` (priority 5): 2x.
        (a::HUGE_POWER | a::PURE_POWER, Event::ModifyAtk, On) => ev.chain_modify(2, 1),
        // Hustle (5): `this.modify(atk, 1.5)`, applied at once rather than
        // chained.
        (a::HUSTLE, Event::ModifyAtk, On) => {
            Some(Relay::Num(crate::stats::modify(ev.value().max(0) as u32, 6144) as i32))
        }
        // Guts (5): 1.5x with a status.
        (a::GUTS, Event::ModifyAtk, On) => {
            if mon(battle, holder)?.status != Status::None {
                ev.chain_modify(3, 2)
            } else {
                None
            }
        }
        // Blaze, Overgrow, Torrent, Swarm (5): moves of their type at a
        // third of max HP or less, 1.5x.
        (a::BLAZE | a::OVERGROW | a::TORRENT | a::SWARM, Event::ModifyAtk | Event::ModifySpA, On) => {
            let ty = match ability {
                a::BLAZE => Type::Fire,
                a::OVERGROW => Type::Grass,
                a::TORRENT => Type::Water,
                _ => Type::Bug,
            };
            let m = mon(battle, holder)?;
            let low = 3 * m.hp as u32 <= m.max_hp() as u32;
            if ev.move_data()?.ty == ty && low { ev.chain_modify(3, 2) } else { None }
        }
        // Dragon's Maw, Steelworker, Rocky Payload, Fire Mane (5): moves of
        // their type, 1.5x; Transistor (5): Electric moves, 5325/4096.
        (
            a::DRAGONS_MAW | a::STEELWORKER | a::ROCKY_PAYLOAD | a::FIRE_MANE,
            Event::ModifyAtk | Event::ModifySpA,
            On,
        ) => {
            let ty = match ability {
                a::DRAGONS_MAW => Type::Dragon,
                a::STEELWORKER => Type::Steel,
                a::ROCKY_PAYLOAD => Type::Rock,
                _ => Type::Fire,
            };
            if ev.move_data()?.ty == ty { ev.chain_modify(3, 2) } else { None }
        }
        (a::TRANSISTOR, Event::ModifyAtk | Event::ModifySpA, On) => {
            if ev.move_data()?.ty == Type::Electric {
                ev.chain(5325);
            }
            None
        }
        // Defeatist (5): half Attack and Special Attack at half HP or less.
        (a::DEFEATIST, Event::ModifyAtk | Event::ModifySpA, On) => {
            let m = mon(battle, holder)?;
            if 2 * m.hp as u32 <= m.max_hp() as u32 { ev.chain_modify(1, 2) } else { None }
        }
        // Stakeout (5): 2x against a Pokémon that switched in this turn
        // (`!defender.activeTurns`).
        (a::STAKEOUT, Event::ModifyAtk | Event::ModifySpA, On) => {
            let defender = ev.source?;
            if mon(battle, defender)?.active_turns == 0 { ev.chain_modify(2, 1) } else { None }
        }
        // Gorilla Tactics (priority 1): 1.5x (not while Dynamaxed).
        (a::GORILLA_TACTICS, Event::ModifyAtk, On) => ev.chain_modify(3, 2),
        // Water Bubble: its own Water moves 2x; Fire moves against it half
        // (`onSourceModifyAtk` / `SpA`, priority 5).
        (a::WATER_BUBBLE, Event::ModifyAtk | Event::ModifySpA, On) => {
            if ev.move_data()?.ty == Type::Water {
                ev.chain_modify(2, 1)
            } else {
                None
            }
        }
        (a::WATER_BUBBLE | a::HEATPROOF, Event::ModifyAtk | Event::ModifySpA, Source) => {
            if ev.move_data()?.ty == Type::Fire { ev.chain_modify(1, 2) } else { None }
        }
        // Thick Fat (`onSourceModifyAtk` 6, `SpA` 5): Fire and Ice moves
        // against it deal half.
        (a::THICK_FAT, Event::ModifyAtk | Event::ModifySpA, Source) => {
            if matches!(ev.move_data()?.ty, Type::Fire | Type::Ice) { ev.chain_modify(1, 2) } else { None }
        }
        // Plus and Minus (`onModifySpA`, 5): 1.5x when an ally
        // (`pokemon.allies()`) has Plus or Minus.
        (a::PLUS | a::MINUS, Event::ModifySpA, On) => {
            let side = holder.side;
            let partner = (0..battle.format.active_per_side() as u8).map(|s| Position::new(side, s)).any(|p| {
                p != holder
                    && events::is_active(battle, p)
                    && mon(battle, p).is_some_and(|m| m.hp > 0)
                    && (super::has_ability(battle, p, a::PLUS) || super::has_ability(battle, p, a::MINUS))
            });
            if partner { ev.chain_modify(3, 2) } else { None }
        }
        // Solar Power (`onModifySpA`, 5): 1.5x in the holder's sun.
        (a::SOLAR_POWER, Event::ModifySpA, On) => {
            if sunny(super::pokemon_weather(battle, holder)) {
                ev.chain_modify(3, 2)
            } else {
                None
            }
        }

        // ---------------------------------------------------------------
        // `ModifyDef` (the defender's).

        // Fur Coat (6): 2x.
        (a::FUR_COAT, Event::ModifyDef, On) => ev.chain_modify(2, 1),
        // Marvel Scale (6): 1.5x with a status.
        (a::MARVEL_SCALE, Event::ModifyDef, On) => {
            if mon(battle, holder)?.status != Status::None {
                ev.chain_modify(3, 2)
            } else {
                None
            }
        }
        // Grass Pelt (6): 1.5x on Grassy Terrain (`field.isTerrain`).
        (a::GRASS_PELT, Event::ModifyDef, On) => {
            if battle.state.field.terrain == Some(Terrain::Grassy) {
                ev.chain_modify(3, 2)
            } else {
                None
            }
        }

        // ---------------------------------------------------------------
        // `ModifySpe`.

        // Chlorophyll and Swift Swim: 2x in the holder's sun or rain
        // (`pokemon.effectiveWeather()`: Utility Umbrella hides it).
        (a::CHLOROPHYLL, Event::ModifySpe, On) => {
            if sunny(super::pokemon_weather(battle, holder)) {
                ev.chain_modify(2, 1)
            } else {
                None
            }
        }
        (a::SWIFT_SWIM, Event::ModifySpe, On) => {
            let rain = matches!(super::pokemon_weather(battle, holder), Some(Weather::Rain | Weather::HeavyRain));
            if rain { ev.chain_modify(2, 1) } else { None }
        }
        // Sand Rush and Slush Rush: 2x in a sandstorm or snow
        // (`field.isWeather`).
        (a::SAND_RUSH, Event::ModifySpe, On) => {
            if super::effective_weather(battle) == Some(Weather::Sand) {
                ev.chain_modify(2, 1)
            } else {
                None
            }
        }
        (a::SLUSH_RUSH, Event::ModifySpe, On) => {
            if super::effective_weather(battle) == Some(Weather::Snow) {
                ev.chain_modify(2, 1)
            } else {
                None
            }
        }
        // Surge Surfer: 2x on Electric Terrain.
        (a::SURGE_SURFER, Event::ModifySpe, On) => {
            if battle.state.field.terrain == Some(Terrain::Electric) {
                ev.chain_modify(2, 1)
            } else {
                None
            }
        }
        // Quick Feet: 1.5x with a status (paralysis's own Speed drop checks
        // for it: `status::par_modify_spe`).
        (a::QUICK_FEET, Event::ModifySpe, On) => {
            if mon(battle, holder)?.status != Status::None {
                ev.chain_modify(3, 2)
            } else {
                None
            }
        }

        // ---------------------------------------------------------------
        // `ModifySTAB`, `ModifyCritRatio`, `CriticalHit`.

        // Adaptability: STAB (`move.forceSTAB` or the holder's type) is 2x,
        // 2.25x with a Tera type matching an original type. In 4096ths.
        (a::ADAPTABILITY, Event::ModifySTAB, On) => {
            let ty = ev.move_data()?.ty;
            let stab =
                manipulation::has_type(mon(battle, holder)?, ty) || crate::resolve::pledges::force_stab(battle, holder);
            if !stab {
                return None;
            }
            Some(Relay::Num(if ev.value() == 8192 { 9216 } else { 8192 }))
        }
        // Super Luck: +1 crit ratio.
        (a::SUPER_LUCK, Event::ModifyCritRatio, On) => Some(Relay::Num(ev.value() + 1)),
        // Merciless: a guaranteed crit against a poisoned target (5, capped
        // to 4 by `getDamage`).
        (a::MERCILESS, Event::ModifyCritRatio, On) => {
            let target = ev.source?;
            if poisoned(mon(battle, target)?) { Some(Relay::Num(5)) } else { None }
        }
        // Battle Armor and Shell Armor: `onCriticalHit: false`.
        (a::BATTLE_ARMOR | a::SHELL_ARMOR, Event::CriticalHit, On) => Some(Relay::False),

        // ---------------------------------------------------------------
        // `ModifyDamage` (the attacker's; `ev.source` is the defender), the
        // defender's `onSourceModifyDamage` and Friend Guard's
        // `onAnyModifyDamage`.

        // Sniper: crits deal 1.5x.
        (a::SNIPER, Event::ModifyDamage, On) => {
            let target = ev.source?;
            if battle.move_state.hit_data(target).crit { ev.chain_modify(3, 2) } else { None }
        }
        // Tinted Lens: resisted moves deal 2x.
        (a::TINTED_LENS, Event::ModifyDamage, On) => {
            let target = ev.source?;
            if battle.move_state.hit_data(target).type_mod < 0 { ev.chain_modify(2, 1) } else { None }
        }
        // Neuroforce: super-effective moves deal 5120/4096.
        (a::NEUROFORCE, Event::ModifyDamage, On) => {
            let target = ev.source?;
            if battle.move_state.hit_data(target).type_mod > 0 {
                ev.chain(5120);
            }
            None
        }
        // `onSourceModifyDamage`: half damage at full HP.
        (a::MULTISCALE | a::SHADOW_SHIELD, Event::ModifyDamage, Source) => {
            let m = mon(battle, holder)?;
            if m.hp >= m.max_hp() { ev.chain_modify(1, 2) } else { None }
        }
        // Filter, Solid Rock, Prism Armor: super-effective moves deal 0.75x.
        (a::FILTER | a::SOLID_ROCK | a::PRISM_ARMOR, Event::ModifyDamage, Source) => {
            if battle.move_state.hit_data(holder).type_mod > 0 { ev.chain_modify(3, 4) } else { None }
        }
        // Fluffy: Fire moves 2x, contact moves (`move.flags.contact`) half;
        // one `chainModify(mod)`.
        (a::FLUFFY, Event::ModifyDamage, Source) => {
            let data = ev.move_data()?;
            let fire = data.ty == Type::Fire;
            let contact = data.flags.contains(MoveFlags::CONTACT);
            match (fire, contact) {
                (true, false) => ev.chain_modify(2, 1),
                (false, true) => ev.chain_modify(1, 2),
                _ => ev.chain_modify(1, 1),
            }
        }
        // Aura Guard (`data/abilities.ts` auraguard):
        // `onSourceModifyDamage`, contact moves (`move.flags['contact']`)
        // deal half. Breakable (Mold Breaker ignores it).
        (a::AURA_GUARD, Event::ModifyDamage, Source) => {
            if ev.move_data()?.flags.contains(MoveFlags::CONTACT) {
                ev.chain_modify(1, 2)
            } else {
                None
            }
        }
        // Punk Rock: sound moves against it deal half.
        (a::PUNK_ROCK, Event::ModifyDamage, Source) => {
            if ev.move_data()?.flags.contains(MoveFlags::SOUND) {
                ev.chain_modify(1, 2)
            } else {
                None
            }
        }
        // Ice Scales: special moves against it deal half.
        (a::ICE_SCALES, Event::ModifyDamage, Source) => {
            if ev.move_data()?.category == MoveCategory::Special {
                ev.chain_modify(1, 2)
            } else {
                None
            }
        }
        // Friend Guard: moves against the holder's allies deal 0.75x.
        (a::FRIEND_GUARD, Event::ModifyDamage, Any) => {
            let target = ev.source?;
            if target != holder && target.side == holder.side { ev.chain_modify(3, 4) } else { None }
        }

        // ---------------------------------------------------------------
        // Accuracy, weight, stages.

        // Hustle (`onSourceModifyAccuracy`, -1): physical moves with an
        // accuracy check, 3277/4096.
        (a::HUSTLE, Event::ModifyAccuracy, Source) => {
            let physical = ev.move_data()?.category == MoveCategory::Physical;
            if physical && matches!(ev.relay, Relay::Num(_)) {
                ev.chain(3277);
            }
            None
        }
        // Heavy Metal (priority 1): twice the weight; Light Metal: half,
        // truncated.
        (a::HEAVY_METAL, Event::ModifyWeight, On) => Some(Relay::Num(ev.value() * 2)),
        (a::LIGHT_METAL, Event::ModifyWeight, On) => Some(Relay::Num(ev.value() / 2)),
        // Unaware (`onAnyModifyBoost`): the holder's attacker ignores its
        // target's defensive and evasion stages; a Pokémon attacking the
        // holder ignores its own offensive and accuracy stages.
        (a::UNAWARE, Event::ModifyBoost, Any) => unaware(battle, ev, holder),

        // ---------------------------------------------------------------
        // `ModifyType`.

        // The -ate abilities (priority -1): a Normal move (not Struggle,
        // `???` by then; nor the moves that set their own type) becomes
        // their type, boosted by their `onBasePower`; Normalize (priority
        // 1) makes every move Normal, boosted too. The rule is
        // `typing::ability_type`; the priorities are the handlers'.
        (a::AERILATE | a::DRAGONIZE | a::GALVANIZE | a::PIXILATE | a::REFRIGERATE, Event::ModifyType, On)
        | (a::NORMALIZE, Event::ModifyType, On) => {
            let terastallized = mon(battle, holder)?.terastallized;
            let move_id = ev.effect.move_id()?;
            let data = ev.move_mut()?;
            if let Some(ty) = crate::typing::ability_type(ability, battle.format.dex, move_id, data.ty, terastallized) {
                data.ty = ty;
                battle.move_state.type_changer = Some(ability);
            }
            None
        }
        // Liquid Voice (priority -1): sound moves become Water (no boost).
        (a::LIQUID_VOICE, Event::ModifyType, On) => {
            let move_id = ev.effect.move_id()?;
            let data = ev.move_mut()?;
            if let Some(ty) = crate::typing::ability_type(ability, battle.format.dex, move_id, data.ty, false) {
                data.ty = ty;
            }
            None
        }

        // ---------------------------------------------------------------
        // `ModifyMove` (the user's, on the active move).

        // Infiltrator: `move.infiltrates`.
        (a::INFILTRATOR, Event::ModifyMove, On) => {
            battle.move_state.infiltrates = true;
            None
        }
        // Long Reach: the move makes no contact.
        (a::LONG_REACH, Event::ModifyMove, On) => {
            let data = ev.move_mut()?;
            data.flags = MoveFlags(data.flags.0 & !MoveFlags::CONTACT.0);
            None
        }
        // Unseen Fist (Scarlet/Violet): contact moves lose `protect`. (The
        // Champions mod replaces it with an `onHitProtect`.)
        (a::UNSEEN_FIST, Event::ModifyMove, On) => {
            let data = ev.move_mut()?;
            if data.flags.contains(MoveFlags::CONTACT) {
                data.flags = MoveFlags(data.flags.0 & !MoveFlags::PROTECT.0);
            }
            None
        }
        // Scrappy and Mind's Eye (priority -5): Normal and Fighting moves
        // ignore immunities (`move.ignoreImmunity`); Mind's Eye also
        // ignores evasion.
        (a::SCRAPPY | a::MINDS_EYE, Event::ModifyMove, On) => {
            if ability == a::MINDS_EYE {
                battle.move_state.ignore_evasion = true;
            }
            battle.move_state.ignore_immunity_fighting_normal = true;
            None
        }
        // Serene Grace (priority -2): secondary chances (and `self.chance`)
        // double.
        (a::SERENE_GRACE, Event::ModifyMove, On) => {
            battle.move_state.chance_doublings += 1;
            None
        }
        // Sheer Force: a move with secondaries loses them and its `self`
        // effects (`move.hasSheerForce`), unless it has
        // `hasSheerForceBoost`.
        (a::SHEER_FORCE, Event::ModifyMove, On) => {
            let data = ev.move_mut()?;
            if !data.secondaries.is_empty() && !data.unmodelled.contains(&"hasSheerForceBoost") {
                data.secondaries = &[];
                data.self_boosts = None;
                data.self_volatile = None;
                battle.move_state.has_sheer_force = true;
            }
            None
        }
        // Skill Link: a multi-hit range hits its maximum; the per-hit
        // accuracy checks go (`delete move.multiaccuracy`).
        (a::SKILL_LINK, Event::ModifyMove, On) => {
            let data = ev.move_mut()?;
            if let Some((min, max)) = data.multihit
                && min != max
            {
                data.multihit = Some((max, max));
            }
            battle.move_state.no_multiaccuracy = true;
            None
        }
        // Stench (priority -1): a damaging move without a flinch chance gets
        // a 10% one (`move.secondaries.push`).
        (a::STENCH, Event::ModifyMove, On) => {
            let data = ev.move_data()?;
            if data.category != MoveCategory::Status && !has_flinch(data) {
                battle.move_state.added_flinch = true;
            }
            None
        }
        // Gorilla Tactics: locks the holder into its first move
        // (`abilityState.choiceLock`).
        (a::GORILLA_TACTICS, Event::ModifyMove, On) => {
            let move_id = ev.effect.move_id()?;
            let state = &mut battle.state.at_mut(holder)?.volatile_counters.ability_state;
            if state.choice_lock.is_none() && move_id != mv::STRUGGLE {
                state.choice_lock = Some(move_id);
            }
            None
        }

        // ---------------------------------------------------------------
        // Gorilla Tactics' lock.
        (a::GORILLA_TACTICS, Event::Start | Event::End, On) => {
            if let Some(m) = battle.state.side_mut(holder.side).team.get_mut(holder.slot as usize) {
                m.volatile_counters.ability_state.choice_lock = None;
            }
            None
        }
        // `onBeforeMove`: another move than the locked one (not Struggle)
        // fails with `|move|` and `-fail`.
        (a::GORILLA_TACTICS, Event::BeforeMove, On) => {
            let move_id = ev.effect.move_id()?;
            let locked = mon(battle, holder)?.volatile_counters.ability_state.choice_lock?;
            if move_id == mv::STRUGGLE || locked == move_id {
                return None;
            }
            let user = actor(battle, holder);
            battle.emit(BattleEvent::Move { user, move_id, target: None, from: None });
            crate::resolve::moves::fail_still(battle, holder);
            Some(Relay::False)
        }
        // `onDisableMove`: every other move is disabled.
        (a::GORILLA_TACTICS, Event::DisableMove, On) => {
            let m = mon(battle, holder)?;
            let locked = m.volatile_counters.ability_state.choice_lock?;
            let others: crate::util::StackVec<MoveId, 4> =
                m.moves.iter().map(|s| s.id).filter(|&id| id != locked).collect();
            for &id in others.iter() {
                events::disable_move(battle, holder, id);
            }
            None
        }

        // ---------------------------------------------------------------
        // Stage changes.

        // Scrappy: Intimidate's Attack drop fails.
        (a::SCRAPPY, Event::TryBoost, On) => {
            let Relay::Boosts(mut b) = ev.relay else { return None };
            if ev.effect != Effect::Ability(a::INTIMIDATE) || b.get(BoostId::Atk).unwrap_or(0) == 0 {
                return None;
            }
            b.delete(BoostId::Atk);
            fail_unboost(battle, holder, ability, BoostId::Atk.id());
            Some(Relay::Boosts(b))
        }
        // Mind's Eye: accuracy can't be lowered by others (the message only
        // for an effect without secondaries).
        (a::MINDS_EYE, Event::TryBoost, On) => {
            if ev.source == Some(holder) {
                return None;
            }
            let Relay::Boosts(mut b) = ev.relay else { return None };
            if b.get(BoostId::Accuracy).unwrap_or(0) >= 0 {
                return None;
            }
            b.delete(BoostId::Accuracy);
            if !events::effect_has_secondaries(battle, ev.effect) {
                fail_unboost(battle, holder, ability, BoostId::Accuracy.id());
            }
            Some(Relay::Boosts(b))
        }
        // Eelevate (`onSourceAfterFaint`): knocking out Pokémon with a move
        // raises the holder's best stat by the number of them.
        (a::EELEVATE, Event::AfterFaint, Source) => {
            if !ev.effect.is_move() {
                return None;
            }
            let stat = super::best_stat(battle, holder, true);
            let changes = boosts::one_boost(stat, ev.value().clamp(0, 6) as i8);
            let source = ev.source;
            boosts::boost_from(battle, holder, changes, source, Effect::Ability(ability), false, false);
            None
        }

        // ---------------------------------------------------------------
        // Immunities, weather, status.

        // Sand Force and Sand Rush: no sandstorm damage.
        (a::SAND_FORCE | a::SAND_RUSH, Event::Immunity, On) => {
            if ev.relay == Relay::Id("sandstorm") {
                Some(Relay::False)
            } else {
                None
            }
        }
        // Solar Power (`onWeather`): 1/8 of max HP each turn in the
        // holder's sun.
        (a::SOLAR_POWER, Event::Weather, On) => {
            let Effect::Weather(weather) = ev.effect else { return None };
            if super::pokemon_weather(battle, holder) != Some(weather) || !sunny(Some(weather)) {
                return None;
            }
            let amount = crate::resolve::hp_fraction(mon(battle, holder)?.max_hp() as u32, 1, 8);
            damage_by(battle, holder, amount, Some(holder), Effect::Ability(ability));
            None
        }
        // Heatproof (`onDamage`): half burn damage.
        (a::HEATPROOF, Event::Damage, On) => {
            if ev.effect != Effect::Status(StatusId::Burn) {
                return None;
            }
            // `damage / 2`, floored to at least 1 by `spreadDamage`.
            let n = ev.value();
            if n > 0 { Some(Relay::Num((n / 2).max(1))) } else { None }
        }
        // Water Bubble: can't be burned (`-immune` for a move that burns as
        // its primary effect); a burn is cured on `Update`.
        (a::WATER_BUBBLE, Event::SetStatus, On) => {
            if ev.relay != Relay::Status(StatusId::Burn) {
                return None;
            }
            if super::effect_has_status(battle, ev.effect) {
                let target = actor(battle, holder);
                let cause = Cause::ability(ability);
                battle.emit(BattleEvent::ImmuneFrom { target, detail: None, cause });
            }
            Some(Relay::False)
        }
        (a::WATER_BUBBLE, Event::Update, On) => {
            if mon(battle, holder)?.status == Status::Burn {
                super::activate(battle, holder, ability);
                status::cure_status(battle, holder);
            }
            None
        }

        // ---------------------------------------------------------------
        // Hits.

        // Parental Bond (`onPrepareHit`): a damaging move with one target
        // that is not multi-hit, charging, a future move or
        // `noparentalbond` hits twice (`move.multihit = 2`,
        // `multihitType`), the second time for 1/4 (`modifyDamage`).
        (a::PARENTAL_BOND, Event::PrepareHit, On) => {
            let data = ev.move_data()?;
            let excluded = MoveFlags::NO_PARENTAL_BOND.0 | MoveFlags::CHARGE.0 | MoveFlags::FUTURE_MOVE.0;
            if data.category != MoveCategory::Status
                && data.multihit.is_none()
                && data.flags.0 & excluded == 0
                && !battle.move_state.spread_hit
            {
                battle.move_state.parental_bond = true;
            }
            None
        }
        // Piercing Drill and the Champions mod's Unseen Fist
        // (`onHitProtect`): a contact move goes through protection
        // (`getMoveHitData(move).bypassProtect`) for 1/4 of its damage.
        (a::PIERCING_DRILL | a::UNSEEN_FIST, Event::HitProtect, On) => {
            let target = ev.source?;
            if !ev.move_data()?.flags.contains(MoveFlags::CONTACT) {
                return None;
            }
            battle.move_state.bypass_protect |= 1 << (target.side.index() * 2 + (target.slot as usize & 1));
            battle.move_state.bypass_ability = Some(ability);
            Some(Relay::False)
        }
        // Mega Sol (`onWeatherModifyDamage`, priority 1): Sunny
        // Day's modifier, whatever the weather (`resolve/mega.rs`).
        (a::MEGA_SOL, Event::WeatherModifyDamage, On) => crate::resolve::mega::weather_modify_damage(battle, ev),
        _ => None,
    }
}

/// The Pokémon at `pos` (its team position, on the field or not).
#[inline]
fn mon(battle: &Battle, pos: Position) -> Option<&PokemonState> {
    battle.state.side(pos.side).team.get(pos.slot as usize)
}

/// A `chainModify` of `modifier` / 4096 for moves with `flag`.
fn flag_boost(ev: &mut EventCtx<'_>, flag: MoveFlags, modifier: u32) -> Option<Relay> {
    if ev.move_data()?.flags.contains(flag) {
        ev.chain(modifier);
    }
    None
}

/// Sun or Desolate Land's harsh sun.
fn sunny(weather: Option<Weather>) -> bool {
    matches!(weather, Some(Weather::Sun | Weather::HarshSun))
}

/// The Pokémon's gender (`pokemon.gender`), `None` when genderless.
fn gender(battle: &Battle, pos: Position) -> Option<Gender> {
    Some(mon(battle, pos)?.gender).filter(|&g| g != Gender::Genderless)
}

/// Whether a move's secondaries include a flinch (Stench's and King's
/// Rock's check).
fn has_flinch(data: &MoveData) -> bool {
    data.secondaries.iter().any(|s| s.volatile_status == Some("flinch"))
}

/// `-fail|<holder>|unboost|<stat>|[from] ability: X|[of] <holder>`.
fn fail_unboost(battle: &mut Battle, holder: Position, ability: AbilityId, stat: &'static str) {
    let target = actor(battle, holder);
    let cause = Cause::ability(ability).of(target);
    battle.emit(BattleEvent::FailFrom {
        target,
        what: LineArg::Text("unboost"),
        detail: Some(LineArg::Text(stat)),
        cause,
    });
}

/// Unaware's `onAnyModifyBoost`, held by `holder`: while the holder uses a
/// move on the Pokémon whose stages are read (`activeTarget`), that
/// Pokémon's Defense, Special Defense and evasion stages are 0; while that
/// Pokémon uses a move on the holder, its Attack, Defense, Special Attack
/// and accuracy stages are 0.
fn unaware(battle: &Battle, ev: &mut EventCtx<'_>, holder: Position) -> Option<Relay> {
    let pokemon = ev.target_pos()?;
    if pokemon == holder {
        return None;
    }
    let Relay::Boosts(mut b) = ev.relay else { return None };
    let active_pokemon = battle.active_move.map(|(user, _)| user);
    let active_target = battle.move_state.active_target;
    let mut changed = false;
    if active_pokemon == Some(holder) && active_target == Some(pokemon) {
        for id in [BoostId::Def, BoostId::SpD, BoostId::Evasion] {
            b.set(id, 0);
        }
        changed = true;
    }
    if active_pokemon == Some(pokemon) && active_target == Some(holder) {
        for id in [BoostId::Atk, BoostId::Def, BoostId::SpA, BoostId::Accuracy] {
            b.set(id, 0);
        }
        changed = true;
    }
    changed.then_some(Relay::Boosts(b))
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::{items as i, moves as mv, species as s};
    use pikapro_data::{ItemId, SpeciesId, Stat, StatBoosts};

    use super::*;
    use crate::resolve::events::run_modify;
    use crate::state::SideId;
    use crate::{BattleOptions, Format, PokemonSet, Seed, Team};

    const P1: Position = Position::new(SideId::P1, 0);
    const P2: Position = Position::new(SideId::P2, 0);

    fn set(species: SpeciesId, ability: AbilityId, item: Option<ItemId>, moves: &[MoveId]) -> PokemonSet {
        let s = PokemonSet::new(species).with_level(50).with_ability(ability).with_moves(moves);
        match item {
            Some(i) => s.with_item(i),
            None => s,
        }
    }

    fn singles(p1: PokemonSet, p2: PokemonSet) -> Battle {
        let format = Format::gen9_singles();
        Battle::new(format, [Team::new(vec![p1]), Team::new(vec![p2])], BattleOptions::new(Seed::fast(1))).unwrap()
    }

    /// The Pokémon at `pos` uses `move_id` on the foe: its `ModifyType`
    /// and `ModifyMove` events, as `run_move` / `use_move` run them.
    fn active_move(b: &mut Battle, pos: Position, move_id: MoveId) -> MoveData {
        let foe = Position::new(pos.side.foe(), 0);
        b.active_move = Some((pos, move_id));
        let mut data = crate::resolve::moves::typed_data(b, pos, move_id);
        b.move_state = events::MoveState { active_target: Some(foe), ..Default::default() };
        crate::resolve::moves::modify_type_event(b, pos, Some(foe), move_id, &mut data);
        events::modify_move_event(b, Event::ModifyMove, pos, Some(foe), move_id, &mut data);
        data
    }

    fn modify(b: &mut Battle, event: Event, pos: Position, move_id: MoveId, data: &MoveData, value: u32) -> u32 {
        let foe = Position::new(pos.side.foe(), 0);
        run_modify(b, event, pos, Some(foe), Effect::Move(move_id), value, Some(data), None)
    }

    /// Pixilate makes Normal moves Fairy and boosts them (4915/4096); the
    /// moves that set their own type keep theirs.
    #[test]
    fn ate_abilities_change_the_type_and_boost() {
        let mut b = singles(
            set(s::SYLVEON, a::PIXILATE, None, &[mv::HYPER_VOICE, mv::WEATHER_BALL]),
            set(s::GARCHOMP, a::ROUGH_SKIN, None, &[mv::TACKLE]),
        );
        let data = active_move(&mut b, P1, mv::HYPER_VOICE);
        assert_eq!(data.ty, Type::Fairy);
        assert_eq!(b.move_state.type_changer, Some(a::PIXILATE));
        let bp = modify(&mut b, Event::BasePower, P1, mv::HYPER_VOICE, &data, 90);
        assert_eq!(bp, crate::stats::modify(90, 4915));
        let data = active_move(&mut b, P1, mv::WEATHER_BALL);
        assert_eq!((data.ty, b.move_state.type_changer), (Type::Normal, None));
        assert_eq!(modify(&mut b, Event::BasePower, P1, mv::WEATHER_BALL, &data, 50), 50);
    }

    /// Sheer Force drops a move's secondaries and boosts it; a move without
    /// secondaries is left alone.
    #[test]
    fn sheer_force_drops_secondaries() {
        let mut b = singles(
            set(s::NIDOKING, a::SHEER_FORCE, None, &[mv::SLUDGE_BOMB, mv::EARTHQUAKE]),
            set(s::GARCHOMP, a::ROUGH_SKIN, None, &[mv::TACKLE]),
        );
        let data = active_move(&mut b, P1, mv::SLUDGE_BOMB);
        assert!(data.secondaries.is_empty() && b.move_state.has_sheer_force);
        let bp = modify(&mut b, Event::BasePower, P1, mv::SLUDGE_BOMB, &data, 90);
        assert_eq!(bp, crate::stats::modify(90, 5325));
        let data = active_move(&mut b, P1, mv::EARTHQUAKE);
        assert!(!b.move_state.has_sheer_force);
        assert_eq!(modify(&mut b, Event::BasePower, P1, mv::EARTHQUAKE, &data, 100), 100);
    }

    /// Unaware: the holder's foe attacks it without its own Attack stages,
    /// and the holder attacks through the foe's Defense stages.
    #[test]
    fn unaware_ignores_stages() {
        let mut b = singles(
            set(s::CLEFABLE, a::UNAWARE, None, &[mv::MOONBLAST]),
            set(s::GARCHOMP, a::ROUGH_SKIN, None, &[mv::TACKLE]),
        );
        let (atk, def) = {
            let m = b.state.at(P2).unwrap();
            (m.stats[Stat::Atk] as u32, m.stats[Stat::Def] as u32)
        };
        // Garchomp attacks Clefable: its Attack +2 is ignored.
        active_move(&mut b, P2, mv::TACKLE);
        assert_eq!(events::calculate_stat(&mut b, P2, Stat::Atk, 2, P2), atk);
        // Clefable attacks Garchomp: its Defense +2 is ignored.
        active_move(&mut b, P1, mv::MOONBLAST);
        assert_eq!(events::calculate_stat(&mut b, P2, Stat::Def, 2, P2), def);
        // Without a move involving the holder, the stages count.
        b.active_move = None;
        assert_eq!(events::calculate_stat(&mut b, P2, Stat::Atk, 2, P2), atk * 2);
    }

    /// Scrappy stops Intimidate's Attack drop (`-fail|...|unboost|Attack`).
    #[test]
    fn scrappy_blocks_intimidate() {
        let mut b = singles(
            set(s::INCINEROAR, a::INTIMIDATE, None, &[mv::TACKLE]),
            set(s::KANGASKHAN, a::SCRAPPY, None, &[mv::TACKLE]),
        );
        b.log.drain();
        let drop = StatBoosts::new([-1, 0, 0, 0, 0, 0, 0]);
        let changed = boosts::boost_from(&mut b, P2, drop, Some(P1), Effect::Ability(a::INTIMIDATE), true, false);
        assert!(!changed);
        assert_eq!(b.state.at(P2).unwrap().boosts.atk, 0);
        assert!(b.log.events().iter().any(|e| matches!(e, BattleEvent::FailFrom { .. })));
    }

    /// Heavy Metal doubles the weight, Light Metal halves it.
    #[test]
    fn metal_abilities_change_the_weight() {
        let mut b = singles(
            set(s::AGGRON, a::HEAVY_METAL, None, &[mv::TACKLE]),
            set(s::METAGROSS, a::LIGHT_METAL, None, &[mv::TACKLE]),
        );
        let weight =
            |b: &mut Battle, pos| run_modify(b, Event::ModifyWeight, pos, None, Effect::None, 1001, None, None);
        assert_eq!(weight(&mut b, P1), 2002);
        assert_eq!(weight(&mut b, P2), 500);
    }

    /// Water Bubble: no burns, and Water moves at 2x Attack.
    #[test]
    fn water_bubble() {
        let mut b = singles(
            set(s::ARAQUANID, a::WATER_BUBBLE, None, &[mv::LIQUIDATION]),
            set(s::CHANDELURE, a::FLASH_FIRE, None, &[mv::WILL_O_WISP]),
        );
        b.active_move = Some((P2, mv::WILL_O_WISP));
        assert!(!status::try_set_status(&mut b, P1, StatusId::Burn, Some(P2), false));
        assert_eq!(b.state.at(P1).unwrap().status, Status::None);
        let data = active_move(&mut b, P1, mv::LIQUIDATION);
        assert_eq!(modify(&mut b, Event::ModifyAtk, P1, mv::LIQUIDATION, &data, 100), 200);
    }

    /// Adaptability: 2x STAB; Stench adds a flinch to moves without one.
    #[test]
    fn adaptability_and_stench() {
        let mut b = singles(
            set(s::DRAGALGE, a::ADAPTABILITY, None, &[mv::SLUDGE_BOMB]),
            set(s::GARBODOR, a::STENCH, Some(i::LEFTOVERS), &[mv::GUNK_SHOT, mv::FAKE_OUT]),
        );
        let data = active_move(&mut b, P1, mv::SLUDGE_BOMB);
        assert_eq!(modify(&mut b, Event::ModifySTAB, P1, mv::SLUDGE_BOMB, &data, 6144), 8192);
        active_move(&mut b, P2, mv::GUNK_SHOT);
        assert!(b.move_state.added_flinch);
        active_move(&mut b, P2, mv::FAKE_OUT);
        assert!(!b.move_state.added_flinch);
    }
}
