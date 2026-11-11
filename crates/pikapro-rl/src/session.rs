//! [`ChannelSession`]: one battle driven by Showdown's text. Fed a
//! player's channel line by line (a websocket client,
//! Showdown's `getPlayerStreams`, `pikapro.Battle`'s log), it keeps the same
//! [`PlayerView`] the environment builds from the engine's typed events,
//! and gives the observation and mask [`VecEnv`](crate::env::VecEnv) gives
//! (same encoder, mask layout and `switch_by` rows) for the same battle,
//! side and decision. An action from the policy that chose it in the
//! environment becomes Showdown's `/choose` text.
//!
//! How it gets there, with no battle to read:
//!
//! * the channel's lines go through [`ChannelParser`] into a
//!   [`PublicTracker`], which is fooled by Illusion exactly as the typed
//!   path is;
//! * each `|request|` becomes the [`OwnOverlay`]
//!   ([`OwnOverlay::from_request`], as the environment's overlay equals the
//!   request's); an update after `[Unavailable choice]` teaches the overlay
//!   the trap or disable, as [`OwnOverlay::reject`] does in the
//!   environment;
//! * the actions the request shows ([`shown_legal`]) are computed from the
//!   overlay and the public view: the environment's masks come from what
//!   the request shows too (`pikapro::legal::shown_actions`);
//! * the player's own team ([`OwnTeam`]) comes from
//!   [`SessionOptions::team`], as the environment's comes from the team it
//!   plays, and learns from each request (the PP of the active Pokémon).
//!   Without the team, it learns what requests show (not the natures).
//! * a game of a best-of series ([`ChannelSession::set_series`]) sees the
//!   series as the environment's series mode shows it: the score, the game
//!   and the earlier games' public records, which the caller carries from
//!   one game's session to the next ([`ChannelSession::recorded_series`]).

use std::sync::Arc;

use pikapro::legal::SlotActions;
use pikapro::{
    BattleEvent, BattleRng, Choice, Format, Gimmick, LegalActions, MAX_ACTIVE, Request, Seed, SideId, SlotAction,
    Target,
};
use pikapro_data::MoveTarget;
use pikapro_protocol::channel::{ChannelParser, LineKind, ParseError, request_body, showdown_format};
use pikapro_protocol::format_choice;
use pikapro_view::{
    HpReport, OwnMoves, OwnOverlay, OwnRequest, OwnTeam, PlayerView, PublicTracker, SeriesView, ViewConfig,
};

use crate::encode::ViewCtx;
use crate::policy::Policy;
use crate::series::{SERIES_WIDTH, Series, SeriesResult};
use crate::space::ActionSpace;
use crate::tables::Tables;
use crate::vec::{MaskLayout, Observation, SwitchBy};

/// Lines kept by [`ChannelSession::unknown_line_texts`] (the count goes
/// on).
pub(crate) const KEEP_UNKNOWN: usize = 1000;

/// Most lines a session holds for its first request
/// ([`ChannelSession::feed`]): a reconnect's replay of a long battle fits.
pub const MAX_HELD: usize = 1 << 17;

/// A session's options: the environment's settings that decide what an
/// observation and a mask are ([`EnvConfig`](crate::env::EnvConfig)'s of
/// the same names). Start from [`SessionOptions::default`] and set the
/// fields.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct SessionOptions {
    pub observation: Observation,
    pub masks: MaskLayout,
    pub switch_by: SwitchBy,
    /// Foe HP as percentages (the default) or exact. Exact needs a channel
    /// that shows it (a custom game).
    pub hp: HpReport,
    /// The player's Showdown name: its `|player|` line says which side is
    /// its own. Without it, the first request does.
    pub username: Option<String>,
    /// The team the player plays, in its registration order: what no
    /// request shows (natures, the Pokémon left out at team preview once
    /// the battle started) comes from it, as in the environment.
    /// [`ChannelSession::new`] takes it as the player registered it and
    /// plays it as the format's server does: where the server validates
    /// teams (`ShowdownFormat::validated`), as its validator leaves it
    /// ([`teams::played`](crate::teams::played): a listed `Floette-Mega` is
    /// Floette-Eternal, as the requests show it).
    /// [`ChannelSession::for_format`] takes it as the server plays it.
    pub team: Option<pikapro::Team>,
}

impl SessionOptions {
    /// With the observation.
    #[must_use]
    pub fn with_observation(self, observation: Observation) -> SessionOptions {
        SessionOptions { observation, ..self }
    }

    /// With the mask layout.
    #[must_use]
    pub fn with_masks(self, masks: MaskLayout) -> SessionOptions {
        SessionOptions { masks, ..self }
    }

    /// With the switch actions' order.
    #[must_use]
    pub fn with_switch_by(self, switch_by: SwitchBy) -> SessionOptions {
        SessionOptions { switch_by, ..self }
    }

    /// With the foe HP report.
    #[must_use]
    pub fn with_hp(self, hp: HpReport) -> SessionOptions {
        SessionOptions { hp, ..self }
    }

    /// With the player's Showdown name (or none).
    #[must_use]
    pub fn with_username(self, username: Option<String>) -> SessionOptions {
        SessionOptions { username, ..self }
    }

    /// With the team the player plays (or none).
    #[must_use]
    pub fn with_team(self, team: Option<pikapro::Team>) -> SessionOptions {
        SessionOptions { team, ..self }
    }
}

/// The decision a session's request asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequestKind {
    /// No request yet.
    #[default]
    None,
    Wait,
    TeamPreview,
    Move,
    /// A forced switch.
    Switch,
}

impl RequestKind {
    /// `none`, `wait`, `teampreview`, `move`, `switch`.
    pub fn name(self) -> &'static str {
        match self {
            RequestKind::None => "none",
            RequestKind::Wait => "wait",
            RequestKind::TeamPreview => "teampreview",
            RequestKind::Move => "move",
            RequestKind::Switch => "switch",
        }
    }
}

