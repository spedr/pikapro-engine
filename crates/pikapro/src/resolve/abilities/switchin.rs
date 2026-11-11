//! Switch-in, field, forme and ability-suppressing abilities.
//!
//! Showdown references: the abilities in `data/abilities.ts` that
//! `abilities::owner` assigns to [`Owner::Switchin`](super::Owner::Switchin)
//! (Intimidate, the weather and terrain setters, the forme changers,
//! Neutralizing Gas, Trace, Illusion, Commander, ...), and
//! `pokemon.formeChange` / `setAbility` (`resolve/forme.rs`,
//! `abilities::set_ability`).
//!
//! Adding an ability: see `abilities/mod.rs`. One arm per callback in
//! [`run`]; this module's volatiles (bits 66 and 68..=79) get their handlers in
//! [`volatile_spec`]. The forme changers are in [`forme_abilities`], the
//! abilities that change stats or power from the field in
//! [`modifier_abilities`]; `run` tries them after its own arms.

mod forme_abilities;
mod modifier_abilities;

pub(crate) use modifier_abilities::booster_energy;

pub(crate) use forme_abilities::{gulp_missile_dive, tera_shell};

use pikapro_data::gen9::{abilities as a, items as i, species};
use pikapro_data::{AbilityFlags, AbilityId, Event, EventPrefix, MoveCategory, Stat, StatBoosts, Type, TypeSet};

use crate::battle::Battle;
use crate::event::{AbilityNote, BattleEvent, Cause, EffectName, LineArg};
use crate::format::Mechanics;
use crate::resolve::events::{self, Effect, EventCtx, Relay, Spec};
use crate::resolve::{actor, boosts, field, manipulation, queue};
use crate::state::{PokemonState, Position, SideCondition, Status, Terrain, Volatiles, Weather};

