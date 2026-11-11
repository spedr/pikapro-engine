//! Our view, projected onto poke-env's normalised state
//! (`tools/view-oracle/SCHEMA.md`, "Output: state records"). Everything
//! the schema and the view disagree on how to say lives here, so aligning
//! with a schema change touches this module only.
//!
//! The projection reads only the view (the public tracker and the
//! player's overlay) plus the harness's name table, which gives each
//! Pokémon its poke-env key (`"p1: <name>"`). Where the view represents a
//! fact another way than poke-env (a side condition's or terrain's start
//! turn: the view counts turns elapsed, never the absolute turn), the
//! value is `"?"` and the diff compares only the key (see the allowlist in
//! `tools/view-oracle/diff/allowlist.py`).

use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro::{Gender, SideId};
use pikapro_data::{BoostId, Dex, MoveId, SpeciesId, Type, to_id};
use pikapro_view::{
    AbilityState, ActiveView, ItemState, MonView, Outcome, OwnMon, OwnOverlay, OwnRequest, Phase, PublicTracker,
    StatusKind,
};

use super::Setup;
use super::sound::row_of;

/// A JSON value (no floats: the schema has none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    pub fn obj(fields: Vec<(&str, Json)>) -> Json {
        Json::Obj(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// Compact JSON, as Python's `json.dumps(..., separators=(",", ":"),
    /// ensure_ascii=False)` writes it.
    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => out.push_str(&n.to_string()),
            Json::Str(s) => pikapro_protocol::push_json_string(out, s),
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pikapro_protocol::push_json_string(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// The facts a golden fixture keeps, as `path=value`
    /// (`team.p1: Pikachu.hp=35`, `opponent_team.p2: Lucario.moves.0=aurasphere`):
    /// every leaf the view asserts, so that a view that learns more only
    /// adds facts. Left out: unknowns (`"?"`) and nulls, stat stages of 0,
    /// `types` (derived from the forme unless the view tracks a change)
    /// and `effects`. Arrays count element by element.
    pub fn flatten(&self, prefix: &str, out: &mut Vec<(String, String)>) {
        let join = |k: &str| if prefix.is_empty() { k.to_string() } else { format!("{prefix}.{k}") };
        match self {
            Json::Obj(fields) => {
                for (k, v) in fields {
                    if k == "types" || k == "effects" {
                        continue;
                    }
                    if prefix.ends_with(".boosts") && *v == Json::Int(0) {
                        continue;
                    }
                    v.flatten(&join(k), out);
                }
            }
            Json::Arr(items) => {
                for (i, v) in items.iter().enumerate() {
                    v.flatten(&join(&i.to_string()), out);
                }
            }
            Json::Null => {}
            Json::Str(s) if s == UNKNOWN => {}
            other => out.push((prefix.to_string(), other.to_json())),
        }
    }
}

/// The schema's marker for a fact not known.
pub const UNKNOWN: &str = "?";

fn id(name: &str) -> Json {
    Json::Str(to_id(name))
}

fn species_id(dex: &Dex, s: SpeciesId) -> Json {
    id(dex.species(s).name)
}

fn gender(g: Gender) -> Json {
    Json::str(match g {
        Gender::Male => "M",
        Gender::Female => "F",
        Gender::Genderless => "N",
    })
}

fn status(s: StatusKind, fainted: bool) -> Json {
    if fainted {
        return Json::str("fnt");
    }
    match s {
        StatusKind::None => Json::Null,
        StatusKind::Burn => Json::str("brn"),
        StatusKind::Freeze => Json::str("frz"),
        StatusKind::Paralysis => Json::str("par"),
        StatusKind::Poison => Json::str("psn"),
        StatusKind::Toxic => Json::str("tox"),
        StatusKind::Sleep => Json::str("slp"),
    }
}

fn boosts(slot: Option<&ActiveView>) -> Json {
    let get = |b: BoostId| Json::Int(slot.map_or(0, |s| s.boost(b)) as i64);
    Json::obj(vec![
        ("accuracy", get(BoostId::Accuracy)),
        ("atk", get(BoostId::Atk)),
        ("def", get(BoostId::Def)),
        ("evasion", get(BoostId::Evasion)),
        ("spa", get(BoostId::SpA)),
        ("spd", get(BoostId::SpD)),
        ("spe", get(BoostId::Spe)),
    ])
}

fn type_id(t: Type) -> Json {
    // Showdown's typeless `???` (Burn Up, Double Shock) is poke-env's
    // `THREE_QUESTION_MARKS`.
    if t == Type::Unknown {
        return Json::str("threequestionmarks");
    }
    id(t.name())
}

/// Current types: the Tera type once Terastallized (not Stellar), else a
/// changed type set, else the forme's.
fn types(dex: &Dex, forme: SpeciesId, slot: Option<&ActiveView>, tera: Option<Type>) -> Json {
    if let Some(t) = tera
        && t != Type::Stellar
    {
        return Json::Arr(vec![type_id(t)]);
    }
    let set = slot.and_then(|s| s.types).unwrap_or(dex.species(forme).types);
    let mut out = vec![type_id(set.primary)];
    if let Some(t) = set.secondary {
        out.push(type_id(t));
    }
    if let Some(t) = slot.and_then(|s| s.added_type) {
        out.push(type_id(t));
    }
    Json::Arr(out)
}

fn is_mega(dex: &Dex, forme: SpeciesId) -> bool {
    dex.species(forme).forme.starts_with("Mega")
}

fn weather_id(w: Weather) -> &'static str {
    match w {
        Weather::Sun => "sunnyday",
        Weather::Rain => "raindance",
        Weather::Sand => "sandstorm",
        Weather::Snow => "snowscape",
        Weather::HarshSun => "desolateland",
        Weather::HeavyRain => "primordialsea",
        Weather::StrongWinds => "deltastream",
    }
}

