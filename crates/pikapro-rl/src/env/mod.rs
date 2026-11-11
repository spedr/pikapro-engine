//! [`VecEnv`]: the vector environment. `n` battles stepped together on
//! worker threads, one request round per [`VecEnv::step`], with one pool
//! dispatch per call.
//!
//! Each call, for each battle, on a worker thread:
//!
//! 1. submits the caller's actions, strictly: an action outside the mask is
//!    rejected (status −1) and nothing is submitted; one the mask allows
//!    but the battle refuses for what the request hides (a trap by a foe's
//!    unseen ability, a move only Imprison disables) is *unavailable*
//!    (status 2): the player's view and mask learn it, as Showdown's
//!    `[Unavailable choice]` updates the request, and it chooses again;
//! 2. plays the environment's own choices (a built-in opponent on its
//!    [`PlayerView`](pikapro_view::PlayerView), an automatic team
//!    preview) until a caller-controlled player must act or the episode
//!    ends;
//! 3. after each resolution, drains the new events into the battle's
//!    [`PublicTracker`](pikapro_view::PublicTracker), rebuilds both sides'
//!    [`OwnOverlay`](pikapro_view::OwnOverlay)s and accrues rewards;
//! 4. writes both players' observations (an
//!    [`Encoder`](crate::encode::Encoder)'s vector), their masks (as
//!    their requests show them), rewards, termination and who must act.
//!
//! Where the outputs go ([`StepOut`], the caller's buffers):
//!
//! - With one thread, the calling thread runs every battle and writes
//!   straight into them.
//! - With several, and many bytes per call
//!   (1 MB per thread), threads started for the call
//!   write straight into their shards' parts of them: starting threads
//!   costs less than one thread copying that much.
//! - Otherwise the pool's workers write into their shards' own buffers,
//!   and the calling thread copies each shard into the caller's buffers
//!   as soon as it is done, while the others still run.
//!
//! The outputs are the same either way. Everything is
//! deterministic and independent of the thread count; episode `e` (battle
//! `e % n`) plays with battle seed [`episode_seed`]`(base, e)`, and [`VecEnv::record`] gives
//! what a plain [`Battle`](pikapro::Battle) needs to replay it.

mod battle;
#[cfg(test)]
mod failure_tests;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pikapro::{Choice, Format, SideId};
use pikapro_protocol::TeamSheet;
use pikapro_view::{HpReport, ViewConfig};

pub use crate::pool::{BattleError, MAX_KEPT_ERRORS, episode_seed};
use battle::ShardOut;
pub use battle::{EnvSlot, hides, shown};

use crate::pool::{Env, Pool, PoolConfig, lock};
use crate::reward::RewardPreset;
use crate::space::ActionSpace;
use crate::tables::Tables;
use crate::vec::{
    Autoreset, MaskLayout, Observation, Opponent, PreviewMode, StepBuffers, StepOut, StepShape, SwitchBy, Teams,
};

/// Who plays player 1 in a battle's episodes: one opponent, or a mixture
/// drawn per episode from the episode's seed.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum OpponentSpec {
    One(Opponent),
    /// Each episode draws one, with probability proportional to its
    /// weight (weights are finite, non-negative, not all zero; any
    /// magnitude: they are scaled before they are summed).
    Mix(Arc<[(Opponent, f64)]>),
}

impl OpponentSpec {
    /// The opponent for an episode whose draw is `draw` (uniform over
    /// `u32`).
    fn pick(&self, draw: u32) -> Opponent {
        match self {
            OpponentSpec::One(o) => o.clone(),
            OpponentSpec::Mix(mix) => {
                // Scaled by a power of two, which is exact: the same picks
                // as the weights themselves, without their sum overflowing.
                let scale = weight_scale(mix);
                let total: f64 = mix.iter().map(|(_, w)| w / scale).sum();
                let mut x = f64::from(draw) / 4_294_967_296.0 * total;
                for (o, w) in mix.iter() {
                    let w = w / scale;
                    if x < w {
                        return o.clone();
                    }
                    x -= w;
                }
                mix.iter().rev().find(|(_, w)| *w > 0.0).map_or_else(Opponent::random, |(o, _)| o.clone())
            }
        }
    }