/// One battle driven by a player's channel. See the module docs.
pub struct ChannelSession {
    format: Format,
    exact_hp: bool,
    options: SessionOptions,
    space: Arc<ActionSpace>,
    tables: Arc<Tables>,
    parser: ChannelParser,
    tracker: PublicTracker,
    own: OwnOverlay,
    /// The player's own team: [`SessionOptions::team`], and what requests
    /// showed.
    team: OwnTeam,
    /// The side, once known.
    me: Option<SideId>,
    kind: RequestKind,
    needs_action: bool,
    /// The last choice this session made for the current request (for an
    /// `[Unavailable choice]`'s update).
    last_choice: Option<Choice>,
    finished: bool,
    winner: Option<SideId>,
    /// The game ended with a `|win|` naming neither player: its result
    /// is unknown, and a series doesn't record it.
    unknown_end: bool,
    /// Lines that came before the player's side was known: fed once it is
    /// (a `|split|` block must know whose secret line to read).
    held: Vec<String>,
    /// Lines are held for the first request: until one comes (read or
    /// not), the battle ends, [`ChannelSession::flush`], or [`MAX_HELD`]
    /// lines were held. Never again after.
    holding: bool,
    /// The first lines the parser didn't know ([`KEEP_UNKNOWN`]).
    unknown: Vec<String>,
    error: Option<String>,
    events: Vec<BattleEvent>,
    /// The series this game is part of ([`ChannelSession::set_series`]),
    /// wins by side; once the game ended, with its result recorded.
    series: Option<Series>,
    recorded: Option<Series>,
    /// The series' result as the server settled it
    /// ([`ChannelSession::settle_series`]), by side.
    settled: Option<SeriesResult>,
    // Lazily computed outputs.
    fresh: bool,
    legal: LegalActions,
    obs: Vec<f32>,
    mask: Vec<bool>,
    min_switches: u8,
}

impl std::fmt::Debug for ChannelSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSession")
            .field("format", &self.format.id)
            .field("me", &self.me)
            .field("kind", &self.kind)
            .field("needs_action", &self.needs_action)
            .field("turn", &self.tracker.turn)
            .finish()
    }
}

impl ChannelSession {
    /// A session for the Showdown format `format_id`
    /// ([`pikapro_protocol::channel::showdown_format`] lists them).
    pub fn new(format_id: &str, mut options: SessionOptions) -> Result<ChannelSession, String> {
        let f = showdown_format(format_id).ok_or_else(|| format!("unsupported Showdown format {format_id:?}"))?;
        if f.validated {
            options.team = options.team.map(|t| crate::teams::played(&t, &f.format));
        }
        ChannelSession::for_format(f.format, f.exact_hp, options)
    }

    /// A session for a pikapro format; `exact_hp`: the channel shows
    /// exact HP for everyone.
    pub fn for_format(format: Format, exact_hp: bool, options: SessionOptions) -> Result<ChannelSession, String> {
        if options.hp == HpReport::Exact && !exact_hp {
            return Err("exact HP needs a format whose channel shows it (a custom game)".into());
        }
        let space = Arc::new(ActionSpace::new(&format));
        space.check_format(&format)?;
        let tables = Tables::for_format(&format);
        let tables = Arc::new(tables);
        let view = ViewConfig::new(&format).with_hp(options.hp);
        let obs = vec![0.0; options.observation.size()];
        let width = options.masks.width(&space);
        let mut parser = ChannelParser::new(&format, None);
        parser.set_exact_hp(exact_hp);
        let team = options.team.as_ref().map_or_else(OwnTeam::default, |t| OwnTeam::from_team(t, &format));
        Ok(ChannelSession {
            parser,
            tracker: PublicTracker::new(view),
            own: OwnOverlay::default(),
            team,
            me: None,
            kind: RequestKind::None,
            needs_action: false,
            last_choice: None,
            finished: false,
            winner: None,
            unknown_end: false,
            held: Vec::new(),
            holding: true,
            unknown: Vec::new(),
            error: None,
            events: Vec::new(),
            series: None,
            recorded: None,
            settled: None,
            fresh: false,
            legal: LegalActions::default(),
            obs,
            mask: vec![false; width],
            min_switches: 0,
            format,
            exact_hp,
            options,
            space,
            tables,
        })
    }

    /// Sets the player's side (when its username isn't known).
    pub fn set_side(&mut self, side: SideId) {
        self.set_me(side);
    }

    pub fn format(&self) -> &Format {
        &self.format
    }

    /// Plays this session as game `series.game` of a best-of series, as the
    /// environment's series mode (`EnvConfig::series`) plays each game: its
    /// view carries the score, the game and the earlier games' public
    /// records ([`PlayerView::series`]), for the encoders and policies that
    /// read them. A session keeps a series' wins **by side** (`wins[0]` is
    /// p1's, the order [`SeriesView`] has them in; Showdown keeps each
    /// player's side through a series). The next game's session gets
    /// [`ChannelSession::recorded_series`]'s [`Series::next_game`].
    ///
    /// Errors for a series that is over, whose length Showdown wouldn't
    /// run ([`crate::series::check_best_of`]), whose score can't lead to its
    /// game, or whose game doesn't follow its recorded games (a series
    /// [resumed](Series::resume) without records has none):
    /// [`Series::check_next`].
    pub fn set_series(&mut self, series: Series) -> Result<(), String> {
        series.check_next()?;
        self.series = Some(series);
        self.recorded = None;
        self.settled = None;
        if self.finished {
            self.record_series();
        }
        self.fresh = false;
        Ok(())
    }

    /// The series this game is part of, as it stood when the game started
    /// (wins by side).
    pub fn series(&self) -> Option<&Series> {
        self.series.as_ref()
    }

