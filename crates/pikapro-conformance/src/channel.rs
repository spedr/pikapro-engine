//! The player-channel round trip.
//!
//! A [`ChannelSession`] fed a player's channel (the lines and requests
//! Showdown sends it) is compared with what the environment gives that
//! player: the view (tracker and overlay), the observation and the mask,
//! at every decision. [`roundtrip`] checks it against [`VecEnv`] itself: one
//! battle in a one-battle environment, both players' actions from the
//! caller (random ones from their masks), and a plain battle replaying the
//! environment's recorded choices in lockstep, whose events are rendered
//! into each player's channel (as Showdown's `getPlayerStreams` sends it:
//! the update's lines, then each side's request; an unavailable choice's
//! `|error|` and updated request) and fed to one session per player.
//!
//! [`channel_showdown`](crate::channel_showdown) checks the parser against Showdown's own text: the
//! oracle's player streams of a conformance battle, fed to sessions,
//! against the typed path of the same battle on pikapro.

use std::sync::Arc;

use pikapro::request::{MoveChoices, RequestData, RequestKind};
use pikapro::{Battle, BattleOptions, BattleRng, Choice, ChoiceError, Gender, Progress, Seed, SideId, SlotAction};
use pikapro_protocol::channel::LineKind;
use pikapro_protocol::{BattleInfo, Perspective, Renderer, parse_choice_for};
use pikapro_rl::env::{EnvConfig, VecEnv};
use pikapro_rl::series::{SERIES_WIDTH, Series};
use pikapro_rl::session::{ChannelSession, SessionOptions};
use pikapro_rl::space::ActionSpace;
use pikapro_rl::vec::{MaskLayout, Opponent, StepOut, SwitchBy, Teams, status};
use pikapro_view::{HpReport, OwnOverlay, PublicSide, PublicTracker};

use crate::view::{Failure, Setup, debug_diff};

/// Mixed into a battle's seed for the players' random actions.
const ACTION_SALT: u64 = 0xC4A7_7E1D;

/// What the round trip compares under.
#[derive(Clone, Debug)]
pub struct Options {
    pub masks: MaskLayout,
    pub switch_by: SwitchBy,
    pub hp: HpReport,
    /// Open Team Sheets: the environment shows both sheets
    /// (`EnvConfig::ots`), the channels `|showteam|` lines.
    pub ots: bool,
    /// Keep both channels' lines ([`Stats::logs`]), for a fixture.
    pub keep_logs: bool,
    /// Reconnects: at every step, a new session per player joins the
    /// battle as Showdown lets a player rejoin its room ([`rejoin`]) and
    /// must give what the session that saw it all gives ([`check_rejoin`]).
    pub reconnect: bool,
    /// A best-of series (`Some(3)`: Bo3), with open sheets: the environment
    /// in series mode (`EnvConfig::series`) plays the battle's games, and
    /// each game's sessions are given the series the last game's sessions
    /// recorded ([`ChannelSession::set_series`]). Their views must show the
    /// environment's series (the score, the game, the earlier games'
    /// records) and their `series` rows the environment's. Formats with
    /// team preview only (where Showdown shows the sheets).
    pub series: Option<u8>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            masks: MaskLayout::Factored,
            switch_by: SwitchBy::Row,
            hp: HpReport::Percent,
            ots: false,
            keep_logs: false,
            reconnect: false,
            series: None,
        }
    }
}

/// What Showdown sends a player who joins its battle's room again
/// (`GameRoom.onConnect`, then `RoomBattle.onConnect`): `|init|battle`,
/// `|title|`, the room's log so far as the player's channel shows it
/// (`Roomlog.getScrollback`: each `|split|` block as the player's own
/// line), then its current request. `stream` is the player's channel so
/// far, ending with that request; its requests and `|error|` lines went to
/// the player alone, not into the log.
pub fn rejoin<S: AsRef<str>>(stream: &[S]) -> Vec<String> {
    let mut out = vec!["|init|battle".to_string(), "|title|p1 vs. p2".to_string(), "|j|☆p1".to_string()];
    let mut request = None;
    for line in stream {
        let line = line.as_ref();
        if line.starts_with("|request|") {
            request = Some(line);
        } else if !line.starts_with("|error|") {
            out.push(line.to_string());
        }
    }
    out.extend(request.map(str::to_string));
    out
}

/// A session for `format` as the harnesses make them: player 2 knows its
/// username, player 1 learns its side from its first request.
/// `team` is the team it plays (the sessions are given theirs, as the
/// environment has them).
pub fn new_session(
    format: &pikapro::Format,
    side: SideId,
    options: &Options,
    team: Option<pikapro::Team>,
) -> Result<ChannelSession, Failure> {
    let exact_hp = BattleInfo::for_format(format).report_exact_hp;
    let session_options = SessionOptions::default()
        .with_masks(options.masks)
        .with_switch_by(options.switch_by)
        .with_hp(options.hp)
        .with_username((side == SideId::P2).then(|| "p2".to_string()))
        .with_team(team);
    ChannelSession::for_format(format.clone(), exact_hp, session_options)
        .map_err(|e| Failure::new("engine", "session", e))
}