fn terrain_id(t: Terrain) -> &'static str {
    match t {
        Terrain::Electric => "electricterrain",
        Terrain::Grassy => "grassyterrain",
        Terrain::Misty => "mistyterrain",
        Terrain::Psychic => "psychicterrain",
    }
}

fn condition_id(c: SideCondition) -> &'static str {
    match c {
        SideCondition::Reflect => "reflect",
        SideCondition::LightScreen => "lightscreen",
        SideCondition::AuroraVeil => "auroraveil",
        SideCondition::Tailwind => "tailwind",
        SideCondition::Safeguard => "safeguard",
        SideCondition::Mist => "mist",
        SideCondition::WideGuard => "wideguard",
        SideCondition::QuickGuard => "quickguard",
        SideCondition::StealthRock => "stealthrock",
        SideCondition::Spikes => "spikes",
        SideCondition::ToxicSpikes => "toxicspikes",
        SideCondition::StickyWeb => "stickyweb",
        SideCondition::FirePledge => "firepledge",
        SideCondition::GrassPledge => "grasspledge",
        SideCondition::WaterPledge => "waterpledge",
    }
}

/// Projects one side's view after each record. It remembers what poke-env
/// keeps across records: the own team's order (the first request's), the
/// foe's team preview, and the last request.
#[derive(Clone, Debug)]
pub struct Projector {
    pub side: SideId,
    /// Per side, the poke-env key of each Pokémon by id.
    keys: [Vec<String>; 2],
    order: Option<Vec<u8>>,
    preview: Option<Vec<Json>>,
    last: Option<OwnOverlay>,
    /// Each own Pokémon as the last request that listed it showed it (a
    /// VGC request after team preview lists only the brought ones).
    known: [Option<OwnMon>; 6],
    /// Each own Pokémon's own moves, in the order the requests showed
    /// them: a request whose moves share none with these (a transformed
    /// Pokémon's) adds nothing.
    moves: [Vec<MoveId>; 6],
    /// Each own Pokémon a request showed Terastallized, until one shows
    /// it standing without (Revival Blessing): a faint deletes the
    /// request's `terastallized`.
    tera: [bool; 6],
    /// The player has an Illusion user: its public records may hold a
    /// disguise's facts.
    illusion: bool,
    /// Per foe row, the own Pokémon (row) it transformed into and the
    /// ability it copied then: the own one may change later (Skill Swap).
    copied: [Option<(u8, pikapro_data::AbilityId)>; 6],
}