    /// Once the game ended (`|win|`, `|tie`), the series with its result
    /// recorded as the environment records it ([`Series::record`]: the
    /// winner, or a tie, and this game's public record at its end), and
    /// settled if it is over. Wins and result **by side** (index 0 is p1;
    /// [`Series`] explains the frames). `None` outside a series, before the
    /// end, and after a `|win|` that named neither player (its result is
    /// unknown: record it with the winner the server gave).
    pub fn recorded_series(&self) -> Option<&Series> {
        self.recorded.as_ref()
    }

    /// The environment's `series` row for this player as player 0 (the
    /// agent's row in series mode): the game, the player's wins, the
    /// opponent's, and the [`SeriesResult`] (`Player0`: this player won the
    /// series), with this game's result once it ended
    /// ([`Series::row_as`]), and the series' end once the server settled
    /// it ([`ChannelSession::settle_series`]). `None` outside a series.
    /// Before the player's side is known, p1's.
    pub fn series_row(&self) -> Option<[u8; SERIES_WIDTH]> {
        let s = self.recorded.as_ref().or(self.series.as_ref())?;
        let me = self.me.unwrap_or(SideId::P1);
        let mut row = s.row_as(me.index());
        if let Some(r) = self.settled.filter(|_| !s.is_over()) {
            row[3] = if me == SideId::P1 { r } else { r.swapped() } as u8;
        }
        Some(row)
    }

    /// The series ended as the server says (the series room's `|win|` or
    /// `|tie`), whatever this game showed: `winner` by side, `None` for a
    /// tie. Showdown ends a series early when a player forfeits it, loses
    /// a game by disconnection or stays away between games; the game's
    /// room then shows only the game's `|win|`. The recorded series
    /// ([`ChannelSession::recorded_series`]) and
    /// [`ChannelSession::series_row`] show it settled (the score stays the
    /// games'). Errors outside a series, or for a series the games already
    /// settled otherwise.
    pub fn settle_series(&mut self, winner: Option<SideId>) -> Result<(), String> {
        let result = match winner {
            Some(SideId::P1) => SeriesResult::Player0,
            Some(SideId::P2) => SeriesResult::Player1,
            None => SeriesResult::Tie,
        };
        let s = self.recorded.as_ref().or(self.series.as_ref()).ok_or("the session isn't a game of a series")?;
        let mut check = s.clone();
        check.settle(result)?;
        if let Some(r) = &mut self.recorded {
            r.settle(result)?;
        }
        self.settled = Some(result);
        self.fresh = false;
        Ok(())
    }