/// A session that joins player `side`'s battle now (`stream`: the
/// channel so far, as `full` read it) must give what `full`, which read
/// it all, gives: the tracker (facts and the player's own ids), the
/// overlay, the observation, the mask and whether to act. After an
/// unavailable choice the rejoined request is the update (`update: true`,
/// which the rejoined overlay's `updated` reads), which shows what the
/// refusal taught but not that a refusal taught it: the overlay's
/// `known_*` aren't compared then.
///
/// Returns whether it compared: not when the lines fit two team orders
/// (`ChannelParser::ambiguous_start`: an own Illusion user never
/// revealed, with its disguise's max HP), which no client can tell apart.
pub fn check_rejoin<S: AsRef<str>>(
    full: &mut ChannelSession,
    stream: &[S],
    side: SideId,
    options: &Options,
) -> Result<bool, Failure> {
    let mut lines = rejoin(stream);
    if full.overlay().answered && full.request_kind() != pikapro_rl::session::RequestKind::Wait {
        // The player chose already: Showdown sends the choice after the
        // request (its text doesn't matter here).
        lines.push("|sentchoice|default".to_string());
    }
    let mut s = new_session(full.format(), side, options, full.options().team.clone())?;
    if let Some(series) = full.series() {
        // A game of a series: the client carries the series to the rejoined room.
        s.set_series(series.clone()).map_err(|e| Failure::new("rejoin", "series", e))?;
    }
    let fail = |fact: &str, m: String| {
        if std::env::var_os("PIKAPRO_CHANNEL_DEBUG").is_some() {
            eprintln!("{}", lines.join("\n"));
        }
        let context: Vec<String> = lines.iter().rev().take(8).rev().cloned().collect();
        Failure::new("rejoin", fact.to_string(), format!("{side:?} after {} lines: {m}", lines.len()))
            .with_context(&context)
    };
    for line in &lines {
        if s.feed(line) == LineKind::Unknown {
            return Err(fail("unknown-line", line.clone()));
        }
    }
    if let Some(e) = s.error() {
        return Err(fail("request", e.to_string()));
    }
    if s.side() != full.side() {
        return Err(fail("side", format!("{:?} vs {:?}", s.side(), full.side())));
    }
    if s.parser().ambiguous_start() {
        return Ok(false);
    }
    compare_tracker(full.tracker(), s.tracker(), side).map_err(|m| {
        if std::env::var_os("PIKAPRO_CHANNEL_DEBUG").is_some() {
            eprintln!("full: {:?}\nrejoined: {:?}", full.parser(), s.parser());
        }
        fail("tracker", m)
    })?;
    let mut mine = *s.overlay();
    let theirs = *full.overlay();
    let updated =
        lines.iter().rev().find(|l| l.starts_with("|request|")).is_some_and(|l| l.contains("\"update\":true"));
    if updated {
        (mine.known_trapped, mine.known_disabled) = (theirs.known_trapped, theirs.known_disabled);
    }
    if !overlay_eq(&theirs, &mine) {
        return Err(fail("overlay", debug_diff(&theirs, &mine)));
    }
    // The honest gap: a session that joins mid-battle hasn't seen the
    // requests before. Given its team, it doesn't know the PP its benched
    // Pokémon had when last active (it takes the team's maximum) or a
    // copied move's (`other_pp`), nor the
    // gender the team preview request showed of a Pokémon left out (its
    // `|poke|` line shows it all the same): its team may differ from the
    // full session's there and nowhere else. Without its team, what past
    // requests showed is all a session knows of it (the Pokémon left out
    // at team preview, the benched ones' PP), and the rejoined one knows
    // only the last. Either way the observation must be the full
    // session's view with the rejoined session's team.
    let (mine, theirs) = (*s.team(), *full.team());
    if full.options().team.is_some() {
        let gap = |m: &pikapro_view::TeamMon| pikapro_view::TeamMon {
            pp: [0; pikapro::MAX_MOVES],
            pp_known: false,
            other_pp: [None; pikapro::MAX_MOVES],
            gender: None,
            ..*m
        };
        let alike =
            mine.mons.len() == theirs.mons.len() && mine.mons.iter().zip(&theirs.mons).all(|(a, b)| gap(a) == gap(b));
        if !alike {
            return Err(fail("team", debug_diff(&theirs, &mine)));
        }
    }
    let tables = *full.tables();
    let me = full.side().unwrap_or(side);
    let (tracker, overlay) = (*full.tracker(), *full.overlay());
    let view = pikapro_view::PlayerView::new(me, &tracker, &overlay).with_team(&mine);
    let encoder = &full.options().observation;
    let mut want = vec![0.0; encoder.size()];
    encoder.encode(&view, &tables, &mut want);
    if s.obs() != &want[..] {
        return Err(fail("obs", "values differ".into()));
    }
    if mine == theirs && s.obs() != full.obs() {
        return Err(fail("obs", "values differ".into()));
    }
    if s.series_row() != full.series_row() {
        return Err(fail("series", format!("{:?} vs {:?}", s.series_row(), full.series_row())));
    }
    if s.needs_action() != full.needs_action() || s.request_kind() != full.request_kind() {
        return Err(fail("needs-action", format!("{} vs {}", s.needs_action(), full.needs_action())));
    }
    if s.mask() != full.mask() || s.min_switches() != full.min_switches() {
        return Err(fail("mask", "differs".into()));
    }
    Ok(true)
}

