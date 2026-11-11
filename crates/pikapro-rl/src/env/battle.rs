//! One battle of the vector environment: its payload ([`EnvSlot`]) and
//! what a step does to it, on a worker thread.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use pikapro::legal::format_actions;
use pikapro::{Battle, BattleRng, Choice, ChoiceError, Gimmick, LegalActions, Progress, Request, Seed, SideId};
use pikapro_protocol::TeamSheet;
use pikapro_view::{OpenSheet, OwnOverlay, OwnTeam, PlayerView, PublicTracker, SeriesView};

use super::{Ctx, EpisodeRecord, OpponentSpec, OtsView, Profile};
use crate::eval::POLICY_TRIES;
use crate::pool::{BattleError, Env, PoolConfig, Slot, episode_seed, new_battle, panic_message};
use crate::reward::RewardPreset;
use crate::series::{SERIES_WIDTH, Series};
use crate::vec::{Autoreset, EnvEndReason, Opponent, PreviewMode, RANDOM_ACTION, StepBuffers, StepOut, status};

/// Mixed into an episode's battle seed for its own draws (side, opponent,
/// open sheets), so they don't share the battle's stream.
const META_SEED: u64 = 0x5EED_0FE9_150D_E5E5;
/// Mixed into an episode's battle seed for player 1's policy RNG (player
/// 0's is the pool's `!seed`).
const PLAYER_SEED: u64 = 0x0B90_4E47_5EED_0001;

/// What the vector environment keeps with each battle. Read it through
/// its methods ([`VecEnv::with_env`](super::VecEnv::with_env)).
#[derive(Clone, Debug)]
pub struct EnvSlot {
    /// What a spectator knows: fed every event after `submit` returns.
    /// With [`OtsView::Player0`] and open sheets, player 0's alone
    /// ([`EnvSlot::tracker_of`]).
    pub(crate) tracker: PublicTracker,
    /// With [`OtsView::Player0`] and open sheets, player 1's: the same
    /// events, with closed sheets. `None`
    /// otherwise: both players read `tracker`.
    pub(crate) closed: Option<Box<PublicTracker>>,
    /// Each side's own view of its request, by side. Rebuilt when a new
    /// request is issued; an unavailable choice updates it (as Showdown
    /// updates the request it sent), and it keeps what that revealed
    /// ([`OwnOverlay::revealed`]).
    pub(crate) own: [OwnOverlay; 2],
    /// Each side's own team (its sets, and the PP its requests showed), by
    /// side: [`OwnTeam::observe`] of every overlay.
    pub(crate) team: [OwnTeam; 2],
    /// Who plays player 1 this episode.
    pub(crate) opponent: Opponent,
    /// The side player 0 plays.
    pub(crate) agent_side: SideId,
    /// Each player's RNG for this episode (random actions, built-in
    /// policies, random team preview), by player.
    rng: [BattleRng; 2],
    /// Reward earned but not yet paid, by player.
    pub(crate) pending: [f32; 2],
    /// Pokémon still standing after the last update, by side (`None`
    /// before the battle starts).
    standing: Option<[u8; 2]>,
    /// The episode ended (the next step starts another, with
    /// [`Autoreset::NextStep`]).
    pub(crate) ended: EnvEndReason,
    /// The episode's battle seed ([`Env::seed`](crate::pool::Env::seed)).
    pub(crate) seed: u64,
    /// The choices submitted this episode, when recording.
    pub(crate) log: Option<Vec<(SideId, Choice)>>,
    /// The previous episode's record, when recording.
    pub(crate) previous: Option<Arc<EpisodeRecord>>,
    /// The best-of series this episode is a game of, in series mode.
    pub(crate) series: Option<Box<Series>>,
}

/// What `side` reads of a slot's record kept twice, open and closed
/// (a tracker, [`EnvSlot::tracker_of`]): the closed-sheet one, when there
/// is one and `side` isn't the agent's, else the open one.
#[inline]
fn of_side<'a, T>(open: &'a T, closed: Option<&'a T>, agent_side: SideId, side: SideId) -> &'a T {
    match closed {
        Some(c) if side != agent_side => c,
        _ => open,
    }
}