    /// The series the view shows: recorded once the game ended, as the
    /// environment's last observation of a game shows it.
    fn series_view(&self) -> Option<SeriesView<'_>> {
        series_view(&self.series, &self.recorded)
    }

    fn record_series(&mut self) {
        if self.unknown_end {
            return;
        }
        self.recorded = self.series.as_ref().and_then(|s| {
            let mut r = s.clone();
            // `set_series` took a series with a game to play: it records.
            r.record(self.winner.map(SideId::index), &self.tracker).ok()?;
            if let Some(result) = self.settled {
                // Settled by the server before the game's end: the game's
                // result may have settled it the same way, or not at all.
                let _ = r.settle(result);
            }
            Some(r)
        });
    }

    /// Whether the channel shows exact HP for everyone.
    pub fn exact_hp(&self) -> bool {
        self.exact_hp
    }

    pub fn options(&self) -> &SessionOptions {
        &self.options
    }

    pub fn space(&self) -> &ActionSpace {
        &self.space
    }

    pub fn tables(&self) -> &Tables {
        &self.tables
    }

    /// The player's side, once known.
    pub fn side(&self) -> Option<SideId> {
        self.me
    }

    /// Feeds one line of the channel (without its newline). A `|request|`
    /// is read at once.
    ///
    /// The battle's lines before its first request are held until that
    /// request is read: it names the player's own Pokémon, which the lines
    /// must be read with. (Showdown sends the first request right after the
    /// first update; without team preview, a lead may be an Illusion that
    /// only the request sees through.) The held lines give the Pokémon
    /// their registration ids ([`ChannelParser::first_request`]), so a
    /// session that joins a battle already going (a reconnect: Showdown
    /// sends the room's whole log, then the current request) reads it as
    /// one that saw it all. At most [`MAX_HELD`] lines are held: past them
    /// the held lines and every later one are read as they come (as a
    /// spectator's), and [`ChannelSession::error`] says so, since the first
    /// request can then no longer read the start ahead (without team
    /// preview, the player's own ids may not follow the registration
    /// order).
    ///
    /// `|sentchoice|` (a reconnect's request the player had answered)
    /// marks the request answered.
    pub fn feed(&mut self, line: &str) -> LineKind {
        // A channel with CRLF line ends (the end is looked for below).
        let line = line.strip_suffix('\r').unwrap_or(line);
        if self.holding && !self.finished {
            if request_body(line).is_some() {
                // The request says whose channel this is, and names the
                // player's Pokémon. Read or not, the hold is over.
                let kind = self.feed_line(line);
                self.release_held();
                return kind;
            }
            if line.starts_with("|win|") || line == "|tie" {
                self.release_held();
                return self.feed_line(line);
            }
            if line.starts_with("|player|") {
                // The username says which side is the player's.
                return self.feed_line(line);
            }
            if self.held.len() < MAX_HELD {
                self.held.push(line.to_string());
                return LineKind::Ignored;
            }
            self.error = Some(format!(
                "more than {MAX_HELD} lines before the first request: read as they came, without its read-ahead"
            ));
            self.release_held();
        }
        self.feed_line(line)
    }

    /// Reads the lines held for the first request now, and every later
    /// line as it comes (a spectator's session, which gets no request).
    pub fn flush(&mut self) {
        self.release_held();
    }

    /// Feeds several lines.
    pub fn feed_many<S: AsRef<str>>(&mut self, lines: &[S]) {
        for l in lines {
            // Showdown batches lines with `\n`.
            for line in l.as_ref().split('\n') {
                self.feed(line);
            }
        }
    }

    fn set_me(&mut self, side: SideId) {
        self.me = Some(side);
        self.parser.set_me(side);
        // The overlay is the player's even before its first request.
        if self.own == OwnOverlay::default() {
            self.own.side = side;
        }
    }

    fn release_held(&mut self) {
        self.holding = false;
        for line in core::mem::take(&mut self.held) {
            self.feed_line(&line);
        }
    }

    fn feed_line(&mut self, line: &str) -> LineKind {
        let events = &mut self.events;
        events.clear();
        let kind = self.parser.feed(line, &mut |e| events.push(e));
        if kind == LineKind::Unknown && self.unknown.len() < KEEP_UNKNOWN {
            self.unknown.push(line.to_string());
        }
        for e in &self.events {
            self.tracker.apply(e);
        }
        if !self.events.is_empty() {
            self.fresh = false;
        }
        match kind {
            LineKind::ShowTeam(side) => {
                // Open Team Sheets (`|showteam|<side>|<packed team>`): what
                // the environment's `ots` shows, from the sets as the
                // sheet gives them. A format without team preview has
                // none: the tracker ignores them there.
                let packed = line.splitn(4, '|').nth(3).unwrap_or("");
                match pikapro_protocol::unpack_team(packed, &self.format) {
                    Ok(sheet) => {
                        let sheet = pikapro_view::OpenSheet::from_team(&sheet.team, &self.format);
                        self.tracker.set_open_sheet(side, sheet);
                        self.fresh = false;
                    }
                    Err(e) => self.error = Some(format!("|showteam|: {e}")),
                }
            }
            LineKind::Player(side) => {
                if self.me.is_none()
                    && let Some(user) = &self.options.username
                    && pikapro_data::id_eq(user, self.parser.player_name(side))
                {
                    self.set_me(side);
                }
            }
            LineKind::Request => {
                if let Some(json) = request_body(line) {
                    let json = json.to_string();
                    if let Err(e) = self.read_request(&json) {
                        self.error = Some(e.to_string());
                    }
                }
            }
            LineKind::Error { unavailable } => {
                // `[Invalid choice]`: the request stands, choose again. An
                // unavailable choice's update comes as a request.
                if !unavailable && !self.finished && self.chooses() && line.contains("[Invalid choice]") {
                    let late =
                        line.contains("not your turn") || line.contains("too late") || line.contains("nothing to");
                    if !late {
                        self.needs_action = true;
                        self.own.answered = false;
                        self.fresh = false;
                    }
                }
            }
            LineKind::End(winner) => {
                self.finished = true;
                self.winner = winner;
                self.unknown_end = false;
                self.needs_action = false;
                self.fresh = false;
            }
            // A winner the channel's `|player|` lines don't name: over, the
            // result unknown (not a tie).
            LineKind::EndUnknown => {
                self.finished = true;
                self.winner = None;
                self.unknown_end = true;
                self.recorded = None;
                self.needs_action = false;
                self.fresh = false;
            }
            // A rejoined player's request it had already answered
            // (`RoomBattle.onConnect` sends the choice after it).
            LineKind::Ignored if line.starts_with("|sentchoice|") && self.needs_action => {
                self.needs_action = false;
                self.own.answered = true;
                self.fresh = false;
            }
            _ => {}
        }
        // The game's record, as the environment records it after the step
        // that ended it: the last update's lines after `|win|` (an ended
        // volatile's `|-end|`) belong in it.
        if self.finished && self.series.is_some() && (matches!(kind, LineKind::End(_)) || !self.events.is_empty()) {
            self.record_series();
        }
        kind
    }

    /// The current request asks for a choice.
    fn chooses(&self) -> bool {
        matches!(self.kind, RequestKind::TeamPreview | RequestKind::Move | RequestKind::Switch)
    }

    /// Why no choice can be made now (the player isn't asked).
    fn not_asked(&self) -> String {
        if self.chooses() && !self.finished {
            "the request was answered: wait for the next one".into()
        } else {
            "no decision is pending".into()
        }
    }

    fn read_request(&mut self, json: &str) -> Result<(), ParseError> {
        // The first reads the lines held for it ahead, for the ids.
        let parsed = self.parser.first_request(&self.held, json, &self.format)?;
        let data = parsed.data;
        if self.me.is_none() {
            self.set_me(data.side);
        }
        if Some(data.side) != self.me {
            return Ok(());
        }
        let new = OwnOverlay::from_request(&data);
        if parsed.update && self.own.side == data.side && self.own.request == new.request {
            self.update_overlay(new);
        } else {
            // An update with no request before it (a rejoin: Showdown
            // sends the room's last request, the update): its state shows
            // what the refusal taught, but not that a refusal taught it.
            self.own = OwnOverlay { updated: parsed.update, ..new };
            self.last_choice = None;
        }
        // `isChoiceDone` fills passes without input for fainted, empty and
        // commanding slots. The JSON still has a move request and no flag
        // saying that it is already complete.
        let automatic = automatic_move_choice(&self.format, &self.own);
        self.own.answered |= automatic;
        self.team.observe(&self.own);
        self.kind = match self.own.request {
            OwnRequest::Move if automatic => RequestKind::Wait,
            OwnRequest::Wait => RequestKind::Wait,
            OwnRequest::TeamPreview { .. } => RequestKind::TeamPreview,
            OwnRequest::Move => RequestKind::Move,
            OwnRequest::ForceSwitch { .. } => RequestKind::Switch,
        };
        self.needs_action = self.chooses() && !self.finished;
        self.fresh = false;
        Ok(())
    }

    /// An `[Unavailable choice]`'s update (`update: true`): the request
    /// with what the refused choice revealed, learnt as the environment's
    /// [`OwnOverlay::reject`] learns it ([`OwnOverlay::learn_update`], the
    /// inference both make).
    fn update_overlay(&mut self, mut new: OwnOverlay) {
        let doubles = self.format.active_per_side() > 1;
        let refused = match self.last_choice {
            Some(Choice::Actions(a)) => a,
            _ => [SlotAction::Pass; MAX_ACTIVE],
        };
        new.learn_update(&self.own, &refused, doubles);
        new.answered = false;
        self.own = new;
    }

    /// Whether the player must choose now.
    pub fn needs_action(&self) -> bool {
        self.needs_action
    }

    pub fn request_kind(&self) -> RequestKind {
        self.kind
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    /// The winner's side (`None` for a tie or before the end).
    pub fn winner(&self) -> Option<SideId> {
        self.winner
    }

    /// The winner's name as the channel gave it.
    pub fn winner_name(&self) -> Option<&str> {
        self.winner.map(|s| self.parser.player_name(s))
    }

    /// The first lines (up to 1,000) the parser didn't know.
    pub fn unknown_line_texts(&self) -> &[String] {
        &self.unknown
    }

    pub fn unknown_lines(&self) -> u32 {
        self.parser.unknown_lines()
    }

    /// The last problem reading the channel, if any: a request that
    /// couldn't be read, a `|showteam|` that couldn't be unpacked, or a
    /// start longer than [`MAX_HELD`] lines ([`ChannelSession::feed`]).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn parser(&self) -> &ChannelParser {
        &self.parser
    }

    pub fn tracker(&self) -> &PublicTracker {
        &self.tracker
    }

    /// What the encoders read: [`ChannelSession::view`] and the tables, up
    /// to date and borrowed together (nothing copied).
    pub fn view_ctx(&mut self) -> ViewCtx<'_> {
        self.refresh();
        let me = &*self;
        ViewCtx::new(&me.view(), &me.tables)
    }

    pub fn overlay(&self) -> &OwnOverlay {
        &self.own
    }

    /// The player's own team as the session knows it.
    pub fn team(&self) -> &OwnTeam {
        &self.team
    }

    /// The player's view (before its side is known, p1's), with the
    /// series in a series ([`ChannelSession::set_series`]).
    pub fn view(&self) -> PlayerView<'_> {
        let me = self.me.unwrap_or(SideId::P1);
        PlayerView::new(me, &self.tracker, &self.own).with_team(&self.team).with_series_opt(self.series_view())
    }

    /// Recomputes the actions, the observation and the mask if the view
    /// changed.
    fn refresh(&mut self) {
        if self.fresh {
            return;
        }
        self.fresh = true;
        let me = self.me.unwrap_or(SideId::P1);
        let (legal, _) = shown_legal(&self.format, &self.own, &self.tracker, me);
        self.legal = legal;
        let series = series_view(&self.series, &self.recorded);
        let view = PlayerView::new(me, &self.tracker, &self.own).with_team(&self.team).with_series_opt(series);
        self.options.observation.encode(&view, &self.tables, &mut self.obs);
        // The mask, as the environment writes it: for a player that must act
        // (all false once it answered, until the next request or an
        // `[Invalid choice]`).
        self.min_switches = 0;
        if !self.needs_action {
            self.mask.fill(false);
            return;
        }
        let (layout, by) = (self.options.masks, self.options.switch_by);
        self.min_switches = self.space.fill_player_mask(layout, by, &legal, &self.own, &mut self.mask);
    }

    /// The actions the current request shows (answered or not).
    pub fn legal(&mut self) -> LegalActions {
        self.refresh();
        self.legal
    }

    /// The observation (`options.observation`'s), as
    /// [`StepOut::obs`](crate::vec::StepOut) holds it for this player.
    pub fn obs(&mut self) -> &[f32] {
        self.refresh();
        &self.obs
    }

    /// The mask, as the environment's mask row for this player: all false
    /// when it needn't act ([`ChannelSession::needs_action`]: before the
    /// first request, once a choice answered the request, after the end).
    pub fn mask(&mut self) -> &[bool] {
        self.refresh();
        &self.mask
    }

    /// The fewest switches a joint action must make
    /// ([`StepOut::min_switches`](crate::vec::StepOut)); 0 when it needn't
    /// act.
    pub fn min_switches(&mut self) -> u8 {
        self.refresh();
        self.min_switches
    }

    /// The choice an action names, as the environment decodes it: the
    /// slot-action indices with factored masks (`actions.len()` is the
    /// active slots; at team preview `actions[0]` is the order's index), or
    /// one choice index with joint masks. `None` if the mask doesn't allow
    /// it (all false when the player needn't act).
    pub fn decode(&mut self, actions: &[i64]) -> Option<Choice> {
        self.refresh();
        if !self.needs_action {
            return None;
        }
        let (layout, by) = (self.options.masks, self.options.switch_by);
        self.space.decode_player(layout, by, &self.legal, &self.own, actions)
    }

    /// Showdown's `/choose` text (what follows `/choose `) for an action
    /// (see [`ChannelSession::decode`]), e.g. `move 1 2 terastallize,
    /// switch 3` or `team 3142`. The session then waits for the next
    /// request: until then (or an `[Invalid choice]`, which asks again)
    /// the player needn't act, its mask is all false, and another choice
    /// is an error, as in the environment.
    pub fn choice(&mut self, actions: &[i64]) -> Result<String, String> {
        self.refresh();
        if !self.needs_action {
            return Err(self.not_asked());
        }
        let choice = self.decode(actions).ok_or_else(|| format!("action {actions:?} is not allowed by the mask"))?;
        Ok(self.commit(choice))
    }

    /// The `/choose` text for a choice (checked against the request's
    /// actions), when the player must act.
    pub fn choice_text(&mut self, choice: &Choice) -> Result<String, String> {
        self.refresh();
        if !self.needs_action {
            return Err(self.not_asked());
        }
        self.legal.check(choice).map_err(|e| e.to_string())?;
        Ok(self.commit(*choice))
    }

    fn commit(&mut self, choice: Choice) -> String {
        self.last_choice = Some(choice);
        // A revival's heal line names the Pokémon by name only.
        if let (OwnRequest::ForceSwitch { slots }, Choice::Actions(actions), Some(me)) =
            (self.own.request, choice, self.me)
        {
            for (slot, action) in actions.iter().enumerate().take(self.format.active_per_side()) {
                if let SlotAction::Switch(pos) = *action
                    && slots[slot]
                    && self.own.mons.get(slot).is_some_and(|m| m.reviving)
                    && let Some(m) = self.own.mons.get(pos as usize)
                {
                    self.parser.expect_revival(me, m.id);
                }
            }
        }
        self.needs_action = false;
        // Answered: waiting for the other side, or the next request.
        self.own.answered = true;
        self.fresh = false;
        format_choice(&choice, self.format.active_per_side())
    }

    /// A built-in policy's choice on this view (the environment's
    /// opponents: [`crate::policy`], [`crate::baselines`]), as `/choose`
    /// text.
    ///
    /// An error, and no choice, when the request allows none (a malformed
    /// request: no action for a slot, or none that fit together).
    pub(crate) fn choose_with(&mut self, policy: &dyn Policy, rng: &mut BattleRng) -> Result<String, String> {
        self.refresh();
        if !self.needs_action {
            return Err(self.not_asked());
        }
        let legal = self.legal;
        if !has_choice(&legal) {
            return Err("the request allows no choice".into());
        }
        let choice = policy.choose(&self.view(), &legal, rng);
        Ok(self.commit(choice))
    }

    /// A built-in policy's choice on this view (the environment's
    /// opponents: [`crate::policy`], [`crate::baselines`]), as `/choose`
    /// text, by the policy's name (`random`, `max_base_power`,
    /// `simple_heuristics`) and an RNG seed.
    ///
    /// An error, and no choice, when the request allows none (a malformed
    /// request: no action for a slot, or none that fit together).
    pub fn choose_builtin(&mut self, name: &str, seed: u64) -> Result<String, String> {
        let policy = policy_by_name(name)?;
        let mut rng = BattleRng::new(Seed::fast(seed));
        self.choose_with(&*policy, &mut rng)
    }
}

