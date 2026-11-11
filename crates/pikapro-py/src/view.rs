//! A player's view as plain Python values (feature `python`): what
//! `pikapro.rl.BattleView` reads. One dict per view, built from the
//! [`PlayerView`] the encoders read, through [`ViewCtx`]'s rows: names
//! instead of ids, `None` for what the player doesn't know.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro::{Format, Gender, Gimmick};
use pikapro_data::{MoveCategory, Type};
use pikapro_rl::encode::{MonRef, ViewCtx};
use pikapro_rl::tables::Tables;
use pikapro_view::{
    AbilityState, ItemState, Outcome, OwnMoves, OwnRequest, Phase, PlayerView, PublicVolatiles as V, StatusKind,
};

/// The public volatiles, by the names `PokemonView.effects` lists them
/// under, in its order.
const VOLATILES: [(u64, &str); 57] = [
    (V::SUBSTITUTE, "substitute"),
    (V::CONFUSION, "confusion"),
    (V::TAUNT, "taunt"),
    (V::ENCORE, "encore"),
    (V::DISABLE, "disable"),
    (V::LEECH_SEED, "leech_seed"),
    (V::YAWN, "yawn"),
    (V::PERISH_SONG, "perish_song"),
    (V::CURSE, "curse"),
    (V::ATTRACT, "attract"),
    (V::TORMENT, "torment"),
    (V::SALT_CURE, "salt_cure"),
    (V::SYRUP_BOMB, "syrup_bomb"),
    (V::THROAT_CHOP, "throat_chop"),
    (V::HEAL_BLOCK, "heal_block"),
    (V::IMPRISON, "imprison"),
    (V::NO_RETREAT, "no_retreat"),
    (V::TRAPPED, "trapped"),
    (V::PARTIALLY_TRAPPED, "partially_trapped"),
    (V::SMACK_DOWN, "smack_down"),
    (V::MAGNET_RISE, "magnet_rise"),
    (V::AQUA_RING, "aqua_ring"),
    (V::INGRAIN, "ingrain"),
    (V::CHARGE, "charge"),
    (V::FOCUS_ENERGY, "focus_energy"),
    (V::DRAGON_CHEER, "dragon_cheer"),
    (V::TAR_SHOT, "tar_shot"),
    (V::STOCKPILE, "stockpile"),
    (V::POWER_TRICK, "power_trick"),
    (V::POWER_SHIFT, "power_shift"),
    (V::UPROAR, "uproar"),
    (V::GLAIVE_RUSH, "glaive_rush"),
    (V::TYPE_CHANGE, "type_change"),
    (V::TYPE_ADD, "type_add"),
    (V::TRANSFORMED, "transformed"),
    (V::MIMIC, "mimic"),
    (V::ABILITY_SUPPRESSED, "ability_suppressed"),
    (V::FLASH_FIRE, "flash_fire"),
    (V::COMMANDING, "commanding"),
    (V::MUST_RECHARGE, "must_recharge"),
    (V::CHARGING, "charging"),
    (V::LOCKED_MOVE, "locked_move"),
    (V::FOCUSING, "focusing"),
    (V::PROTECT, "protect"),
    (V::ENDURE, "endure"),
    (V::ROOST, "roost"),
    (V::HELPING_HAND, "helping_hand"),
    (V::CENTER_OF_ATTENTION, "center_of_attention"),
    (V::ELECTRIFY, "electrify"),
    (V::DESTINY_BOND, "destiny_bond"),
    (V::LOCK_ON, "lock_on"),
    (V::MINIMIZE, "minimize"),
    (V::DEFENSE_CURL, "defense_curl"),
    (V::SLOW_START, "slow_start"),
    (V::NEUTRALIZING_GAS, "neutralizing_gas"),
    (V::COMMANDED, "commanded"),
    (V::PARADOX_BOOST, "paradox_boost"),
];

/// The side conditions' names, in [`SideCondition::ALL`]'s order.
const CONDITIONS: [&str; SideCondition::ALL.len()] = [
    "reflect",
    "light_screen",
    "aurora_veil",
    "tailwind",
    "safeguard",
    "mist",
    "wide_guard",
    "quick_guard",
    "stealth_rock",
    "spikes",
    "toxic_spikes",
    "sticky_web",
    "fire_pledge",
    "grass_pledge",
    "water_pledge",
];

/// A type's name in lower case (`"fire"`, `"stellar"`); `None` for the
/// typeless `???` Burn Up and Double Shock leave.
fn type_name(t: Type) -> Option<String> {
    (t != Type::Unknown).then(|| t.name().to_ascii_lowercase())
}

/// A duration: turns elapsed, -1 when not up.
fn turns(on: bool, n: u8) -> i16 {
    if on { i16::from(n) } else { -1 }
}

