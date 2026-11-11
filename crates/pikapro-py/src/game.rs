//! [`Game`]: a battle with what its players registered and the action
//! space, the object behind Python's `pikapro.Battle`.

use std::sync::Arc;

use pikapro::legal::{Revealed, shown_actions};
use pikapro::request::{MoveChoices, RequestData, RequestKind};
use pikapro::{Battle, BattleOptions, Choice, ChoiceError, Format, MAX_ACTIVE, Progress, Seed, SideId, SlotAction};
use pikapro_protocol::{
    BattleInfo, ChoiceFlags, ChoiceMessage, Perspective, Renderer, RequestUpdate, TeamSheet, format_choice,
    import_sheet, pack_team, side_id,
};
use pikapro_view::{OwnOverlay, OwnTeam, PlayerView, PublicTracker, ViewConfig};

use crate::space::ActionSpace;

/// Looks a format up by id.
pub fn format_by_id(id: &str) -> Result<Format, String> {
    Format::by_id(id).ok_or_else(|| {
        let known: Vec<&str> = Format::presets().iter().map(|f| f.id).collect();
        format!("unknown format {id:?} (known: {})", known.join(", "))
    })
}

/// Parses a team in Showdown's export format or packed format.
pub fn parse_team(text: &str, format: &Format) -> Result<TeamSheet, String> {
    let sheet = import_sheet(text, format).map_err(|e| format!("team: {e}"))?;
    sheet.team.validate(format).map_err(|e| format!("team: {e}"))?;
    Ok(sheet)
}

/// What rejected choices changed in a side's current request, as
/// Showdown's `emitChoiceError` with an update does
/// (`updateRequestForPokemon`, `updateDisabledRequest`): Showdown sends the
/// request again with these changes and `update: true`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestChanges {
    /// Trapped slots (`maybeTrapped` deleted, `trapped: true`) and
    /// disabled move slots (`disabled: true`).
    pub revealed: Revealed,
    /// `maybeLocked` and (in doubles) `maybeDisabled` cleared, by slot.
    pub cleared_locked: [bool; MAX_ACTIVE],
    pub cleared_disabled: [bool; MAX_ACTIVE],
    /// The request was updated since it was issued (`update: true`).
    pub updated: bool,
    /// `side.choice.cantUndo`: changing this choice would reveal hidden
    /// trapping or disabling information.
    pub cant_undo: bool,
}

/// A battle, the team sheets it was created from (for nicknames in logs and
/// choice strings, and to hand the teams back) and its format's action
/// space.
#[derive(Clone, Debug)]
pub struct Game {
    pub battle: Battle,
    pub space: Arc<ActionSpace>,
    pub sheets: Arc<[TeamSheet; 2]>,
    pub nicknames: [Vec<String>; 2],
    pub shiny: [Vec<bool>; 2],
    /// The players' names (`|player|`, `|win|`): `p1` and `p2` unless set.
    pub players: [String; 2],
    /// What rejected choices changed in each side's current request.
    pub changes: [RequestChanges; 2],
    /// Each side's own team as its requests showed it (the PP of a benched
    /// Pokémon is the last its requests showed), observed at every request
    /// the battle issues: the memory the environment's view has
    /// (`pikapro_rl::env::EnvSlot::team`, by side), so a replayed episode's
    /// `view` is the observation the environment gave.
    pub team: [OwnTeam; 2],
}

impl Game {
    pub fn new(format: Format, sheets: [TeamSheet; 2], seed: Seed, log: bool) -> Result<Game, String> {
        let space = Arc::new(ActionSpace::new(&format));
        let mut options = BattleOptions::new(seed);
        options.log_events = log;
        let teams = [sheets[0].team.clone(), sheets[1].team.clone()];
        let battle = Battle::new(format, teams, options).map_err(|e| format!("team: {e}"))?;
        Ok(Game::from_battle(battle, space, Arc::new(sheets)))
    }

