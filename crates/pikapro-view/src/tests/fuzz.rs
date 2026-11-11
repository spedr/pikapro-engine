//! Random battles with every check at every decision point: the tracker's
//! soundness against the state ([`check_sound`]), the no-leak rules per
//! event, the overlay's equality, the row mappings, and the
//! refused-choice updates.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use pikapro::event::DamageSource;
use pikapro::testing::sample_team;
use pikapro::{
    Battle, BattleEvent, BattleOptions, BattleRng, Format, Progress, Seed, SideId, SlotAction, Status, Team,
};
use pikapro_protocol::{BattleInfo, Perspective, Renderer};
use pikapro_teams::{Mode, TeamGenerator};

use crate::check::check_sound;
use crate::*;

/// The eight presets (championsbss, gen9ou, then the random battles, last,
/// so the others keep their seeds).
pub(super) fn formats() -> [Format; 8] {
    [
        Format::champions_vgc(),
        Format::gen9_vgc(),
        Format::gen9_singles(),
        Format::gen9_doubles(),
        Format::champions_bss(),
        Format::gen9_ou(),
        Format::gen9_random_battle(),
        Format::gen9_random_doubles_battle(),
    ]
}

/// A team source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Teams {
    Sample,
    Uniform,
    Sets,
}

fn generator(format: &Format, mode: Mode) -> &'static TeamGenerator {
    static CACHE: OnceLock<std::sync::Mutex<BTreeMap<(String, &'static str), &'static TeamGenerator>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    let key = (format.id.to_string(), mode.name());
    cache.entry(key).or_insert_with(|| Box::leak(Box::new(TeamGenerator::new(format, mode).unwrap())))
}

pub(super) fn teams(format: &Format, source: Teams, seed: u64) -> [Team; 2] {
    match source {
        Teams::Sample => [sample_team(format), sample_team(format)],
        Teams::Uniform | Teams::Sets => {
            let mode = if source == Teams::Uniform { Mode::Uniform } else { Mode::Sets };
            let g = generator(format, mode);
            [g.team(seed).unwrap(), g.team(seed ^ 0x5eed).unwrap()]
        }
    }
}

/// What a run looks at.
#[derive(Clone, Copy, Debug)]
pub(super) struct Opts {
    pub hp: HpReport,
    /// Check the per-event no-leak rules (renders every event).
    pub leak: bool,
    /// Open Team Sheets.
    pub ots: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts { hp: HpReport::Percent, leak: true, ots: false }
    }
}

/// What a run saw.
#[derive(Clone, Debug, Default)]
pub(super) struct Seen {
    pub battles: u64,
    pub points: u64,
    pub events: u64,
    /// Events by variant name.
    pub variants: BTreeMap<String, u64>,
    pub rejected: u64,
}

/// Plays one battle with random choices and checks everything; the first
/// failure, with the battle's description.
pub(super) fn run(format: &Format, teams: [Team; 2], seed: u64, opts: Opts, seen: &mut Seen) -> Result<(), String> {
    run_with(format, teams, seed, opts, seen, &mut |b, side, rng| b.legal_actions(side).sample(rng), &mut |_, _| {})
}

/// A choice for a side with a request.
pub(super) type Policy<'a> = &'a mut dyn FnMut(&Battle, SideId, &mut BattleRng) -> pikapro::Choice;