/// The series a view shows: the recorded one once the game ended.
fn series_view<'a>(series: &'a Option<Series>, recorded: &'a Option<Series>) -> Option<SeriesView<'a>> {
    let s = recorded.as_ref().or(series.as_ref())?;
    Some(SeriesView::new(s.best_of, s.game, s.wins, s.ties, &s.previous))
}

/// Whether `legal` has a choice to sample (what [`LegalActions::sample`]
/// needs): a team to order, or a jointly legal action per slot.
fn has_choice(legal: &LegalActions) -> bool {
    match legal.request() {
        Request::Wait => false,
        Request::TeamPreview { .. } => legal.team_len() > 0,
        Request::Move | Request::ForceSwitch { .. } => {
            let pass = [SlotAction::Pass];
            let second = if legal.slot(1).is_empty() { &pass[..] } else { legal.slot(1) };
            legal.slot(0).iter().any(|&a| second.iter().any(|&b| legal.is_legal(&Choice::Actions([a, b]))))
        }
    }
}

/// A built-in policy by name.
pub fn policy_by_name(name: &str) -> Result<Arc<dyn Policy>, String> {
    Ok(match name {
        "random" => Arc::new(crate::policy::RandomPolicy),
        "max_base_power" => Arc::new(crate::baselines::MaxBasePowerPolicy),
        "simple_heuristics" => Arc::new(crate::baselines::SimpleHeuristicsPolicy::new()),
        _ => return Err(format!("unknown policy {name:?} (random, max_base_power, simple_heuristics)")),
    })
}

