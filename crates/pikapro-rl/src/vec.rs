//! The vector environment's contract: what one fused step returns, laid
//! out in flat buffers the caller can bind once, and the configuration
//! choices (opponents, team sources, reset and switch modes) around it.
//! [`crate::env::VecEnv`] implements it.
//!
//! A step is one request round for `n` battles: the players' actions go
//! in; observations, masks, rewards and termination come out, for both
//! players (self-play is one batched forward pass).
//!
//! # Players and sides
//!
//! Rows are by *player*: player 0 is the agent (its actions always come
//! from the caller), player 1 its opponent (a built-in [`Policy`] or the
//! caller, [`Opponent`]). Without side randomisation player `p` plays side
//! `p` (p1, p2); with it, [`StepOut::agent_side`] says which side player 0
//! plays in each battle's episode.
//!
//! # Observations
//!
//! An [`Encoder`] writes each player's observation, an `f32` vector, into
//! [`StepOut::obs`] ([`Observation`]: [`Basic`] by default, or the user's
//! own). Each player's observation is ego-centric: its own rows first.

use std::fmt;
use std::sync::Arc;

use pikapro::{BattleResult, BattleRng, EndReason, Format, Seed};
use pikapro_protocol::TeamSheet;

use crate::encode::{Basic, Encoder};
use crate::policy::{Policy, RandomPolicy};
use crate::series::SERIES_WIDTH;
use crate::space::ActionSpace;
use crate::teams;

/// Why an episode ended, as the `end_reason` buffer codes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum EnvEndReason {
    /// Not over.
    #[default]
    None = 0,
    /// A side has no Pokémon left: `terminated`.
    AllFainted = 1,
    /// The format's turn limit (Showdown's turn 1000 tie): `truncated`.
    TurnLimit = 2,
    /// A side forfeited: `terminated`.
    Forfeit = 3,
    /// The environment's own `max_turns` cut the episode: `truncated`.
    MaxTurns = 4,
    /// The battle failed: a panic (an engine bug, a user
    /// [`Policy`] or [`Encoder`]) or an error while stepping it. `truncated`,
    /// with no result (`winner` −1), no reward and zeroed observations;
    /// nobody is asked to act. The battle starts its next episode at the
    /// next call, whatever the autoreset mode, and the other battles go on.
    /// The message is in
    /// [`VecEnv::take_errors`](crate::env::VecEnv::take_errors), under the
    /// failed episode's number.
    ///
    /// An episode that fails to start is skipped. With next-step autoreset
    /// its row is this end. With same-step autoreset the step that ended
    /// the episode before keeps that one's end (reward, winner, final
    /// observation); only its new-episode rows (observation, masks,
    /// `needs_action`, `episode`) are the failed one's: zeros, nobody asked.
    /// Either way, as after any failure, the next call starts the next
    /// episode, whose first row has end reason 0.
    ///
    /// Catching a panic needs the unwinding panic strategy
    /// (`panic = "unwind"`, Rust's default and the Python wheels'); with
    /// `panic = "abort"` a panic ends the process.
    Error = 5,
    /// The Endless Battle Clause ended the battle (a win or a tie by
    /// Showdown's rule, in a format that has it): `terminated`.
    EndlessBattle = 6,
}

impl EnvEndReason {
    /// The code of an engine result.
    #[must_use]
    pub fn of(result: &BattleResult) -> EnvEndReason {
        match result.reason {
            EndReason::AllFainted => EnvEndReason::AllFainted,
            EndReason::TurnLimit => EnvEndReason::TurnLimit,
            EndReason::Forfeit => EnvEndReason::Forfeit,
            EndReason::EndlessBattle => EnvEndReason::EndlessBattle,
        }
    }

    /// The episode ended on its own terms (a win, a loss, or a tie by
    /// fainting): Gymnasium's `terminated`.
    pub const fn terminated(self) -> bool {
        matches!(self, EnvEndReason::AllFainted | EnvEndReason::Forfeit | EnvEndReason::EndlessBattle)
    }

    /// The episode was cut short by a limit: Gymnasium's `truncated`.
    pub const fn truncated(self) -> bool {
        matches!(self, EnvEndReason::TurnLimit | EnvEndReason::MaxTurns | EnvEndReason::Error)
    }

    /// The episode is over.
    pub const fn is_over(self) -> bool {
        !matches!(self, EnvEndReason::None)
    }
}