    /// Wraps an existing battle (from a pool).
    pub fn from_battle(battle: Battle, space: Arc<ActionSpace>, sheets: Arc<[TeamSheet; 2]>) -> Game {
        let format = battle.format();
        let nicknames = [sheets[0].nicknames(format), sheets[1].nicknames(format)];
        let shiny = [0, 1].map(|i| sheets[i].extras.iter().map(|e| e.shiny).collect());
        let team = SideId::BOTH.map(|s| OwnTeam::from_team(&sheets[s.index()].team, format));
        let mut game = Game {
            battle,
            space,
            sheets,
            nicknames,
            shiny,
            players: ["p1".into(), "p2".into()],
            changes: [RequestChanges::default(); 2],
            team,
        };
        game.observe_requests();
        game
    }

    /// Each side's own team learns from its current request, as the
    /// environment's does after every resolution.
    fn observe_requests(&mut self) {
        for side in SideId::BOTH {
            let own = OwnOverlay::from_battle(&self.battle, side);
            self.team[side.index()].observe(&own);
        }
    }

    pub fn renderer(&self, perspective: Perspective) -> Renderer {
        let mut info = BattleInfo::for_format(self.battle.format());
        for (p, name) in info.players.iter_mut().zip(&self.players) {
            p.name.clone_from(name);
        }
        Renderer::new(self.battle.format(), perspective)
            .with_info(info)
            .with_nicknames(self.nicknames.clone())
            .with_shiny(self.shiny.clone())
            .with_pokeballs([0, 1].map(|i| self.sheets[i].pokeballs()))
    }

    /// Submits a choice. A choice the battle refuses although `side`'s
    /// request shows it (a trap by a foe's unseen ability, a move only
    /// Imprison disables) is an `[Unavailable choice]`: the request is
    /// updated as Showdown updates it.
    pub fn submit(&mut self, side: SideId, choice: Choice) -> Result<Progress, String> {
        if self.battle.request(side) == pikapro::Request::Wait {
            self.check_change(side)?;
        }
        match self.battle.submit(side, choice) {
            Ok(progress) => {
                self.submitted(progress);
                Ok(progress)
            }
            Err(ChoiceError::IllegalAction { .. })
                if shown_actions(&self.battle, side, &self.changes[side.index()].revealed).check(&choice).is_ok() =>
            {
                // Showdown's own reading of the choice gives the message and
                // the update.
                let text = format_choice(&choice, self.space.active);
                match self.choose(side, &text) {
                    Err(e) => Err(e),
                    Ok(_) => Err(format!("rejected choice for {}: {text}", side_id(side))),
                }
            }
            Err(e) => Err(format!("rejected choice for {}: {e}", side_id(side))),
        }
    }

    /// Submits a Showdown choice string, read as Showdown's `battle.choose`
    /// reads it: a rejection's message is Showdown's (`[Invalid choice]
    /// ...`, `[Unavailable choice] ...`), and an unavailable choice updates
    /// the request.
    pub fn submit_text(&mut self, side: SideId, input: &str) -> Result<Progress, String> {
        self.check_change(side)?;
        self.battle.clear_choice(side);
        let choice = self.choose(side, input)?;
        self.submit(side, choice)
    }

    fn check_change(&self, side: SideId) -> Result<(), String> {
        if self.changes[side.index()].cant_undo && !self.battle.is_over() {
            Err("[Invalid choice] Can't undo: A trapping/disabling effect would cause undo to leak information".into())
        } else {
            Ok(())
        }
    }

    fn submitted(&mut self, progress: Progress) {
        if progress == Progress::Resolved {
            self.changes = [RequestChanges::default(); 2];
            self.observe_requests();
        }
    }

    /// Reads `input` as Showdown's `battle.choose` does, applying the
    /// request updates its rejection makes (`emitChoiceError`).
    fn choose(&mut self, side: SideId, input: &str) -> Result<Choice, String> {
        let i = side.index();
        let flags = self.flags(side);
        let chosen = pikapro_protocol::choose(&self.battle, side, input, &self.nicknames[i], flags);
        self.changes[i].cant_undo = chosen.cant_undo;
        let mut error = None;
        for message in &chosen.messages {
            match message {
                ChoiceMessage::Error { message, update } => {
                    let updated = update.is_some_and(|u| self.apply_update(side, u));
                    let kind = if updated { "Unavailable" } else { "Invalid" };
                    error.get_or_insert_with(|| format!("[{kind} choice] {message}"));
                }
                ChoiceMessage::Resend { slot } => {
                    self.update_disabled_request(side, *slot as usize);
                    self.changes[i].updated = true;
                }
            }
        }
        match chosen.result {
            Ok(choice) => Ok(choice),
            Err(e) => Err(error.unwrap_or_else(|| e.to_string())),
        }
    }

