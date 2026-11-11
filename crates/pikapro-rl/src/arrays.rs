//! Raw battle state as integer rows, for numpy.
//!
//! This is a dump of [`BattleState`] fields, omniscient and unnormalised:
//! IDs stay IDs (index them with `pikapro.names`), nothing is hidden, scaled
//! or one-hot encoded: for analysis and tooling, never an observation
//! (observations come from the player's view, [`crate::encode`]).
//!
//! Pokémon rows are in current team order (Showdown's: `team[slot]` is the
//! Pokémon active in `slot`); the `id` column is the stable registration
//! index. Missing values (no item, an empty move slot, no second type) are
//! `-1`.

use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro::{Battle, BattleState, Gender, MAX_MOVES, MAX_TEAM, PokemonState, Request, SideId, Status};

/// Columns of a Pokémon row.
pub const POKEMON_COLUMNS: &[&str] = &[
    "id",
    "species",
    "base_species",
    "level",
    "hp",
    "max_hp",
    "status",
    "status_counter",
    "type1",
    "type2",
    "tera_type",
    "terastallized",
    "ability",
    "item",
    "atk",
    "def",
    "spa",
    "spd",
    "spe",
    "boost_atk",
    "boost_def",
    "boost_spa",
    "boost_spd",
    "boost_spe",
    "boost_accuracy",
    "boost_evasion",
    "move1",
    "move1_pp",
    "move1_max_pp",
    "move1_disabled",
    "move2",
    "move2_pp",
    "move2_max_pp",
    "move2_disabled",
    "move3",
    "move3_pp",
    "move3_max_pp",
    "move3_disabled",
    "move4",
    "move4_pp",
    "move4_max_pp",
    "move4_disabled",
    "active_slot",
    "fainted",
    "gender",
    "volatiles0",
    "volatiles1",
    "volatiles2",
    "volatiles3",
    "can_mega_evo",
    "last_move",
    "active_turns",
];

/// Columns of a side row.
pub const SIDE_COLUMNS: &[&str] = &[
    "team_len",
    "active1",
    "active2",
    "total_fainted",
    "gimmicks_used",
    "request",
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

/// Columns of the field row.
pub const FIELD_COLUMNS: &[&str] = &[
    "turn",
    "weather",
    "weather_turns",
    "terrain",
    "terrain_turns",
    "trick_room_turns",
    "gravity_turns",
    "magic_room_turns",
    "wonder_room_turns",
    "fairy_lock_turns",
    "over",
    "winner",
];

/// `status` codes.
pub const STATUS_CODES: &[&str] = &["none", "brn", "frz", "par", "psn", "tox", "slp"];
/// `weather` codes.
pub const WEATHER_CODES: &[&str] = &["Sun", "Rain", "Sand", "Snow", "HarshSun", "HeavyRain", "StrongWinds"];
/// `terrain` codes.
pub const TERRAIN_CODES: &[&str] = &["Electric", "Grassy", "Misty", "Psychic"];
/// `request` codes.
pub const REQUEST_CODES: &[&str] = &["wait", "team_preview", "move", "force_switch"];

pub const POKEMON_WIDTH: usize = POKEMON_COLUMNS.len();
pub const SIDE_WIDTH: usize = SIDE_COLUMNS.len();
pub const FIELD_WIDTH: usize = FIELD_COLUMNS.len();

/// One battle's rows: `2 * 6 * POKEMON_WIDTH` Pokémon values (empty team
/// positions all `-1`), `2 * SIDE_WIDTH` side values, `FIELD_WIDTH` field
/// values.
pub fn fill(battle: &Battle, pokemon: &mut [i32], sides: &mut [i32], field: &mut [i32]) {
    let state = battle.state();
    for side in SideId::BOTH {
        let s = state.side(side);
        for pos in 0..MAX_TEAM {
            let row = &mut pokemon[(side.index() * MAX_TEAM + pos) * POKEMON_WIDTH..][..POKEMON_WIDTH];
            match s.team.get(pos) {
                Some(mon) => fill_pokemon(state, side, pos, mon, row),
                None => row.fill(-1),
            }
        }
        fill_side(battle, side, &mut sides[side.index() * SIDE_WIDTH..][..SIDE_WIDTH]);
    }
    fill_field(battle, field);
}

pub fn request_code(request: Request) -> i8 {
    match request {
        Request::Wait => 0,
        Request::TeamPreview { .. } => 1,
        Request::Move => 2,
        Request::ForceSwitch { .. } => 3,
    }
}

fn status_code(status: Status) -> (i32, i32) {
    match status {
        Status::None => (0, 0),
        Status::Burn => (1, 0),
        Status::Freeze { turns } => (2, turns as i32),
        Status::Paralysis => (3, 0),
        Status::Poison => (4, 0),
        Status::Toxic { counter } => (5, counter as i32),
        Status::Sleep { turns } => (6, turns as i32),
    }
}

fn fill_pokemon(state: &BattleState, side: SideId, pos: usize, mon: &PokemonState, row: &mut [i32]) {
    let s = state.side(side);
    let (status, counter) = status_code(mon.status);
    let mut out = Row { row, i: 0 };
    out.extend([
        mon.id as i32,
        mon.species.0 as i32,
        mon.base_species.0 as i32,
        mon.level as i32,
        mon.hp as i32,
        mon.max_hp() as i32,
        status,
        counter,
        mon.types.primary as i32,
        mon.types.secondary.map_or(-1, |t| t as i32),
        mon.tera_type as i32,
        mon.terastallized as i32,
        mon.ability.0 as i32,
        mon.item.map_or(-1, |i| i.0 as i32),
    ]);
    out.extend(mon.stats.0[1..].iter().map(|&v| v as i32));
    let b = mon.boosts;
    out.extend([b.atk, b.def, b.spa, b.spd, b.spe, b.accuracy, b.evasion].map(|v| v as i32));
    for i in 0..MAX_MOVES {
        match mon.moves.get(i) {
            Some(m) => out.extend([m.id.0 as i32, m.pp as i32, m.max_pp as i32, m.disabled as i32]),
            None => out.extend([-1; 4]),
        }
    }
    let active_slot = s.active.iter().position(|a| *a == Some(pos as u8)).map_or(-1, |slot| slot as i32);
    out.push(active_slot);
    out.push(mon.is_fainted() as i32);
    out.push(match mon.gender {
        Gender::Male => 0,
        Gender::Female => 1,
        Gender::Genderless => 2,
    });
    let bits = mon.volatiles.bits();
    out.extend((0..4).map(|k| (bits >> (32 * k)) as u32 as i32));
    out.push(mon.can_mega_evo.map_or(-1, |s| s.0 as i32));
    out.push(mon.last_move.map_or(-1, |m| m.0 as i32));
    out.push(mon.active_turns as i32);
    debug_assert_eq!(out.i, POKEMON_WIDTH);
}

fn fill_side(battle: &Battle, side: SideId, row: &mut [i32]) {
    let s = battle.state().side(side);
    let mut out = Row { row, i: 0 };
    out.push(s.team.len() as i32);
    out.extend(s.active.map(|a| a.map_or(-1, |p| p as i32)));
    out.push(s.total_fainted as i32);
    out.push(s.gimmicks_used.0 as i32);
    out.push(request_code(battle.request(side)) as i32);
    out.extend(SideCondition::ALL.map(|c| s.conditions.turns(c) as i32));
    debug_assert_eq!(out.i, SIDE_WIDTH);
}

/// Writes a row front to back.
struct Row<'a> {
    row: &'a mut [i32],
    i: usize,
}