impl EnvSlot {
    /// The payload of a battle that just started: `open`, the sheets
    /// shown by side, with open sheets; `closed`, a tracker with closed
    /// sheets beside (player 1's, when player 0 alone sees the sheets);
    /// the episode's opponent, player 0's side and series (`start`).
    fn new(
        battle: &mut Battle,
        ctx: &Ctx,
        open: Option<[&TeamSheet; 2]>,
        closed: bool,
        sheets: &[TeamSheet; 2],
        seed: u64,
        start: Start,
    ) -> EnvSlot {
        let view = ctx.view;
        let mut tracker = PublicTracker::new(view);
        let mut closed = closed.then(|| Box::new(PublicTracker::new(view)));
        if let Some(open) = open {
            for side in SideId::BOTH {
                let shown = open[side.index()];
                // The genders the battle drew, unless the sheet is another team's.
                let sheet = if std::ptr::eq(shown, &sheets[side.index()]) {
                    OpenSheet::from_battle(&shown.team, battle, side)
                } else {
                    OpenSheet::from_team(&shown.team, battle.format())
                };
                tracker.set_open_sheet(side, sheet);
            }
        }
        for e in battle.drain_events() {
            tracker.apply(&e);
            if let Some(c) = &mut closed {
                c.apply(&e);
            }
        }
        let own = SideId::BOTH.map(|s| OwnOverlay::from_battle(battle, s));
        let team = SideId::BOTH.map(|s| {
            let mut t = OwnTeam::from_team(&sheets[s.index()].team, battle.format());
            t.observe(&own[s.index()]);
            t
        });
        EnvSlot {
            tracker,
            closed,
            own,
            team,
            opponent: start.opponent,
            agent_side: start.agent_side,
            rng: [BattleRng::new(Seed::fast(!seed)), BattleRng::new(Seed::fast(!seed ^ PLAYER_SEED))],
            pending: [0.0; 2],
            standing: standing(battle),
            ended: EnvEndReason::None,
            seed,
            log: ctx.record.then(Vec::new),
            previous: None,
            series: start.series,
        }
    }

    /// The side player 0 plays.
    #[inline]
    pub fn agent_side(&self) -> SideId {
        self.agent_side
    }

    /// The best-of series this episode is a game of (wins by player), in
    /// series mode.
    #[inline]
    pub fn series(&self) -> Option<&Series> {
        self.series.as_deref()
    }

    /// `side`'s own view of its request.
    #[inline]
    pub fn own(&self, side: SideId) -> &OwnOverlay {
        &self.own[side.index()]
    }

    /// `side`'s own team: its sets, and the PP its requests showed.
    #[inline]
    pub fn team(&self, side: SideId) -> &OwnTeam {
        &self.team[side.index()]
    }

    /// The side player `p` plays.
    #[inline]
    pub fn side_of(&self, p: usize) -> SideId {
        if p == 0 { self.agent_side } else { self.agent_side.foe() }
    }

    /// The player on `side`.
    #[inline]
    pub fn player_of(&self, side: SideId) -> usize {
        usize::from(side != self.agent_side)
    }

    /// Whether player `p`'s actions come from the caller.
    #[inline]
    fn external(&self, p: usize) -> bool {
        p == 0 || self.opponent.is_external()
    }

    /// `side`'s view of the battle now: what its encoder or policy reads
    /// (with its own team, and the series in series mode).
    pub fn view(&self, side: SideId) -> PlayerView<'_> {
        player_view(self.tracker_of(side), &self.own, &self.team, self.series.as_deref(), self.agent_side, side)
    }

    /// What `side` knows of the public battle: the spectator's tracker, but
    /// player 1's closed-sheet tracker when player 0 alone sees the sheets
    /// ([`OtsView::Player0`]).
    #[inline]
    pub fn tracker_of(&self, side: SideId) -> &PublicTracker {
        of_side(&self.tracker, self.closed.as_deref(), self.agent_side, side)
    }
}

impl crate::pool::sealed::Sealed for EnvSlot {}

impl Slot for EnvSlot {
    type Out = ShardOut;

    /// Never called: the vector environment starts its episodes itself
    /// (opponent, sides, sheets, series), and never calls the
    /// pool methods that start one with this ([`Pool::reset`](crate::pool::Pool::reset),
    /// `step`, `play_random`).
    fn start(_: &mut Battle) -> EnvSlot {
        unreachable!("the vector environment starts its own episodes")
    }
}

/// A shard's step buffers and time counters (the crate's own: unnameable
/// outside it).
#[derive(Debug, Default)]
pub struct ShardOut {
    /// The shard's outputs, when a call's workers can't write into the
    /// caller's buffers (the calling thread copies them there).
    pub(crate) buffers: StepBuffers,
    /// Which battles ended this step (for [`FinalOut`](crate::vec::FinalOut)).
    pub(crate) ended: Vec<bool>,
    pub(crate) profile: Profile,
    /// The battles that failed during the last call, in order.
    pub(crate) errors: Vec<BattleError>,
}

/// Where a shard's step writes: its own buffers (`kept`: they hold what
/// the last step wrote), or its part of the caller's buffers (which may
/// hold anything: every value is written).
pub(crate) struct Target<'a, 'b> {
    pub out: &'a mut StepOut<'b>,
    pub kept: bool,
    pub ended: &'a mut [bool],
    pub profile: &'a mut Profile,
    /// The battles that failed ([`EnvEndReason::Error`]).
    pub errors: &'a mut Vec<BattleError>,
}

