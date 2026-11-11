//! Best-of series: a battle's consecutive episodes
//! grouped into a best-of-`n` (Bo3 by default), as Showdown's
//! `BestOfGame` (`server/room-battle-bestof.ts`) runs one.
//!
//! * Every game of a series plays the same two teams, on the same sides,
//!   against the same opponent, with Open Team Sheets.
//! * A player wins the series at `floor((best_of - ties) / 2) + 1` wins;
//!   after `best_of` games without that, the series is a tie (`onBattleWin`).
//!   A game cut by the environment's `max_turns` counts as a tie.
//! * Each game is an episode of its own (its own seed, rewards and
//!   termination); [`Series`] carries the score and the public record of
//!   the earlier games ([`Series::previous`]).
//!
//! * A series is best of 3, 5, 7 or 9 games, as Showdown allows
//!   (`bestof.onValidateRule` in `data/rulesets.ts`: "an odd number
//!   between three and nine").
//!
//! Encoders see the series through the view: each player's
//! [`PlayerView::series`](pikapro_view::PlayerView) borrows it (the score by
//! side and the earlier games' records), so a user's [`Encoder`](crate::encode::Encoder)
//! can read it. The built-in encoder (`basic`) encodes each game alone;
//! [`StepOut::series`](crate::vec::StepOut::series) gives the caller the
//! score.

use pikapro_view::PublicTracker;

/// The most games a series may have.
pub(crate) const MAX_GAMES: u8 = 9;

/// Checks a series length: 3, 5, 7 or 9 games (Showdown's `bestof` rule).
pub fn check_best_of(best_of: u8) -> Result<(), String> {
    if best_of % 2 == 1 && (3..=MAX_GAMES).contains(&best_of) {
        Ok(())
    } else {
        Err(format!("a series is best of 3, 5, 7 or 9 games (Showdown's rule), not {best_of}"))
    }
}

/// A series' outcome, as the `series` buffer codes it. `Player0` and
/// `Player1` are the first and second of the [`Series`]' frame: players 0
/// and 1 in the environment, p1 and p2 in a [`ChannelSession`](crate::session::ChannelSession).
///
/// Exhaustive on purpose: these are the buffer's codes (0 to 3), whose
/// meaning a new variant would change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SeriesResult {
    #[default]
    Ongoing = 0,
    /// The first won the series (player 0; p1 in a session).
    Player0 = 1,
    /// The second won the series (player 1; p2 in a session).
    Player1 = 2,
    Tie = 3,
}

impl SeriesResult {
    /// The result seen from the other of the two: the winner swapped.
    pub fn swapped(self) -> SeriesResult {
        match self {
            SeriesResult::Player0 => SeriesResult::Player1,
            SeriesResult::Player1 => SeriesResult::Player0,
            other => other,
        }
    }
}

/// Values per battle in the `series` buffer: the game (1-based), the
/// first's wins, the second's and the [`SeriesResult`]
/// ([`Series::row`]).
pub const SERIES_WIDTH: usize = 4;

/// A series under way, kept with its battle.
///
/// **Frames.** [`Series::wins`] and [`Series::result`] are indexed in the
/// frame of whoever keeps the series:
///
/// * the environment ([`VecEnv`](crate::env::VecEnv)'s series mode) keeps
///   them **by player**: index 0 is player 0, the agent;
/// * a [`ChannelSession`](crate::session::ChannelSession) (its
///   [`series`](crate::session::ChannelSession::series) and
///   [`recorded_series`](crate::session::ChannelSession::recorded_series))
///   and a Python `ChannelSeries` keep them **by side**: index 0 is p1,
///   the order [`SeriesView`](pikapro_view::SeriesView) has them in
///   (Showdown keeps each player's side through a series).
///
/// [`Series::row`] is the row in the series' own frame; [`Series::row_as`]
/// is the row seen from one of the two (their wins first, the result
/// `Player0` when they won), which is what
/// [`ChannelSession::series_row`](crate::session::ChannelSession::series_row)
/// gives for the session's player.
///
/// Build one with [`Series::new`] (game 1) or [`Series::resume`] (a series
/// already under way).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Series {
    pub best_of: u8,
    /// The game being played (1-based); once recorded, the game that was.
    pub game: u8,
    /// Games won, in the series' frame (see above).
    pub wins: [u8; 2],
    pub ties: u8,
    pub result: SeriesResult,
    /// Each earlier game's public record at its end (both sides, as every
    /// spectator saw them; with the player's own request, what each player
    /// knew), in order. Empty for a series [resumed](Series::resume)
    /// without them.
    pub previous: Vec<PublicTracker>,
}