/// One Pokémon row as a dict, `None` for an empty row.
fn mon_dict<'py>(py: Python<'py>, m: &MonRef<'_>, tables: &Tables, mega: bool) -> PyResult<Option<Bound<'py, PyDict>>> {
    if !m.present() {
        return Ok(None);
    }
    let dex = tables.dex;
    let d = PyDict::new(py);
    let species = m.species().map(|id| dex.species(id));
    d.set_item("species", species.map(|s| s.name))?;
    d.set_item("base_species", species.map(|s| dex.species(s.base_species).name))?;
    // A list of ints (a `Vec<u8>` would arrive as `bytes`).
    d.set_item("base_stats", species.map(|s| s.base_stats.0.map(u16::from).to_vec()))?;
    d.set_item("level", m.level())?;
    d.set_item(
        "gender",
        match m.gender() {
            Gender::Genderless => "genderless",
            Gender::Male => "male",
            Gender::Female => "female",
        },
    )?;
    let (hp, max) = m.hp();
    d.set_item("hp", hp)?;
    d.set_item("max_hp", max)?;
    let status = m.status();
    d.set_item("status", status as u8)?;
    let counted = matches!(status, StatusKind::Sleep | StatusKind::Freeze | StatusKind::Toxic);
    d.set_item("status_turns", if counted { m.status_turns() } else { 0 })?;
    d.set_item("boosts", m.boosts().to_vec())?;
    // Its types: what every forme shares if team preview hid its forme.
    let types: Vec<String> = match (m.wildcard_types(tables), m.types(tables)) {
        (Some((shared, _)), _) => shared.into_iter().flatten().filter_map(type_name).collect(),
        (None, Some(t)) => {
            [Some(t.set.primary), t.set.secondary, t.added].into_iter().flatten().filter_map(type_name).collect()
        }
        (None, None) => Vec::new(),
    };
    d.set_item("types", types)?;
    d.set_item("tera_type", m.tera_type(tables).and_then(type_name))?;
    d.set_item("terastallized", tables.tera && m.terastallized())?;
    d.set_item("mega", mega && m.is_mega())?;
    // The item: `None` unknown, `""` none (or lost), else its name.
    d.set_item(
        "item",
        match m.item() {
            ItemState::Unknown => None,
            ItemState::Known(id) => Some(dex.item(id).name),
            ItemState::NoItem | ItemState::Gone { .. } => Some(""),
        },
    )?;
    d.set_item(
        "ability",
        match m.ability() {
            AbilityState::Unknown => None,
            AbilityState::Known(id) => Some(dex.ability(id).name),
        },
    )?;
    let moves = PyList::empty(py);
    for (slot, mv) in m.moves().iter().enumerate() {
        let Some(id) = mv.slot.id() else { continue };
        let data = dex.move_(id);
        let e = PyDict::new(py);
        e.set_item("slot", slot)?;
        e.set_item("name", data.name)?;
        e.set_item("base_power", data.base_power)?;
        e.set_item("type", type_name(data.ty))?;
        e.set_item(
            "category",
            match data.category {
                MoveCategory::Physical => "physical",
                MoveCategory::Special => "special",
                MoveCategory::Status => "status",
            },
        )?;
        e.set_item("accuracy", data.accuracy)?;
        e.set_item("priority", data.priority)?;
        e.set_item("target", data.target as u8)?;
        e.set_item("pp", mv.pp)?;
        e.set_item("max_pp", mv.max_pp)?;
        e.set_item("disabled", mv.disabled)?;
        e.set_item("uses", mv.uses)?;
        moves.append(e)?;
    }
    d.set_item("moves", moves)?;
    d.set_item("active", m.is_active())?;
    d.set_item("fainted", m.is_fainted())?;
    d.set_item("revealed", m.is_revealed())?;
    d.set_item("slot", m.slot)?;
    d.set_item("stats", m.exact_stats().map(|s| s[1..].to_vec()))?;
    let volatiles = m.active.map_or(V::default(), |a| a.volatiles);
    let effects: Vec<&str> = VOLATILES.iter().filter(|(bit, _)| volatiles.has(*bit)).map(|(_, name)| *name).collect();
    d.set_item("effects", effects)?;
    d.set_item("turns_active", m.active.map(|a| a.turns_active))?;
    d.set_item("protect_chain", m.active.map_or(0, |a| a.counters.protect_chain))?;
    d.set_item("brought", m.brought)?;
    d.set_item("team_pos", m.team_pos)?;
    let q = m.request;
    d.set_item("request_moves", q.is_some_and(|q| matches!(q.moves, OwnMoves::Moves(_))))?;
    d.set_item("can_tera", tables.tera && q.is_some_and(|q| q.can_terastallize.is_some()))?;
    d.set_item("can_mega", mega && q.is_some_and(|q| q.can_mega_evo))?;
    d.set_item("trapped", q.is_some_and(|q| q.trapped))?;
    d.set_item("maybe_trapped", q.is_some_and(|q| q.maybe_trapped))?;
    Ok(Some(d))
}