/// `side`'s view of the battle, with its own team and the series it is a
/// game of (whose wins are by player; player 0 plays `agent_side`).
fn player_view<'a>(
    tracker: &'a PublicTracker,
    own: &'a [OwnOverlay; 2],
    team: &'a [OwnTeam; 2],
    series: Option<&'a Series>,
    agent_side: SideId,
    side: SideId,
) -> PlayerView<'a> {
    let view = PlayerView::new(side, tracker, &own[side.index()]).with_team(&team[side.index()]);
    match series {
        Some(s) => {
            let mut wins = [0; 2];
            wins[agent_side.index()] = s.wins[0];
            wins[agent_side.foe().index()] = s.wins[1];
            view.with_series(SeriesView::new(s.best_of, s.game, wins, s.ties, &s.previous))
        }
        None => view,
    }
}

/// Pokémon still standing per side, once the battle has started (with
/// team preview, only the brought ones are left in the team).
fn standing(battle: &Battle) -> Option<[u8; 2]> {
    (battle.turn() > 0)
        .then(|| SideId::BOTH.map(|s| battle.state().side(s).team.iter().filter(|p| !p.is_fainted()).count() as u8))
}

/// Nanoseconds since the last lap, when profiling.
struct Clock(Option<Instant>);

impl Clock {
    #[inline]
    fn lap(&mut self) -> u64 {
        match &mut self.0 {
            Some(t) => {
                let now = Instant::now();
                let ns = now.duration_since(*t).as_nanos() as u64;
                *t = now;
                ns
            }
            None => 0,
        }
    }
}

/// What a worker needs for one call.
pub(crate) struct Job<'a> {
    pub ctx: &'a Ctx,
    pub config: &'a PoolConfig,
    pub base_seed: u64,
    pub opponents: &'a [OpponentSpec],
    pub profile: bool,
}

/// One battle's part of a call: `actions` for a step (by player, `per`
/// values each), `None` to start a new episode (a reset).
fn run(
    job: &Job<'_>,
    env: &mut Env<EnvSlot>,
    i: usize,
    j: usize,
    actions: Option<&[i64]>,
    target: &mut Target<'_, '_>,
    stage: &mut Stage,
) -> Result<(), String> {
    let mut clock = Clock(job.profile.then(Instant::now));
    let Target { out: buf, kept, ended, profile: prof, .. } = target;
    let kept = *kept;
    prof.steps += 1;
    let mut status = [status::IGNORED; 2];
    let mut dirty = false;
    // A battle that failed starts its next episode (and a new series).
    let failed = env.poisoned.is_some();
    let new_episode = actions.is_none() || env.slot.ended.is_over() || failed;
    if new_episode {
        *stage = Stage::Starting(next_episode(job, env, i));
        begin(job, env, i, actions.is_some() && !failed)?;
        prof.other += clock.lap();
    } else if let Some(actions) = actions {
        // Who was asked before anything is submitted: a player's choice can
        // resolve the turn and issue the other a new request, which this
        // step's action (chosen for the old one) doesn't answer.
        let asked = [0, 1].map(|p| env.slot.external(p) && caller_asked(job.ctx, &env.battle, env.slot.side_of(p)));
        for (p, st) in status.iter_mut().enumerate() {
            if !asked[p] {
                continue;
            }
            let acts = &actions[p * job.ctx.per..][..job.ctx.per];
            *st = act(job.ctx, env, p, acts, &mut dirty);
        }
        prof.decisions += status.iter().filter(|&&s| s == status::SUBMITTED).count() as u64;
        prof.engine += clock.lap();
    }
    advance(job.ctx, env, dirty, prof, &mut clock)?;
    *stage = Stage::Playing;
    // Termination.
    let slot = &mut env.slot;
    let over_turns = job.ctx.max_turns.is_some_and(|m| !env.battle.is_over() && env.battle.turn() > m);
    let result = env.battle.result();
    let reason = match &result {
        Some(r) => EnvEndReason::of(r),
        None if over_turns => EnvEndReason::MaxTurns,
        None => EnvEndReason::None,
    };
    let winner = match (&result, reason) {
        (Some(r), _) => r.winner.map_or(2, |w| slot.player_of(w) as i8),
        _ => -1,
    };
    if reason.terminated() {
        let w = result.and_then(|r| r.winner);
        for p in 0..2 {
            slot.pending[p] += RewardPreset::terminal(slot.side_of(p), w);
        }
    }
    if reason.is_over() {
        prof.battles += 1;
        slot.ended = reason;
        let game_winner = if winner == 0 || winner == 1 { Some(winner as usize) } else { None };
        if let Some(series) = &mut slot.series {
            // Neither over (a new series starts with the next episode once
            // it is) nor won by a third: it records.
            let _ = series.record(game_winner, &slot.tracker);
        }
    }
    buf.end_reason[j] = reason as u8;
    buf.terminated[j] = reason.terminated();
    buf.truncated[j] = reason.truncated();
    buf.winner[j] = winner;
    ended[j] = reason.is_over();
    prof.other += clock.lap();
    // Observations, masks and rewards; with same-step autoreset the ended
    // episode's observation goes to the final buffers and the next one
    // starts.
    let same_step = reason.is_over() && job.ctx.autoreset == Autoreset::SameStep;
    let needs = observe(job.ctx, env, j, buf, kept, same_step, new_episode, prof, &mut clock);
    let slot = &mut env.slot;
    for p in 0..2 {
        let pay = reason.is_over() || needs[p];
        buf.reward[2 * j + p] = if pay { core::mem::take(&mut slot.pending[p]) } else { 0.0 };
        buf.status[2 * j + p] = status[p];
    }
    if same_step {
        let f = buf.final_out.as_mut().ok_or("same-step autoreset needs the final buffers")?;
        f.episode[j] = env.episode;
        // The turns it lasted: a `max_turns` cut comes as the next turn
        // starts, before anyone acts in it.
        f.turns[j] = match (reason, job.ctx.max_turns) {
            (EnvEndReason::MaxTurns, Some(m)) => m,
            _ => env.battle.turn(),
        };
        if let (Some(s), Some(out)) = (&env.slot.series, f.series.as_deref_mut()) {
            out[j * SERIES_WIDTH..][..SERIES_WIDTH].copy_from_slice(&s.row());
        }
        // The ended episode's rows are written: a failure from here on is
        // the next episode's, and leaves them.
        *stage = Stage::Next(next_episode(job, env, i));
        begin(job, env, i, true)?;
        prof.other += clock.lap();
        advance(job.ctx, env, false, prof, &mut clock)?;
        let needs = observe(job.ctx, env, j, buf, kept, false, true, prof, &mut clock);
        debug_assert!(!env.battle.is_over() || !needs.contains(&true));
    }
    buf.agent_side[j] = env.slot.agent_side as u8;
    buf.episode[j] = env.episode;
    if let (Some(s), Some(out)) = (&env.slot.series, buf.series.as_deref_mut()) {
        out[j * SERIES_WIDTH..][..SERIES_WIDTH].copy_from_slice(&s.row());
    }
    Ok(())
}