/// Runs `ability`'s callback for `ev.event` / `ev.prefix` on its holder.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, ability: AbilityId) -> Option<Relay> {
    use EventPrefix::{Any, Foe, On};
    match (ability, ev.event, ev.prefix) {
        (a::INTIMIDATE, Event::Start, On) => intimidate(battle, holder),
        // Weather and terrain setters (`onStart`: `field.setWeather`,
        // `field.setTerrain`).
        (a::DROUGHT, Event::Start, On) => {
            // Primal Groudon's Desolate Land takes over
            // (`source.species.id === 'groudon'`: the species it
            // transformed into).
            let mon = mon(battle, holder);
            if !(crate::resolve::tera::species_of(mon) == species::GROUDON && mon.item == Some(i::RED_ORB)) {
                field::set_weather_from(battle, Weather::Sun, Some(holder), Effect::Ability(ability));
            }
            None
        }
        (a::DRIZZLE, Event::Start, On) => {
            let mon = mon(battle, holder);
            if !(crate::resolve::tera::species_of(mon) == species::KYOGRE && mon.item == Some(i::BLUE_ORB)) {
                field::set_weather_from(battle, Weather::Rain, Some(holder), Effect::Ability(ability));
            }
            None
        }
        (a::SAND_STREAM, Event::Start, On) => {
            field::set_weather_from(battle, Weather::Sand, Some(holder), Effect::Ability(ability));
            None
        }
        (a::SNOW_WARNING, Event::Start, On) => {
            field::set_weather_from(battle, Weather::Snow, Some(holder), Effect::Ability(ability));
            None
        }
        (a::ELECTRIC_SURGE | a::GRASSY_SURGE | a::MISTY_SURGE | a::PSYCHIC_SURGE, Event::Start, On) => {
            let terrain = match ability {
                a::ELECTRIC_SURGE => Terrain::Electric,
                a::GRASSY_SURGE => Terrain::Grassy,
                a::MISTY_SURGE => Terrain::Misty,
                _ => Terrain::Psychic,
            };
            field::set_terrain_from(battle, terrain, holder, Effect::Ability(ability));
            None
        }
        // Abilities that announce themselves (`onStart`: `-ability`).
        (a::PRESSURE | a::MOLD_BREAKER | a::TERAVOLT | a::TURBOBLAZE, Event::Start, On) => {
            super::show(battle, holder, ability);
            None
        }
        // Pressure's `onDeductPP`: one more PP from a foe's move.
        (a::PRESSURE, Event::DeductPP, On) => {
            let source = ev.source?;
            (source.side != holder.side).then_some(Relay::Num(1))
        }
        // Mold Breaker's, Teravolt's and Turboblaze's `onModifyMove`.
        (a::MOLD_BREAKER | a::TERAVOLT | a::TURBOBLAZE, Event::ModifyMove, On) => {
            battle.move_state.ignore_ability = true;
            None
        }
        // Unnerve: announced once per `abilityState`; foes can't eat
        // berries (`onFoeTryEatItem`).
        (a::UNNERVE, Event::Start, On) => {
            if !ability_state(battle, holder).unnerved {
                super::show(battle, holder, ability);
                ability_state_mut(battle, holder).unnerved = true;
            }
            None
        }
        (a::UNNERVE, Event::End, On) => {
            ability_state_mut(battle, holder).unnerved = false;
            None
        }
        (a::UNNERVE, Event::TryEatItem, Foe) => Some(Relay::from_bool(!ability_state(battle, holder).unnerved)),
        (a::FRISK, Event::Start, On) => {
            frisk(battle, holder);
            None
        }
        (a::ANTICIPATION, Event::Start, On) => {
            anticipation(battle, holder);
            None
        }
        (a::FOREWARN, Event::Start, On) => {
            forewarn(battle, holder);
            None
        }
        (a::DOWNLOAD, Event::Start, On) => {
            download(battle, holder);
            None
        }
        (a::CURIOUS_MEDICINE, Event::Start, On) => {
            for ally in adjacent_allies(battle, holder).iter().copied() {
                battle.state.side_mut(ally.side).team[ally.slot as usize].boosts = Default::default();
                let target = actor(battle, ally);
                let cause = Cause::ability(ability).of(actor(battle, holder));
                battle.emit(BattleEvent::ClearBoostFrom { target, cause });
            }
            None
        }
        (a::HOSPITALITY, Event::Start, On) => {
            for ally in adjacent_allies(battle, holder).iter().copied() {
                let amount = crate::resolve::hp_fraction(u32::from(mon(battle, ally).max_hp()), 1, 4);
                crate::resolve::heal_by(battle, ally, amount, Some(holder), Effect::Ability(ability));
            }
            None
        }
        (a::SUPERSWEET_SYRUP, Event::Start, On) => {
            supersweet_syrup(battle, holder);
            None
        }
        (a::SUPREME_OVERLORD, Event::Start, On) => {
            let fallen = battle.state.side(holder.side).total_fainted.min(5);
            if fallen > 0 {
                super::activate(battle, holder, ability);
                const FALLEN: [&str; 6] = ["", "fallen1", "fallen2", "fallen3", "fallen4", "fallen5"];
                let target = actor(battle, holder);
                battle.emit(BattleEvent::EffectStartEnd {
                    actor: target,
                    start: true,
                    what: LineArg::Text(FALLEN[fallen as usize]),
                    effect: None,
                    cause: None,
                    silent: true,
                });
                ability_state_mut(battle, holder).fallen = fallen;
            }
            None
        }
        (a::SUPREME_OVERLORD, Event::End, On) => {
            // `-end|<pokemon>|fallen${fallen}|[silent]`, `fallenundefined`
            // when it never started.
            const FALLEN: [&str; 6] = ["fallenundefined", "fallen1", "fallen2", "fallen3", "fallen4", "fallen5"];
            let fallen = ability_state(battle, holder).fallen.min(5);
            let target = actor(battle, holder);
            battle.emit(BattleEvent::EffectStartEnd {
                actor: target,
                start: false,
                what: LineArg::Text(FALLEN[fallen as usize]),
                effect: None,
                cause: None,
                silent: true,
            });
            None
        }
        // `onBasePower` (priority 21).
        (a::SUPREME_OVERLORD, Event::BasePower, On) => {
            const POW_MOD: [u32; 6] = [4096, 4506, 4915, 5325, 5734, 6144];
            let fallen = ability_state(battle, holder).fallen.min(5);
            if fallen > 0 {
                ev.chain(POW_MOD[fallen as usize]);
            }
            None
        }
        (a::INTREPID_SWORD, Event::Start, On) => {
            let mon = mon_mut(battle, holder);
            if !mon.sword_boost {
                mon.sword_boost = true;
                let atk = StatBoosts::new([1, 0, 0, 0, 0, 0, 0]);
                boosts::boost_from(battle, holder, atk, None, Effect::Ability(ability), false, false);
            }
            None
        }
        (a::DAUNTLESS_SHIELD, Event::Start, On) => {
            let mon = mon_mut(battle, holder);
            if !mon.shield_boost {
                mon.shield_boost = true;
                let def = StatBoosts::new([0, 1, 0, 0, 0, 0, 0]);
                boosts::boost_from(battle, holder, def, None, Effect::Ability(ability), false, false);
            }
            None
        }
        (a::SCREEN_CLEANER, Event::Start, On) => {
            screen_cleaner(battle, holder);
            None
        }
        // Klutz's `onStart`: the item's `End` (suppressed by Klutz itself
        // unless the item ignores Klutz).
        (a::KLUTZ, Event::Start, On) => {
            if let Some(item) = mon(battle, holder).item {
                events::single_event_item(battle, Event::End, holder, item, None, Effect::None, Relay::True);
            }
            None
        }
        // Air Lock and Cloud Nine: `onSwitchIn` announces them, `onStart`
        // and `onEnd` tell the field the weather changed.
        (a::AIR_LOCK | a::CLOUD_NINE, Event::SwitchIn, On) => {
            super::show(battle, holder, ability);
            weather_suppressor(battle, holder, ability, false);
            None
        }
        (a::AIR_LOCK | a::CLOUD_NINE, Event::Start, On) => {
            weather_suppressor(battle, holder, ability, false);
            None
        }
        (a::AIR_LOCK | a::CLOUD_NINE, Event::End, On) => {
            weather_suppressor(battle, holder, ability, true);
            None
        }
        (a::NATURAL_CURE, Event::CheckShow, On) => {
            if battle.format.mechanics != Mechanics::Champions {
                natural_cure_check_show(battle, holder);
            }
            None
        }
        (a::NATURAL_CURE, Event::SwitchOut, On) => {
            natural_cure_switch_out(battle, holder);
            None
        }
        (a::REGENERATOR, Event::SwitchOut, On) => {
            regenerator(battle, holder);
            None
        }
        // Imposter's `onSwitchIn`: it transforms into the foe facing it.
        (a::IMPOSTER, Event::SwitchIn, On) => {
            let n = battle.format.active_per_side() as u8;
            let target = Position::new(holder.side.foe(), n - 1 - holder.slot);
            crate::resolve::order::transform_into(battle, holder, target, Some(a::IMPOSTER));
            None
        }
        // The primal weathers.
        (a::DESOLATE_LAND | a::PRIMORDIAL_SEA | a::DELTA_STREAM, Event::Start, On) => {
            let weather = match ability {
                a::DESOLATE_LAND => Weather::HarshSun,
                a::PRIMORDIAL_SEA => Weather::HeavyRain,
                _ => Weather::StrongWinds,
            };
            field::set_weather_from(battle, weather, Some(holder), Effect::Ability(ability));
            None
        }
        (a::DESOLATE_LAND, Event::SetWeather, Any) => primal_blocks(battle, Weather::HarshSun, ev),
        (a::PRIMORDIAL_SEA, Event::SetWeather, Any) => primal_blocks(battle, Weather::HeavyRain, ev),
        (a::DELTA_STREAM, Event::SetWeather, Any) => primal_blocks(battle, Weather::StrongWinds, ev),
        (a::DESOLATE_LAND | a::PRIMORDIAL_SEA | a::DELTA_STREAM, Event::End, On) => {
            primal_end(battle, holder, ability);
            None
        }
        // Commander (`onAnySwitchIn`, priority -2; `onStart`; `onUpdate`).
        (a::COMMANDER, Event::SwitchIn, Any) | (a::COMMANDER, Event::Start | Event::Update, On) => {
            commander_update(battle, holder);
            None
        }
        // Trace.
        (a::TRACE, Event::Start, On) => {
            trace_start(battle, holder);
            None
        }
        (a::TRACE, Event::Update, On) => {
            trace_update(battle, holder);
            None
        }
        // Neutralizing Gas (`onSwitchIn`, priority 2; `onEnd`).
        (a::NEUTRALIZING_GAS, Event::SwitchIn, On) => {
            neutralizing_gas_switch_in(battle, holder);
            None
        }
        (a::NEUTRALIZING_GAS, Event::End, On) => {
            neutralizing_gas_end(battle, holder);
            None
        }
        // Illusion.
        (a::ILLUSION, Event::BeforeSwitchIn, On) => {
            illusion_before_switch_in(battle, holder);
            None
        }
        (a::ILLUSION, Event::DamagingHit, On) => {
            if mon(battle, holder).illusion.is_some() {
                illusion_end(battle, holder);
            }
            None
        }
        (a::ILLUSION, Event::End, On) => {
            illusion_end(battle, holder);
            None
        }
        (a::ILLUSION, Event::Faint, On) => {
            mon_mut(battle, holder).illusion = None;
            None
        }
        _ => match forme_abilities::run(battle, ev, holder, ability) {
            Some(relay) => relay,
            None => modifier_abilities::run(battle, ev, holder, ability).flatten(),
        },
    }
}

// ---------------------------------------------------------------------------
// Commander.