impl Series {
    /// The first game of a best-of-`best_of` ([`check_best_of`] says which
    /// lengths Showdown runs).
    pub fn new(best_of: u8) -> Series {
        Series { best_of, game: 1, wins: [0; 2], ties: 0, result: SeriesResult::Ongoing, previous: Vec::new() }
    }

    /// Game `game` of a best-of-`best_of` series under way, with the
    /// score so far (`wins` in the frame its keeper uses) and the earlier
    /// games' public records (`previous`: all `game - 1` of them, or none
    /// when they aren't known, as for a client that restarts in the middle
    /// of a series). Errors for a score that can't lead to that game
    /// ([`Series::check_next`]).
    pub fn resume(
        best_of: u8,
        game: u8,
        wins: [u8; 2],
        ties: u8,
        previous: Vec<PublicTracker>,
    ) -> Result<Series, String> {
        let s = Series { best_of, game, wins, ties, result: SeriesResult::Ongoing, previous };
        s.check_next()?;
        Ok(s)
    }

    /// Checks a series whose game `game` is still to play: a length
    /// Showdown runs, a game within it, a score of `game - 1` games that
    /// doesn't settle it, and `game - 1` records (or none).
    pub fn check_next(&self) -> Result<(), String> {
        check_best_of(self.best_of)?;
        if self.is_over() {
            return Err("the series is over: it has no game to play".into());
        }
        if self.game == 0 || self.game > self.best_of {
            return Err(format!("game {} of a best-of-{}", self.game, self.best_of));
        }
        let played = u16::from(self.wins[0]) + u16::from(self.wins[1]) + u16::from(self.ties);
        if played + 1 != u16::from(self.game) {
            return Err(format!(
                "game {} after {} and {} wins and {} ties",
                self.game, self.wins[0], self.wins[1], self.ties
            ));
        }
        if self.wins.iter().any(|&w| w >= self.threshold()) {
            return Err(format!(
                "{} wins settle a best-of-{} with {} ties: no game to play",
                self.threshold(),
                self.best_of,
                self.ties
            ));
        }
        if !self.previous.is_empty() && self.previous.len() + 1 != usize::from(self.game) {
            return Err(format!("game {} with {} earlier games recorded", self.game, self.previous.len()));
        }
        Ok(())
    }

    /// The wins a player needs: `floor((best_of - ties) / 2) + 1`.
    pub fn threshold(&self) -> u8 {
        self.best_of.saturating_sub(self.ties) / 2 + 1
    }

    /// Records the end of the current game: won by `winner` (0 or 1, in
    /// the series' frame; `None`: a tie), with `tracker` its public record.
    /// Settles the series if it is over (`onBattleWin`). Errors, changing
    /// nothing, for a series already over or a winner that is neither.
    pub fn record(&mut self, winner: Option<usize>, tracker: &PublicTracker) -> Result<(), String> {
        if self.is_over() {
            return Err("the series is over: no game to record".into());
        }
        match winner {
            Some(p @ 0..=1) => self.wins[p] = self.wins[p].saturating_add(1),
            Some(p) => return Err(format!("a game is won by 0 or 1, not {p}")),
            None => self.ties = self.ties.saturating_add(1),
        }
        self.previous.push(*tracker);
        let threshold = self.threshold();
        self.result = if self.wins[0] >= threshold {
            SeriesResult::Player0
        } else if self.wins[1] >= threshold {
            SeriesResult::Player1
        } else if self.game >= self.best_of {
            SeriesResult::Tie
        } else {
            SeriesResult::Ongoing
        };
        Ok(())
    }

    /// Ends the series as the server did, whatever its score: Showdown
    /// ends a series early when a player forfeits it (the series room's
    /// `/forfeit`), loses a game by disconnection, or stays away between
    /// games (`BestOfGame`'s `forfeitPlayer`, `onBattleWin` and
    /// `pokeNextBattleTimer`); the game's room shows only its own `|win|`.
    /// Errors for `Ongoing`, or for a series already settled otherwise.
    pub fn settle(&mut self, result: SeriesResult) -> Result<(), String> {
        if result == SeriesResult::Ongoing {
            return Err("a series is settled with a winner or a tie".into());
        }
        if self.is_over() && self.result != result {
            return Err(format!("the series is already settled: {:?}", self.result));
        }
        self.result = result;
        Ok(())
    }

    pub fn is_over(&self) -> bool {
        self.result != SeriesResult::Ongoing
    }

