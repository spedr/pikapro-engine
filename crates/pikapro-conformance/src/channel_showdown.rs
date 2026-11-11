//! The channel parser against Showdown's own text
//! (`cargo channel-conformance --showdown`).
//!
//! A conformance scenario (random teams and choices, as `cargo
//! conformance` draws them) is played on Showdown, whose player streams
//! (`getPlayerStreams`, `tools/showdown-oracle/players.mjs`) are fed to one
//! [`ChannelSession`] per player. At each of a player's decisions (each
//! request it must answer) the session's view, observation and mask are
//! compared with the typed path's for the same battle on pikapro, which
//! matches Showdown's log by conformance. This catches every line our renderer
//! writes differently from Showdown, and every line only Showdown sends.

use pikapro::{Battle, Progress, SideId};
use pikapro_protocol::BattleInfo;
use pikapro_protocol::channel::LineKind;
use pikapro_rl::encode::{Basic, Encoder};
use pikapro_rl::session::{ChannelSession, SessionOptions};
use pikapro_rl::space::ActionSpace;
use pikapro_rl::vec::{MaskLayout, SwitchBy};
use pikapro_view::{OwnOverlay, PublicTracker};

use crate::channel::{Options, compare_tracker, indistinguishable, named_alike, overlay_eq, shared_names};
use crate::view::{Failure, debug_diff};
use crate::{OracleError, Scenario};

/// What a player must see at one of its decisions (the typed path).
#[derive(Clone)]
struct Snapshot {
    tracker: PublicTracker,
    own: OwnOverlay,
    obs: Vec<f32>,
    mask: Vec<bool>,
    min_switches: u8,
}

/// The mask the environment writes for `side` from the battle: of the
/// actions [`pikapro_rl::env::shown`] gives (the environment's own: the
/// engine's legal actions, or what the request shows when it may hide a
/// trap or a disable, less the switches a trapping ability the view has
/// revealed forbids).
fn env_mask(
    battle: &Battle,
    own: &OwnOverlay,
    tracker: &PublicTracker,
    side: SideId,
    space: &ActionSpace,
    options: &Options,
    out: &mut [bool],
) -> u8 {
    use pikapro::Request;
    let legal = pikapro_rl::env::shown(battle, own, tracker, side);
    out.fill(false);
    let slots = space.active * space.slot_size;
    let bench = battle.state().side(side).bench().count();
    let index = |pos: u8| match options.switch_by {
        SwitchBy::Position => Some(pos as usize),
        SwitchBy::Row => own.mons.get(pos as usize).map(|m| m.id as usize),
    };
    match (options.masks, legal.request()) {
        (_, Request::Wait) => 0,
        (layout, Request::TeamPreview { .. }) => {
            let off = if layout == MaskLayout::Factored { slots } else { space.joint_size };
            let team = battle.state().side(side).team.len();
            space.fill_preview_mask(&legal, team, &mut out[off..]);
            0
        }
        (MaskLayout::Factored, _) => space.fill_slot_mask_by(&legal, bench, index, &mut out[..slots]),
        (MaskLayout::Joint, _) => {
            let mut mask = vec![false; slots];
            let min = space.fill_slot_mask_by(&legal, bench, index, &mut mask);
            space.fill_joint_from_slots(&mask, min, &mut out[..space.joint_size]);
            min
        }
    }
}

