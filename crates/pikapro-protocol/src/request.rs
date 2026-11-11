//! Showdown's `|request|` JSON.
//!
//! [`Renderer::request_json`] writes a [`RequestData`] exactly as Showdown's
//! simulator sends it to a player: `JSON.stringify` of the object
//! `battle.getRequests` builds, with its key order and its `undefined`
//! fields left out. The simulator has no `rqid`; Showdown's server adds one
//! as the last key (pikapro-showdown-server does).
//!
//! Idents and details come from the log renderer, so nicknames and
//! genders show the same way in both.

use pikapro::event::Actor;
use pikapro::request::{ActiveRequest, MoveChoices, PokemonRequest, RequestData, RequestKind};
use pikapro::{Mechanics, Position};
use pikapro_data::{MoveTarget, Stat};

use crate::js::{push_bool, push_json_body, push_json_string, push_uint};
use crate::log::{Renderer, details, side_id, status_id};

impl Renderer {
    /// The request as Showdown's simulator sends it (the text after
    /// `|request|`).
    pub fn request_json(&self, data: &RequestData) -> String {
        let mut out = String::new();
        self.write_request(data, &mut out);
        out
    }

    /// Appends the request's JSON to `out`. (Showdown's server adds an
    /// `rqid` key to what it relays; pikapro-showdown-server does that.)
    pub fn write_request(&self, data: &RequestData, out: &mut String) {
        out.push('{');
        match &data.kind {
            RequestKind::Wait => out.push_str("\"wait\":true"),
            RequestKind::TeamPreview { max_chosen_team_size } => {
                // `{ teamPreview: true, maxChosenTeamSize, side }`:
                // `pickedTeamSize || undefined`, and JSON leaves an
                // undefined key out.
                out.push_str("\"teamPreview\":true");
                if let Some(n) = max_chosen_team_size {
                    out.push_str(",\"maxChosenTeamSize\":");
                    push_uint(out, u32::from(*n));
                }
            }
            RequestKind::Move { active } => {
                out.push_str("\"active\":[");
                for (i, slot) in active.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    match slot {
                        Some(a) => self.write_active(a, out),
                        None => out.push_str("null"),
                    }
                }
                out.push(']');
            }
            RequestKind::ForceSwitch { slots } => {
                out.push_str("\"forceSwitch\":[");
                for (i, &f) in slots.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    push_bool(out, f);
                }
                out.push(']');
            }
        }
        out.push_str(",\"side\":");
        self.write_side(data, out);
        if data.no_cancel {
            out.push_str(",\"noCancel\":true");
        }
        out.push('}');
    }

    /// `pokemon.getMoveRequestData()`.
    fn write_active(&self, a: &ActiveRequest, out: &mut String) {
        let dex = self.dex;
        out.push_str("{\"moves\":[");
        match &a.moves {
            MoveChoices::Struggle => out
                .push_str("{\"move\":\"Struggle\",\"id\":\"struggle\",\"target\":\"randomNormal\",\"disabled\":false}"),
            MoveChoices::Recharge => out.push_str("{\"move\":\"Recharge\",\"id\":\"recharge\"}"),
            MoveChoices::Locked(m) => {
                let name = dex.move_(*m).name;
                out.push_str("{\"move\":");
                push_json_string(out, name);
                out.push_str(",\"id\":");
                push_id(out, name);
                out.push('}');
            }
            MoveChoices::Moves(moves) => {
                for (i, m) in moves.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    let name = dex.move_(m.id).name;
                    out.push_str("{\"move\":");
                    push_json_string(out, name);
                    out.push_str(",\"id\":");
                    push_id(out, name);
                    out.push_str(",\"pp\":");
                    push_uint(out, u32::from(m.pp));
                    out.push_str(",\"maxpp\":");
                    push_uint(out, u32::from(m.max_pp));
                    out.push_str(",\"target\":\"");
                    out.push_str(target_id(m.target));
                    out.push_str("\",\"disabled\":");
                    push_bool(out, m.disabled);
                    out.push('}');
                }
            }
        }
        out.push(']');
        for (set, key) in [
            (a.maybe_disabled, "maybeDisabled"),
            (a.maybe_locked, "maybeLocked"),
            (a.trapped, "trapped"),
            (a.maybe_trapped, "maybeTrapped"),
            (a.can_mega_evo, "canMegaEvo"),
        ] {
            if set {
                out.push_str(",\"");
                out.push_str(key);
                out.push_str("\":true");
            }
        }
        if let Some(t) = a.can_terastallize {
            out.push_str(",\"canTerastallize\":\"");
            out.push_str(t.name());
            out.push('"');
        }
        out.push('}');
    }

    /// `side.getRequestData()`.
    fn write_side(&self, data: &RequestData, out: &mut String) {
        out.push_str("{\"name\":");
        push_json_string(out, &self.info.players[data.side.index()].name);
        out.push_str(",\"id\":\"");
        out.push_str(side_id(data.side));
        out.push_str("\",\"pokemon\":[");
        for (i, p) in data.pokemon.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            self.write_pokemon(data, p, out);
        }
        out.push_str("]}");
    }

    /// `pokemon.getSwitchRequestData()`.
    fn write_pokemon(&self, data: &RequestData, p: &PokemonRequest, out: &mut String) {
        let dex = self.dex;
        // `fullname`: the side and the name, never the slot.
        // Its own name (`pokemon.fullname`, not an Illusion's).
        let actor = Actor { pos: Position::new(data.side, 0), species: p.species, active: false, id: p.id };
        out.push_str("{\"ident\":\"");
        let ident = self.ident(actor);
        ident.push_prefix(out);
        push_json_body(out, ident.name());
        out.push_str("\",\"details\":\"");
        // `details` (the species' name, the level and gender need no escape).
        push_json_body(out, &details(dex, p.species, p.level, p.gender));
        if self.is_shiny(data.side, p.id) {
            out.push_str(", shiny");
        }
        out.push('"');
        // `getHealth().secret`.
        if p.hp == 0 {
            out.push_str(",\"condition\":\"0 fnt\"");
        } else {
            out.push_str(",\"condition\":\"");
            push_uint(out, u32::from(p.hp));
            out.push('/');
            push_uint(out, u32::from(p.max_hp));
            if p.status != pikapro::Status::None {
                out.push(' ');
                out.push_str(status_id(p.status));
            }
            out.push('"');
        }
        out.push_str(",\"active\":");
        push_bool(out, p.active);
        out.push_str(",\"stats\":{");
        for (i, (stat, key)) in
            [(Stat::Atk, "atk"), (Stat::Def, "def"), (Stat::SpA, "spa"), (Stat::SpD, "spd"), (Stat::Spe, "spe")]
                .into_iter()
                .enumerate()
        {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(key);
            out.push_str("\":");
            push_uint(out, u32::from(p.stats[stat]));
        }
        out.push_str("},\"moves\":[");
        for (i, &m) in p.moves.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            push_id(out, dex.move_(m).name);
        }
        out.push_str("],\"baseAbility\":");
        push_id(out, dex.ability(p.base_ability).name);
        out.push_str(",\"item\":");
        push_id(out, p.item.map_or("", |i| dex.item(i).name));
        out.push_str(",\"pokeball\":");
        // An ID: no escape.
        out.push('"');
        self.push_pokeball(out, data.side, p.id);
        out.push('"');
        out.push_str(",\"ability\":");
        push_id(out, dex.ability(p.ability).name);
        out.push_str(",\"commanding\":");
        push_bool(out, p.commanding);
        out.push_str(",\"reviving\":");
        push_bool(out, p.reviving);
        // Gen 9, except the Champions mod.
        if self.mechanics != Mechanics::Champions {
            out.push_str(",\"teraType\":\"");
            out.push_str(p.tera_type.name());
            out.push_str("\",\"terastallized\":\"");
            out.push_str(p.terastallized.map_or("", |t| t.name()));
            out.push('"');
        }
        out.push('}');
    }
}