    /// The `maybeLocked` / `maybeDisabled` flags Showdown's side holds now
    /// (`getMoveRequestData` again: `maybeLocked ||= maybeDisabled`).
    fn flags(&self, side: SideId) -> Option<ChoiceFlags> {
        let RequestKind::Move { active } = self.battle.request_data(side).kind else { return None };
        let c = &self.changes[side.index()];
        let mut flags = ChoiceFlags::default();
        for (slot, a) in active.iter().enumerate().take(MAX_ACTIVE) {
            if let Some(a) = a {
                let disabled = a.maybe_disabled && !c.cleared_disabled[slot];
                flags.maybe_disabled[slot] = disabled;
                flags.maybe_locked[slot] = (a.maybe_locked && !c.cleared_locked[slot]) || disabled;
            }
        }
        Some(flags)
    }

    /// Tries a rejected choice's update (`updateRequestForPokemon`):
    /// whether it changed the request.
    fn apply_update(&mut self, side: SideId, update: RequestUpdate) -> bool {
        let before = self.request_data(side);
        match update {
            RequestUpdate::Trapped { slot } => {
                self.changes[side.index()].revealed.trapped[slot as usize] = true;
            }
            RequestUpdate::Disabled { slot, move_index } => {
                self.update_disabled_request(side, slot as usize);
                self.changes[side.index()].revealed.disabled[slot as usize] |= 1 << move_index;
            }
        }
        let changed = self.request_data(side) != before;
        self.changes[side.index()].updated |= changed;
        changed
    }

    /// `side.updateDisabledRequest(pokemon, req)`: `maybeLocked` goes; in
    /// doubles `maybeDisabled` too, and every move the Pokémon can't use
    /// shows `disabled`.
    fn update_disabled_request(&mut self, side: SideId, slot: usize) {
        let doubles = self.battle.format().active_per_side() > 1;
        let RequestKind::Move { active } = self.battle.request_data(side).kind else { return };
        let Some(Some(req)) = active.get(slot).copied() else { return };
        let legal = *self.battle.legal_actions(side);
        let c = &mut self.changes[side.index()];
        let maybe_disabled = req.maybe_disabled && !c.cleared_disabled[slot];
        c.cleared_locked[slot] = true;
        if doubles && maybe_disabled {
            c.cleared_disabled[slot] = true;
            if let MoveChoices::Moves(moves) = req.moves {
                for k in 0..moves.len() {
                    let usable = legal
                        .slot(slot)
                        .iter()
                        .any(|a| matches!(a, SlotAction::Move { index, .. } if *index as usize == k));
                    if !usable {
                        c.revealed.disabled[slot] |= 1 << k;
                    }
                }
            }
        }
    }

    /// `side`'s request as Showdown shows it now: the battle's, with what
    /// rejected choices changed in it.
    pub fn request_data(&self, side: SideId) -> RequestData {
        let mut data = self.battle.request_data(side);
        let c = &self.changes[side.index()];
        if let RequestKind::Move { active } = &mut data.kind {
            for (slot, a) in active.iter_mut().enumerate() {
                let Some(a) = a else { continue };
                if c.revealed.trapped[slot] {
                    a.maybe_trapped = false;
                    a.trapped = true;
                }
                if c.cleared_locked[slot] {
                    a.maybe_locked = false;
                }
                if c.cleared_disabled[slot] {
                    a.maybe_disabled = false;
                }
                if let MoveChoices::Moves(moves) = &mut a.moves {
                    for (k, m) in moves.iter_mut().enumerate() {
                        if c.revealed.disabled[slot] >> k & 1 == 1 {
                            m.disabled = true;
                        }
                    }
                    if c.revealed.disabled[slot] != 0 && moves.iter().all(|m| m.disabled) {
                        a.can_mega_evo = false;
                        a.can_terastallize = None;
                    }
                }
            }
        }
        data
    }