/// What a battle's round trip compared.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Decisions compared (per player).
    pub decisions: u64,
    /// Views compared (after every step, both players).
    pub views: u64,
    /// Lines fed to the sessions.
    pub lines: u64,
    /// Unavailable choices (an update request).
    pub unavailable: u64,
    pub turns: u64,
    /// Sessions that joined mid-battle ([`Options::reconnect`]) and were
    /// compared.
    pub rejoins: u64,
    /// Sessions that joined where the lines fit two team orders, not
    /// compared ([`check_rejoin`]).
    pub ambiguous_rejoins: u64,
    /// Each player's channel as fed (with [`Options::keep_logs`]).
    pub logs: Option<[Vec<String>; 2]>,
    /// The battle wasn't compared: a side has two Pokémon its channel
    /// shows identically ([`indistinguishable`]), or wasn't compared from
    /// where a line named one of two alike ([`named_alike`]).
    pub skipped: bool,
    /// Games played ([`Options::series`]: the series' games).
    pub games: u64,
}

/// A player's channel as the harness writes it: its renderer and session.
struct Player {
    side: SideId,
    renderer: Renderer,
    session: ChannelSession,
    /// The lines fed so far (for a failure's context).
    log: Vec<String>,
}

impl Player {
    fn feed(&mut self, line: &str, stats: &mut Stats) -> Result<(), Failure> {
        stats.lines += 1;
        self.log.push(line.to_string());
        if self.session.feed(line) == LineKind::Unknown {
            return Err(Failure::new("channel", "unknown-line", format!("{:?}: unknown line {line:?}", self.side)));
        }
        if let Some(e) = self.session.error() {
            return Err(Failure::new("channel", "request", format!("{:?}: {e} in {line}", self.side)));
        }
        Ok(())
    }

    fn context(&self) -> Vec<String> {
        let n = if std::env::var_os("PIKAPRO_CHANNEL_DEBUG").is_some() { usize::MAX } else { 12 };
        self.log.iter().rev().take(n).rev().cloned().collect()
    }
}

/// The side's request as sent, with what refused choices changed.
fn request(battle: &Battle, side: SideId) -> RequestData {
    let mut data = battle.request_data(side);
    data.answered = false;
    data
}