    fn check(&self) -> Result<(), String> {
        match self {
            OpponentSpec::One(_) => Ok(()),
            OpponentSpec::Mix(mix) => {
                let bad = mix.iter().any(|(_, w)| !(w.is_finite() && *w >= 0.0));
                if bad || !mix.iter().any(|(_, w)| *w > 0.0) {
                    return Err("mixture weights must be finite, non-negative and not all zero".into());
                }
                Ok(())
            }
        }
    }
}

/// The power of two a mixture's weights are divided by before they are
/// summed: the largest one at most the largest weight (1 when every weight
/// is below 1), so each scaled weight is below 2 and the sum is finite.
fn weight_scale(mix: &[(Opponent, f64)]) -> f64 {
    let max = mix.iter().map(|(_, w)| *w).fold(0.0, f64::max);
    if max.is_finite() && max >= 1.0 {
        // `max`'s exponent alone: 2^floor(log2(max)).
        f64::from_bits(max.to_bits() & (0x7ff << 52))
    } else {
        1.0
    }
}

impl From<Opponent> for OpponentSpec {
    fn from(o: Opponent) -> OpponentSpec {
        OpponentSpec::One(o)
    }
}

/// Who sees the sheets of an episode played with Open Team Sheets
/// ([`EnvConfig::ots_view`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OtsView {
    /// Both players, as Showdown shows them (the default).
    #[default]
    Both,
    /// Player 0 alone: it sees both sheets, as in a game with open sheets,
    /// while player 1 plays a game with closed sheets: its view (its
    /// observations, its masks, a built-in policy's choices) is, event for
    /// event, that of the same game with closed sheets. What a sheet is
    /// worth to a policy (one trained with closed sheets, say).
    Player0,
}

/// A vector environment's configuration. [`EnvConfig::new`] has the
/// defaults; set the fields to change them.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct EnvConfig {
    pub format: Format,
    /// Battles.
    pub n: usize,
    /// The base seed: episode `e` plays with battle seed
    /// [`episode_seed`]`(seed, e)`.
    pub seed: u64,
    /// Where each episode's teams come from, by player.
    pub teams: Teams,
    /// Who plays player 1 in every battle, until
    /// [`VecEnv::set_opponent`] changes it. Random by default.
    pub opponent: OpponentSpec,
    /// What writes each player's observation:
    /// [`Basic`](crate::encode::Basic) (the default) or an encoder of
    /// your own.
    pub observation: Observation,
    /// The reward preset: terminal (the default) or `ko`.
    pub reward: RewardPreset,
    /// When a battle that ended starts its next episode: at the next step
    /// (the default) or in the same one.
    pub autoreset: Autoreset,
    /// Factored (default) or joint masks, and the matching actions.
    pub masks: MaskLayout,
    /// Switch actions name observation rows (default) or team positions.
    pub switch_by: SwitchBy,
    /// How caller-controlled players answer team preview.
    pub preview: PreviewMode,
    /// Cut an episode (`truncated`, end reason 4) once a turn beyond this
    /// one starts.
    pub max_turns: Option<u16>,
    /// Foe HP as percentages (default, the ladder) or exact.
    pub hp: HpReport,
    /// The probability that an episode is played with Open Team Sheets (0:
    /// closed, the default; 1: always open). Only in a format with team
    /// preview, where Showdown shows the sheets.
    pub ots: f64,
    /// Who sees the sheets of an episode played with Open Team Sheets:
    /// both players (the default), or player 0 alone ([`OtsView`]). Only
    /// for single games (`series` is `None`).
    pub ots_view: OtsView,
    /// Player 0 plays p1 or p2 at random, per episode.
    pub randomize_sides: bool,
    /// Keep every episode's choices for [`VecEnv::record`] (costs an
    /// allocation per episode).
    pub record: bool,
    /// Group each battle's episodes into best-of-`n` series (Bo3: `3`),
    /// with open sheets ([`crate::series`]), in a format with team
    /// preview; `None` (the default): single games.
    pub series: Option<u8>,
    /// Encode a built-in opponent's view too (its observation rows are
    /// zeros otherwise: encoding costs time, and only an external
    /// opponent's actions come from the caller).
    pub observe_builtin: bool,
    /// How long an idle worker of the pool polls for the next call before
    /// it sleeps (a call that writes straight into the caller's buffers runs
    /// on threads started for it, and the workers wait).
    pub spin: Duration,
    /// Shards per thread (with more than one thread): more shards hold up
    /// a call less when a thread falls behind, fewer cost less to hand out.
    /// At most one shard per battle (a larger value is clamped); 0 counts as
    /// 1. [`SHARDS_PER_THREAD`](crate::pool::SHARDS_PER_THREAD) by default.
    pub shards_per_thread: usize,
    /// With more than one thread: the bytes per thread a call must write
    /// ([`StepOut::bytes_per_battle`] times `n`, over the threads) for the
    /// call's shards to run on threads started for it, which write
    /// straight into the caller's buffers, rather than on the pool's
    /// workers, whose outputs the calling thread then copies there (a few
    /// milliseconds per 10 MB, while starting 16 threads costs about a
    /// millisecond). `None`: always the workers. With one thread the calling
    /// thread always writes straight into the caller's buffers. 1 MB
    /// (2^20 bytes) by default. For the benchmarks.
    #[doc(hidden)]
    pub direct_bytes_per_thread: Option<usize>,
}

