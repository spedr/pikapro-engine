//! Reading Showdown's `|request|` JSON back into a [`RequestData`]: the
//! inverse of [`Renderer::request_json`](crate::Renderer::request_json),
//! plus what only Showdown's server adds (`rqid`, `update`, a string
//! `disabled`, `disabledSource`, `pokeball`, keys of other generations).

use core::fmt;

use pikapro::request::{ActiveRequest, MoveChoices, MoveRequest, PokemonRequest, RequestData, RequestKind};
use pikapro::util::StackVec;
use pikapro::{Format, Gender, MAX_ACTIVE, MAX_MOVES, MAX_TEAM, SideId, Status};
use pikapro_data::{Dex, MoveTarget, SpeciesId, Stat, Stats, Type};

use super::json::{JsonError, Value};

/// Why a request could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// The request is not valid JSON.
    Json(JsonError),
    /// A value the request must have is missing or has the wrong type.
    /// (A name the format's dex doesn't know is no error: it is read as a
    /// stand-in and counted in [`ParsedRequest::unknown_names`].)
    Missing(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Json(e) => write!(f, "invalid request JSON: {e}"),
            ParseError::Missing(what) => write!(f, "request without a valid {what}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<JsonError> for ParseError {
    fn from(e: JsonError) -> ParseError {
        ParseError::Json(e)
    }
}

/// A request as Showdown sent it: its data and what the server adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParsedRequest {
    pub data: RequestData,
    /// `update: true`: the request sent again after a choice was refused
    /// (`[Unavailable choice]`), changed by what that revealed.
    pub update: bool,
    /// The server's request id (`rqid`), if any.
    pub rqid: Option<u64>,
    /// Names the format's dex doesn't know (a newer server's), read as
    /// stand-ins: a species as its base species (or the dex's first), a
    /// move as Struggle, an ability as none, an item as none.
    pub unknown_names: u32,
}

/// A Pokémon's `details` (`Species, L50, M, shiny, tera:Fire`) as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Details {
    pub species: SpeciesId,
    pub level: u8,
    pub gender: Gender,
    pub shiny: bool,
    pub tera: Option<Type>,
    /// A team preview wildcard (`Urshifu-*`): `species` is the base.
    pub wildcard: bool,
}

/// `details` (`Species, L50, F, shiny, tera:Fire`), or with a species the dex doesn't know (a newer server's)
/// its stand-in ([`species_or_standin`], counted in `unknown`).
pub(crate) fn details_or_standin(dex: &Dex, text: &str, unknown: &mut u32) -> Details {
    let (name, _) = text.split_once(", ").unwrap_or((text, ""));
    let species = species_or_standin(dex, name.strip_suffix("-*").unwrap_or(name), unknown);
    details_with(species, text)
}

/// A species by name; one the dex doesn't know is read as its base species
/// (the name before the forme's `-`) if known, else the dex's first, and
/// counted in `unknown`.
pub(crate) fn species_or_standin(dex: &Dex, name: &str, unknown: &mut u32) -> SpeciesId {
    if let Some(s) = dex.species_by_name(name) {
        return s;
    }
    *unknown = unknown.saturating_add(1);
    name.split_once('-').and_then(|(b, _)| dex.species_by_name(b)).unwrap_or_default()
}

/// The details `text` shows, with `species`.
fn details_with(species: SpeciesId, text: &str) -> Details {
    let mut parts = text.split(", ");
    let wildcard = parts.next().is_some_and(|n| n.ends_with("-*"));
    let mut d = Details { species, level: 100, gender: Gender::Genderless, shiny: false, tera: None, wildcard };
    for p in parts {
        match p {
            "M" => d.gender = Gender::Male,
            "F" => d.gender = Gender::Female,
            "shiny" => d.shiny = true,
            _ => {
                if let Some(l) = p.strip_prefix('L') {
                    d.level = l.parse().unwrap_or(100);
                } else if let Some(t) = p.strip_prefix("tera:") {
                    d.tera = super::type_by_name(t);
                }
            }
        }
    }
    d
}