/// Where in a call a battle is, for what a failure there ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Playing its episode: a failure ends it.
    Playing,
    /// Starting this episode (at a reset, at the step after an episode
    /// ended with next-step autoreset, or after a failure): a failure ends
    /// it at once.
    Starting(u64),
    /// Starting this episode in the step that ended the one before
    /// (same-step autoreset): the ended one's rows stand (its end, reward,
    /// winner and final observation), and the failure ends this one.
    Next(u64),
}

/// The episode battle `i` starts next.
fn next_episode(job: &Job<'_>, env: &Env<EnvSlot>, i: usize) -> u64 {
    i as u64 + env.started * job.config.n as u64
}

/// [`run`], turning a panic or an error into the end of the failed
/// episode ([`EnvEndReason::Error`]): its rows get defined values (see
/// [`fail`]), and the battle starts its next episode at the next call. The
/// other battles aren't affected.
pub(crate) fn run_guarded(
    job: &Job<'_>,
    env: &mut Env<EnvSlot>,
    i: usize,
    j: usize,
    actions: Option<&[i64]>,
    out: &mut Target<'_, '_>,
) {
    let mut stage = Stage::Playing;
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(job, env, i, j, actions, out, &mut stage)));
    let message = match result {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e,
        Err(payload) => format!("panic: {}", panic_message(&*payload)),
    };
    env.poisoned = Some(message.clone());
    let (episode, next) = match stage {
        Stage::Playing => (env.episode, false),
        Stage::Starting(e) => (e, false),
        Stage::Next(e) => (e, true),
    };
    fail(job.ctx, env, j, out, episode, next);
    out.errors.push(BattleError { battle: i, episode, message });
}

/// Writes battle `j`'s rows for its failed `episode`: truncated with end
/// reason [`EnvEndReason::Error`], no result, no reward (what was pending
/// is dropped), zeroed observations (the final ones too, with same-step
/// autoreset), nobody asked to act.
///
/// With `next`, the episode failed to start in the step that ended the one
/// before it (same-step autoreset): that one's end reason, winner, reward,
/// statuses and final observation stand, and only the rows that describe
/// the new episode (observation, masks, who must act, `episode`) are
/// written, as for a failure.
fn fail(ctx: &Ctx, env: &mut Env<EnvSlot>, j: usize, target: &mut Target<'_, '_>, episode: u64, next: bool) {
    let Target { out: buf, ended, profile, .. } = target;
    let slot = &mut env.slot;
    slot.pending = [0.0; 2];
    slot.ended = EnvEndReason::Error;
    profile.battles += 1;
    if !next {
        let reason = EnvEndReason::Error;
        buf.end_reason[j] = reason as u8;
        buf.terminated[j] = reason.terminated();
        buf.truncated[j] = reason.truncated();
        buf.winner[j] = -1;
        ended[j] = true;
    }
    let size = ctx.shape.obs;
    let width = ctx.shape.mask;
    for k in [2 * j, 2 * j + 1] {
        buf.obs[k * size..][..size].fill(0.0);
        buf.masks[k * width..][..width].fill(false);
        buf.min_switches[k] = 0;
        buf.needs_action[k] = false;
        if !next {
            buf.reward[k] = 0.0;
            buf.status[k] = status::IGNORED;
        }
    }
    buf.agent_side[j] = slot.agent_side as u8;
    buf.episode[j] = episode;
    let row = slot.series.as_ref().map_or([0; SERIES_WIDTH], |s| s.row());
    if let Some(out) = buf.series.as_deref_mut() {
        out[j * SERIES_WIDTH..][..SERIES_WIDTH].copy_from_slice(&row);
    }
    if ctx.autoreset == Autoreset::SameStep
        && !next
        && let Some(f) = buf.final_out.as_mut()
    {
        for k in [2 * j, 2 * j + 1] {
            f.obs[k * size..][..size].fill(0.0);
        }
        f.episode[j] = episode;
        f.turns[j] = env.battle.turn();
        if let Some(out) = f.series.as_deref_mut() {
            out[j * SERIES_WIDTH..][..SERIES_WIDTH].copy_from_slice(&row);
        }
    }
}