/// [`run`] with the choices of `policy`, and `check` called at every
/// decision point after the built-in checks.
pub(super) fn run_with(
    format: &Format,
    teams: [Team; 2],
    seed: u64,
    opts: Opts,
    seen: &mut Seen,
    policy: Policy<'_>,
    check: &mut dyn FnMut(&Battle, &PublicTracker),
) -> Result<(), String> {
    let mut b =
        Battle::new(format.clone(), teams.clone(), BattleOptions::new(Seed::fast(seed))).map_err(|e| e.to_string())?;
    let mut tracker = PublicTracker::new(ViewConfig::new(format).with_hp(opts.hp));
    let info = BattleInfo { report_exact_hp: opts.hp == HpReport::Exact, ..BattleInfo::for_format(format) };
    let renderer = Renderer::new(format, Perspective::Spectator).with_info(info);
    let mut rng = BattleRng::new(Seed::fast(!seed));
    let mut own = [OwnOverlay::default(); 2];
    let mut team = SideId::BOTH.map(|s| OwnTeam::from_team(&teams[s.index()], format));
    let mut log: Vec<BattleEvent> = Vec::new();
    let fail = |what: String, log: &[BattleEvent]| {
        let tail: Vec<String> = log.iter().rev().take(40).rev().map(|e| renderer.line(e)).collect();
        Err(format!("{} seed {seed}: {what}\n--- last lines:\n{}", format.id, tail.join("\n")))
    };
    let mut first = true;
    seen.battles += 1;
    loop {
        let events: Vec<BattleEvent> = b.drain_events().collect();
        for e in &events {
            seen.events += 1;
            let name = format!("{e:?}");
            let name = name.split([' ', '{', '(']).next().unwrap_or("").to_string();
            *seen.variants.entry(name).or_default() += 1;
            let before = tracker;
            tracker.apply(e);
            log.push(*e);
            if opts.leak
                && let Err(msg) = no_leak(&renderer, &before, &tracker, e, opts.hp)
            {
                return fail(msg, &log);
            }
        }
        if first {
            first = false;
            if opts.ots && format.team_preview.shown() {
                for side in SideId::BOTH {
                    tracker.set_open_sheet(side, OpenSheet::from_team(&teams[side.index()], format));
                }
            }
        }
        seen.points += 1;
        if let Err(v) = check_sound(&tracker, &b) {
            return fail(format!("unsound at turn {}: {v} <{}>", b.turn(), v.what), &log);
        }
        for side in SideId::BOTH {
            if let Err(msg) = check_overlay(&b, &tracker, side, &mut own[side.index()], seen) {
                return fail(msg, &log);
            }
            if let Err(msg) = check_team(&b, &tracker, side, &own[side.index()], &mut team[side.index()]) {
                return fail(msg, &log);
            }
        }
        check(&b, &tracker);
        if b.is_over() {
            return Ok(());
        }
        // Both sides answer the requests of this decision point.
        let choices = SideId::BOTH.map(|side| {
            let legal = b.legal_actions(side);
            (!legal.is_waiting()).then(|| policy(&b, side, &mut rng))
        });
        for (side, choice) in SideId::BOTH.into_iter().zip(choices) {
            let Some(choice) = choice else { continue };
            let progress = b.submit(side, choice).map_err(|e| e.to_string())?;
            // Waiting for the other side: the request stands, answered.
            if progress == Progress::Waiting {
                let o = OwnOverlay::from_battle(&b, side);
                if !o.answered {
                    return fail("an answered request not flagged".into(), &log);
                }
                if o != OwnOverlay::from_request(&b.request_data(side)) {
                    return fail("an answered overlay differs from the request".into(), &log);
                }
            }
        }
    }
}

/// The no-leak rules for one event: a line that shows nothing changes
/// nothing, and the fields a line doesn't show (a switch's team position,
/// a status's counters, exact HP under percentages) don't matter.
fn no_leak(
    renderer: &Renderer,
    before: &PublicTracker,
    after: &PublicTracker,
    e: &BattleEvent,
    hp: HpReport,
) -> Result<(), String> {
    let line = renderer.line(e);
    if line.is_empty() && before != after {
        return Err(format!("{e:?} shows nothing but changed the view"));
    }
    for p in perturbations(e, hp) {
        if renderer.line(&p) != line {
            continue;
        }
        let mut other = *before;
        other.apply(&p);
        if other != *after {
            return Err(format!("{e:?} and {p:?} show the same line ({line}) but differ in the view"));
        }
    }
    Ok(())
}

/// Copies of `e` differing only in what its line doesn't show.
fn perturbations(e: &BattleEvent, hp: HpReport) -> Vec<BattleEvent> {
    let mut out = Vec::new();
    let bump = |s: Status| match s {
        Status::Sleep { turns } => Status::Sleep { turns: turns.wrapping_add(1) },
        Status::Toxic { counter } => Status::Toxic { counter: counter.wrapping_add(1) },
        Status::Freeze { turns } => Status::Freeze { turns: turns.wrapping_add(1) },
        other => other,
    };
    let scale = |h: u16, m: u16| if hp == HpReport::Percent && m < 30000 { (h * 2, m * 2) } else { (h, m) };
    let mut p = *e;
    match &mut p {
        BattleEvent::Switch { team_index, status, hp, max_hp, .. }
        | BattleEvent::Drag { team_index, status, hp, max_hp, .. } => {
            *team_index = (*team_index + 1) % 6;
            *status = bump(*status);
            (*hp, *max_hp) = scale(*hp, *max_hp);
        }
        BattleEvent::Damage { status, hp, max_hp, source, .. } => {
            *status = bump(*status);
            if let DamageSource::Status(s) = source {
                *s = bump(*s);
            }
            (*hp, *max_hp) = scale(*hp, *max_hp);
        }
        BattleEvent::Heal { status, hp, max_hp, .. }
        | BattleEvent::EffectDamage { status, hp, max_hp, .. }
        | BattleEvent::EffectHeal { status, hp, max_hp, .. }
        | BattleEvent::ConditionDamage { status, hp, max_hp, .. }
        | BattleEvent::MoveHeal { status, hp, max_hp, .. }
        | BattleEvent::WishHeal { status, hp, max_hp, .. }
        | BattleEvent::SetHp { status, hp, max_hp, .. }
        | BattleEvent::ManipulationHeal { status, hp, max_hp, .. }
        | BattleEvent::HealSilent { status, hp, max_hp, .. }
        | BattleEvent::HealFromSilent { status, hp, max_hp, .. } => {
            *status = bump(*status);
            (*hp, *max_hp) = scale(*hp, *max_hp);
        }
        BattleEvent::StatusStart { status, .. }
        | BattleEvent::StatusFrom { status, .. }
        | BattleEvent::CureStatus { status, .. }
        | BattleEvent::CureStatusFrom { status, .. }
        | BattleEvent::StatusCant { status, .. }
        | BattleEvent::StatusFail { status, .. } => *status = bump(*status),
        _ => return out,
    }
    if p != *e {
        out.push(p);
    }
    out
}