/// `view` as a dict (see `pikapro.rl.view`, which reads it): the
/// `format`, the battle's `turn`, `phase` and `outcome`, the field, the player's `request`, both
/// `sides` and their Pokémon rows (`mons`), the player's first.
pub(crate) fn view_dict<'py>(py: Python<'py>, view: &PlayerView<'_>, format: &Format) -> PyResult<Bound<'py, PyDict>> {
    let tables = Tables::for_format(format);
    let tera = tables.tera;
    let mega = format.gimmicks.contains(Gimmick::MegaEvolve);
    let ctx = ViewCtx::new(view, &tables);
    let d = PyDict::new(py);
    d.set_item("format", format.id)?;
    let public = view.public;
    d.set_item("turn", public.turn)?;
    d.set_item(
        "phase",
        match public.phase {
            Phase::Setup => 0,
            Phase::TeamPreview => 1,
            Phase::Battle => 2,
            Phase::Over => 3,
        },
    )?;
    d.set_item(
        "outcome",
        match public.outcome {
            None => 0,
            Some(Outcome::Win(s)) if s == view.me => 1,
            Some(Outcome::Win(_)) => 2,
            Some(Outcome::Tie) => 3,
        },
    )?;
    let f = view.field();
    d.set_item(
        "weather",
        match f.weather {
            None => 0,
            Some(Weather::Sun) => 1,
            Some(Weather::Rain) => 2,
            Some(Weather::Sand) => 3,
            Some(Weather::Snow) => 4,
            Some(Weather::HarshSun) => 5,
            Some(Weather::HeavyRain) => 6,
            Some(Weather::StrongWinds) => 7,
        },
    )?;
    d.set_item("weather_turns", turns(f.weather.is_some(), f.weather_turns))?;
    d.set_item(
        "terrain",
        match f.terrain {
            None => 0,
            Some(Terrain::Electric) => 1,
            Some(Terrain::Grassy) => 2,
            Some(Terrain::Misty) => 3,
            Some(Terrain::Psychic) => 4,
        },
    )?;
    d.set_item("terrain_turns", turns(f.terrain.is_some(), f.terrain_turns))?;
    // Turns elapsed, `None` while not up.
    d.set_item("trick_room", f.trick_room)?;
    d.set_item("gravity", f.gravity)?;
    d.set_item("magic_room", f.magic_room)?;
    d.set_item("wonder_room", f.wonder_room)?;
    d.set_item("fairy_lock", f.fairy_lock.then_some(f.fairy_lock_turns))?;
    let (request, force_switch) = match view.own.request {
        OwnRequest::Wait => (0, Vec::new()),
        OwnRequest::TeamPreview { .. } => (1, Vec::new()),
        OwnRequest::Move => (2, Vec::new()),
        OwnRequest::ForceSwitch { slots } => (3, slots.to_vec()),
    };
    d.set_item("request", request)?;
    d.set_item("force_switch", force_switch)?;
    let sides = PyList::empty(py);
    let mons = PyList::empty(py);
    for ego in 0..2 {
        let s = ctx.sides[ego];
        let e = PyDict::new(py);
        let conditions = PyDict::new(py);
        for (name, c) in CONDITIONS.iter().zip(&s.conditions) {
            if c.layers > 0 {
                conditions.set_item(name, c.layers)?;
            }
        }
        e.set_item("conditions", conditions)?;
        e.set_item("tera_used", tera && s.tera_used)?;
        e.set_item("mega_used", mega && s.mega_used)?;
        sides.append(e)?;
        let rows = PyList::empty(py);
        for m in &ctx.mons[ego] {
            rows.append(mon_dict(py, m, &tables, mega)?)?;
        }
        mons.append(rows)?;
    }
    d.set_item("sides", sides)?;
    d.set_item("mons", mons)?;
    Ok(d)
}

/// The damage multiplier of an attack of type `attack` against a Pokémon
/// with `types` (lower-case names, as the view gives them): the product
/// over its types.
#[pyfunction]
fn _type_multiplier(attack: &str, types: Vec<String>) -> PyResult<f64> {
    let parse =
        |name: &str| Type::from_name(name).ok_or_else(|| PyValueError::new_err(format!("unknown type {name:?}")));
    let attack = parse(attack)?;
    let mut multiplier = 1.0;
    for t in &types {
        multiplier *= match attack.against(parse(t)?) {
            pikapro_data::Effectiveness::Immune => 0.0,
            pikapro_data::Effectiveness::Exp(n) => 2f64.powi(i32::from(n)),
        };
    }
    Ok(multiplier)
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(_type_multiplier, m)?)?;
    Ok(())
}