/// How action masks (and actions) are laid out per player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MaskLayout {
    /// Each active slot's own legal actions, then the team-preview orders:
    /// `active * slot_size + preview_size` values (`2 * 47 + 360` in VGC).
    /// Which pairs of slot actions are jointly legal (two Pokémon can't
    /// switch to the same one; only one can Mega Evolve or Terastallize; a
    /// forced switch with one replacement for two holes must use it)
    /// follows from [`StepOut::min_switches`] and the masks:
    /// [`ActionSpace::pair_legal`] and [`ActionSpace::pair_mask`]. Actions
    /// are `active` slot-action indices per player (at team preview, slot
    /// 0 holds the order's index in `0..preview_size`). The default.
    #[default]
    Factored,
    /// Every joint choice ([`ActionSpace::size`]: 2,569 in VGC), 5.7 times
    /// larger in VGC (23.5 in gen9doubles) and enumerated per step. Actions are one choice index per
    /// player (in `0..size`; team preview orders at `joint_size..`).
    Joint,
}

impl MaskLayout {
    /// Mask values per player.
    pub fn width(self, space: &ActionSpace) -> usize {
        match self {
            MaskLayout::Factored => space.active * space.slot_size + space.preview_size,
            MaskLayout::Joint => space.size,
        }
    }

    /// Action values per player.
    pub fn actions(self, space: &ActionSpace) -> usize {
        match self {
            MaskLayout::Factored => space.active,
            MaskLayout::Joint => 1,
        }
    }
}

/// What became of a player's action ([`StepOut::status`]).
pub mod status {
    /// Nothing was asked of this player (it waits, it is a built-in
    /// policy, or the step started a new episode): its action was ignored.
    pub const IGNORED: i8 = 0;
    /// Submitted.
    pub const SUBMITTED: i8 = 1;
    /// Rejected: outside the mask (or not an action). Nothing was
    /// submitted; the player must choose again.
    pub const REJECTED: i8 = -1;
    /// Unavailable: allowed by what the player's request shows, but the
    /// battle refused it because of something the request hides (a trap by
    /// a foe's unseen ability, a move only a foe's Imprison disables).
    /// Showdown answers `[Unavailable choice]` and updates the request; so
    /// does the environment: the player's view and mask now show the trap
    /// or the disabled move, and it must choose again.
    pub const UNAVAILABLE: i8 = 2;
}

/// An action value that lets the environment choose for the player: a
/// uniformly random choice among the ones its mask allows, from the
/// episode's own RNG (for tests, benchmarks and exploration). It is read
/// from the player's *first* action value (with factored masks, slot 0's;
/// the others are then ignored); elsewhere it is an action outside the
/// mask, rejected.
pub const RANDOM_ACTION: i64 = -2;

/// What writes each player's observation: an [`Encoder`], shared by the
/// worker threads. [`Basic`] by default.
#[derive(Clone)]
pub struct Observation(pub Arc<dyn Encoder>);

impl Observation {
    /// `encoder` as the observation.
    pub fn new(encoder: impl Encoder + 'static) -> Observation {
        Observation(Arc::new(encoder))
    }

    /// `f32` values per player in [`StepOut::obs`].
    pub fn size(&self) -> usize {
        self.0.size()
    }
}

impl Default for Observation {
    fn default() -> Observation {
        Observation::new(Basic)
    }
}

impl From<Arc<dyn Encoder>> for Observation {
    fn from(encoder: Arc<dyn Encoder>) -> Observation {
        Observation(encoder)
    }
}

impl core::ops::Deref for Observation {
    type Target = dyn Encoder;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl fmt::Debug for Observation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Observation({} values)", self.0.size())
    }
}

/// The shape of a step's buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepShape {
    /// Battles.
    pub n: usize,
    /// `f32` observation values per player (the encoder's size).
    pub obs: usize,
    /// Mask values per player ([`MaskLayout::width`]).
    pub mask: usize,
}

/// The terminal step's extra outputs with same-step autoreset
/// ([`Autoreset::SameStep`]): the ended episodes' last observations, which
/// the main buffers no longer hold. Rows of battles that didn't end this
/// step are left as they were.
#[derive(Debug)]
pub struct FinalOut<'a> {
    /// `(n, 2, obs)`: each player's terminal observation.
    pub obs: &'a mut [f32],
    /// `(n,)`: the ended episode.
    pub episode: &'a mut [u64],
    /// `(n,)`: the turns it lasted: the battle's turn when it ended, and
    /// `max_turns` for an episode that cut (end reason 4, which comes as
    /// turn `max_turns + 1` starts, before anyone acts in it).
    pub turns: &'a mut [u16],
    /// `(n, 4)`: the ended game's series, as [`StepOut::series`] (series
    /// mode only).
    pub series: Option<&'a mut [u8]>,
}