/// The typed path's decisions of a scenario, by side: after each
/// resolution (and at the start), each side with a request to answer.
fn typed_decisions(s: &Scenario, options: &Options) -> Result<[Vec<Snapshot>; 2], Failure> {
    use pikapro_rl::tables::Tables;
    let format = &s.format;
    let space = ActionSpace::new(format);
    let tables = Tables::for_format(format);
    let config = pikapro_view::ViewConfig::new(format).with_hp(options.hp);
    let mut tracker = PublicTracker::new(config);
    let mut battle = crate::new_battle(format, &s.teams, s.seed);
    let names = s.name_table();
    let width = options.masks.width(&space);
    let mut out: [Vec<Snapshot>; 2] = [Vec::new(), Vec::new()];
    // Each player's team, as the environment's and the sessions' (which
    // are given it).
    let mut teams = SideId::BOTH.map(|side| pikapro_view::OwnTeam::from_team(&s.teams[side.index()], format));
    let mut take = |battle: &mut Battle, tracker: &mut PublicTracker, out: &mut [Vec<Snapshot>; 2]| {
        for e in battle.drain_events() {
            tracker.apply(&e);
        }
        if battle.is_over() {
            return;
        }
        for side in SideId::BOTH {
            if battle.request(side) == pikapro::Request::Wait {
                continue;
            }
            let own = OwnOverlay::from_battle(battle, side);
            let team = &mut teams[side.index()];
            team.observe(&own);
            let view = pikapro_view::PlayerView::new(side, tracker, &own).with_team(team);
            let mut obs = vec![0.0; Basic::SIZE];
            Basic.encode(&view, &tables, &mut obs);
            let mut mask = vec![false; width];
            let min_switches = env_mask(battle, &own, tracker, side, &space, options, &mut mask);
            out[side.index()].push(Snapshot { tracker: *tracker, own, obs, mask, min_switches });
        }
    };
    take(&mut battle, &mut tracker, &mut out);
    for (side, text) in &s.choices {
        let choice = pikapro_protocol::parse_choice_for(&battle, *side, text, &names[side.index()])
            .map_err(|e| Failure::new("engine", "choice", format!("recorded choice {text:?}: {e}")))?;
        match battle.submit(*side, choice) {
            Ok(Progress::Resolved) => take(&mut battle, &mut tracker, &mut out),
            Ok(Progress::Waiting) => {}
            Err(e) => return Err(Failure::new("engine", "choice", format!("{text:?}: {e}"))),
        }
    }
    Ok(out)
}