    /// The series' next game, if it isn't over.
    #[must_use]
    pub fn next_game(&self) -> Option<Series> {
        (!self.is_over()).then(|| Series { game: self.game + 1, ..self.clone() })
    }

    /// The `series` buffer's values in the series' own frame: the game,
    /// the first's wins, the second's, the [`SeriesResult`].
    pub fn row(&self) -> [u8; SERIES_WIDTH] {
        [self.game, self.wins[0], self.wins[1], self.result as u8]
    }

    /// The row seen from `index` (0 or 1 in the series' frame): the game,
    /// its wins, the other's, and the result with `Player0` meaning it won
    /// the series.
    pub fn row_as(&self, index: usize) -> [u8; SERIES_WIDTH] {
        if index == 0 {
            return self.row();
        }
        [self.game, self.wins[1], self.wins[0], self.result.swapped() as u8]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::Format;

    #[test]
    fn best_of_three_follows_showdown() {
        let t = PublicTracker::for_format(&Format::champions_vgc());
        let play = |games: &[Option<usize>]| {
            let mut s = Series::new(3);
            for &w in games {
                assert!(!s.is_over());
                s.record(w, &t).unwrap();
                if let Some(next) = s.next_game() {
                    s = next;
                }
            }
            s
        };
        assert_eq!(play(&[Some(0), Some(0)]).result, SeriesResult::Player0);
        assert_eq!(play(&[Some(0), Some(1), Some(1)]).result, SeriesResult::Player1);
        // Ties lower the threshold (Showdown's `winThreshold =
        // floor((bestOf - ties) / 2) + 1`): after two ties one win takes a
        // Bo3; after one, it still takes two.
        assert_eq!(play(&[None, None, Some(0)]).result, SeriesResult::Player0);
        assert_eq!(play(&[Some(1), None, Some(1)]).result, SeriesResult::Player1);
        assert_eq!(play(&[None, None, None]).result, SeriesResult::Tie);
        let s = play(&[Some(0), Some(1), None]);
        assert_eq!((s.result, s.row(), s.previous.len()), (SeriesResult::Tie, [3, 1, 1, 3], 3));
    }

    /// No panic, nothing counted past the end, and only scores that can
    /// happen.
    #[test]
    fn records_and_scores_are_checked() {
        let t = PublicTracker::for_format(&Format::champions_vgc());
        let mut s = Series::new(3);
        assert!(s.record(Some(2), &t).is_err(), "a third player");
        assert_eq!(s, Series::new(3), "unchanged");
        s.record(Some(0), &t).unwrap();
        let mut s = s.next_game().unwrap();
        s.record(Some(0), &t).unwrap();
        assert_eq!(s.result, SeriesResult::Player0);
        assert!(s.record(Some(1), &t).is_err(), "settled");
        assert_eq!((s.wins, s.previous.len(), s.row_as(1)), ([2, 0], 2, [2, 0, 2, 2]));
        assert!(Series::resume(3, 2, [7, 7], 0, vec![t]).is_err());
        assert!(Series::resume(3, 3, [2, 0], 0, Vec::new()).is_err(), "settled at 2 wins");
        assert!(Series::resume(3, 3, [1, 0], 0, Vec::new()).is_err(), "two games before game 3");
        assert!(Series::resume(3, 2, [1, 0], 0, vec![t, t]).is_err(), "one record");
        let r = Series::resume(3, 3, [1, 1], 0, Vec::new()).unwrap();
        assert_eq!(r.row(), [3, 1, 1, 0]);
        assert!(Series::resume(5, 3, [1, 0], 1, vec![t, t]).is_ok());
    }

    /// A series the server ended early (a forfeit, a disconnection).
    #[test]
    fn a_series_can_be_settled_early() {
        let t = PublicTracker::for_format(&Format::champions_vgc());
        let mut s = Series::new(3);
        s.record(Some(0), &t).unwrap();
        assert!(s.settle(SeriesResult::Ongoing).is_err());
        s.settle(SeriesResult::Player1).unwrap();
        assert!(s.is_over() && s.next_game().is_none());
        assert_eq!((s.row(), s.row_as(1)), ([1, 1, 0, 2], [1, 0, 1, 1]));
        assert!(s.settle(SeriesResult::Player0).is_err(), "settled otherwise");
        assert!(s.settle(SeriesResult::Player1).is_ok(), "the same again");
    }

    #[test]
    fn a_series_is_an_odd_number_of_games_from_three_to_nine() {
        let ok: Vec<u8> = (0..=u8::MAX).filter(|&b| check_best_of(b).is_ok()).collect();
        assert_eq!(ok, [3, 5, 7, 9]);
    }
}
