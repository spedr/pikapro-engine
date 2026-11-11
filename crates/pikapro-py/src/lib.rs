//! # pikapro-py
//!
//! Python bindings for pikapro: the `pikapro` Python package (see
//! `README.md` next to this crate).
//!
//! The crate has two layers:
//!
//! * a pure-Rust layer, always built and tested by `cargo test`: typed
//!   values from engine types ([`value`]) and a battle with its team sheets
//!   ([`game`]) here, and from `pikapro-rl` (re-exported under the same
//!   paths) the action space ([`space`]), raw state rows ([`arrays`]),
//!   random teams ([`teams`]) and the multi-threaded battle pool
//!   ([`pool`]);
//! * the PyO3 module (behind the `python` feature), which maturin builds:
//!   the simulator (battles, typed events and requests, the battle pool),
//!   and `pikapro.rl`'s native side (the vector environment and the
//!   channel sessions).
//!   Without the feature nothing here needs Python, so `cargo test
//!   --workspace` and `cargo clippy --workspace` work on machines without
//!   a Python toolchain.

pub mod game;
pub mod owed;
pub mod value;
pub mod volatiles;

pub use pikapro_rl::{arrays, pool, space, teams};

#[cfg(feature = "python")]
mod python;
#[cfg(feature = "python")]
mod rl;
#[cfg(feature = "python")]
mod session;
#[cfg(feature = "python")]
mod view;
