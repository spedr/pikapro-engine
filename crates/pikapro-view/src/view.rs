//! [`PlayerView`]: one player's information set.

use pikapro::SideId;

use crate::mon::MonView;
use crate::overlay::{OwnMon, OwnOverlay};
use crate::side::{PublicField, PublicSide};
use crate::team::OwnTeam;
use crate::tracker::PublicTracker;

/// What player `me` knows: the public record every spectator shares, plus
/// its own request. This is all an encoder, a policy or a search may read:
/// nothing here is derived from the omniscient state.
///
/// A borrow of the battle's one tracker and the player's overlay, so
/// building it costs nothing. Build one with [`PlayerView::new`] and the
/// `with_*` methods.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PlayerView<'a> {
    pub me: SideId,
    pub public: &'a PublicTracker,
    pub own: &'a OwnOverlay,
    /// The best-of series this game is part of, if any
    /// ([`PlayerView::with_series`]).
    pub series: Option<SeriesView<'a>>,
    /// The player's own team, if it gave it ([`PlayerView::with_team`]):
    /// natures, the Pokémon left out at team preview, benched PP.
    pub team: Option<&'a OwnTeam>,
}

/// A best-of series as its players know it: the score and each earlier
/// game's public record at its end. The same two teams play every game on
/// the same sides, with Open Team Sheets (Showdown's `BestOfGame`).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct SeriesView<'a> {
    pub best_of: u8,
    /// The game being played (1-based).
    pub game: u8,
    /// Games won, by [`SideId::index`] (p1's first).
    pub wins: [u8; 2],
    pub ties: u8,
    /// The earlier games' public records, in order (none for a series
    /// resumed without them).
    pub previous: &'a [PublicTracker],
}

impl<'a> SeriesView<'a> {
    /// Game `game` of a best-of-`best_of`, `wins` by side.
    pub fn new(best_of: u8, game: u8, wins: [u8; 2], ties: u8, previous: &'a [PublicTracker]) -> SeriesView<'a> {
        SeriesView { best_of, game, wins, ties, previous }
    }
}

impl<'a> PlayerView<'a> {
    pub fn new(me: SideId, public: &'a PublicTracker, own: &'a OwnOverlay) -> PlayerView<'a> {
        debug_assert_eq!(own.side, me, "an overlay for the other side");
        PlayerView { me, public, own, series: None, team: None }
    }

    /// The view with the player's own team.
    pub fn with_team(self, team: &'a OwnTeam) -> PlayerView<'a> {
        PlayerView { team: Some(team), ..self }
    }

    /// The view with the series this game is part of.
    pub fn with_series(self, series: SeriesView<'a>) -> PlayerView<'a> {
        PlayerView { series: Some(series), ..self }
    }

    /// The view with the series this game is part of, if any.
    pub fn with_series_opt(self, series: Option<SeriesView<'a>>) -> PlayerView<'a> {
        PlayerView { series, ..self }
    }

    /// Games the player has won in the series, and its opponent (none
    /// outside a series).
    #[cfg(test)]
    pub(crate) fn series_score(&self) -> [u8; 2] {
        self.series.map_or([0; 2], |s| [s.wins[self.me.index()], s.wins[self.me.foe().index()]])
    }

    /// The player's side in the public record.
    pub fn my_side(&self) -> &'a PublicSide {
        self.public.side(self.me)
    }

    /// The opponent's side.
    pub fn foe_side(&self) -> &'a PublicSide {
        self.public.side(self.me.foe())
    }

    pub fn field(&self) -> &'a PublicField {
        &self.public.field
    }

    pub fn turn(&self) -> u16 {
        self.public.turn
    }

    /// The public row of the player's own Pokémon `id` (an
    /// [`OwnMon::id`]), once the log has named it. A Pokémon whose
    /// Illusion is up is named as its disguise, so the log's rows follow
    /// the disguise.
    ///
    /// With team preview a Pokémon's row is its id (the `|poke|` order is
    /// the registration order). Without it, rows follow the order of
    /// appearance and a Pokémon not yet seen has none: a player's own
    /// stable index for all its Pokémon is then its id, which it knows.
    pub fn own_row(&self, id: u8) -> Option<u8> {
        self.my_side().row_of(id)
    }

    /// The own Pokémon (its [`OwnMon::id`]) whose public row is `row`.
    pub fn own_id_of_row(&self, row: u8) -> Option<u8> {
        self.my_side().id_of(row)
    }

    /// The current team position (`SlotAction::Switch(i)`) of the own
    /// Pokémon `id`.
    #[cfg(test)]
    pub(crate) fn own_position(&self, id: u8) -> Option<usize> {
        self.own.by_id(id).map(|(i, _)| i)
    }

    /// The player's own Pokémon with the public record of it, by current
    /// team position (`SlotAction::Switch(i)`).
    pub fn own_mons(&self) -> impl Iterator<Item = (&'a OwnMon, Option<&'a MonView>)> + 'a {
        let side = self.my_side();
        self.own.mons.iter().map(move |m| (m, side.row_of(m.id).and_then(|r| side.mon(r))))
    }
}
