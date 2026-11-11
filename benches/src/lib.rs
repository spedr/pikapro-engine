//! Workloads shared by the Criterion benches (`cargo bench -p pikapro-bench`)
//! and the instruction-count harness (`cargo bench-insns`).
//!
//! Every workload is deterministic: battles are random self-play from fixed
//! seeds, with the policy drawing from its own RNG (as `pikapro bench` does),
//! so the same build always executes the same instructions.

use pikapro::event::BattleEvent;
use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Format, LegalActions, Seed, SideId, Team};
use pikapro_protocol::{Perspective, Renderer};

/// Five selected presets the benchmarks run, most important first.
pub const FORMATS: [&str; 5] = ["championsvgc", "gen9vgc", "gen9ou", "gen9doubles", "gen9singles"];

/// The preset format `id`.
///
/// # Panics
/// If `id` is not a preset.
pub fn format(id: &str) -> Format {
    Format::by_id(id).unwrap_or_else(|| panic!("unknown format {id:?}"))
}

/// Both sides' teams: the built-in sample team, as `pikapro bench` uses.
pub fn teams(format: &Format) -> [Team; 2] {
    [sample_team(format), sample_team(format)]
}

/// A new battle from `seed`.
pub fn new_battle(format: &Format, teams: &[Team; 2], seed: u64, events: bool) -> Battle {
    let mut options = BattleOptions::new(Seed::fast(seed));
    options.log_events = events;
    Battle::new(format.clone(), teams.clone(), options).expect("sample teams are valid")
}

/// One decision per side waiting on a request, drawn by `policy`.
pub fn step(battle: &mut Battle, policy: &mut BattleRng) {
    for side in SideId::BOTH {
        let legal = battle.legal_actions(side);
        if legal.is_waiting() {
            continue;
        }
        let choice = legal.sample(policy);
        battle.submit(side, choice).expect("sampled choices are legal");
    }
}

/// Plays one random self-play battle to the end, as `pikapro bench` does
/// (with `events`, the log is drained after every step). Returns the
/// number of turns.
pub fn play(format: &Format, teams: &[Team; 2], seed: u64, events: bool) -> u16 {
    let mut battle = new_battle(format, teams, seed, events);
    let mut policy = BattleRng::new(Seed::fast(!seed));
    while !battle.is_over() {
        step(&mut battle, &mut policy);
        if events {
            battle.drain_events().for_each(drop);
        }
    }
    battle.turn()
}

/// Battles paused at their requests (every decision point of the battles
/// from `seed` on), until there are `n`.
pub fn snapshots(format: &Format, teams: &[Team; 2], seed: u64, n: usize) -> Vec<Battle> {
    let mut out = Vec::with_capacity(n);
    let mut seed = seed;
    while out.len() < n {
        let mut battle = new_battle(format, teams, seed, false);
        let mut policy = BattleRng::new(Seed::fast(!seed));
        while !battle.is_over() && out.len() < n {
            out.push(battle.clone());
            step(&mut battle, &mut policy);
        }
        seed += 1;
    }
    out
}

/// The full event log of the battle from `seed`.
pub fn battle_log(format: &Format, teams: &[Team; 2], seed: u64) -> Vec<BattleEvent> {
    let mut battle = new_battle(format, teams, seed, true);
    let mut policy = BattleRng::new(Seed::fast(!seed));
    while !battle.is_over() {
        step(&mut battle, &mut policy);
    }
    battle.events().to_vec()
}

/// Forks `battle` `n` times with new seeds (determinised search).
pub fn fork(battle: &Battle, n: u64) -> u64 {
    let mut turns = 0;
    for i in 0..n {
        turns += std::hint::black_box(battle.fork(Seed::fast(i))).turn() as u64;
    }
    turns
}

/// Regenerates both sides' legal actions.
pub fn legal_actions(battle: &Battle) -> [LegalActions; 2] {
    SideId::BOTH.map(|side| pikapro::legal::legal_actions(battle, side))
}

/// Both sides' `|request|` JSON: [`Battle::request_data`] then
/// [`Renderer::write_request`]. Returns the bytes written.
pub fn requests(battle: &Battle, renderer: &Renderer, out: &mut String) -> usize {
    let mut n = 0;
    for side in SideId::BOTH {
        out.clear();
        renderer.write_request(&battle.request_data(side), out);
        n += out.len();
    }
    n
}

/// The perspectives a log is rendered for.
pub const PERSPECTIVES: [(&str, Perspective); 3] = [
    ("omniscient", Perspective::Omniscient),
    ("p1", Perspective::Player(SideId::P1)),
    ("spectator", Perspective::Spectator),
];

/// Renders `events` for `perspective`. Returns the bytes written.
pub fn render(format: &Format, perspective: Perspective, events: &[BattleEvent]) -> usize {
    Renderer::new(format, perspective).lines(events).len()
}

/// The sample team in Showdown's export format.
pub fn team_text(format: &Format) -> String {
    pikapro_protocol::export_sheet(&pikapro_protocol::TeamSheet::new(sample_team(format)), format)
}

/// Parses a team in Showdown's export format.
pub fn import_team(format: &Format, text: &str) -> Team {
    pikapro_protocol::import_team(text, format).expect("exported teams import")
}

/// Builds a battle (team validation, stats, the first requests).
pub fn construct(format: &Format, teams: &[Team; 2], seed: u64) -> Battle {
    new_battle(format, teams, seed, false)
}

// RL environment: player views and the fused step.

/// A decision point with events on: the battle (its log drained), the
/// public tracker fed every event so far, and each player's own team as
/// the environment keeps it (every request so far observed).
#[derive(Clone, Debug)]
pub struct ViewPoint {
    pub battle: Battle,
    pub tracker: pikapro_view::PublicTracker,
    pub teams: [pikapro_view::OwnTeam; 2],
}