/// Checks one battle (see the module docs); with [`Options::series`],
/// each game of its series.
pub fn roundtrip(setup: &Setup, options: &Options) -> Result<Stats, Failure> {
    let format = &setup.format;
    let mut stats = Stats::default();
    if options.series.is_some() && !format.team_preview.shown() {
        return Err(Failure::new("engine", "series", "a series needs team preview (where Showdown shows the sheets)"));
    }
    let sheets = Arc::new(setup.sheets.clone());
    let mut config = EnvConfig::new(crate::oracle_format(format), 1, Teams::Pairs(vec![sheets.clone()]));
    config.seed = setup.seed;
    config.opponent = Opponent::External.into();
    config.masks = options.masks;
    config.switch_by = options.switch_by;
    config.hp = options.hp;
    // Showdown shows Open Team Sheets at team preview only; a series plays
    // with them.
    let ots = (options.ots || options.series.is_some()) && format.team_preview.shown();
    config.ots = if ots { 1.0 } else { 0.0 };
    config.series = options.series;
    config.record = true;
    let env = VecEnv::new(config, 1).map_err(|e| Failure::new("engine", "env", e))?;
    let mut bufs = env.buffers();
    env.reset(Some(setup.seed), &mut bufs.as_out()).map_err(|e| Failure::new("engine", "env", e))?;
    let space = env.space().clone();
    let names = setup.names();
    let active = format.active_per_side();
    let per = env.actions_per_player();
    let mut rng = BattleRng::new(Seed::fast(setup.seed ^ ACTION_SALT));
    // The series as the sessions carry it from game to game (wins by side).
    let mut series = options.series.map(Series::new);
    for game in 0u64.. {
        if game > 0 {
            // `NEXT_STEP` autoreset: this step starts the series' next game
            // (and ignores the actions).
            env.step(&vec![0; 2 * per], &mut bufs.as_out()).map_err(|e| Failure::new("engine", "env", e))?;
        }
        let Some((mut battle, mut players)) = start_game(setup, options, game, series.as_ref(), ots, &mut stats)?
        else {
            stats.skipped = true;
            return Ok(stats);
        };
        stats.games += 1;
        let turns_before = stats.turns;
        let mut recorded = 0usize;
        loop {
            if let Err(f) = compare(&env, &mut bufs.as_out(), &space, &mut players, &mut stats) {
                // A name two Pokémon share, named off the field: no client
                // can tell which one it is ([`named_alike`]).
                let shared = shared_names(&battle, &names);
                if f.fact == "tracker" && players.iter().any(|p| named_alike(&p.log, &shared)) {
                    stats.skipped = true;
                    return Ok(stats);
                }
                return Err(f);
            }
            let over = bufs.terminated[0] || bufs.truncated[0];
            if options.reconnect && !over {
                for p in &mut players {
                    if check_rejoin(&mut p.session, &p.log, p.side, options)? {
                        stats.rejoins += 1;
                    } else {
                        stats.ambiguous_rejoins += 1;
                    }
                }
            }
            if over {
                for p in &players {
                    if !p.session.finished() {
                        return Err(Failure::new(
                            "channel",
                            "end",
                            format!("{:?}: the session missed the end", p.side),
                        )
                        .with_context(&p.context()));
                    }
                }
                let winner = battle.result().and_then(|r| r.winner);
                for p in &players {
                    if p.session.winner() != winner {
                        return Err(Failure::new(
                            "channel",
                            "winner",
                            format!("{:?}: winner {:?}, the battle's {winner:?}", p.side, p.session.winner()),
                        ));
                    }
                }
                if options.keep_logs {
                    stats.logs = Some([players[0].log.clone(), players[1].log.clone()]);
                }
                break;
            }
            // Each asked player's action, drawn from its mask.
            let mut actions = vec![0i64; 2 * per];
            let mut chosen: [Option<(Choice, String)>; 2] = [None, None];
            for pl in 0..2 {
                if !bufs.needs_action[pl] {
                    continue;
                }
                let width = bufs.masks.len() / 2;
                let mask = &bufs.masks[pl * width..][..width];
                let acts = random_action(&space, options.masks, mask, bufs.min_switches[pl], &mut rng);
                actions[pl * per..][..per].copy_from_slice(&acts[..per]);
                let p = &mut players[pl];
                // Decoded before the choice answers the request (the session's
                // mask is all false then, as the environment's).
                let decoded = p.session.decode(&acts[..per]);
                let text = p.session.choice(&acts[..per]).map_err(|e| {
                    Failure::new("channel", "choice", format!("{:?}: {e}", p.side)).with_context(&p.context())
                })?;
                // The text is Showdown's for the choice the environment decodes
                // (or one Showdown refuses as unavailable: a hidden trap or
                // disable, which the mask shows).
                let parsed = match parse_choice_for(&battle, p.side, &text, &names[p.side.index()]) {
                    Ok(c) => c,
                    Err(e) if e.unavailable => {
                        decoded.ok_or_else(|| Failure::new("channel", "choice", "undecodable"))?
                    }
                    Err(e) => {
                        return Err(Failure::new("channel", "choice", format!("{:?}: {text:?} refused: {e}", p.side)));
                    }
                };
                chosen[pl] = Some((parsed, text));
            }
            env.step(&actions, &mut bufs.as_out()).map_err(|e| Failure::new("engine", "env", e))?;
            // The environment's submissions, replayed in order.
            let record = env.record(0, false).ok_or_else(|| Failure::new("engine", "record", "no record"))?;
            for &(side, choice) in &record.choices[recorded..] {
                let pl = side.index();
                if let Some((parsed, text)) = &chosen[pl]
                    && !same_choice(&battle, side, parsed, &choice, active)
                {
                    return Err(Failure::new(
                        "channel",
                        "choice",
                        format!("{side:?}: {text:?} is {parsed:?}, the environment submitted {choice:?}"),
                    ));
                }
                match battle.submit(side, choice) {
                    Ok(Progress::Resolved) => {
                        let events: Vec<_> = battle.drain_events().collect();
                        stats.turns = turns_before + battle.turn() as u64;
                        for p in &mut players {
                            for line in p.renderer.log(events.iter()) {
                                p.feed(&line, &mut stats)?;
                            }
                        }
                        send_requests(&battle, &mut players, &mut stats)?;
                    }
                    Ok(Progress::Waiting) => {}
                    Err(e) => return Err(Failure::new("engine", "replay", format!("{side:?} {choice:?}: {e}"))),
                }
            }
            recorded = record.choices.len();
            // Unavailable choices: Showdown's error and updated request.
            for pl in 0..2 {
                if bufs.status[pl] != status::UNAVAILABLE {
                    continue;
                }
                stats.unavailable += 1;
                let Some((choice, _)) = chosen[pl] else { continue };
                let side = SideId::BOTH[pl];
                let p = &mut players[pl];
                let (message, data) = unavailable_update(&battle, side, &p.session, &choice)?;
                p.feed(&format!("|error|[Unavailable choice] {message}"), &mut stats)?;
                let mut json = p.renderer.request_json(&data);
                json.pop();
                json.push_str(",\"update\":true}");
                p.feed(&format!("|request|{json}"), &mut stats)?;
            }
        }
        // The game ended: the series goes on, or it (or the battle) is over.
        if series.is_none() {
            break;
        }
        match check_series_end(&env, &players)?.next_game() {
            Some(next) => series = Some(next),
            None => break,
        }
    }
    Ok(stats)
}