/// The record of `env`'s current episode, when recording.
pub(crate) fn current_record(env: &Env<EnvSlot>) -> Option<Arc<EpisodeRecord>> {
    let choices = env.slot.log.clone()?;
    Some(Arc::new(EpisodeRecord {
        episode: env.episode,
        seed: env.slot.seed,
        sheets: env.sheets.clone(),
        agent_side: env.slot.agent_side,
        choices,
    }))
}

/// Battle `i`'s first episode, provisionally: it counts as not started, so
/// the first reset starts it again.
pub(crate) fn provisional(job: &Job<'_>, i: usize) -> Result<Env<EnvSlot>, String> {
    let mut new = None;
    episode(job, i, 0, None, Place::New(&mut new))?;
    let mut env = new.expect("a new episode");
    env.started = 0;
    Ok(env)
}

/// Starts battle `i`'s next episode. With `next_game` (an episode ended),
/// a series under way plays its next game; a reset starts a new series.
fn begin(job: &Job<'_>, env: &mut Env<EnvSlot>, i: usize, next_game: bool) -> Result<(), String> {
    let previous = if job.ctx.record { current_record(env) } else { None };
    let under_way = env.slot.series.as_ref().filter(|_| next_game);
    let series = under_way.and_then(|s| s.next_game()).map(|next| Carry {
        series: Box::new(next),
        sheets: env.sheets.clone(),
        agent_side: env.slot.agent_side,
        opponent: env.slot.opponent.clone(),
    });
    // Counted before it starts: an episode that fails to start (an error or
    // a panic) is skipped, not tried again.
    let started = env.started;
    env.started += 1;
    episode(job, i, started, series, Place::Over(env))?;
    env.slot.previous = previous;
    Ok(())
}

/// What the next game of a series keeps from the last.
struct Carry {
    series: Box<Series>,
    /// By side.
    sheets: Arc<[TeamSheet; 2]>,
    agent_side: SideId,
    opponent: Opponent,
}

/// What an episode's [`EnvSlot`] starts with beside its battle.
struct Start {
    opponent: Opponent,
    agent_side: SideId,
    series: Option<Box<Series>>,
}

