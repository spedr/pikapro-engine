//! Check 3's input side: decision records and our state records
//! (`tools/view-oracle/SCHEMA.md`).
//!
//! After every update each side gets a **decision record**: the lines its
//! channel shows since its previous record (the first starts with the
//! header), then the request it is sent (`null` in the final record, once
//! the battle is over). Next to it, a **state record** holds our view after
//! those lines and that request, projected onto poke-env's normalised
//! state ([`super::project`]). `tools/view-oracle/pokeenv_view.py` turns
//! the decision records into poke-env's state records, and
//! `tools/view-oracle/diff/diff.py` compares the two.
//!
//! Battles follow the schema's generator recipe (`gen_decisions.py`):
//! battle seed `s`, teams `pikapro.RandomTeams(format, mode).pair(s)`, choices
//! from `RandomPolicy(s ^ 0x5EED)`, side 1 then side 2 after each update
//! ([`Driver::advance`](super::Driver::advance)), so both write the same
//! records for the same battle.

use pikapro::SideId;
use pikapro_protocol::Perspective;

use super::project::{Json, Projector};
use super::{Checks, Driver, Failure, Played, Setup, overlays, play};

/// A battle's records, as JSON Lines.
#[derive(Clone, Debug, Default)]
pub struct Dump {
    pub decisions: String,
    pub states: String,
    pub records: usize,
}

/// Plays `setup`'s battle with `driver`, running `checks`, and writes its
/// records. On a failure, the records up to it are returned with it.
pub fn dump(setup: &Setup, driver: Driver, checks: Checks) -> (Dump, Result<Played, Failure>) {
    let mut out = Dump::default();
    let renderers = SideId::BOTH.map(|s| setup.renderer(Perspective::Player(s)));
    let mut projectors = SideId::BOTH.map(|s| Projector::new(setup, s));
    let header = setup.renderer(Perspective::Player(SideId::P1)).header();
    let teams = setup.exported();
    let result = play(setup, driver, checks, |u| {
        let over = u.battle.is_over();
        let own = overlays(u.battle);
        for side in SideId::BOTH {
            let mut lines = if u.index == 0 { header.clone() } else { Vec::new() };
            lines.extend(renderers[side.index()].log(&u.battle.events()[u.start..]));
            let request = (!over).then(|| renderers[side.index()].request_json(&u.battle.request_data(side)));
            let head = |extra: Vec<(&str, Json)>| {
                let mut fields = vec![
                    ("format", Json::str(setup.format.id)),
                    ("seed", Json::Int(setup.seed as i64)),
                    ("side", Json::str(pikapro_protocol::side_id(side))),
                    ("index", Json::Int(u.index as i64)),
                    ("turn", Json::Int(u.battle.turn() as i64)),
                ];
                fields.extend(extra);
                Json::obj(fields)
            };
            let mut decision = vec![
                ("lines", Json::Arr(lines.into_iter().map(Json::Str).collect())),
                ("request", request.map_or(Json::Null, Json::Str)),
            ];
            if u.index == 0 {
                decision.push(("teams", Json::Arr(teams.iter().cloned().map(Json::Str).collect())));
            }
            head(decision).write(&mut out.decisions);
            out.decisions.push('\n');
            let state = projectors[side.index()].state(setup, u.tracker, (!over).then_some(&own[side.index()]));
            head(vec![("state", state)]).write(&mut out.states);
            out.states.push('\n');
            out.records += 1;
        }
        Ok(())
    });
    (out, result)
}