/// Game `game` of `setup`'s battle (the environment's episode `game`): the
/// shadow battle replaying it, and each player's session (with `series`,
/// the series' game), fed the battle's first update. `None` when a side has
/// two Pokémon its channel shows alike ([`indistinguishable`]).
fn start_game(
    setup: &Setup,
    options: &Options,
    game: u64,
    series: Option<&Series>,
    ots: bool,
    stats: &mut Stats,
) -> Result<Option<(Battle, Vec<Player>)>, Failure> {
    let format = &setup.format;
    // The shadow battle: the environment's episode, replayed.
    let mut options_b = BattleOptions::new(Seed::fast(pikapro_rl::env::episode_seed(setup.seed, game)));
    options_b.log_events = true;
    let teams = setup.sheets.each_ref().map(|s| s.team.clone());
    let mut battle = Battle::new(crate::oracle_format(format), teams, options_b)
        .map_err(|e| Failure::new("engine", "teams", e.to_string()))?;
    if indistinguishable(&battle, &setup.names()) {
        return Ok(None);
    }
    let mut players: Vec<Player> = SideId::BOTH
        .iter()
        .map(|&side| {
            // Each session plays its side's team, as the environment does.
            let mut session = new_session(format, side, options, Some(setup.sheets[side.index()].team.clone()))?;
            if let Some(s) = series {
                session.set_series(s.clone()).map_err(|e| Failure::new("channel", "series", e))?;
            }
            Ok(Player { side, renderer: setup.renderer(Perspective::Player(side)), session, log: Vec::new() })
        })
        .collect::<Result<_, Failure>>()?;

    // The battle's first update: the header, the events so far, then the
    // requests.
    let first: Vec<_> = battle.drain_events().collect();
    for p in &mut players {
        let mut lines = p.renderer.header();
        lines.extend(p.renderer.log(first.iter()));
        for line in &lines {
            p.feed(line, stats)?;
        }
    }
    if ots {
        // Showdown's Open Team Sheets: each side's sets, packed, with
        // `pokemon.gender` (the one the battle drew).
        for side in SideId::BOTH {
            let mut sheet = setup.sheets[side.index()].clone();
            for mon in battle.state().side(side).team.iter() {
                if let Some(set) = sheet.team.sets.get_mut(usize::from(mon.id))
                    && matches!(mon.gender, Gender::Male | Gender::Female)
                {
                    set.gender = Some(mon.gender);
                }
            }
            let line = format!(
                "|showteam|{}|{}",
                pikapro_protocol::side_id(side),
                pikapro_protocol::pack_team(&sheet, format)
            );
            for p in &mut players {
                p.feed(&line, stats)?;
            }
        }
    }
    send_requests(&battle, &mut players, stats)?;
    Ok(Some((battle, players)))
}

/// The environment's series (wins by player; player 0 plays
/// `agent_side`) with its wins and result by side, as a session keeps it.
fn series_by_side(s: &Series, agent_side: SideId) -> Series {
    let mut out = s.clone();
    out.wins[agent_side.index()] = s.wins[0];
    out.wins[agent_side.foe().index()] = s.wins[1];
    out.result = if agent_side == SideId::P1 { s.result } else { s.result.swapped() };
    out
}

/// The environment's series now, by side.
fn env_series(env: &VecEnv) -> Option<Series> {
    env.with_env(0, |e| e.slot.series().map(|s| series_by_side(s, e.slot.agent_side()))).flatten()
}

/// At a game's end: each session's recorded series
/// ([`ChannelSession::recorded_series`]) must be the environment's, the
/// earlier games' public records included (compared as the live trackers
/// are, [`compare_tracker`]). Returns it.
fn check_series_end(env: &VecEnv, players: &[Player]) -> Result<Series, Failure> {
    let theirs = env_series(env).ok_or_else(|| Failure::new("engine", "series", "the environment has no series"))?;
    let facts = |s: &Series| (s.best_of, s.game, s.wins, s.ties, s.result, s.previous.len());
    for p in players {
        let fail =
            |m: String| Failure::new("channel", "series", format!("{:?}: {m}", p.side)).with_context(&p.context());
        let mine = p.session.recorded_series().ok_or_else(|| fail("no series recorded at the end".into()))?;
        if facts(mine) != facts(&theirs) {
            return Err(fail(format!("recorded {:?}, the environment's {:?}", facts(mine), facts(&theirs))));
        }
        for (k, (a, b)) in theirs.previous.iter().zip(&mine.previous).enumerate() {
            compare_tracker(a, b, p.side).map_err(|m| fail(format!("game {}'s record: {m}", k + 1)))?;
        }
    }
    Ok(theirs)
}

/// Whether a side has two Pokémon the other player's channel can't tell
/// apart: with team preview, two `|poke|` lines alike (species as shown,
/// level, gender), as the first to come in could be either; without, two
/// of one name and details. No client can; the typed path's ids can.
pub fn indistinguishable(battle: &Battle, names: &[Vec<String>; 2]) -> bool {
    let format = battle.format();
    let preview = format.team_preview.shown();
    SideId::BOTH.iter().any(|&side| {
        let team = &battle.state().side(side).team;
        let key = |m: &pikapro::PokemonState| {
            let shown = pikapro_protocol::details_species(m.base_species);
            let species = pikapro_protocol::preview_wildcard(format.dex, format.mechanics, shown).unwrap_or(shown);
            let name = if preview {
                String::new()
            } else {
                names[side.index()].get(m.id as usize).cloned().unwrap_or_default()
            };
            (species, m.level, m.gender, name)
        };
        let keys: Vec<_> = team.iter().map(key).collect();
        keys.iter().enumerate().any(|(i, k)| keys[..i].contains(k))
    })
}

/// Per side, the names two of its Pokémon share: a nickname, or two
/// formes named after their base species (Tauros-Paldea-Combat and
/// Tauros-Paldea-Aqua are both "Tauros").
pub fn shared_names(battle: &Battle, names: &[Vec<String>; 2]) -> [Vec<String>; 2] {
    let dex = battle.format().dex;
    SideId::BOTH.map(|side| {
        let team = &battle.state().side(side).team;
        let name = |m: &pikapro::PokemonState| match names[side.index()].get(m.id as usize) {
            Some(n) if !n.is_empty() => n.clone(),
            _ => dex.species(dex.species(m.base_species).base_species).name.to_string(),
        };
        let all: Vec<String> = team.iter().map(name).collect();
        let mut shared: Vec<String> =
            all.iter().enumerate().filter(|(i, n)| all[..*i].contains(n)).map(|(_, n)| n.clone()).collect();
        shared.dedup();
        shared
    })
}