/// One fused step's outputs, as mutable views of buffers the caller owns
/// (numpy arrays, pinned torch tensors bound once): the step writes every
/// value of the main buffers, whatever they held; the final ones
/// ([`FinalOut`]) only for the battles that ended. All are batch-major and
/// C-contiguous, and by player (see the module docs).
///
/// With [`Autoreset::NextStep`] the step that ends a battle returns its
/// terminal observation; the next step starts the next episode there (and
/// ignores that battle's actions). With [`Autoreset::SameStep`] it starts
/// the next episode at once: `obs`, `masks`, `needs_action`, `min_switches`,
/// `agent_side` and `episode` describe the new one, `reward`, `terminated`,
/// `truncated`, `end_reason` and `winner` the one that ended, whose last
/// observation is in [`StepOut::final_out`].
#[derive(Debug)]
pub struct StepOut<'a> {
    /// `(n, 2, obs)`: each player's encoded [`PlayerView`](pikapro_view::PlayerView).
    /// A built-in opponent's rows are zeros unless the environment is
    /// asked to encode it.
    pub obs: &'a mut [f32],
    /// `(n, 2, mask)`: each player's legal actions ([`MaskLayout`]), as
    /// its request shows them; all false for a player that isn't asked
    /// anything.
    pub masks: &'a mut [bool],
    /// `(n, 2)`: the fewest switches the player's joint action must make:
    /// in a forced switch, the slots to fill, up to the Pokémon left to
    /// fill them; 0 otherwise. The masks already enforce it except when
    /// two holes have one replacement: either slot may take it, but one
    /// must ([`ActionSpace::pair_legal`]).
    pub min_switches: &'a mut [u8],
    /// `(n, 2)`: the reward each player earned since its previous decision
    /// ([`RewardPreset`](crate::reward::RewardPreset)): paid when it must
    /// act again or the episode ends, 0 in the steps between. Zero-sum
    /// over an episode that ends on its own or by a limit. An episode cut
    /// by [`VecEnv::reset`](crate::env::VecEnv::reset) or a failure
    /// ([`EnvEndReason::Error`]) drops what was pending.
    pub reward: &'a mut [f32],
    /// `(n,)`: the episode ended this step on its own terms
    /// ([`EnvEndReason::terminated`]).
    pub terminated: &'a mut [bool],
    /// `(n,)`: the episode was cut short this step
    /// ([`EnvEndReason::truncated`]).
    pub truncated: &'a mut [bool],
    /// `(n,)`: an [`EnvEndReason`] code, 0 while the episode goes on.
    pub end_reason: &'a mut [u8],
    /// `(n,)`: `-1` no result, `0` or `1` the winning *player*, `2` a tie
    /// (a turn-limit end is a tie with `end_reason` 2, `truncated`; an
    /// environment `max_turns` cut or a failure has no result: `-1`,
    /// `end_reason` 4 or 5).
    pub winner: &'a mut [i8],
    /// `(n, 2)`: this player must act next step. A forced switch asks only
    /// the sides with a Pokémon to replace (one or both); a player whose
    /// actions come from a built-in policy is never asked.
    pub needs_action: &'a mut [bool],
    /// `(n, 2)`: what became of this step's actions ([`status`]).
    pub status: &'a mut [i8],
    /// `(n,)`: the battle side (0 p1, 1 p2) player 0 plays.
    pub agent_side: &'a mut [u8],
    /// `(n,)`: the episode `obs` belongs to (episode `e` of battle `i` has
    /// `e % n == i`; its battle seed is
    /// [`episode_seed`](crate::env::episode_seed)`(base, e)`).
    pub episode: &'a mut [u64],
    /// `(n, 4)`, in series mode ([`crate::series`]): the series of the
    /// episode `obs` belongs to: its game (1-based), player 0's wins,
    /// player 1's wins and the [`SeriesResult`](crate::series::SeriesResult)
    /// code (0 while it goes on), counting the game that ended this step.
    /// One row per battle, in player 0's frame (the agent's; what
    /// [`ChannelSession::series_row`](crate::session::ChannelSession::series_row)
    /// gives a player for itself): player 1 swaps the wins and the result.
    pub series: Option<&'a mut [u8]>,
    /// Same-step autoreset's terminal observations (required then, unused
    /// otherwise).
    pub final_out: Option<FinalOut<'a>>,
}