/// Commander's `onUpdate` (also its `onStart` and `onAnySwitchIn`): in
/// doubles, a Tatsugiri next to a Dondozo goes inside it (`commanding`; its
/// queued action is cancelled) and the Dondozo is `commanded` (+2 to every
/// stat); once the Dondozo is gone, Tatsugiri comes out.
fn commander_update(battle: &mut Battle, pos: Position) {
    if battle.format.active_per_side() != 2 {
        return;
    }
    if matches!(battle.queue.peek().map(|a| a.kind), Some(queue::ActionKind::RunSwitch)) {
        return;
    }
    let dex = battle.format.dex;
    let other = Position::new(pos.side, 1 - pos.slot);
    let ally = (events::is_active(battle, other) && mon(battle, other).hp > 0).then_some(other);
    let flagged = |p: Position| mon(battle, p).switch_flag != crate::state::SwitchFlag::None;
    if flagged(pos) || ally.is_some_and(flagged) {
        return;
    }
    let base = |p: Position| dex.species(mon(battle, p).base_species).base_species;
    let Some(ally) = ally.filter(|&al| base(pos) == species::TATSUGIRI && base(al) == species::DONDOZO) else {
        if mon(battle, pos).volatiles.has(Volatiles::COMMANDING) {
            events::remove_volatile_flag(battle, pos, Volatiles::COMMANDING);
        }
        return;
    };
    if mon(battle, pos).volatiles.has(Volatiles::COMMANDING) {
        // `if (!ally.fainted) return`: the ally has HP here.
        return;
    }
    if mon(battle, ally).volatiles.has(Volatiles::COMMANDED) {
        return;
    }
    // `queue.cancelAction(pokemon)`.
    let me = queue::MonRef { side: pos.side, id: mon(battle, pos).id };
    battle.queue.retain(|a| a.mon != Some(me));
    let (holder, of) = (actor(battle, pos), actor(battle, ally));
    let effect = EffectName::Ability(a::COMMANDER);
    battle.emit(BattleEvent::ActivateEffect { actor: holder, effect, args: [None, None], of: Some(of) });
    let effect = Effect::Ability(a::COMMANDER);
    events::add_volatile_flag(battle, pos, Volatiles::COMMANDING, "commanding", None, effect);
    if events::add_volatile_flag(battle, ally, Volatiles::COMMANDED, "commanded", Some(pos), effect) {
        mon_mut(battle, ally).volatile_counters.commanded_by = Some(me.id);
        // `commanded`'s `onStart`: `{atk: 2, spa: 2, spe: 2, def: 2, spd: 2}`,
        // its source the event's, the Tatsugiri.
        let changes = StatBoosts::ordered([2, 2, 2, 2, 2, 0, 0], [0, 2, 4, 1, 3, 5, 6]);
        let effect = Effect::volatile(Volatiles::COMMANDED);
        boosts::boost_from(battle, ally, changes, Some(pos), effect, false, false);
    }
}

/// `commanding`'s and `commanded`'s `onDragOut` (priority 2) and
/// `commanding`'s `onInvulnerability`: `false`.
fn commander_false(_: &mut Battle, _: &mut EventCtx<'_>, _: &crate::resolve::handlers::Handler) -> Option<Relay> {
    Some(Relay::False)
}

/// `commanding`'s and `commanded`'s `onTrapPokemon` (priority -11).
fn commander_trap(battle: &mut Battle, _: &mut EventCtx<'_>, h: &crate::resolve::handlers::Handler) -> Option<Relay> {
    if let crate::resolve::handlers::Holder::Pokemon(pos) = h.holder {
        mon_mut(battle, pos).trapped = true;
    }
    None
}

/// Order Up's `onAfterMoveSecondarySelf`: a commanded Dondozo gets +1 to
/// the stat of the Tatsugiri inside's forme (Droopy: Defense, Stretchy:
/// Speed, otherwise Attack).
pub(crate) fn order_up(battle: &mut Battle, user: Position) {
    let m = mon(battle, user);
    if !m.volatiles.has(Volatiles::COMMANDED) {
        return;
    }
    let side = battle.state.side(user.side);
    let Some(tatsugiri) = m.volatile_counters.commanded_by.and_then(|id| side.team.iter().find(|t| t.id == id)) else {
        return;
    };
    let base = tatsugiri.base_species;
    if battle.format.dex.species(base).base_species != species::TATSUGIRI {
        return;
    }
    let stages = match battle.format.dex.species(base).forme {
        "Droopy" => [0, 1, 0, 0, 0, 0, 0],
        "Stretchy" => [0, 0, 0, 0, 1, 0, 0],
        _ => [1, 0, 0, 0, 0, 0, 0],
    };
    boosts::boost(battle, user, StatBoosts::new(stages), Some(user), false, false);
}

// ---------------------------------------------------------------------------
// Trace.

/// Trace's `onStart`: it seeks unless a foe has no ability or it holds
/// Ability Shield (`-block`), then looks (`Update`).
fn trace_start(battle: &mut Battle, holder: Position) {
    let mut seek = true;
    if super::foes(battle, holder).iter().any(|&p| mon(battle, p).ability == a::NO_ABILITY) {
        seek = false;
    }
    if crate::resolve::items::has_item(battle, holder, i::ABILITY_SHIELD) {
        let target = actor(battle, holder);
        battle.emit(BattleEvent::Block {
            actor: target,
            effect: EffectName::Item(i::ABILITY_SHIELD),
            arg: None,
            of: None,
        });
        seek = false;
    }
    ability_state_mut(battle, holder).seek = seek;
    if seek {
        trace_update(battle, holder);
    }
}

/// Trace's `onUpdate`: while seeking, it copies the ability of a random
/// adjacent foe whose ability can be traced (`-ability|<holder>|<ability>|Trace|[from]
/// ability: Trace|[of] <foe>`).
fn trace_update(battle: &mut Battle, holder: Position) {
    if !ability_state(battle, holder).seek {
        return;
    }
    let dex = battle.format.dex;
    let targets: crate::util::StackVec<Position, 2> = super::foes(battle, holder)
        .iter()
        .copied()
        .filter(|&p| {
            let ability = mon(battle, p).ability;
            !dex.ability(ability).flags.contains(AbilityFlags::NO_TRACE) && ability != a::NO_ABILITY
        })
        .collect();
    if targets.is_empty() {
        return;
    }
    let target = *battle.rng.sample(&targets);
    let ability = mon(battle, target).ability;
    let effect = Some(EffectName::Ability(a::TRACE));
    super::set_ability(battle, holder, ability, Some(target), effect, super::SetAbilityKind::Normal);
}

// ---------------------------------------------------------------------------
// Neutralizing Gas.

/// Neutralizing Gas's `onSwitchIn`: `-ability`, then for every active
/// Pokémon: Ability Shield blocks it (`-block`), a commanding Tatsugiri is
/// left alone, an Illusion ends, and a primal weather ability ends.
fn neutralizing_gas_switch_in(battle: &mut Battle, holder: Position) {
    super::show(battle, holder, a::NEUTRALIZING_GAS);
    ability_state_mut(battle, holder).ending = false;
    let active: crate::util::StackVec<Position, 4> = super::all_active(battle).collect();
    for target in active.iter().copied() {
        if crate::resolve::items::has_item(battle, target, i::ABILITY_SHIELD) {
            let actor = actor(battle, target);
            let effect = EffectName::Item(i::ABILITY_SHIELD);
            battle.emit(BattleEvent::Block { actor, effect, arg: None, of: None });
            continue;
        }
        let m = mon(battle, target);
        if m.volatiles.has(Volatiles::COMMANDING) {
            continue;
        }
        if m.illusion.is_some() {
            illusion_end(battle, target);
        }
        let ability = mon(battle, target).ability;
        if matches!(ability, a::DESOLATE_LAND | a::PRIMORDIAL_SEA | a::DELTA_STREAM) {
            let effect = Effect::Ability(a::NEUTRALIZING_GAS);
            events::single_event_ability(battle, Event::End, target, ability, Some(holder), effect, Relay::True);
        }
    }
}