/// Whether a line of `log` names a Pokémon off the field (an ident
/// without a slot: `|-heal|p1: Tauros|49/100|[from] move: Revival
/// Blessing`) by a name two of its side share ([`shared_names`]). Such an
/// ident shows the name alone, and a percentage no max HP: no client can
/// tell which Pokémon it is, the typed path's ids can. Unlike
/// [`indistinguishable`], which skips a battle before it starts, this
/// excuses a battle only once its channel showed such a line and its view
/// then differed (two fainted Tauros, one revived).
pub fn named_alike<'a>(log: impl IntoIterator<Item = &'a String>, shared: &[Vec<String>; 2]) -> bool {
    let idents: Vec<String> = SideId::BOTH
        .iter()
        .flat_map(|&side| shared[side.index()].iter().map(move |n| format!("{}: {n}", pikapro_protocol::side_id(side))))
        .collect();
    if idents.is_empty() {
        return false;
    }
    log.into_iter().any(|line| {
        line.split('|').any(|field| {
            let field = field.strip_prefix("[of] ").unwrap_or(field);
            idents.iter().any(|i| i == field)
        })
    })
}

/// Whether two choices are the same (a team order compared on the
/// Pokémon brought). A Pokémon with no usable move struggles whatever it
/// chose, and `chooseMove` drops the gimmick then (the request may offer
/// one: moves only a foe's Imprison disables), as Showdown reads the text.
fn same_choice(battle: &pikapro::Battle, side: pikapro::SideId, a: &Choice, b: &Choice, active: usize) -> bool {
    let plain = |x: SlotAction| match x {
        SlotAction::Move { index, target, .. } => SlotAction::Move { index, target, gimmick: None },
        other => other,
    };
    let struggling = |slot: usize| battle.state().side(side).active(slot).is_some_and(|m| !m.has_usable_move());
    match (a, b) {
        (Choice::Actions(x), Choice::Actions(y)) => {
            (0..active).all(|i| x[i] == y[i] || struggling(i) && plain(x[i]) == plain(y[i]))
        }
        (Choice::Team(x), Choice::Team(y)) => {
            let n = x.len().min(y.len());
            x[..n] == y[..n]
        }
        _ => false,
    }
}

/// After a resolution, each side's request (Showdown sends both, a wait
/// request to the side with nothing to decide).
fn send_requests(battle: &Battle, players: &mut [Player], stats: &mut Stats) -> Result<(), Failure> {
    if battle.is_over() {
        return Ok(());
    }
    for p in players.iter_mut() {
        let json = p.renderer.request_json(&request(battle, p.side));
        p.feed(&format!("|request|{json}"), stats)?;
    }
    Ok(())
}

/// Showdown's `emitChoiceError` with an update for a choice the battle
/// refused: the message and the updated request (`updateRequestForPokemon`
/// on the request the session last read).
fn unavailable_update(
    battle: &Battle,
    side: SideId,
    session: &ChannelSession,
    choice: &Choice,
) -> Result<(&'static str, RequestData), Failure> {
    let Choice::Actions(actions) = choice else {
        return Err(Failure::new("channel", "unavailable", "a team order refused"));
    };
    let legal = battle.legal_actions(side);
    let Err(ChoiceError::IllegalAction { slot }) = legal.check(choice) else {
        return Err(Failure::new("channel", "unavailable", format!("{choice:?} isn't refused for one slot")));
    };
    let slot = slot as usize;
    // The request as the session holds it (earlier updates included).
    let mut data = request(battle, side);
    let own = session.overlay();
    let RequestKind::Move { active } = &mut data.kind else {
        return Err(Failure::new("channel", "unavailable", "not a move request"));
    };
    for (s, a) in active.iter_mut().enumerate() {
        if let (Some(a), Some(o)) = (a.as_mut(), own.active.get(s).copied().flatten()) {
            a.trapped = o.trapped;
            a.maybe_trapped = o.maybe_trapped;
            a.maybe_disabled = o.maybe_disabled;
            a.maybe_locked = o.maybe_locked;
            a.can_mega_evo = o.can_mega_evo;
            a.can_terastallize = o.can_terastallize;
            if let (MoveChoices::Moves(ms), pikapro_view::OwnMoves::Moves(os)) = (&mut a.moves, o.moves) {
                for (m, x) in ms.iter_mut().zip(os.iter()) {
                    m.disabled = x.disabled;
                }
            }
        }
    }
    let doubles = battle.format().active_per_side() > 1;
    let usable = battle.state().side(side).active(slot).is_some_and(|m| m.has_usable_move());
    let enabled = |k: usize| {
        usable && legal.slot(slot).iter().any(|a| matches!(a, SlotAction::Move { index, .. } if *index as usize == k))
    };
    let Some(Some(a)) = active.get_mut(slot) else {
        return Err(Failure::new("channel", "unavailable", "no request for the slot"));
    };
    let message = match actions[slot] {
        SlotAction::Switch(_) => {
            a.maybe_trapped = false;
            a.trapped = true;
            "Can't switch: The active Pokémon is trapped"
        }
        SlotAction::Move { index, .. } => {
            // `updateDisabledRequest`, then the refused move.
            a.maybe_locked = false;
            if let MoveChoices::Moves(ms) = &mut a.moves {
                if a.maybe_disabled && doubles {
                    a.maybe_disabled = false;
                    for (i, m) in ms.iter_mut().enumerate() {
                        if !enabled(i) {
                            m.disabled = true;
                        }
                    }
                }
                if ms.iter().all(|m| m.disabled) {
                    a.can_mega_evo = false;
                    a.can_terastallize = None;
                }
                if let Some(m) = ms.get_mut(index as usize) {
                    m.disabled = true;
                }
            }
            "Can't move: the move is disabled"
        }
        SlotAction::Pass => return Err(Failure::new("channel", "unavailable", "a pass refused")),
    };
    Ok((message, data))
}