    /// Parses a Showdown choice string as Showdown's `side.choose` does
    /// (without updating the request).
    pub fn parse_choice(&self, side: SideId, input: &str) -> Result<Choice, String> {
        self.check_change(side)?;
        let flags = self.flags(side);
        pikapro_protocol::choose(&self.battle, side, input, &self.nicknames[side.index()], flags)
            .result
            .map_err(|e| e.to_string())
    }

    /// `choice` as a choice string that [`Game::parse_choice`] reads back
    /// for `side`'s current request (`pikapro_protocol::format_choice_for`).
    pub fn format_choice(&self, side: SideId, choice: &Choice) -> String {
        pikapro_protocol::format_choice_for(&self.battle, side, choice, &self.nicknames[side.index()])
    }

    /// The log lines `perspective` reads for the events from `start` on.
    pub fn log(&self, perspective: Perspective, start: usize) -> Vec<String> {
        let events = self.battle.events();
        self.renderer(perspective).log(&events[start.min(events.len())..])
    }

    /// Showdown's raw stream (`|split|` blocks included) from event `start`.
    pub fn raw_log(&self, start: usize) -> Vec<String> {
        let events = self.battle.events();
        self.renderer(Perspective::Omniscient).raw(&events[start.min(events.len())..])
    }

    /// `side`'s `|request|` JSON as Showdown sends it now (after an
    /// `[Unavailable choice]`, the updated request, with `update: true`).
    pub fn request_json(&self, side: SideId) -> String {
        let json = self.renderer(Perspective::Player(side)).request_json(&self.request_data(side));
        if self.changes[side.index()].updated
            && let Some(body) = json.strip_suffix('}')
        {
            return format!("{body},\"update\":true}}");
        }
        json
    }

    /// `battle.showOpenTeamSheets()`'s lines: `|showteam|p1|<packed>` and
    /// the same for p2, each side's sets as the other player sees them
    /// (species, item, ability, moves, level, gender, Tera type, and the
    /// nature in Champions; no EVs or IVs), from `pikapro-server`'s battle
    /// stream.
    pub fn open_team_sheets(&self) -> Result<Vec<String>, String> {
        use pikapro_server::json::Json;
        use pikapro_server::{BattleStream, Message, StreamOptions};
        let format = self.battle.format();
        // `if (this.turn !== 0) return`.
        if self.battle.turn() != 0 {
            return Err("open team sheets are shown at team preview; the battle has started".into());
        }
        let mut stream = BattleStream::new(StreamOptions::default());
        let mut start = Json::object();
        start.set("formatid", Json::str(format.id));
        start.set("seed", Json::str("gen5,0001000200030004"));
        let fail = |e: pikapro_server::StreamError| e.message;
        stream.write(&format!(">start {}", start.stringify())).map_err(fail)?;
        for (side, sheet) in SideId::BOTH.into_iter().zip(self.sheets.iter()) {
            let mut player = Json::object();
            player.set("name", Json::str(self.players[side.index()].clone()));
            player.set("team", Json::str(pack_team(sheet, format)));
            stream.write(&format!(">player {} {}", side_id(side), player.stringify())).map_err(fail)?;
        }
        stream.drain();
        stream.write(">show-openteamsheets").map_err(fail)?;
        let mut lines = Vec::new();
        for m in stream.drain() {
            if let Message::Update(data) = m {
                lines.extend(data.lines().filter(|l| l.starts_with("|showteam|")).map(str::to_string));
            }
        }
        if lines.len() != 2 {
            return Err(format!("{}: no open team sheets (the battle has started, or the format has none)", format.id));
        }
        Ok(lines)
    }