impl Projector {
    pub fn new(setup: &Setup, side: SideId) -> Projector {
        let dex = setup.format.dex;
        let names = setup.names();
        let keys = SideId::BOTH.map(|s| {
            let sets = &setup.sheets[s.index()].team.sets;
            sets.iter()
                .enumerate()
                .map(|(i, set)| {
                    let name = names[s.index()].get(i).filter(|n| !n.is_empty()).cloned();
                    let name = name.unwrap_or_else(|| dex.species(dex.species(set.species).base_species).name.into());
                    format!("{}: {name}", pikapro_protocol::side_id(s))
                })
                .collect()
        });
        Projector {
            side,
            keys,
            order: None,
            preview: None,
            last: None,
            known: [None; 6],
            moves: Default::default(),
            tera: [false; 6],
            illusion: false,
            copied: [None; 6],
        }
    }

    /// The state record's `state` after the lines of a record and its
    /// request (`None` for the final record).
    pub fn state(&mut self, setup: &Setup, tracker: &PublicTracker, request: Option<&OwnOverlay>) -> Json {
        let dex = setup.format.dex;
        let me = self.side;
        let foe = me.foe();
        // The own Pokémon before this record's lines (the request comes
        // after them).
        let before = self.known;
        if let Some(r) = request {
            self.last = Some(*r);
            for m in r.mons.iter() {
                if let Some(k) = self.known.get_mut(m.id as usize) {
                    *k = Some(*m);
                }
                self.illusion |= m.base_ability == pikapro_data::gen9::abilities::ILLUSION;
                if let Some(t) = self.tera.get_mut(m.id as usize)
                    && (m.terastallized.is_some() || m.hp > 0)
                {
                    *t = m.terastallized.is_some();
                }
                // A transformed Pokémon's request shows the copied moves.
                let transformed = row_of(tracker, me, m.id).is_some_and(|r| {
                    tracker.side(me).slots.iter().any(|s| s.row == Some(r) && s.transformed.is_some())
                });
                if let Some(own) = self.moves.get_mut(m.id as usize)
                    && !transformed
                    && (own.is_empty() || m.moves.iter().any(|mv| own.contains(mv)))
                {
                    for &mv in m.moves.iter() {
                        if !own.contains(&mv) {
                            own.push(mv);
                        }
                    }
                }
            }
            if self.order.is_none() {
                self.order = Some(r.mons.iter().map(|m| m.id).collect());
            }
        }
        let own = self.last.unwrap_or(OwnOverlay { side: me, ..OwnOverlay::default() });
        let final_record = request.is_none();

        // The foe's `|poke|` species, as first shown.
        if self.preview.is_none() && tracker.phase != pikapro_view::Phase::Setup {
            let rows = &tracker.side(foe).mons;
            let previewed: Vec<Json> = rows
                .iter()
                .filter(|m| m.flags.has(pikapro_view::MonFlags::PREVIEWED))
                .map(|m| species_id(dex, m.species))
                .collect();
            if !previewed.is_empty() || !tracker.config.team_preview {
                self.preview = Some(previewed);
            }
        }

        let active_n = setup.format.active_per_side();
        let slots_of = |side: SideId, row: u8| tracker.side(side).slots.iter().find(|s| s.row == Some(row));
        let key_of_row = |side: SideId, row: u8| -> Option<String> {
            (0..self.keys[side.index()].len() as u8)
                .find(|&id| row_of(tracker, side, id) == Some(row))
                .map(|id| self.keys[side.index()][id as usize].clone())
        };

        // Own team.
        let preview = matches!(own.request, OwnRequest::TeamPreview { .. });
        let mut team = Vec::new();
        let mut own_active_keys: Vec<String> = Vec::new();
        for &id in self.order.as_deref().unwrap_or(&[]) {
            let Some(mut m) = self.known.get(id as usize).copied().flatten() else { continue };
            let rec = row_of(tracker, me, id).and_then(|r| tracker.side(me).mon(r).map(|v| (r, v)));
            let mut slot = rec.and_then(|(r, v)| (v.is_active() || v.is_fainted()).then(|| slots_of(me, r)).flatten());
            // Under Illusion the public slot names the disguise's record:
            // the player's active Pokémon are the request's first ones.
            if self.illusion && !final_record {
                slot = own
                    .mons
                    .iter()
                    .position(|x| x.id == id)
                    .filter(|&p| p < active_n && m.active)
                    .and_then(|p| tracker.side(me).slots.get(p));
            }
            // The request is sent after the lines; the final record has
            // none, so a faint in its lines comes from the public record.
            if final_record && rec.is_some_and(|(_, v)| v.is_fainted()) {
                m.hp = 0;
            }
            // A switch in the final record's lines moves `active`.
            let active = match rec {
                Some((_, v)) if final_record && v.is_revealed() => v.is_active(),
                _ => m.active,
            };
            let key = self.keys[me.index()][id as usize].clone();
            team.push((key.clone(), self.own_mon(dex, &m, rec.map(|x| x.1), slot, active)));
            if active {
                own_active_keys.push(key);
            }
        }

        // Foe's team: rows that have appeared, in order of appearance.
        let fs = tracker.side(foe);
        let mut opponent = Vec::new();
        for &row in fs.reveal_order.iter() {
            let Some(rec) = fs.mon(row) else { continue };
            let Some(key) = key_of_row(foe, row) else { continue };
            let slot = slots_of(foe, row).filter(|_| rec.is_active() || rec.is_fainted());
            // A foe transformed into one of the player's own Pokémon has the
            // ability that one had then, which the player knows from its
            // request.
            let into = slot.and_then(|s| s.transformed).filter(|t| t.side == me);
            let kept = self.copied.get(row as usize).copied().flatten();
            let copied = match (into, kept) {
                (Some(t), Some((r, a))) if r == t.row => Some(a),
                (Some(t), _) => {
                    let id = (0..self.keys[me.index()].len() as u8).find(|&id| row_of(tracker, me, id) == Some(t.row));
                    let at = |k: &[Option<OwnMon>; 6]| id.and_then(|id| k.get(id as usize).copied().flatten());
                    at(&before).or_else(|| at(&self.known)).map(|m| m.ability)
                }
                (None, _) => None,
            };
            if let Some(c) = self.copied.get_mut(row as usize) {
                *c = into.zip(copied).map(|(t, a)| (t.row, a));
            }
            opponent.push((key, foe_mon(dex, rec, slot, copied)));
        }

        // poke-env's singles `active_pokemon` is the first Pokémon flagged
        // active (fainted or not); its doubles one is by slot, empty for a
        // fainted Pokémon.
        let (own_active, foe_active): (Vec<Json>, Vec<Json>) = if active_n == 1 {
            let foe_key = opponent.iter().find(|(_, v)| field_is(v, "active", true)).map(|(k, _)| k.clone());
            (
                vec![own_active_keys.first().filter(|_| !preview).cloned().map_or(Json::Null, Json::Str)],
                vec![foe_key.map_or(Json::Null, Json::Str)],
            )
        } else {
            let by_slot = |side: SideId, slot: usize| {
                let s = tracker.side(side);
                s.slots[slot]
                    .row
                    .filter(|&r| s.mon(r).is_some_and(|v| !v.is_fainted() && v.is_active()))
                    .and_then(|r| key_of_row(side, r))
                    .map_or(Json::Null, Json::Str)
            };
            let own_slots = (0..active_n)
                .map(|slot| {
                    if final_record {
                        return by_slot(me, slot);
                    }
                    own.mons
                        .get(slot)
                        .filter(|m| m.active && m.hp > 0 && !preview)
                        .map_or(Json::Null, |m| Json::Str(self.keys[me.index()][m.id as usize].clone()))
                })
                .collect();
            (own_slots, (0..active_n).map(|slot| by_slot(foe, slot)).collect())
        };

        let force: Vec<Json> = match own.request {
            OwnRequest::ForceSwitch { slots } => slots.iter().take(active_n).map(|&b| Json::Bool(b)).collect(),
            _ => vec![Json::Bool(false); active_n],
        };
        let won = match tracker.outcome {
            Some(Outcome::Win(s)) => Json::Bool(s == me),
            _ => Json::Null,
        };
        let field = &tracker.field;
        let weather = field
            .weather
            .map_or(Json::Null, |w| Json::obj(vec![("name", Json::str(weather_id(w))), ("since", Json::str(UNKNOWN))]));
        let mut fields = Vec::new();
        if let Some(t) = field.terrain {
            fields.push((terrain_id(t).to_string(), Json::str(UNKNOWN)));
        }
        for (on, name) in [
            (field.trick_room.is_some(), "trickroom"),
            (field.gravity.is_some(), "gravity"),
            (field.magic_room.is_some(), "magicroom"),
            (field.wonder_room.is_some(), "wonderroom"),
        ] {
            if on {
                fields.push((name.to_string(), Json::str(UNKNOWN)));
            }
        }
        let conditions = |side: SideId| {
            let s = tracker.side(side);
            Json::Obj(
                SideCondition::ALL
                    .into_iter()
                    .filter(|&c| s.condition(c).layers > 0)
                    .map(|c| {
                        let value = match c {
                            SideCondition::Spikes | SideCondition::ToxicSpikes => {
                                Json::Int(s.condition(c).layers as i64)
                            }
                            _ => Json::str(UNKNOWN),
                        };
                        (condition_id(c).to_string(), value)
                    })
                    .collect(),
            )
        };

        Json::obj(vec![
            ("role", Json::str(pikapro_protocol::side_id(me))),
            ("turn", Json::Int(tracker.turn as i64)),
            ("finished", Json::Bool(tracker.phase == Phase::Over)),
            ("won", won),
            ("teampreview", Json::Bool(matches!(own.request, OwnRequest::TeamPreview { .. }))),
            ("wait", Json::Bool(own.request == OwnRequest::Wait)),
            ("force_switch", Json::Arr(force)),
            ("used_tera", Json::Bool(tracker.side(me).tera_used)),
            ("opponent_used_tera", Json::Bool(fs.tera_used)),
            ("used_mega", Json::Bool(tracker.side(me).mega_used)),
            ("opponent_used_mega", Json::Bool(fs.mega_used)),
            ("weather", weather),
            ("fields", Json::Obj(fields)),
            ("side_conditions", conditions(me)),
            ("opponent_side_conditions", conditions(foe)),
            ("active", Json::Arr(own_active)),
            ("opponent_active", Json::Arr(foe_active)),
            ("team", Json::Obj(team)),
            ("opponent_team", Json::Obj(opponent)),
            ("opponent_preview", Json::Arr(self.preview.clone().unwrap_or_default())),
        ])
    }

