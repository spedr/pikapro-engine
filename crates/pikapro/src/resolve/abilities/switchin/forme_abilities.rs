//! The forme-changing abilities of `switchin.rs`: Stance Change, Disguise,
//! Ice Face, Zero to Hero, Shields Down, Hunger Switch, Forecast, Flower
//! Gift, Mimicry, Schooling, Zen Mode, Power Construct, Battle Bond, Gulp
//! Missile, Embody Aspect, Tera Shift, Teraform Zero and Tera Shell.
//!
//! Showdown references: these abilities in `data/abilities.ts`,
//! `pokemon.formeChange` (`resolve/forme.rs`; Disguise also in the Champions
//! `formeChange` of `data/mods/champions/scripts.ts`), and Tera Shell in
//! `Pokemon#runEffectiveness`.

use pikapro_data::gen9::{abilities as a, moves as m, species as s};
use pikapro_data::{AbilityId, Event, EventPrefix, MoveCategory, MoveData, MoveFlags, MoveId, SpeciesId, StatBoosts};
use pikapro_data::{StatusId, Type, TypeSet};

use super::{ability_state, ability_state_mut, mon, mon_mut};
use crate::battle::Battle;
use crate::event::{BattleEvent, Cause, DamageSource, EffectName, LineArg};
use crate::format::Mechanics;
use crate::resolve::events::{self, Condition, Effect, EventCtx, Relay};
use crate::resolve::{abilities, actor, boosts, forme, manipulation};
use crate::state::{PokemonState, Position, Terrain, Volatiles, Weather};