impl Row<'_> {
    #[inline]
    fn push(&mut self, v: i32) {
        self.row[self.i] = v;
        self.i += 1;
    }

    #[inline]
    fn extend(&mut self, values: impl IntoIterator<Item = i32>) {
        for v in values {
            self.push(v);
        }
    }
}

fn fill_field(battle: &Battle, row: &mut [i32]) {
    let f = &battle.state().field;
    let weather = f.weather.map_or(-1, |w| match w {
        Weather::Sun => 0,
        Weather::Rain => 1,
        Weather::Sand => 2,
        Weather::Snow => 3,
        Weather::HarshSun => 4,
        Weather::HeavyRain => 5,
        Weather::StrongWinds => 6,
    });
    let terrain = f.terrain.map_or(-1, |t| match t {
        Terrain::Electric => 0,
        Terrain::Grassy => 1,
        Terrain::Misty => 2,
        Terrain::Psychic => 3,
    });
    let (over, winner) = match battle.result() {
        None => (0, -1),
        Some(r) => (1, r.winner.map_or(2, |w| w.index() as i32)),
    };
    row.copy_from_slice(&[
        battle.turn() as i32,
        weather,
        f.weather_turns as i32,
        terrain,
        f.terrain_turns as i32,
        f.trick_room_turns as i32,
        f.gravity_turns as i32,
        f.magic_room_turns as i32,
        f.wonder_room_turns as i32,
        f.fairy_lock_turns as i32,
        over,
        winner,
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::testing::sample_team;
    use pikapro::{BattleOptions, Format, Seed};

    #[test]
    fn rows_fill_every_column() {
        let format = Format::gen9_vgc();
        let teams = [sample_team(&format), sample_team(&format)];
        let battle = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
        let mut p = vec![0; 2 * MAX_TEAM * POKEMON_WIDTH];
        let mut s = vec![0; 2 * SIDE_WIDTH];
        let mut f = vec![0; FIELD_WIDTH];
        fill(&battle, &mut p, &mut s, &mut f);
        assert!(p[POKEMON_COLUMNS.iter().position(|c| *c == "hp").unwrap()] > 0);
        assert_eq!(s[SIDE_COLUMNS.iter().position(|c| *c == "request").unwrap()], 1);
        assert_eq!(f[FIELD_COLUMNS.iter().position(|c| *c == "winner").unwrap()], -1);
    }
}