/// Where [`episode`] puts an episode.
enum Place<'a> {
    /// Over the battle's last one, in place (building one and moving it
    /// there would copy an episode, several kilobytes, several times).
    Over(&'a mut Env<EnvSlot>),
    /// A new one.
    New(&'a mut Option<Env<EnvSlot>>),
}

/// Battle `i`'s episode number `started` (its first is 0): a series' next
/// game with `carry`, put in `place`. On an error `place` is left as it
/// was.
fn episode(job: &Job<'_>, i: usize, started: u64, carry: Option<Carry>, place: Place<'_>) -> Result<(), String> {
    let ctx = job.ctx;
    let config = job.config;
    let episode = i as u64 + started * config.n as u64;
    #[cfg(test)]
    if ctx.fail_start.is_some_and(|f| f(episode)) {
        return Err(format!("episode {episode}: can't start (test)"));
    }
    let seed = episode_seed(job.base_seed, episode);
    // The episode's own draws, always all of them in this order, so an
    // option doesn't shift the others.
    let mut meta = BattleRng::new(Seed::fast(seed ^ META_SEED));
    let swap = meta.random(2) == 1 && ctx.randomize_sides;
    let opponent = job.opponents[i].pick(meta.next_u32());
    // A series plays with open sheets.
    let ots = f64::from(meta.next_u32()) < ctx.ots * 4_294_967_296.0 || ctx.series.is_some();
    let (sheets, agent_side, opponent, series) = match carry {
        Some(c) => (c.sheets, c.agent_side, c.opponent, Some(c.series)),
        None => {
            let by_player = config.teams.teams(episode, seed).map_err(|e| format!("episode {episode}: {e}"))?;
            let sheets = if swap { Arc::new([by_player[1].clone(), by_player[0].clone()]) } else { by_player };
            let agent_side = if swap { SideId::P2 } else { SideId::P1 };
            (sheets, agent_side, opponent, ctx.series.map(|b| Box::new(Series::new(b))))
        }
    };
    let battle = new_battle(config, &sheets, seed).map_err(|e| format!("episode {episode}: {e}"))?;
    // The sheets player 0 sees, by side.
    let open = ots.then(|| [&sheets[0], &sheets[1]]);
    let closed = ots && ctx.ots_view != OtsView::Both;
    let start = Start { opponent, agent_side, series };
    let slot = |battle: &mut Battle| EnvSlot::new(battle, ctx, open, closed, &sheets, seed, start);
    match place {
        Place::Over(env) => {
            env.restart(battle, sheets.clone(), episode, started + 1, seed, slot);
        }
        Place::New(new) => *new = Some(Env::with_battle(battle, sheets.clone(), episode, started + 1, seed, slot)),
    }
    Ok(())
}

/// Whether the caller must answer `side`'s request (rather than the
/// environment's team preview mode).
fn caller_asked(ctx: &Ctx, battle: &Battle, side: SideId) -> bool {
    match battle.request(side) {
        Request::Wait => false,
        Request::TeamPreview { .. } => ctx.preview == PreviewMode::Learned,
        Request::Move | Request::ForceSwitch { .. } => true,
    }
}

/// The actions `side`'s request shows it, restricted to the format's gimmicks.
/// A trap or disable stays hidden until a rejected choice reveals it, or,
/// for a trap, the public view shows the foe's trapping ability
/// ([`revealed_traps`](crate::session::revealed_traps)), as
/// [`ChannelSession`](crate::session::ChannelSession) masks it.
///
/// `own` is the side's overlay and `tracker` what it sees of the battle.
/// The environment's masks are this; [`eval::play`](crate::eval::play) and
/// the conformance harness call it to mask alike.
pub fn shown(battle: &Battle, own: &OwnOverlay, tracker: &PublicTracker, side: SideId) -> LegalActions {
    if needs_shown(battle, own) {
        let revealed = crate::session::shown_revealed(battle.format(), own, tracker, side);
        format_actions(battle, side, Some(&revealed))
    } else {
        *battle.legal_actions(side)
    }
}

/// Whether a request may hide a trap or a disable (`maybeTrapped`,
/// `maybeDisabled` on an active Pokémon of the side's overlay `own`):
/// otherwise what it shows is the engine's legal actions.
#[inline]
pub fn hides(own: &OwnOverlay) -> bool {
    own.active.iter().flatten().any(|a| a.maybe_trapped || a.maybe_disabled)
}

/// The cached actions need the request's restrictions, including gimmicks
/// outside the format's fixed action space. Ordinary teams keep the borrow.
fn needs_shown(battle: &Battle, own: &OwnOverlay) -> bool {
    own.active.iter().flatten().any(|a| {
        a.maybe_trapped || a.maybe_disabled || a.can_mega_evo && !battle.format().gimmicks.contains(Gimmick::MegaEvolve)
    })
}

/// [`shown`] for `side` of a slot: the battle's own legal actions
/// borrowed, unless the request hides something ([`hides`]; then the
/// actions it shows, owned).
fn slot_shown<'a>(battle: &'a Battle, slot: &EnvSlot, side: SideId) -> Cow<'a, LegalActions> {
    let own = &slot.own[side.index()];
    if needs_shown(battle, own) {
        let revealed = crate::session::shown_revealed(battle.format(), own, slot.tracker_of(side), side);
        Cow::Owned(format_actions(battle, side, Some(&revealed)))
    } else {
        Cow::Borrowed(battle.legal_actions(side))
    }
}

/// The shown actions [`submit`] checks a choice against: owned, as
/// [`slot_shown`] gives them when the request hides something (the battle
/// checks a choice from its own legal actions itself).
fn hidden_shown(legal: Cow<'_, LegalActions>) -> Option<LegalActions> {
    match legal {
        Cow::Owned(l) => Some(l),
        Cow::Borrowed(_) => None,
    }
}

/// What submitting `choice` for `side` did.
enum Submitted {
    Ok,
    /// Refused because of what the request hides: the side learnt it.
    Unavailable,
    /// Refused outright, and why (the shown actions' or the battle's
    /// error).
    Rejected(ChoiceError),
}

