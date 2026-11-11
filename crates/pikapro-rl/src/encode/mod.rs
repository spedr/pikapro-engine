//! Turning a [`PlayerView`] into numbers, in Rust, with no Python per
//! battle per step:
//!
//! - [`Basic`]: one small `f32` vector for a plain network. The default
//!   observation, and the example to copy.
//! - The [`Encoder`] trait, for code of your own, with the resolved view
//!   ([`ViewCtx`], [`MonRef`]) to read.
//!
//! Which features to encode is the modeler's choice.

pub mod basic;
pub mod ctx;

use std::sync::Arc;

pub use basic::Basic;
pub use ctx::{MonRef, MoveInfo, MoveKnowledge, Types, ViewCtx};

use pikapro_view::PlayerView;

use crate::tables::Tables;

/// Encodes a player's view into a flat `f32` buffer.
///
/// It takes a [`PlayerView`], never a `Battle`: an encoder can't leak
/// hidden information by construction. It runs on the pool's worker
/// threads, once per side per step, so it must be deterministic and
/// shouldn't allocate.
///
/// To write one, resolve the view with [`ViewCtx::new`] and read its rows
/// ([`MonRef`]'s helpers give HP, types, moves and items as the player
/// knows them); [`Basic`] is a short example.
pub trait Encoder: Send + Sync {
    /// Values written per side: the length of `out` in [`Encoder::encode`].
    fn size(&self) -> usize;

    /// Writes `view` into `out` (exactly [`Encoder::size`] values). Every
    /// value must be written: buffers are reused across steps without
    /// clearing.
    fn encode(&self, view: &PlayerView<'_>, tables: &Tables, out: &mut [f32]);
}

impl<E: Encoder + ?Sized> Encoder for Arc<E> {
    fn size(&self) -> usize {
        (**self).size()
    }

    fn encode(&self, view: &PlayerView<'_>, tables: &Tables, out: &mut [f32]) {
        (**self).encode(view, tables, out)
    }
}

impl<E: Encoder + ?Sized> Encoder for Box<E> {
    fn size(&self) -> usize {
        (**self).size()
    }

    fn encode(&self, view: &PlayerView<'_>, tables: &Tables, out: &mut [f32]) {
        (**self).encode(view, tables, out)
    }
}

/// Writes a one-hot: `out[k] = 1` (nothing if `k` is out of range).
#[inline]
pub fn one_hot(out: &mut [f32], k: usize) {
    if let Some(x) = out.get_mut(k) {
        *x = 1.0;
    }
}

/// `1.0` or `0.0`.
#[inline]
pub fn flag(b: bool) -> f32 {
    f32::from(u8::from(b))
}

#[cfg(test)]
mod tests;