/// The overlay: equal to the request's (built from the battle), mapped
/// to rows, and updated by refused choices as Showdown
/// does.
fn check_overlay(
    b: &Battle,
    tracker: &PublicTracker,
    side: SideId,
    own: &mut OwnOverlay,
    seen: &mut Seen,
) -> Result<(), String> {
    let fresh = OwnOverlay::from_battle(b, side);
    if fresh != OwnOverlay::from_request(&b.request_data(side)) {
        return Err("from_battle differs from from_request".into());
    }
    *own = fresh;
    let view = PlayerView::new(side, tracker, own);
    for (pos, m) in own.mons.iter().enumerate() {
        if view.own_position(m.id) != Some(pos) {
            return Err("own position mapping".into());
        }
        if let Some(row) = view.own_row(m.id)
            && view.own_id_of_row(row) != Some(m.id)
        {
            return Err(format!("row {row} maps to {:?}, not {}", view.own_id_of_row(row), m.id));
        }
    }
    // Refused choices: an action the request allows that the battle
    // doesn't is Showdown's `[Unavailable choice]`.
    let legal = b.legal_actions(side);
    for (slot, a) in own.active.iter().enumerate() {
        let Some(a) = a else { continue };
        if legal.is_waiting() {
            break;
        }
        let bench = legal.slot(slot).iter().any(|x| matches!(x, SlotAction::Switch(_)));
        if a.maybe_trapped && !bench && b.state().side(side).bench().count() > 0 {
            let mut o = *own;
            if !o.reject(b, slot, Unavailable::Switch) || !o.active[slot].unwrap().trapped || !o.known_trapped[slot] {
                return Err("a hidden trap's refusal didn't update the request".into());
            }
            seen.rejected += 1;
        }
        if let OwnMoves::Moves(ms) = a.moves {
            for (i, m) in ms.iter().enumerate() {
                let listed = legal
                    .slot(slot)
                    .iter()
                    .any(|x| matches!(x, SlotAction::Move { index, .. } if *index as usize == i));
                if !m.disabled && !listed && b.state().side(side).active(slot).is_some_and(|p| p.has_usable_move()) {
                    let mut o = *own;
                    if !o.reject(b, slot, Unavailable::Move { index: i as u8 }) {
                        return Err(format!("move {i}'s refusal didn't update the request"));
                    }
                    let OwnMoves::Moves(after) = o.active[slot].unwrap().moves else { unreachable!() };
                    if !after[i].disabled || o.known_disabled[slot] & (1 << i) == 0 {
                        return Err("the refused move isn't disabled".into());
                    }
                    seen.rejected += 1;
                }
            }
        }
    }
    Ok(())
}

