//! A channel session as a game of a best-of series: the series it is given
//! shows in its view, and it records the game's result for the next game's
//! session, as the environment's series mode does (the round trip against
//! `VecEnv` is `cargo channel-conformance --series 3`).

use pikapro::{Format, SideId};
use pikapro_rl::series::{Series, SeriesResult};
use pikapro_rl::session::{ChannelSession, SessionOptions};

mod common;

fn game(series: Option<Series>) -> ChannelSession {
    let mut s = ChannelSession::for_format(Format::champions_vgc(), false, SessionOptions::default()).unwrap();
    s.set_side(SideId::P1);
    if let Some(series) = series {
        s.set_series(series).unwrap();
    }
    s
}

#[test]
fn a_game_records_its_result_for_the_next() {
    let lines = common::commander_channel();
    let mut first = game(Some(Series::new(3)));
    assert_eq!(first.series_row(), Some([1, 0, 0, 0]));
    assert!(first.recorded_series().is_none());
    for line in &lines {
        first.feed(line);
    }
    assert!(first.finished());
    let winner = first.winner().expect("the recorded battle has a winner");
    let recorded = first.recorded_series().expect("recorded at the end").clone();
    assert_eq!((recorded.game, recorded.previous.len(), recorded.result), (1, 1, SeriesResult::Ongoing));
    assert_eq!(recorded.wins[winner.index()], 1);
    assert_eq!(recorded.previous[0], *first.tracker(), "the game's public record at its end");
    let mine = u8::from(winner == SideId::P1);
    assert_eq!(first.series_row(), Some([1, mine, 1 - mine, 0]));
    let view = first.view();
    assert_eq!(view.series.map(|v| (v.game, v.wins, v.previous.len())), Some((1, recorded.wins, 1)));

    // Game 2: the same battle again, won by the same side, settles it.
    let mut second = game(recorded.next_game());
    assert_eq!(second.series_row(), Some([2, mine, 1 - mine, 0]));
    for line in &lines {
        second.feed(line);
    }
    let settled = second.recorded_series().unwrap();
    assert!(settled.is_over() && settled.previous.len() == 2);
    assert_eq!(second.series_row(), Some([2, 2 * mine, 2 * (1 - mine), if mine == 1 { 1 } else { 2 }]));
    assert!(settled.next_game().is_none());

    // The observation of a single game is the series game's (the built-in
    // encoder encodes each game alone).
    let mut alone = game(None);
    for line in &lines {
        alone.feed(line);
    }
    assert!(alone.series_row().is_none() && alone.recorded_series().is_none());
    assert_eq!(alone.obs().to_vec(), second.obs());
}

#[test]
fn only_a_game_the_series_has_is_played() {
    let t = pikapro_view::PublicTracker::for_format(&Format::champions_vgc());
    let mut s = game(None);
    let mut over = Series::new(3);
    over.settle(SeriesResult::Player0).unwrap();
    assert!(s.set_series(over).is_err());
    assert!(s.set_series(Series::new(4)).is_err(), "an even length");
    let mut unrecorded = Series::new(3);
    unrecorded.game = 2;
    assert!(s.set_series(unrecorded.clone()).is_err(), "game 1 isn't recorded");
    // A score that can't lead to the game.
    let mut odd = Series::resume(3, 2, [1, 0], 0, vec![t]).unwrap();
    odd.wins = [7, 7];
    assert!(s.set_series(odd).is_err(), "7-7 before game 2");
    // Resumed without the earlier games' records (a client restarted).
    let resumed = Series::resume(3, 3, [1, 1], 0, Vec::new()).unwrap();
    assert!(s.set_series(resumed).is_ok());
    assert_eq!(s.series_row(), Some([3, 1, 1, 0]));
    assert!(s.set_series(Series::new(5)).is_ok());
    assert_eq!(s.series().map(|x| x.best_of), Some(5));
}

/// A `|win|` naming neither player is no tie:
/// the game is over, its result unknown, and not recorded.
#[test]
fn an_unknown_winner_is_not_a_tie() {
    let mut s = ChannelSession::new("gen9championsvgc2026regmcbo3", SessionOptions::default()).unwrap();
    s.set_series(Series::new(3)).unwrap();
    s.set_side(SideId::P1);
    for l in ["|player|p1|Alice|", "|player|p2|Bob|", "|start", "|win|Bob2"] {
        s.feed(l);
    }
    assert!(s.finished() && s.winner().is_none());
    assert!(s.recorded_series().is_none(), "the result is unknown");
    assert_eq!(s.series_row(), Some([1, 0, 0, 0]));
}

/// CRLF line ends, and a game that ends before its first request (a
/// forfeit at team preview): the end is read.
#[test]
fn crlf_ends_are_read_while_lines_are_held() {
    let mut s = ChannelSession::new("gen9championsvgc2026regmcbo3", SessionOptions::default()).unwrap();
    s.set_series(Series::new(3)).unwrap();
    for l in ["|player|p1|Alice|\r", "|player|p2|Bob|\r", "|teampreview|4\r", "|tie\r"] {
        s.feed(l);
    }
    assert!(s.finished());
    assert_eq!(s.recorded_series().map(|r| r.ties), Some(1));
}

/// A series the server ended early (a forfeit in the series room, a game
/// lost by disconnection): the session shows it settled.
#[test]
fn a_series_the_server_ended_is_settled() {
    let lines = common::commander_channel();
    let mut first = game(Some(Series::new(3)));
    for line in &lines {
        first.feed(line);
    }
    let winner = first.winner().unwrap();
    let mine = u8::from(winner == SideId::P1);
    assert_eq!(first.series_row(), Some([1, mine, 1 - mine, 0]));
    // p2 forfeits the series (or lost the game by disconnection).
    first.settle_series(Some(SideId::P1)).unwrap();
    assert_eq!(first.series_row(), Some([1, mine, 1 - mine, 1]));
    let r = first.recorded_series().unwrap();
    assert_eq!((r.result, r.next_game()), (SeriesResult::Player0, None));
    assert!(first.settle_series(Some(SideId::P2)).is_err(), "settled otherwise");
    // Settled before the game's end: the record keeps it.
    let mut early = game(Some(Series::new(3)));
    early.settle_series(None).unwrap();
    assert_eq!(early.series_row(), Some([1, 0, 0, 3]));
    for line in &lines {
        early.feed(line);
    }
    assert_eq!(early.recorded_series().map(|r| r.result), Some(SeriesResult::Tie));
    assert!(game(None).settle_series(None).is_err(), "no series");
}