impl<'a> FinalOut<'a> {
    /// The first `m` battles' part, and the rest, for `shape`.
    pub fn split_battles(self, m: usize, shape: StepShape) -> (FinalOut<'a>, FinalOut<'a>) {
        let (obs, obs2) = self.obs.split_at_mut(m * 2 * shape.obs);
        let (episode, episode2) = self.episode.split_at_mut(m);
        let (turns, turns2) = self.turns.split_at_mut(m);
        let (series, series2) = self.series.map(|s| s.split_at_mut(m * SERIES_WIDTH)).unzip();
        (
            FinalOut { obs, episode, turns, series },
            FinalOut { obs: obs2, episode: episode2, turns: turns2, series: series2 },
        )
    }
}

impl<'a> StepOut<'a> {
    /// The first `m` battles' part (the buffers of battles `0..m`) and the
    /// rest (whose battle 0 is battle `m`), for `shape`: to hand parts to
    /// several threads.
    #[must_use]
    pub fn split_battles(self, m: usize, shape: StepShape) -> (StepOut<'a>, StepOut<'a>) {
        let (obs, obs2) = self.obs.split_at_mut(m * 2 * shape.obs);
        let (masks, masks2) = self.masks.split_at_mut(m * 2 * shape.mask);
        let (min_switches, min_switches2) = self.min_switches.split_at_mut(m * 2);
        let (reward, reward2) = self.reward.split_at_mut(m * 2);
        let (terminated, terminated2) = self.terminated.split_at_mut(m);
        let (truncated, truncated2) = self.truncated.split_at_mut(m);
        let (end_reason, end_reason2) = self.end_reason.split_at_mut(m);
        let (winner, winner2) = self.winner.split_at_mut(m);
        let (needs_action, needs_action2) = self.needs_action.split_at_mut(m * 2);
        let (status, status2) = self.status.split_at_mut(m * 2);
        let (agent_side, agent_side2) = self.agent_side.split_at_mut(m);
        let (episode, episode2) = self.episode.split_at_mut(m);
        let (series, series2) = self.series.map(|s| s.split_at_mut(m * SERIES_WIDTH)).unzip();
        let (final_out, final_out2) = self.final_out.map(|f| f.split_battles(m, shape)).unzip();
        (
            StepOut {
                obs,
                masks,
                min_switches,
                reward,
                terminated,
                truncated,
                end_reason,
                winner,
                needs_action,
                status,
                agent_side,
                episode,
                series,
                final_out,
            },
            StepOut {
                obs: obs2,
                masks: masks2,
                min_switches: min_switches2,
                reward: reward2,
                terminated: terminated2,
                truncated: truncated2,
                end_reason: end_reason2,
                winner: winner2,
                needs_action: needs_action2,
                status: status2,
                agent_side: agent_side2,
                episode: episode2,
                series: series2,
                final_out: final_out2,
            },
        )
    }

    /// The buffers, borrowed again (for [`StepOut::split_battles`]).
    #[must_use]
    pub fn reborrow(&mut self) -> StepOut<'_> {
        StepOut {
            obs: &mut *self.obs,
            masks: &mut *self.masks,
            min_switches: &mut *self.min_switches,
            reward: &mut *self.reward,
            terminated: &mut *self.terminated,
            truncated: &mut *self.truncated,
            end_reason: &mut *self.end_reason,
            winner: &mut *self.winner,
            needs_action: &mut *self.needs_action,
            status: &mut *self.status,
            agent_side: &mut *self.agent_side,
            episode: &mut *self.episode,
            series: self.series.as_deref_mut(),
            final_out: self.final_out.as_mut().map(|f| FinalOut {
                obs: &mut *f.obs,
                episode: &mut *f.episode,
                turns: &mut *f.turns,
                series: f.series.as_deref_mut(),
            }),
        }
    }

    /// Bytes a step writes into buffers of `shape` per battle, but for the
    /// final ones (only ended battles get those).
    pub fn bytes_per_battle(shape: StepShape) -> usize {
        // obs, masks, min_switches, reward, terminated, truncated,
        // end_reason, winner, needs_action, status, agent_side, episode.
        2 * 4 * shape.obs + 2 * shape.mask + 2 + 2 * 4 + 1 + 1 + 1 + 1 + 2 + 2 + 1 + 8
    }
}