/// A random action allowed by a player's mask row (the environment's
/// action encoding): uniform over the allowed choices.
fn random_action(space: &ActionSpace, layout: MaskLayout, mask: &[bool], min: u8, rng: &mut BattleRng) -> Vec<i64> {
    let slots = space.active * space.slot_size;
    let mut all: Vec<Vec<i64>> = Vec::new();
    match layout {
        MaskLayout::Joint => all.extend((0..mask.len()).filter(|&i| mask[i]).map(|i| vec![i as i64])),
        MaskLayout::Factored => {
            let preview = &mask[slots..];
            if preview.iter().any(|&b| b) {
                all.extend((0..preview.len()).filter(|&i| preview[i]).map(|i| {
                    let mut v = vec![0; space.active];
                    v[0] = i as i64;
                    v
                }));
            } else if space.active < 2 {
                all.extend((0..space.slot_size).filter(|&i| mask[i]).map(|i| vec![i as i64]));
            } else {
                for i in 0..space.slot_size {
                    for j in 0..space.slot_size {
                        if space.pair_legal(&mask[..slots], min, i, j) {
                            all.push(vec![i as i64, j as i64]);
                        }
                    }
                }
            }
        }
    }
    if all.is_empty() {
        return vec![0; space.active];
    }
    all.swap_remove(rng.random(all.len() as u32) as usize)
}

/// Compares every player's session with the environment's outputs.
fn compare(
    env: &VecEnv,
    out: &mut StepOut<'_>,
    space: &ActionSpace,
    players: &mut [Player],
    stats: &mut Stats,
) -> Result<(), Failure> {
    let (tracker, own) = env
        .with_env(0, |e| (*e.slot.tracker_of(e.slot.agent_side()), SideId::BOTH.map(|side| *e.slot.own(side))))
        .ok_or_else(|| Failure::new("engine", "env", "no battle 0"))?;
    let series = env_series(env);
    let width = out.masks.len() / 2;
    for (pl, p) in players.iter_mut().enumerate() {
        let side = p.side;
        let context = p.context();
        let fail = |fact: &str, message: String| {
            let mut f = Failure::new("channel", fact.to_string(), format!("{side:?}: {message}"));
            f.context = context.clone();
            f
        };
        let s = &mut p.session;
        if s.side() != Some(side) {
            return Err(fail("side", format!("the session plays {:?}", s.side())));
        }
        stats.views += 1;
        compare_tracker(&tracker, s.tracker(), side).map_err(|m| {
            let debug = std::env::var_os("PIKAPRO_CHANNEL_DEBUG").is_some();
            fail("tracker", if debug { format!("{m}\n{:#?}", s.parser()) } else { m })
        })?;
        // The series the view shows (what an encoder may read), and the
        // `series` row (the environment's is player 0's: p1's here).
        let facts = |v: pikapro_view::SeriesView<'_>| (v.best_of, v.game, v.wins, v.ties, v.previous.len());
        let want = series.as_ref().map(|t| (t.best_of, t.game, t.wins, t.ties, t.previous.len()));
        if s.view().series.map(facts) != want {
            return Err(fail(
                "series",
                format!("the view's {:?}, the environment's {want:?}", s.view().series.map(facts)),
            ));
        }
        if let Some(rows) = out.series.as_deref() {
            let row = &rows[..SERIES_WIDTH];
            let swap = |r: u8| match r {
                1 => 2,
                2 => 1,
                other => other,
            };
            let want = if side == SideId::P1 {
                [row[0], row[1], row[2], row[3]]
            } else {
                [row[0], row[2], row[1], swap(row[3])]
            };
            if s.series_row() != Some(want) {
                return Err(fail("series", format!("row {:?}, the environment's {want:?}", s.series_row())));
            }
        }
        // After the end, no request comes: the own side stays the last
        // request's (no decision is made on it).
        if out.terminated[0] || out.truncated[0] {
            continue;
        }
        let theirs = own[side.index()];
        let mine = *s.overlay();
        if !overlay_eq(&theirs, &mine) {
            return Err(fail("overlay", debug_diff(&theirs, &mine)));
        }
        // The observation.
        let n = out.obs.len() / 2;
        let theirs = &out.obs[pl * n..][..n];
        let mine = s.obs();
        if mine.len() != theirs.len() {
            return Err(fail("obs", format!("{} values, the environment has {}", mine.len(), theirs.len())));
        }
        if let Some(i) = mine.iter().zip(theirs).position(|(a, b)| a != b) {
            return Err(fail("obs", format!("value {i}: {} vs {}", mine[i], theirs[i])));
        }
        let needs = out.needs_action[pl];
        if s.needs_action() != needs {
            return Err(fail("needs-action", format!("session {} environment {needs}", s.needs_action())));
        }
        if needs {
            stats.decisions += 1;
            let theirs = &out.masks[pl * width..][..width];
            let mine = s.mask();
            if mine != theirs {
                let on = |m: &[bool]| -> Vec<String> {
                    m.iter().enumerate().filter(|&(_, &b)| b).map(|(i, _)| describe(space, i)).collect()
                };
                let (a, b) = (on(mine), on(theirs));
                let only_mine: Vec<_> = a.iter().filter(|x| !b.contains(x)).collect();
                let only_theirs: Vec<_> = b.iter().filter(|x| !a.contains(x)).collect();
                let foe = s.tracker().side(side.foe());
                let slots: Vec<_> = foe.slots.iter().map(|a| a.row.map(|r| (r, foe.mons[r as usize].flags))).collect();
                return Err(fail(
                    "mask",
                    format!("session only {only_mine:?}, environment only {only_theirs:?}; foe slots {slots:?}"),
                ));
            }
            if s.min_switches() != out.min_switches[pl] {
                return Err(fail("min-switches", format!("{} vs {}", s.min_switches(), out.min_switches[pl])));
            }
        }
    }
    Ok(())
}

