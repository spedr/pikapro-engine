//! Showdown choice strings.
//!
//! [`parse_choice_for`] reads a choice exactly as Showdown's `side.choose`
//! does, against the battle's state: every spelling it accepts (moves and
//! switches by name, `default`, implicit passes, ...), its rejections and
//! their messages, and its numbers as JavaScript's `parseInt` reads them
//! (`switch 0x3`). It is diffed against Showdown by `pikapro-conformance`
//! (`--choices`). [`choose`] also returns everything else Showdown's
//! `battle.choose` does; [`format_choice`] writes a choice back, and
//! [`format_choice_for`] writes one that parses back to the same choice
//! against the battle (`default` for what only `autoChoose` gives).
//!
//! All numbers are 1-based, as in Showdown. `switch N` refers to the N-th
//! Pokémon in the side's current team order.

use core::fmt::{self, Write as _};
use std::borrow::Cow;

use pikapro::request::{ActiveRequest, MoveChoices, RequestData, RequestKind};
use pikapro::state::{SlotCondition, Volatiles};
use pikapro::{
    Battle, Choice, Gimmick, LegalActions, MAX_ACTIVE, PokemonState, Request, SideId, SideState, SlotAction, Target,
    TeamOrder,
};
use pikapro_data::gen9::moves;
use pikapro_data::{MoveId, MoveTarget, to_id};

use crate::js::{self, is_js_whitespace, number_to_string};

/// Formats a choice as a Showdown choice string. `active` is the number of
/// active slots in the format (trailing slots are omitted beyond it).
///
/// It knows nothing of the battle, so it writes a move with no target as
/// `move N`. In doubles a move that needs a target but has none is
/// Showdown's `autoChoose` (`default`: a random target when it runs), and
/// `move N` doesn't parse there ("needs a target");
/// [`format_choice_for`] writes such a choice with `default`.
pub fn format_choice(choice: &Choice, active: usize) -> String {
    let mut out = String::new();
    match choice {
        Choice::Team(order) => {
            out.push_str("team ");
            for &i in order.iter() {
                let _ = write!(out, "{}", i + 1);
            }
        }
        Choice::Actions(actions) => {
            for (slot, action) in actions.iter().take(active).enumerate() {
                if slot > 0 {
                    out.push_str(", ");
                }
                match *action {
                    SlotAction::Pass => out.push_str("pass"),
                    SlotAction::Switch(i) => {
                        let _ = write!(out, "switch {}", i + 1);
                    }
                    SlotAction::Move { index, target, gimmick } => {
                        let _ = write!(out, "move {}", index + 1);
                        match target {
                            Some(Target::Foe(n)) => {
                                let _ = write!(out, " {}", n + 1);
                            }
                            Some(Target::Ally(n)) => {
                                let _ = write!(out, " -{}", n + 1);
                            }
                            None => {}
                        }
                        match gimmick {
                            Some(Gimmick::Terastallize) => out.push_str(" terastallize"),
                            Some(Gimmick::MegaEvolve) => out.push_str(" mega"),
                            None => {}
                        }
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Showdown's `side.choose`, against the battle's state.

/// Why Showdown's `side.choose` would reject a choice: its message, as sent
/// in `|error|[Invalid choice] <message>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceRejected {
    /// `[Unavailable choice]`: the rejection reveals something the request
    /// did not show (a trapped Pokémon, a disabled move).
    pub unavailable: bool,
    pub message: String,
}

impl fmt::Display for ChoiceRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.unavailable { "Unavailable" } else { "Invalid" };
        write!(f, "[{kind} choice] {}", self.message)
    }
}

impl std::error::Error for ChoiceRejected {}

/// One message Showdown's `battle.choose` sends the player while
/// reading a choice (see [`choose`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChoiceMessage {
    /// `emitChoiceError`: an `|error|` line. With `update`, Showdown first
    /// tries that change on the side's active request: the line says
    /// `[Unavailable choice]` if it changed anything, and the request is
    /// then sent again (`emitRequest(activeRequest, true)`); otherwise
    /// `[Invalid choice]`.
    Error { message: String, update: Option<RequestUpdate> },
    /// `chooseMove('testfight')` with `maybeLocked`: the slot's request gets
    /// `updateDisabledRequest` and is sent again, with no error line.
    Resend { slot: u8 },
}

/// A change `emitChoiceError` tries on the active request's entry for
/// a slot (`updateRequestForPokemon`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestUpdate {
    /// A disabled move was chosen: `updateDisabledRequest`, then move
    /// `move_index` of the request gets `disabled: true` and
    /// `disabledSource: ""`.
    Disabled { slot: u8, move_index: u8 },
    /// A trapped Pokémon tried to switch: `maybeTrapped` is deleted and
    /// `trapped: true` set.
    Trapped { slot: u8 },
}

/// The flags of a side's active Pokémon that `side.choose` reads and
/// `updateDisabledRequest` clears (`pokemon.maybeLocked`,
/// `pokemon.maybeDisabled`), by slot. [`choose`] takes them from the
/// request unless given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChoiceFlags {
    pub maybe_locked: [bool; MAX_ACTIVE],
    pub maybe_disabled: [bool; MAX_ACTIVE],
}

/// Everything Showdown's `battle.choose(side, input)` does besides
/// queueing the choice, for a battle stream (`pikapro-server`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    /// What [`parse_choice_for`] returns.
    pub result: Result<Choice, ChoiceRejected>,
    /// The choice as Showdown commits it once every player's is done: the
    /// result, or a rejected choice's actions if passes complete them
    /// (`isChoiceDone` passes for fainted Pokémon).
    pub done: Option<Choice>,
    /// The messages sent to the player, in order (`Incomplete choice` and
    /// `Unknown error` included).
    pub messages: Vec<ChoiceMessage>,
    /// `side.getChoice()`: the choice as Showdown's input log records it
    /// (`move thunderbolt +1 terastallize, switch 3`, `team 1, 2, 3, 4`).
    pub input_log: String,
    /// `side.choice.cantUndo` afterwards: a hidden trap or disable was
    /// involved, so the side can neither change nor undo the choice.
    pub cant_undo: bool,
    /// The slots of the move actions chosen (of a partial choice too),
    /// whose requests `undoChoice` updates.
    pub move_slots: Vec<u8>,
}

/// [`parse_choice_for`], with everything else Showdown's
/// `battle.choose` does: every message it sends the player, the input
/// log's spelling of the choice, `cantUndo`, and the move actions of a
/// partial choice. `flags` replaces the request's `maybeLocked` and
/// `maybeDisabled` (Showdown's Pokémon flags, which an earlier rejected
/// choice may have cleared).
pub fn choose(battle: &Battle, side: SideId, input: &str, names: &[String], flags: Option<ChoiceFlags>) -> Chosen {
    let mut c = Chooser::new(battle, side, names);
    if let (Some(flags), RequestKind::Move { active }) = (flags, &mut c.request.kind) {
        for (slot, a) in active.iter_mut().enumerate() {
            if let Some(a) = a {
                a.maybe_locked = flags.maybe_locked[slot];
                a.maybe_disabled = flags.maybe_disabled[slot];
            }
        }
    }
    let ok = c.choose(input);
    let result = if !ok {
        match c.error.clone() {
            Some(e) => Err(e),
            None => {
                let message = format!(
                    "Unknown error for choice: {input}. If you're not using a custom client, please report this as a bug."
                );
                c.messages.push(ChoiceMessage::Error { message: message.clone(), update: None });
                Err(ChoiceRejected { unavailable: false, message })
            }
        }
    } else if !c.is_choice_done() {
        let message = format!("Incomplete choice: {input} - missing other pokemon");
        c.messages.push(ChoiceMessage::Error { message: message.clone(), update: None });
        Err(ChoiceRejected { unavailable: false, message })
    } else {
        Ok(c.to_choice())
    };
    // `allChoicesDone` later asks `isChoiceDone` again, which fills in
    // passes: a rejected choice may still be complete.
    let done = match &result {
        Ok(choice) => Some(*choice),
        Err(_) if !ok && c.is_choice_done() => Some(c.to_choice()),
        Err(_) => None,
    };
    let move_slots = c
        .actions
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a, Act::Slot(SlotAction::Move { .. })))
        .map(|(i, _)| i as u8)
        .collect();
    let teams: Vec<u8> = c.actions.iter().filter_map(|a| if let Act::Team(i) = a { Some(*i) } else { None }).collect();
    let input_log = if teams.len() > 1 && teams.len() == c.actions.len() {
        let positions: Vec<String> = teams.iter().map(|i| (i + 1).to_string()).collect();
        format!("team {}", positions.join(", "))
    } else {
        c.shown.join(", ")
    };
    Chosen { result, done, messages: c.messages, input_log, cant_undo: c.cant_undo, move_slots }
}

