//! [`Pool`]: many battles stepped together on worker threads.
//!
//! The battles are split into contiguous shards, several per thread. The
//! calling thread and plain `std` worker threads, living as long as the
//! pool, claim shards until none is left: each call sends every worker the
//! same job, each thread runs it on the shards it claims and writes the
//! results into the shard's buffers, and the caller copies each shard's
//! buffers into one output as soon as that shard is done, while the others
//! still run (a single copy per call, into memory the caller hands to
//! numpy, mostly hidden behind the other shards' work). A call can also run
//! on threads started for it, which may borrow the caller's memory and
//! write straight into it.
//!
//! Everything is deterministic and independent of the thread count: battle
//! `i`'s `k`-th episode is episode `e = i + k * n`, played with battle seed
//! `s = `[`episode_seed`]`(seed, e)` (as the vector environment's) and the
//! teams [`Teams::teams`] gives for `e`; the built-in random policy
//! draws from `!s`, as `pikapro bench` does for its battle seeds, so a plain
//! [`Battle`] with seed `s` replays an episode.
//!
//! # What the pool shows
//!
//! [`Pool::masks`] and [`Pool::sample_random`] give each side the actions
//! its request *shows* ([`pikapro::legal::shown_actions`]), as the vector
//! environment's masks do: a trap by a foe's unseen ability, or a move only
//! a foe's Imprison disables, stays allowed until a choice reveals it.
//! [`Pool::step`] then answers that choice as Showdown's `[Unavailable
//! choice]` does: status 2, nothing submitted, and the side's masks show
//! the trap or the disable from then on, until its next request.
//! [`Pool::state`] is the omniscient raw state (both sides, everything),
//! and [`Pool::play_random`] plays the engine's own legal actions, as `pikapro
//! bench` does.
//!
//! # Failures
//!
//! A battle that fails during a call (an error, such as an episode that
//! can't start, or a panic in the engine) ends its episode alone, as in the
//! vector environment: the call goes on with the other battles and returns
//! their results, and [`Pool::take_errors`] says what went wrong. The
//! failed battle shows nothing (no masks, no request, no random action,
//! an empty state) until its next episode starts: [`Pool::step`] reports
//! its end once ([`PoolStep::failed`]) and, with `auto_reset`, starts the
//! next one in the same step; otherwise [`Pool::reset`] does. An episode
//! that fails to start is skipped. Catching a panic needs the unwinding
//! panic strategy (`panic = "unwind"`, Rust's default); with
//! `panic = "abort"` a panic ends the process.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pikapro::legal::{Revealed, format_actions};
use pikapro::request::MoveChoices;
use pikapro::{
    Battle, BattleOptions, BattleRng, Choice, ChoiceError, Format, LegalActions, MAX_TEAM, Progress, Request, Seed,
    SideId, SlotAction, TeamError,
};
use pikapro_protocol::TeamSheet;

use crate::arrays::{self, FIELD_WIDTH, POKEMON_WIDTH, SIDE_WIDTH};
use crate::space::ActionSpace;
use crate::vec::{Teams, status};

/// A pool's configuration. [`PoolConfig::new`] has the defaults.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PoolConfig {
    pub format: Format,
    /// Where each episode's teams come from.
    pub teams: Teams,
    /// The base seed: episode `e` plays with battle seed
    /// [`episode_seed`]`(seed, e)`. [`Pool::set_seed`] changes it
    /// ([`Pool::config`] has the current one).
    pub seed: u64,
    /// Battles.
    pub n: usize,
    /// Record events (off for throughput).
    pub log: bool,
    /// Start the next episode as soon as a battle ends.
    pub auto_reset: bool,
    /// How long an idle worker polls for its next job before it sleeps. A
    /// step loop driven from Python calls again within microseconds, and a
    /// worker that slept wakes up slower, maybe on another core.
    pub spin: Duration,
}

impl PoolConfig {
    /// `n` battles of `format` playing `teams`: seed 0, no event log,
    /// `auto_reset`, workers polling 100 µs.
    pub fn new(format: Format, n: usize, teams: Teams) -> PoolConfig {
        PoolConfig { format, teams, seed: 0, n, log: false, auto_reset: true, spin: Duration::from_micros(100) }
    }

    pub(crate) fn battle_seed(&self, episode: u64) -> u64 {
        episode_seed(self.seed, episode)
    }
}

/// Episode `episode`'s battle seed under the base seed `base` (the pool's
/// and the vector environment's): a hash of the pair (splitmix64's output
/// at step `episode + 1` of the stream seeded with the mixed base). Bases
/// `s` and `s + 1`, which Gymnasium's and Stable-Baselines3's vector envs
/// give sub-envs `i` and `i + 1`, so play unrelated episodes instead of the
/// same ones shifted by one.
#[must_use]
pub fn episode_seed(base: u64, episode: u64) -> u64 {
    const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
    fn mix(mut z: u64) -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    mix(mix(base).wrapping_add(episode.wrapping_add(1).wrapping_mul(GAMMA)))
}

/// What a pool keeps with each battle: nothing (`()`) for a [`Pool`]; the
/// vector environment ([`crate::env`]) keeps player views, rewards and
/// episode settings here ([`EnvSlot`](crate::env::EnvSlot)). Sealed: only
/// this crate's payloads implement it.
pub trait Slot: sealed::Sealed + Clone + fmt::Debug + Send + 'static {
    /// A shard's output buffers for the crate's own jobs.
    #[doc(hidden)]
    type Out: Default + fmt::Debug + Send + 'static;

    /// The payload for an episode, when its battle has just been created.
    #[doc(hidden)]
    fn start(battle: &mut Battle) -> Self;
}

pub(crate) mod sealed {
    /// Keeps [`Slot`](super::Slot) to this crate's payloads.
    pub trait Sealed {}

    impl Sealed for () {}
}

impl Slot for () {
    type Out = ();

    fn start(_: &mut Battle) {}
}

/// One battle of the pool.
#[derive(Clone, Debug)]
pub struct Env<S: Slot = ()> {
    pub battle: Battle,
    pub sheets: Arc<[TeamSheet; 2]>,
    /// The episode being played (or, after a failed start, the one that
    /// failed).
    pub episode: u64,
    /// Its battle seed: a plain [`Battle`] with this seed and `sheets`
    /// replays it.
    pub(crate) seed: u64,
    /// Episodes this slot has started (or tried to).
    pub(crate) started: u64,
    /// The built-in random policy's RNG for this episode.
    pub(crate) policy: BattleRng,
    /// Choices submitted in this episode.
    pub(crate) decisions: u64,
    /// What choices refused as unavailable revealed about each side's
    /// current request (by side): the pool's masks show it from then on.
    /// Cleared when the battle resolves (a new request).
    pub(crate) revealed: [Revealed; 2],
    /// Set when the battle failed during a call (an error or a panic): its
    /// episode is over and its state unknown. The pool shows nothing of it
    /// until its next episode starts (see the module docs); the vector
    /// environment ends the episode
    /// ([`EnvEndReason::Error`](crate::vec::EnvEndReason::Error)) and
    /// starts the next at its next call.
    pub(crate) poisoned: Option<String>,
    /// The pool's [`Pool::step`] hasn't reported the failure yet.
    pub(crate) unreported: bool,
    /// The payload [`Slot::start`] made for this episode.
    pub slot: S,
}

impl<S: Slot> Env<S> {
    /// Battle `index`'s episode number `started` (its first is 0).
    fn start(config: &PoolConfig, index: usize, started: u64) -> Result<Env<S>, String> {
        let episode = index as u64 + started * config.n as u64;
        let seed = config.battle_seed(episode);
        let sheets = config.teams.teams(episode, seed).map_err(|e| format!("episode {episode}: {e}"))?;
        let battle = new_battle(config, &sheets, seed).map_err(|e| format!("episode {episode}: {e}"))?;
        Ok(Env::with_battle(battle, sheets, episode, started + 1, seed, |battle| S::start(battle)))
    }

    /// An episode's battle, made by the caller: `episode`, with battle seed
    /// `seed`, played on `sheets` (p1's, p2's); `started` counts the slot's
    /// episodes, this one included. `slot` makes the payload.
    pub(crate) fn with_battle(
        mut battle: Battle,
        sheets: Arc<[TeamSheet; 2]>,
        episode: u64,
        started: u64,
        seed: u64,
        slot: impl FnOnce(&mut Battle) -> S,
    ) -> Env<S> {
        let slot = slot(&mut battle);
        let policy = BattleRng::new(Seed::fast(!seed));
        Env {
            battle,
            slot,
            sheets,
            episode,
            started,
            policy,
            seed,
            decisions: 0,
            revealed: [Revealed::default(); 2],
            poisoned: None,
            unreported: false,
        }
    }

    /// [`Env::with_battle`] in place: every field is set as `with_battle`
    /// sets it (building one and moving it into place would copy an
    /// episode, several kilobytes, several times). The payload is made before
    /// anything is replaced, so a `slot` that panics leaves the old
    /// episode whole.
    pub(crate) fn restart(
        &mut self,
        mut new_battle: Battle,
        sheets: Arc<[TeamSheet; 2]>,
        episode: u64,
        started: u64,
        seed: u64,
        slot: impl FnOnce(&mut Battle) -> S,
    ) {
        // Every field, by name: a new one must say what a restart sets.
        let Env {
            battle,
            sheets: kept_sheets,
            episode: kept_episode,
            seed: kept_seed,
            started: kept_started,
            policy,
            decisions,
            revealed,
            poisoned,
            unreported,
            slot: payload,
        } = self;
        let new_payload = slot(&mut new_battle);
        *battle = new_battle;
        *payload = new_payload;
        *policy = BattleRng::new(Seed::fast(!seed));
        *kept_sheets = sheets;
        *kept_episode = episode;
        *kept_started = started;
        *kept_seed = seed;
        *decisions = 0;
        *revealed = [Revealed::default(); 2];
        *poisoned = None;
        *unreported = false;
    }