/// Runs `ability`'s callback for `ev.event` / `ev.prefix` on its holder.
/// `None` for an ability or callback this file does not handle.
pub(super) fn run(
    battle: &mut Battle,
    ev: &mut EventCtx<'_>,
    holder: Position,
    ability: AbilityId,
) -> Option<Option<Relay>> {
    use EventPrefix::{Ally, On, Source};
    let relay = match (ability, ev.event, ev.prefix) {
        (a::STANCE_CHANGE, Event::ModifyMove, On) => {
            stance_change(battle, ev, holder);
            None
        }
        // Disguise.
        (a::DISGUISE, Event::Damage, On) => {
            let mimikyu = matches!(mon(battle, holder).species, s::MIMIKYU | s::MIMIKYU_TOTEM);
            if ev.effect.is_move() && mimikyu {
                abilities::activate(battle, holder, a::DISGUISE);
                ability_state_mut(battle, holder).busted = true;
                Some(Relay::Num(0))
            } else {
                None
            }
        }
        (a::DISGUISE, Event::CriticalHit, On) => {
            let data = *ev.move_data()?;
            let mimikyu = matches!(mon(battle, holder).species, s::MIMIKYU | s::MIMIKYU_TOTEM);
            let move_id = ev.effect.move_id();
            (mimikyu && !hits_substitute(battle, holder, &data) && move_immunity(battle, holder, move_id, &data))
                .then_some(Relay::False)
        }
        (a::DISGUISE, Event::Effectiveness, On) => disguise_effectiveness(battle, ev, holder),
        (a::DISGUISE, Event::Update, On) => {
            let species = mon(battle, holder).species;
            if matches!(species, s::MIMIKYU | s::MIMIKYU_TOTEM) && ability_state(battle, holder).busted {
                let busted = if species == s::MIMIKYU_TOTEM { s::MIMIKYU_BUSTED_TOTEM } else { s::MIMIKYU_BUSTED };
                forme::forme_change(battle, holder, busted, Effect::Ability(a::DISGUISE), true, false);
                let amount = crate::resolve::hp_fraction(u32::from(mon(battle, holder).max_hp()), 1, 8);
                let effect = Effect::Condition(Condition::Other);
                crate::resolve::spread_damage(
                    battle,
                    holder,
                    amount,
                    Some(holder),
                    effect,
                    DamageSource::Species(busted),
                );
            }
            None
        }
        // Ice Face.
        (a::ICE_FACE, Event::Start, On) => {
            ice_face_restore(battle, holder);
            None
        }
        (a::ICE_FACE, Event::WeatherChange, On) => {
            let suppressor = match ev.effect {
                Effect::Ability(x) => battle.format.dex.ability(x).suppress_weather,
                _ => false,
            };
            if !suppressor && mon(battle, holder).hp > 0 {
                ice_face_restore(battle, holder);
            }
            None
        }
        (a::ICE_FACE, Event::Damage, On) => {
            // `effect.category`: the active move's, after `ModifyMove`.
            let category = match ev.effect.move_id() {
                Some(id) => battle.move_state.category.or(Some(battle.format.dex.move_(id).category)),
                None => None,
            };
            let physical = category == Some(MoveCategory::Physical);
            if physical && mon(battle, holder).species == s::EISCUE {
                abilities::activate(battle, holder, a::ICE_FACE);
                ability_state_mut(battle, holder).busted = true;
                Some(Relay::Num(0))
            } else {
                None
            }
        }
        (a::ICE_FACE, Event::CriticalHit | Event::Effectiveness, On) => {
            let data = *ev.move_data()?;
            if data.category != MoveCategory::Physical || mon(battle, holder).species != s::EISCUE {
                return Some(None);
            }
            // `onCriticalHit` checks Infiltrator in every generation.
            if hits_substitute(battle, holder, &data) {
                return Some(None);
            }
            if !move_immunity(battle, holder, ev.effect.move_id(), &data) {
                return Some(None);
            }
            Some(if ev.event == Event::CriticalHit { Relay::False } else { Relay::Num(0) })
        }
        (a::ICE_FACE, Event::Update, On) => {
            if mon(battle, holder).species == s::EISCUE && ability_state(battle, holder).busted {
                forme::forme_change(battle, holder, s::EISCUE_NOICE, Effect::Ability(a::ICE_FACE), true, false);
            }
            None
        }
        // Zero to Hero.
        (a::ZERO_TO_HERO, Event::SwitchOut, On) => {
            let m = mon(battle, holder);
            if base_of(battle, m.base_species) == s::PALAFIN && m.species != s::PALAFIN_HERO {
                forme::forme_change(battle, holder, s::PALAFIN_HERO, Effect::Ability(a::ZERO_TO_HERO), true, false);
                mon_mut(battle, holder).hero_message_displayed = false;
            }
            None
        }
        (a::ZERO_TO_HERO, Event::SwitchIn, On) => {
            let m = mon(battle, holder);
            if base_of(battle, m.base_species) == s::PALAFIN
                && !m.hero_message_displayed
                && m.species == s::PALAFIN_HERO
            {
                abilities::activate(battle, holder, a::ZERO_TO_HERO);
                mon_mut(battle, holder).hero_message_displayed = true;
            }
            None
        }
        // Shields Down.
        (a::SHIELDS_DOWN, Event::Start, On) => {
            shields_down(battle, holder, false);
            None
        }
        (a::SHIELDS_DOWN, Event::Residual, On) => {
            shields_down(battle, holder, true);
            None
        }
        (a::SHIELDS_DOWN, Event::SetStatus, On) => {
            let m = mon(battle, holder);
            if m.species != s::MINIOR_METEOR || transformed(m) {
                return Some(None);
            }
            if abilities::effect_has_status(battle, ev.effect) {
                immune(battle, holder, a::SHIELDS_DOWN);
            }
            Some(Relay::False)
        }
        (a::SHIELDS_DOWN, Event::TryAddVolatile, On) => {
            let m = mon(battle, holder);
            if m.species != s::MINIOR_METEOR || transformed(m) || ev.relay != Relay::Id("yawn") {
                return Some(None);
            }
            immune(battle, holder, a::SHIELDS_DOWN);
            Some(Relay::Null)
        }
        // Hunger Switch (`onResidual`, order 29).
        (a::HUNGER_SWITCH, Event::Residual, On) => {
            let m = mon(battle, holder);
            if base_of(battle, m.species) == s::MORPEKO && !m.terastallized {
                let target = if m.species == s::MORPEKO { s::MORPEKO_HANGRY } else { s::MORPEKO };
                let source = implicit_source(battle, a::HUNGER_SWITCH);
                forme::forme_change(battle, holder, target, source, false, false);
            }
            None
        }
        // Forecast and Flower Gift.
        (a::FORECAST, Event::Start | Event::WeatherChange, On) => {
            forecast(battle, holder);
            None
        }
        (a::FLOWER_GIFT, Event::Start | Event::WeatherChange, On) => {
            flower_gift(battle, holder);
            None
        }
        (a::FLOWER_GIFT, Event::ModifyAtk | Event::ModifySpD, Ally) => {
            let cherrim = base_of(battle, mon(battle, holder).base_species) == s::CHERRIM;
            let stat_holder = ev.target_pos()?;
            let sunny =
                matches!(abilities::pokemon_weather(battle, stat_holder), Some(Weather::Sun | Weather::HarshSun));
            if cherrim && sunny { ev.chain_modify(3, 2) } else { None }
        }
        // Mimicry.
        (a::MIMICRY, Event::Start | Event::TerrainChange, On) => {
            mimicry(battle, holder);
            None
        }
        // Schooling.
        (a::SCHOOLING, Event::Start, On) => {
            schooling(battle, holder, false);
            None
        }
        (a::SCHOOLING, Event::Residual, On) => {
            schooling(battle, holder, true);
            None
        }
        // Zen Mode.
        (a::ZEN_MODE, Event::Residual, On) => {
            zen_mode_residual(battle, holder);
            None
        }
        (a::ZEN_MODE, Event::End, On) => {
            zen_mode_end(battle, holder);
            None
        }
        // Power Construct (`onResidual`, order 29).
        (a::POWER_CONSTRUCT, Event::Residual, On) => {
            let m = mon(battle, holder);
            if base_of(battle, m.base_species) == s::ZYGARDE
                && !transformed(m)
                && m.hp > 0
                && m.species != s::ZYGARDE_COMPLETE
                && u32::from(m.hp) * 2 <= u32::from(m.max_hp())
            {
                let set_species = m.set_species;
                abilities::activate(battle, holder, a::POWER_CONSTRUCT);
                forme::forme_change(
                    battle,
                    holder,
                    s::ZYGARDE_COMPLETE,
                    Effect::Ability(a::POWER_CONSTRUCT),
                    true,
                    false,
                );
                // `canMegaEvo` is refreshed, and `formeRegression`:
                // it reverts on fainting.
                crate::resolve::mega::refresh_can_mega_evo(battle, holder);
                mon_mut(battle, holder).forme_regression = Some(set_species);
            }
            None
        }
        // Battle Bond.
        (a::BATTLE_BOND, Event::AfterFaint, Source) => {
            battle_bond(battle, ev, holder);
            None
        }
        (a::BATTLE_BOND, Event::ModifyMove, On) => {
            let m = mon(battle, holder);
            if ev.effect.move_id() == Some(m::WATER_SHURIKEN)
                && m.species == s::GRENINJA_ASH
                && !transformed(m)
                && let Some(data) = ev.move_mut()
            {
                data.multihit = Some((3, 3));
            }
            None
        }
        // Gulp Missile.
        (a::GULP_MISSILE, Event::DamagingHit, On) => {
            gulp_missile_hit(battle, ev, holder);
            None
        }
        (a::GULP_MISSILE, Event::TryPrimaryHit, Source) => {
            let m = mon(battle, holder);
            if ev.effect.move_id() == Some(m::SURF)
                && abilities::has_ability(battle, holder, a::GULP_MISSILE)
                && m.species == s::CRAMORANT
            {
                let forme = if u32::from(m.hp) * 2 <= u32::from(m.max_hp()) {
                    s::CRAMORANT_GORGING
                } else {
                    s::CRAMORANT_GULPING
                };
                forme::forme_change(battle, holder, forme, ev.effect, false, false);
            }
            None
        }
        // Embody Aspect.
        (
            a::EMBODY_ASPECT_CORNERSTONE
            | a::EMBODY_ASPECT_HEARTHFLAME
            | a::EMBODY_ASPECT_TEAL
            | a::EMBODY_ASPECT_WELLSPRING,
            Event::Start,
            On,
        ) => {
            let (species, stages) = match ability {
                a::EMBODY_ASPECT_CORNERSTONE => (s::OGERPON_CORNERSTONE_TERA, [0, 1, 0, 0, 0, 0, 0]),
                a::EMBODY_ASPECT_HEARTHFLAME => (s::OGERPON_HEARTHFLAME_TERA, [1, 0, 0, 0, 0, 0, 0]),
                a::EMBODY_ASPECT_TEAL => (s::OGERPON_TEAL_TERA, [0, 0, 0, 0, 1, 0, 0]),
                _ => (s::OGERPON_WELLSPRING_TERA, [0, 0, 0, 1, 0, 0, 0]),
            };
            let m = mon(battle, holder);
            if m.base_species == species && m.terastallized && !ability_state(battle, holder).embodied {
                ability_state_mut(battle, holder).embodied = true;
                boosts::boost_from(
                    battle,
                    holder,
                    StatBoosts::new(stages),
                    None,
                    Effect::Ability(ability),
                    false,
                    false,
                );
            }
            None
        }
        // Tera Shift (`onSwitchIn`, priority 2).
        (a::TERA_SHIFT, Event::SwitchIn, On) => {
            let m = mon(battle, holder);
            if base_of(battle, m.base_species) == s::TERAPAGOS && m.species != s::TERAPAGOS_TERASTAL {
                abilities::activate(battle, holder, a::TERA_SHIFT);
                forme::forme_change(battle, holder, s::TERAPAGOS_TERASTAL, Effect::Ability(a::TERA_SHIFT), true, false);
            }
            None
        }
        // Teraform Zero.
        (a::TERAFORM_ZERO, Event::AfterTerastallization, On) => {
            let f = &battle.state.field;
            if mon(battle, holder).base_species == s::TERAPAGOS_STELLAR && (f.weather.is_some() || f.terrain.is_some())
            {
                abilities::show(battle, holder, a::TERAFORM_ZERO);
                super::clear_weather(battle);
                crate::resolve::field::clear_terrain(battle);
            }
            None
        }
        _ => return None,
    };
    Some(relay)
}