/// Parses a choice the way Showdown's `battle.choose(side, input)` does,
/// with the battle's current request and state: moves by number or name
/// (`move Protect`, `move protect 2`), switches by position, nickname or
/// species (`switch Garchomp`), `default` / `auto` (`autoChoose`), `team`
/// with any number of positions, `pass` / `skip`, the `mega` and
/// `terastallize` / `terastal` suffixes in any order with the target, the
/// implicit passes of `getChoiceIndex` (fainted, commanding and
/// non-switching Pokémon), and Showdown's rejections with its messages.
///
/// `names` gives each Pokémon's nickname by
/// [`PokemonState::id`](pikapro::PokemonState::id) (empty, or missing:
/// its base species' name), as [`TeamSheet::nicknames`](crate::TeamSheet::nicknames)
/// returns them.
///
/// The result is what Showdown would queue; [`Battle::submit`] accepts it.
/// Showdown's quirks are kept: a locked Pokémon's or a struggling one's
/// target and gimmick are dropped, `move` alone and `default` pick the
/// first usable move with no target (a random one in doubles). In singles a
/// valid target is accepted and dropped (the foe is the only one).
///
/// [`Battle::submit`]: pikapro::Battle::submit
pub fn parse_choice_for(
    battle: &Battle,
    side: SideId,
    input: &str,
    names: &[String],
) -> Result<Choice, ChoiceRejected> {
    choose(battle, side, input, names, None).result
}

/// [`format_choice`] for `side`'s current request in `battle`, written so
/// that [`parse_choice_for`] reads it back as `choice`. It is
/// [`format_choice`]'s string, except for a choice `default` made: in
/// doubles, a move that needs a target but has none (`autoChoose`'s
/// `targetLoc` 0, a random target when it runs) has no other spelling, so
/// the slots `autoChoose` filled are written `default`, which ends the
/// string (`move 4 1 terastallize, default`), as they were chosen. A choice no
/// string gives (such a move `autoChoose` wouldn't pick) keeps
/// [`format_choice`]'s spelling. `names` is the side's nickname table, as
/// for [`parse_choice_for`].
pub fn format_choice_for(battle: &Battle, side: SideId, choice: &Choice, names: &[String]) -> String {
    let active = battle.format().active_per_side();
    let plain = format_choice(choice, active);
    let Choice::Actions(actions) = choice else { return plain };
    let auto = |a: &SlotAction| matches!(a, SlotAction::Move { target: None, .. });
    if active < 2 || !actions[..active].iter().any(auto) {
        return plain;
    }
    let reads = |text: &str| parse_choice_for(battle, side, text, names).is_ok_and(|c| c == *choice);
    if reads(&plain) {
        return plain;
    }
    // `default` fills every slot left, so it ends the string; the slots
    // before it are written as they are.
    for k in (0..active).rev() {
        let text = if k == 0 { "default".to_string() } else { format!("{}, default", format_choice(choice, k)) };
        if reads(&text) {
            return text;
        }
    }
    plain
}

/// Showdown's `requestState`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestState {
    None,
    TeamPreview { picked: usize },
    Move,
    Switch { slots: [bool; MAX_ACTIVE] },
}

impl RequestState {
    fn name(self) -> &'static str {
        match self {
            RequestState::None => "",
            RequestState::TeamPreview { .. } => "teampreview",
            RequestState::Move => "move",
            RequestState::Switch { .. } => "switch",
        }
    }
}

/// What a move request lists for a move (`getMoveRequestData().moves`).
#[derive(Clone, Copy)]
struct RequestMove {
    /// `None`: `recharge`.
    id: Option<MoveId>,
    /// `target`, `None` when the request omits it (a locked move).
    target: Option<MoveTarget>,
    /// The move slot, for moves that are not locked or Struggle.
    slot: Option<u8>,
    disabled: bool,
}

/// One of Showdown's `choice.actions`.
#[derive(Clone, Copy)]
enum Act {
    Slot(SlotAction),
    Team(u8),
}

struct Chooser<'a> {
    battle: &'a Battle,
    legal: Cow<'a, LegalActions>,
    /// `getRequests`' data for the side.
    request: RequestData,
    side: SideId,
    names: &'a [String],
    state: RequestState,
    active: usize,
    actions: Vec<Act>,
    error: Option<ChoiceRejected>,
    forced_switches_left: usize,
    forced_passes_left: usize,
    switch_ins: u8,
    mega: bool,
    terastallize: bool,
    /// The messages sent (see [`choose`]).
    messages: Vec<ChoiceMessage>,
    /// `getChoice()`'s spelling of each action.
    shown: Vec<String>,
    /// `choice.cantUndo`.
    cant_undo: bool,
}

/// `/\s(?:-|\+)?[1-3]$/.test(data)`: the byte offset of the last two
/// characters (`data.slice(-2)`, the target) if it ends with a target.
fn target_suffix(data: &str) -> Option<usize> {
    let mut it = data.char_indices().rev();
    let (_, '1'..='3') = it.next()? else { return None };
    let (i, c) = it.next()?;
    let target = is_js_whitespace(c) || matches!(c, '-' | '+') && it.next().is_some_and(|(_, c)| is_js_whitespace(c));
    target.then_some(i)
}

/// Showdown's `validTargetLoc` (singles and doubles).
fn valid_target_loc(target_loc: i64, slot: usize, active: usize, target: MoveTarget) -> bool {
    if target_loc == 0 {
        return true;
    }
    let num_slots = active as i64;
    let source_loc = -(slot as i64 + 1);
    if target_loc.abs() > num_slots {
        return false;
    }
    let is_self = source_loc == target_loc;
    let is_foe = target_loc > 0;
    let across = -(num_slots + 1 - target_loc);
    let adjacent = if target_loc > 0 { (across - source_loc).abs() <= 1 } else { (target_loc - source_loc).abs() == 1 };
    match target {
        MoveTarget::RandomNormal | MoveTarget::Scripted | MoveTarget::Normal => adjacent,
        MoveTarget::AdjacentAlly => adjacent && !is_foe,
        MoveTarget::AdjacentAllyOrSelf => adjacent && !is_foe || is_self,
        MoveTarget::AdjacentFoe => adjacent && is_foe,
        MoveTarget::Any => !is_self,
        _ => false,
    }
}