/// One `side.pokemon` entry's identity, for the caller to map to a stable
/// id ([`PokemonRequest::id`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RequestMon<'a> {
    /// Team position.
    pub position: usize,
    /// The name in `ident` (`p1: <name>`), as sent (JSON escapes kept).
    pub raw_name: &'a str,
}

/// A number no server sends past `u8::MAX`, clamped (see the `channel`
/// module docs).
fn clamp_u8(n: u64) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

/// Reads a `|request|` body with ids by team position (right for the
/// first request of a battle, before any switch: then position is
/// registration order). [`ChannelParser::request`](super::ChannelParser::request)
/// keeps ids stable over the battle.
pub fn parse_request(json: &str, format: &Format) -> Result<RequestData, ParseError> {
    parse_request_with(json, format, |m| m.position as u8).map(|r| r.data)
}

/// Reads a `|request|` body; `id_of` gives each Pokémon's stable id.
pub(crate) fn parse_request_with<'j>(
    json: &'j str,
    format: &Format,
    mut id_of: impl FnMut(&RequestMon<'j>) -> u8,
) -> Result<ParsedRequest, ParseError> {
    let dex = format.dex;
    let root = Value::parse(json)?;
    if !matches!(root, Value::Object(_)) {
        return Err(ParseError::Missing("object"));
    }
    let [side_v, wait, team_preview, max_chosen, force_switch, active_v, no_cancel, update, rqid] = root.fields([
        "side",
        "wait",
        "teamPreview",
        "maxChosenTeamSize",
        "forceSwitch",
        "active",
        "noCancel",
        "update",
        "rqid",
    ])?;
    let side_v = side_v.ok_or(ParseError::Missing("side"))?;
    let [side_id, list] = side_v.fields(["id", "pokemon"])?;
    let side = match side_id.and_then(|v| v.as_raw_str()) {
        Some("p1") => SideId::P1,
        Some("p2") => SideId::P2,
        _ => return Err(ParseError::Missing("side.id")),
    };
    let mut unknown = 0u32;
    let mut pokemon = StackVec::<PokemonRequest, MAX_TEAM>::new();
    let list = list.ok_or(ParseError::Missing("side.pokemon"))?;
    for (position, p) in list.items().enumerate() {
        let p = p?;
        if pokemon.is_full() {
            break;
        }
        pokemon.push(pokemon_entry(dex, position, &p, &mut id_of, &mut unknown)?);
    }
    let active_count = format.active_per_side();
    let kind = if wait.is_some_and(|v| v.truthy()) {
        RequestKind::Wait
    } else if team_preview.is_some_and(|v| v.truthy()) {
        // Absent without a picked team size (the whole team is brought).
        RequestKind::TeamPreview { max_chosen_team_size: max_chosen.and_then(|v| v.as_u64()).map(clamp_u8) }
    } else if let Some(f) = force_switch {
        let mut slots = StackVec::<bool, MAX_ACTIVE>::new();
        for v in f.items() {
            if !slots.is_full() {
                slots.push(v?.truthy());
            }
        }
        RequestKind::ForceSwitch { slots }
    } else if let Some(a) = active_v {
        let mut active = StackVec::<Option<ActiveRequest>, MAX_ACTIVE>::new();
        for v in a.items() {
            let v = v?;
            if active.len() >= active_count.min(MAX_ACTIVE) {
                break;
            }
            active.push(match v {
                Value::Null => None,
                v => Some(active_entry(dex, &v, &mut unknown)?),
            });
        }
        RequestKind::Move { active }
    } else {
        RequestKind::Wait
    };
    let data = RequestData { side, kind, answered: false, no_cancel: no_cancel.is_some_and(|v| v.truthy()), pokemon };
    Ok(ParsedRequest {
        data,
        update: update.is_some_and(|v| v.truthy()),
        rqid: rqid.and_then(|v| v.as_u64()),
        unknown_names: unknown,
    })
}