impl StepOut<'_> {
    /// Checks that every buffer has `shape`'s length.
    pub fn check(&self, shape: StepShape) -> Result<(), String> {
        let n = shape.n;
        // Each length in turn (the first that is wrong is the error); a
        // name is only formatted for the error.
        check_len("", "obs", self.obs.len(), n * 2 * shape.obs)?;
        check_len("", "masks", self.masks.len(), n * 2 * shape.mask)?;
        check_len("", "min_switches", self.min_switches.len(), n * 2)?;
        check_len("", "reward", self.reward.len(), n * 2)?;
        check_len("", "terminated", self.terminated.len(), n)?;
        check_len("", "truncated", self.truncated.len(), n)?;
        check_len("", "end_reason", self.end_reason.len(), n)?;
        check_len("", "winner", self.winner.len(), n)?;
        check_len("", "needs_action", self.needs_action.len(), n * 2)?;
        check_len("", "status", self.status.len(), n * 2)?;
        check_len("", "agent_side", self.agent_side.len(), n)?;
        check_len("", "episode", self.episode.len(), n)?;
        if let Some(s) = &self.series {
            check_len("", "series", s.len(), n * SERIES_WIDTH)?;
        }
        if let Some(f) = &self.final_out {
            check_len("", "final_obs", f.obs.len(), n * 2 * shape.obs)?;
            check_len("", "final_episode", f.episode.len(), n)?;
            check_len("", "final_turns", f.turns.len(), n)?;
            if let Some(s) = &f.series {
                check_len("", "final_series", s.len(), n * SERIES_WIDTH)?;
            }
        }
        Ok(())
    }
}

/// A buffer `prefix` `name` of `got` values where `want` are expected:
/// [`StepOut::check`]'s error.
fn check_len(prefix: &str, name: &str, got: usize, want: usize) -> Result<(), String> {
    if got == want { Ok(()) } else { Err(format!("{prefix}{name}: {got} values, expected {want}")) }
}

/// Owned [`StepOut`] buffers, for Rust callers and per-shard scratch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepBuffers {
    pub shape: Option<StepShape>,
    pub obs: Vec<f32>,
    pub masks: Vec<bool>,
    pub min_switches: Vec<u8>,
    pub reward: Vec<f32>,
    pub terminated: Vec<bool>,
    pub truncated: Vec<bool>,
    pub end_reason: Vec<u8>,
    pub winner: Vec<i8>,
    pub needs_action: Vec<bool>,
    pub status: Vec<i8>,
    pub agent_side: Vec<u8>,
    pub episode: Vec<u64>,
    /// Empty unless made [`with`](Self::with) series buffers.
    pub series: Vec<u8>,
    /// [`FinalOut`]'s buffers, when made [`with`](Self::with) `final_out`.
    pub final_obs: Vec<f32>,
    pub final_episode: Vec<u64>,
    pub final_turns: Vec<u16>,
    pub final_series: Vec<u8>,
    has_final: bool,
    has_series: bool,
}

fn fit<T: Copy + Default>(v: &mut Vec<T>, len: usize) {
    v.clear();
    v.resize(len, T::default());
}

impl StepBuffers {
    pub fn new(shape: StepShape) -> StepBuffers {
        let mut b = StepBuffers::default();
        b.resize(shape);
        b
    }

    /// Buffers with [`FinalOut`]'s (`final_out`) and the series ones
    /// (`series`) as asked.
    pub fn with(shape: StepShape, final_out: bool, series: bool) -> StepBuffers {
        let mut b = StepBuffers { has_final: final_out, has_series: series, ..StepBuffers::default() };
        b.resize(shape);
        b
    }

    /// Resizes (and zeroes) every buffer for `shape`, keeping capacity.
    pub fn resize(&mut self, shape: StepShape) {
        let n = shape.n;
        fit(&mut self.obs, n * 2 * shape.obs);
        fit(&mut self.masks, n * 2 * shape.mask);
        fit(&mut self.min_switches, n * 2);
        fit(&mut self.reward, n * 2);
        fit(&mut self.terminated, n);
        fit(&mut self.truncated, n);
        fit(&mut self.end_reason, n);
        fit(&mut self.winner, n);
        fit(&mut self.needs_action, n * 2);
        fit(&mut self.status, n * 2);
        fit(&mut self.agent_side, n);
        fit(&mut self.episode, n);
        let (f, s) = (usize::from(self.has_final), usize::from(self.has_series));
        fit(&mut self.series, s * n * SERIES_WIDTH);
        fit(&mut self.final_obs, f * n * 2 * shape.obs);
        fit(&mut self.final_episode, f * n);
        fit(&mut self.final_turns, f * n);
        fit(&mut self.final_series, f * s * n * SERIES_WIDTH);
        self.shape = Some(shape);
    }