impl<'a> Chooser<'a> {
    fn new(battle: &'a Battle, side: SideId, names: &'a [String]) -> Chooser<'a> {
        let active = battle.format().active_per_side();
        let s = battle.state().side(side);
        // Showdown's requestState remains active after choosing: side.choose
        // clears the previous answer before reading a replacement.
        let request = battle.issued_request(side);
        let legal = if battle.request(side) == Request::Wait && request != Request::Wait {
            Cow::Owned(pikapro::legal::issued_actions(battle, side))
        } else {
            Cow::Borrowed(battle.legal_actions(side))
        };
        let state = match request {
            Request::Wait => RequestState::None,
            Request::TeamPreview { bring } => RequestState::TeamPreview { picked: s.team.len().min(bring as usize) },
            Request::Move => RequestState::Move,
            Request::ForceSwitch { slots } => RequestState::Switch { slots },
        };
        let mut c = Chooser {
            battle,
            legal,
            request: battle.request_data(side),
            side,
            names,
            state,
            active,
            actions: Vec::new(),
            error: None,
            forced_switches_left: 0,
            forced_passes_left: 0,
            switch_ins: 0,
            mega: false,
            terastallize: false,
            messages: Vec::new(),
            shown: Vec::new(),
            cant_undo: false,
        };
        // `clearChoice`.
        if let RequestState::Switch { slots } = state {
            let can_switch_out = slots.iter().take(active).filter(|&&f| f).count();
            // (Past the active slots, except after a stale switch: see
            // `stuck`.)
            let can_switch_in = s.bench().count();
            c.forced_switches_left = can_switch_out.min(can_switch_in);
            c.forced_passes_left = can_switch_out - c.forced_switches_left;
        }
        c
    }

    fn side_state(&self) -> &'a SideState {
        self.battle.state().side(self.side)
    }

    fn legal(&self) -> &LegalActions {
        &self.legal
    }

    /// `pokemon.name`: its nickname, or its set's base species.
    fn name(&self, mon: &PokemonState) -> String {
        crate::team::name_ref(self.names, mon.id, mon.set_species, self.battle.format().dex).to_string()
    }

    fn fail(&mut self, message: impl Into<String>) -> bool {
        self.fail_with(message.into(), false, None)
    }

    /// `emitChoiceError` with a request update; `unavailable` is the
    /// verdict for [`parse_choice_for`].
    fn fail_with(&mut self, message: String, unavailable: bool, update: Option<RequestUpdate>) -> bool {
        self.messages.push(ChoiceMessage::Error { message: message.clone(), update });
        self.error = Some(ChoiceRejected { unavailable, message });
        false
    }

    /// `side.choose`.
    fn choose(&mut self, input: &str) -> bool {
        if self.state == RequestState::None {
            let m = if self.battle.is_over() {
                "Can't do anything: The game is over"
            } else {
                "Can't do anything: It's not your turn"
            };
            return self.fail(m);
        }
        let team = input.starts_with("team ");
        let count = if team { 1 } else { input.bytes().filter(|&b| b == b',').count() + 1 };
        if count > self.active {
            let game = if self.active == 1 { "singles" } else { "doubles" };
            return self
                .fail(format!("Can't make choices: You sent choices for {count} Pokémon, but this is a {game} game!"));
        }
        let strings: Vec<&str> = if team { vec![input] } else { input.split(',').collect() };
        for choice in strings {
            let trimmed = js::trim(choice);
            let (mut kind, data) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
            let mut data = js::trim(data);
            if kind == "testfight" {
                kind = "move";
                data = "testfight";
            }
            let ok = match kind {
                "move" => self.parse_move(data),
                "switch" => self.choose_switch(data),
                "shift" => {
                    if !data.is_empty() {
                        return self.fail(format!("Unrecognized data after \"shift\": {data}"));
                    }
                    self.choose_shift()
                }
                "team" => self.choose_team(data),
                "pass" | "skip" => {
                    if !data.is_empty() {
                        return self.fail(format!("Unrecognized data after \"pass\": {data}"));
                    }
                    self.choose_pass()
                }
                "auto" | "default" => self.auto_choose(),
                _ => {
                    // Showdown carries on with the next choice.
                    self.fail(format!("Unrecognized choice: {choice}"));
                    true
                }
            };
            if !ok {
                return false;
            }
        }
        self.error.is_none()
    }

    /// `choose`'s `move` arguments: the target and the gimmick, from the end.
    fn parse_move(&mut self, data: &str) -> bool {
        let original = data;
        let mut data = data.to_string();
        let mut target_loc: Option<i64> = None;
        let mut event = "";
        let conflict = |c: &mut Chooser<'_>| c.fail(format!("Conflicting arguments for \"move\": {original}"));
        loop {
            if let Some(i) = target_suffix(&data).filter(|_| to_id(&data) != "conversion2") {
                if target_loc.is_some() {
                    return conflict(self);
                }
                // `parseInt(data.slice(-2))`: a whitespace character or a
                // sign, then 1-3.
                target_loc = js::parse_int(&data[i..], 0).map(|n| n as i64);
                data = js::trim(&data[..i]).to_string();
                continue;
            }
            let suffixes: [(&str, &str); 10] = [
                (" mega", "mega"),
                (" megax", "megax"),
                (" megay", "megay"),
                (" zmove", "zmove"),
                (" ultra", "ultra"),
                (" dynamax", "dynamax"),
                (" gigantamax", "dynamax"),
                (" max", "dynamax"),
                (" terastal", "terastallize"),
                (" terastallize", "terastallize"),
            ];
            let Some(&(suffix, ev)) = suffixes.iter().find(|(s, _)| data.ends_with(s)) else {
                break;
            };
            if !event.is_empty() {
                return conflict(self);
            }
            event = ev;
            data.truncate(data.len() - suffix.len());
        }
        self.choose_move(&data, target_loc.unwrap_or(0), event)
    }

    /// `getChoiceIndex`.
    fn choice_index(&mut self, is_pass: bool) -> usize {
        let mut index = self.actions.len();
        if !is_pass {
            match self.state {
                RequestState::Move => {
                    while index < self.active && self.skips_move(index) {
                        self.choose_pass();
                        index += 1;
                    }
                }
                RequestState::Switch { slots } => {
                    while index < self.active && !slots[index] {
                        self.choose_pass();
                        index += 1;
                    }
                }
                _ => {}
            }
        }
        index
    }

    /// A fainted or commanding active Pokémon, which passes.
    fn skips_move(&self, slot: usize) -> bool {
        self.side_state().active(slot).is_none_or(|m| m.volatiles.has(Volatiles::COMMANDING))
    }

    /// `isChoiceDone`.
    fn is_choice_done(&mut self) -> bool {
        match self.state {
            RequestState::None => true,
            _ if self.forced_switches_left > 0 => false,
            RequestState::TeamPreview { picked } => self.actions.len() >= picked,
            _ => {
                self.choice_index(false);
                self.actions.len() >= self.active
            }
        }
    }

    /// What `getMoveRequestData` lists for the Pokémon in `slot`.
    fn request_moves(&self, slot: usize) -> Vec<RequestMove> {
        match self.active_request(slot).moves {
            MoveChoices::Struggle => {
                let target = Some(MoveTarget::RandomNormal);
                vec![RequestMove { id: Some(moves::STRUGGLE), target, slot: None, disabled: false }]
            }
            MoveChoices::Recharge => vec![RequestMove { id: None, target: None, slot: None, disabled: false }],
            MoveChoices::Locked(id) => vec![RequestMove { id: Some(id), target: None, slot: None, disabled: false }],
            MoveChoices::Moves(list) => list
                .iter()
                .enumerate()
                .map(|(i, m)| RequestMove {
                    id: Some(m.id),
                    target: Some(m.target),
                    slot: Some(i as u8),
                    disabled: m.disabled,
                })
                .collect(),
        }
    }

    /// `pokemon.getMoveRequestData()` for `slot` (the request data).
    fn active_request(&self, slot: usize) -> ActiveRequest {
        match &self.request.kind {
            RequestKind::Move { active } => active.get(slot).copied().flatten().unwrap_or_default(),
            _ => ActiveRequest::default(),
        }
    }

    /// `getMoves()` without `restrictData`: whether move slot `i` can be
    /// used (the legal actions list it).
    fn enabled(&self, slot: usize, i: u8) -> bool {
        self.legal().slot(slot).iter().any(|a| matches!(a, SlotAction::Move { index, .. } if *index == i))
    }

    fn move_name(&self, id: Option<MoveId>) -> &'static str {
        // `Dex.moves.get("recharge")` does not exist: its name is its id.
        id.map_or("recharge", |id| self.battle.format().dex.move_(id).name)
    }

    /// `chooseMove`.
    fn choose_move(&mut self, text: &str, target_loc: i64, event: &str) -> bool {
        if self.state != RequestState::Move {
            return self.fail(format!("Can't move: You need a {} response", self.state.name()));
        }
        let index = self.choice_index(false);
        if index >= self.active {
            return self.fail("Can't move: You sent more choices than unfainted Pokémon.");
        }
        let side = self.side_state();
        let mon = side.active(index).expect("choice_index skips empty slots");
        let name = self.name(mon);
        let request = self.request_moves(index);
        let req = self.active_request(index);
        let auto = text.is_empty();
        let locked = matches!(req.moves, MoveChoices::Recharge | MoveChoices::Locked(_));
        let struggle = matches!(req.moves, MoveChoices::Struggle);
        let mut pick: Option<RequestMove> = None;
        let mut target_type: Option<MoveTarget> = None;
        let mut testfight = false;
        let numeric = if auto { Some("1") } else { Some(text).filter(|t| t.bytes().all(|b| b.is_ascii_digit())) };
        if let Some(n) = numeric {
            // `Number(moveText) - 1`.
            let i = js::number(n) - 1.0;
            if !(0.0..request.len() as f64).contains(&i) {
                let n = number_to_string(i + 1.0);
                return self.fail(format!("Can't move: Your {name} doesn't have a move {n}"));
            }
            pick = Some(request[i as usize]);
            target_type = request[i as usize].target;
        } else {
            let mut id = to_id(text);
            if id.starts_with("hiddenpower") {
                id = "hiddenpower".into();
            }
            let matches = |m: &RequestMove| {
                let mid = m.id.map_or("recharge".to_string(), |x| to_id(self.move_name(Some(x))));
                let mid = if mid.starts_with("hiddenpower") { "hiddenpower".to_string() } else { mid };
                mid == id
            };
            match request.iter().find(|m| matches(m)) {
                Some(m) => {
                    pick = Some(*m);
                    target_type = Some(m.target.unwrap_or(MoveTarget::Normal));
                }
                None if id == "testfight" => testfight = true,
                None => return self.fail(format!("Can't move: Your {name} doesn't have a move matching {id}")),
            }
        }
        if auto {
            // The first move neither the request nor `getMoves` disables.
            let usable = |m: &&RequestMove| !m.disabled && m.slot.is_none_or(|i| self.enabled(index, i));
            if let Some(m) = request.iter().find(usable) {
                pick = Some(*m);
                target_type = m.target;
            }
        }
        let move_id = pick.and_then(|m| m.id);
        let move_name = if testfight { "testfight" } else { self.move_name(move_id) };
        let data = move_id.map(|id| self.battle.format().dex.move_(id));
        if event == "zmove" {
            // No Z-Crystals in these formats: `getZMove` finds nothing.
            return self.fail(format!("Can't move: {name} can't use {move_name} as a Z-move"));
        }
        // `getMaxMove`: Struggle stays itself, a status move becomes Max
        // Guard (`self`), any other a Max Move (`adjacentFoe`).
        let max_move = event == "dynamax";
        if max_move && data.is_none() {
            // `recharge` and `testfight` are no moves: no Max Move either.
            return self.fail(format!("Can't move: {name} can't use {move_name} as a Max Move"));
        }
        if max_move {
            target_type = Some(match data {
                _ if move_id == Some(moves::STRUGGLE) => MoveTarget::RandomNormal,
                Some(d) if d.category == pikapro_data::MoveCategory::Status => MoveTarget::User,
                _ => MoveTarget::AdjacentFoe,
            });
        }
        let mut target_loc = target_loc;
        if auto || testfight {
            target_loc = 0;
        } else if let Some(tt) = target_type.filter(|&t| t.requires_choice()) {
            if target_loc == 0 && self.active >= 2 {
                return self.fail(format!("Can't move: {move_name} needs a target"));
            }
            if !valid_target_loc(target_loc, index, self.active, tt) {
                return self.fail(format!("Can't move: Invalid target for {move_name}"));
            }
        } else if target_loc != 0 {
            return self.fail(format!("Can't move: You can't choose a target for {move_name}"));
        }
        // A locked Pokémon uses its locked move; one without a usable move
        // struggles. Targets and gimmicks are ignored.
        if locked || struggle || pick.is_some_and(|m| m.slot.is_none()) {
            // `if (pokemon.maybeLocked) this.choice.cantUndo = true`,
            // and the action's `moveid` and `targetLoc` for `getChoice`.
            self.cant_undo |= req.maybe_locked;
            let shown = match req.moves {
                MoveChoices::Recharge => self.shown_move("recharge", mon.last_move_target_loc.into(), None),
                MoveChoices::Locked(id) => {
                    let c = &mon.volatile_counters;
                    let charged = mon.volatiles.has(Volatiles::CHARGING) && c.charge_move == id;
                    let loc = if charged && c.charge_target_loc != 0 {
                        c.charge_target_loc
                    } else {
                        mon.last_move_target_loc
                    };
                    self.shown_move(&to_id(self.move_name(Some(id))), loc.into(), None)
                }
                _ => "move struggle".to_string(),
            };
            return self.push_move(0, None, None, shown);
        }
        let target = if self.active >= 2 {
            match target_loc {
                0 => None,
                l if l > 0 => Some(Target::Foe((l - 1) as u8)),
                l => Some(Target::Ally((-l - 1) as u8)),
            }
        } else {
            None
        };
        if !mon.has_usable_move() {
            // `getMoves()` is empty although the request showed moves (only
            // Imprison's hidden disables): Struggle, which the engine reads
            // from the move chosen as shown.
            let slot = pick.and_then(|m| m.slot).unwrap_or(0);
            self.cant_undo |= req.maybe_locked;
            return self.push_move(slot, target, None, "move struggle".into());
        }
        if testfight {
            // `maybeLocked` (a foe's Imprison may disable a move): a new
            // request instead.
            if req.maybe_locked {
                self.messages.push(ChoiceMessage::Resend { slot: index as u8 });
                self.error = Some(ChoiceRejected {
                    unavailable: false,
                    message: "Hack to avoid sending error messages to the client :D".into(),
                });
                return false;
            }
            return self.fail(format!("Can't move: {name}'s Fight button is known to be safe"));
        }
        let slot = pick.and_then(|m| m.slot).expect("a move slot");
        let enabled = self.enabled(index, slot);
        if max_move {
            // `maxMoveDisabled`: no PP, or a status move under Taunt or
            // with an Assault Vest.
            let m = mon.moves[slot as usize];
            let status = data.is_some_and(|d| d.category == pikapro_data::MoveCategory::Status);
            let vest = mon.item == Some(pikapro_data::gen9::items::ASSAULT_VEST);
            if m.pp == 0 || status && (vest || mon.volatiles.has(Volatiles::TAUNT)) {
                return self.fail(format!("Can't move: {name}'s {} is disabled", max_move_name(data)));
            }
        } else if !enabled {
            // `[Unavailable choice]` when `updateDisabledRequest` changes
            // the request.
            let updates = req.maybe_locked || req.maybe_disabled && self.active > 1 || !request[slot as usize].disabled;
            let message = format!("Can't move: {name}'s {move_name} is disabled");
            let update = RequestUpdate::Disabled { slot: index as u8, move_index: slot };
            return self.fail_with(message, updates, Some(update));
        }
        match event {
            "mega" if !req.can_mega_evo => {
                return self.fail(format!("Can't move: {name} can't mega evolve"));
            }
            "megax" => return self.fail(format!("Can't move: {name} can't mega evolve X")),
            "megay" => return self.fail(format!("Can't move: {name} can't mega evolve Y")),
            _ => {}
        }
        if event == "mega" && self.mega {
            return self.fail("Can't move: You can only mega-evolve once per battle");
        }
        if event == "ultra" {
            return self.fail(format!("Can't move: {name} can't ultra burst"));
        }
        if max_move {
            return self.fail("Can't move: Dynamaxing doesn't outside of Gen 8.");
        }
        if event == "terastallize" && req.can_terastallize.is_none() {
            return self.fail(format!("Can't move: {name} can't Terastallize."));
        }
        if event == "terastallize" && self.terastallize {
            return self.fail("Can't move: You can only Terastallize once per battle.");
        }
        let gimmick = match event {
            "mega" => Some(Gimmick::MegaEvolve),
            "terastallize" => Some(Gimmick::Terastallize),
            _ => None,
        };
        self.mega |= gimmick == Some(Gimmick::MegaEvolve);
        self.terastallize |= gimmick == Some(Gimmick::Terastallize);
        // `if (pokemon.maybeDisabled && gameType === 'singles')
        // this.choice.cantUndo = true`.
        self.cant_undo |= req.maybe_disabled && self.active == 1;
        let moveid = match move_id {
            Some(id) => to_id(self.move_name(Some(id))),
            None => "recharge".into(),
        };
        let shown = self.shown_move(&moveid, target_loc, gimmick);
        self.push_move(slot, target, gimmick, shown)
    }

    fn push_move(&mut self, index: u8, target: Option<Target>, gimmick: Option<Gimmick>, shown: String) -> bool {
        self.actions.push(Act::Slot(SlotAction::Move { index, target, gimmick }));
        self.shown.push(shown);
        true
    }

    /// `getChoice()` for a move action: `move <id>`, the target
    /// location in doubles, the gimmick.
    fn shown_move(&self, moveid: &str, target_loc: i64, gimmick: Option<Gimmick>) -> String {
        let mut out = format!("move {moveid}");
        if target_loc != 0 && self.active > 1 {
            let _ = write!(out, " {}{target_loc}", if target_loc > 0 { "+" } else { "" });
        }
        match gimmick {
            Some(Gimmick::MegaEvolve) => out.push_str(" mega"),
            Some(Gimmick::Terastallize) => out.push_str(" terastallize"),
            None => {}
        }
        out
    }

    /// `chooseSwitch`.
    fn choose_switch(&mut self, text: &str) -> bool {
        let switching = matches!(self.state, RequestState::Switch { .. });
        if self.state != RequestState::Move && !switching {
            return self.fail(format!("Can't switch: You need a {} response", self.state.name()));
        }
        let index = self.choice_index(false);
        if index >= self.active {
            return self.fail(if switching {
                "Can't switch: You sent more switches than Pokémon that need to switch"
            } else {
                "Can't switch: You sent more choices than unfainted Pokémon"
            });
        }
        let side = self.side_state();
        let team = &side.team;
        let revival = side.conditions.slots[index].has(SlotCondition::RevivalBlessing);
        let slot: usize = if text.is_empty() {
            if !switching {
                return self.fail("Can't switch: You need to select a Pokémon to switch in");
            }
            if revival {
                (0..team.len()).find(|&i| team[i].is_fainted()).unwrap_or(team.len())
            } else {
                if self.forced_switches_left == 0 {
                    return self.choose_pass();
                }
                let mut slot = self.active;
                while slot < team.len() && (self.switch_ins & (1 << slot) != 0 || team[slot].is_fainted()) {
                    slot += 1;
                }
                slot
            }
        } else {
            // `parseInt(slotText) - 1`, a name if `NaN` or negative.
            match js::parse_int(text, 0).map(|n| n - 1.0).filter(|&n| n >= 0.0) {
                Some(n) if n >= team.len() as f64 => {
                    let slot = number_to_string(n + 1.0);
                    return self.fail(format!("Can't switch: You do not have a Pokémon in slot {slot} to switch to"));
                }
                Some(n) => n as usize,
                None => {
                    let lower = text.to_lowercase();
                    let id = to_id(text);
                    let dex = self.battle.format().dex;
                    let found = team.iter().position(|m| {
                        lower == self.name(m).to_lowercase() || id == to_id(dex.species(species(m)).name)
                    });
                    match found {
                        Some(i) => i,
                        None => {
                            return self.fail(format!(
                                "Can't switch: You do not have a Pokémon named \"{text}\" to switch to"
                            ));
                        }
                    }
                }
            }
        };
        if slot >= team.len() {
            return self.fail(format!("Can't switch: You do not have a Pokémon in slot {} to switch to", slot + 1));
        } else if slot < self.active && !revival && !stuck(side, slot) {
            return self.fail("Can't switch: You can't switch to an active Pokémon");
        } else if self.switch_ins & (1 << slot) != 0 {
            return self.fail(format!("Can't switch: The Pokémon in slot {} can only switch in once", slot + 1));
        }
        let target = &team[slot];
        if revival {
            if !target.is_fainted() {
                return self.fail("Can't switch: You have to pass to a fainted Pokémon");
            }
            self.forced_switches_left = self.forced_switches_left.saturating_sub(1);
            self.actions.push(Act::Slot(SlotAction::Switch(slot as u8)));
            self.shown.push(format!("switch {}", slot + 1));
            return true;
        }
        if target.is_fainted() {
            return self.fail("Can't switch: You can't switch to a fainted Pokémon");
        }
        if self.state == RequestState::Move {
            let can = self.legal().slot(index).iter().any(|a| matches!(a, SlotAction::Switch(_)));
            if !can {
                // `[Unavailable choice]` when the request only said
                // `maybeTrapped`: an ability's hidden trap (`tryTrap(true)`).
                let message = "Can't switch: The active Pokémon is trapped";
                let shown = self.active_request(index).trapped;
                let update = RequestUpdate::Trapped { slot: index as u8 };
                return self.fail_with(message.into(), !shown, Some(update));
            }
            // `else if (pokemon.maybeTrapped) this.choice.cantUndo = true`.
            self.cant_undo |= self.active_request(index).maybe_trapped;
        } else {
            if self.forced_switches_left == 0 {
                return self.fail("Can't switch: Player somehow switched too many Pokemon");
            }
            self.forced_switches_left -= 1;
        }
        self.switch_ins |= 1 << slot;
        self.actions.push(Act::Slot(SlotAction::Switch(slot as u8)));
        self.shown.push(format!("switch {}", slot + 1));
        true
    }

    /// `chooseShift` (triples only).
    fn choose_shift(&mut self) -> bool {
        let index = self.choice_index(false);
        if index >= self.active {
            return self.fail(format!("Can't shift: You do not have a Pokémon in slot {}", index + 1));
        } else if self.state != RequestState::Move {
            return self.fail("Can't shift: You can only shift during a move phase");
        }
        self.fail("Can't shift: You can only shift to the center in triples")
    }

    /// `chooseTeam` (Showdown master a5df827): `TEAMSPEC` in brackets
    /// (`team [3, 1]`) is always comma-separated and never trimmed to the
    /// team size; without them, commas are required from 10 Pokémon on.
    fn choose_team(&mut self, data: &str) -> bool {
        let RequestState::TeamPreview { picked } = self.state else {
            return self.fail("Can't choose for Team Preview: You're not in a Team Preview phase");
        };
        // Not Showdown's: a second team piece (`default, team 1` or
        // ` team 12, team 34` in doubles; singles take one piece). Showdown
        // has no check and queues both pieces' actions, eight for a team of
        // four: `runAction`'s `team` then builds a team that lists Pokémon
        // twice, or the battle throws when it starts. Neither is a team, so
        // pikapro rejects the piece, with the message for a pick of the
        // wrong size. The first piece's team stays chosen, as after any
        // other rejected second piece.
        if !self.actions.is_empty() {
            return self.fail(format!("Can't choose for Team Preview: You must choose exactly {picked} Pokémon"));
        }
        let len = self.side_state().team.len();
        // `data.slice(1, -1).trim()`: the brackets are ASCII.
        let bracketed = data.len() >= 2 && data.starts_with('[') && data.ends_with(']');
        let team_data = if bracketed { js::trim(&data[1..data.len() - 1]) } else { data };
        // `parseInt(datum) - 1` for each comma-separated datum, or for each
        // UTF-16 unit (`split('')`); `None` is `NaN`.
        let position = |d: &str| js::parse_int(d, 0).map(|n| n - 1.0);
        let mut positions: Vec<Option<f64>> = if team_data.is_empty() {
            (0..len).map(|i| Some(i as f64)).collect()
        } else if bracketed || team_data.contains(',') || len >= 10 {
            team_data.split(',').map(position).collect()
        } else {
            team_data
                .chars()
                .flat_map(|c| {
                    let p = if c.len_utf16() == 1 { position(c.encode_utf8(&mut [0; 4])) } else { None };
                    core::iter::repeat_n(p, c.len_utf16())
                })
                .collect()
        };
        // `positions.splice(pickedTeamSize)` keeps the first ones only.
        if !bracketed {
            positions.truncate(picked);
        }
        if positions.len() < picked {
            for i in 0..picked {
                if !positions.contains(&Some(i as f64)) {
                    positions.push(Some(i as f64));
                }
                if positions.len() >= picked {
                    break;
                }
            }
        }
        if positions.len() != picked {
            return self.fail(format!("Can't choose for Team Preview: You must choose exactly {picked} Pokémon"));
        }
        for (index, &pos) in positions.iter().enumerate() {
            match pos {
                Some(p) if p >= 0.0 && p < len as f64 => {
                    if positions.iter().position(|&q| q == pos) != Some(index) {
                        return self.fail(format!(
                            "Can't choose for Team Preview: The Pokémon in slot {} can only switch in once",
                            p + 1.0
                        ));
                    }
                }
                p => {
                    let slot = number_to_string(p.map_or(f64::NAN, |p| p + 1.0));
                    return self
                        .fail(format!("Can't choose for Team Preview: You do not have a Pokémon in slot {slot}"));
                }
            }
        }
        for pos in positions.into_iter().flatten() {
            self.actions.push(Act::Team(pos as u8));
            self.shown.push(format!("team {}", pos as u8 + 1));
        }
        true
    }

    /// `choosePass`.
    fn choose_pass(&mut self) -> bool {
        let index = self.choice_index(true);
        if index >= self.active {
            return false;
        }
        match self.state {
            RequestState::Switch { slots } => {
                if slots[index] {
                    if self.forced_passes_left == 0 {
                        let name = self.name(&self.side_state().team[index]);
                        return self.fail(format!("Can't pass: You need to switch in a Pokémon to replace {name}"));
                    }
                    self.forced_passes_left -= 1;
                }
            }
            RequestState::Move => {
                if !self.skips_move(index) {
                    let name = self.name(&self.side_state().team[index]);
                    return self.fail(format!("Can't pass: Your {name} must make a move (or switch)"));
                }
            }
            _ => return self.fail("Can't pass: Not a move or switch request"),
        }
        self.actions.push(Act::Slot(SlotAction::Pass));
        self.shown.push("pass".into());
        true
    }

    /// `autoChoose`.
    fn auto_choose(&mut self) -> bool {
        match self.state {
            RequestState::TeamPreview { .. } => {
                if !self.is_choice_done() {
                    self.choose_team("");
                }
            }
            RequestState::Switch { .. } => {
                for _ in 0..=10 {
                    if self.is_choice_done() || !self.choose_switch("") {
                        break;
                    }
                }
            }
            RequestState::Move => {
                for _ in 0..=10 {
                    if self.is_choice_done() || !self.choose_move("", 0, "") {
                        break;
                    }
                }
            }
            RequestState::None => {}
        }
        true
    }

    fn to_choice(&self) -> Choice {
        if let RequestState::TeamPreview { .. } = self.state {
            // At most a team's worth (`choose_team` takes one piece).
            let teams = self.actions.iter().filter_map(|a| if let Act::Team(i) = a { Some(*i) } else { None });
            let order: TeamOrder = teams.take(pikapro::MAX_TEAM).collect();
            return Choice::Team(order);
        }
        let mut actions = [SlotAction::Pass; MAX_ACTIVE];
        for (slot, a) in self.actions.iter().enumerate().take(MAX_ACTIVE) {
            if let Act::Slot(action) = a {
                actions[slot] = *action;
            }
        }
        Choice::Actions(actions)
    }
}