// ---------------------------------------------------------------------------
// Helpers.

/// A species' base species (Showdown's `species.baseSpecies`).
fn base_of(battle: &Battle, species: SpeciesId) -> SpeciesId {
    battle.format.dex.species(species).base_species
}

/// The source of `formeChange(species)` called without one: Showdown's
/// default, `battle.effect` (the ability running); the Champions mod's
/// `formeChange` has no default, so no `[from]`.
fn implicit_source(battle: &Battle, ability: AbilityId) -> Effect {
    match battle.format.mechanics {
        Mechanics::Champions => Effect::None,
        Mechanics::ScarletViolet => Effect::Ability(ability),
    }
}

/// Showdown's `pokemon.transformed`.
fn transformed(mon: &PokemonState) -> bool {
    mon.volatiles.has(Volatiles::TRANSFORMED)
}

/// `-immune|<pokemon>|[from] ability: X`.
fn immune(battle: &mut Battle, pos: Position, ability: AbilityId) {
    let target = actor(battle, pos);
    battle.emit(BattleEvent::ImmuneFrom { target, detail: None, cause: Cause::ability(ability) });
}

/// Disguise's and Ice Face's `hitSub`: the move would hit the holder's
/// substitute (not `bypasssub`, not `infiltrates`).
fn hits_substitute(battle: &Battle, pos: Position, data: &MoveData) -> bool {
    mon(battle, pos).volatiles.has(Volatiles::SUBSTITUTE)
        && !data.flags.contains(MoveFlags::BYPASS_SUB)
        && !battle.move_state.infiltrates
}