/// The player's own team ([`OwnTeam`]), updated with the request: its
/// sets are the battle's (nature, stats outside a permanent forme change,
/// maximum PP), and a benched Pokémon's last PP seen is its true PP, or
/// more by what it used after the last request that showed it (the move
/// it left with).
fn check_team(
    b: &Battle,
    tracker: &PublicTracker,
    side: SideId,
    own: &OwnOverlay,
    team: &mut OwnTeam,
) -> Result<(), String> {
    let _ = tracker;
    team.observe(own);
    let s = b.state().side(side);
    for (pos, mon) in s.team.iter().enumerate() {
        let Some(t) = team.get(mon.id) else { return Err(format!("no team entry for {}", mon.id)) };
        if t.nature != Some(mon.nature) || t.level != mon.level || t.gender != Some(mon.gender) {
            return Err(format!("team entry {}: {:?}", mon.id, (t.nature, t.level, t.gender)));
        }
        let st = mon.set_stats;
        let set = [st[pikapro_data::Stat::Hp], st[pikapro_data::Stat::Atk], st[pikapro_data::Stat::Def]];
        // A permanent forme change (Mega Evolution) changes its stats.
        if t.species == pikapro_protocol::details_species(mon.base_species) && t.stats[..3] != set {
            return Err(format!("team entry {} stats {:?} vs {set:?}", mon.id, t.stats));
        }
        let active = pos < b.format().active_per_side();
        let own_moves = match &mon.volatile_counters.transform {
            Some(base) => &base.moves,
            None => &mon.moves,
        };
        for (k, m) in own_moves.iter().enumerate() {
            if t.moves.get(k) != Some(&m.id) {
                continue;
            }
            if t.max_pp[k] != m.max_pp {
                return Err(format!("team entry {} move {k} max PP {} vs {}", mon.id, t.max_pp[k], m.max_pp));
            }
            if active || mon.is_fainted() {
                continue;
            }
            let seen = t.last_pp(k);
            if seen.is_none_or(|p| p < m.pp || p > m.pp + 3) {
                return Err(format!("benched {} move {k} PP {seen:?} vs {}", mon.id, m.pp));
            }
        }
    }
    Ok(())
}

/// Runs `n` battles per format and team source from `base`, panicking on
/// the first failure.
///
/// With `PIKAPRO_VIEW_COLLECT` set, it keeps going and reports one example of
/// each kind of failure at the end instead.
pub(super) fn fuzz(n: u64, base: u64, sources: &[Teams], opts: Opts) -> Seen {
    let collect = std::env::var_os("PIKAPRO_VIEW_COLLECT").is_some();
    let mut failures: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut seen = Seen::default();
    for (f, format) in formats().iter().enumerate() {
        for (k, &source) in sources.iter().enumerate() {
            for i in 0..n {
                let seed = base + (f as u64 * 3 + k as u64) * 10_000 + i;
                let teams = teams(format, source, seed);
                if let Err(e) = run(format, teams, seed, opts, &mut seen) {
                    if !collect {
                        panic!("{source:?} teams: {e}");
                    }
                    let first = e.lines().next().unwrap_or("");
                    let kind = match (first.rfind('<'), first.rfind('>')) {
                        (Some(a), Some(b)) if a < b => first[a + 1..b].to_string(),
                        _ => first.split(": ").nth(1).unwrap_or("").chars().take(40).collect(),
                    };
                    let entry = failures.entry(kind).or_insert((0, format!("{source:?} teams: {e}")));
                    entry.0 += 1;
                }
            }
        }
    }
    if !failures.is_empty() {
        for (kind, (count, example)) in &failures {
            eprintln!("=== {count}x {kind}\n{example}\n");
        }
        panic!("{} kinds of failure", failures.len());
    }
    seen
}

#[test]
fn random_battles_are_sound_and_leak_free() {
    let seen = fuzz(12, 4_200_000, &[Teams::Sample, Teams::Uniform, Teams::Sets], Opts::default());
    eprintln!("{} battles, {} decision points, {} events", seen.battles, seen.points, seen.events);
}

#[test]
fn exact_hp_battles_are_sound() {
    fuzz(4, 4_210_000, &[Teams::Uniform], Opts { hp: HpReport::Exact, ..Opts::default() });
}

#[test]
fn open_team_sheets_are_sound() {
    fuzz(4, 4_220_000, &[Teams::Sets], Opts { ots: true, ..Opts::default() });
}

/// The long run: `PIKAPRO_VIEW_FUZZ` battles (default 1000) per format and
/// team source, in release: `cargo test --release -p pikapro-view -- --ignored fuzz_long --nocapture`.
#[test]
#[ignore = "long: cargo test --release -p pikapro-view fuzz_long -- --ignored --nocapture"]
fn fuzz_long() {
    let n = std::env::var("PIKAPRO_VIEW_FUZZ").ok().and_then(|v| v.parse().ok()).unwrap_or(1000);
    let seen = fuzz(n, 4_230_000, &[Teams::Uniform, Teams::Sets, Teams::Sample], Opts::default());
    eprintln!(
        "{} battles, {} decision points, {} events, {} refused choices",
        seen.battles, seen.points, seen.events, seen.rejected
    );
    for (k, v) in &seen.variants {
        eprintln!("{k:>24} {v}");
    }
}