    /// The battle seed of the episode being played: a plain [`Battle`]
    /// with this seed and [`Env::sheets`](Env) replays it.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Starts this battle's next episode. On an error the battle is
    /// unchanged but for its episode number and seed, which name the
    /// episode that failed: it is counted, and skipped.
    pub(crate) fn reset(&mut self, config: &PoolConfig, index: usize) -> Result<(), String> {
        let started = self.started;
        self.started += 1;
        match Env::start(config, index, started) {
            Ok(env) => *self = env,
            Err(e) => {
                self.episode = index as u64 + started * config.n as u64;
                self.seed = config.battle_seed(self.episode);
                return Err(e);
            }
        }
        Ok(())
    }

    /// The actions `side`'s request shows it within the format's action space:
    /// [`format_actions`], less what refused choices revealed ([`Env::revealed`](Env)).
    pub fn shown(&self, side: SideId) -> LegalActions {
        format_actions(&self.battle, side, Some(&self.revealed[side.index()]))
    }

    /// Submits `choice` for `side` as the pool's step does:
    /// [`SUBMITTED`](status::SUBMITTED), [`UNAVAILABLE`](status::UNAVAILABLE)
    /// (the request showed it, but it hid a trap or a disable the refusal
    /// reveals: the side's masks show it from now on) or
    /// [`REJECTED`](status::REJECTED) (nothing changed).
    pub(crate) fn submit(&mut self, side: SideId, choice: Choice) -> i8 {
        match self.battle.submit(side, choice) {
            Ok(progress) => {
                self.decisions += 1;
                if progress == Progress::Resolved {
                    self.revealed = [Revealed::default(); 2];
                }
                status::SUBMITTED
            }
            Err(ChoiceError::IllegalAction { slot }) if self.shown(side).check(&choice).is_ok() => {
                let Choice::Actions(actions) = choice else { return status::REJECTED };
                reveal(&self.battle, side, slot as usize, actions[slot as usize], &mut self.revealed[side.index()]);
                status::UNAVAILABLE
            }
            Err(_) => status::REJECTED,
        }
    }

    /// Whether the battle failed (see the module docs): its episode is
    /// over, and nothing of it shows until its next one starts.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.is_some()
    }
}

/// What Showdown's update of a request after an `[Unavailable choice]`
/// shows (`emitChoiceError` with `updateRequestForPokemon`, or
/// `updateDisabledRequest`), as [`OwnOverlay::reject`](pikapro_view::OwnOverlay::reject)
/// learns it: `slot`'s refused `action` revealed a trap, or a disable (in
/// doubles, with `maybeDisabled`, every move the Pokémon can't use shows
/// disabled at once).
fn reveal(battle: &Battle, side: SideId, slot: usize, action: SlotAction, revealed: &mut Revealed) {
    match action {
        SlotAction::Switch(_) => revealed.trapped[slot] = true,
        SlotAction::Move { index, .. } => {
            let doubles = battle.format().active_per_side() > 1;
            if let Some(req) = battle.active_request(side, slot)
                && let (true, true, MoveChoices::Moves(moves)) = (doubles, req.maybe_disabled, req.moves)
            {
                let legal = battle.legal_actions(side).slot(slot);
                let usable = battle.state().side(side).active(slot).is_some_and(|m| m.has_usable_move());
                for k in 0..moves.len() {
                    let listed =
                        legal.iter().any(|a| matches!(a, SlotAction::Move { index, .. } if *index as usize == k));
                    if !(usable && listed) {
                        revealed.disabled[slot] |= 1 << k;
                    }
                }
            }
            revealed.disabled[slot] |= 1 << index;
        }
        SlotAction::Pass => {}
    }
}

/// A new battle for `sheets` (p1's, p2's) with battle seed `seed`, events
/// as the pool records them.
pub(crate) fn new_battle(config: &PoolConfig, sheets: &[TeamSheet; 2], seed: u64) -> Result<Battle, TeamError> {
    let mut options = BattleOptions::new(Seed::fast(seed));
    options.log_events = config.log;
    let teams = [sheets[0].team.clone(), sheets[1].team.clone()];
    Battle::new(config.format.clone(), teams, options)
}

/// A battle that failed during a call: its episode ended (the pool's
/// [`Pool::take_errors`], the vector environment's
/// [`VecEnv::take_errors`](crate::env::VecEnv::take_errors), where the
/// episode ended with [`EnvEndReason::Error`](crate::vec::EnvEndReason::Error)).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct BattleError {
    pub battle: usize,
    /// The episode that failed: the one under way, or the one that failed
    /// to start (which is skipped).
    pub episode: u64,
    /// The error, or the panic's message (`panic: ...`).
    pub message: String,
}

/// The most [`BattleError`]s a pool or a vector environment keeps for
/// `take_errors` (the outputs still say which battles failed).
pub const MAX_KEPT_ERRORS: usize = 1024;

/// Counts over finished battles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Totals {
    pub battles: u64,
    pub turns: u64,
    /// Choices submitted: in [`Pool::play_random`]'s totals, by the
    /// built-in random policy during the call; in [`Pool::finished`], in
    /// the finished battles.
    pub decisions: u64,
    pub wins: [u64; 2],
    pub ties: u64,
}

impl Totals {
    fn record(&mut self, battle: &Battle) {
        if let Some(r) = battle.result() {
            self.battles += 1;
            self.turns += r.turns as u64;
            match r.winner {
                Some(side) => self.wins[side.index()] += 1,
                None => self.ties += 1,
            }
        }
    }

    /// A finished battle, with its decisions.
    fn record_env<S: Slot>(&mut self, env: &Env<S>) {
        self.record(&env.battle);
        self.decisions += env.decisions;
    }

    fn add(&mut self, other: &Totals) {
        self.battles += other.battles;
        self.turns += other.turns;
        self.decisions += other.decisions;
        self.wins[0] += other.wins[0];
        self.wins[1] += other.wins[1];
        self.ties += other.ties;
    }
}

/// How [`Pool::step`] reads its actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionMode {
    /// One choice index per side (`n * 2` values).
    Choice,
    /// One slot-action index per active slot (`n * 2 * active` values); at
    /// team preview, slot 0 holds the preview order's index within
    /// `0..preview_size`.
    Slots,
}

/// Which mask [`Pool::masks`] fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaskKind {
    /// `n * 2 * size`: jointly legal choices.
    Choice,
    /// `n * 2 * active * slot_size`: each slot's own legal actions.
    Slots,
    /// `n * 2 * preview_size`: legal team-preview orders.
    Preview,
}

/// What [`Pool::step`] did, per battle. (The pool has no players: sides
/// are p1 and p2. The vector environment's outputs are
/// [`vec::StepOut`](crate::vec::StepOut), by player.)
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PoolStep {
    /// Per battle and side: `1` submitted, `0` nothing was asked of this
    /// side, `-1` rejected (out of range or illegal; nothing changed), `2`
    /// unavailable (the request showed it, but a trap or a disable it hid
    /// refused it: nothing submitted, and the side's masks now show why;
    /// see the module docs).
    pub status: Vec<i8>,
    /// Per battle: the battle's episode ended during this step (once per
    /// episode: a battle that was already over, without `auto_reset`, is
    /// not done again).
    pub done: Vec<bool>,
    /// Per battle: `-1` not over (or failed), `0`/`1` the winning side
    /// (p1, p2), `2` a tie.
    pub winner: Vec<i8>,
    /// Per battle: the episode that ended (`done`) failed, with no result
    /// (see the module docs; [`Pool::take_errors`] says why). Its statuses
    /// are 0.
    pub failed: Vec<bool>,
}

/// A shard's output buffers and counts.
#[derive(Debug, Default)]
struct Out {
    bools: Vec<bool>,
    i8s: Vec<i8>,
    i32s: Vec<i32>,
    i64s: Vec<i64>,
    totals: Totals,
    /// Every battle finished since the pool was made.
    finished: Totals,
}

/// A shard: some battles and the buffers their jobs write.
#[derive(Debug)]
struct Shard<S: Slot> {
    start: usize,
    envs: Vec<Env<S>>,
    out: Out,
    /// The buffers of [`Pool::run_shards`] jobs.
    ext: S::Out,
    /// What went wrong during the current call outside any battle: it
    /// fails the call.
    errors: Vec<String>,
    /// The battles that failed during the current call.
    failed: Vec<BattleError>,
}

impl<S: Slot> Shard<S> {
    fn new(start: usize, envs: Vec<Env<S>>) -> Shard<S> {
        Shard { start, envs, out: Out::default(), ext: S::Out::default(), errors: Vec::new(), failed: Vec::new() }
    }

    /// Runs `f` on each battle but the failed ones (with its index in the
    /// shard): an error or a panic fails the battle it happened on
    /// ([`guard`]), and the other battles go on.
    fn each(&mut self, mut f: impl FnMut(&mut Env<S>, &mut Out, usize) -> Result<(), String>) {
        let Shard { start, envs, out, failed, .. } = self;
        for (j, env) in envs.iter_mut().enumerate() {
            if env.poisoned.is_none() {
                guard(env, *start + j, failed, |env| f(env, out, j));
            }
        }
    }
}

/// Runs `f` on battle `i`: an error or a panic fails the battle (its
/// episode ends; see the module docs), recorded in `failed`.
fn guard<S: Slot>(
    env: &mut Env<S>,
    i: usize,
    failed: &mut Vec<BattleError>,
    f: impl FnOnce(&mut Env<S>) -> Result<(), String>,
) {
    let message = match catch_unwind(AssertUnwindSafe(|| f(env))) {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e,
        Err(payload) => format!("panic: {}", panic_message(&*payload)),
    };
    env.poisoned = Some(message.clone());
    env.unreported = true;
    failed.push(BattleError { battle: i, episode: env.episode, message });
}

type Job<S> = Arc<dyn Fn(&mut Shard<S>, &PoolConfig) + Send + Sync>;

/// One call's job and the next shard to claim.
struct Run<S: Slot> {
    /// Tags the workers' replies, so that a reply to an earlier call is
    /// never counted for this one.
    id: u64,
    job: Job<S>,
    /// The pool's configuration for this call.
    config: Arc<PoolConfig>,
    next: AtomicUsize,
}

impl<S: Slot> Run<S> {
    /// Claims the next shard, if any is left.
    fn claim(&self, count: usize) -> Option<usize> {
        let i = self.next.fetch_add(1, Ordering::Relaxed);
        (i < count).then_some(i)
    }