/// Decision points of the battles from `seed` on, with their trackers,
/// until there are `n`.
pub fn view_points(format: &Format, teams: &[Team; 2], seed: u64, n: usize) -> Vec<ViewPoint> {
    let mut out = Vec::with_capacity(n);
    let mut seed = seed;
    while out.len() < n {
        let mut battle = new_battle(format, teams, seed, true);
        let mut tracker = pikapro_view::PublicTracker::for_format(format);
        let mut own = SideId::BOTH.map(|s| pikapro_view::OwnTeam::from_team(&teams[s.index()], format));
        let mut policy = BattleRng::new(Seed::fast(!seed));
        tracker.apply_all(battle.drain_events().as_slice());
        while !battle.is_over() && out.len() < n {
            for s in SideId::BOTH {
                own[s.index()].observe(&pikapro_view::OwnOverlay::from_request(&battle.request_data(s)));
            }
            out.push(ViewPoint { battle: battle.clone(), tracker, teams: own });
            step(&mut battle, &mut policy);
            for e in battle.drain_events() {
                tracker.apply(&e);
            }
        }
        seed += 1;
    }
    out
}

/// Encodes both sides of every point with the `basic` encoder, as the
/// vector environment's workers do. Returns a checksum.
pub fn encode_points(
    points: &[ViewPoint],
    overlays: &[[pikapro_view::OwnOverlay; 2]],
    tables: &pikapro_rl::tables::Tables,
) -> u64 {
    use pikapro_rl::encode::{Basic, Encoder};
    let mut out = [vec![0f32; Basic::SIZE], vec![0f32; Basic::SIZE]];
    let mut sum = 0u64;
    for (p, own) in points.iter().zip(overlays) {
        for side in SideId::BOTH {
            let s = side.index();
            let view = pikapro_view::PlayerView::new(side, &p.tracker, &own[s]).with_team(&p.teams[s]);
            Basic.encode(&view, tables, &mut out[s]);
            sum = sum.wrapping_add(out[s][3].to_bits() as u64);
        }
    }
    sum
}

/// The events each decision point's next step produces (random choices
/// drawn from `seed + i`), with the tracker before them.
pub fn view_batches(points: &[ViewPoint]) -> Vec<(pikapro_view::PublicTracker, Vec<BattleEvent>)> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut battle = p.battle.clone();
            step(&mut battle, &mut BattleRng::new(Seed::fast(i as u64)));
            (p.tracker, battle.drain_events().collect())
        })
        .collect()
}

/// Feeds a whole battle log to a new tracker. Returns the turn it saw.
pub fn apply_view(format: &Format, events: &[BattleEvent]) -> u16 {
    let mut tracker = pikapro_view::PublicTracker::for_format(format);
    tracker.apply_all(events);
    std::hint::black_box(&tracker).turn
}

/// Both sides' overlays from their requests.
pub fn overlays(battle: &Battle) -> [pikapro_view::OwnOverlay; 2] {
    SideId::BOTH.map(|s| pikapro_view::OwnOverlay::from_request(&battle.request_data(s)))
}

/// Both sides' factored masks (`out`: `2 * width` values).
pub fn factored_masks(battle: &Battle, space: &pikapro_rl::space::ActionSpace, out: &mut [bool]) {
    let width = pikapro_rl::vec::MaskLayout::Factored.width(space);
    for side in SideId::BOTH {
        let legal = battle.legal_actions(side);
        let row = &mut out[side.index() * width..][..width];
        let (slots, preview) = row.split_at_mut(space.active * space.slot_size);
        space.fill_slot_mask(legal, slots);
        space.fill_preview_mask(legal, battle.state().side(side).team.len(), preview);
    }
}

/// A pool of `n` battles of the sample teams from `seed`.
pub fn pool_config(format: &Format, n: usize, seed: u64, log: bool) -> pikapro_rl::pool::PoolConfig {
    use std::sync::Arc;
    let sheets =
        [pikapro_protocol::TeamSheet::new(sample_team(format)), pikapro_protocol::TeamSheet::new(sample_team(format))];
    let teams = pikapro_rl::vec::Teams::Pairs(vec![Arc::new(sheets)]);
    let mut config = pikapro_rl::pool::PoolConfig::new(format.clone(), n, teams);
    config.seed = seed;
    config.log = log;
    config.spin = std::time::Duration::from_micros(200);
    config
}

/// The vector environment on `n` battles of the sample teams from `seed`,
/// with `shards_per_thread`: player 0 plays
/// [`RANDOM_ACTION`](pikapro_rl::vec::RANDOM_ACTION)s, player 1 the built-in
/// random policy (random self-play, one request round per step).
pub fn env_config(format: &Format, n: usize, seed: u64, shards_per_thread: usize) -> pikapro_rl::env::EnvConfig {
    use std::sync::Arc;
    let sheets =
        [pikapro_protocol::TeamSheet::new(sample_team(format)), pikapro_protocol::TeamSheet::new(sample_team(format))];
    let teams = pikapro_rl::vec::Teams::Pairs(vec![Arc::new(sheets)]);
    let mut config = pikapro_rl::env::EnvConfig::new(format.clone(), n, teams);
    config.seed = seed;
    config.spin = std::time::Duration::from_micros(200);
    config.shards_per_thread = shards_per_thread;
    config
}

/// [`RANDOM_ACTION`](pikapro_rl::vec::RANDOM_ACTION) for every battle and
/// player.
pub fn random_actions(env: &pikapro_rl::env::VecEnv) -> Vec<i64> {
    vec![pikapro_rl::vec::RANDOM_ACTION; env.shape().n * 2 * env.actions_per_player()]
}