/// The actions `me`'s request shows, from what the player knows (its
/// overlay and the public view), with the Pokémon that can switch in:
/// what `pikapro::legal::shown_actions` gives from the battle.
///
/// Per active slot of a move request: nothing but `pass` for an empty
/// slot (fainted, or never filled) or a commanding Tatsugiri; `move 1`
/// alone when recharging or locked; else each move the request doesn't
/// show disabled (Struggle's `move 1` when it shows none, or shows them
/// all disabled, which Showdown never sends: its request would show
/// Struggle), at each target
/// its type allows in doubles (a foe slot that holds a Pokémon; the
/// ally's slot when there is an ally, or always for an `adjacentAlly`
/// move; its own for `adjacentAllyOrSelf`), plain and with each gimmick
/// the request offers; then a switch to each benched Pokémon unless it
/// shows `trapped`, or the view shows the trap its `maybeTrapped` hides
/// ([`revealed_traps`]). A forced switch lists the benched Pokémon (the fainted
/// ones for Revival Blessing), and `pass` when there are fewer than the
/// holes.
pub fn shown_legal(format: &Format, own: &OwnOverlay, public: &PublicTracker, me: SideId) -> (LegalActions, u8) {
    let n_active = format.active_per_side();
    let mons = &own.mons;
    let team_len = mons.len() as u8;
    let alive = |i: usize| mons.get(i).is_some_and(|m| m.hp > 0);
    let bench: Vec<u8> = (n_active..mons.len()).filter(|&i| alive(i)).map(|i| i as u8).collect();
    let n_bench = bench.len() as u8;
    let mut slots = [SlotActions::new(); MAX_ACTIVE];
    let request = match own.request {
        OwnRequest::Move if automatic_move_choice(format, own) => Request::Wait,
        OwnRequest::Wait => Request::Wait,
        OwnRequest::TeamPreview { bring } => Request::TeamPreview { bring },
        OwnRequest::Move => Request::Move,
        OwnRequest::ForceSwitch { slots } => Request::ForceSwitch { slots },
    };
    match request {
        Request::Wait | Request::TeamPreview { .. } => {}
        Request::ForceSwitch { slots: forced_slots } => {
            let forced = forced_slots[..n_active].iter().filter(|&&f| f).count() as u8;
            for (slot, list) in slots.iter_mut().enumerate().take(n_active) {
                let must = forced_slots[slot];
                if must && mons.get(slot).is_some_and(|m| m.reviving) {
                    (0..mons.len()).filter(|&i| !alive(i)).for_each(|i| list.push(SlotAction::Switch(i as u8)));
                    if n_bench < forced {
                        list.push(SlotAction::Pass);
                    }
                    continue;
                }
                if must {
                    bench.iter().for_each(|&i| list.push(SlotAction::Switch(i)));
                    if n_bench < forced || list.is_empty() {
                        list.push(SlotAction::Pass);
                    }
                } else {
                    list.push(SlotAction::Pass);
                }
            }
        }
        Request::Move => {
            let doubles = n_active > 1;
            let shown_trap = revealed_traps(format, own, public, me);
            let foe = public.side(me.foe());
            // A foe slot holds a Pokémon that hasn't fainted there (its
            // record may say fainted when an Illusion fainted under its
            // name in the other slot).
            let foe_there = |f: usize| foe.slots.get(f).is_some_and(|a| a.row.is_some() && a.hp.hp > 0);
            for (slot, list) in slots.iter_mut().enumerate().take(n_active) {
                let (Some(a), true) = (own.active[slot], alive(slot)) else {
                    list.push(SlotAction::Pass);
                    continue;
                };
                if mons[slot].commanding {
                    list.push(SlotAction::Pass);
                    continue;
                }
                let moves = match a.moves {
                    OwnMoves::Recharge | OwnMoves::Locked(_) => {
                        list.push(SlotAction::Move { index: 0, target: None, gimmick: None });
                        continue;
                    }
                    OwnMoves::Struggle => None,
                    OwnMoves::Moves(ms) => Some(ms),
                };
                let ally = 1 - slot as u8;
                let has_ally = alive(ally as usize) && doubles;
                // Struggle takes no gimmick.
                let usable = moves.is_some_and(|ms| ms.iter().any(|m| !m.disabled));
                let gimmicks: &[Gimmick] = match (usable, a.can_terastallize.is_some(), a.can_mega_evo) {
                    (false, _, _) => &[],
                    (true, true, true) => &[Gimmick::Terastallize, Gimmick::MegaEvolve],
                    (true, true, false) => &[Gimmick::Terastallize],
                    (true, false, true) => &[Gimmick::MegaEvolve],
                    (true, false, false) => &[],
                };
                let mut push_move = |index: u8, kind: MoveTarget| {
                    let mut targets = pikapro::util::StackVec::<Option<Target>, 4>::new();
                    if doubles && kind.requires_choice() {
                        if matches!(kind, MoveTarget::Normal | MoveTarget::AdjacentFoe | MoveTarget::Any) {
                            for f in 0..2u8 {
                                if foe_there(f as usize) {
                                    targets.push(Some(Target::Foe(f)));
                                }
                            }
                        }
                        if matches!(
                            kind,
                            MoveTarget::Normal
                                | MoveTarget::Any
                                | MoveTarget::AdjacentAlly
                                | MoveTarget::AdjacentAllyOrSelf
                        ) && (has_ally || kind == MoveTarget::AdjacentAlly)
                        {
                            targets.push(Some(Target::Ally(ally)));
                        }
                        if kind == MoveTarget::AdjacentAllyOrSelf {
                            targets.push(Some(Target::Ally(slot as u8)));
                        }
                    } else {
                        targets.push(None);
                    }
                    for &target in targets.iter() {
                        list.push(SlotAction::Move { index, target, gimmick: None });
                        for &g in gimmicks {
                            if format.gimmicks.contains(g) {
                                list.push(SlotAction::Move { index, target, gimmick: Some(g) });
                            }
                        }
                    }
                };
                match moves {
                    Some(ms) if ms.iter().any(|m| !m.disabled) => {
                        for (i, m) in ms.iter().enumerate() {
                            if !m.disabled {
                                push_move(i as u8, m.target);
                            }
                        }
                    }
                    // Struggle (a malformed request that shows every move
                    // disabled too: `getMoves` would have shown Struggle).
                    _ => push_move(0, MoveTarget::RandomNormal),
                }
                // A trap the request hides (`maybeTrapped`) that the view
                // shows anyway: a revealed Shadow Tag, Arena Trap or Magnet
                // Pull ([`revealed_traps`]).
                if !a.trapped && !shown_trap[slot] {
                    bench.iter().for_each(|&i| list.push(SlotAction::Switch(i)));
                }
            }
        }
    }
    (LegalActions::from_slots(request, slots, n_active as u8, team_len, n_bench), n_bench)
}

