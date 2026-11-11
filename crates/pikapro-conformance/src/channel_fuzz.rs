//! Structured channel fuzzing: plausible but unusual channels through
//! whole [`ChannelSession`]s.
//!
//! Each battle is played on the engine with random choices and rendered as
//! each player's channel, chunk by chunk: a chunk is one update's lines and
//! the request that ends it, with the battle kept at that decision point.
//! Then:
//!
//! * **legal transforms**, which must leave the view sound at every
//!   decision point ([`check_sound`], team preview formats: without it the
//!   foes' ids are by appearance) and, but where noted, equal to the plain
//!   session's (tracker, overlay, `needs_action`): room and chat noise and
//!   `>room` headers, each request sent twice, the request before its
//!   update's lines (Showdown's order; soundness only), CRLF line ends,
//!   `|showteam|` again mid-battle, or only mid-battle (soundness only);
//! * **illegal transforms**, which must not panic, and must leave the
//!   tracker consistent ([`check_consistent`]): stray `|replace|` and
//!   `|detailschange|` lines, requests out of order, another battle's
//!   lines and requests interleaved, an early `|win|` or `|tie`, a
//!   truncated log;
//! * **series** (team preview formats): game 1 then game 2 on the same
//!   channel with its `|win|` turned into `|tie`, a rejoin in the middle of
//!   game 2 compared with the continuous session (tracker, overlay, series
//!   row, observation), and the recorded series.
//!
//! Panics are caught with `catch_unwind`, so run it in a profile that
//! unwinds (`--profile release-tools`, or a test); under `panic = "abort"` the
//! first panic ends the run. `cargo test` runs a small round
//! (`tests/channel_fuzz.rs`); the `channel_fuzz` binary any number of
//! battles.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use pikapro::{Battle, BattleRng, Format, Seed, SideId};
use pikapro_data::gen9::abilities;
use pikapro_protocol::Perspective;
use pikapro_rl::series::{Series, SeriesResult};
use pikapro_rl::session::{ChannelSession, SessionOptions};
use pikapro_view::check::{check_consistent, check_sound};
use pikapro_view::{OwnOverlay, PublicTracker};

use crate::view::{Driver, Generator, Setup, Source, Target};

/// One update of a player's channel.
#[derive(Clone)]
struct Chunk {
    /// The update's lines (`|split|` blocks as rendered).
    lines: Vec<String>,
    /// The request sent at the end of the update (`None` once over).
    request: Option<String>,
    /// The battle at this decision point.
    battle: Battle,
}

/// A battle as one player's channel.
struct Game {
    format: Format,
    header: Vec<String>,
    /// Both sides' `|showteam|` lines (team preview formats).
    showteam: Vec<String>,
    chunks: Vec<Chunk>,
    team: pikapro::Team,
    /// Per side, the names two of its Pokémon share
    /// ([`shared_names`](crate::channel::shared_names)).
    shared: [Vec<String>; 2],
}

/// What a round found.
#[derive(Clone, Debug, Default)]
pub struct FuzzStats {
    /// Channels fed (a battle, one player).
    pub games: u64,
    /// Decision points checked.
    pub points: u64,
    /// A legal transform's view unsound at a decision point.
    pub unsound: Vec<String>,
    /// A legal transform's view unlike the plain session's.
    pub differs: Vec<String>,
    /// Panics.
    pub panics: Vec<String>,
    /// An illegal transform left an inconsistent tracker.
    pub inconsistent: Vec<String>,
    /// Series results unlike the games'.
    pub series: Vec<String>,
}

impl FuzzStats {
    /// Nothing found.
    pub fn clean(&self) -> bool {
        self.unsound.is_empty()
            && self.differs.is_empty()
            && self.panics.is_empty()
            && self.inconsistent.is_empty()
            && self.series.is_empty()
    }

    fn extend(&mut self, other: FuzzStats) {
        self.games += other.games;
        self.points += other.points;
        self.unsound.extend(other.unsound);
        self.differs.extend(other.differs);
        self.panics.extend(other.panics);
        self.inconsistent.extend(other.inconsistent);
        self.series.extend(other.series);
    }