impl EnvConfig {
    /// The defaults: a random opponent, the `basic` observation, terminal rewards, `NEXT_STEP`
    /// autoreset, factored masks, switches by row, learned team preview,
    /// percentages, closed sheets, fixed sides.
    pub fn new(format: Format, n: usize, teams: Teams) -> EnvConfig {
        EnvConfig {
            format,
            n,
            seed: 0,
            teams,
            opponent: OpponentSpec::One(Opponent::random()),
            observation: Observation::default(),
            reward: RewardPreset::Terminal,
            autoreset: Autoreset::NextStep,
            masks: MaskLayout::Factored,
            switch_by: SwitchBy::Row,
            preview: PreviewMode::Learned,
            max_turns: None,
            hp: HpReport::Percent,
            ots: 0.0,
            ots_view: OtsView::Both,
            randomize_sides: false,
            record: false,
            observe_builtin: false,
            series: None,
            spin: Duration::from_micros(50),
            shards_per_thread: crate::pool::SHARDS_PER_THREAD,
            direct_bytes_per_thread: Some(DIRECT_BYTES_PER_THREAD),
        }
    }
}

/// What the workers share for the environment's lifetime.
pub(crate) struct Ctx {
    space: Arc<ActionSpace>,
    observation: Observation,
    tables: Tables,
    view: ViewConfig,
    reward: RewardPreset,
    autoreset: Autoreset,
    layout: MaskLayout,
    switch_by: SwitchBy,
    preview: PreviewMode,
    max_turns: Option<u16>,
    ots: f64,
    ots_view: OtsView,
    randomize_sides: bool,
    record: bool,
    observe_builtin: bool,
    series: Option<u8>,
    shape: StepShape,
    /// Action values per player.
    per: usize,
    /// Tests: the episodes that fail to start, with an error.
    #[cfg(test)]
    fail_start: Option<fn(u64) -> bool>,
}

/// The trackers' configuration for `config`.
fn view_config(config: &EnvConfig) -> ViewConfig {
    ViewConfig::new(&config.format).with_hp(config.hp)
}

/// Where a call's time went, in nanoseconds (the benchmarks' per-phase
/// clock). The per-phase times are summed over every
/// battle and thread (work); `wall` is the call's elapsed time on the
/// calling thread. With `t` threads, the time lost to dispatching,
/// waiting and copying is about `wall - work / t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Profile {
    /// Submitting (the caller's and built-in choices) and resolving.
    pub engine: u64,
    /// Applying events to the trackers.
    pub view: u64,
    /// Building the overlays.
    pub overlay: u64,
    /// Encoding the observations.
    pub encode: u64,
    /// Filling the masks.
    pub masks: u64,
    /// Episode starts, termination, rewards.
    pub other: u64,
    /// The call's elapsed time on the calling thread.
    pub wall: u64,
    /// The calling thread copying finished shards into the caller's
    /// buffers (part of `wall`).
    pub copy: u64,
    /// The calling thread waiting for the workers' last shards once none
    /// was left to claim (part of `wall`).
    pub wait: u64,
    /// Battle steps (battles times calls).
    pub steps: u64,
    /// Caller choices submitted.
    pub decisions: u64,
    /// Episodes ended.
    pub battles: u64,
}

