//! The `basic` observation: one small `f32` vector a plain MLP can read.
//!
//! Twelve Pokémon rows (the player's six, then the foe's), the two sides,
//! then the field. No ids: everything is a flag or a number near 0..1.
//!
//! | block | values |
//! |---|---|
//! | Pokémon | `present`, `active`, `fainted`, `hp` (fraction), `status_*` (6), `boost_*` (7, stage / 6), `type_*` (18), `gimmick` (Mega Evolved or Terastallized) |
//! | each of its 4 moves | `known`, `base_power` (/ 200), `type_*` (18), `physical`, `special`, `status`, `pp` (fraction), `disabled` |
//! | each side | `team_size` (/ 6), `fainted` (over the team size), `gimmick_used`, then one value per side condition (layers over its maximum) |
//! | field | `weather_*` (7), `terrain_*` (4), `trick_room`, `gravity`, `magic_room`, `wonder_room`, `turn` (min(turn, 100) / 100), `request_*` (4) |
//!
//! Foe rows follow [`ViewCtx`]: what isn't known yet is 0, and `hp` is the
//! shown percentage. It is a starting point: copy this file to make your
//! own [`Encoder`].

use pikapro::MAX_MOVES;
use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro_data::Type;
use pikapro_view::{OwnRequest, PlayerView};

use super::ctx::{MonRef, ROWS, ViewCtx};
use super::{Encoder, flag, one_hot};
use crate::tables::Tables;

const STATUSES: [&str; 6] = ["brn", "frz", "par", "psn", "tox", "slp"];
const BOOSTS: [&str; 7] = ["atk", "def", "spa", "spd", "spe", "accuracy", "evasion"];
const CATEGORIES: [&str; 3] = ["physical", "special", "status"];
const WEATHERS: [&str; 7] = ["sun", "rain", "sand", "snow", "harsh_sun", "heavy_rain", "strong_winds"];
const TERRAINS: [&str; 4] = ["electric", "grassy", "misty", "psychic"];
const PSEUDO: [&str; 4] = ["trick_room", "gravity", "magic_room", "wonder_room"];
const REQUESTS: [&str; 4] = ["wait", "team_preview", "move", "force_switch"];
/// The side conditions, in [`SideCondition::ALL`]'s order.
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

/// The types of a one-hot: Stellar is never a Pokémon's or a move's type.
const TYPES: usize = Type::COUNT;
/// One move slot: `known`, `base_power`, the type, the category, `pp` and
/// `disabled`.
const MOVE: usize = 1 + 1 + TYPES + CATEGORIES.len() + 1 + 1;
/// One Pokémon row.
const MON: usize = 4 + STATUSES.len() + BOOSTS.len() + TYPES + 1 + MAX_MOVES * MOVE;
/// One side.
const SIDE: usize = 3 + SideCondition::ALL.len();
/// The field.
const FIELD: usize = WEATHERS.len() + TERRAINS.len() + PSEUDO.len() + 1 + REQUESTS.len();

/// The most layers a side condition has.
const fn max_layers(c: SideCondition) -> u8 {
    match c {
        SideCondition::Spikes => 3,
        SideCondition::ToxicSpikes => 2,
        _ => 1,
    }
}

/// The name of the type in a one-hot's `k`-th place.
fn type_name(k: usize) -> String {
    Type::ALL[k].name().to_ascii_lowercase()
}

/// A weather's place in the `weather_*` one-hot.
const fn weather_index(w: Weather) -> usize {
    match w {
        Weather::Sun => 0,
        Weather::Rain => 1,
        Weather::Sand => 2,
        Weather::Snow => 3,
        Weather::HarshSun => 4,
        Weather::HeavyRain => 5,
        Weather::StrongWinds => 6,
    }
}

/// A terrain's place in the `terrain_*` one-hot.
const fn terrain_index(t: Terrain) -> usize {
    match t {
        Terrain::Electric => 0,
        Terrain::Grassy => 1,
        Terrain::Misty => 2,
        Terrain::Psychic => 3,
    }
}

/// The `basic` observation (see the module documentation), and its column
/// names. Its size is the same in every format.
#[derive(Clone, Copy, Debug, Default)]
pub struct Basic;

impl Basic {
    /// Values per player.
    pub const SIZE: usize = 2 * ROWS * MON + 2 * SIDE + FIELD;