    /// A worker's part of the call: claims shards and runs the job on them
    /// until none is left, reporting each one on `done` as soon as it is
    /// done. False if the pool has gone.
    fn work(&self, shards: &[Mutex<Shard<S>>], done: &Sender<Done>) -> bool {
        while let Some(i) = self.claim(shards.len()) {
            let ok = run_job(&self.job, &mut lock(&shards[i]), &self.config);
            if done.send(Done { run: self.id, shard: i, ok }).is_err() {
                return false;
            }
        }
        true
    }
}

/// A worker's report: shard `shard` of call `run` is done, and whether the
/// job went well there.
#[derive(Clone, Copy, Debug)]
struct Done {
    run: u64,
    shard: usize,
    ok: bool,
}

#[derive(Debug)]
struct Worker<S: Slot> {
    jobs: Sender<Arc<Run<S>>>,
    handle: Option<JoinHandle<()>>,
}

/// Shards per thread, by default (the pool's and the vector
/// environment's, [`EnvConfig::shards_per_thread`](crate::env::EnvConfig)):
/// threads claim shards as they go, so a thread that falls behind (a slow
/// shard, or a core the OS gives to someone else) holds up a call by a
/// fraction of its share, not all of it. A pool has at most one shard per
/// battle.
pub const SHARDS_PER_THREAD: usize = 8;

/// Battles stepped together (see the module docs), each with a [`Slot`]
/// payload (none by default).
pub struct Pool<S: Slot = ()> {
    /// The configuration, replaced whole when [`Pool::set_seed`] changes
    /// the seed (each call takes the current one).
    config: Mutex<Arc<PoolConfig>>,
    n: usize,
    space: Arc<ActionSpace>,
    shards: Arc<Vec<Mutex<Shard<S>>>>,
    threads: usize,
    workers: Vec<Worker<S>>,
    /// The workers' replies, one per shard they ran.
    done: Mutex<Receiver<Done>>,
    /// The next call's id.
    next_run: AtomicU64,
    /// Held for a whole call (run and collect), so concurrent callers can't
    /// interleave.
    running: Mutex<()>,
    /// How long the last call waited for the workers once no shard was
    /// left to claim, in nanoseconds.
    last_wait: AtomicU64,
    /// The battles that failed since the last [`Pool::take_errors`].
    errors: Mutex<Vec<BattleError>>,
}

impl<S: Slot> fmt::Debug for Pool<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool").field("n", &self.n).field("threads", &self.threads).finish()
    }
}

/// Adds a call's failed battles to `kept`, in battle order (the shards
/// finish in any order), up to [`MAX_KEPT_ERRORS`].
pub(crate) fn keep_errors(kept: &Mutex<Vec<BattleError>>, mut failed: Vec<BattleError>) {
    if failed.is_empty() {
        return;
    }
    failed.sort_by_key(|e| e.battle);
    let mut kept = lock(kept);
    let room = MAX_KEPT_ERRORS.saturating_sub(kept.len());
    kept.extend(failed.into_iter().take(room));
}

/// Locks `m`, whether or not a panic poisoned it (every value behind the
/// pool's mutexes is left consistent by a panic, or reset).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A panic's message (its payload's `&str` or `String`).
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

/// Runs a job on a shard. False if it reported an error outside a battle
/// (a battle's failure is in [`Shard::failed`]), or panicked outside one.
fn run_job<S: Slot>(job: &Job<S>, shard: &mut Shard<S>, config: &PoolConfig) -> bool {
    shard.errors.clear();
    shard.failed.clear();
    if catch_unwind(AssertUnwindSafe(|| job(shard, config))).is_err() {
        shard.errors.push(format!("pool panic in the shard of battles {}..", shard.start));
    }
    shard.errors.is_empty()
}

/// How long the calling thread polls for the workers to finish, after its
/// own shard, before it sleeps.
const WAIT_SPIN: Duration = Duration::from_millis(2);

/// The next job, or `None` once the pool is dropped. Polls for `spin`
/// before sleeping (see [`PoolConfig::spin`]).
fn next_job<S: Slot>(rx: &Receiver<Arc<Run<S>>>, spin: Duration) -> Option<Arc<Run<S>>> {
    let start = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(job) => return Some(job),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) if start.elapsed() < spin => std::hint::spin_loop(),
            Err(TryRecvError::Empty) => return rx.recv().ok(),
        }
    }
}

/// Battles per [`Pool::play_random`] round: between rounds the caller can
/// stop the call (Python's Ctrl-C).
const PLAY_ROUND: u64 = 512;

impl Pool {
    /// Creates the battles (each at its first episode), split over `threads`
    /// threads (at most one per battle): the caller and `threads - 1` workers.
    /// An error if the teams can't play the format ([`Teams::check`]).
    pub fn new(config: PoolConfig, threads: usize) -> Result<Pool, String> {
        Pool::with_slots(config, threads)
    }
}

impl<S: Slot> Pool<S> {
    /// [`Pool::new`] with a [`Slot`] payload per battle.
    pub(crate) fn with_slots(config: PoolConfig, threads: usize) -> Result<Pool<S>, String> {
        Pool::with_envs(config, threads, SHARDS_PER_THREAD, |config, i| Env::start(config, i, 0))
    }

    /// [`Pool::with_slots`] with `shards_per_thread` shards per thread
    /// (with more than one thread), whose battle `i` starts as `make(config,
    /// i)` says.
    pub(crate) fn with_envs(
        config: PoolConfig,
        threads: usize,
        shards_per_thread: usize,
        make: impl Fn(&PoolConfig, usize) -> Result<Env<S>, String>,
    ) -> Result<Pool<S>, String> {
        if config.n == 0 {
            return Err("a pool needs at least one battle".into());
        }
        config.teams.check(&config.format)?;
        let space = Arc::new(ActionSpace::new(&config.format));
        let threads = threads.clamp(1, config.n);
        // At most one shard per battle, whatever `shards_per_thread` says.
        let count = if threads == 1 { 1 } else { threads.saturating_mul(shards_per_thread.max(1)).min(config.n) };
        let config = Arc::new(config);
        let mut shards = Vec::with_capacity(count);
        for t in 0..count {
            let (start, end) = (config.n * t / count, config.n * (t + 1) / count);
            let envs = (start..end).map(|i| make(&config, i)).collect::<Result<Vec<_>, _>>()?;
            shards.push(Mutex::new(Shard::new(start, envs)));
        }
        let shards = Arc::new(shards);
        let (done_tx, done_rx) = channel();
        let mut workers = Vec::new();
        // The calling thread is the first of the `threads` (see `run`).
        for _ in 1..threads {
            let (tx, rx) = channel::<Arc<Run<S>>>();
            let shards = shards.clone();
            let spin = config.spin;
            let done = done_tx.clone();
            let handle = std::thread::Builder::new()
                .name("pikapro-pool".into())
                .spawn(move || {
                    while let Some(run) = next_job(&rx, spin) {
                        if !run.work(&shards, &done) {
                            break;
                        }
                    }
                })
                .map_err(|e| format!("spawning a worker: {e}"))?;
            workers.push(Worker { jobs: tx, handle: Some(handle) });
        }
        Ok(Pool {
            n: config.n,
            space,
            config: Mutex::new(config),
            shards,
            threads,
            workers,
            done: Mutex::new(done_rx),
            next_run: AtomicU64::new(0),
            running: Mutex::new(()),
            last_wait: AtomicU64::new(0),
            errors: Mutex::new(Vec::new()),
        })
    }

    /// The number of battles.
    pub fn len(&self) -> usize {
        self.n
    }

    /// Never true: a pool has at least one battle.
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    /// The current configuration (with the seed [`Pool::set_seed`] set).
    pub fn config(&self) -> Arc<PoolConfig> {
        lock(&self.config).clone()
    }

    /// Sets the base seed: the episodes started from now on (by
    /// [`Pool::reset`], `auto_reset` or [`Pool::play_random`]) play with
    /// battle seed [`episode_seed`]`(seed, e)`, and [`Pool::seeds`] reads
    /// it.
    pub fn set_seed(&self, seed: u64) {
        let _running = lock(&self.running);
        let mut config = lock(&self.config);
        if config.seed != seed {
            *config = Arc::new(PoolConfig { seed, ..(**config).clone() });
        }
    }

    /// Replaces entries of a [`Teams::Pairs`] source: entry `k` of
    /// `updates` puts its pair at position `k.0` of the list, for the
    /// episodes started from now on (an episode under way keeps the teams
    /// it started with). The list keeps its length, so episode `e` still
    /// plays position `e % len`. Every new sheet must pass the check
    /// starting a battle does ([`Teams::check`]); an error (another
    /// source, a position out of range, a team the format refuses) changes
    /// nothing.
    pub fn set_pairs(&self, updates: Vec<(usize, Arc<[TeamSheet; 2]>)>) -> Result<(), String> {
        let _running = lock(&self.running);
        let mut config = lock(&self.config);
        let Teams::Pairs(old) = &config.teams else {
            return Err("set_pairs needs a list of team pairs as the teams".into());
        };
        let mut pairs = old.clone();
        for (k, pair) in updates {
            let len = pairs.len();
            let slot = pairs.get_mut(k).ok_or_else(|| format!("pair {k} is out of range (the list has {len})"))?;
            for (p, sheet) in pair.iter().enumerate() {
                sheet.team.validate(&config.format).map_err(|e| format!("pair {k}, player {p}'s team: {e}"))?;
            }
            *slot = pair;
        }
        *config = Arc::new(PoolConfig { teams: Teams::Pairs(pairs), ..(**config).clone() });
        Ok(())
    }

    /// The format's action space.
    pub fn space(&self) -> &ActionSpace {
        &self.space
    }

    /// The battles that failed since the last call to this, in order (see
    /// the module docs). At most [`MAX_KEPT_ERRORS`] are kept.
    pub fn take_errors(&self) -> Vec<BattleError> {
        core::mem::take(&mut *lock(&self.errors))
    }

    /// Runs `job` on every shard and waits for all of them. Fails only for
    /// a failure outside the battles (a battle's goes to
    /// [`Pool::take_errors`]).
    fn run(&self, job: Job<S>) -> Result<(), String> {
        self.run_collect(job, |_| {})
    }