impl Profile {
    /// Every battle's phases summed.
    #[must_use]
    pub fn work(&self) -> u64 {
        self.engine + self.view + self.overlay + self.encode + self.masks + self.other
    }

    /// Adds `o`'s times and counts to these.
    pub fn add(&mut self, o: &Profile) {
        self.engine += o.engine;
        self.view += o.view;
        self.overlay += o.overlay;
        self.encode += o.encode;
        self.masks += o.masks;
        self.other += o.other;
        self.wall += o.wall;
        self.copy += o.copy;
        self.wait += o.wait;
        self.steps += o.steps;
        self.decisions += o.decisions;
        self.battles += o.battles;
    }
}

/// What a plain [`Battle`](pikapro::Battle) needs to replay an episode:
/// `Battle::new(format, [sheets[0].team, sheets[1].team],
/// BattleOptions::new(Seed::fast(seed)))`, then `choices` submitted in
/// order.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct EpisodeRecord {
    pub episode: u64,
    /// Its battle seed.
    pub seed: u64,
    /// By side (p1's, p2's).
    pub sheets: Arc<[TeamSheet; 2]>,
    /// The side player 0 played.
    pub agent_side: SideId,
    /// Every choice submitted, by side, in order.
    pub choices: Vec<(SideId, Choice)>,
}

/// `EnvConfig::direct_bytes_per_thread` by default.
pub(crate) const DIRECT_BYTES_PER_THREAD: usize = 1 << 20;

/// The vector environment (see the module docs).
pub struct VecEnv {
    pool: Pool<EnvSlot>,
    ctx: Arc<Ctx>,
    config: EnvConfig,
    /// Each battle's opponent, from its next episode. Shared with a call by
    /// cloning the `Arc`: the workers never wait on it.
    opponents: Mutex<Arc<Vec<OpponentSpec>>>,
    /// [`VecEnv::reset`] was called: [`VecEnv::step`] may run.
    ready: AtomicBool,
    profile: AtomicBool,
    last_profile: Mutex<Profile>,
    /// The battles that failed since the last [`VecEnv::take_errors`].
    errors: Mutex<Vec<BattleError>>,
    /// Each shard's first battle and number of battles.
    bounds: Vec<(usize, usize)>,
}

impl std::fmt::Debug for VecEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VecEnv").field("pool", &self.pool).field("shape", &self.ctx.shape).finish()
    }
}

impl VecEnv {
    /// `config.n` battles on `threads` threads. Call [`VecEnv::reset`]
    /// before the first step.
    pub fn new(config: EnvConfig, threads: usize) -> Result<VecEnv, String> {
        if !(0.0..=1.0).contains(&config.ots) {
            return Err(format!("ots is a probability, not {}", config.ots));
        }
        // Showdown offers open sheets at team preview only, and a sheet's
        // entries are matched to the Pokémon team preview shows.
        let preview = config.format.team_preview.shown();
        if config.ots > 0.0 && !preview {
            return Err(format!(
                "ots needs a format with team preview (where Showdown shows the sheets): {} has none",
                config.format.id
            ));
        }
        if let Some(b) = config.series {
            crate::series::check_best_of(b)?;
            if !preview {
                return Err(format!(
                    "a series is played with open team sheets, which need team preview: {} has none",
                    config.format.id
                ));
            }
            if config.ots_view != OtsView::Both {
                return Err("a series shows both players the sheets: ots_view is for single games".into());
            }
        }
        if config.max_turns == Some(0) {
            return Err("max_turns must be at least 1 (0 would end every episode before its first turn)".into());
        }
        if let RewardPreset::Ko { weight } = config.reward
            && !weight.is_finite()
        {
            return Err(format!("the ko reward's weight must be finite, not {weight}"));
        }
        config.opponent.check()?;
        let space = Arc::new(ActionSpace::new(&config.format));
        space.check_format(&config.format)?;
        let tables = Tables::for_format(&config.format);
        let shape = StepShape { n: config.n, obs: config.observation.size(), mask: config.masks.width(&space) };
        let ctx = Arc::new(Ctx {
            per: config.masks.actions(&space),
            space: space.clone(),
            observation: config.observation.clone(),
            tables,
            view: view_config(&config),
            reward: config.reward,
            autoreset: config.autoreset,
            layout: config.masks,
            switch_by: config.switch_by,
            preview: config.preview,
            max_turns: config.max_turns,
            ots: config.ots,
            ots_view: config.ots_view,
            randomize_sides: config.randomize_sides,
            record: config.record,
            observe_builtin: config.observe_builtin,
            series: config.series,
            shape,
            #[cfg(test)]
            fail_start: None,
        });
        let pool_config = PoolConfig {
            seed: config.seed,
            log: true,
            auto_reset: false,
            spin: config.spin,
            ..PoolConfig::new(config.format.clone(), config.n, config.teams.clone())
        };
        let opponents = Arc::new(vec![config.opponent.clone(); config.n]);
        // Each battle holds its first episode's battle, provisionally: the
        // first reset starts it again (`started` 0).
        let make = |pool: &PoolConfig, i: usize| {
            let job =
                battle::Job { ctx: &ctx, config: pool, base_seed: config.seed, opponents: &opponents, profile: false };
            battle::provisional(&job, i)
        };
        let pool = Pool::with_envs(pool_config, threads, config.shards_per_thread, make)?;
        Ok(VecEnv {
            bounds: pool.shard_bounds(),
            pool,
            opponents: Mutex::new(opponents),
            ctx,
            config,
            ready: AtomicBool::new(false),
            profile: AtomicBool::new(false),
            last_profile: Mutex::new(Profile::default()),
            errors: Mutex::new(Vec::new()),
        })
    }