/// Neutralizing Gas's `onEnd`: unless another Neutralizing Gas is out,
/// `-end|<holder>|ability: Neutralizing Gas`, then every other active
/// Pokémon's ability starts again, fastest first (a speed sort that draws
/// on ties).
fn neutralizing_gas_end(battle: &mut Battle, holder: Position) {
    if mon(battle, holder).volatiles.has(Volatiles::TRANSFORMED) {
        return;
    }
    let active: crate::util::StackVec<Position, 4> = super::all_active(battle).collect();
    if active.iter().any(|&p| p != holder && super::has_ability(battle, p, a::NEUTRALIZING_GAS)) {
        return;
    }
    let target = actor(battle, holder);
    let effect = Some(EffectName::Ability(a::NEUTRALIZING_GAS));
    let what = LineArg::Text("");
    battle.emit(BattleEvent::EffectStartEnd { actor: target, start: false, what, effect, cause: None, silent: false });
    if ability_state(battle, holder).ending {
        return;
    }
    ability_state_mut(battle, holder).ending = true;
    let mut sorted: crate::util::StackVec<(Position, i32), 4> =
        active.iter().map(|&p| (p, mon(battle, p).speed)).collect();
    queue::speed_sort_by_speed(&mut sorted, &mut battle.rng);
    let dex = battle.format.dex;
    for (pos, _) in sorted.iter().copied() {
        if pos == holder {
            continue;
        }
        let ability = mon(battle, pos).ability;
        if dex.ability(ability).flags.contains(AbilityFlags::CANT_SUPPRESS)
            || crate::resolve::items::has_item(battle, pos, i::ABILITY_SHIELD)
        {
            continue;
        }
        events::single_event_ability(battle, Event::Start, pos, ability, None, Effect::None, Relay::True);
        // `if (pokemon.ability === 'gluttony') pokemon.abilityState.gluttony
        // = false`.
        if ability == a::GLUTTONY {
            battle.state.side_mut(pos.side).team[pos.slot as usize].volatile_counters.ability_state.gluttony = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Illusion.

/// The species others see for `mon` (on `side`): its Illusion's, if it has
/// one (Showdown's `pokemon.illusion` in `toString` and `getFullDetails`).
/// A disguise as a transformed teammate shows that teammate's name but its
/// transformed species in `details` ([`seen_details`]).
pub(crate) fn seen_species(side: &crate::state::SideState, mon: &PokemonState) -> pikapro_data::SpeciesId {
    match mon.illusion {
        Some(id) => side.team.iter().find(|m| m.id == id).map_or(mon.species, |m| m.species),
        None => mon.species,
    }
}

/// The stable id of the Pokémon whose name is shown for `mon`: its
/// disguise's under Illusion (`illusion.fullname`), else its own.
pub(crate) fn seen_id(side: &crate::state::SideState, mon: &PokemonState) -> u8 {
    match mon.illusion {
        Some(id) if side.team.iter().any(|m| m.id == id) => id,
        _ => mon.id,
    }
}

/// The species an Illusion's `details` show when it is not
/// [`seen_species`]'s: its disguise is a transformed teammate (Showdown's
/// `illusion.getUpdatedDetails()` reads `species`).
pub(crate) fn seen_details(side: &crate::state::SideState, mon: &PokemonState) -> Option<pikapro_data::SpeciesId> {
    let seen = side.team.iter().find(|m| Some(m.id) == mon.illusion)?;
    if !seen.volatiles.has(Volatiles::TRANSFORMED) {
        return None;
    }
    seen.volatile_counters.transform.map(|t| t.species)
}

/// Whether the Pokémon at `pos` is disguised by Illusion.
pub(crate) fn has_illusion(battle: &Battle, pos: Position) -> bool {
    battle.state.side(pos.side).team.get(pos.slot as usize).is_some_and(|m| m.illusion.is_some())
}

/// Illusion's `onBeforeSwitchIn`: the holder looks like the last teammate
/// that has not fainted, if it comes after it in the party (not Ogerpon or
/// Terapagos for a terastallized holder).
fn illusion_before_switch_in(battle: &mut Battle, holder: Position) {
    let dex = battle.format.dex;
    let side = battle.state.side(holder.side);
    let me = &side.team[holder.slot as usize];
    let mut illusion = None;
    for i in (holder.slot as usize + 1..side.team.len()).rev() {
        let target = &side.team[i];
        if !target.is_fainted() {
            // `possibleTarget.species`: a teammate transformed into one counts.
            let base = dex.species(crate::resolve::tera::species_of(target)).base_species;
            if !me.terastallized || !matches!(base, species::OGERPON | species::TERAPAGOS) {
                illusion = Some(target.id);
            }
            break;
        }
    }
    mon_mut(battle, holder).illusion = illusion;
}

/// Illusion's `onEnd` (also run by `onDamagingHit`): unless it is switching
/// out, the disguise drops (`replace|<pokemon>|<details>`, then
/// `-end|<pokemon>|Illusion`).
pub(crate) fn illusion_end(battle: &mut Battle, holder: Position) {
    let m = mon(battle, holder);
    if m.illusion.is_none() || m.being_called_back {
        return;
    }
    let (species, level, gender) = (m.species, m.level, m.gender);
    mon_mut(battle, holder).illusion = None;
    let target = actor(battle, holder);
    battle.emit(BattleEvent::Replace { actor: target, species, level, gender });
    let what = LineArg::Text("Illusion");
    battle.emit(BattleEvent::EffectStartEnd {
        actor: target,
        start: false,
        what,
        effect: None,
        cause: None,
        silent: false,
    });
    // `this.hint('Illusion Level Mod is active, ...', true)`: once per
    // battle.
    if battle.format.illusion_level_mod && !battle.state.field.illusion_level_hint {
        battle.state.field.illusion_level_hint = true;
        battle.hint(crate::event::Hint::IllusionLevelMod);
    }
}

/// `endTurn`'s type reveal for a Pokémon disguised by Illusion (not
/// terastallized itself): the Pokémon it looks like (`seenPokemon`) is
/// compared, its types (its Tera type if it terastallized) against its
/// `apparentType`; a difference is shown on the disguised Pokémon
/// (`-start|...|typechange|<types>|[silent]`, then its added type).
pub(crate) fn illusion_type_reveal(battle: &mut Battle, pos: Position) {
    let m = mon(battle, pos);
    let Some(id) = m.illusion else { return };
    if m.terastallized {
        return;
    }
    let added = m.volatile_counters.added_type;
    let dex = battle.format.dex;
    let side = battle.state.side(pos.side);
    let Some(index) = side.team.iter().position(|t| t.id == id) else { return };
    let seen = &side.team[index];
    let on_field = index < battle.format.active_per_side() && side.active[index].is_some();
    let species_types = dex.species(seen.species).types;
    let c = &seen.volatile_counters;
    let tera = seen.terastallized.then(|| TypeSet::mono(seen.tera_type));
    let (real, differs) = if on_field {
        // On the field its `apparentType` follows its types, unless it lags
        // (Reflect Type, a Tera type copied by Transform), shows the Tera
        // type it terastallized into here, or was revealed by an Illusion
        // before it came in; a Terastallized Pokémon that came in shows its
        // species' types (`setSpecies` on switching out).
        // (Stellar keeps its own types.)
        let real = seen.tera_types().unwrap_or(seen.types);
        let shown_tera = tera.filter(|_| c.tera_apparent);
        let fallback = if tera.is_some() { species_types } else { real };
        let apparent = c.apparent_types.or(shown_tera).or(seen.illusion_apparent).unwrap_or(fallback);
        (real, c.apparent_type_stale || apparent != real)
    } else {
        // On the bench: revealed by an earlier Illusion, else its species'
        // types (`setSpecies` on switching out).
        let real = seen.tera_types().unwrap_or(species_types);
        (real, seen.illusion_apparent.unwrap_or(species_types) != real)
    };
    if !differs {
        return;
    }
    // `seenPokemon.apparentType = realTypeString`.
    let seen = &mut battle.state.side_mut(pos.side).team[index];
    if on_field {
        seen.volatile_counters.apparent_type_stale = false;
        seen.volatile_counters.apparent_types = None;
        // Its `apparentType` is no longer its Tera type (Stellar,
        // which Transform would otherwise copy).
        seen.volatile_counters.tera_apparent = false;
    }
    seen.illusion_apparent = Some(real);
    let target = actor(battle, pos);
    battle.emit(BattleEvent::TypeChange { target, types: real, silent: true });
    if let Some(ty) = added {
        battle.emit(BattleEvent::TypeAdd { target, ty, move_id: None });
    }
}

/// Showdown's `terastallize` for Morpeko: a terastallized Hangry Morpeko
/// stays Hangry (`baseSpecies = species`, `formeRegression`).
pub(crate) fn terastallized(battle: &mut Battle, pos: Position) {
    let dex = battle.format.dex;
    let m = mon_mut(battle, pos);
    // `apparentType = type`.
    m.illusion_apparent = None;
    if dex.species(m.species).base_species == species::MORPEKO
        && !m.volatiles.has(Volatiles::TRANSFORMED)
        && m.base_species != m.species
    {
        m.forme_regression = Some(m.set_species);
        m.base_species = m.species;
        m.base_types = dex.species(m.species).types;
    }
}

/// `faintMessages`' `formeRegression`: a fainted Pokémon's base species
/// and ability go back to its set's (`detailschange|...|[silent]`,
/// `updateMaxHp`), unless it was transformed (`transformed`: before
/// `clearVolatile`).
pub(crate) fn faint_forme_regression(battle: &mut Battle, pos: Position, transformed: bool) {
    // `pokemon.illusion = null`; `clearVolatile` resets `apparentType`.
    mon_mut(battle, pos).illusion = None;
    mon_mut(battle, pos).illusion_apparent = None;
    let Some(species) = mon_mut(battle, pos).forme_regression.take() else { return };
    let data = battle.format.dex.species(species);
    let m = mon_mut(battle, pos);
    if !transformed {
        m.base_species = species;
        // `baseAbility = set.ability`, which `clearVolatile` makes the
        // ability (Ogerpon's and Terapagos's Tera formes).
        m.base_ability = m.set_ability;
        m.ability = m.set_ability;
    }
    let base = m.base_species;
    let data = if base == species { data } else { battle.format.dex.species(base) };
    let m = mon_mut(battle, pos);
    m.species = base;
    m.base_types = data.types;
    m.types = data.types;
    for stat in [Stat::Hp, Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe] {
        let v = crate::stats::calc_stat(stat, data.base_stats[stat], m.ivs[stat], m.evs[stat], m.level, m.nature);
        m.set_stats[stat] = if stat == Stat::Hp { data.max_hp.unwrap_or(v) } else { v };
    }
    m.stats = m.set_stats;
    m.speed = i32::from(m.stats[Stat::Spe]);
    let (level, gender, target) = (m.level, m.gender, actor(battle, pos));
    battle.emit(BattleEvent::DetailsChange { actor: target, species: base, level, tera: None, silent: true, gender });
}

/// `setWeather`'s message when `SetWeather` returns `false` (a primal
/// weather is up): `-fail` for a weather move, `-ability|...|[fail]` for an
/// ability.
pub(crate) fn set_weather_failed(battle: &mut Battle, source: Position, effect: Effect) {
    let Some(current) = battle.state.field.weather else { return };
    let name = match effect {
        Effect::Move(m) if battle.format.dex.move_(m).weather.is_some() => EffectName::Move(m),
        Effect::Ability(x) => EffectName::Ability(x),
        _ => return,
    };
    let source = actor(battle, source);
    battle.emit(BattleEvent::WeatherFail { source, effect: name, weather: current });
}

/// The primal weathers' `onAnySetWeather`: only another primal weather
/// replaces this one.
fn primal_blocks(battle: &Battle, own: Weather, ev: &EventCtx<'_>) -> Option<Relay> {
    let strong = matches!(ev.effect, Effect::Weather(Weather::HarshSun | Weather::HeavyRain | Weather::StrongWinds));
    (battle.state.field.weather == Some(own) && !strong).then_some(Relay::False)
}

/// The primal weathers' `onEnd`: the weather passes to another active
/// Pokémon with the ability, or ends.
fn primal_end(battle: &mut Battle, holder: Position, ability: AbilityId) {
    let id = mon(battle, holder).id;
    if battle.state.field.weather_source != Some((holder.side, id)) {
        return;
    }
    let active: crate::util::StackVec<Position, 4> = super::all_active(battle).collect();
    for target in active.iter().copied() {
        if target != holder && super::has_ability(battle, target, ability) {
            battle.state.field.weather_source = Some((target.side, mon(battle, target).id));
            return;
        }
    }
    clear_weather(battle);
}

/// The `arceus` species condition's `onType` (Gen 8+): an Arceus with
/// Multitype is of its Plate's type, or Normal. Arceus can't lose its Plate
/// or its ability, so its types are set once. (RKS System's Memories are
/// not in Gen 9.)
pub(crate) fn multitype_types(format: &crate::format::Format, set: &crate::team::PokemonSet) -> Option<TypeSet> {
    arceus_types(format, set.species, set.ability, set.item)
}

/// [`multitype_types`] for a Pokémon in battle (its current ability and
/// item): its types while on the field.
pub(crate) fn multitype_types_of(format: &crate::format::Format, mon: &PokemonState) -> Option<TypeSet> {
    arceus_types(format, mon.base_species, mon.ability, mon.item)
}

fn arceus_types(
    format: &crate::format::Format,
    species_id: pikapro_data::SpeciesId,
    ability: pikapro_data::AbilityId,
    item: Option<pikapro_data::ItemId>,
) -> Option<TypeSet> {
    let dex = format.dex;
    if dex.species(species_id).base_species != species::ARCEUS || ability != a::MULTITYPE {
        return None;
    }
    let plate = item.and_then(|it| dex.item(it).plate);
    Some(TypeSet::mono(plate.unwrap_or(Type::Normal)))
}

/// Showdown's `field.clearWeather()`: the weather's `FieldEnd` (`-weather|none`),
/// then `eachEvent('WeatherChange')`. False without a weather.
pub(crate) fn clear_weather(battle: &mut Battle) -> bool {
    if battle.state.field.weather.is_none() {
        return false;
    }
    let field = &mut battle.state.field;
    field.weather = None;
    field.weather_turns = 0;
    battle.conditions_changed();
    battle.emit(BattleEvent::Weather { weather: None, upkeep: false });
    crate::resolve::each_event(battle, Some(Event::WeatherChange), Effect::None);
    true
}

// ---------------------------------------------------------------------------
// Helpers.

/// The Pokémon at `pos` (its team position, on the field or just fainted).
fn mon(battle: &Battle, pos: Position) -> &PokemonState {
    &battle.state.side(pos.side).team[pos.slot as usize]
}

fn mon_mut(battle: &mut Battle, pos: Position) -> &mut PokemonState {
    &mut battle.state.side_mut(pos.side).team[pos.slot as usize]
}

fn ability_state(battle: &Battle, pos: Position) -> &crate::state::AbilityState {
    &mon(battle, pos).volatile_counters.ability_state
}

fn ability_state_mut(battle: &mut Battle, pos: Position) -> &mut crate::state::AbilityState {
    &mut mon_mut(battle, pos).volatile_counters.ability_state
}

/// Showdown's `pokemon.adjacentAllies()`: the other active Pokémon of the
/// side with HP left.
fn adjacent_allies(battle: &Battle, pos: Position) -> crate::util::StackVec<Position, 1> {
    let mut out = crate::util::StackVec::new();
    for slot in 0..battle.format.active_per_side() {
        let p = Position::new(pos.side, slot as u8);
        // `side.allies()` keeps allies with HP; `isAdjacent` checks
        // `fainted`, which a Pokémon at 0 HP isn't until its faint is
        // reported (Wandering Spirit's swap gives it Hospitality).
        if p != pos && events::is_active(battle, p) && mon(battle, p).hp > 0 && events::is_active(battle, pos) {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Switch-in announcements.

/// Intimidate's `onStart`: `-ability|<holder>|Intimidate|boost` once, then
/// each foe (`adjacentFoes()`) loses 1 Attack (a secondary boost: no
/// message for "can't go lower"), unless its substitute makes it `-immune`.
fn intimidate(battle: &mut Battle, holder: Position) -> Option<Relay> {
    let mut activated = false;
    for target in super::foes(battle, holder).iter().copied() {
        if !activated {
            let actor = actor(battle, holder);
            battle.emit(BattleEvent::AbilityShow {
                actor,
                ability: a::INTIMIDATE,
                note: AbilityNote::Boost,
                cause: None,
            });
            activated = true;
        }
        if battle.state.at(target).is_some_and(|m| m.volatiles.has(Volatiles::SUBSTITUTE)) {
            let target = actor(battle, target);
            battle.emit(BattleEvent::Immune { target });
        } else {
            let atk = StatBoosts::new([-1, 0, 0, 0, 0, 0, 0]);
            boosts::boost_from(battle, target, atk, Some(holder), Effect::Ability(a::INTIMIDATE), true, false);
        }
    }
    None
}

/// Supersweet Syrup's `onStart`: once per battle, `-ability`, then each
/// adjacent foe loses 1 evasion (a secondary boost), unless its
/// substitute makes it `-immune`.
fn supersweet_syrup(battle: &mut Battle, holder: Position) {
    if mon(battle, holder).syrup_triggered {
        return;
    }
    mon_mut(battle, holder).syrup_triggered = true;
    super::show(battle, holder, a::SUPERSWEET_SYRUP);
    for target in super::foes(battle, holder).iter().copied() {
        if mon(battle, target).volatiles.has(Volatiles::SUBSTITUTE) {
            let target = actor(battle, target);
            battle.emit(BattleEvent::Immune { target });
        } else {
            let evasion = StatBoosts::new([0, 0, 0, 0, 0, 0, -1]);
            boosts::boost_from(
                battle,
                target,
                evasion,
                Some(holder),
                Effect::Ability(a::SUPERSWEET_SYRUP),
                true,
                false,
            );
        }
    }
}

/// Frisk's `onStart`: each foe's item is revealed
/// (`-item|<foe>|<item>|[from] ability: Frisk|[of] <holder>`).
fn frisk(battle: &mut Battle, holder: Position) {
    for target in super::foes(battle, holder).iter().copied() {
        if let Some(item) = mon(battle, target).item {
            let cause = Cause::ability(a::FRISK).of(actor(battle, holder));
            let actor = actor(battle, target);
            battle.emit(BattleEvent::ItemShow { actor, item, cause: Some(cause) });
        }
    }
}

/// Showdown's `dex.getImmunity(type, pokemon)` and
/// `dex.getEffectiveness(type, pokemon)` by the type chart alone (the
/// Pokémon's `getTypes()`): whether it is not immune, and the sum of the
/// per-type steps (-1, 0, 1).
fn chart_effectiveness(mon: &PokemonState, ty: Type) -> (bool, i32) {
    let mut immune = false;
    let mut total = 0;
    for t in manipulation::all_types(mon).into_iter().flatten() {
        match ty.against(t) {
            pikapro_data::Effectiveness::Immune => immune = true,
            pikapro_data::Effectiveness::Exp(n) => total += i32::from(n.signum()),
        }
    }
    (!immune, total)
}

/// Anticipation's `onStart`: `-ability` if a foe knows a damaging move the
/// holder is weak to (by the type chart), or an OHKO move.
fn anticipation(battle: &mut Battle, holder: Position) {
    let dex = battle.format.dex;
    let me = *mon(battle, holder);
    for target in super::foes(battle, holder).iter().copied() {
        for slot in mon(battle, target).moves.iter() {
            let data = dex.move_(slot.id);
            if data.category == MoveCategory::Status {
                continue;
            }
            let (hits, effectiveness) = chart_effectiveness(&me, data.ty);
            if hits && effectiveness > 0 || data.ohko {
                super::show(battle, holder, a::ANTICIPATION);
                return;
            }
        }
    }
}

/// Forewarn's `onStart`: the foes' strongest moves (OHKO 150; Counter,
/// Mirror Coat and Metal Burst 120; other damaging moves without a base
/// power 80), one of them sampled (a draw even for one), announced with
/// `-activate|<holder>|ability: Forewarn|<move>|[of] <foe>`.
fn forewarn(battle: &mut Battle, holder: Position) {
    use pikapro_data::gen9::moves as m;
    let dex = battle.format.dex;
    let mut warn: crate::util::StackVec<(pikapro_data::MoveId, Position), 8> = crate::util::StackVec::new();
    let mut warn_bp = 1;
    for target in super::foes(battle, holder).iter().copied() {
        for slot in mon(battle, target).moves.iter() {
            let data = dex.move_(slot.id);
            let mut bp = u32::from(data.base_power);
            if data.ohko {
                bp = 150;
            }
            if matches!(slot.id, m::COUNTER | m::METAL_BURST | m::MIRROR_COAT) {
                bp = 120;
            }
            if bp == 1 {
                bp = 80;
            }
            if bp == 0 && data.category != MoveCategory::Status {
                bp = 80;
            }
            if bp > warn_bp {
                warn.clear();
                warn.push((slot.id, target));
                warn_bp = bp;
            } else if bp == warn_bp {
                warn.push((slot.id, target));
            }
        }
    }
    if warn.is_empty() {
        return;
    }
    let (move_id, target) = *battle.rng.sample(&warn);
    let actor = actor(battle, holder);
    let of = Some(crate::resolve::actor(battle, target));
    battle.emit(BattleEvent::ActivateEffect {
        actor,
        effect: EffectName::Ability(a::FOREWARN),
        args: [Some(LineArg::Move(move_id)), None],
        of,
    });
}

/// Download's `onStart`: +1 Special Attack if the foes' Defense (boosted,
/// unmodified) is at most their Special Defense, else +1 Attack.
fn download(battle: &mut Battle, holder: Position) {
    let (mut def, mut spd) = (0, 0);
    for target in super::foes(battle, holder).iter().copied() {
        def += events::get_stat(battle, target, Stat::Def, false, true);
        spd += events::get_stat(battle, target, Stat::SpD, false, true);
    }
    let changes = if def != 0 && def >= spd {
        StatBoosts::new([0, 0, 1, 0, 0, 0, 0])
    } else if spd != 0 {
        StatBoosts::new([1, 0, 0, 0, 0, 0, 0])
    } else {
        return;
    };
    boosts::boost_from(battle, holder, changes, None, Effect::Ability(a::DOWNLOAD), false, false);
}

/// Screen Cleaner's `onStart`: Reflect, Light Screen and Aurora Veil end on
/// both sides (its own first), after one `-activate`.
fn screen_cleaner(battle: &mut Battle, holder: Position) {
    let mut activated = false;
    for condition in [SideCondition::Reflect, SideCondition::LightScreen, SideCondition::AuroraVeil] {
        for side in [holder.side, holder.side.foe()] {
            if battle.state.side(side).conditions.turns(condition) > 0 {
                if !activated {
                    super::activate(battle, holder, a::SCREEN_CLEANER);
                    activated = true;
                }
                field::remove_side_condition(battle, side, condition);
            }
        }
    }
}

/// Air Lock's and Cloud Nine's `onStart` / `onEnd`: `abilityState.ending`,
/// then `eachEvent('WeatherChange', ability)`.
fn weather_suppressor(battle: &mut Battle, holder: Position, ability: AbilityId, ending: bool) {
    ability_state_mut(battle, holder).ending = ending;
    crate::resolve::each_event(battle, Some(Event::WeatherChange), Effect::Ability(ability));
}

// ---------------------------------------------------------------------------
// Switching out.

/// Showdown's `queue.willSwitch(pokemon)`.
fn will_switch(battle: &Battle, pos: Position) -> bool {
    let id = mon(battle, pos).id;
    battle.queue.as_slice().iter().any(|a| {
        matches!(a.kind, queue::ActionKind::Switch { .. } | queue::ActionKind::InstaSwitch { .. })
            && a.mon == Some(queue::MonRef { side: pos.side, id })
    })
}

/// Natural Cure's `onCheckShow` (Scarlet/Violet; the Champions mod has
/// none): in doubles, when a statused Pokémon that could have Natural Cure
/// switches out alongside one that could but doesn't, the cures are hidden
/// (`showCure = false`, with a `-message`).
fn natural_cure_check_show(battle: &mut Battle, pos: Position) {
    if battle.format.active_per_side() == 1 {
        return;
    }
    if mon(battle, pos).show_cure.is_some() {
        return;
    }
    let dex = battle.format.dex;
    let mut cure_list: crate::util::StackVec<Position, 2> = crate::util::StackVec::new();
    let mut no_cure = 0;
    for slot in 0..battle.format.active_per_side() {
        let p = Position::new(pos.side, slot as u8);
        let Some(cur) = battle.state.side(pos.side).team.get(slot) else { continue };
        if cur.status == Status::None {
            continue;
        }
        if cur.show_cure == Some(true) {
            continue;
        }
        let slots = dex.species(cur.species).abilities;
        let has = [Some(slots.primary), slots.secondary, slots.hidden].contains(&Some(a::NATURAL_CURE));
        if !has || slots.secondary.is_none() && slots.hidden.is_none() {
            continue;
        }
        if p != pos && !will_switch(battle, p) {
            continue;
        }
        if super::has_ability(battle, p, a::NATURAL_CURE) {
            cure_list.push(p);
        } else {
            no_cure += 1;
        }
    }
    let show = cure_list.is_empty() || no_cure == 0;
    if !show {
        // `(N of <side>'s pokemon were cured by Natural Cure.)`.
        let message = crate::event::Message::NaturalCure { side: pos.side, cured: cure_list.len() as u8 };
        battle.emit(BattleEvent::Message { message });
    }
    for p in cure_list.iter().copied() {
        mon_mut(battle, p).show_cure = Some(show);
    }
}

/// Natural Cure's `onSwitchOut`: the status is cured, announced silently
/// (`-curestatus|...|[from] ability: Natural Cure|[silent]`) unless
/// `showCure` is false (Scarlet/Violet doubles); the Champions mod always
/// announces it.
fn natural_cure_switch_out(battle: &mut Battle, pos: Position) {
    let status = mon(battle, pos).status;
    if status == Status::None {
        return;
    }
    let champions = battle.format.mechanics == Mechanics::Champions;
    let show = if champions {
        true
    } else {
        let m = mon_mut(battle, pos);
        *m.show_cure.get_or_insert(true)
    };
    if show {
        let target = actor(battle, pos);
        let cause = Cause::ability(a::NATURAL_CURE);
        battle.emit(BattleEvent::CureStatusFrom { target, status, cause, silent: true });
    }
    let m = mon_mut(battle, pos);
    m.status = Status::None;
    if !champions && m.show_cure == Some(false) {
        m.show_cure = None;
    }
}

/// Regenerator's `onSwitchOut`: `pokemon.heal(baseMaxhp / 3)` (no event,
/// no message); the Champions mod then announces it silently.
fn regenerator(battle: &mut Battle, pos: Position) {
    let m = mon_mut(battle, pos);
    if m.hp == 0 || m.hp >= m.max_hp() {
        return;
    }
    let amount = m.max_hp() / 3;
    if amount == 0 {
        return;
    }
    m.hp = (m.hp + amount).min(m.max_hp());
    let (hp, max_hp, status) = (m.hp, m.max_hp(), m.status);
    if battle.format.mechanics == Mechanics::Champions {
        let target = actor(battle, pos);
        let cause = Cause::ability(a::REGENERATOR);
        battle.emit(BattleEvent::HealFromSilent { target, hp, max_hp, status, cause });
    }
}

// ---------------------------------------------------------------------------
// Volatiles.

/// This module's volatiles' handlers (bits 66 and 68..=79).
pub(crate) fn volatile_spec(flag: u128, event: Event, prefix: EventPrefix) -> Option<Spec> {
    if matches!(flag, Volatiles::COMMANDING | Volatiles::COMMANDED) && prefix == EventPrefix::On {
        return match (flag, event) {
            (_, Event::DragOut) => Some(Spec::relay(commander_false).priority(2)),
            (_, Event::TrapPokemon) => Some(Spec::relay(commander_trap).priority(-11)),
            (Volatiles::COMMANDING, Event::Invulnerability) => Some(Spec::relay(commander_false)),
            _ => None,
        };
    }
    modifier_abilities::volatile_spec(flag, event, prefix)
}

/// The events [`volatile_spec`] handles for `flag` (any prefix). Keep it in
/// step with `volatile_spec` (a test checks them).
pub(crate) const fn volatile_events(flag: u128) -> u128 {
    match flag {
        Volatiles::COMMANDING => {
            const { pikapro_data::EventSet::of(&[Event::DragOut, Event::TrapPokemon, Event::Invulnerability]).0 }
        }
        Volatiles::COMMANDED => const { pikapro_data::EventSet::of(&[Event::DragOut, Event::TrapPokemon]).0 },
        _ => modifier_abilities::volatile_events(flag),
    }
}

#[cfg(test)]
mod tests {
    use pikapro_data::gen9::{abilities as a, moves as m, species as s};
    use pikapro_data::{AbilityId, SpeciesId};

    use crate::battle::Battle;
    use crate::choice::{Choice, SlotAction};
    use crate::event::BattleEvent;
    use crate::state::{Position, SideId, Volatiles, Weather};
    use crate::team::{PokemonSet, Team};
    use crate::{BattleOptions, Format, Seed};

    const P1: Position = Position::new(SideId::P1, 0);

    fn set(species: SpeciesId, ability: AbilityId) -> PokemonSet {
        PokemonSet::new(species).with_level(50).with_ability(ability).with_moves(&[m::SPLASH])
    }

    fn singles(p1: Vec<PokemonSet>, p2: Vec<PokemonSet>) -> Battle {
        let teams = [Team::new(p1), Team::new(p2)];
        Battle::new(Format::gen9_singles(), teams, BattleOptions::new(Seed::fast(1))).unwrap()
    }

    fn splash(b: &mut Battle) {
        for side in SideId::BOTH {
            let action = SlotAction::Move { index: 0, target: None, gimmick: None };
            b.submit(side, Choice::single(action)).unwrap();
        }
    }

    /// Tera Shift turns Terapagos into its Terastal Form on switching in.
    #[test]
    fn tera_shift_changes_forme_on_switch_in() {
        let b = singles(vec![set(s::TERAPAGOS, a::TERA_SHIFT)], vec![set(s::MEW, a::NO_ABILITY)]);
        let mon = b.state.at(P1).unwrap();
        assert_eq!(
            (mon.species, mon.base_species, mon.ability),
            (s::TERAPAGOS_TERASTAL, s::TERAPAGOS_TERASTAL, a::TERA_SHELL)
        );
        assert!(b.events().iter().any(|e| matches!(e, BattleEvent::DetailsChange { .. })));
    }

    /// Tera Shell makes a full-HP Terapagos-Terastal resist a hit.
    #[test]
    fn tera_shell_resists_at_full_hp() {
        let foe = PokemonSet::new(s::MEW).with_level(50).with_ability(a::NO_ABILITY).with_moves(&[m::TACKLE]);
        let mut b = singles(vec![set(s::TERAPAGOS, a::TERA_SHIFT)], vec![foe]);
        splash(&mut b);
        let shell = |e: &BattleEvent| matches!(e, BattleEvent::ActivateEffect { effect: crate::event::EffectName::Ability(x), .. } if *x == a::TERA_SHELL);
        assert!(b.events().iter().any(shell));
        assert!(b.events().iter().any(|e| matches!(e, BattleEvent::Resisted { .. })));
    }

    /// Zen Mode: Darmanitan takes its Zen forme at half HP or less at the
    /// end of the turn, and goes back above half.
    #[test]
    fn zen_mode_follows_hp() {
        let mut b = singles(vec![set(s::DARMANITAN, a::ZEN_MODE)], vec![set(s::MEW, a::NO_ABILITY)]);
        let max = b.state.at(P1).unwrap().max_hp();
        b.state.at_mut(P1).unwrap().hp = max / 2;
        splash(&mut b);
        let mon = b.state.at(P1).unwrap();
        assert_eq!(mon.species, s::DARMANITAN_ZEN);
        assert!(mon.volatiles.has(Volatiles::ZEN_MODE));
        b.state.at_mut(P1).unwrap().hp = max;
        splash(&mut b);
        let mon = b.state.at(P1).unwrap();
        assert_eq!(mon.species, s::DARMANITAN);
        assert!(!mon.volatiles.has(Volatiles::ZEN_MODE));
    }

    /// Power Construct completes Zygarde at half HP; fainting reverts it.
    #[test]
    fn power_construct_completes_and_regresses() {
        let mut b = singles(vec![set(s::ZYGARDE_10, a::POWER_CONSTRUCT)], vec![set(s::MEW, a::NO_ABILITY)]);
        let max = b.state.at(P1).unwrap().max_hp();
        b.state.at_mut(P1).unwrap().hp = max / 2;
        splash(&mut b);
        let mon = b.state.at(P1).unwrap();
        assert_eq!((mon.species, mon.forme_regression), (s::ZYGARDE_COMPLETE, Some(s::ZYGARDE_10)));
        assert!(mon.max_hp() > max);
        super::faint_forme_regression(&mut b, P1, false);
        let mon = &b.state.side(SideId::P1).team[0];
        assert_eq!((mon.species, mon.base_species, mon.max_hp()), (s::ZYGARDE_10, s::ZYGARDE_10, max));
    }

    /// Booster Energy's volatile keeps Protosynthesis going without sun.
    #[test]
    fn booster_energy_starts_protosynthesis() {
        let mut b = singles(vec![set(s::GREAT_TUSK, a::PROTOSYNTHESIS)], vec![set(s::MEW, a::NO_ABILITY)]);
        assert!(super::booster_energy(&mut b, P1, false));
        let mon = b.state.at(P1).unwrap();
        assert!(mon.volatiles.has(Volatiles::PROTOSYNTHESIS) && mon.volatile_counters.paradox_booster);
        // Great Tusk's best stat is Attack.
        assert_eq!(mon.volatile_counters.paradox_stat, 0);
        splash(&mut b);
        assert!(b.state.at(P1).unwrap().volatiles.has(Volatiles::PROTOSYNTHESIS));
    }

    /// A primal weather ends when its source leaves the field.
    #[test]
    fn desolate_land_ends_with_its_source() {
        let mut b = singles(
            vec![set(s::GROUDON, a::DESOLATE_LAND), set(s::MEW, a::NO_ABILITY)],
            vec![set(s::SNORLAX, a::DROUGHT)],
        );
        assert_eq!(b.state.field.weather, Some(Weather::HarshSun));
        // Drought fails against it.
        assert!(b.events().iter().any(|e| matches!(e, BattleEvent::WeatherFail { .. })));
        b.submit(SideId::P1, Choice::single(SlotAction::Switch(1))).unwrap();
        b.submit(SideId::P2, Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None })).unwrap();
        assert_eq!(b.state.field.weather, None);
    }
}
