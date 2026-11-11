//! What a player submits in answer to a [`Request`](crate::Request).

use core::fmt;

use crate::format::Gimmick;
use crate::state::{MAX_ACTIVE, MAX_TEAM};
use crate::util::StackVec;

/// A move target relative to the user.
///
/// Showdown encodes these as integers: `Foe(n)` is `n+1`, `Ally(n)` is
/// `-(n+1)` (where `Ally` includes the user's own slot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The foe in slot `n`.
    Foe(u8),
    /// The Pokémon in slot `n` on the user's side (possibly the user).
    Ally(u8),
}

/// The action for one active slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SlotAction {
    /// Use the move in move slot `index` (0-based).
    Move {
        /// The move slot, 0-based (the order of the set's moves).
        index: u8,
        /// For single-target moves in doubles, `None` otherwise. `None`
        /// for such a move is Showdown's `autoChoose` (`default`): a random
        /// target (the legal-action lists always give one).
        target: Option<Target>,
        /// Terastallize or Mega Evolve before moving (Showdown's `terastallize`
        /// or `mega` after the move), if the format and the Pokémon allow it.
        gimmick: Option<Gimmick>,
    },
    /// Switch to the Pokémon at this position in the side's *current* team
    /// order (positions change on every switch; see
    /// [`SideState`](crate::SideState)).
    Switch(u8),
    /// No action: an empty/fainted slot, or a forced switch with nothing to
    /// switch to.
    #[default]
    Pass,
}

/// Team-preview order: team indices, the first `bring` of which are brought.
pub type TeamOrder = StackVec<u8, MAX_TEAM>;

/// A side's answer to its current [`Request`](crate::Request), for
/// [`Battle::submit`](crate::Battle::submit).
///
/// Build one from [`LegalActions`](crate::LegalActions) (its lists, or
/// [`sample`](crate::LegalActions::sample)), or directly: a choice that
/// isn't legal is rejected with a [`ChoiceError`] and changes nothing.
/// `pikapro-protocol` reads and writes Showdown's choice strings
/// (`move 1 2, switch 3`, `team 1234`).
///
/// ```
/// use pikapro::{Choice, SlotAction, Target};
/// // Singles: the first move.
/// let a = Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None });
/// // Doubles: slot 0 uses its second move on the foe in slot 1; slot 1
/// // switches to the Pokémon now third in the team order.
/// let hit = SlotAction::Move { index: 1, target: Some(Target::Foe(1)), gimmick: None };
/// let b = Choice::doubles(hit, SlotAction::Switch(2));
/// // Team preview: bring the fourth, first, second and third Pokémon.
/// let c = Choice::team(&[3, 0, 1, 2]);
/// # let _ = (a, b, c);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Choice {
    /// Answer to [`Request::TeamPreview`](crate::Request::TeamPreview).
    Team(TeamOrder),
    /// One action per active slot; unused slots must be [`SlotAction::Pass`].
    Actions([SlotAction; MAX_ACTIVE]),
}

impl Choice {
    /// A singles choice: `action` for the one active slot.
    pub fn single(action: SlotAction) -> Choice {
        Choice::Actions([action, SlotAction::Pass])
    }

    /// A doubles choice: `a` for slot 0 (the left), `b` for slot 1.
    pub fn doubles(a: SlotAction, b: SlotAction) -> Choice {
        Choice::Actions([a, b])
    }

    /// A team-preview answer: team indices (registration order, from 0), the
    /// first [`bring`](crate::Request::TeamPreview) of which are brought,
    /// in that order (Showdown's `team 4123`, 1-based). It needs at least
    /// `bring` distinct indices and at most the whole team.
    ///
    /// # Panics
    ///
    /// If `order` has more than six entries ([`MAX_TEAM`]).
    pub fn team(order: &[u8]) -> Choice {
        assert!(
            order.len() <= crate::MAX_TEAM,
            "a team order has at most {} entries, got {}",
            crate::MAX_TEAM,
            order.len()
        );
        Choice::Team(order.iter().copied().collect())
    }
}

/// Why a choice was rejected. A rejected replacement clears the previous
/// answer; the simulated battle state is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChoiceError {
    /// The battle has ended.
    BattleOver,
    /// This side has nothing to decide right now.
    NotRequested,
    /// Reserved for clients that disallow replacing an answer.
    /// [`Battle::submit`](crate::Battle::submit) accepts replacements.
    AlreadySubmitted,
    /// E.g. move actions sent during team preview.
    WrongKind,
    /// The action in `slot` is not legal.
    IllegalAction {
        /// The active slot, 0-based.
        slot: u8,
    },
    /// Two slots switch to the same Pokémon.
    DuplicateSwitch,
    /// The same gimmick used by more than one slot in the same turn.
    MultipleGimmicks,
    /// A slot passed while it was required to act.
    MustAct {
        /// The active slot, 0-based.
        slot: u8,
    },
    /// Team order has the wrong length, a duplicate or an out-of-range index.
    InvalidTeamOrder,
}

impl fmt::Display for ChoiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChoiceError::BattleOver => f.write_str("the battle is over"),
            ChoiceError::NotRequested => f.write_str("no decision is pending for this side"),
            ChoiceError::AlreadySubmitted => f.write_str("a choice was already submitted"),
            ChoiceError::WrongKind => f.write_str("choice does not match the pending request"),
            ChoiceError::IllegalAction { slot } => write!(f, "illegal action for slot {slot}"),
            ChoiceError::DuplicateSwitch => f.write_str("two slots switch to the same Pokémon"),
            ChoiceError::MultipleGimmicks => f.write_str("the same gimmick may only be used once per turn"),
            ChoiceError::MustAct { slot } => write!(f, "slot {slot} must act"),
            ChoiceError::InvalidTeamOrder => f.write_str("invalid team order"),
        }
    }
}

impl std::error::Error for ChoiceError {}