fn automatic_move_choice(format: &Format, own: &OwnOverlay) -> bool {
    own.request == OwnRequest::Move
        && (0..format.active_per_side())
            .all(|slot| own.active[slot].is_none() || own.mons.get(slot).is_none_or(|m| m.hp == 0 || m.commanding))
}

/// What `me`'s request has revealed it, as [`shown_actions`](pikapro::legal::shown_actions)
/// takes it: the overlay's rejected choices ([`OwnOverlay::revealed`]) and
/// the traps the view shows ([`revealed_traps`]). The environment and the
/// session ([`shown_legal`]) mask with it alike.
pub fn shown_revealed(
    format: &Format,
    own: &OwnOverlay,
    public: &PublicTracker,
    me: SideId,
) -> pikapro::legal::Revealed {
    let mut revealed = own.revealed();
    for (t, v) in revealed.trapped.iter_mut().zip(revealed_traps(format, own, public, me)) {
        *t |= v;
    }
    revealed
}

/// The active slots of `me` that a foe's trapping ability the view has
/// revealed traps, though the request hides it (`maybeTrapped`): what a
/// switch's `[Unavailable choice]` would reveal (`Revealed::trapped`).
///
/// Showdown's `endTurn` runs `TrapPokemon` on each active Pokémon; a foe's
/// Shadow Tag, Arena Trap or Magnet Pull (`data/abilities.js`
/// `onFoeTrapPokemon`) calls `pokemon.tryTrap(true)` (`sim/pokemon.js`),
/// which sets the hidden `trapped = 'hidden'` unless the Pokémon is immune
/// to `trapped` (a Ghost type). The last active Pokémon's request shows
/// that only as `maybeTrapped` (`getMoveRequestData`), and `chooseSwitch`
/// refuses its switches (`side.js`: "Can't switch: The active Pokémon is
/// trapped"). Before the ability shows, the player can't tell either, so a
/// slot counts only when, with the request's `maybeTrapped`:
///
/// * an adjacent foe on the field (every foe, in these formats:
///   `isAdjacent`) shows the ability, unsuppressed (no Gastro Acid, no
///   Neutralizing Gas on the field) and not transformed;
/// * its rule holds for our Pokémon: Shadow Tag unless it has Shadow Tag
///   (`!pokemon.hasAbility('shadowtag')`), Arena Trap if it is grounded
///   for sure (`pokemon.isGrounded()`, [`pikapro::typing::is_grounded`]),
///   Magnet Pull if it is a Steel type (`hasType('Steel')`);
/// * it isn't a Ghost type (`runStatusImmunity('trapped')`), and holds no
///   Shed Shell nor (Champions) has Run Away, whose `onTrapPokemon`
///   (priority -10) frees it.
///
/// Every doubt keeps the switches: a slot is never marked that Showdown
/// would let switch, so this only drops actions the server refuses.
pub fn revealed_traps(format: &Format, own: &OwnOverlay, public: &PublicTracker, me: SideId) -> [bool; MAX_ACTIVE] {
    use pikapro_data::Type;
    use pikapro_data::gen9::{abilities as ab, items as it};
    use pikapro_view::{AbilityState, PublicVolatiles as V};
    let mut out = [false; MAX_ACTIVE];
    let n_active = format.active_per_side().min(MAX_ACTIVE);
    let mine = public.side(me);
    let foes = public.side(me.foe());
    let field = &public.field;
    // Neutralizing Gas on the field: whether it suppresses a foe's ability
    // depends on its Ability Shield, unknown; keep the switches.
    let gas = public
        .sides
        .iter()
        .any(|s| s.slots.iter().take(n_active).any(|a| a.row.is_some() && a.volatiles.has(V::NEUTRALIZING_GAS)));
    if gas {
        return out;
    }
    for (slot, flag) in out.iter_mut().enumerate().take(n_active) {
        let (Some(req), Some(mon)) = (own.active[slot], own.mons.get(slot)) else { continue };
        if !req.maybe_trapped || mon.hp == 0 {
            continue;
        }
        let Some(types) = public.types(me, slot) else { continue };
        let active = &mine.slots[slot];
        let added = active.added_type;
        let has_type = |t: Type| types.contains(t) || added == Some(t);
        if has_type(Type::Ghost) || mon.item == Some(it::SHED_SHELL) {
            continue;
        }
        if mon.ability == ab::RUN_AWAY
            && format.dex.ability(ab::RUN_AWAY).events.contains(pikapro_data::Event::TrapPokemon)
        {
            continue;
        }
        let ours_suppressed = active.volatiles.has(V::ABILITY_SUPPRESSED);
        // `isGrounded()`: our item and ability are the request's.
        let item = if field.magic_room.is_some()
            || (mon.ability == ab::KLUTZ
                && !ours_suppressed
                && mon.item.is_some_and(|i| !format.dex.item(i).ignore_klutz))
        {
            None
        } else {
            mon.item
        };
        let levitate = matches!(mon.ability, ab::LEVITATE | ab::EELEVATE) && !ours_suppressed;
        let grounded = pikapro::typing::is_grounded(&pikapro::typing::GroundFacts {
            gravity: field.gravity.is_some(),
            ingrain: active.volatiles.has(V::INGRAIN),
            smack_down: active.volatiles.has(V::SMACK_DOWN),
            magnet_rise: active.volatiles.has(V::MAGNET_RISE),
            flying: has_type(Type::Flying),
            item: Some(item),
            levitate: Some(levitate),
        });
        for f in foes.slots.iter().take(n_active) {
            let Some(row) = f.row else { continue };
            let Some(m) = foes.mons.get(row as usize) else { continue };
            if f.hp.hp == 0 || m.is_fainted() || f.transformed.is_some() || f.volatiles.has(V::ABILITY_SUPPRESSED) {
                continue;
            }
            let traps = match m.ability {
                AbilityState::Known(ab::SHADOW_TAG) => mon.ability != ab::SHADOW_TAG,
                AbilityState::Known(ab::ARENA_TRAP) => grounded == Some(true),
                AbilityState::Known(ab::MAGNET_PULL) => has_type(Type::Steel),
                _ => false,
            };
            if traps {
                *flag = true;
                break;
            }
        }
    }
    out
}