fn pokemon_entry<'j>(
    dex: &Dex,
    position: usize,
    p: &Value<'j>,
    id_of: &mut impl FnMut(&RequestMon<'j>) -> u8,
    unknown: &mut u32,
) -> Result<PokemonRequest, ParseError> {
    let [
        ident,
        details_v,
        condition,
        active,
        stats_v,
        moves_v,
        base_ability,
        item_v,
        ability_v,
        commanding,
        reviving,
        tera_type,
        terastallized,
    ] = p.fields([
        "ident",
        "details",
        "condition",
        "active",
        "stats",
        "moves",
        "baseAbility",
        "item",
        "ability",
        "commanding",
        "reviving",
        "teraType",
        "terastallized",
    ])?;
    let ident = ident.and_then(|v| v.as_raw_str()).ok_or(ParseError::Missing("ident"))?;
    let raw_name = ident.split_once(": ").map_or(ident, |(_, n)| n);
    let details_raw = details_v.and_then(|v| v.as_str()).ok_or(ParseError::Missing("details"))?;
    let details_text = details_raw.text();
    let details = details_or_standin(dex, &details_text, unknown);
    let id = id_of(&RequestMon { position, raw_name });
    let condition = condition.and_then(|v| v.as_raw_str()).unwrap_or("0 fnt");
    let (hp, max_hp, status) = parse_condition(condition);
    let mut stats = Stats::<u16>::default();
    if let Some(s) = stats_v {
        let values = s.fields(["atk", "def", "spa", "spd", "spe"])?;
        for (stat, v) in [Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe].into_iter().zip(values) {
            stats[stat] = v.and_then(|v| v.as_u64()).map_or(0, |n| u16::try_from(n).unwrap_or(u16::MAX));
        }
    }
    let mut moves = StackVec::new();
    if let Some(ms) = moves_v {
        for m in ms.items() {
            let m = m?;
            let name = m.as_raw_str().ok_or(ParseError::Missing("moves"))?;
            // Hidden Power's type is part of its id in older generations
            // (`hiddenpowerfire60`); none of these formats has it.
            let id = dex.move_by_name(name).unwrap_or_else(|| {
                *unknown += 1;
                pikapro_data::gen9::moves::STRUGGLE
            });
            if !moves.is_full() {
                moves.push(id);
            }
        }
    }
    let mut ability_of = |v: Option<Value<'_>>| -> pikapro_data::AbilityId {
        match v.and_then(|v| v.as_raw_str()) {
            None | Some("") => pikapro_data::AbilityId::NONE,
            Some(a) => dex.ability_by_name(a).unwrap_or_else(|| {
                *unknown += 1;
                pikapro_data::AbilityId::NONE
            }),
        }
    };
    let (base_ability, ability) = (ability_of(base_ability), ability_of(ability_v));
    let item = match item_v.and_then(|v| v.as_raw_str()) {
        None | Some("") => None,
        Some(i) => {
            let item = dex.item_by_name(i);
            *unknown += u32::from(item.is_none());
            item
        }
    };
    let tera_type = tera_type.and_then(|v| v.as_raw_str()).and_then(Type::from_name);
    let terastallized = terastallized.and_then(|v| v.as_raw_str()).and_then(Type::from_name);
    // The Champions mod sends no `teraType`: the type the battle would
    // take (the species' first).
    let tera_type = tera_type.unwrap_or_else(|| dex.species(details.species).types.primary);
    Ok(PokemonRequest {
        id,
        species: details.species,
        level: details.level,
        hp,
        max_hp,
        status,
        active: active.is_some_and(|v| v.truthy()),
        stats,
        moves,
        base_ability,
        item,
        ability,
        commanding: commanding.is_some_and(|v| v.truthy()),
        reviving: reviving.is_some_and(|v| v.truthy()),
        tera_type,
        terastallized,
        gender: details.gender,
    })
}