    pub fn as_out(&mut self) -> StepOut<'_> {
        let has_series = self.has_series;
        let final_out = self.has_final.then(|| FinalOut {
            obs: &mut self.final_obs,
            episode: &mut self.final_episode,
            turns: &mut self.final_turns,
            series: has_series.then_some(&mut self.final_series[..]),
        });
        StepOut {
            obs: &mut self.obs,
            masks: &mut self.masks,
            min_switches: &mut self.min_switches,
            reward: &mut self.reward,
            terminated: &mut self.terminated,
            truncated: &mut self.truncated,
            end_reason: &mut self.end_reason,
            winner: &mut self.winner,
            needs_action: &mut self.needs_action,
            status: &mut self.status,
            agent_side: &mut self.agent_side,
            episode: &mut self.episode,
            series: has_series.then_some(&mut self.series[..]),
            final_out,
        }
    }

    /// Copies these buffers, for the battles `start..start + m`, into
    /// `out` (for all `n`): a shard's part of a step. With `ended`, the
    /// final buffers too, only for the battles `ended` flags (by index in
    /// these buffers). Panics if `out` is too small.
    pub(crate) fn copy_into(&self, start: usize, out: &mut StepOut<'_>, ended: Option<&[bool]>) {
        let Some(shape) = self.shape else { return };
        fn put<T: Copy>(dst: &mut [T], src: &[T], start: usize, per: usize) {
            dst[start * per..][..src.len()].copy_from_slice(src);
        }
        put(out.obs, &self.obs, start, 2 * shape.obs);
        put(out.masks, &self.masks, start, 2 * shape.mask);
        put(out.min_switches, &self.min_switches, start, 2);
        put(out.reward, &self.reward, start, 2);
        put(out.terminated, &self.terminated, start, 1);
        put(out.truncated, &self.truncated, start, 1);
        put(out.end_reason, &self.end_reason, start, 1);
        put(out.winner, &self.winner, start, 1);
        put(out.needs_action, &self.needs_action, start, 2);
        put(out.status, &self.status, start, 2);
        put(out.agent_side, &self.agent_side, start, 1);
        put(out.episode, &self.episode, start, 1);
        if let (Some(s), true) = (&mut out.series, self.has_series) {
            put(s, &self.series, start, SERIES_WIDTH);
        }
        if let (Some(f), Some(ended), true) = (&mut out.final_out, ended, self.has_final) {
            let obs = 2 * shape.obs;
            for (j, _) in ended.iter().enumerate().filter(|(_, e)| **e) {
                let i = start + j;
                f.obs[i * obs..][..obs].copy_from_slice(&self.final_obs[j * obs..][..obs]);
                f.episode[i] = self.final_episode[j];
                f.turns[i] = self.final_turns[j];
                if let (Some(s), true) = (&mut f.series, self.has_series) {
                    s[i * SERIES_WIDTH..][..SERIES_WIDTH]
                        .copy_from_slice(&self.final_series[j * SERIES_WIDTH..][..SERIES_WIDTH]);
                }
            }
        }
    }
}

/// Who plays player 1 in an episode (player 0's actions always come from
/// the caller). Can change between calls, taking effect from each battle's
/// next episode, so a curriculum (heuristics first, then self-play) is a
/// few lines of the user's code.
#[derive(Clone)]
#[non_exhaustive]
pub enum Opponent {
    /// A policy running on the workers, on its side's
    /// [`PlayerView`](pikapro_view::PlayerView), with a per-episode RNG:
    /// poke-env's baselines ([`Opponent::random`],
    /// [`Opponent::max_base_power`], [`Opponent::simple_heuristics`]), or
    /// any [`Policy`]. A policy that panics ends the episode
    /// ([`EnvEndReason::Error`]; this needs `panic = "unwind"`).
    Policy(Arc<dyn Policy>),
    /// The actions come from the caller (self-play, a frozen checkpoint).
    External,
}

impl Opponent {
    /// Uniformly random legal choices (poke-env's `RandomPlayer`).
    pub fn random() -> Opponent {
        Opponent::Policy(Arc::new(RandomPolicy))
    }