    /// [`Pool::run`], calling `collect` on the calling thread for each
    /// shard as soon as it is done (in any order), while the others may
    /// still run: the caller copies finished shards out before claiming
    /// more of its own, so the copy mostly hides behind the other threads'
    /// work.
    ///
    /// A panic in `collect` stops the collecting, not the call: the call
    /// still runs on every shard and waits for them (so no worker is left
    /// running it into the next call), then fails.
    fn run_collect(&self, job: Job<S>, mut collect: impl FnMut(&Shard<S>)) -> Result<(), String> {
        let id = self.next_run.fetch_add(1, Ordering::Relaxed);
        let run = Arc::new(Run { id, job, config: self.config(), next: AtomicUsize::new(0) });
        // A panic in `collect` (the caller's copy) is kept, and the shards
        // still to come are waited for, not collected.
        let mut broken: Option<String> = None;
        let mut failed = Vec::new();
        let mut collect = |shard: &mut Shard<S>| {
            failed.append(&mut shard.failed);
            if broken.is_none()
                && let Err(payload) = catch_unwind(AssertUnwindSafe(|| collect(shard)))
            {
                broken = Some(panic_message(&*payload));
            }
        };
        let mut stopped = false;
        for w in &self.workers {
            if w.jobs.send(run.clone()).is_err() {
                stopped = true;
            }
        }
        let count = self.shards.len();
        let mut ok = true;
        let mut left = count;
        // The calling thread claims shards too, but first collects the
        // shards the workers finished. Once none is left to claim it waits,
        // polling for a while (a thread woken from sleep can take a long
        // time to run again).
        let done = lock(&self.done);
        let mut waiting: Option<Instant> = None;
        while left > 0 && !stopped {
            let msg = match done.try_recv() {
                Ok(msg) => Some(msg),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    // No worker is left (with none, the caller runs
                    // every shard).
                    stopped = !self.workers.is_empty();
                    None
                }
            };
            if let Some(Done { run: reply, shard, ok: shard_ok }) = msg {
                if reply == id {
                    ok &= shard_ok;
                    collect(&mut lock(&self.shards[shard]));
                    left -= 1;
                }
                continue;
            }
            if let Some(i) = run.claim(count) {
                let mut shard = lock(&self.shards[i]);
                ok &= run_job(&run.job, &mut shard, &run.config);
                collect(&mut shard);
                left -= 1;
                continue;
            }
            let since = *waiting.get_or_insert_with(Instant::now);
            if since.elapsed() < WAIT_SPIN {
                std::hint::spin_loop();
                continue;
            }
            match done.recv() {
                Ok(Done { run: reply, shard, ok: shard_ok }) if reply == id => {
                    ok &= shard_ok;
                    collect(&mut lock(&self.shards[shard]));
                    left -= 1;
                }
                Ok(_) => {}
                Err(_) => stopped = true,
            }
        }
        drop(done);
        let waited = waiting.map_or(0, |w| w.elapsed().as_nanos() as u64);
        self.last_wait.store(waited, Ordering::Relaxed);
        keep_errors(&self.errors, failed);
        let mut errors = Vec::new();
        if stopped {
            errors.push("a pool worker has stopped".to_string());
        }
        if let Some(why) = broken {
            errors.push(format!("panic while collecting the shards' outputs: {why}"));
        }
        if !ok {
            for shard in self.shards.iter() {
                errors.append(&mut lock(shard).errors);
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
    }

    /// Concatenates a buffer of every shard, in order.
    fn gather<T: Copy>(&self, len: usize, buf: impl Fn(&Shard<S>) -> &[T]) -> Vec<T> {
        let mut out = Vec::with_capacity(len);
        for shard in self.shards.iter() {
            out.extend_from_slice(buf(&lock(shard)));
        }
        out
    }

    /// Submits one step of actions (see [`ActionMode`]) and, with
    /// `auto_reset`, starts new episodes where battles ended. A choice the
    /// side's request showed but that it hid a reason to refuse is
    /// unavailable (status 2, see the module docs). A battle that fails
    /// ends its episode alone ([`PoolStep::failed`]); an error is for the
    /// call itself (the number of actions, or a failure outside the
    /// battles).
    pub fn step(&self, actions: Vec<i64>, mode: ActionMode) -> Result<PoolStep, String> {
        let _running = lock(&self.running);
        let per = match mode {
            ActionMode::Choice => 2,
            ActionMode::Slots => 2 * self.space.active,
        };
        if actions.len() != self.n * per {
            return Err(format!("expected {} actions, got {}", self.n * per, actions.len()));
        }
        let actions = Arc::new(actions);
        let space = self.space.clone();
        self.run(Arc::new(move |shard, config| step_shard(shard, config, &space, &actions, mode)))?;
        let n = self.n;
        let status = self.gather(2 * n, |s| &s.out.i8s[..2 * s.envs.len()]);
        let done = self.gather(n, |s| &s.out.bools[..s.envs.len()]);
        let failed = self.gather(n, |s| &s.out.bools[s.envs.len()..]);
        let winner = self.gather(n, |s| &s.out.i8s[2 * s.envs.len()..]);
        Ok(PoolStep { status, done, winner, failed })
    }

    /// The masks of every battle (see [`MaskKind`]): each side's actions as
    /// its request shows them (see the module docs); all false for a
    /// battle that failed.
    pub fn masks(&self, kind: MaskKind) -> Result<Vec<bool>, String> {
        let _running = lock(&self.running);
        let space = self.space.clone();
        self.run(Arc::new(move |shard, _| mask_shard(shard, &space, kind)))?;
        let space = &self.space;
        let per = match kind {
            MaskKind::Choice => space.size,
            MaskKind::Slots => space.active * space.slot_size,
            MaskKind::Preview => space.preview_size,
        };
        Ok(self.gather(self.n * 2 * per, |s| &s.out.bools))
    }

    /// Each side's request (`arrays::REQUEST_CODES`), `n * 2` values (0,
    /// waiting, for a battle that failed).
    pub fn requests(&self) -> Result<Vec<i8>, String> {
        let _running = lock(&self.running);
        self.run(Arc::new(|shard, _| {
            shard.out.i8s.clear();
            shard.out.i8s.resize(2 * shard.envs.len(), 0);
            shard.each(|env, out, j| {
                for s in SideId::BOTH {
                    out.i8s[2 * j + s.index()] = arrays::request_code(env.battle.request(s));
                }
                Ok(())
            });
        }))?;
        Ok(self.gather(2 * self.n, |s| &s.out.i8s))
    }

    /// Uniformly random choices from the built-in policy among the ones
    /// each side's request shows (its masks), as actions for
    /// [`Pool::step`] (`-1` for sides that are not asked, and in a battle
    /// that failed).
    pub fn sample_random(&self, mode: ActionMode) -> Result<Vec<i64>, String> {
        let _running = lock(&self.running);
        let space = self.space.clone();
        self.run(Arc::new(move |shard, _| sample_shard(shard, &space, mode)))?;
        let per = if mode == ActionMode::Choice { 2 } else { 2 * self.space.active };
        Ok(self.gather(per * self.n, |s| &s.out.i64s))
    }

    /// Plays `battles` whole battles with the built-in random policy, on
    /// the workers, without returning to the caller in between: battle `i`
    /// plays `battles / n` of them (one more for the first `battles % n`),
    /// finishing the one under way first. Returns the totals and the
    /// elapsed seconds.
    ///
    /// The battles are played in rounds of about 512 battles,
    /// which doesn't change them. Between rounds `check` may stop the call
    /// with an error (the rounds already played stay played). A battle
    /// that fails ends its episode alone (it isn't in the totals, and
    /// [`Pool::take_errors`] says why) and starts its next one at the next
    /// round.
    pub fn play_random(
        &self,
        battles: u64,
        check: impl FnMut() -> Result<(), String>,
    ) -> Result<(Totals, f64), String> {
        self.play_in_rounds(battles, PLAY_ROUND, check)
    }

    /// How many of [`Pool::play_random`]'s `battles` battle `i` plays:
    /// `battles / n`, and one more for the first `battles % n`. A battle
    /// that plays any starts by finishing the one under way, or by
    /// starting its next episode if that one is over.
    pub fn play_random_share(&self, battles: u64, i: usize) -> u64 {
        let n = self.n as u64;
        battles / n + u64::from((i as u64) < battles % n)
    }

    /// [`Pool::play_random`] in rounds of about `round` battles.
    fn play_in_rounds(
        &self,
        battles: u64,
        round: u64,
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<(Totals, f64), String> {
        let _running = lock(&self.running);
        let start = Instant::now();
        let n = self.n as u64;
        // Battles per battle slot and round.
        let per_round = round.div_ceil(n);
        let rounds = battles.div_ceil(n).div_ceil(per_round).max(1);
        let mut totals = Totals::default();
        for round in 0..rounds {
            if round > 0 {
                check()?;
            }
            let quota = move |i: usize| {
                // (`play_random_share`.)
                let all = battles / n + u64::from((i as u64) < battles % n);
                all.saturating_sub(round * per_round).min(per_round)
            };
            self.run(Arc::new(move |shard, config| play_shard(shard, config, round == 0, quota)))?;
            for shard in self.shards.iter() {
                totals.add(&lock(shard).out.totals);
            }
        }
        Ok((totals, start.elapsed().as_secs_f64()))
    }

    /// Every battle's raw state (see [`arrays`]): Pokémon rows
    /// (`n * 2 * 6 * POKEMON_WIDTH`), side rows (`n * 2 * SIDE_WIDTH`) and
    /// field rows (`n * FIELD_WIDTH`). A battle that failed has no Pokémon
    /// (rows of −1) and zeros elsewhere.
    pub fn state(&self) -> Result<[Vec<i32>; 3], String> {
        let _running = lock(&self.running);
        let (p, s, f) = (2 * MAX_TEAM * POKEMON_WIDTH, 2 * SIDE_WIDTH, FIELD_WIDTH);
        self.run(Arc::new(move |shard, _| {
            let m = shard.envs.len();
            shard.out.i32s.clear();
            shard.out.i32s.resize(m * (p + s + f), 0);
            shard.out.i32s[..m * p].fill(-1);
            shard.each(|env, out, j| {
                let (pokemon, rest) = out.i32s.split_at_mut(m * p);
                let (sides, field) = rest.split_at_mut(m * s);
                arrays::fill(
                    &env.battle,
                    &mut pokemon[j * p..][..p],
                    &mut sides[j * s..][..s],
                    &mut field[j * f..][..f],
                );
                Ok(())
            });
        }))?;
        let n = self.n;
        let pokemon = self.gather(n * p, |sh| &sh.out.i32s[..sh.envs.len() * p]);
        let sides = self.gather(n * s, |sh| &sh.out.i32s[sh.envs.len() * p..][..sh.envs.len() * s]);
        let field = self.gather(n * f, |sh| &sh.out.i32s[sh.envs.len() * (p + s)..]);
        Ok([pokemon, sides, field])
    }

    /// Starts a new episode in the given battles (all when `None`), failed
    /// ones included. An episode that fails to start is skipped: the
    /// battle fails ([`Pool::take_errors`]), and the next [`Pool::step`]
    /// reports it ([`PoolStep::failed`]). An error only for a battle out of
    /// range (nothing is reset) or a failure outside the battles.
    pub fn reset(&self, which: Option<Vec<usize>>) -> Result<(), String> {
        let _running = lock(&self.running);
        if let Some(bad) = which.as_ref().and_then(|w| w.iter().find(|&&i| i >= self.n)) {
            return Err(format!("battle {bad} is out of range"));
        }
        let which = Arc::new(which);
        self.run(Arc::new(move |shard, config| {
            let Shard { start, envs, failed, .. } = shard;
            for (j, env) in envs.iter_mut().enumerate() {
                let i = *start + j;
                if which.as_ref().as_ref().is_none_or(|w| w.contains(&i)) {
                    guard(env, i, failed, |env| env.reset(config, i));
                }
            }
        }))
    }

    /// Runs `f` on battle `i` (between calls): its battle, sheets and
    /// payload, to read.
    pub fn with_env<R>(&self, i: usize, f: impl FnOnce(&Env<S>) -> R) -> Option<R> {
        self.with_env_mut(i, |env| f(env))
    }

    /// [`Pool::with_env`], mutably: for the crate's own calls and tests (a
    /// change behind the pool's back would desync what it keeps).
    pub(crate) fn with_env_mut<R>(&self, i: usize, f: impl FnOnce(&mut Env<S>) -> R) -> Option<R> {
        if i >= self.n {
            return None;
        }
        let _running = lock(&self.running);
        for shard in self.shards.iter() {
            let mut shard = lock(shard);
            if i >= shard.start && i < shard.start + shard.envs.len() {
                let start = shard.start;
                return Some(f(&mut shard.envs[i - start]));
            }
        }
        None
    }

    /// The episode each battle is playing, `n` values.
    pub fn episodes(&self) -> Vec<u64> {
        let _running = lock(&self.running);
        let mut out = Vec::with_capacity(self.n);
        for shard in self.shards.iter() {
            out.extend(lock(shard).envs.iter().map(|e| e.episode));
        }
        out
    }

    /// The battle seed of each battle's current episode (made with the base
    /// seed of the call that started it, whatever [`Pool::set_seed`] set
    /// since): `Battle::new(format, teams, BattleOptions::new(Seed::fast(s)))`
    /// replays it.
    pub fn seeds(&self) -> Vec<u64> {
        let _running = lock(&self.running);
        let mut out = Vec::with_capacity(self.n);
        for shard in self.shards.iter() {
            out.extend(lock(shard).envs.iter().map(|e| e.seed));
        }
        out
    }

    /// Runs `job` on every shard, on the pool's threads, and `collect` on
    /// each shard's output on the calling thread as soon as that shard is
    /// done, in any order, while other shards may still run (a single copy
    /// into the caller's buffers, mostly hidden behind the other shards'
    /// work). `job` gets the shard's first battle index, its battles and
    /// its output buffers ([`Slot::Out`]); an error or a panic fails the
    /// call, after it ran (and `collect` ran) on every shard. Unlike the
    /// pool's own calls, a panic outside a battle doesn't poison one
    /// battle: the caller should reset them all.
    pub(crate) fn run_shards<J, C>(&self, job: J, mut collect: C) -> Result<(), String>
    where
        J: Fn(usize, &mut [Env<S>], &mut S::Out, &PoolConfig) -> Result<(), String> + Send + Sync + 'static,
        C: FnMut(usize, &S::Out),
    {
        let _running = lock(&self.running);
        self.run_collect(
            Arc::new(move |shard, config| {
                let Shard { start, envs, ext, errors, .. } = shard;
                if let Err(e) = job(*start, envs, ext, config) {
                    errors.push(e);
                }
            }),
            |shard| collect(shard.start, &shard.ext),
        )
    }

    /// How long the last call's calling thread waited for the workers once
    /// no shard was left to claim (the barrier's cost, with the copies of
    /// the shards that finished meanwhile).
    pub(crate) fn last_wait(&self) -> Duration {
        Duration::from_nanos(self.last_wait.load(Ordering::Relaxed))
    }

    /// Each shard's first battle and number of battles, in order: the
    /// shards are contiguous and cover every battle.
    pub(crate) fn shard_bounds(&self) -> Vec<(usize, usize)> {
        let _running = lock(&self.running);
        self.shards.iter().map(|s| lock(s)).map(|s| (s.start, s.envs.len())).collect()
    }

    /// Runs `job` on every shard with its part of `parts` (one per shard, in
    /// order), which the job may borrow from the caller: on the calling
    /// thread with one thread, else on the calling thread and threads
    /// started for the call (the workers wait), which claim shards as the
    /// workers do. Then, still within the call (a concurrent call can't
    /// change them in between), `collect` reads each shard's buffers, in
    /// order. Errors and panics as in [`Pool::run_shards`];
    /// [`Pool::last_wait`] is the calling thread's wait for the others once
    /// it ran out of shards.
    pub(crate) fn run_parts<P, J>(&self, parts: Vec<P>, job: J, mut collect: impl FnMut(&S::Out)) -> Result<(), String>
    where
        P: Send,
        J: Fn(usize, &mut [Env<S>], &mut S::Out, &PoolConfig, &mut P) -> Result<(), String> + Sync,
    {
        if parts.len() != self.shards.len() {
            return Err(format!("{} parts for {} shards", parts.len(), self.shards.len()));
        }
        let _running = lock(&self.running);
        let parts: Vec<Mutex<P>> = parts.into_iter().map(Mutex::new).collect();
        let next = AtomicUsize::new(0);
        let config = self.config();
        let config = &*config;
        let work = || {
            let mut ok = true;
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(shard) = self.shards.get(i) else { break };
                let mut shard = lock(shard);
                let mut part = lock(&parts[i]);
                shard.errors.clear();
                let Shard { start, envs, ext, errors, .. } = &mut *shard;
                match catch_unwind(AssertUnwindSafe(|| job(*start, envs, ext, config, &mut part))) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => errors.push(e),
                    Err(_) => errors.push(format!("pool panic in the shard of battles {}..", *start)),
                }
                ok &= errors.is_empty();
            }
            ok
        };
        let mut waited = Duration::ZERO;
        let ok = if self.threads == 1 {
            work()
        } else {
            std::thread::scope(|scope| {
                // A thread the system won't start leaves its share to the
                // others.
                let others: Vec<_> = (1..self.threads)
                    .filter_map(|_| {
                        std::thread::Builder::new().name("pikapro-step".into()).spawn_scoped(scope, work).ok()
                    })
                    .collect();
                let mine = work();
                let t = Instant::now();
                let ok = others.into_iter().fold(mine, |ok, h| ok & h.join().unwrap_or(false));
                waited = t.elapsed();
                ok
            })
        };
        self.last_wait.store(waited.as_nanos() as u64, Ordering::Relaxed);
        let mut errors = Vec::new();
        for shard in self.shards.iter() {
            let mut shard = lock(shard);
            collect(&shard.ext);
            if !ok {
                errors.append(&mut shard.errors);
            }
        }
        if ok { Ok(()) } else { Err(errors.join("; ")) }
    }