/// A mask index as text (`s0:move 2 foe1 tera`).
fn describe(space: &ActionSpace, i: usize) -> String {
    let slots = space.active * space.slot_size;
    if i >= slots {
        return format!("preview {}", i - slots);
    }
    format!("s{}:{:?}", i / space.slot_size, space.decode_slot(i % space.slot_size))
}

/// The overlays are equal but for bookkeeping, and the `answered` flag
/// (a session answers by sending its choice).
pub(crate) fn overlay_eq(a: &OwnOverlay, b: &OwnOverlay) -> bool {
    let mut b = *b;
    b.answered = a.answered;
    *a == b
}

/// Two trackers show the same, for player `me`: every public fact, and
/// the ids of `me`'s own Pokémon (a foe's are its registration order in
/// the engine's events, which the text doesn't show without team preview:
/// only their rows are compared).
pub fn compare_tracker(a: &PublicTracker, b: &PublicTracker, me: SideId) -> Result<(), String> {
    let fields = |t: &PublicTracker| (t.config, t.field, t.turn, t.phase, t.outcome, t.ots);
    if fields(a) != fields(b) {
        return Err(debug_diff(&fields(a), &fields(b)));
    }
    for side in SideId::BOTH {
        let (x, y) = (a.side(side), b.side(side));
        if !side_eq(x, y) {
            if x.mons.len() != y.mons.len() {
                let species = |s: &PublicSide| s.mons.iter().map(|m| m.species).collect::<Vec<_>>();
                return Err(format!("{side:?} rows {:?} vs {:?}", species(x), species(y)));
            }
            if let Some(r) = (0..x.mons.len()).find(|&r| x.mons[r] != y.mons[r]) {
                return Err(format!("{side:?} row {r}: {}", debug_diff(&x.mons[r], &y.mons[r])));
            }
            if let Some(s) = (0..x.slots.len()).find(|&s| x.slots[s] != y.slots[s]) {
                return Err(format!("{side:?} slot {s}: {}", debug_diff(&x.slots[s], &y.slots[s])));
            }
            return Err(format!("{side:?} {}", debug_diff(&side_facts(x), &side_facts(y))));
        }
        let ids_shown = side == me || a.config.team_preview;
        if ids_shown {
            for id in 0..pikapro::MAX_TEAM as u8 {
                let actor = pikapro::event::Actor {
                    pos: pikapro::Position::new(side, 0),
                    species: Default::default(),
                    active: false,
                    id,
                };
                if a.row_of_actor(&actor) != b.row_of_actor(&actor) {
                    return Err(format!(
                        "{side:?} id {id}: row {:?} vs {:?}",
                        a.row_of_actor(&actor),
                        b.row_of_actor(&actor)
                    ));
                }
            }
        }
    }
    Ok(())
}

type SideFacts<'a> = (
    &'a [pikapro_view::MonView],
    &'a [pikapro_view::ActiveView],
    &'a [pikapro_view::ConditionView],
    (u8, u8, bool, bool),
    &'a [u8],
    &'a [pikapro_view::MonView],
    &'a [bool],
    &'a [pikapro_view::FutureView],
    &'a [pikapro_view::SlotConditions],
    &'a [pikapro_view::TurnRecord],
    &'a [pikapro_view::TurnRecord],
);

fn side_facts(s: &PublicSide) -> SideFacts<'_> {
    (
        &s.mons[..],
        &s.slots,
        &s.conditions,
        (s.team_size, s.fainted, s.tera_used, s.mega_used),
        &s.reveal_order[..],
        &s.entered,
        &s.entered_first,
        &s.future,
        &s.slot_conditions,
        &s.last_turn,
        &s.this_turn,
    )
}

fn side_eq(a: &PublicSide, b: &PublicSide) -> bool {
    side_facts(a) == side_facts(b)
}