    /// The move with the highest base power (poke-env's
    /// `MaxBasePowerPlayer`, [`MaxBasePowerPolicy`](crate::baselines::MaxBasePowerPolicy)).
    pub fn max_base_power() -> Opponent {
        Opponent::Policy(Arc::new(crate::baselines::MaxBasePowerPolicy))
    }

    /// poke-env's `SimpleHeuristicsPlayer`
    /// ([`SimpleHeuristicsPolicy`](crate::baselines::SimpleHeuristicsPolicy)).
    pub fn simple_heuristics() -> Opponent {
        Opponent::Policy(Arc::new(crate::baselines::SimpleHeuristicsPolicy::new()))
    }

    /// Whether the caller supplies the actions.
    pub fn is_external(&self) -> bool {
        matches!(self, Opponent::External)
    }
}

impl Default for Opponent {
    fn default() -> Opponent {
        Opponent::random()
    }
}

impl fmt::Debug for Opponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Opponent::Policy(p) => f.debug_tuple("Policy").field(p).finish(),
            Opponent::External => f.write_str("External"),
        }
    }
}

/// When a battle that ended starts its next episode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Autoreset {
    /// Gymnasium 1.x's `NEXT_STEP`: the step that ends an episode returns
    /// its terminal observation; the next step starts the next episode and
    /// ignores the battle's actions. The default (native and Gymnasium
    /// APIs).
    #[default]
    NextStep,
    /// The step that ends an episode starts the next one at once and
    /// returns its first observation; the terminal one goes to
    /// [`FinalOut`] (Stable-Baselines3's `VecEnv`).
    SameStep,
}

/// What a switch action's index means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SwitchBy {
    /// `switch k` sends in the player's Pokémon in observation row `k`:
    /// its own Pokémon with identity `k` (the team sheet's order,
    /// [`OwnMon::id`](pikapro_view::OwnMon::id)), stable for the whole battle.
    /// The environment maps it to the current team position. The default.
    #[default]
    Row,
    /// `switch k` sends in the Pokémon at current team position `k`, as
    /// Showdown's `switch k+1` does (positions change as Pokémon switch).
    Position,
}

/// How a caller-controlled player answers team preview.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PreviewMode {
    /// The player chooses: its mask shows the orders (360 in VGC). The
    /// default.
    #[default]
    Learned,
    /// The environment picks an order uniformly at random, from the
    /// episode's RNG; the player never sees team preview.
    Random,
    /// The environment brings the first Pokémon of the team, in order
    /// (Showdown's `team 1234`); the player never sees team preview.
    Fixed,
}

/// Mixed into an episode's battle seed for a team pool's draws.
const POOL_SEED: u64 = 0x9001_7EA5_B00C_5EED;

/// Where each episode's teams come from, by player (player 0's first). No
/// per-episode callbacks: every source is data, so episodes replay from
/// their seed.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Teams {
    /// Episode `e` plays `pairs[e % pairs.len()]` (one pair: fixed teams).
    Pairs(Vec<Arc<[TeamSheet; 2]>>),
    /// Each player draws uniformly from its own pool, from the episode's
    /// seed: a tournament team pool ([`teams::import_pool`]), poke-env
    /// teambuilders precomputed, or any list. The two may be the same.
    PerSide([Arc<[TeamSheet]>; 2]),
    /// Random teams (`pikapro-teams`): the episode with battle seed `s`
    /// plays [`teams::pair`]`(generator, s)`. The generator must be for
    /// the format.
    Generator(Arc<pikapro_teams::TeamGenerator>),
}

impl Teams {
    /// Both players' teams for episode `episode`, whose battle seed is
    /// `seed`. An error for an empty source.
    pub fn teams(&self, episode: u64, seed: u64) -> Result<Arc<[TeamSheet; 2]>, String> {
        match self {
            Teams::Pairs(pairs) if pairs.is_empty() => Err("no teams".into()),
            Teams::Pairs(pairs) => Ok(pairs[(episode % pairs.len() as u64) as usize].clone()),
            Teams::PerSide(pools) if pools.iter().any(|p| p.is_empty()) => Err("an empty team pool".into()),
            Teams::PerSide(pools) => {
                let mut rng = BattleRng::new(Seed::fast(seed ^ POOL_SEED));
                let mut draw = |pool: &[TeamSheet]| pool[rng.random(pool.len() as u32) as usize].clone();
                Ok(Arc::new([draw(&pools[0]), draw(&pools[1])]))
            }
            Teams::Generator(g) => teams::pair(g, seed).map(Arc::new).map_err(|e| e.to_string()),
        }
    }