    /// Totals over every battle that finished during [`Pool::step`] or
    /// [`Pool::play_random`] since the pool was made.
    pub fn finished(&self) -> Totals {
        let _running = lock(&self.running);
        let mut totals = Totals::default();
        for shard in self.shards.iter() {
            totals.add(&lock(shard).out.finished);
        }
        totals
    }
}

impl<S: Slot> Drop for Pool<S> {
    fn drop(&mut self) {
        for w in &mut self.workers {
            // Closing the channel stops the worker.
            let (tx, _) = channel();
            drop(core::mem::replace(&mut w.jobs, tx));
            if let Some(h) = w.handle.take() {
                let _ = h.join();
            }
        }
    }
}

fn winner_code(battle: &Battle) -> i8 {
    match battle.result() {
        None => -1,
        Some(r) => r.winner.map_or(2, |w| w.index() as i8),
    }
}

fn step_shard<S: Slot>(
    shard: &mut Shard<S>,
    config: &PoolConfig,
    space: &ActionSpace,
    actions: &[i64],
    mode: ActionMode,
) {
    let m = shard.envs.len();
    let start = shard.start;
    // Status (2 per battle), then winner; done, then failed.
    shard.out.i8s.clear();
    shard.out.i8s.resize(3 * m, 0);
    shard.out.bools.clear();
    shard.out.bools.resize(2 * m, false);
    let Shard { envs, out, failed, .. } = shard;
    for (j, env) in envs.iter_mut().enumerate() {
        let i = start + j;
        if env.poisoned.is_none() {
            guard(env, i, failed, |env| step_battle(env, out, config, space, actions, mode, i, j, m));
        }
        // A battle that failed (in this call or since the last step) ends
        // its episode here, once, unless its episode ended on its own this
        // step (then only the next one failed, to start: it is reported at
        // the next step).
        if env.poisoned.is_none() || out.bools[j] {
            continue;
        }
        out.i8s[2 * j..][..2].fill(0);
        out.i8s[2 * m + j] = -1;
        if env.unreported {
            env.unreported = false;
            out.bools[j] = true;
            out.bools[m + j] = true;
            if config.auto_reset {
                guard(env, i, failed, |env| env.reset(config, i));
            }
        }
    }
}