/// `target.runImmunity(move)` without a message: typeless moves (Struggle)
/// always pass.
fn move_immunity(battle: &mut Battle, pos: Position, move_id: Option<MoveId>, data: &MoveData) -> bool {
    if move_id == Some(m::STRUGGLE) {
        return true;
    }
    // `move.ignoreImmunity[type]` set by Scrappy and Mind's Eye (Normal and
    // Fighting): `runImmunity` passes.
    if battle.move_state.ignore_immunity_fighting_normal && matches!(data.ty, Type::Normal | Type::Fighting) {
        return true;
    }
    events::run_immunity(battle, pos, data.ty, false)
}

// ---------------------------------------------------------------------------
// Stance Change, Disguise, Ice Face.

/// Stance Change's `onModifyMove` (priority 1): Aegislash takes its Blade
/// Forme to attack and its Shield Forme for King's Shield.
fn stance_change(battle: &mut Battle, ev: &EventCtx<'_>, holder: Position) {
    let m = mon(battle, holder);
    if base_of(battle, m.species) != s::AEGISLASH || transformed(m) {
        return;
    }
    let (Some(move_id), Some(data)) = (ev.effect.move_id(), ev.move_data()) else { return };
    if data.category == MoveCategory::Status && move_id != m::KINGS_SHIELD {
        return;
    }
    let target = if move_id == m::KINGS_SHIELD { s::AEGISLASH } else { s::AEGISLASH_BLADE };
    if m.species != target {
        let source = implicit_source(battle, a::STANCE_CHANGE);
        forme::forme_change(battle, holder, target, source, false, false);
    }
}