    /// Whether this source can play `format`: every team of a list or a
    /// pool passes the engine's structural check for it
    /// ([`Team::validate`](pikapro::Team::validate), what starting a
    /// battle checks), so whether a source is accepted doesn't depend on
    /// which teams a seed draws; a generator must be for the format. The
    /// error names the first team that fails.
    pub fn check(&self, format: &Format) -> Result<(), String> {
        let valid = |sheet: &TeamSheet, which: &dyn Fn() -> String| {
            sheet.team.validate(format).map_err(|e| format!("{}: {e}", which()))
        };
        match self {
            Teams::Pairs(p) if p.is_empty() => Err("no teams".into()),
            Teams::Pairs(pairs) => {
                for (k, pair) in pairs.iter().enumerate() {
                    for (p, sheet) in pair.iter().enumerate() {
                        valid(sheet, &|| format!("pair {k}, player {p}'s team"))?;
                    }
                }
                Ok(())
            }
            Teams::PerSide(pools) if pools.iter().any(|p| p.is_empty()) => Err("an empty team pool".into()),
            Teams::PerSide(pools) if pools.iter().any(|p| p.len() > u32::MAX as usize) => {
                Err("a team pool too large".into())
            }
            Teams::PerSide(pools) => {
                for (p, pool) in pools.iter().enumerate() {
                    // A pool both players share is checked once.
                    if p == 1 && Arc::ptr_eq(&pools[0], pool) {
                        break;
                    }
                    for (k, sheet) in pool.iter().enumerate() {
                        valid(sheet, &|| format!("player {p}'s pool, team {k}"))?;
                    }
                }
                Ok(())
            }
            Teams::Generator(g) if !teams::same_rules(g.format(), format) => {
                Err(format!("teams for {} in a {} pool (or its rules differ)", g.format().id, format.id))
            }
            Teams::Generator(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_reasons_split_termination_from_truncation() {
        use EnvEndReason as R;
        for (r, code, term, trunc) in [
            (R::None, 0, false, false),
            (R::AllFainted, 1, true, false),
            (R::TurnLimit, 2, false, true),
            (R::Forfeit, 3, true, false),
            (R::MaxTurns, 4, false, true),
            (R::Error, 5, false, true),
            (R::EndlessBattle, 6, true, false),
        ] {
            assert_eq!((r as u8, r.terminated(), r.truncated()), (code, term, trunc));
        }
    }

    #[test]
    fn buffers_have_the_shape() {
        let space = ActionSpace::new(&Format::champions_vgc());
        assert_eq!(MaskLayout::Factored.width(&space), 2 * 47 + 360);
        assert_eq!(MaskLayout::Joint.width(&space), 47 * 47 + 360);
        let shape = StepShape { n: 3, obs: 5, mask: MaskLayout::Factored.width(&space) };
        let mut all = StepBuffers::with(shape, true, false);
        assert_eq!(all.as_out().check(shape), Ok(()));
        let mut part = StepBuffers::with(StepShape { n: 1, ..shape }, true, false);
        part.obs.fill(1.0);
        part.final_obs.fill(2.0);
        part.winner[0] = 1;
        part.copy_into(2, &mut all.as_out(), Some(&[true]));
        assert!(all.obs[..2 * 2 * 5].iter().all(|&x| x == 0.0) && all.obs[2 * 2 * 5..].iter().all(|&x| x == 1.0));
        assert!(all.final_obs[2 * 2 * 5..].iter().all(|&x| x == 2.0));
        assert_eq!(all.winner, [0, 0, 1]);
        assert!(StepBuffers::new(StepShape { n: 2, ..shape }).as_out().check(shape).is_err());
        assert!(StepBuffers::new(shape).as_out().final_out.is_none());
    }

    #[test]
    fn team_pools_draw_from_the_seed() {
        use pikapro::testing::sample_team;
        let format = Format::gen9_singles();
        let a: Arc<[TeamSheet]> = vec![TeamSheet::new(sample_team(&format)); 3].into();
        let teams = Teams::PerSide([a.clone(), a]);
        assert_eq!(teams.teams(0, 5).unwrap(), teams.teams(9, 5).unwrap());
        assert!(teams.check(&format).is_ok());
        assert!(Teams::PerSide([Arc::from(vec![]), Arc::from(vec![])]).check(&format).is_err());
    }
}