    /// The buffers' shape.
    pub fn shape(&self) -> StepShape {
        self.ctx.shape
    }

    /// Action values per battle and player: `active` slot actions with
    /// factored masks, one choice index with joint masks.
    pub fn actions_per_player(&self) -> usize {
        self.ctx.per
    }

    /// The format's action space.
    pub fn space(&self) -> &ActionSpace {
        &self.ctx.space
    }

    /// The configuration it was made with (its `seed` is the first; a
    /// reset with a seed changes [`VecEnv::seed`]).
    pub fn config(&self) -> &EnvConfig {
        &self.config
    }

    /// The base seed of the episodes under way: episode `e` plays with
    /// battle seed [`episode_seed`]`(seed, e)`.
    pub fn seed(&self) -> u64 {
        self.pool.config().seed
    }

    /// The number of threads stepping the battles.
    pub fn threads(&self) -> usize {
        self.pool.threads()
    }

    /// Output buffers of the right shape for this environment (with the
    /// final ones for same-step autoreset, the series ones in series mode).
    pub fn buffers(&self) -> StepBuffers {
        StepBuffers::with(self.ctx.shape, self.ctx.autoreset == Autoreset::SameStep, self.ctx.series.is_some())
    }

    /// Turns the per-phase clock on or off (reading it costs time too).
    /// For the benchmarks.
    #[doc(hidden)]
    pub fn set_profile(&self, on: bool) {
        self.profile.store(on, Ordering::Relaxed);
    }

    /// The per-phase times since the last call to this (with
    /// `VecEnv::set_profile`). For the benchmarks.
    #[doc(hidden)]
    pub fn take_profile(&self) -> Profile {
        core::mem::take(&mut *lock(&self.last_profile))
    }

    /// The battles that failed since the last call to this, in order: a
    /// panic (an engine bug, a user [`Policy`](crate::policy::Policy) or
    /// [`Encoder`](crate::encode::Encoder)) or an error while stepping one
    /// ended its episode with
    /// [`EnvEndReason::Error`](crate::vec::EnvEndReason::Error) instead of
    /// failing the call. At most [`MAX_KEPT_ERRORS`] are kept.
    ///
    /// Catching a panic needs the unwinding panic strategy
    /// (`panic = "unwind"`, Rust's default and the Python wheels'); with
    /// `panic = "abort"` a panic ends the process.
    pub fn take_errors(&self) -> Vec<BattleError> {
        core::mem::take(&mut *lock(&self.errors))
    }

    /// Changes the opponent of the given battles (all when `None`), from
    /// each one's next episode on (in series mode, from its next series).
    pub fn set_opponent(&self, which: Option<&[usize]>, opponent: impl Into<OpponentSpec>) -> Result<(), String> {
        let opponent = opponent.into();
        opponent.check()?;
        if let Some(bad) = which.and_then(|w| w.iter().find(|&&i| i >= self.config.n)) {
            return Err(format!("battle {bad} is out of range"));
        }
        let mut guard = lock(&self.opponents);
        let mut all = (**guard).clone();
        match which {
            None => all.iter_mut().for_each(|o| *o = opponent.clone()),
            Some(which) => which.iter().for_each(|&i| all[i] = opponent.clone()),
        }
        *guard = Arc::new(all);
        Ok(())
    }