/// Showdown's name for a move target type.
pub(crate) fn target_id(target: MoveTarget) -> &'static str {
    match target {
        MoveTarget::Normal => "normal",
        MoveTarget::AdjacentFoe => "adjacentFoe",
        MoveTarget::AdjacentAlly => "adjacentAlly",
        MoveTarget::AdjacentAllyOrSelf => "adjacentAllyOrSelf",
        MoveTarget::Any => "any",
        MoveTarget::AllAdjacentFoes => "allAdjacentFoes",
        MoveTarget::AllAdjacent => "allAdjacent",
        MoveTarget::User => "self",
        MoveTarget::Allies => "allies",
        MoveTarget::AllySide => "allySide",
        MoveTarget::FoeSide => "foeSide",
        MoveTarget::All => "all",
        MoveTarget::RandomNormal => "randomNormal",
        MoveTarget::AllyTeam => "allyTeam",
        MoveTarget::Scripted => "scripted",
    }
}

/// A name's Showdown ID (`toID`) as a JSON string: it needs no escaping.
fn push_id(out: &mut String, name: &str) {
    out.push('"');
    out.extend(pikapro_data::id_chars(name));
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Perspective;
    use pikapro::testing::sample_team;
    use pikapro::{Battle, BattleOptions, Format, Seed, SideId};

    #[test]
    fn ids_are_json_strings() {
        let mut id = String::new();
        push_id(&mut id, "King's Shield");
        assert_eq!(id, "\"kingsshield\"");
    }

    #[test]
    fn renders_a_team_preview_and_a_move_request() {
        let format = Format::gen9_vgc();
        let teams = [sample_team(&format), sample_team(&format)];
        let battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(1))).unwrap();
        let renderer = Renderer::new(&format, Perspective::Player(SideId::P1));
        let json = renderer.request_json(&battle.request_data(SideId::P1));
        assert!(
            json.starts_with("{\"teamPreview\":true,\"maxChosenTeamSize\":4,\"side\":{\"name\":\"p1\",\"id\":\"p1\"")
        );
        assert!(json.contains("\"teraType\":"));
        assert!(!json.contains("noCancel"));
    }

    /// Without a picked team size (gen9ou), Showdown's request has no
    /// `maxChosenTeamSize` and its log `|teampreview` without a count; the
    /// request reads back as it was.
    #[test]
    fn a_team_preview_without_a_picked_size_has_no_max_chosen_team_size() {
        let format = Format::gen9_ou();
        let teams = [sample_team(&format), sample_team(&format)];
        let battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(1))).unwrap();
        let renderer = Renderer::new(&format, Perspective::Player(SideId::P1));
        let data = battle.request_data(SideId::P1);
        let json = renderer.request_json(&data);
        assert!(json.starts_with("{\"teamPreview\":true,\"side\":{\"name\":\"p1\",\"id\":\"p1\""), "{json}");
        let read = crate::channel::parse_request(&json, &format).unwrap();
        assert_eq!(read.kind, data.kind);
        let log = renderer.log(battle.events());
        assert!(log.iter().any(|l| l == "|teampreview"), "{log:?}");
        assert_eq!(log.iter().filter(|l| l.starts_with("|poke|")).count(), 12);
    }
}