    /// Each kind's count and its first `show` entries, sorted.
    pub fn report(&self, show: usize) -> String {
        let mut out = format!("games {} points {}\n", self.games, self.points);
        for (name, v) in [
            ("UNSOUND", &self.unsound),
            ("DIFFERS", &self.differs),
            ("PANIC", &self.panics),
            ("INCONSISTENT", &self.inconsistent),
            ("SERIES", &self.series),
        ] {
            out.push_str(&format!("{name}: {}\n", v.len()));
            let mut v = v.clone();
            v.sort();
            v.dedup();
            for x in v.iter().take(show) {
                out.push_str(&format!("  {x}\n"));
            }
        }
        out
    }
}

/// A round: `battles` battles per format and source, from `seed`.
#[derive(Clone, Debug)]
pub struct FuzzOptions {
    pub formats: Vec<Format>,
    pub sources: Vec<Source>,
    pub battles: u64,
    pub seed: u64,
    /// Where an illegal transform's channel that panicked is written.
    pub dump: Option<PathBuf>,
}

impl FuzzOptions {
    /// Seven presets (all but championsbss) and these sources: Illusion, Imposter,
    /// Commander, Revival Blessing, Ogerpon, Mega, the Illusion-Imposter
    /// and Commander-Revival combinations, and legal sets.
    pub fn new(battles: u64, seed: u64) -> FuzzOptions {
        FuzzOptions {
            formats: vec![
                Format::champions_vgc(),
                Format::gen9_vgc(),
                Format::gen9_doubles(),
                Format::gen9_singles(),
                Format::gen9_ou(),
                Format::gen9_random_battle(),
                Format::gen9_random_doubles_battle(),
            ],
            sources: vec![
                Source::Target(Target::Illusion),
                Source::Target(Target::Imposter),
                Source::Target(Target::Commander),
                Source::Target(Target::RevivalBlessing),
                Source::Target(Target::Ogerpon),
                Source::Target(Target::Mega),
                Source::Target(Target::IllusionImposter),
                Source::Target(Target::CommanderRevival),
                Source::Sets,
            ],
            battles,
            seed,
            dump: None,
        }
    }
}

/// Runs a round; `progress` hears each format's end.
pub fn run(opts: &FuzzOptions, progress: &mut dyn FnMut(&Format, &FuzzStats)) -> FuzzStats {
    let mut stats = FuzzStats::default();
    for format in &opts.formats {
        let ots = format.team_preview.shown();
        for (si, source) in opts.sources.iter().enumerate() {
            let Ok(generator) = Generator::new(format, *source) else { continue };
            for i in 0..opts.battles {
                let seed = opts.seed + i * 31 + si as u64 * 7919;
                let setup = generator.setup(seed);
                let other = generator.setup(seed + 1);
                for side in SideId::BOTH {
                    let (Some(g), Some(o)) = (play(&setup, side, ots), play(&other, side, ots)) else { continue };
                    stats.games += 1;
                    stats.extend(one_game(&g, &o, side, seed, ots, opts.dump.as_ref()));
                }
            }
        }
        progress(format, &stats);
    }
    stats
}

/// Every transform of one player's channel.
fn one_game(g: &Game, o: &Game, side: SideId, seed: u64, ots: bool, dump: Option<&PathBuf>) -> FuzzStats {
    let mut stats = FuzzStats::default();
    let plain = catch_unwind(AssertUnwindSafe(|| {
        let mut st = FuzzStats::default();
        let (_, base) = feed_game(g, side, Legal::default(), seed, None, None, &mut st, "plain");
        (st, base)
    }));
    let base = match plain {
        Ok((st, base)) => {
            stats.extend(st);
            base
        }
        Err(_) => {
            stats.panics.push(format!("plain {} seed {seed} {side:?}", g.format.id));
            return stats;
        }
    };
    let legal = [
        Legal { noise: true, ..Legal::default() },
        Legal { dup_request: true, ..Legal::default() },
        Legal { request_first: true, ..Legal::default() },
        Legal { crlf: true, ..Legal::default() },
        Legal { showteam_twice: ots, ..Legal::default() },
        Legal { showteam_late: ots, ..Legal::default() },
    ];
    for l in legal {
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut st = FuzzStats::default();
            // Showdown's order reads the lines after the request: only
            // soundness compares.
            let base = (!l.request_first).then_some(&base[..]);
            feed_game(g, side, l, seed, None, base, &mut st, "legal");
            st
        }));
        match r {
            Ok(st) => stats.extend(st),
            Err(_) => stats.panics.push(format!("legal {l:?} {} seed {seed} {side:?}", g.format.id)),
        }
    }
    illegal(g, side, seed, o, &mut stats, dump);
    if ots {
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut st = FuzzStats::default();
            series(g, side, seed, &mut st);
            st
        }));
        match r {
            Ok(st) => stats.extend(st),
            Err(_) => stats.panics.push(format!("series {} seed {seed} {side:?}", g.format.id)),
        }
    }
    stats
}