/// Submits `choice` for `side`: records it, marks a resolution in
/// `dirty`, and on an unavailable choice updates the side's view as
/// Showdown updates its request. `legal`: the shown set the choice is
/// from, when the request hides something ([`hidden_shown`]).
fn submit(
    env: &mut Env<EnvSlot>,
    side: SideId,
    legal: Option<&LegalActions>,
    choice: Choice,
    dirty: &mut bool,
) -> Submitted {
    // Unless the request hides something, what it shows is what the battle
    // accepts: the battle's own check is enough.
    let hidden = hides(&env.slot.own[side.index()]);
    debug_assert!(!hidden || legal.is_some(), "a hidden request's shown actions");
    if hidden
        && let Some(legal) = legal
        && let Err(e) = legal.check(&choice)
    {
        return Submitted::Rejected(e);
    }
    match env.battle.submit(side, choice) {
        Ok(progress) => {
            env.decisions += 1;
            if let Some(log) = &mut env.slot.log {
                log.push((side, choice));
            }
            match progress {
                Progress::Resolved => *dirty = true,
                Progress::Waiting => env.slot.own[side.index()].answered = true,
            }
            Submitted::Ok
        }
        Err(e @ ChoiceError::IllegalAction { slot }) if hidden => {
            let Choice::Actions(actions) = choice else { return Submitted::Rejected(e) };
            crate::eval::reveal_unavailable(
                &mut env.slot.own[side.index()],
                &env.battle,
                slot as usize,
                actions[slot as usize],
            );
            Submitted::Unavailable
        }
        Err(e) => Submitted::Rejected(e),
    }
}

/// Player `p`'s caller actions: returns their status.
fn act(ctx: &Ctx, env: &mut Env<EnvSlot>, p: usize, acts: &[i64], dirty: &mut bool) -> i8 {
    let side = env.slot.side_of(p);
    let legal = slot_shown(&env.battle, &env.slot, side);
    let choice = if acts[0] == RANDOM_ACTION {
        Some(legal.sample(&mut env.slot.rng[p]))
    } else {
        ctx.space.decode_player(ctx.layout, ctx.switch_by, &legal, &env.slot.own[side.index()], acts)
    };
    let Some(choice) = choice else { return status::REJECTED };
    match submit(env, side, hidden_shown(legal).as_ref(), choice, dirty) {
        Submitted::Ok => status::SUBMITTED,
        Submitted::Unavailable => status::UNAVAILABLE,
        Submitted::Rejected(_) => status::REJECTED,
    }
}

/// A built-in choice for player `p` (its policy, or the team preview
/// mode): the policy chooses from what its request shows, and chooses again
/// if that was unavailable (after [`POLICY_TRIES`] unavailable choices, a
/// random one of the engine's legal actions). An error if a policy's choice
/// is refused outright (not one the request shows), which ends the
/// battle's episode ([`EnvEndReason::Error`]); a refused choice that no
/// policy made (the preview mode's, a random one for the caller's player)
/// is replaced by a random one of the engine's legal actions.
fn auto(ctx: &Ctx, env: &mut Env<EnvSlot>, p: usize, dirty: &mut bool) -> Result<(), String> {
    let side = env.slot.side_of(p);
    let by_policy = !env.slot.external(p);
    for _ in 0..POLICY_TRIES {
        let legal = slot_shown(&env.battle, &env.slot, side);
        let slot = &mut env.slot;
        let choice = if !by_policy {
            // A caller's player is only answered for at team preview, by
            // the preview mode.
            match (legal.request(), ctx.preview) {
                (Request::TeamPreview { bring }, PreviewMode::Fixed) => {
                    let team = env.battle.state().side(side).team.len() as u8;
                    let mut order = pikapro::TeamOrder::new();
                    (0..bring.min(team)).for_each(|i| order.push(i));
                    Choice::Team(order)
                }
                _ => legal.sample(&mut slot.rng[p]),
            }
        } else {
            let Opponent::Policy(policy) = &slot.opponent else { unreachable!("a built-in player has a policy") };
            let tracker = of_side(&slot.tracker, slot.closed.as_deref(), slot.agent_side, side);
            let view = player_view(tracker, &slot.own, &slot.team, slot.series.as_deref(), slot.agent_side, side);
            policy.choose(&view, &legal, &mut slot.rng[p])
        };
        match submit(env, side, hidden_shown(legal).as_ref(), choice, dirty) {
            Submitted::Ok => return Ok(()),
            Submitted::Unavailable => continue,
            // As `eval::play`: a policy's choice that its request doesn't
            // show is the policy's error, not a move to replace.
            Submitted::Rejected(e) if by_policy => {
                return Err(format!("turn {}: {side:?}'s choice {choice:?} was rejected: {e}", env.battle.turn()));
            }
            // The preview mode's or a random choice for the caller's player:
            // the engine's own legal actions below.
            Submitted::Rejected(_) => break,
        }
    }
    // A policy that keeps choosing what can't be played: the engine's own
    // legal actions never fail.
    let legal = format_actions(&env.battle, side, None);
    let choice = legal.sample(&mut env.slot.rng[p]);
    match submit(env, side, Some(&legal), choice, dirty) {
        Submitted::Ok => Ok(()),
        _ => Err(format!("the battle refused {side:?}'s choice {choice:?} from its own legal actions")),
    }
}