/// Each player's stream from Showdown's `getPlayerStreams`
/// (`tools/showdown-oracle/players.mjs`), as lines.
pub fn showdown_streams(s: &Scenario) -> Result<[Vec<String>; 2], OracleError> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let formatid = crate::showdown_format(&s.format)
        .ok_or_else(|| OracleError::Unavailable(format!("no Showdown format for {}", s.format.id)))?;
    let json = crate::scenario_json(s, formatid);
    let mut child = Command::new("node")
        .arg(crate::oracle_dir().join("players.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| OracleError::Unavailable(e.to_string()))?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(json.as_bytes())
        .map_err(|e| OracleError::Unavailable(e.to_string()))?;
    let out = child.wait_with_output().map_err(|e| OracleError::Unavailable(e.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(OracleError::Failed {
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            log: Vec::new(),
        });
    }
    let bad = |what: &str| OracleError::Failed { status: None, stderr: what.to_string(), log: Vec::new() };
    let root = pikapro_protocol::channel::json::Value::parse(text.trim()).map_err(|e| bad(&e.to_string()))?;
    let mut streams: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    for side in SideId::BOTH {
        let chunks = root.get(pikapro_protocol::side_id(side)).ok_or_else(|| bad("no stream"))?;
        for chunk in chunks.items() {
            let chunk = chunk.map_err(|e| bad(&e.to_string()))?;
            let text = chunk.as_str().ok_or_else(|| bad("a chunk isn't a string"))?.text().into_owned();
            streams[side.index()].extend(text.split('\n').map(str::to_owned));
        }
    }
    Ok(streams)
}

/// What a battle's Showdown check compared.
#[derive(Clone, Debug, Default)]
pub struct ShowdownStats {
    pub decisions: u64,
    pub lines: u64,
    /// Lines the sessions didn't know, and names the dex didn't.
    pub unknown_lines: u64,
    pub unknown_names: u64,
    /// Sessions that joined mid-battle (`Options::reconnect`), compared
    /// and not (the lines fit two team orders).
    pub rejoins: u64,
    pub ambiguous_rejoins: u64,
    /// Not compared: a side has two Pokémon its channel shows identically
    /// (`channel::indistinguishable`), or from where a line named one of
    /// two alike (`channel::named_alike`).
    pub skipped: bool,
}

/// Feeds Showdown's player streams of a scenario to sessions and compares,
/// at every decision, the view, observation and mask with pikapro's
/// typed path of the same battle. A failure says where it happened: the
/// side, and how many of its decisions its session had passed (the
/// failure is at that decision, or in the lines before it). `None`:
/// pikapro's typed path failed.
pub fn check_at(
    s: &Scenario,
    streams: &[Vec<String>; 2],
    options: &Options,
) -> Result<ShowdownStats, (Failure, Option<(SideId, usize)>)> {
    let mut stats = ShowdownStats::default();
    let battle = crate::new_battle(&s.format, &s.teams, s.seed);
    if indistinguishable(&battle, &s.name_table()) {
        stats.skipped = true;
        return Ok(stats);
    }
    let typed = typed_decisions(s, options).map_err(|f| (f, None))?;
    let exact_hp = BattleInfo::for_format(&s.format).report_exact_hp;
    for side in SideId::BOTH {
        let session_options = SessionOptions::default()
            .with_masks(options.masks)
            .with_switch_by(options.switch_by)
            .with_hp(options.hp)
            .with_username(Some(pikapro_protocol::side_id(side).to_string()))
            .with_team(Some(s.teams[side.index()].clone()));
        let mut session = ChannelSession::for_format(s.format.clone(), exact_hp, session_options)
            .map_err(|e| (Failure::new("engine", "session", e), None))?;
        let mut k = 0usize;
        let mut log: Vec<String> = Vec::new();
        for line in &streams[side.index()] {
            stats.lines += 1;
            log.push(line.clone());
            let kind = session.feed(line);
            if kind == LineKind::Unknown {
                let f = Failure::new("showdown", "unknown-line", format!("{side:?}: {line:?}")).with_context(&log);
                return Err((f, Some((side, k))));
            }
            if let Some(e) = session.error() {
                let f = Failure::new("showdown", "request", format!("{side:?}: {e}")).with_context(&log);
                return Err((f, Some((side, k))));
            }
            if kind != LineKind::Request || !session.needs_action() {
                continue;
            }
            let fail = |fact: &str, m: String| {
                let f = Failure::new("showdown", fact.to_string(), format!("{side:?} decision {k}: {m}"));
                (f.with_context(&log), Some((side, k)))
            };
            let Some(want) = typed[side.index()].get(k) else {
                return Err(fail("decisions", "more decisions than the typed path".into()));
            };
            if let Err(m) = compare_tracker(&want.tracker, session.tracker(), side) {
                // A name two Pokémon share, named off the field: no client
                // can tell which one it is (`channel::named_alike`).
                if named_alike(&log, &shared_names(&battle, &s.name_table())) {
                    stats.skipped = true;
                    return Ok(stats);
                }
                return Err(fail("tracker", m));
            }
            if !overlay_eq(&want.own, session.overlay()) {
                return Err(fail("overlay", debug_diff(&want.own, session.overlay())));
            }
            if session.obs() != &want.obs[..] {
                return Err(fail("obs", "values differ".into()));
            }
            if session.mask() != &want.mask[..] || session.min_switches() != want.min_switches {
                return Err(fail("mask", "differs".into()));
            }
            if options.reconnect {
                let compared = crate::channel::check_rejoin(&mut session, &log, side, options)
                    .map_err(|f| (f, Some((side, k))))?;
                if compared {
                    stats.rejoins += 1;
                } else {
                    stats.ambiguous_rejoins += 1;
                }
            }
            stats.decisions += 1;
            k += 1;
        }
        if k != typed[side.index()].len() {
            let f = Failure::new(
                "showdown",
                "decisions",
                format!("{side:?}: {k} decisions, the typed path has {}", typed[side.index()].len()),
            );
            return Err((f.with_context(&log), Some((side, k))));
        }
        stats.unknown_lines += session.unknown_lines() as u64;
        stats.unknown_names += session.parser().unknown_names() as u64;
    }
    Ok(stats)
}