    /// One of the player's own Pokémon: the request's facts, the public
    /// record's for the rest.
    fn own_mon(&self, dex: &Dex, m: &OwnMon, rec: Option<&MonView>, slot: Option<&ActiveView>, active: bool) -> Json {
        let moves = self.moves.get(m.id as usize).map_or(&[][..], Vec::as_slice);
        let forme = match rec {
            Some(r) if r.is_active() => r.species,
            _ => m.species,
        };
        let fainted = m.hp == 0;
        Json::obj(vec![
            ("species", species_id(dex, m.species)),
            ("forme", species_id(dex, forme)),
            ("level", Json::Int(m.level as i64)),
            ("gender", gender(m.gender)),
            ("hp", Json::Int(m.hp as i64)),
            ("max_hp", Json::Int(m.max_hp as i64)),
            ("status", status(m.status, fainted)),
            ("boosts", boosts(slot)),
            ("item", m.item.map_or(Json::Null, |i| id(dex.item(i).name))),
            ("ability", id(dex.ability(m.ability).name)),
            ("moves", Json::Arr(moves.iter().map(|&mv| id(dex.move_(mv).name)).collect())),
            ("types", types(dex, forme, slot, m.terastallized)),
            ("tera_type", type_id(m.tera_type)),
            (
                "terastallized",
                // The request's, which a faint deletes (then the last one
                // that showed it; not the public record's, which under
                // Illusion may be a disguise's).
                Json::Bool(
                    m.terastallized.is_some()
                        || (fainted
                            && (self.tera.get(m.id as usize).copied().unwrap_or(false)
                                || (!self.illusion
                                    && rec.is_some_and(|r| r.flags.has(pikapro_view::MonFlags::TERASTALLIZED))))),
                ),
            ),
            ("mega", Json::Bool(is_mega(dex, forme))),
            ("fainted", Json::Bool(fainted)),
            ("active", Json::Bool(active)),
            ("revealed", Json::Bool(rec.is_some_and(|r| r.is_revealed()))),
            ("effects", Json::Obj(Vec::new())),
        ])
    }
}