    /// The name of every value, in order: `own0.hp`,
    /// `foe3.move1_base_power`, `foe_side.spikes`, `field.weather_rain`.
    pub fn columns() -> Vec<String> {
        let mut names = Vec::with_capacity(Basic::SIZE);
        for who in ["own", "foe"] {
            for row in 0..ROWS {
                let p = format!("{who}{row}");
                names.extend(["present", "active", "fainted", "hp"].map(|n| format!("{p}.{n}")));
                names.extend(STATUSES.map(|n| format!("{p}.status_{n}")));
                names.extend(BOOSTS.map(|n| format!("{p}.boost_{n}")));
                names.extend((0..TYPES).map(|t| format!("{p}.type_{}", type_name(t))));
                names.push(format!("{p}.gimmick"));
                for k in 0..MAX_MOVES {
                    let m = format!("{p}.move{k}");
                    names.push(format!("{m}_known"));
                    names.push(format!("{m}_base_power"));
                    names.extend((0..TYPES).map(|t| format!("{m}_type_{}", type_name(t))));
                    names.extend(CATEGORIES.map(|n| format!("{m}_{n}")));
                    names.push(format!("{m}_pp"));
                    names.push(format!("{m}_disabled"));
                }
            }
        }
        for who in ["own_side", "foe_side"] {
            names.extend(["team_size", "fainted", "gimmick_used"].map(|n| format!("{who}.{n}")));
            names.extend(CONDITIONS.map(|n| format!("{who}.{n}")));
        }
        names.extend(WEATHERS.map(|n| format!("field.weather_{n}")));
        names.extend(TERRAINS.map(|n| format!("field.terrain_{n}")));
        names.extend(PSEUDO.map(|n| format!("field.{n}")));
        names.push("field.turn".into());
        names.extend(REQUESTS.map(|n| format!("field.request_{n}")));
        names
    }
}

/// Writes `value` to `out[*at]` and moves on.
fn push(out: &mut [f32], at: &mut usize, value: f32) {
    out[*at] = value;
    *at += 1;
}

fn mon_row(m: &MonRef<'_>, tables: &Tables, o: &mut [f32]) {
    if !m.present() {
        return;
    }
    let mut at = 0;
    for v in [flag(true), flag(m.is_active()), flag(m.is_fainted()), m.hp_frac()] {
        push(o, &mut at, v);
    }
    let status = m.status() as usize;
    if status > 0 {
        one_hot(&mut o[at..at + STATUSES.len()], status - 1);
    }
    at += STATUSES.len();
    for b in m.boosts() {
        push(o, &mut at, f32::from(b) / 6.0);
    }
    if let Some(t) = m.types(tables) {
        for k in 0..TYPES {
            o[at + k] = f32::from(u8::from(t.bits() & (1 << k) != 0));
        }
    }
    at += TYPES;
    push(o, &mut at, flag(m.terastallized() || m.is_mega()));
    for mv in m.moves() {
        let start = at;
        if let Some(id) = mv.slot.id() {
            let facts = tables.move_facts(id);
            push(o, &mut at, 1.0);
            push(o, &mut at, f32::from(facts.base_power) / 200.0);
            one_hot(&mut o[at..at + TYPES], usize::from(facts.ty));
            at += TYPES;
            one_hot(&mut o[at..at + CATEGORIES.len()], usize::from(facts.category));
            at += CATEGORIES.len();
            push(o, &mut at, mv.pp_frac(tables));
            push(o, &mut at, flag(mv.disabled));
        }
        at = start + MOVE;
    }
}

fn side_row(ctx: &ViewCtx<'_>, side: usize, o: &mut [f32]) {
    let s = ctx.sides[side];
    let size = s.team_size.min(ROWS as u8);
    let mut at = 0;
    push(o, &mut at, f32::from(size) / 6.0);
    push(o, &mut at, if size > 0 { f32::from(s.fainted.min(size)) / f32::from(size) } else { 0.0 });
    push(o, &mut at, flag(s.tera_used || s.mega_used));
    for c in SideCondition::ALL {
        let layers = s.conditions[c as usize].layers;
        push(o, &mut at, f32::from(layers.min(max_layers(c))) / f32::from(max_layers(c)));
    }
}

fn field_row(ctx: &ViewCtx<'_>, o: &mut [f32]) {
    let f = ctx.view.field();
    if let Some(w) = f.weather {
        one_hot(&mut o[..WEATHERS.len()], weather_index(w));
    }
    let mut at = WEATHERS.len();
    if let Some(t) = f.terrain {
        one_hot(&mut o[at..at + TERRAINS.len()], terrain_index(t));
    }
    at += TERRAINS.len();
    for on in [f.trick_room, f.gravity, f.magic_room, f.wonder_room].map(|t| t.is_some()) {
        push(o, &mut at, flag(on));
    }
    push(o, &mut at, f32::from(ctx.view.turn().min(100)) / 100.0);
    let request = match ctx.view.own.request {
        OwnRequest::Wait => 0,
        OwnRequest::TeamPreview { .. } => 1,
        OwnRequest::Move => 2,
        OwnRequest::ForceSwitch { .. } => 3,
    };
    one_hot(&mut o[at..], request);
}

impl Encoder for Basic {
    fn size(&self) -> usize {
        Basic::SIZE
    }

    fn encode(&self, view: &PlayerView<'_>, tables: &Tables, out: &mut [f32]) {
        out.fill(0.0);
        let ctx = ViewCtx::new(view, tables);
        for (side, rows) in ctx.mons.iter().enumerate() {
            for (row, m) in rows.iter().enumerate() {
                mon_row(m, tables, &mut out[(side * ROWS + row) * MON..][..MON]);
            }
        }
        let rest = &mut out[2 * ROWS * MON..];
        side_row(&ctx, 0, &mut rest[..SIDE]);
        side_row(&ctx, 1, &mut rest[SIDE..2 * SIDE]);
        field_row(&ctx, &mut rest[2 * SIDE..]);
    }
}