/// Battle `i`'s (shard index `j` of `m`) part of [`step_shard`].
#[allow(clippy::too_many_arguments)]
fn step_battle<S: Slot>(
    env: &mut Env<S>,
    out: &mut Out,
    config: &PoolConfig,
    space: &ActionSpace,
    actions: &[i64],
    mode: ActionMode,
    i: usize,
    j: usize,
    m: usize,
) -> Result<(), String> {
    // Without `auto_reset` a battle stays over: it is done once.
    let was_over = env.battle.is_over();
    // What each side was asked before this step: a side's choice can
    // resolve the turn and issue the other side a new request, which this
    // step's action (sampled for the old one) doesn't answer.
    let requests = SideId::BOTH.map(|s| env.battle.request(s));
    for side in SideId::BOTH {
        let request = requests[side.index()];
        if request == Request::Wait || env.battle.is_over() {
            continue;
        }
        // (A team-preview choice depends on the team's size.)
        let team_len = env.battle.state().side(side).team.len();
        let choice = match mode {
            ActionMode::Choice => {
                let a = actions[2 * i + side.index()];
                usize::try_from(a).ok().and_then(|a| space.decode_for(a, team_len))
            }
            ActionMode::Slots => {
                let per = space.active;
                let slots = &actions[(2 * i + side.index()) * per..][..per];
                if matches!(request, Request::TeamPreview { .. }) {
                    usize::try_from(slots[0]).ok().and_then(|k| space.preview_choice(k, team_len))
                } else {
                    let mut idx = [0usize; pikapro::MAX_ACTIVE];
                    let ok = slots.iter().zip(&mut idx).all(|(&a, x)| usize::try_from(a).map(|a| *x = a).is_ok());
                    if ok { space.joint_index(&idx[..per]).and_then(|j| space.decode(j)) } else { None }
                }
            }
        };
        out.i8s[2 * j + side.index()] = choice.map_or(-1, |c| env.submit(side, c));
    }
    out.i8s[2 * m + j] = winner_code(&env.battle);
    if env.battle.is_over() && !was_over {
        out.bools[j] = true;
        out.finished.record_env(env);
        if config.auto_reset {
            env.reset(config, i)?;
        }
    }
    Ok(())
}

fn mask_shard<S: Slot>(shard: &mut Shard<S>, space: &ActionSpace, kind: MaskKind) {
    let per = match kind {
        MaskKind::Choice => space.size,
        MaskKind::Slots => space.active * space.slot_size,
        MaskKind::Preview => space.preview_size,
    };
    shard.out.bools.clear();
    shard.out.bools.resize(shard.envs.len() * 2 * per, false);
    shard.each(|env, out, j| {
        for side in SideId::BOTH {
            let legal = &env.shown(side);
            let out = &mut out.bools[(2 * j + side.index()) * per..][..per];
            match kind {
                MaskKind::Choice => space.fill_mask(legal, legal.team_len(), out),
                MaskKind::Slots => space.fill_slot_mask(legal, out),
                MaskKind::Preview => space.fill_preview_mask(legal, legal.team_len(), out),
            }
        }
        Ok(())
    });
}

fn sample_shard<S: Slot>(shard: &mut Shard<S>, space: &ActionSpace, mode: ActionMode) {
    let per = if mode == ActionMode::Choice { 1 } else { space.active };
    shard.out.i64s.clear();
    shard.out.i64s.resize(shard.envs.len() * 2 * per, -1);
    shard.each(|env, out, j| {
        for side in SideId::BOTH {
            if env.battle.request(side) == Request::Wait || env.battle.is_over() {
                continue;
            }
            let choice = env.shown(side).sample(&mut env.policy);
            // Where team preview brings the whole team, the space names a
            // lead: the sampled order's (uniform), the others in team order.
            let index = match &choice {
                Choice::Team(order) if space.lead_only => order.first().map(|&lead| space.joint_size + lead as usize),
                _ => space.encode(&choice),
            };
            let Some(index) = index else { continue };
            let out = &mut out.i64s[(2 * j + side.index()) * per..][..per];
            match mode {
                ActionMode::Choice => out[0] = index as i64,
                ActionMode::Slots if index >= space.joint_size => out[0] = (index - space.joint_size) as i64,
                ActionMode::Slots => {
                    let slots = space.split_joint(index);
                    for (o, a) in out.iter_mut().zip(slots) {
                        *o = a as i64;
                    }
                }
            }
        }
        Ok(())
    });
}