/// Plays `setup` with random choices and renders `side`'s channel.
fn play(setup: &Setup, side: SideId, ots: bool) -> Option<Game> {
    let mut b = setup.try_battle().ok()?;
    let r = setup.renderer(Perspective::Player(side));
    let names = setup.names();
    let shared = crate::channel::shared_names(&b, &names);
    let mut d = Driver::random(setup.seed);
    let mut log = Vec::new();
    let mut chunks = Vec::new();
    loop {
        let ev: Vec<_> = b.drain_events().collect();
        let lines = r.log(ev.iter());
        let request = (!b.is_over()).then(|| {
            let mut data = b.request_data(side);
            data.answered = false;
            format!("|request|{}", r.request_json(&data))
        });
        chunks.push(Chunk { lines, request, battle: b.clone() });
        if b.is_over() || b.turn() > 300 || !matches!(d.advance(&mut b, &names, &mut log), Ok(true)) {
            break;
        }
    }
    let showteam = if ots {
        SideId::BOTH
            .iter()
            .map(|&s| {
                format!(
                    "|showteam|{}|{}",
                    pikapro_protocol::side_id(s),
                    pikapro_protocol::pack_team(&setup.sheets[s.index()], &setup.format)
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    Some(Game {
        format: setup.format.clone(),
        header: r.header(),
        showteam,
        chunks,
        team: setup.sheets[side.index()].team.clone(),
        shared,
    })
}

/// How a game is fed.
#[derive(Clone, Copy, Debug, Default)]
struct Legal {
    noise: bool,
    dup_request: bool,
    request_first: bool,
    crlf: bool,
    /// Sheets at team preview and again mid-battle.
    showteam_twice: bool,
    /// Sheets only mid-battle.
    showteam_late: bool,
}

/// Room, chat and HTML lines a battle room sends.
const NOISE: [&str; 12] = [
    ">battle-gen9vgc2025regi-123456",
    "|c|☆p2|gl hf",
    "|c|~|/uhtml controls,",
    "|j|☆guest123",
    "|l|guest123",
    "|raw|<div class=\"broadcast-blue\">x</div>",
    "|inactive|p2 has 120 seconds left.",
    "|inactiveoff|Battle timer is now OFF.",
    "|uhtml|bestof|<h2><strong>Game 2</strong> of <a href=\"/game-bestof3-x\">a best-of-3</a></h2>",
    "|html|<table width=\"100%\"><tr><td align=\"left\">p1</td></tr></table>",
    "|n|☆p1b|p1",
    "|tempnotify|choice|Next game|It's time for game 2 in your best-of-3!",
];

/// The lines a session is fed for chunk `k`.
fn chunk_lines(g: &Game, k: usize, legal: Legal, rng: &mut BattleRng) -> Vec<String> {
    let c = &g.chunks[k];
    let mut out: Vec<String> = Vec::new();
    if k == 0 {
        out.extend(g.header.iter().cloned());
    }
    let mut body = c.lines.clone();
    if k == 0 && !legal.showteam_late {
        // Sheets at team preview (after `|teampreview|`).
        let at = body.iter().position(|l| l.starts_with("|teampreview")).map_or(body.len(), |i| i + 1);
        for (j, l) in g.showteam.iter().enumerate() {
            body.insert(at + j, l.clone());
        }
    }
    let mid = g.chunks.len() / 2;
    if k == mid.max(1) && (legal.showteam_twice || legal.showteam_late) {
        body.extend(g.showteam.iter().cloned());
    }
    if legal.noise {
        for _ in 0..3 {
            let at = rng.random(body.len() as u32 + 1) as usize;
            // Not inside a `|split|` block (the two lines after it).
            let inside = (1..=2).any(|d| at >= d && body.get(at - d).is_some_and(|l| l.starts_with("|split|")));
            if !inside {
                body.insert(at, NOISE[rng.random(NOISE.len() as u32) as usize].to_string());
            }
        }
    }
    let req: Vec<String> = match &c.request {
        Some(r) if legal.dup_request => vec![r.clone(), r.clone()],
        Some(r) => vec![r.clone()],
        None => vec![],
    };
    if legal.request_first && k > 0 {
        out.extend(req);
        out.extend(body);
    } else {
        out.extend(body);
        out.extend(req);
    }
    if legal.crlf {
        for l in &mut out {
            l.push('\r');
        }
    }
    out
}

fn session(g: &Game, side: SideId, series: Option<&Series>, team: bool) -> ChannelSession {
    let options = SessionOptions::default()
        .with_username((side == SideId::P2).then(|| "p2".to_string()))
        .with_team(team.then(|| g.team.clone()));
    let exact = pikapro_protocol::BattleInfo::for_format(&g.format).report_exact_hp;
    let mut s = ChannelSession::for_format(g.format.clone(), exact, options).expect("a preset's session");
    if let Some(series) = series {
        s.set_series(series.clone()).expect("a series with a game to play");
    }
    s
}

type Snapshot = (PublicTracker, OwnOverlay, bool);

/// Feeds a game with `legal`; checks soundness at each decision point and,
/// with `base`, equality with the plain session's views there.
#[allow(clippy::too_many_arguments)]
fn feed_game(
    g: &Game,
    side: SideId,
    legal: Legal,
    seed: u64,
    series: Option<&Series>,
    base: Option<&[Snapshot]>,
    stats: &mut FuzzStats,
    tag: &str,
) -> (ChannelSession, Vec<Snapshot>) {
    let mut rng = BattleRng::new(Seed::fast(seed ^ 0xF00D));
    let mut s = session(g, side, series, true);
    let mut views = Vec::new();
    // A line named a Pokémon off the field by a name two share, and the
    // view then differed from the battle: no client could tell them apart
    // (`channel::named_alike`), so soundness isn't checked from there (two
    // fainted Alcremie, one revived). The channel is still fed to its end.
    let mut excused = false;
    for k in 0..g.chunks.len() {
        for l in chunk_lines(g, k, legal, &mut rng) {
            s.feed(&l);
        }
        let c = &g.chunks[k];
        stats.points += 1;
        // Without team preview the channel's foe ids are by appearance, not
        // the engine's: `check_sound` compares ids, so only with preview.
        let sound =
            if g.format.team_preview.shown() { sound(s.tracker(), &c.battle) } else { check_consistent(s.tracker()) };
        if let Err(v) = sound
            && !excused
        {
            if crate::channel::named_alike(g.chunks[..=k].iter().flat_map(|c| &c.lines), &g.shared) {
                excused = true;
            } else {
                stats
                    .unsound
                    .push(format!("{tag} {} seed {seed} {side:?} chunk {k}: {v} <{}> {legal:?}", g.format.id, v.what));
                break;
            }
        }
        let snap = (*s.tracker(), *s.overlay(), s.needs_action());
        if let Some(base) = base
            && let Some(b) = base.get(k)
            && !legal.showteam_late
            && *b != snap
        {
            let what = if b.0 != snap.0 {
                "tracker"
            } else if b.1 != snap.1 {
                "overlay"
            } else {
                "needs_action"
            };
            stats
                .differs
                .push(format!("{tag} {} seed {seed} {side:?} chunk {k}: {what} differs {legal:?}", g.format.id));
            break;
        }
        views.push(snap);
        if s.needs_action() {
            // Answer like a client would, to keep the answered flag honest.
            let _ = s.choose_builtin("random", seed + k as u64);
        }
    }
    (s, views)
}

/// [`check_sound`], but for the types Reflect Type or Transform copied
/// from a foe whose Illusion is up: the disguise's, as far as anyone can
/// see (`view::sound::check` exempts them too).
fn sound(tracker: &PublicTracker, battle: &Battle) -> Result<(), pikapro_view::check::Violation> {
    match check_sound(tracker, battle) {
        Err(v)
            if v.what == "types"
                && battle.state().side(v.side.foe()).team.iter().any(|m| m.base_ability == abilities::ILLUSION) =>
        {
            Ok(())
        }
        r => r,
    }
}

/// Illegal transforms of the whole channel, twelve rounds.
fn illegal(g: &Game, side: SideId, seed: u64, other: &Game, stats: &mut FuzzStats, dump: Option<&PathBuf>) {
    let mut rng = BattleRng::new(Seed::fast(seed ^ 0xBAD));
    let mut lines: Vec<String> = g.header.clone();
    for c in &g.chunks {
        lines.extend(c.lines.iter().cloned());
        if let Some(r) = &c.request {
            lines.push(r.clone());
        }
    }
    let field = |n: usize| -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| l.strip_prefix("|switch|").and_then(|r| r.split('|').nth(n)).map(str::to_string))
            .collect()
    };
    let (names, details) = (field(0), field(1));
    for round in 0..12u32 {
        let mut m = lines.clone();
        let mut what: Vec<String> = Vec::new();
        for _ in 0..=(round % 4) {
            let at = rng.random(m.len() as u32) as usize;
            let pick = |rng: &mut BattleRng, v: &[String]| {
                if v.is_empty() { "p1a: X".to_string() } else { v[rng.random(v.len() as u32) as usize].clone() }
            };
            match rng.random(8) {
                0 => {
                    let n = pick(&mut rng, &names);
                    let d = pick(&mut rng, &details);
                    let slot = if rng.random(2) == 0 { n.replacen("a:", "b:", 1) } else { n };
                    what.push(format!("{at}: |replace|{slot}|{d}"));
                    m.insert(at, format!("|replace|{slot}|{d}"));
                }
                1 => {
                    let n = pick(&mut rng, &names);
                    let d = pick(&mut rng, &details);
                    what.push(format!("{at}: |detailschange|{n}|{d}"));
                    m.insert(at, format!("|detailschange|{n}|{d}"));
                }
                2 => {
                    // A later request before an earlier one.
                    let reqs: Vec<usize> = (0..m.len()).filter(|&i| m[i].starts_with("|request|")).collect();
                    if reqs.len() > 2 {
                        let r = rng.random(reqs.len() as u32 - 1) as usize;
                        what.push(format!("swap requests {} {}", reqs[r], reqs[r + 1]));
                        m.swap(reqs[r], reqs[r + 1]);
                    }
                }
                3 => {
                    // Another battle's lines (another room's) interleaved.
                    let o: Vec<String> = other.chunks.iter().flat_map(|c| c.lines.iter().cloned()).collect();
                    if !o.is_empty() {
                        let from = rng.random(o.len() as u32) as usize;
                        what.push(format!("{at}: other battle's lines from {from}"));
                        for (j, l) in o[from..(from + 20).min(o.len())].iter().enumerate() {
                            m.insert((at + j * 2).min(m.len()), l.clone());
                        }
                    }
                }
                4 => {
                    let end = if rng.random(2) == 0 { "|tie" } else { "|win|p2" };
                    what.push(format!("{at}: {end}"));
                    m.insert(at, end.into());
                }
                5 => {
                    what.push(format!("truncate {at}"));
                    m.truncate(at.max(1));
                }
                6 => {
                    // Another battle's request.
                    if let Some(r) = other.chunks.iter().filter_map(|c| c.request.clone()).nth(rng.random(8) as usize) {
                        what.push(format!("{at}: another battle's request"));
                        m.insert(at, r);
                    }
                }
                _ => {
                    what.push(format!("{at}: |replace| of nobody"));
                    m.insert(at, "|replace|p1a: Nobody|Zoroark, L50, M".into());
                }
            }
        }
        let res = catch_unwind(AssertUnwindSafe(|| {
            let mut s = session(g, side, (round % 2 == 0).then(|| Series::new(3)).as_ref(), round % 3 == 0);
            for (i, l) in m.iter().enumerate() {
                s.feed(l);
                if i % 17 == 0 || l.starts_with("|request|") {
                    let _ = s.obs();
                    let _ = s.mask().len();
                    let _ = s.series_row();
                    if s.needs_action() {
                        let _ = s.choose_builtin(["random", "max_base_power", "simple_heuristics"][i % 3], i as u64);
                    }
                }
            }
            let _ = s.recorded_series().and_then(Series::next_game);
            check_consistent(s.tracker())
        }));
        let tag = format!("{} seed {seed} {side:?} round {round}: {what:?}", g.format.id);
        match res {
            Ok(Ok(())) => continue,
            Ok(Err(v)) => stats.inconsistent.push(format!("{tag}: {v}")),
            Err(_) => stats.panics.push(tag),
        }
        if let Some(dir) = dump {
            let _ = std::fs::create_dir_all(dir);
            let path = dir.join(format!("channel-fuzz-{}-{seed}-{side:?}-{round}.txt", g.format.id));
            let _ = std::fs::write(path, m.join("\n"));
        }
    }
}

/// Two games of a series (the same battle twice: game 2 ends `|tie`), a
/// rejoin in game 2, and the rows.
fn series(g: &Game, side: SideId, seed: u64, stats: &mut FuzzStats) {
    let Some(last) = g.chunks.last() else { return };
    if !last.battle.is_over() {
        return;
    }
    let s0 = Series::new(3);
    let (s1, _) = feed_game(g, side, Legal::default(), seed, Some(&s0), None, stats, "series-g1");
    let Some(rec1) = s1.recorded_series().cloned() else {
        stats.series.push(format!("{} seed {seed} {side:?}: game 1 not recorded", g.format.id));
        return;
    };
    let winner = last.battle.result().and_then(|r| r.winner);
    if s1.winner() != winner {
        stats.series.push(format!("{} seed {seed}: winner {:?} vs {winner:?}", g.format.id, s1.winner()));
    }
    let Some(next) = rec1.next_game() else { return };
    // Game 2: the same channel, its `|win|` turned into `|tie`.
    let mut chunks = g.chunks.clone();
    for c in &mut chunks {
        for l in &mut c.lines {
            if l.starts_with("|win|") {
                *l = "|tie".into();
            }
        }
    }
    let g2 = Game {
        format: g.format.clone(),
        header: g.header.clone(),
        showteam: g.showteam.clone(),
        chunks,
        team: g.team.clone(),
        shared: g.shared.clone(),
    };
    let mut full = session(&g2, side, Some(&next), true);
    let mut rng = BattleRng::new(Seed::fast(seed));
    let rejoin_at = 1 + rng.random(g2.chunks.len().max(2) as u32 - 1) as usize;
    let mut prefix: Vec<String> = Vec::new();
    for k in 0..g2.chunks.len() {
        let lines = chunk_lines(&g2, k, Legal::default(), &mut rng);
        for l in &lines {
            full.feed(l);
        }
        prefix.extend(lines);
        if k == rejoin_at && full.needs_action() {
            // A rejoin: a fresh session for the same game, the whole log.
            let mut r = session(&g2, side, Some(&next), true);
            for l in &prefix {
                r.feed(l);
            }
            let same = r.tracker() == full.tracker()
                && r.overlay() == full.overlay()
                && r.series_row() == full.series_row()
                && r.obs() == full.obs();
            if !same {
                stats.series.push(format!("{} seed {seed} {side:?}: rejoin at chunk {k} differs", g.format.id));
            }
        }
        if full.needs_action() {
            let _ = full.choose_builtin("random", seed + k as u64);
        }
    }
    match full.recorded_series() {
        Some(r) => {
            if r.ties != 1 || r.previous.len() != 2 || r.game != 2 {
                stats.series.push(format!("{} seed {seed}: tie game recorded as {:?}", g.format.id, r.row()));
            }
            let row = full.series_row().unwrap_or_default();
            let (mine, theirs) = (rec1.wins[side.index()], rec1.wins[side.foe().index()]);
            if row[..3] != [2, mine, theirs] || r.result == SeriesResult::Ongoing && row[3] != 0 {
                stats.series.push(format!("{} seed {seed}: row {row:?} after a tie (1-0 before)", g.format.id));
            }
        }
        None => stats.series.push(format!("{} seed {seed}: game 2 (tie) not recorded", g.format.id)),
    }
}