fn max_move_name(data: Option<&pikapro_data::MoveData>) -> &'static str {
    let Some(d) = data else { return "Struggle" };
    if d.name == "Struggle" {
        return "Struggle";
    }
    if d.category == pikapro_data::MoveCategory::Status {
        return "Max Guard";
    }
    match d.ty {
        pikapro_data::Type::Flying => "Max Airstream",
        pikapro_data::Type::Dark => "Max Darkness",
        pikapro_data::Type::Fire => "Max Flare",
        pikapro_data::Type::Bug => "Max Flutterby",
        pikapro_data::Type::Water => "Max Geyser",
        pikapro_data::Type::Ice => "Max Hailstorm",
        pikapro_data::Type::Fighting => "Max Knuckle",
        pikapro_data::Type::Electric => "Max Lightning",
        pikapro_data::Type::Psychic => "Max Mindstorm",
        pikapro_data::Type::Poison => "Max Ooze",
        pikapro_data::Type::Grass => "Max Overgrowth",
        pikapro_data::Type::Ghost => "Max Phantasm",
        pikapro_data::Type::Ground => "Max Quake",
        pikapro_data::Type::Rock => "Max Rockfall",
        pikapro_data::Type::Fairy => "Max Starfall",
        pikapro_data::Type::Steel => "Max Steelspike",
        pikapro_data::Type::Dragon => "Max Wyrmwind",
        _ => "Max Strike",
    }
}