    /// Runs `f` on `side`'s player view: a [`PublicTracker`] fed with every
    /// event so far (the battle must record events), the side's request as
    /// Showdown shows it now, and its own team as every request so far
    /// showed it (a benched Pokémon's PP is the last a request showed).
    pub fn with_view<R>(&self, side: SideId, f: impl FnOnce(&PlayerView<'_>) -> R) -> Result<R, String> {
        let events = self.battle.events();
        if events.is_empty() {
            return Err("the battle records no events (Battle(..., log=True))".into());
        }
        let mut tracker = PublicTracker::new(ViewConfig::new(self.battle.format()));
        tracker.apply_all(events);
        let own = OwnOverlay::from_request(&self.request_data(side));
        // The side's own team as every request so far showed it (the PP a
        // benched Pokémon had when last seen), as the environment's view
        // keeps it.
        let mut team = self.team[side.index()];
        team.observe(&own);
        Ok(f(&PlayerView::new(side, &tracker, &own).with_team(&team)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::testing::sample_team;

    #[test]
    fn open_team_sheets_come_from_the_battle_stream() {
        let format = Format::champions_vgc();
        let sheets = [TeamSheet::new(sample_team(&format)), TeamSheet::new(sample_team(&format))];
        let mut game = Game::new(format, sheets, Seed::fast(1), true).unwrap();
        game.players = ["Alice".into(), "Bob".into()];
        let lines = game.open_team_sheets().unwrap();
        assert!(lines[0].starts_with("|showteam|p1|") && lines[1].starts_with("|showteam|p2|"), "{lines:?}");
        assert!(game.log(Perspective::Omniscient, 0).iter().any(|l| l.starts_with("|player|p1|Alice|")));
        // Singles custom games have no team preview: no sheets.
        let singles = Format::gen9_singles();
        let sheets = [TeamSheet::new(sample_team(&singles)), TeamSheet::new(sample_team(&singles))];
        let game = Game::new(singles, sheets, Seed::fast(1), true).unwrap();
        assert!(game.open_team_sheets().is_err());
    }

    /// p2's Wobbuffet traps p1 with Shadow Tag, which p1's request doesn't
    /// show (`maybeTrapped`): a switch is an `[Unavailable choice]`, and the
    /// request is sent again with `trapped: true` and `update: true`.
    #[test]
    fn an_unavailable_choice_updates_the_request() {
        use pikapro::{PokemonSet, Team};
        use pikapro_data::gen9::{abilities, moves, species};
        let format = Format::gen9_singles();
        let p1 = Team::new(vec![
            PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE]),
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE]),
        ]);
        let p2 = Team::new(vec![
            PokemonSet::new(species::WOBBUFFET).with_ability(abilities::SHADOW_TAG).with_moves(&[moves::SPLASH]),
        ]);
        let mut game =
            Game::new(format, [TeamSheet::new(p1), TeamSheet::new(p2)], Seed::fast(4_600_000), true).unwrap();
        game.submit_text(SideId::P1, "move 1").unwrap();
        game.submit_text(SideId::P2, "move 1").unwrap();
        let json = game.request_json(SideId::P1);
        assert!(json.contains("\"maybeTrapped\":true") && !json.contains("update"), "{json}");
        let e = game.submit_text(SideId::P1, "switch 2").unwrap_err();
        assert_eq!(e, "[Unavailable choice] Can't switch: The active Pokémon is trapped");
        let json = game.request_json(SideId::P1);
        assert!(json.contains("\"trapped\":true") && !json.contains("maybeTrapped"), "{json}");
        assert!(json.ends_with(",\"update\":true}"), "{json}");
        // Again: nothing more to reveal, an invalid choice.
        let e = game.submit_text(SideId::P1, "switch 2").unwrap_err();
        assert!(e.starts_with("[Invalid choice]"), "{e}");
        // By index: the request now shows the trap, so the switch is
        // plainly illegal.
        let e = game.submit(SideId::P1, Choice::Actions([SlotAction::Switch(1), SlotAction::Pass])).unwrap_err();
        assert!(e.starts_with("rejected choice for p1"), "{e}");
        game.submit_text(SideId::P1, "move 1").unwrap();
        game.submit_text(SideId::P2, "move 1").unwrap();
        // A new request: shown as the battle issues it.
        assert!(!game.request_json(SideId::P1).contains("update"));
        assert_eq!(game.changes, [RequestChanges::default(); 2]);
    }

    #[test]
    fn views_come_from_the_request_and_the_log() {
        let format = Format::gen9_vgc();
        let sheets = [TeamSheet::new(sample_team(&format)), TeamSheet::new(sample_team(&format))];
        let game = Game::new(format.clone(), sheets, Seed::fast(1), true).unwrap();
        let own = game.with_view(SideId::P1, |view| view.own_mons().count()).unwrap();
        assert_eq!(own, 6);
        let unlogged = Game::new(format.clone(), game.sheets.as_ref().clone(), Seed::fast(1), false).unwrap();
        assert!(unlogged.with_view(SideId::P1, |_| ()).is_err());
    }
}