/// `hp/max status` or `0 fnt` (a request's `condition`: always exact).
fn parse_condition(text: &str) -> (u16, u16, Status) {
    let (hp, status) = text.split_once(' ').map_or((text, ""), |(h, s)| (h, s));
    let status = parse_status(status);
    let (hp, max) = hp.split_once('/').map_or((hp, "0"), |(a, b)| (a, b));
    (hp.parse().unwrap_or(0), max.parse().unwrap_or(0), status)
}

/// A status id (`par`, `tox`, ...; `fnt` and anything else: none).
pub(crate) fn parse_status(id: &str) -> Status {
    match id {
        "brn" => Status::Burn,
        "frz" => Status::Freeze { turns: 0 },
        "par" => Status::Paralysis,
        "psn" => Status::Poison,
        "tox" => Status::Toxic { counter: 0 },
        "slp" => Status::Sleep { turns: 0 },
        _ => Status::None,
    }
}

fn active_entry(dex: &Dex, v: &Value<'_>, unknown: &mut u32) -> Result<ActiveRequest, ParseError> {
    let [list, maybe_disabled, maybe_locked, trapped, maybe_trapped, can_mega_evo, can_terastallize] = v.fields([
        "moves",
        "maybeDisabled",
        "maybeLocked",
        "trapped",
        "maybeTrapped",
        "canMegaEvo",
        "canTerastallize",
    ])?;
    let flag = |x: Option<Value<'_>>| x.is_some_and(|x| x.truthy());
    let list = list.ok_or(ParseError::Missing("active.moves"))?;
    let mut entries = StackVec::<MoveRequest, MAX_MOVES>::new();
    let mut locked = None;
    let mut special = None;
    for m in list.items() {
        let [id, pp, maxpp, target, disabled] = m?.fields(["id", "pp", "maxpp", "target", "disabled"])?;
        let id = id.and_then(|x| x.as_raw_str()).ok_or(ParseError::Missing("move id"))?;
        match id {
            "recharge" => special = Some(MoveChoices::Recharge),
            "struggle" if pp.is_none() => special = Some(MoveChoices::Struggle),
            _ => {
                let move_id = dex.move_by_name(id).unwrap_or_else(|| {
                    *unknown += 1;
                    pikapro_data::gen9::moves::STRUGGLE
                });
                // A locked move (Outrage, a charged Fly) has no `pp`.
                let Some(pp) = pp.and_then(|x| x.as_u64()) else {
                    locked = Some(move_id);
                    continue;
                };
                let target = target
                    .and_then(|x| x.as_raw_str())
                    .and_then(parse_target)
                    .unwrap_or_else(|| dex.move_(move_id).target);
                if !entries.is_full() {
                    entries.push(MoveRequest {
                        id: move_id,
                        pp: clamp_u8(pp),
                        max_pp: maxpp.and_then(|x| x.as_u64()).map_or(0, clamp_u8),
                        target,
                        disabled: flag(disabled),
                    });
                }
            }
        }
    }
    let moves = match (special, locked) {
        (Some(s), _) => s,
        (None, Some(m)) if entries.is_empty() => MoveChoices::Locked(m),
        _ => MoveChoices::Moves(entries),
    };
    Ok(ActiveRequest {
        moves,
        maybe_disabled: flag(maybe_disabled),
        maybe_locked: flag(maybe_locked),
        trapped: flag(trapped),
        maybe_trapped: flag(maybe_trapped),
        can_mega_evo: flag(can_mega_evo),
        can_terastallize: can_terastallize.and_then(|x| x.as_raw_str()).and_then(Type::from_name),
    })
}

/// Every move target type, for reading their Showdown names.
const TARGETS: [MoveTarget; 15] = [
    MoveTarget::Normal,
    MoveTarget::AdjacentFoe,
    MoveTarget::AdjacentAlly,
    MoveTarget::AdjacentAllyOrSelf,
    MoveTarget::Any,
    MoveTarget::AllAdjacentFoes,
    MoveTarget::AllAdjacent,
    MoveTarget::User,
    MoveTarget::Allies,
    MoveTarget::AllySide,
    MoveTarget::FoeSide,
    MoveTarget::All,
    MoveTarget::RandomNormal,
    MoveTarget::AllyTeam,
    MoveTarget::Scripted,
];