/// A healthy Pokémon at an active slot's team position that is not on the
/// field: only after a stale switch (an Eject Pack switch brought in the
/// Pokémon another slot's switch was for, and it fainted), on which
/// Showdown throws; pikapro lets it switch in again.
fn stuck(side: &SideState, slot: usize) -> bool {
    side.active.get(slot).is_some_and(Option::is_none) && !side.team[slot].is_fainted()
}

/// `pokemon.species`: the species it transformed into, if it did.
fn species(mon: &PokemonState) -> pikapro_data::SpeciesId {
    match mon.volatile_counters.transform {
        Some(t) if mon.volatiles.has(Volatiles::TRANSFORMED) => t.species,
        _ => mon.species,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::testing::sample_team;
    use pikapro::{BattleOptions, Format, Seed};

    fn battle(format: Format) -> Battle {
        let teams = [sample_team(&format), sample_team(&format)];
        Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap()
    }

    fn mv(index: u8, target: Option<Target>, gimmick: Option<Gimmick>) -> SlotAction {
        SlotAction::Move { index, target, gimmick }
    }

    #[test]
    fn reads_choices_like_showdown() {
        // Singles: Incineroar leads (Knock Off, Flamethrower, Protect, Heat Wave).
        let b = battle(Format::gen9_singles());
        let p = |s: &str| parse_choice_for(&b, SideId::P1, s, &[]);
        assert_eq!(p("move Protect"), Ok(Choice::single(mv(2, None, None))));
        assert_eq!(p("move heatwave terastal"), Ok(Choice::single(mv(3, None, Some(Gimmick::Terastallize)))));
        // A valid target is taken and dropped in singles.
        assert_eq!(p("move 1 1"), Ok(Choice::single(mv(0, None, None))));
        assert_eq!(p("default"), Ok(Choice::single(mv(0, None, None))));
        assert_eq!(p("skip").unwrap_err().message, "Can't pass: Your Incineroar must make a move (or switch)");
        assert_eq!(p("switch garchomp"), Ok(Choice::single(SlotAction::Switch(3))));
        assert_eq!(p("switch Flutter Mane"), Ok(Choice::single(SlotAction::Switch(2))));
        assert_eq!(p("move 5").unwrap_err().message, "Can't move: Your Incineroar doesn't have a move 5");
        assert_eq!(p("move 1 2").unwrap_err().message, "Can't move: Invalid target for Knock Off");
        assert_eq!(p("move 1 mega").unwrap_err().message, "Can't move: Incineroar can't mega evolve");
        assert_eq!(
            p("move 1 mega terastallize").unwrap_err().message,
            "Conflicting arguments for \"move\": 1 mega terastallize"
        );
        assert_eq!(p("Move 1").unwrap_err().message, "Unrecognized choice: Move 1");
        assert_eq!(p("switch 1").unwrap_err().message, "Can't switch: You can't switch to an active Pokémon");
        let names = ["Blaze".to_string()];
        assert_eq!(
            parse_choice_for(&b, SideId::P1, "switch 1", &names).unwrap_err().to_string(),
            "[Invalid choice] Can't switch: You can't switch to an active Pokémon"
        );

        // Doubles: Incineroar and Rillaboom.
        let b = battle(Format::gen9_doubles());
        let p = |s: &str| parse_choice_for(&b, SideId::P1, s, &[]);
        assert_eq!(
            p("move 1 +2, move uturn 1"),
            Ok(Choice::doubles(mv(0, Some(Target::Foe(1)), None), mv(3, Some(Target::Foe(0)), None)))
        );
        assert_eq!(p("move 1").unwrap_err().message, "Can't move: Knock Off needs a target");
        assert_eq!(p("move 1 1").unwrap_err().message, "Incomplete choice: move 1 1 - missing other pokemon");
        // `default` picks the first move with no target (a random one).
        assert_eq!(p("move 1 -2, default"), Ok(Choice::doubles(mv(0, Some(Target::Ally(1)), None), mv(0, None, None))));
        assert!(b.legal_actions(SideId::P1).is_legal(&p("default").unwrap()));
        assert_eq!(
            p("move 1 1 terastallize, move 1 1 terastallize").unwrap_err().message,
            "Can't move: You can only Terastallize once per battle."
        );
        assert_eq!(
            p("switch 3, switch 3").unwrap_err().message,
            "Can't switch: The Pokémon in slot 3 can only switch in once"
        );

        // Team preview (VGC: bring 4 of 6), filled up like `chooseTeam`.
        let b = battle(Format::gen9_vgc());
        let p = |s: &str| parse_choice_for(&b, SideId::P1, s, &[]);
        assert_eq!(p("team 3"), Ok(Choice::team(&[2, 0, 1, 3])));
        assert_eq!(p("team 6, 5"), Ok(Choice::team(&[5, 4, 0, 1])));
        assert_eq!(p("default"), Ok(Choice::team(&[0, 1, 2, 3])));
        assert_eq!(
            p("team 7").unwrap_err().message,
            "Can't choose for Team Preview: You do not have a Pokémon in slot 7"
        );
        assert_eq!(
            p("team 1 2").unwrap_err().message,
            "Can't choose for Team Preview: You do not have a Pokémon in slot NaN"
        );
        // Master's brackets: comma-separated, filled up but never trimmed.
        assert_eq!(p("team [3, 1]"), Ok(Choice::team(&[2, 0, 1, 3])));
        assert_eq!(p("team [ 6,5,4,3 ]"), Ok(Choice::team(&[5, 4, 3, 2])));
        assert_eq!(
            p("team [31]").unwrap_err().message,
            "Can't choose for Team Preview: You do not have a Pokémon in slot 31"
        );
        for s in ["team [1, 2, 3, 4, 5]", "team []"] {
            assert_eq!(p(s).unwrap_err().message, "Can't choose for Team Preview: You must choose exactly 4 Pokémon");
        }
        assert_eq!(p("team 1, 2, 3, 4, 5"), Ok(Choice::team(&[0, 1, 2, 3])));
        assert_eq!(p("move 1").unwrap_err().message, "Can't move: You need a teampreview response");
    }

    /// `choose`'s messages, input-log spelling and partial choices.
    #[test]
    fn choose_reports_what_showdown_sends() {
        let b = battle(Format::gen9_doubles());
        let c = |s: &str| choose(&b, SideId::P1, s, &[], None);
        let ok = c("move 1 +2, move uturn 1 terastal");
        assert_eq!(ok.input_log, "move knockoff +2, move uturn +1 terastallize");
        assert_eq!((ok.done, ok.messages.len(), ok.move_slots.as_slice()), (ok.result.clone().ok(), 0, &[0, 1][..]));
        // Showdown carries on after an unrecognized choice: two errors.
        let two = c("foo, bar");
        let errors: Vec<_> = two
            .messages
            .iter()
            .map(|m| match m {
                ChoiceMessage::Error { message, update: None } => message.as_str(),
                _ => panic!("{m:?}"),
            })
            .collect();
        assert_eq!(errors, ["Unrecognized choice: foo", "Unrecognized choice:  bar"]);
        assert!(two.done.is_none());
        let partial = c("switch 3");
        assert_eq!(partial.result.unwrap_err().message, "Incomplete choice: switch 3 - missing other pokemon");
        assert_eq!(partial.input_log, "switch 3");
        let b = battle(Format::gen9_vgc());
        assert_eq!(choose(&b, SideId::P1, "team 3", &[], None).input_log, "team 3, 1, 2, 4");
    }

    #[test]
    fn submitted_choices_can_be_replaced_against_the_issued_request() {
        for format in Format::presets() {
            for side in SideId::BOTH {
                let mut b = battle(format.clone());
                let first = parse_choice_for(&b, side, "default", &[]).unwrap();
                assert_eq!(b.submit(side, first), Ok(pikapro::Progress::Waiting));
                assert_eq!(b.request(side), Request::Wait);
                // Parsing is read-only, including after a submission. The
                // text consumer clears the queued choice before parsing.
                assert_eq!(parse_choice_for(&b, side, "default", &[]), Ok(first));
                let input =
                    if matches!(b.issued_request(side), Request::TeamPreview { .. }) { "team 0" } else { "move 9" };
                let expected = {
                    let mut unchosen = b.clone();
                    unchosen.clear_choice(side);
                    choose(&unchosen, side, input, &[], None)
                };
                assert!(expected.result.is_err());
                assert_eq!(choose(&b, side, input, &[], None), expected, "{} {side:?}", format.id);
                assert_eq!(b.submit(side, first), Ok(pikapro::Progress::Waiting));
            }
        }
    }

    /// Every pair of choice pieces, at every request kind, in singles and
    /// doubles: no panic, an accepted choice is one the battle takes, and a
    /// team order never outgrows the team.
    #[test]
    fn two_piece_choices_never_panic() {
        use pikapro::BattleRng;
        const PIECES: [&str; 22] = [
            "default",
            "auto",
            "team",
            "team 1",
            "team 12",
            "team 4321",
            "team 654321",
            "team [1,2,3,4]",
            "team [4, 3, 2, 1, 5, 6]",
            "move",
            "move 1",
            "move 1 1",
            "move 2 -1 terastallize",
            "switch",
            "switch 2",
            "switch 3",
            "pass",
            "skip",
            "shift",
            "testfight",
            "foo",
            "",
        ];
        let check = |b: &Battle, side: SideId| {
            let mut checked = 0;
            for first in PIECES {
                for second in PIECES {
                    // The leading space keeps a first `team` piece from
                    // taking the whole input.
                    for input in
                        [format!("{first}, {second}"), format!("{first},{second}"), format!(" {first}, {second}")]
                    {
                        let chosen = choose(b, side, &input, &[], None);
                        let context = format!("{} {input:?}: {chosen:?}", b.format().id);
                        match &chosen.result {
                            Ok(choice) => assert_eq!(chosen.done, Some(*choice), "{context}"),
                            Err(_) => assert!(!chosen.messages.is_empty(), "{context}"),
                        }
                        // What a battle stream commits, of a rejected
                        // choice too.
                        if let Some(done) = chosen.done {
                            assert!(b.clone().submit(side, done).is_ok(), "{context}");
                        }
                        if let (Some(Choice::Team(order)), Request::TeamPreview { bring }) =
                            (&chosen.done, b.request(side))
                        {
                            assert_eq!(order.len(), b.state().side(side).team.len().min(bring as usize), "{context}");
                        }
                        checked += 1;
                    }
                }
            }
            checked
        };
        for format in Format::presets() {
            // 0: team preview, 1: move, 2: forced switch.
            let mut seen = [0usize; 3];
            for seed in 0..40u64 {
                let teams = [sample_team(format), sample_team(format)];
                let mut b =
                    Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(8_400_000 + seed))).unwrap();
                let mut rng = BattleRng::new(Seed::fast(8_400_100 + seed));
                while !b.is_over() && b.turn() < 60 {
                    for side in SideId::BOTH {
                        if b.is_over() {
                            break;
                        }
                        let kind = match b.request(side) {
                            Request::Wait => continue,
                            Request::TeamPreview { .. } => 0,
                            Request::Move => 1,
                            Request::ForceSwitch { .. } => 2,
                        };
                        // Each battle's first request of a kind, and a few
                        // more of the rarer ones.
                        if seen[kind] < 6 {
                            seen[kind] += 1;
                            assert_eq!(check(&b, side), PIECES.len() * PIECES.len() * 3);
                        }
                        let sampled = b.legal_actions(side).sample(&mut rng);
                        b.submit(side, sampled).unwrap();
                    }
                }
            }
            assert_eq!(seen[0] > 0, format.team_preview.shown(), "{}", format.id);
            assert!(seen[1] > 0 && seen[2] > 0, "{}: {seen:?}", format.id);
        }
    }

    /// A second `team` piece: Showdown (a5df827) queues both, eight team
    /// actions for a team of four, and then plays a team with duplicates
    /// or throws (see `choose_team`). pikapro rejects it.
    #[test]
    fn a_second_team_choice_is_rejected() {
        let exactly = "Can't choose for Team Preview: You must choose exactly 4 Pokémon";
        for format in [Format::gen9_vgc(), Format::champions_vgc()] {
            let b = battle(format);
            let c = |s: &str| choose(&b, SideId::P1, s, &[], None);
            for input in ["default, team 1", "auto, team 1", "default,team 1", " team 12, team 34", " team 1,team 2"] {
                let chosen = c(input);
                assert_eq!(chosen.result.clone().unwrap_err().message, exactly, "{input}");
                assert!(!chosen.result.unwrap_err().unavailable);
                // The first piece's team stays chosen, as after any other
                // rejected second piece.
                assert_eq!(chosen.done.map(|d| matches!(d, Choice::Team(o) if o.len() == 4)), Some(true), "{input}");
                assert_eq!(chosen.messages.len(), 1, "{input}");
            }
            assert_eq!(c(" team 12, team 34").input_log, "team 1, 2, 3, 4");
            // What Showdown does with the other second pieces.
            assert_eq!(c("default, default").result, Ok(Choice::team(&[0, 1, 2, 3])));
            assert_eq!(c(" team 21, default").result, Ok(Choice::team(&[1, 0, 2, 3])));
            assert_eq!(
                c("team 1, default").result.unwrap_err().message,
                "Can't choose for Team Preview: You do not have a Pokémon in slot NaN"
            );
            assert_eq!(c("pass, team 1").result.unwrap_err().message, "Can't pass: Not a move or switch request");
            assert_eq!(c("foo, team 1").result.unwrap_err().message, "Unrecognized choice: foo");
            assert_eq!(c("foo, team 1").done, Some(Choice::team(&[0, 1, 2, 3])));
            assert_eq!(c("default, move 1").result.unwrap_err().message, "Can't move: You need a teampreview response");
            assert_eq!(
                c("default, switch 2").result.unwrap_err().message,
                "Can't switch: You need a teampreview response"
            );
            assert_eq!(
                c("default, pass").result.unwrap_err().message,
                "Unknown error for choice: default, pass. If you're not using a custom client, please report this as a bug."
            );
            assert_eq!(
                c("default, shift").result.unwrap_err().message,
                "Can't shift: You do not have a Pokémon in slot 5"
            );
            assert_eq!(
                c(" team [1,2,3,4], team 1").result.unwrap_err().message,
                "Can't make choices: You sent choices for 5 Pokémon, but this is a doubles game!"
            );
        }
        // Singles: one piece only.
        let b = battle(Format::champions_bss());
        assert_eq!(
            choose(&b, SideId::P1, "default, team 1", &[], None).result.unwrap_err().message,
            "Can't make choices: You sent choices for 2 Pokémon, but this is a singles game!"
        );
    }

    #[test]
    fn numbers_are_read_like_parse_int() {
        // Expected: Showdown 0.11.11's `side.choose`.
        let b = battle(Format::gen9_singles());
        let p = |s: &str| parse_choice_for(&b, SideId::P1, s, &[]);
        assert_eq!(p("switch 0x4"), Ok(Choice::single(SlotAction::Switch(3))));
        assert_eq!(p("switch 4abc"), Ok(Choice::single(SlotAction::Switch(3))));
        assert_eq!(p("\u{feff}switch 4\u{3000}"), Ok(Choice::single(SlotAction::Switch(3))));
        assert_eq!(p("move 1\u{3000}1"), Ok(Choice::single(mv(0, None, None))));
        assert_eq!(
            p("switch 123456789012345678901234567890").unwrap_err().message,
            "Can't switch: You do not have a Pokémon in slot 1.2345678901234568e+29 to switch to"
        );
        assert_eq!(
            p("move 99999999999999999999").unwrap_err().message,
            "Can't move: Your Incineroar doesn't have a move 100000000000000000000"
        );
        assert_eq!(p("move 0").unwrap_err().message, "Can't move: Your Incineroar doesn't have a move 0");
        assert_eq!(
            p("switch \u{85}4").unwrap_err().message,
            "Can't switch: You do not have a Pokémon named \"\u{85}4\" to switch to"
        );
        let b = battle(Format::gen9_vgc());
        let p = |s: &str| parse_choice_for(&b, SideId::P1, s, &[]);
        assert_eq!(p("team 0x2, 1"), Ok(Choice::team(&[1, 0, 2, 3])));
        assert_eq!(
            p("team \u{1f600}1").unwrap_err().message,
            "Can't choose for Team Preview: You do not have a Pokémon in slot NaN"
        );
        assert_eq!(
            p("team 99999999999999999999999, 1").unwrap_err().message,
            "Can't choose for Team Preview: You do not have a Pokémon in slot 1e+23"
        );
    }

    #[test]
    fn round_trips() {
        let singles = battle(Format::gen9_singles());
        let doubles = battle(Format::gen9_doubles());
        let vgc = battle(Format::gen9_vgc());
        for (b, s) in [
            (&doubles, "move 1 2 terastallize, switch 3"),
            (&doubles, "move 2 -2, move 4 1"),
            (&singles, "switch 4"),
            (&vgc, "team 1234"),
        ] {
            let active = b.format().active_per_side();
            assert_eq!(format_choice(&parse_choice_for(b, SideId::P1, s, &[]).unwrap(), active), s);
        }
    }

    /// Parse, write, parse: [`format_choice_for`]'s string reads back as
    /// the same choice, for random legal choices and for `default` (alone
    /// and after a first slot's action) in every format, through whole
    /// battles. In doubles `default` leaves a targeted move without a
    /// target, which only `default` writes.
    #[test]
    fn choices_round_trip_through_format_choice_for() {
        use pikapro::BattleRng;
        for format in [Format::gen9_singles(), Format::gen9_doubles(), Format::gen9_vgc(), Format::champions_vgc()] {
            let active = format.active_per_side();
            let (mut checked, mut auto) = (0, 0);
            for seed in 0..12u64 {
                let teams = [sample_team(&format), sample_team(&format)];
                let options = BattleOptions::new(Seed::fast(8_300_000 + seed));
                let mut b = Battle::new(format.clone(), teams, options).unwrap();
                let mut rng = BattleRng::new(Seed::fast(8_300_100 + seed));
                while !b.is_over() && b.turn() < 80 {
                    for side in SideId::BOTH {
                        if b.is_over() || b.legal_actions(side).is_waiting() {
                            continue;
                        }
                        let sampled = b.legal_actions(side).sample(&mut rng);
                        let first = format_choice(&sampled, 1);
                        let inputs = [format_choice(&sampled, active), "default".into(), format!("{first}, default")];
                        for input in &inputs {
                            let Ok(c) = parse_choice_for(&b, side, input, &[]) else { continue };
                            let text = format_choice_for(&b, side, &c, &[]);
                            assert_eq!(
                                parse_choice_for(&b, side, &text, &[]),
                                Ok(c),
                                "{}: {input:?} was written {text:?}",
                                format.id
                            );
                            checked += 1;
                            auto += usize::from(text != format_choice(&c, active));
                        }
                        b.submit(side, sampled).unwrap();
                    }
                }
            }
            assert!(checked > 500, "{}: {checked}", format.id);
            // Doubles reach choices only `default` writes.
            assert_eq!(auto > 0, active > 1, "{}: {auto}", format.id);
        }
    }
}