/// After a resolution: the new events into the tracker, both overlays
/// anew, and the reward shaping.
fn sync(ctx: &Ctx, env: &mut Env<EnvSlot>, prof: &mut Profile, clock: &mut Clock) {
    let slot = &mut env.slot;
    // Read in place, then dropped (a drain would copy each event out first).
    for e in env.battle.events() {
        slot.tracker.apply(e);
        if let Some(c) = &mut slot.closed {
            c.apply(e);
        }
    }
    env.battle.drain_events();
    prof.view += clock.lap();
    for side in SideId::BOTH {
        slot.own[side.index()].refresh(&env.battle, side);
        slot.team[side.index()].observe(&slot.own[side.index()]);
    }
    prof.overlay += clock.lap();
    // Only the shaping of the `ko` preset reads the Pokémon standing (the
    // terminal preset's is 0).
    if ctx.reward == RewardPreset::Terminal {
        return;
    }
    if let Some(now) = standing(&env.battle) {
        if let Some(before) = slot.standing {
            for p in 0..2 {
                let side = slot.side_of(p);
                slot.pending[p] += ctx.reward.shaping(side, before, now);
            }
        }
        slot.standing = Some(now);
    }
}

/// Rounds of built-in choices [`advance`] plays without a resolution
/// before it gives up. A round in which every player that must choose has
/// chosen resolves; one without a resolution leaves a caller to act. More
/// than a few mean the battle took choices and resolved nothing: the call
/// would never return.
const IDLE_ROUNDS: u32 = 8;

/// Plays the environment's own choices (built-in policies, automatic team
/// preview) until a caller must act or the episode ends; `dirty` says a
/// resolution happened since the last [`sync`]. An error if the battle
/// stops making progress ([`IDLE_ROUNDS`]).
fn advance(
    ctx: &Ctx,
    env: &mut Env<EnvSlot>,
    mut dirty: bool,
    prof: &mut Profile,
    clock: &mut Clock,
) -> Result<(), String> {
    let mut idle = 0;
    loop {
        if dirty {
            prof.engine += clock.lap();
            sync(ctx, env, prof, clock);
            dirty = false;
            idle = 0;
        } else if idle == IDLE_ROUNDS {
            let turn = env.battle.turn();
            return Err(format!(
                "the battle made no progress in {IDLE_ROUNDS} rounds of built-in choices (turn {turn})"
            ));
        } else {
            idle += 1;
        }
        if env.battle.is_over() || ctx.max_turns.is_some_and(|m| env.battle.turn() > m) {
            break;
        }
        let mut acted = false;
        let mut waiting = false;
        for p in 0..2 {
            let side = env.slot.side_of(p);
            if env.battle.request(side) == Request::Wait {
                continue;
            }
            if env.slot.external(p) && caller_asked(ctx, &env.battle, side) {
                waiting = true;
            } else {
                auto(ctx, env, p, &mut dirty)?;
                acted = true;
            }
        }
        if !dirty && (waiting || !acted) {
            break;
        }
    }
    prof.engine += clock.lap();
    Ok(())
}

/// Writes battle `j`'s observations and masks (into the final buffers with
/// `terminal`); returns which players must act. A built-in opponent's
/// observation rows (unless it is observed) are zeroed when an episode
/// starts (`fresh`), and in buffers that don't keep what the last step
/// wrote (`!kept`); not written otherwise.
#[allow(clippy::too_many_arguments)]
fn observe(
    ctx: &Ctx,
    env: &Env<EnvSlot>,
    j: usize,
    buf: &mut StepOut<'_>,
    kept: bool,
    terminal: bool,
    fresh: bool,
    prof: &mut Profile,
    clock: &mut Clock,
) -> [bool; 2] {
    let slot = &env.slot;
    let size = ctx.shape.obs;
    let tables = &ctx.tables;
    for p in 0..2 {
        let side = slot.side_of(p);
        let k = 2 * j + p;
        let observed = slot.external(p) || ctx.observe_builtin;
        if !observed && !fresh && !terminal && kept {
            continue;
        }
        // The terminal observation goes to the final buffers; the main
        // ones keep what they hold.
        let f32s = match (terminal, buf.final_out.as_mut()) {
            (true, Some(f)) => &mut *f.obs,
            _ => &mut *buf.obs,
        };
        let row = &mut f32s[k * size..][..size];
        if observed {
            ctx.observation.encode(&slot.view(side), tables, row);
        } else {
            row.fill(0.0);
        }
    }
    prof.encode += clock.lap();
    if terminal {
        return [false; 2];
    }
    let over = slot.ended.is_over();
    let width = ctx.shape.mask;
    let mut needs = [false; 2];
    for (p, need) in needs.iter_mut().enumerate() {
        let side = slot.side_of(p);
        let row = &mut buf.masks[(2 * j + p) * width..][..width];
        let mut min = 0;
        *need = !over && slot.external(p) && caller_asked(ctx, &env.battle, side);
        if !*need {
            row.fill(false);
        } else {
            let legal = slot_shown(&env.battle, slot, side);
            min = ctx.space.fill_player_mask(ctx.layout, ctx.switch_by, &legal, &slot.own[side.index()], row);
        }
        buf.needs_action[2 * j + p] = *need;
        buf.min_switches[2 * j + p] = min;
    }
    prof.masks += clock.lap();
    needs
}