/// A move target type from its Showdown name (`adjacentFoe`).
pub(crate) fn parse_target(name: &str) -> Option<MoveTarget> {
    TARGETS.into_iter().find(|&t| crate::request::target_id(t) == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Perspective, Renderer};
    use pikapro::testing::sample_team;
    use pikapro::{Battle, BattleOptions, Seed};

    /// Every request of random battles reads back as the request it was
    /// rendered from (ids by position at the first request).
    #[test]
    fn requests_read_back() {
        for format in Format::presets() {
            for seed in 0..20 {
                let teams = [sample_team(format), sample_team(format)];
                let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
                let mut rng = pikapro::BattleRng::new(Seed::fast(seed ^ 77));
                let r = Renderer::new(format, Perspective::Omniscient);
                let mut max_hp = [[0u16; MAX_TEAM]; 2];
                while !battle.is_over() {
                    for side in SideId::BOTH {
                        let mut want = battle.request_data(side);
                        want.answered = false;
                        let json = r.request_json(&want);
                        let mut got = parse_request_with(&json, format, |m| {
                            // The battle's stable ids, as a player keeps them.
                            battle.state().side(side).team.get(m.position).map_or(0, |p| p.id)
                        })
                        .unwrap()
                        .data;
                        for (p, w) in got.pokemon.iter_mut().zip(want.pokemon.iter()) {
                            if p.hp == 0 && p.max_hp == 0 {
                                p.max_hp = max_hp[side.index()][p.id as usize];
                            } else {
                                max_hp[side.index()][p.id as usize] = p.max_hp;
                            }
                            // The request shows no HP stat; statuses lose
                            // their counters.
                            assert_eq!(p.stats[Stat::Atk], w.stats[Stat::Atk]);
                            p.stats = w.stats;
                            p.status = w.status;
                            if format.mechanics == pikapro::Mechanics::Champions {
                                p.tera_type = w.tera_type;
                            }
                        }
                        assert_eq!(got, want, "{json}");
                    }
                    for side in SideId::BOTH {
                        let legal = battle.legal_actions(side);
                        if !legal.is_waiting() {
                            let c = legal.sample(&mut rng);
                            battle.submit(side, c).unwrap();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reads_showdown_extras() {
        let format = Format::gen9_singles();
        let json = r#"{"active":[{"moves":[{"move":"Tackle","id":"tackle","pp":35,"maxpp":56,"target":"normal","disabled":"hidden","disabledSource":""}],"trapped":true}],"side":{"name":"Me","id":"p2","pokemon":[{"ident":"p2: Zoë","details":"Pikachu, L50, F, shiny","condition":"50/100 par","active":true,"stats":{"atk":1,"def":2,"spa":3,"spd":4,"spe":5},"moves":["tackle"],"baseAbility":"static","item":"","pokeball":"pokeball","ability":"static","commanding":false,"reviving":false,"teraType":"Electric","terastallized":""}]},"update":true,"rqid":7}"#;
        let r = parse_request_with(json, &format, |m| {
            assert_eq!(super::super::json::unescape(m.raw_name), "Zoë");
            3
        })
        .unwrap();
        assert!(r.update);
        assert_eq!(r.rqid, Some(7));
        assert_eq!(r.data.side, SideId::P2);
        let p = r.data.pokemon[0];
        assert_eq!((p.id, p.level, p.hp, p.max_hp, p.gender), (3, 50, 50, 100, Gender::Female));
        let RequestKind::Move { active } = r.data.kind else { panic!() };
        let a = active[0].unwrap();
        assert!(a.trapped);
        let MoveChoices::Moves(ms) = a.moves else { panic!() };
        assert!(ms[0].disabled);
    }
}
