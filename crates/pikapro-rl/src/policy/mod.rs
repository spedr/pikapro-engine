//! Native policies: the built-in opponents and yardsticks (poke-env's
//! three baseline players), playing from a [`PlayerView`].

use core::fmt;

use pikapro::{BattleRng, Choice, LegalActions};
use pikapro_view::PlayerView;

/// A player that runs in Rust, on the pool's threads: it chooses from what
/// its side knows ([`PlayerView`]) and the actions its request shows,
/// drawing any randomness from `rng` (its own stream, never a battle's), so
/// a policy replays deterministically.
///
/// In the vector environment ([`crate::env::VecEnv`]), `legal` is what
/// the side's request shows ([`pikapro::legal::shown_actions`]), not the
/// engine's truth: a Pokémon a foe's unseen Shadow Tag traps still lists
/// its switches (the request says `maybe_trapped`), and a move only a
/// foe's Imprison disables stays listed (`maybe_disabled`). If the policy
/// picks one, the battle refuses it as Showdown does (`[Unavailable
/// choice]`), the view learns the fact (`trapped`, the move `disabled`),
/// and the policy is asked again with the updated view and actions. A
/// choice `legal` doesn't hold is an error: the vector environment ends
/// the battle's episode ([`EnvEndReason::Error`](crate::vec::EnvEndReason::Error)),
/// and [`eval::play`](crate::eval::play) returns it.
pub trait Policy: Send + Sync + fmt::Debug {
    /// A choice for `view.me`'s current request, from `legal`. Called only
    /// when `legal` isn't waiting.
    fn choose(&self, view: &PlayerView<'_>, legal: &LegalActions, rng: &mut BattleRng) -> Choice;
}

/// Uniformly random legal choices ([`LegalActions::sample`]), as poke-env's
/// `RandomPlayer` and `pikapro bench`.
#[derive(Clone, Copy, Debug, Default)]
pub struct RandomPolicy;

impl Policy for RandomPolicy {
    fn choose(&self, _: &PlayerView<'_>, legal: &LegalActions, rng: &mut BattleRng) -> Choice {
        legal.sample(rng)
    }
}