/// Disguise's `onEffectiveness`: a damaging move is neutral against an
/// undisguised Mimikyu. Champions inherits the base ability.
fn disguise_effectiveness(battle: &mut Battle, ev: &EventCtx<'_>, holder: Position) -> Option<Relay> {
    let data = *ev.move_data()?;
    if data.category == MoveCategory::Status {
        return None;
    }
    if !matches!(mon(battle, holder).species, s::MIMIKYU | s::MIMIKYU_TOTEM) {
        return None;
    }
    if hits_substitute(battle, holder, &data) || !move_immunity(battle, holder, ev.effect.move_id(), &data) {
        return None;
    }
    Some(Relay::Num(0))
}

/// Ice Face's `onStart` / `onWeatherChange`: in snow, Eiscue's Noice Face
/// becomes its Ice Face again.
fn ice_face_restore(battle: &mut Battle, holder: Position) {
    if abilities::effective_weather(battle) == Some(Weather::Snow) && mon(battle, holder).species == s::EISCUE_NOICE {
        abilities::activate(battle, holder, a::ICE_FACE);
        ability_state_mut(battle, holder).busted = false;
        forme::forme_change(battle, holder, s::EISCUE, Effect::Ability(a::ICE_FACE), true, false);
    }
}

// ---------------------------------------------------------------------------
// HP-based formes.

/// Shields Down's `onStart` / `onResidual`: Minior is in its Meteor Form
/// above half HP, in its Core form at half or below.
fn shields_down(battle: &mut Battle, holder: Position, residual: bool) {
    let m = mon(battle, holder);
    if base_of(battle, m.base_species) != s::MINIOR || transformed(m) || residual && m.hp == 0 {
        return;
    }
    let meteor = battle.format.dex.species(m.species).forme == "Meteor";
    let effect = implicit_source(battle, a::SHIELDS_DOWN);
    if u32::from(m.hp) * 2 > u32::from(m.max_hp()) {
        if !meteor {
            forme::forme_change(battle, holder, s::MINIOR_METEOR, effect, false, false);
        }
    } else if meteor {
        // `formeChange(pokemon.set.species)`.
        let core = m.set_species;
        forme::forme_change(battle, holder, core, effect, false, false);
    }
}

