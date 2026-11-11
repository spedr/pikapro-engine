//! # pikapro-rl
//!
//! The reinforcement-learning layer on pikapro (see `docs/RL_GUIDE.md`):
//! an environment, not a bot. It answers "what happened, what can I do,
//! what did I get"; which features to encode, the network and the
//! algorithm belong to its user.
//!
//! * [`env`](mod@env): the vector environment ([`env::VecEnv`]): one fused call
//!   steps `n` battles and returns observations, masks, rewards and
//!   termination for both players;
//! * [`vec`](mod@vec): its contract: the output buffers ([`vec::StepOut`]),
//!   end reasons, mask layouts, opponents, team sources and modes;
//! * [`space`]: a format's fixed action space, masks over it and the
//!   pairing rules of factored doubles masks;
//! * [`pool`]: many battles stepped together on worker threads (the
//!   vector environment's engine, and a raw battle pool of its own);
//! * [`reward`]: the two reward presets; [`series`]: best-of series;
//! * [`encode`]: the [`encode::Basic`] vector and the [`encode::Encoder`]
//!   trait, for an observation of your own; [`tables`]: what an encoder
//!   reads of the format's dex;
//! * [`policy`]: native policies on the player view (the built-in
//!   opponents); [`baselines`]: poke-env's baseline players; [`eval`]: win
//!   rates and cross-evaluation;
//! * [`arrays`]: raw (omniscient) state rows, for analysis and tooling;
//! * [`teams`]: random teams per episode, team validation and
//!   tournament team pools;
//! * [`session`]: one player's side of a battle played over Showdown's
//!   player channel (a server), with the environment's observations and
//!   actions.
//!
//! Observations come from [`pikapro_view::PlayerView`], never from the
//! battle's state. No external dependencies.
//!
//! # Failures and the panic strategy
//!
//! A battle that fails while it is stepped (an engine panic, a panic in a
//! user [`Policy`](policy::Policy) or [`Encoder`](encode::Encoder), or an
//! episode that can't start) ends its episode alone
//! ([`vec::EnvEndReason::Error`]; [`env::VecEnv::take_errors`],
//! [`pool::Pool::take_errors`]), and the other battles go on. Catching a
//! panic needs the unwinding panic strategy: Rust's default
//! (`panic = "unwind"`), and the Python wheels' (the `release-py`
//! profile). A program built with `panic = "abort"` (this workspace's own
//! `release` profile does) aborts on the first panic instead.

pub mod arrays;
pub mod baselines;
pub mod encode;
pub mod env;
pub mod eval;
pub mod policy;
pub mod pool;
pub mod reward;
pub mod series;
pub mod session;
pub mod space;
pub mod tables;
pub mod teams;
pub mod vec;

/// The README's examples, run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