/// Whether the object `v` has `field` equal to `value`.
fn field_is(v: &Json, field: &str, value: bool) -> bool {
    matches!(v, Json::Obj(fields) if fields.iter().any(|(k, x)| k == field && *x == Json::Bool(value)))
}

/// A foe's Pokémon, from its public record.
fn foe_mon(dex: &Dex, rec: &MonView, slot: Option<&ActiveView>, copied: Option<pikapro_data::AbilityId>) -> Json {
    let fainted = rec.is_fainted();
    let tera = rec.tera_type.filter(|_| rec.flags.has(pikapro_view::MonFlags::TERASTALLIZED));
    let item = match rec.item {
        ItemState::Unknown => Json::str(UNKNOWN),
        ItemState::Known(i) => id(dex.item(i).name),
        ItemState::NoItem | ItemState::Gone { .. } => Json::Null,
    };
    let ability = match (rec.ability, copied) {
        (AbilityState::Known(a), _) | (AbilityState::Unknown, Some(a)) => id(dex.ability(a).name),
        (AbilityState::Unknown, None) => Json::str(UNKNOWN),
    };
    Json::obj(vec![
        ("species", species_id(dex, rec.details_species)),
        ("forme", species_id(dex, rec.species)),
        ("level", Json::Int(rec.level as i64)),
        ("gender", gender(rec.gender)),
        ("hp", Json::Int(rec.hp.hp as i64)),
        ("max_hp", Json::Int(rec.hp.max as i64)),
        ("status", status(rec.status, fainted)),
        ("boosts", boosts(slot)),
        ("item", item),
        ("ability", ability),
        ("moves", Json::Arr(rec.moves.iter().map(|mv| id(dex.move_(mv.id).name)).collect())),
        ("types", types(dex, rec.species, slot, tera)),
        ("tera_type", rec.tera_type.map_or(Json::str(UNKNOWN), type_id)),
        ("terastallized", Json::Bool(rec.flags.has(pikapro_view::MonFlags::TERASTALLIZED))),
        ("mega", Json::Bool(is_mega(dex, rec.species))),
        ("fainted", Json::Bool(fainted)),
        ("active", Json::Bool(rec.is_active())),
        ("revealed", Json::Bool(rec.is_revealed())),
        ("effects", Json::Obj(Vec::new())),
    ])
}