/// Schooling's `onStart` / `onResidual`: Wishiwashi (level 20 or more) is
/// in its School Form above a quarter of its HP.
fn schooling(battle: &mut Battle, holder: Position, residual: bool) {
    let m = mon(battle, holder);
    if base_of(battle, m.base_species) != s::WISHIWASHI || m.level < 20 || transformed(m) || residual && m.hp == 0 {
        return;
    }
    let effect = implicit_source(battle, a::SCHOOLING);
    if u32::from(m.hp) * 4 > u32::from(m.max_hp()) {
        if m.species == s::WISHIWASHI {
            forme::forme_change(battle, holder, s::WISHIWASHI_SCHOOL, effect, false, false);
        }
    } else if m.species == s::WISHIWASHI_SCHOOL {
        forme::forme_change(battle, holder, s::WISHIWASHI, effect, false, false);
    }
}

/// The standard forme of a Zen Mode forme (`species.battleOnly`).
fn standard_forme(species: SpeciesId) -> Option<SpeciesId> {
    match species {
        s::DARMANITAN_ZEN => Some(s::DARMANITAN),
        s::DARMANITAN_GALAR_ZEN => Some(s::DARMANITAN_GALAR),
        _ => None,
    }
}

/// Zen Mode's `onResidual` (order 29): at half HP or less Darmanitan adds
/// the `zenmode` volatile (its `onStart` takes the Zen forme); above half
/// in its Zen forme, the volatile ends (its `onEnd` reverts).
fn zen_mode_residual(battle: &mut Battle, holder: Position) {
    let m = *mon(battle, holder);
    if base_of(battle, m.base_species) != s::DARMANITAN || transformed(&m) {
        return;
    }
    let zen = standard_forme(m.species).is_some();
    let low = u32::from(m.hp) * 2 <= u32::from(m.max_hp());
    if low && !zen {
        if events::add_volatile_flag(battle, holder, Volatiles::ZEN_MODE, "zenmode", None, Effect::None) {
            zen_mode_start(battle, holder);
        }
    } else if !low && zen {
        if events::add_volatile_flag(battle, holder, Volatiles::ZEN_MODE, "zenmode", None, Effect::None) {
            zen_mode_start(battle, holder);
        }
        if events::remove_volatile_flag(battle, holder, Volatiles::ZEN_MODE) {
            // The volatile's `onEnd`.
            if let Some(standard) = standard_forme(mon(battle, holder).species) {
                forme::forme_change(battle, holder, standard, Effect::None, false, false);
            }
        }
    }
}

/// The `zenmode` volatile's `onStart`.
fn zen_mode_start(battle: &mut Battle, holder: Position) {
    // `species.name.includes('Galar')`.
    let species = mon(battle, holder).species;
    let galar = matches!(species, s::DARMANITAN_GALAR | s::DARMANITAN_GALAR_ZEN);
    let target = if galar { s::DARMANITAN_GALAR_ZEN } else { s::DARMANITAN_ZEN };
    if species != target {
        forme::forme_change(battle, holder, target, Effect::None, false, false);
    }
}

/// Zen Mode's `onEnd`: the volatile goes and Darmanitan takes back its
/// standard forme (`[silent]`).
fn zen_mode_end(battle: &mut Battle, holder: Position) {
    let m = mon(battle, holder);
    if !m.volatiles.has(Volatiles::ZEN_MODE) || m.hp == 0 {
        return;
    }
    let species = m.species;
    // `delete pokemon.volatiles['zenmode']`.
    events::delete_volatile_flag(battle, holder, Volatiles::ZEN_MODE);
    if let Some(standard) = standard_forme(species) {
        // `formeChange(battleOnly, this.effect, false, '0', '[silent]')`.
        forme::set_species(battle, holder, standard);
        let target = actor(battle, holder);
        let cause = Cause::ability(a::ZEN_MODE);
        battle.emit(BattleEvent::FormeChangeSilent { actor: target, species: standard, cause });
    }
}

// ---------------------------------------------------------------------------
// Weather and terrain formes.