/// One [`Pool::play_random`] round: battle `i` plays `quota(i)` battles,
/// after finishing the one under way (`first` round). A battle that failed
/// starts its next episode first.
fn play_shard<S: Slot>(shard: &mut Shard<S>, config: &PoolConfig, first: bool, quota: impl Fn(usize) -> u64) {
    let start = shard.start;
    shard.out.totals = Totals::default();
    let Shard { envs, out, failed, .. } = shard;
    for (j, env) in envs.iter_mut().enumerate() {
        let i = start + j;
        let quota = quota(i);
        if quota == 0 {
            continue;
        }
        if env.poisoned.is_some() {
            guard(env, i, failed, |env| env.reset(config, i));
            if env.poisoned.is_some() {
                continue;
            }
        }
        guard(env, i, failed, |env| {
            if first && env.battle.is_over() {
                env.reset(config, i)?;
            }
            for _ in 0..quota {
                while !env.battle.is_over() {
                    for side in SideId::BOTH {
                        let legal = env.battle.legal_actions(side);
                        if legal.is_waiting() {
                            continue;
                        }
                        let choice = legal.sample(&mut env.policy);
                        env.battle.submit(side, choice).expect("a sampled choice is legal");
                        env.decisions += 1;
                        out.totals.decisions += 1;
                    }
                }
                out.totals.record(&env.battle);
                out.finished.record_env(env);
                env.reset(config, i)?;
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::Team;
    use pikapro::testing::sample_team;

    fn config(format: Format, n: usize) -> PoolConfig {
        let sheets = [TeamSheet::new(sample_team(&format)), TeamSheet::new(sample_team(&format))];
        PoolConfig { seed: 11, ..PoolConfig::new(format, n, Teams::Pairs(vec![Arc::new(sheets)])) }
    }

    /// `pikapro bench`'s loop over the pool's battle seeds; with `stepped`,
    /// the pool's `sample_random` + `step` loop instead (both sides sample
    /// before either submits).
    fn bench(format: &Format, seed: u64, battles: u64, stepped: bool) -> Totals {
        let mut t = Totals::default();
        for i in 0..battles {
            let s = episode_seed(seed, i);
            let teams = [sample_team(format), sample_team(format)];
            let mut b = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(s)).without_events()).unwrap();
            let mut policy = BattleRng::new(Seed::fast(!s));
            while !b.is_over() {
                if stepped {
                    let choices = SideId::BOTH.map(|side| {
                        let legal = b.legal_actions(side);
                        (!legal.is_waiting()).then(|| legal.sample(&mut policy))
                    });
                    for (side, choice) in SideId::BOTH.into_iter().zip(choices) {
                        if let Some(c) = choice {
                            b.submit(side, c).unwrap();
                        }
                    }
                    continue;
                }
                for side in SideId::BOTH {
                    let legal = b.legal_actions(side);
                    if !legal.is_waiting() {
                        b.submit(side, legal.sample(&mut policy)).unwrap();
                        t.decisions += 1;
                    }
                }
            }
            t.record(&b);
        }
        t
    }

    #[test]
    fn play_random_matches_bench_for_any_thread_count() {
        let format = Format::gen9_vgc();
        let expected = bench(&format, 11, 20, false);
        for threads in [1, 3, 8] {
            let pool = Pool::new(config(format.clone(), 10), threads).unwrap();
            let (totals, _) = pool.play_random(20, || Ok(())).unwrap();
            assert_eq!(totals, expected, "{threads} threads");
            assert_eq!(pool.finished(), expected, "the finished battles' decisions too");
            // In rounds of one battle per battle slot: 4 rounds, 3 checks.
            let pool = Pool::new(config(format.clone(), 6), threads).unwrap();
            let mut rounds = 0;
            let (totals, _) = pool
                .play_in_rounds(20, 6, || {
                    rounds += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!((totals, rounds), (expected, 3), "{threads} threads");
        }
    }

    #[test]
    fn play_random_stops_between_rounds() {
        let pool = Pool::new(config(Format::gen9_singles(), 2), 1).unwrap();
        let e = pool.play_in_rounds(10, 2, || Err("stop".into())).unwrap_err();
        assert_eq!(e, "stop");
        assert_eq!(pool.finished().battles, 2, "the first round stays played");
    }

    #[test]
    fn stepping_random_choices_is_deterministic() {
        // Sampling in one call and stepping in another plays the same
        // battles in both action modes, whatever the thread count.
        let format = Format::gen9_doubles();
        let expected = bench(&format, 11, 4, true);
        for (mode, threads) in [(ActionMode::Choice, 8), (ActionMode::Slots, 3), (ActionMode::Choice, 1)] {
            let pool = Pool::new(config(format.clone(), 4), threads).unwrap();
            while pool.finished().battles < 4 {
                let actions = pool.sample_random(mode).unwrap();
                let out = pool.step(actions, mode).unwrap();
                assert!(out.status.iter().all(|&s| s >= 0), "{mode:?} {out:?} {:?}", pool.requests());
            }
            let f = pool.finished();
            assert_eq!((f.battles, f.turns, f.wins, f.ties), (4, expected.turns, expected.wins, expected.ties));
        }
    }

    #[test]
    fn automatic_passes_do_not_submit_a_stale_action_after_resolution() {
        use pikapro::{PokemonSet, SlotAction, Target, TeamPreview};
        use pikapro_data::gen9::{abilities as a, moves as m, species as s};

        for automatic_side in SideId::BOTH {
            for mode in [ActionMode::Choice, ActionMode::Slots] {
                let mut format = Format::gen9_doubles();
                format.team_preview = TeamPreview::None;
                let mew = PokemonSet::new(s::MEW).with_ability(a::NO_GUARD).with_moves(&[m::SPLASH, m::HEALING_WISH]);
                let pair = Team::new(vec![
                    PokemonSet::new(s::DONDOZO).with_ability(a::UNAWARE).with_moves(&[m::DOODLE, m::MEMENTO]),
                    PokemonSet::new(s::TATSUGIRI).with_ability(a::COMMANDER).with_moves(&[m::SPLASH]),
                ]);
                let others = Team::new(vec![mew.clone(), mew.clone(), mew]);
                let teams = if automatic_side == SideId::P1 { [pair, others] } else { [others, pair] };
                let sheets = teams.map(TeamSheet::new);
                let pool = Pool::new(PoolConfig::new(format, 1, Teams::Pairs(vec![Arc::new(sheets)])), 1).unwrap();
                let splash = SlotAction::Move { index: 0, target: None, gimmick: None };
                pool.with_env_mut(0, |env| {
                    for index in [0, 1] {
                        env.battle.submit(automatic_side.foe(), Choice::Actions([splash; 2])).unwrap();
                        let action = SlotAction::Move { index, target: Some(Target::Foe(0)), gimmick: None };
                        env.battle.submit(automatic_side, Choice::Actions([action, SlotAction::Pass])).unwrap();
                    }
                    assert_eq!(env.battle.request(automatic_side), Request::Wait);
                })
                .unwrap();
                assert_eq!(pool.requests().unwrap()[automatic_side.index()], 0);
                let per_side = if mode == ActionMode::Choice { 1 } else { 2 };
                let sampled = pool.sample_random(mode).unwrap();
                assert!(sampled[automatic_side.index() * per_side..][..per_side].iter().all(|&a| a == -1));
                // Healing Wish asks the active side for a replacement before
                // the turn finishes. A precomputed pass for the other side
                // must not be submitted against this new request.
                let healing = SlotAction::Move { index: 1, target: None, gimmick: None };
                let choices = SideId::BOTH.map(|side| {
                    if side == automatic_side {
                        Choice::Actions([SlotAction::Pass; 2])
                    } else {
                        Choice::Actions([healing, splash])
                    }
                });
                let space = pool.space();
                let mut actions = Vec::new();
                for choice in choices {
                    let encoded = space.encode(&choice).unwrap();
                    match mode {
                        ActionMode::Choice => actions.push(encoded as i64),
                        ActionMode::Slots => actions.extend(space.split_joint(encoded).map(|i| i as i64)),
                    }
                }
                let before = pool.with_env(0, |env| env.battle.turn()).unwrap();
                let out = pool.step(actions, mode).unwrap();
                assert_eq!(out.status[automatic_side.index()], status::IGNORED);
                assert_eq!(out.status[automatic_side.foe().index()], status::SUBMITTED);
                pool.with_env(0, |env| {
                    assert_eq!(env.battle.turn(), before);
                    assert_eq!(env.battle.request(automatic_side), Request::Wait);
                    assert!(matches!(env.battle.request(automatic_side.foe()), Request::ForceSwitch { .. }));
                })
                .unwrap();
            }
        }
    }

    #[test]
    fn masks_contain_the_sampled_choices() {
        let format = Format::champions_vgc();
        let pool = Pool::new(config(format.clone(), 3), 2).unwrap();
        let space = pool.space().clone();
        for _ in 0..30 {
            let masks = pool.masks(MaskKind::Choice).unwrap();
            let slot_masks = pool.masks(MaskKind::Slots).unwrap();
            let actions = pool.sample_random(ActionMode::Choice).unwrap();
            for (k, &a) in actions.iter().enumerate() {
                if a >= 0 {
                    assert!(masks[k * space.size + a as usize]);
                    if (a as usize) < space.joint_size {
                        let slots = space.split_joint(a as usize);
                        for slot in 0..space.active {
                            assert!(slot_masks[(k * space.active + slot) * space.slot_size + slots[slot]]);
                        }
                    }
                }
            }
            pool.step(actions, ActionMode::Choice).unwrap();
        }
        let [p, s, f] = pool.state().unwrap();
        assert_eq!((p.len(), s.len(), f.len()), (3 * 2 * 6 * POKEMON_WIDTH, 3 * 2 * SIDE_WIDTH, 3 * FIELD_WIDTH));
    }

    #[test]
    fn rejected_actions_change_nothing() {
        let pool = Pool::new(config(Format::gen9_singles(), 2), 1).unwrap();
        let out = pool.step(vec![-1, 9999, 0, 0], ActionMode::Choice).unwrap();
        assert_eq!(out.status[..2], [-1, -1]);
        assert!(pool.step(vec![0; 3], ActionMode::Choice).is_err());
    }

    /// Without `auto_reset`, a battle that ended is done once, counted once.
    #[test]
    fn a_finished_battle_is_done_once() {
        let pool = Pool::new(PoolConfig { auto_reset: false, ..config(Format::gen9_singles(), 1) }, 1).unwrap();
        let mut done = 0;
        let mut decisions = 0;
        for _ in 0..2000 {
            let out = pool.step(pool.sample_random(ActionMode::Choice).unwrap(), ActionMode::Choice).unwrap();
            done += out.done.iter().filter(|&&d| d).count();
            decisions += out.status.iter().filter(|&&s| s == 1).count() as u64;
            if pool.with_env(0, |env| env.battle.is_over()).unwrap() {
                assert!(out.winner[0] >= 0);
            }
        }
        assert_eq!(done, 1);
        let f = pool.finished();
        assert_eq!((f.battles, f.decisions), (1, decisions));
    }

    /// A team list with a team the engine rejects is refused when the pool
    /// is made, naming the team (not when a seed happens to draw it).
    #[test]
    fn a_team_that_can_t_play_is_refused() {
        let format = Format::gen9_singles();
        let good = Arc::new([TeamSheet::new(sample_team(&format)), TeamSheet::new(sample_team(&format))]);
        let bad = Arc::new([TeamSheet::new(Team::new(Vec::new())), TeamSheet::new(Team::new(Vec::new()))]);
        let e = Pool::new(PoolConfig { teams: Teams::Pairs(vec![good, bad]), ..config(format, 1) }, 1).unwrap_err();
        assert!(e.starts_with("pair 1, player 0's team: team has 0 Pokémon"), "{e}");
    }

    /// A panic fails its battle alone: the call goes on with the others,
    /// the failed one shows nothing until it is reset, and the error is
    /// taken once.
    #[test]
    fn a_panic_fails_its_battle_alone() {
        for threads in [1, 3] {
            let pool = Pool::new(config(Format::gen9_singles(), 4), threads).unwrap();
            pool.run(Arc::new(|shard, _| {
                shard.each(|env, _, _| {
                    env.decisions += 100;
                    assert!(env.episode != 2, "boom");
                    Ok(())
                })
            }))
            .unwrap();
            let errors = pool.take_errors();
            assert_eq!(errors, [BattleError { battle: 2, episode: 2, message: "panic: boom".into() }]);
            assert!(pool.take_errors().is_empty(), "taken");
            for i in 0..4 {
                assert_eq!(pool.with_env(i, |env| (env.decisions, env.is_poisoned())), Some((100, i == 2)));
            }
            let size = pool.space().size;
            let masks = pool.masks(MaskKind::Choice).unwrap();
            assert!(masks[4 * size..6 * size].iter().all(|&m| !m) && masks[..4 * size].iter().any(|&m| m));
            assert_eq!(pool.requests().unwrap()[4..6], [0, 0]);
            assert_eq!(pool.sample_random(ActionMode::Choice).unwrap()[4..6], [-1, -1]);
            pool.reset(Some(vec![2])).unwrap();
            assert_eq!(pool.episodes(), [0, 1, 6, 3]);
            assert!(pool.masks(MaskKind::Choice).unwrap()[4 * size..6 * size].iter().any(|&m| m));
        }
    }

    /// Battle 1 of 3 fails at its first step (a panic in the job before its
    /// actions): `step` still returns every other battle's results, reports
    /// the failure once, and (with `auto_reset`) starts its next episode
    /// in the same step; without, the battle stays failed until reset.
    #[test]
    fn a_failed_battle_ends_its_episode_in_the_step() {
        let format = Format::gen9_singles();
        for auto_reset in [true, false] {
            let pool = Pool::new(PoolConfig { auto_reset, ..config(format.clone(), 3) }, 2).unwrap();
            let clean = Pool::new(PoolConfig { auto_reset, ..config(format.clone(), 3) }, 1).unwrap();
            pool.with_env_mut(1, |env| {
                env.poisoned = Some("test".into());
                env.unreported = true;
            });
            let actions = pool.sample_random(ActionMode::Choice).unwrap();
            assert_eq!(actions[2..4], [-1, -1], "nothing to sample in the failed battle");
            let want = clean.step(clean.sample_random(ActionMode::Choice).unwrap(), ActionMode::Choice).unwrap();
            let out = pool.step(actions, ActionMode::Choice).unwrap();
            for i in [0, 2] {
                assert_eq!(
                    (out.status[2 * i..][..2].to_vec(), out.done[i], out.winner[i], out.failed[i]),
                    (want.status[2 * i..][..2].to_vec(), want.done[i], want.winner[i], false),
                    "battle {i} plays on"
                );
            }
            assert_eq!((&out.status[2..4], out.done[1], out.winner[1], out.failed[1]), (&[0, 0][..], true, -1, true));
            assert_eq!(pool.episodes()[1], if auto_reset { 4 } else { 1 });
            let again = pool.step(pool.sample_random(ActionMode::Choice).unwrap(), ActionMode::Choice).unwrap();
            assert!(!again.done[1] && !again.failed[1], "reported once");
            assert_eq!(again.status[2..4] == [0, 0], !auto_reset, "a new episode plays");
            if !auto_reset {
                pool.reset(Some(vec![1])).unwrap();
                assert_eq!(pool.episodes()[1], 4);
            }
            assert!(pool.with_env(1, |env| !env.is_poisoned()).unwrap());
        }
    }

    /// An episode that fails to start (at a reset, or in the step that
    /// ended the one before, with `auto_reset`) is skipped: the battle
    /// fails under the failed episode's number, the episode before keeps
    /// its end and winner, and the next step reports the failure.
    #[test]
    fn an_episode_that_fails_to_start_is_skipped() {
        let format = Format::gen9_singles();
        let pool = Pool::new(config(format, 1), 1).unwrap();
        // Episode 1 on: an empty team, which the engine refuses (the pool's
        // own teams were checked when it was made).
        let bad = {
            let mut c = (*pool.config()).clone();
            c.teams = Teams::Pairs(vec![Arc::new([
                TeamSheet::new(Team::new(Vec::new())),
                TeamSheet::new(Team::new(Vec::new())),
            ])]);
            c
        };
        *lock(&pool.config) = Arc::new(bad);
        let mut ended = None;
        for _ in 0..2000 {
            let out = pool.step(pool.sample_random(ActionMode::Choice).unwrap(), ActionMode::Choice).unwrap();
            if out.done[0] {
                ended = Some(out);
                break;
            }
        }
        let ended = ended.expect("episode 0 ends");
        assert!(!ended.failed[0] && ended.winner[0] >= 0, "episode 0's own end: {ended:?}");
        let errors = pool.take_errors();
        assert_eq!((errors.len(), errors[0].episode), (1, 1), "{errors:?}");
        assert!(errors[0].message.starts_with("episode 1: team has 0"), "{errors:?}");
        let out = pool.step(vec![-1, -1], ActionMode::Choice).unwrap();
        assert_eq!((out.done[0], out.failed[0], out.winner[0]), (true, true, -1), "episode 1's end");
        assert_eq!(pool.take_errors()[0].episode, 2, "episode 2 can't start either");
        assert_eq!(pool.episodes(), [2]);
        pool.reset(None).unwrap();
        assert_eq!((pool.episodes(), pool.take_errors()[0].episode), (vec![3], 3));
    }

    /// A panic in the calling thread's `collect` doesn't leave workers
    /// running the call: it ran on every shard exactly once when it
    /// returns, and the next call runs cleanly.
    #[test]
    fn a_panic_while_collecting_waits_for_the_workers() {
        let pool = Pool::new(config(Format::gen9_singles(), 12), 4).unwrap();
        let job = |_: usize, envs: &mut [Env], _: &mut (), _: &PoolConfig| {
            for env in envs {
                // Slow enough that the workers are still busy when the
                // calling thread's collect panics.
                std::thread::sleep(Duration::from_millis(2));
                env.decisions += 1;
            }
            Ok(())
        };
        let mut calls = 0;
        let e = pool
            .run_shards(job, |_, _| {
                calls += 1;
                panic!("copy boom")
            })
            .unwrap_err();
        assert!(e.contains("panic while collecting the shards' outputs: copy boom"), "{e}");
        assert_eq!(calls, 1, "collecting stops at the panic");
        let decisions =
            |pool: &Pool| (0..12).map(|i| pool.with_env(i, |env| env.decisions).unwrap()).collect::<Vec<_>>();
        assert_eq!(decisions(&pool), [1; 12]);
        pool.run_shards(job, |_, _| {}).unwrap();
        assert_eq!(decisions(&pool), [2; 12]);
    }

    #[test]
    fn set_seed_is_the_seed_of_the_next_episodes() {
        let pool = Pool::new(config(Format::gen9_singles(), 2), 1).unwrap();
        assert_eq!(pool.seeds(), [episode_seed(11, 0), episode_seed(11, 1)]);
        pool.set_seed(100);
        assert_eq!(pool.config().seed, 100);
        // The episodes under way were made with the old seed.
        assert_eq!(pool.seeds(), [episode_seed(11, 0), episode_seed(11, 1)]);
        pool.reset(None).unwrap();
        assert_eq!((pool.episodes(), pool.seeds()), (vec![2, 3], vec![episode_seed(100, 2), episode_seed(100, 3)]));
        let battle_seed = pool.with_env(1, |env| env.battle.seed()).unwrap();
        assert_eq!(battle_seed, Seed::fast(episode_seed(100, 3)));
    }

    /// Random sets in every preset (trappers and Imprison come up): a
    /// random choice from the masks is submitted, or unavailable for what
    /// the request hid, never rejected; and every unavailable choice
    /// shrinks the side's mask.
    #[test]
    fn choices_from_the_masks_are_never_rejected_with_random_sets() {
        let mut unavailable = 0;
        for format in Format::presets() {
            for mode in [pikapro_teams::Mode::Sets, pikapro_teams::Mode::Uniform] {
                let g = crate::teams::generator(format, mode, Default::default()).unwrap();
                let c = PoolConfig { teams: Teams::Generator(g), seed: 6_200_100, ..config(format.clone(), 16) };
                let pool = Pool::new(c, 2).unwrap();
                let size = pool.space().size;
                for _ in 0..300 {
                    let masks = pool.masks(MaskKind::Choice).unwrap();
                    let actions = pool.sample_random(ActionMode::Choice).unwrap();
                    let out = pool.step(actions.clone(), ActionMode::Choice).unwrap();
                    assert!(!out.status.contains(&-1), "{} {mode:?} {:?}", format.id, out.status);
                    for (k, _) in out.status.iter().enumerate().filter(|(_, s)| **s == 2) {
                        unavailable += 1;
                        let after = pool.masks(MaskKind::Choice).unwrap();
                        let a = actions[k] as usize;
                        assert!(masks[k * size + a] && !after[k * size + a], "the refused choice leaves the mask");
                    }
                }
            }
        }
        assert!(unavailable > 0, "some hidden trap or disable came up");
    }

    /// A singles pool where p2's Wobbuffet traps p1 with Shadow Tag, which
    /// p1's request doesn't show.
    fn trapped_pool() -> Pool {
        use pikapro::PokemonSet;
        use pikapro_data::gen9::{abilities, moves, species};
        let format = Format::gen9_singles();
        let p1 = Team::new(vec![
            PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE]),
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE]),
        ]);
        let p2 = Team::new(vec![
            PokemonSet::new(species::WOBBUFFET).with_ability(abilities::SHADOW_TAG).with_moves(&[moves::SPLASH]),
        ]);
        let teams = Teams::Pairs(vec![Arc::new([TeamSheet::new(p1), TeamSheet::new(p2)])]);
        Pool::new(PoolConfig { teams, ..config(format, 1) }, 1).unwrap()
    }

    /// The masks show what the request shows: a hidden trap keeps the
    /// switch until a switch reveals it (status 2), then the mask drops it
    /// until the next request.
    #[test]
    fn masks_show_what_the_request_shows() {
        let pool = trapped_pool();
        let space = pool.space().clone();
        let switch = (space.switch_base() + 1) as i64;
        let out = pool.step(vec![0, 0], ActionMode::Choice).unwrap();
        assert_eq!(out.status, [1, 1]);
        // Turn 2: trapped, which p1's request doesn't show.
        let masks = pool.masks(MaskKind::Choice).unwrap();
        assert!(masks[switch as usize], "the request shows the switch");
        let choice = space.decode(switch as usize).unwrap();
        let engine = pool.with_env(0, |env| env.battle.legal_actions(SideId::P1).is_legal(&choice));
        assert_eq!(engine, Some(false), "the engine knows");
        let out = pool.step(vec![switch, 0], ActionMode::Choice).unwrap();
        assert_eq!(out.status, [2, 1], "unavailable; p2's choice stands");
        let masks = pool.masks(MaskKind::Choice).unwrap();
        assert!(!masks[switch as usize] && masks[0]);
        let slots = pool.masks(MaskKind::Slots).unwrap();
        assert!(!slots[switch as usize] && slots[0]);
        // Random choices come from the masks.
        for _ in 0..20 {
            let a = pool.sample_random(ActionMode::Choice).unwrap()[0];
            assert!(masks[a as usize] && a != switch, "{a}");
        }
        let out = pool.step(vec![0, 0], ActionMode::Choice).unwrap();
        assert_eq!(out.status, [1, 0]);
        // A new request: shown as issued again.
        assert!(pool.masks(MaskKind::Choice).unwrap()[switch as usize]);
    }

    /// Imprison in doubles: the last active Pokémon's mask shows the move
    /// until a choice of it is refused, which reveals every move it can't
    /// use (`updateDisabledRequest`).
    #[test]
    fn an_imprisoned_move_is_unavailable() {
        use pikapro::{PokemonSet, Target};
        use pikapro_data::gen9::{moves, species};
        let format = Format::gen9_doubles();
        let p1 = Team::new(vec![
            PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE, moves::SPLASH]),
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE, moves::SPLASH]),
        ]);
        let p2 = Team::new(vec![
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::IMPRISON, moves::TACKLE]),
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::SPLASH]),
        ]);
        let teams = Teams::Pairs(vec![Arc::new([TeamSheet::new(p1), TeamSheet::new(p2)])]);
        let pool = Pool::new(PoolConfig { teams, ..config(format, 1) }, 1).unwrap();
        let space = pool.space().clone();
        let slot = |index: u8, target: Option<Target>| {
            space.encode_slot(&SlotAction::Move { index, target, gimmick: None }).unwrap() as i64
        };
        let (splash, imprison, tackle) = (slot(1, None), slot(0, None), slot(0, Some(Target::Foe(0))));
        let pass = space.pass_index() as i64;
        // p2's Pokémon both use their first move (Imprison, Splash).
        let out = pool.step(vec![splash, splash, imprison, imprison], ActionMode::Slots).unwrap();
        assert_eq!(out.status, [1, 1]);
        let masks = pool.masks(MaskKind::Slots).unwrap();
        let slot1 = |m: &[bool], a: i64| m[space.slot_size + a as usize];
        assert!(!masks[tackle as usize] && slot1(&masks, tackle), "only the last active Pokémon's request hides it");
        let out = pool.step(vec![splash, tackle, imprison, imprison], ActionMode::Slots).unwrap();
        assert_eq!(out.status, [2, 1]);
        let masks = pool.masks(MaskKind::Slots).unwrap();
        assert!(!slot1(&masks, tackle) && slot1(&masks, splash));
        let out = pool.step(vec![splash, splash, pass, pass], ActionMode::Slots).unwrap();
        assert_eq!(out.status, [1, 0], "p2's choice stood");
    }
}