    /// Replaces entries of the team pair list ([`Teams::Pairs`]): each
    /// `(k, pair)` puts `pair` at position `k`, for the episodes started
    /// from now on (an episode under way keeps its teams; in series mode,
    /// for the series started from now on), so episode `e`
    /// still plays position `e % len` of the list. [`VecEnv::config`]
    /// keeps the list the environment was made with. An error (another
    /// team source, a position out of range, a team the format refuses)
    /// changes nothing.
    pub fn set_pairs(&self, updates: Vec<(usize, Arc<[TeamSheet; 2]>)>) -> Result<(), String> {
        self.pool.set_pairs(updates)
    }

    /// Starts a new episode in every battle and writes its first
    /// observations into `out` (status 0, no reward; a battle that fails
    /// to start ends with [`EnvEndReason::Error`](crate::vec::EnvEndReason::Error)
    /// and starts again at the next call). With `seed`, the
    /// episodes are numbered from 0 again with that base seed (episode `e`
    /// of battle `e % n` plays with battle seed `episode_seed(seed, e)`), so a reset
    /// with a seed is reproducible; without, each battle starts its next
    /// episode.
    pub fn reset(&self, seed: Option<u64>, out: &mut StepOut<'_>) -> Result<(), String> {
        if let Some(s) = seed {
            // Buffers it would refuse leave the seed alone.
            self.check(out)?;
            self.pool.set_seed(s);
        }
        self.run(None, seed.is_some(), out)?;
        self.ready.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// One request round of every battle. `actions` holds `n * 2 *
    /// actions_per_player()` values, by battle and player (see
    /// [`MaskLayout`]): the actions of players that aren't asked anything
    /// are ignored, and [`RANDOM_ACTION`](crate::vec::RANDOM_ACTION) plays
    /// a random allowed one.
    ///
    /// A battle that fails (a panic or an error while stepping it) doesn't
    /// fail the call: its episode ends with
    /// [`EnvEndReason::Error`](crate::vec::EnvEndReason::Error), the next
    /// call starts its next one, and [`VecEnv::take_errors`] says what went
    /// wrong. An error is for the call itself (its arguments, or a failure
    /// outside any battle).
    pub fn step(&self, actions: &[i64], out: &mut StepOut<'_>) -> Result<(), String> {
        if !self.ready.load(Ordering::Relaxed) {
            return Err("call reset before the first step".into());
        }
        let per = 2 * self.ctx.per;
        if actions.len() != self.config.n * per {
            return Err(format!("expected {} actions, got {}", self.config.n * per, actions.len()));
        }
        self.run(Some(actions.into()), false, out)
    }

    /// Whether `out` fits this environment's calls.
    fn check(&self, out: &StepOut<'_>) -> Result<(), String> {
        out.check(self.ctx.shape)?;
        if self.ctx.autoreset == Autoreset::SameStep && out.final_out.is_none() {
            return Err("same-step autoreset needs the final buffers".into());
        }
        let series = self.ctx.series.is_some();
        if series != out.series.is_some() || series && out.final_out.as_ref().is_some_and(|f| f.series.is_none()) {
            return Err("the series buffers are for series mode (and only for it)".into());
        }
        Ok(())
    }

    /// One call on every battle: a step with `actions`, or a reset.
    fn run(&self, actions: Option<Arc<[i64]>>, rebase: bool, out: &mut StepOut<'_>) -> Result<(), String> {
        let shape = self.ctx.shape;
        self.check(out)?;
        let start = Instant::now();
        let profile = self.profile.load(Ordering::Relaxed);
        let opponents = lock(&self.opponents).clone();
        let ctx = self.ctx.clone();
        let per = 2 * ctx.per;
        // Runs a shard's battles into `target` (the shard's own buffers, or
        // its part of `out`).
        let shard =
            move |start: usize, envs: &mut [Env<EnvSlot>], target: &mut battle::Target<'_, '_>, config: &PoolConfig| {
                // The pool's seed: the last reset's.
                let job = battle::Job { ctx: &ctx, config, base_seed: config.seed, opponents: &opponents, profile };
                *target.profile = Profile::default();
                target.errors.clear();
                for (j, env) in envs.iter_mut().enumerate() {
                    let i = start + j;
                    if rebase {
                        env.started = 0;
                    }
                    let acts = actions.as_ref().map(|a| &a[i * per..][..per]);
                    battle::run_guarded(&job, env, i, j, acts, target);
                }
                Ok(())
            };
        let mut total = Profile::default();
        let mut errors = Vec::new();
        let result = if self.direct() {
            // Every shard writes into its part of `out`: on the
            // calling thread alone, or on scoped threads that can borrow
            // `out` (a call writes too much for one thread to copy it).
            let mut rest = out.reborrow();
            let mut parts = Vec::with_capacity(self.bounds.len());
            for &(_, len) in &self.bounds {
                let (part, r) = rest.split_battles(len, shape);
                parts.push(part);
                rest = r;
            }
            let job = |start: usize,
                       envs: &mut [Env<EnvSlot>],
                       ext: &mut ShardOut,
                       config: &PoolConfig,
                       part: &mut StepOut<'_>| {
                if ext.ended.len() != envs.len() {
                    ext.ended = vec![false; envs.len()];
                }
                let ShardOut { ended, profile, errors, .. } = ext;
                let target = &mut battle::Target { out: part, kept: false, ended, profile, errors };
                shard(start, envs, target, config)
            };
            // Collected within the call: a concurrent call can't clear the
            // shards' errors first.
            let result = self.pool.run_parts(parts, job, |ext| {
                total.add(&ext.profile);
                errors.extend_from_slice(&ext.errors);
            });
            total.wait = self.pool.last_wait().as_nanos() as u64;
            result
        } else {
            let ctx = self.ctx.clone();
            let job = move |start: usize, envs: &mut [Env<EnvSlot>], ext: &mut ShardOut, config: &PoolConfig| {
                let size = StepShape { n: envs.len(), ..ctx.shape };
                if ext.buffers.shape != Some(size) {
                    ext.buffers = StepBuffers::with(size, ctx.autoreset == Autoreset::SameStep, ctx.series.is_some());
                    ext.ended = vec![false; envs.len()];
                }
                let ShardOut { buffers, ended, profile, errors } = ext;
                let target = &mut battle::Target { out: &mut buffers.as_out(), kept: true, ended, profile, errors };
                shard(start, envs, target, config)
            };
            let mut copy = 0;
            // The calling thread copies each shard as it finishes, between
            // the shards it runs itself.
            let result = self.pool.run_shards(job, |start, ext| {
                let t = profile.then(Instant::now);
                ext.buffers.copy_into(start, out, Some(&ext.ended));
                copy += t.map_or(0, |t| t.elapsed().as_nanos() as u64);
                total.add(&ext.profile);
                errors.extend_from_slice(&ext.errors);
            });
            total.copy = copy;
            total.wait = self.pool.last_wait().as_nanos() as u64;
            result
        };
        total.wall = start.elapsed().as_nanos() as u64;
        if profile {
            lock(&self.last_profile).add(&total);
        }
        crate::pool::keep_errors(&self.errors, errors);
        result
    }

    /// Whether the shards write straight into the caller's buffers
    /// (`EnvConfig::direct_bytes_per_thread`).
    fn direct(&self) -> bool {
        let threads = self.pool.threads();
        let bytes = StepOut::bytes_per_battle(self.ctx.shape) * self.config.n;
        threads == 1 || self.config.direct_bytes_per_thread.is_some_and(|b| bytes >= b * threads)
    }

    /// The record of battle `i`'s current episode (`previous`: the one
    /// before), with [`EnvConfig::record`].
    pub fn record(&self, i: usize, previous: bool) -> Option<Arc<EpisodeRecord>> {
        self.pool
            .with_env(i, |env| if previous { env.slot.previous.clone() } else { battle::current_record(env) })
            .flatten()
    }

    /// Runs `f` on battle `i` (between calls): its battle, sheets and
    /// views ([`EnvSlot::view`]), to read. `None` for a battle out of
    /// range.
    pub fn with_env<R>(&self, i: usize, f: impl FnOnce(&Env<EnvSlot>) -> R) -> Option<R> {
        self.pool.with_env(i, f)
    }
}