/// Forecast's `onWeatherChange` (also its `onStart`): Castform's forme
/// follows its `effectiveWeather()`.
fn forecast(battle: &mut Battle, holder: Position) {
    let m = mon(battle, holder);
    if base_of(battle, m.base_species) != s::CASTFORM || transformed(m) {
        return;
    }
    let species = m.species;
    let target = match abilities::pokemon_weather(battle, holder) {
        Some(Weather::Sun | Weather::HarshSun) => s::CASTFORM_SUNNY,
        Some(Weather::Rain | Weather::HeavyRain) => s::CASTFORM_RAINY,
        Some(Weather::Snow) => s::CASTFORM_SNOWY,
        _ => s::CASTFORM,
    };
    if species != target && events::is_active(battle, holder) {
        forme::forme_change(battle, holder, target, Effect::Ability(a::FORECAST), false, true);
    }
}

/// Flower Gift's `onWeatherChange` (also its `onStart`): Cherrim blooms in
/// the sun.
fn flower_gift(battle: &mut Battle, holder: Position) {
    let m = mon(battle, holder);
    if !events::is_active(battle, holder)
        || base_of(battle, m.base_species) != s::CHERRIM
        || transformed(m)
        || m.hp == 0
    {
        return;
    }
    let species = m.species;
    let sunny = matches!(abilities::pokemon_weather(battle, holder), Some(Weather::Sun | Weather::HarshSun));
    let effect = Effect::Ability(a::FLOWER_GIFT);
    if sunny {
        if species != s::CHERRIM_SUNSHINE {
            forme::forme_change(battle, holder, s::CHERRIM_SUNSHINE, effect, false, true);
        }
    } else if species == s::CHERRIM_SUNSHINE {
        forme::forme_change(battle, holder, s::CHERRIM, effect, false, true);
    }
}

/// Mimicry's `onTerrainChange` (also its `onStart`): the holder takes the
/// terrain's type, or its base species' types without one.
fn mimicry(battle: &mut Battle, holder: Position) {
    let terrain = battle.state.field.terrain;
    let m = *mon(battle, holder);
    let types = match terrain {
        Some(Terrain::Electric) => TypeSet::mono(Type::Electric),
        Some(Terrain::Grassy) => TypeSet::mono(Type::Grass),
        Some(Terrain::Misty) => TypeSet::mono(Type::Fairy),
        Some(Terrain::Psychic) => TypeSet::mono(Type::Psychic),
        None => battle.format.dex.species(m.base_species).types,
    };
    // `oldTypes.join() === types.join()` (the added type included).
    let old = manipulation::all_types(&m);
    if old == [Some(types.primary), types.secondary, None] || !manipulation::set_type(battle, holder, types) {
        return;
    }
    let target = actor(battle, holder);
    if terrain.is_some() || transformed(&m) {
        battle.emit(BattleEvent::TypeChangeFrom { target, types, cause: Cause::ability(a::MIMICRY) });
        if terrain.is_none() {
            battle.hint(crate::event::Hint::TransformMimicry);
        }
    } else {
        abilities::activate(battle, holder, a::MIMICRY);
        battle.emit(BattleEvent::EffectStartEnd {
            actor: target,
            start: false,
            what: LineArg::Text("typechange"),
            effect: None,
            cause: None,
            silent: true,
        });
    }
}

// ---------------------------------------------------------------------------
// Battle Bond, Gulp Missile.

/// Battle Bond's `onSourceAfterFaint`: once per battle, a Greninja-Bond
/// that knocked out a foe with a move gets +1 Attack, Special Attack and
/// Speed.
fn battle_bond(battle: &mut Battle, ev: &EventCtx<'_>, holder: Position) {
    let m = mon(battle, holder);
    if m.bond_triggered || !ev.effect.is_move() {
        return;
    }
    let foes_left = {
        let foe = battle.state.side(holder.side.foe());
        (0..foe.team.len()).any(|i| !foe.team[i].is_fainted() || foe.is_active(i as u8))
    };
    if m.species == s::GRENINJA_BOND && m.hp > 0 && !transformed(m) && foes_left {
        let changes = StatBoosts::new([1, 0, 1, 0, 1, 0, 0]);
        boosts::boost_from(battle, holder, changes, Some(holder), Effect::Ability(a::BATTLE_BOND), false, false);
        abilities::activate(battle, holder, a::BATTLE_BOND);
        mon_mut(battle, holder).bond_triggered = true;
    }
}

/// Gulp Missile's `onDamagingHit`: Cramorant with prey spits it at the
/// attacker (a quarter of its max HP, then -1 Defense or paralysis) and
/// goes back to its base forme.
fn gulp_missile_hit(battle: &mut Battle, ev: &EventCtx<'_>, holder: Position) {
    let Some(source) = ev.source else { return };
    if !events::is_active(battle, source)
        || mon(battle, source).hp == 0
        || crate::resolve::multiturn::is_semi_invulnerable(battle, holder)
    {
        return;
    }
    let species = mon(battle, holder).species;
    if !matches!(species, s::CRAMORANT_GULPING | s::CRAMORANT_GORGING) {
        return;
    }
    let amount = crate::resolve::hp_fraction(u32::from(mon(battle, source).max_hp()), 1, 4);
    crate::resolve::damage_by(battle, source, amount, Some(holder), Effect::Ability(a::GULP_MISSILE));
    if species == s::CRAMORANT_GULPING {
        let def = StatBoosts::new([0, -1, 0, 0, 0, 0, 0]);
        boosts::boost_from(battle, source, def, Some(holder), Effect::Ability(a::GULP_MISSILE), true, false);
    } else {
        crate::resolve::status::set_status_from(battle, source, StatusId::Paralysis, Some(holder), ev.effect, true);
    }
    forme::forme_change(battle, holder, s::CRAMORANT, ev.effect, false, false);
}

/// Dive's `onTryMove` on its charging turn: a Cramorant with Gulp Missile
/// catches prey.
pub(crate) fn gulp_missile_dive(battle: &mut Battle, user: Position) {
    let m = mon(battle, user);
    if abilities::has_ability(battle, user, a::GULP_MISSILE) && m.species == s::CRAMORANT && !transformed(m) {
        let forme =
            if u32::from(m.hp) * 2 <= u32::from(m.max_hp()) { s::CRAMORANT_GORGING } else { s::CRAMORANT_GULPING };
        forme::forme_change(battle, user, forme, Effect::Move(m::DIVE), false, false);
    }
}

/// Tera Shell in `Pokemon#runEffectiveness`: Terapagos-Terastal at full HP
/// takes a damaging hit as resisted (`-activate`), and the rest of the
/// move's hits too. `total` is the effectiveness so far.
pub(crate) fn tera_shell(battle: &mut Battle, target: Position, move_id: MoveId, data: &MoveData, total: i8) -> i8 {
    let m = *mon(battle, target);
    // `this.species` (a Pokémon transformed into Terapagos-Terastal).
    if crate::resolve::tera::species_of(&m) != s::TERAPAGOS_TERASTAL
        || !abilities::has_ability(battle, target, a::TERA_SHELL)
        || abilities::suppressing_ability(battle, target)
    {
        return total;
    }
    if battle.move_state.hit_number == 1 {
        ability_state_mut(battle, target).resisted = false;
    }
    if ability_state(battle, target).resisted {
        return -1;
    }
    let (hp, max_hp) = (m.hp, m.max_hp());
    if data.category == MoveCategory::Status
        || move_id == m::STRUGGLE
        || !move_immunity(battle, target, Some(move_id), data)
        || total < 0
        || hp < max_hp
    {
        return total;
    }
    let holder = actor(battle, target);
    let effect = EffectName::Ability(a::TERA_SHELL);
    battle.emit(BattleEvent::ActivateEffect { actor: holder, effect, args: [None, None], of: None });
    ability_state_mut(battle, target).resisted = true;
    -1
}
