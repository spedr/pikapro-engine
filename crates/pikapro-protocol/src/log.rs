//! Rendering [`BattleEvent`]s as Showdown protocol lines.
//!
//! Output follows <https://github.com/smogon/pokemon-showdown/blob/master/sim/SIM-PROTOCOL.md>
//! and is checked line for line against Showdown by `pikapro-conformance`
//! too: the header, blank lines, `|split|` blocks and every annotation.
//!
//! Showdown's log is one stream in which a line whose content depends on
//! who reads it (exact HP) is written as a `|split|pN` block: the secret
//! line, then the shared one. [`Renderer::raw`] gives that stream;
//! [`Renderer::log`] gives what one [`Perspective`] reads, as Showdown's
//! `getPlayerStreams` / `extractChannelMessages` would deliver it.

use core::cell::Cell;
use core::fmt::{self, Write as _};

use pikapro::event::{
    AbilityNote, Actor, CantReason, Cause, DamageSource, EffectName, FailNote, FieldEffect, HealSource, Hint,
    ItemEndNote, LineArg, Message, MoveAnim, MoveAttr, MoveFrom, Volatile, VolatileEffect,
};
use pikapro::format::GameType;
use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro::{BattleEvent, Format, Mechanics, SideId, Status};
use pikapro_data::gen9::moves;
use pikapro_data::{Dex, SpeciesId};

use crate::TeamSheet;

/// The capacity a rendered line starts with.
const LINE_CAPACITY: usize = 64;

/// Whose eyes the log is rendered for: Showdown's channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Perspective {
    /// Exact HP for everyone (replays, debugging, the conformance oracle):
    /// Showdown's omniscient channel (`-1`).
    Omniscient,
    /// Exact HP for this side's Pokémon, percentages for the foe's: the
    /// player's channel (`1`, `2`).
    Player(SideId),
    /// Percentages for everyone: the spectator channel (`0`).
    Spectator,
}

impl Perspective {
    /// Whether this perspective reads the secret line of a `|split|` block
    /// for `side`.
    fn sees_secret(self, side: SideId) -> bool {
        match self {
            Perspective::Omniscient => true,
            Perspective::Player(me) => me == side,
            Perspective::Spectator => false,
        }
    }
}

/// One entry of Showdown's log, as `battle.add` writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A line every channel reads.
    Line(String),
    /// A `|split|<side>` block: the line `side` (and the omniscient log)
    /// reads, then the one everyone else reads (empty: nothing).
    Split { side: SideId, secret: String, shared: String },
}

impl Entry {
    /// Appends the entry's lines in the raw stream, moving them.
    fn into_raw(self, out: &mut Vec<String>) {
        match self {
            Entry::Line(line) => out.push(line),
            Entry::Split { side, secret, shared } => {
                out.push(format!("|split|{}", side_id(side)));
                out.push(secret);
                out.push(shared);
            }
        }
    }

    /// The line `perspective` reads, if any (Showdown's
    /// `extractChannelMessages` drops an empty shared line), moving it.
    fn into_line_for(self, perspective: Perspective) -> Option<String> {
        match self {
            Entry::Line(line) => Some(line),
            Entry::Split { side, secret, shared } => split_line(perspective, side, secret, shared),
        }
    }
}

/// The line of a `|split|` block `perspective` reads: the secret one if it
/// sees `side`'s secrets, else the shared one, unless that is empty.
fn split_line<T: AsRef<str>>(perspective: Perspective, side: SideId, secret: T, shared: T) -> Option<T> {
    let line = if perspective.sees_secret(side) { secret } else { shared };
    (!line.as_ref().is_empty()).then_some(line)
}

/// Showdown's `extractChannelMessages` for one channel: reads a raw stream
/// (`|split|pN` blocks included, as [`Renderer::raw`] writes it or as
/// Showdown's `update` chunks carry it) and keeps what `perspective`
/// reads. Empty lines are skipped, as Showdown's `splitRegex` does.
pub fn extract_channel<S: AsRef<str>>(raw: &[S], perspective: Perspective) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let line = raw[i].as_ref();
        let side = match line.strip_prefix("|split|") {
            Some("p1") => Some(SideId::P1),
            Some("p2") => Some(SideId::P2),
            _ => None,
        };
        match side {
            Some(side) if i + 1 < raw.len() => {
                let secret = raw[i + 1].as_ref();
                let shared = raw.get(i + 2).map_or("", |l| l.as_ref());
                let chosen = if perspective.sees_secret(side) { secret } else { shared };
                if !chosen.is_empty() {
                    out.push(chosen.to_string());
                }
                i += 3;
            }
            _ => {
                if !line.is_empty() {
                    out.push(line.to_string());
                }
                i += 1;
            }
        }
    }
    out
}

/// A player as the `|player|` line introduces them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerInfo {
    /// The name in `|player|`, `|win|` and side idents (`p1: <name>`).
    pub name: String,
    pub avatar: String,
    pub rating: String,
}

/// What Showdown writes that the battle itself does not know: the header
/// and the presentation settings of the Showdown format being mirrored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BattleInfo {
    pub players: [PlayerInfo; 2],
    /// The time `|t:|` lines show (seconds since the epoch). Core has no
    /// clock.
    pub timestamp: u64,
    /// `|gen|`.
    pub generation: u8,
    /// `|tier|`: the format's name.
    pub tier: String,
    /// `|rule|` lines the format's rules write as the battle begins.
    pub rules: Vec<String>,
    /// A custom game's custom rules (Showdown's `format.customRules`),
    /// listed in a `|raw|` infobox.
    pub custom_rules: Vec<String>,
    /// Open Team Sheets: the `|uhtml|otsrequest|` buttons at team preview.
    pub open_team_sheets: bool,
    /// Showdown's `reportExactHP` (debug formats such as custom games): the
    /// shared line of a `|split|` block shows exact HP too.
    pub report_exact_hp: bool,
}

impl BattleInfo {
    /// The header of the Showdown format each preset is checked against
    /// (see `docs/CONFORMANCE.md`), with players `p1` and `p2`:
    /// `gen9singles` and `gen9doubles` are custom games without team
    /// preview, `gen9vgc` a doubles custom game with `Picked Team Size = 4`
    /// and `Adjust Level Down = 50`, `championsvgc` `[Gen 9 Champions] VGC
    /// 2026 Reg M-C`, `championsbss` `[Gen 9 Champions] BSS Reg M-C`,
    /// `gen9ou` `[Gen 9] OU` (with its eight `|rule|` lines), `gen9randombattle`
    /// and `gen9randomdoublesbattle` Showdown's random battles. Other
    /// formats get their id as the tier.
    pub fn for_format(format: &Format) -> BattleInfo {
        let player = |name: &str| PlayerInfo { name: name.into(), ..PlayerInfo::default() };
        let mut info = BattleInfo {
            players: [player("p1"), player("p2")],
            timestamp: 0,
            generation: 9,
            tier: format.id.to_string(),
            rules: Vec::new(),
            custom_rules: Vec::new(),
            open_team_sheets: false,
            report_exact_hp: false,
        };
        let custom = |info: &mut BattleInfo, tier: &str, rules: &[&str]| {
            info.tier = tier.into();
            info.custom_rules = rules.iter().map(|r| r.to_string()).collect();
            info.report_exact_hp = true;
        };
        match format.id {
            "gen9singles" => custom(&mut info, "[Gen 9] Custom Game", &["!Team Preview"]),
            "gen9doubles" => custom(&mut info, "[Gen 9] Doubles Custom Game", &["!Team Preview"]),
            "gen9vgc" => {
                custom(&mut info, "[Gen 9] Doubles Custom Game", &["Picked Team Size = 4", "Adjust Level Down = 50"])
            }
            "championsvgc" => return info.with_flat_rules("[Gen 9 Champions] VGC 2026 Reg M-C", true),
            "gen9ou" => {
                // The `onBegin` messages of its rules, in the rule table's
                // order (HP Percentage Mod and Endless Battle Clause from
                // Standard AG, then Standard's and the format's clauses).
                info.tier = "[Gen 9] OU".into();
                info.rules = [
                    "HP Percentage Mod: HP is shown in percentages",
                    "Endless Battle Clause: Forcing endless battles is banned",
                    "Species Clause: Limit one of each Pokémon",
                    "OHKO Clause: OHKO moves are banned",
                    "Evasion Items Clause: Evasion items are banned",
                    "Evasion Moves Clause: Evasion moves are banned",
                    "Evasion Abilities Clause: Evasion abilities are banned",
                    "Sleep Moves Clause: Sleep-inducing moves are banned",
                ]
                .map(String::from)
                .to_vec();
            }
            // Each rule's `onBegin`, in the rule table's order (Cancel
            // Mod and Obtainable add none; PotD's only with a Pokémon of
            // the day, which a battle stream has none of).
            "gen9randombattle" => {
                info.tier = "[Gen 9] Random Battle".into();
                info.rules = [
                    "Species Clause: Limit one of each Pokémon",
                    "HP Percentage Mod: HP is shown in percentages",
                    "Sleep Clause Mod: Limit one foe put to sleep",
                    "Illusion Level Mod: Illusion disguises the Pokémon's true level",
                ]
                .map(String::from)
                .to_vec();
            }
            "gen9randomdoublesbattle" => {
                info.tier = "[Gen 9] Random Doubles Battle".into();
                info.rules = [
                    "Species Clause: Limit one of each Pokémon",
                    "HP Percentage Mod: HP is shown in percentages",
                    "Illusion Level Mod: Illusion disguises the Pokémon's true level",
                    "Sleep Clause Mod: Limit one foe put to sleep",
                ]
                .map(String::from)
                .to_vec();
            }
            // Flat Rules and VGC Timer, without Open Team Sheets.
            "championsbss" => return info.with_flat_rules("[Gen 9 Champions] BSS Reg M-C", false),
            _ => {}
        }
        info
    }

    /// The header of a format played under Flat Rules (VGC, Battle Stadium
    /// Singles), named `tier`: the `onBegin` messages of its Species Clause
    /// and Item Clause, no custom rules, HP not reported exactly (not a
    /// debug format), and Open Team Sheets' buttons at team preview if
    /// `open_team_sheets`.
    pub fn with_flat_rules(mut self, tier: &str, open_team_sheets: bool) -> BattleInfo {
        self.tier = tier.into();
        self.rules =
            vec!["Species Clause: Limit one of each Pokémon".into(), "Item Clause: Limit 1 of each item".into()];
        self.custom_rules = Vec::new();
        self.report_exact_hp = false;
        self.open_team_sheets = open_team_sheets;
        self
    }
}

/// The Open Team Sheets rule's `onTeamPreview` message.
const OTS_REQUEST: &str = "|uhtml|otsrequest|<button name=\"send\" value=\"/acceptopenteamsheets\" class=\"button\" style=\"margin-right: 10px;\"><strong>Accept Open Team Sheets</strong></button><button name=\"send\" value=\"/rejectopenteamsheets\" class=\"button\" style=\"margin-top: 10px\"><strong>Deny Open Team Sheets</strong></button>";

/// Renders events for one perspective.
#[derive(Clone, Debug)]
pub struct Renderer {
    pub(crate) dex: &'static Dex,
    pub(crate) mechanics: Mechanics,
    game_type: GameType,
    team_preview: bool,
    perspective: Perspective,
    /// The header and presentation settings (player names, tier, ...).
    pub info: BattleInfo,
    /// Nicknames per side, indexed by [`PokemonState::id`](pikapro::PokemonState::id)
    /// (the registration index). An empty or missing name means the
    /// Pokémon is named after its base species, as Showdown does.
    pub nicknames: [Vec<String>; 2],
    /// Which Pokémon are shiny (`, shiny` in details), per side, indexed
    /// like [`Renderer::nicknames`] (missing: not shiny).
    pub shiny: [Vec<bool>; 2],
    /// Each Pokémon's Poké Ball as its set gives it (`SetExtras::pokeball`),
    /// per side, indexed like [`Renderer::nicknames`]: requests show its ID,
    /// or `pokeball` for none (the `Pokemon` constructor's
    /// `toID(set.pokeball) || 'pokeball'`).
    pub pokeballs: [Vec<String>; 2],
    /// While rendering: whether HP is written exactly (the secret line of
    /// a `|split|` block), and the side whose HP a line showed.
    secret: Cell<bool>,
    split: Cell<Option<SideId>>,
    /// Between calls: whether the last event rendered was a `|poke|` one,
    /// and each side's `|poke|` lines in the current team preview (the next
    /// one's stable id), so that a preview rendered a slice or an event
    /// at a time reads as one rendered whole.
    preview: Cell<(bool, [u8; 2])>,
}

impl Renderer {
    pub fn new(format: &Format, perspective: Perspective) -> Renderer {
        Renderer {
            dex: format.dex,
            mechanics: format.mechanics,
            game_type: format.game_type,
            team_preview: format.team_preview.shown(),
            perspective,
            info: BattleInfo::for_format(format),
            nicknames: [Vec::new(), Vec::new()],
            shiny: [Vec::new(), Vec::new()],
            pokeballs: [Vec::new(), Vec::new()],
            secret: Cell::new(true),
            split: Cell::new(None),
            preview: Cell::new((false, [0, 0])),
        }
    }

    /// Replaces the header and presentation settings.
    pub fn with_info(mut self, info: BattleInfo) -> Renderer {
        self.info = info;
        self
    }

    /// Sets which Pokémon are shiny (see [`Renderer::shiny`]).
    pub fn with_shiny(mut self, shiny: [Vec<bool>; 2]) -> Renderer {
        self.shiny = shiny;
        self
    }

    /// Whether the Pokémon with stable id `id` on `side` is shiny.
    pub(crate) fn is_shiny(&self, side: SideId, id: u8) -> bool {
        self.shiny[side.index()].get(id as usize).copied().unwrap_or(false)
    }

    /// The details of the Pokémon `actor` shows (its disguise under
    /// Illusion): Showdown's `getUpdatedDetails`, `, shiny` included.
    fn details_of(&self, actor: Actor, species: SpeciesId, level: u8, gender: pikapro::Gender) -> Details {
        let name = self.dex.species(details_species(species)).name;
        Details { name, level, gender, shiny: self.is_shiny(actor.pos.side, actor.id) }
    }

    /// Sets the nicknames (see [`Renderer::nicknames`]).
    pub fn with_nicknames(mut self, nicknames: [Vec<String>; 2]) -> Renderer {
        self.nicknames = nicknames;
        self
    }

    /// Sets the Poké Balls (see [`Renderer::pokeballs`]).
    pub fn with_pokeballs(mut self, pokeballs: [Vec<String>; 2]) -> Renderer {
        self.pokeballs = pokeballs;
        self
    }

    /// Sets the nicknames, shininess and Poké Balls from both sides' sheets.
    pub fn with_sheets(self, sheets: [&TeamSheet; 2], format: &Format) -> Renderer {
        self.with_nicknames(sheets.map(|s| s.nicknames(format)))
            .with_shiny(sheets.map(|s| s.extras.iter().map(|e| e.shiny).collect()))
            .with_pokeballs(sheets.map(TeamSheet::pokeballs))
    }

    /// Appends the Poké Ball of the Pokémon with stable id `id` on `side`,
    /// as its request shows it (an ID, `pokeball` by default).
    pub(crate) fn push_pokeball(&self, out: &mut String, side: SideId, id: u8) {
        let ball = self.pokeballs[side.index()].get(id as usize).map_or("", String::as_str);
        let start = out.len();
        out.extend(pikapro_data::id_chars(ball));
        if out.len() == start {
            out.push_str("pokeball");
        }
    }

    /// The lines Showdown writes before the battle's first event (its
    /// constructor's): `|t:|` and `|gametype|`. Every channel reads them.
    /// The `|player|` lines and the rest of the header (`|gen|`, `|tier|`,
    /// the rules) come with [`BattleEvent::Player`].
    pub fn header(&self) -> Vec<String> {
        let game_type = match self.game_type {
            GameType::Singles => "singles",
            GameType::Doubles => "doubles",
        };
        vec![format!("|t:|{}", self.info.timestamp), format!("|gametype|{game_type}")]
    }

    /// `setPlayer`'s `|player|` line; after p2's, `start`'s `|gen|`,
    /// `|tier|`, the rules and a custom game's rules.
    fn player_lines(&self, side: SideId, out: &mut Vec<Entry>) {
        let info = &self.info;
        let p = &info.players[side.index()];
        out.push(Entry::Line(format!("|player|{}|{}|{}|{}", side_id(side), p.name, p.avatar, p.rating)));
        if side != SideId::P2 {
            return;
        }
        out.push(Entry::Line(format!("|gen|{}", info.generation)));
        out.push(Entry::Line(format!("|tier|{}", info.tier)));
        for rule in &info.rules {
            out.push(Entry::Line(format!("|rule|{rule}")));
        }
        let n = info.custom_rules.len();
        if n > 0 {
            let plural = if n == 1 { "" } else { "s" };
            let open = if n <= 5 { " open" } else { "" };
            out.push(Entry::Line(format!(
                "|raw|<div class=\"infobox\"><details class=\"readmore\"{open}><summary><strong>{n} custom rule{plural}:</strong></summary> {}</details></div>",
                info.custom_rules.join(", ")
            )));
        }
    }

    /// Renders events as Showdown's log entries. A slice is rendered as
    /// one piece: an annotation of the last `|move|` line
    /// ([`BattleEvent::MoveAttr`]) amends it only within the slice and
    /// the same resolution (after its last `|t:|`), as no one sees
    /// Showdown amend a line it has already sent; so rendering a whole
    /// battle's events at once gives the lines rendering each resolution's
    /// does. A team preview's [`BattleEvent::Poke`] events may come in
    /// several slices, in order: the renderer keeps their count between
    /// calls.
    pub fn entries<'a>(&self, events: impl IntoIterator<Item = &'a BattleEvent>) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut last_move: Option<usize> = None;
        // Each side's `|poke|` lines so far: the Pokémon's stable id.
        let (mut in_preview, mut pokes) = self.preview.get();
        for event in events {
            if let BattleEvent::Poke { side, species, level, gender } = *event {
                // The first `|poke|` line of a preview clears the last one.
                if !in_preview {
                    pokes = [0, 0];
                    if self.team_preview {
                        out.push(Entry::Line("|clearpoke".into()));
                    }
                }
                in_preview = true;
                if self.team_preview {
                    self.poke(side, pokes[side.index()], species, level, gender, &mut out);
                }
                pokes[side.index()] = pokes[side.index()].saturating_add(1);
                continue;
            }
            in_preview = false;
            self.push_entries(event, &mut out, &mut last_move);
        }
        self.preview.set((in_preview, pokes));
        out
    }

    /// The raw stream (Showdown's `update` chunks): every entry once,
    /// `|split|pN` blocks included.
    pub fn raw<'a>(&self, events: impl IntoIterator<Item = &'a BattleEvent>) -> Vec<String> {
        let mut out = Vec::new();
        for entry in self.entries(events) {
            entry.into_raw(&mut out);
        }
        out
    }

    /// The lines this renderer's perspective reads.
    pub fn log<'a>(&self, events: impl IntoIterator<Item = &'a BattleEvent>) -> Vec<String> {
        self.entries(events).into_iter().filter_map(|e| e.into_line_for(self.perspective)).collect()
    }

    /// Renders one event for this perspective: usually one line, several
    /// separated by `\n` for some events, none for others.
    pub fn line(&self, event: &BattleEvent) -> String {
        self.log([event]).join("\n")
    }

    /// Renders events for this perspective, newline-terminated.
    pub fn lines<'a>(&self, events: impl IntoIterator<Item = &'a BattleEvent>) -> String {
        let mut out = String::new();
        for line in self.log(events) {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    fn push_entries(&self, event: &BattleEvent, out: &mut Vec<Entry>, last_move: &mut Option<usize>) {
        match *event {
            BattleEvent::MoveAttr { attr } => {
                self.attr_last_move(attr, out, last_move);
                return;
            }
            BattleEvent::Blank => {
                out.push(Entry::Line("|".into()));
                return;
            }
            BattleEvent::Timestamp => {
                // `turnLoop`'s `|t:|` begins each resolution: Showdown sent
                // the move lines before it when it last waited for a
                // choice, so its `attrLastMove` changes them for no one
                // (as the engine's `retargetLastMove` leaves them).
                *last_move = None;
                out.push(Entry::Line(format!("|t:|{}", self.info.timestamp)));
                return;
            }
            BattleEvent::Player { side } => {
                self.player_lines(side, out);
                return;
            }
            _ => {}
        }
        self.secret.set(true);
        self.split.set(None);
        // Room for most lines, which then never grow.
        let mut line = String::with_capacity(LINE_CAPACITY);
        // Writing into a String cannot fail.
        let _ = self.write(event, &mut line);
        match self.split.get() {
            Some(side) => {
                self.secret.set(false);
                let mut shared = String::with_capacity(LINE_CAPACITY);
                let _ = self.write(event, &mut shared);
                self.secret.set(true);
                out.push(Entry::Split { side, secret: line, shared });
            }
            None => {
                if matches!(event, BattleEvent::Move { .. } | BattleEvent::MoveNull { .. } | BattleEvent::Anim { .. }) {
                    *last_move = Some(out.len());
                }
                out.push(Entry::Line(line));
            }
        }
        // The Open Team Sheets rule's `onTeamPreview` and `onBattleStart`.
        if self.info.open_team_sheets {
            let secret = match event {
                BattleEvent::TeamPreview { .. } => OTS_REQUEST,
                BattleEvent::Start => "|uhtmlchange|otsrequest|",
                _ => return,
            };
            for side in SideId::BOTH {
                out.push(Entry::Split { side, secret: secret.into(), shared: String::new() });
            }
        }
    }

    /// The Team Preview rule's `onTeamPreview`: a Pokémon's `|poke|` line
    /// (`-*` formes; `, shiny` only in the Champions mod, whose rule doesn't
    /// remove it), after the `|clearpoke|` that [`Renderer::entries`] writes.
    fn poke(
        &self,
        side: SideId,
        id: u8,
        species: pikapro_data::SpeciesId,
        level: u8,
        gender: pikapro::Gender,
        out: &mut Vec<Entry>,
    ) {
        let mut details = details(self.dex, species, level, gender);
        if self.mechanics == Mechanics::Champions && self.is_shiny(side, id) {
            details.push_str(", shiny");
        }
        out.push(Entry::Line(format!("|poke|{}|{}|", side_id(side), self.preview_details(species, &details))));
    }

    /// Showdown's `attrLastMove`.
    fn attr_last_move(&self, attr: MoveAttr, out: &mut Vec<Entry>, last_move: &mut Option<usize>) {
        let Some(index) = *last_move else { return };
        let Some(Entry::Line(line)) = out.get_mut(index) else { return };
        if attr == MoveAttr::Still {
            if line.starts_with("|-anim|") {
                out.remove(index);
                *last_move = None;
                return;
            }
            let mut parts: Vec<&str> = line.split('|').collect();
            if parts.len() > 4 {
                parts[4] = "";
            }
            *line = parts.join("|");
        }
        line.push('|');
        match attr {
            MoveAttr::Still => line.push_str("[still]"),
            MoveAttr::Miss => line.push_str("[miss]"),
            MoveAttr::NoTarget => line.push_str("[notarget]"),
            MoveAttr::Spread(slots) => {
                line.push_str("[spread] ");
                let names: Vec<String> = slots.as_slice().iter().map(|p| slot_id(p.side, p.slot)).collect();
                line.push_str(&names.join(","));
            }
            MoveAttr::Anim(anim) => {
                line.push_str("[anim] ");
                let _ = match anim {
                    MoveAnim::FickleBeamAllOut => write!(line, "Fickle Beam All Out"),
                    MoveAnim::IvyCudgel(t) => write!(line, "Ivy Cudgel {}", t.name()),
                    MoveAnim::ShellSideArm(c) => write!(line, "Shell Side Arm {}", category_name(c)),
                    MoveAnim::TeraBlast(t) => write!(line, "Tera Blast {}", t.name()),
                };
            }
        }
    }

    fn write(&self, event: &BattleEvent, out: &mut String) -> fmt::Result {
        let dex = self.dex;
        let species = |id| dex.species(id).name;
        match *event {
            // `add(`teampreview${pickedTeamSize ? `|${pickedTeamSize}` : ''}`)`.
            BattleEvent::TeamPreview { bring: Some(bring) } => write!(out, "|teampreview|{bring}"),
            BattleEvent::TeamPreview { bring: None } => write!(out, "|teampreview"),
            BattleEvent::Start => write!(out, "|start"),
            BattleEvent::Turn { turn } => write!(out, "|turn|{turn}"),
            BattleEvent::Switch { actor, level, hp, max_hp, status, tera, from, shown, gender, .. } => {
                let sp = shown.unwrap_or(actor.species);
                write!(out, "|switch|{}|{}", self.ident(actor), self.details_of(actor, sp, level, gender))?;
                if let Some(t) = tera {
                    write!(out, ", tera:{}", t.name())?;
                }
                write!(out, "|{}", self.health(actor.pos.side, hp, max_hp, status))?;
                match from {
                    Some(m) => write!(out, "|[from] {}", dex.move_(m).name),
                    None => Ok(()),
                }
            }
            BattleEvent::Drag { actor, level, hp, max_hp, status, tera, shown, gender, .. } => {
                let sp = shown.unwrap_or(actor.species);
                write!(out, "|drag|{}|{}", self.ident(actor), self.details_of(actor, sp, level, gender))?;
                if let Some(t) = tera {
                    write!(out, ", tera:{}", t.name())?;
                }
                write!(out, "|{}", self.health(actor.pos.side, hp, max_hp, status))
            }
            BattleEvent::Move { user, move_id, target, from } => {
                write!(out, "|move|{}|{}|", self.ident(user), dex.move_(move_id).name)?;
                if let Some(t) = target {
                    write!(out, "{}", self.ident(t))?;
                }
                self.move_from(out, from)
            }
            BattleEvent::Damage { target, hp, max_hp, status, source } => {
                write!(out, "|-damage|{}|{}", self.ident(target), self.health(target.pos.side, hp, max_hp, status))?;
                match source {
                    DamageSource::Move => Ok(()),
                    // Showdown names toxic damage `psn`.
                    DamageSource::Status(Status::Toxic { .. }) => write!(out, "|[from] psn"),
                    DamageSource::Status(s) => write!(out, "|[from] {}", status_id(s)),
                    DamageSource::Recoil => write!(out, "|[from] recoil"),
                    DamageSource::MoveRecoil => write!(out, "|[from] Recoil"),
                    DamageSource::Confusion => write!(out, "|[from] confusion"),
                    DamageSource::Weather(w) => write!(out, "|[from] {}", weather_name(w)),
                    DamageSource::MoveEffect { move_id, of } => {
                        write!(out, "|[from] {}|[of] {}", dex.move_(move_id).name, self.ident(of))
                    }
                    DamageSource::MoveCondition(move_id) => {
                        write!(out, "|[from] {}", pikapro_data::to_id(dex.move_(move_id).name))
                    }
                    DamageSource::Effect(cause) => self.cause(out, Some(cause)),
                    DamageSource::Species(sp) => write!(out, "|[from] pokemon: {}", species(sp)),
                }
            }
            BattleEvent::Heal { target, hp, max_hp, status, source } => {
                write!(out, "|-heal|{}|{}", self.ident(target), self.health(target.pos.side, hp, max_hp, status))?;
                match source {
                    HealSource::Move => Ok(()),
                    HealSource::Drain { of } => write!(out, "|[from] drain|[of] {}", self.ident(of)),
                    HealSource::Terrain(t) => write!(out, "|[from] {}", terrain_name(t)),
                    HealSource::Effect(cause) => self.cause(out, Some(cause)),
                }
            }
            BattleEvent::Crit { target } => write!(out, "|-crit|{}", self.ident(target)),
            BattleEvent::SuperEffective { target, level } => {
                write!(out, "|-supereffective|{}", self.ident(target))?;
                self.effectiveness_level(out, level)
            }
            BattleEvent::Resisted { target, level } => {
                write!(out, "|-resisted|{}", self.ident(target))?;
                self.effectiveness_level(out, level)
            }
            BattleEvent::Immune { target } => write!(out, "|-immune|{}", self.ident(target)),
            BattleEvent::OhkoImmune { target } => write!(out, "|-immune|{}|[ohko]", self.ident(target)),
            BattleEvent::HitCount { target, hits } => write!(out, "|-hitcount|{}|{hits}", self.ident(target)),
            BattleEvent::Ohko => write!(out, "|-ohko"),
            BattleEvent::Miss { user, target } => write!(out, "|-miss|{}|{}", self.ident(user), self.ident(target)),
            BattleEvent::Boost { target, stat, amount } => {
                write!(out, "|-boost|{}|{}|{amount}", self.ident(target), stat.id())
            }
            BattleEvent::Unboost { target, stat, amount } => {
                write!(out, "|-unboost|{}|{}|{amount}", self.ident(target), stat.id())
            }
            BattleEvent::Fail { user } => write!(out, "|-fail|{}", self.ident(user)),
            BattleEvent::HealFail { target } => write!(out, "|-fail|{}|heal", self.ident(target)),
            // Gen 5+ Showdown reports a missing target as a failure.
            BattleEvent::NoTarget { user } => write!(out, "|-fail|{}", self.ident(user)),
            BattleEvent::StruggleStart { actor } => write!(out, "|-activate|{}|move: Struggle", self.ident(actor)),
            BattleEvent::ProtectStart { actor, move_id } => {
                // Each protection's `onStart` names itself one of two ways.
                let name = match move_id {
                    moves::SPIKY_SHIELD | moves::BANEFUL_BUNKER | moves::BURNING_BULWARK => "move: Protect",
                    _ => "Protect",
                };
                write!(out, "|-singleturn|{}|{name}", self.ident(actor))
            }
            BattleEvent::Protected { target } => write!(out, "|-activate|{}|move: Protect", self.ident(target)),
            BattleEvent::ProtectionBroken { target, move_id } => match move_id {
                moves::FEINT => write!(out, "|-activate|{}|move: Feint", self.ident(target)),
                _ => write!(out, "|-activate|{}|move: {}|[broken]", self.ident(target), dex.move_(move_id).name),
            },
            BattleEvent::Terastallize { actor, ty } => {
                write!(out, "|-terastallize|{}|{}", self.ident(actor), ty.name())
            }
            BattleEvent::StatusStart { target, status, from_move } => {
                write!(out, "|-status|{}|{}", self.ident(target), status_id(status))?;
                if let Some(m) = from_move {
                    write!(out, "|[from] move: {}", dex.move_(m).name)?;
                }
                Ok(())
            }
            BattleEvent::CureStatus { target, status, from_move } => {
                write!(out, "|-curestatus|{}|{}", self.ident(target), status_id(status))?;
                match from_move {
                    Some(m) => write!(out, "|[from] move: {}", dex.move_(m).name),
                    None => write!(out, "|[msg]"),
                }
            }
            BattleEvent::StatusCant { actor, status } => {
                write!(out, "|cant|{}|{}", self.ident(actor), status_id(status))
            }
            BattleEvent::StatusFail { target, status } => {
                write!(out, "|-fail|{}|{}", self.ident(target), status_id(status))
            }
            BattleEvent::Cant { actor, reason } => {
                write!(out, "|cant|{}|", self.ident(actor))?;
                match reason {
                    CantReason::Flinch => write!(out, "flinch"),
                    CantReason::Recharge => write!(out, "recharge"),
                    CantReason::Taunt(m) => write!(out, "move: Taunt|{}", dex.move_(m).name),
                    CantReason::Disable(m) => write!(out, "Disable|{}", dex.move_(m).name),
                }
            }
            BattleEvent::VolatileStart { actor, volatile, move_id } => {
                write!(out, "|-start|{}|{}", self.ident(actor), volatile_name(volatile))?;
                if let Some(m) = move_id {
                    write!(out, "|{}", dex.move_(m).name)?;
                }
                Ok(())
            }
            BattleEvent::VolatileEnd { actor, volatile } => {
                write!(out, "|-end|{}|{}", self.ident(actor), volatile_name(volatile))
            }
            BattleEvent::VolatileActivate { actor, volatile } => {
                write!(out, "|-activate|{}|{}", self.ident(actor), volatile_name(volatile))
            }
            BattleEvent::MustRecharge { actor } => write!(out, "|-mustrecharge|{}", self.ident(actor)),
            BattleEvent::Faint { actor } => write!(out, "|faint|{}", self.ident(actor)),
            BattleEvent::Weather { weather: None, .. } => write!(out, "|-weather|none"),
            BattleEvent::Weather { weather: Some(w), upkeep } => {
                write!(out, "|-weather|{}", weather_name(w))?;
                if upkeep {
                    out.push_str("|[upkeep]");
                }
                Ok(())
            }
            BattleEvent::FieldStart { effect, of } => {
                write!(out, "|-fieldstart|move: {}", field_effect_name(effect))?;
                if let Some(of) = of {
                    write!(out, "|[of] {}", self.ident(of))?;
                }
                Ok(())
            }
            // Showdown ends Misty Terrain without the `move: ` prefix.
            BattleEvent::FieldEnd { effect: FieldEffect::Terrain(Terrain::Misty) } => {
                write!(out, "|-fieldend|Misty Terrain")
            }
            BattleEvent::FieldEnd { effect } => write!(out, "|-fieldend|move: {}", field_effect_name(effect)),
            BattleEvent::SideStart { side, condition } => {
                write!(out, "|-sidestart|{}|{}", self.side(side), side_condition_name(condition))
            }
            BattleEvent::SideEnd { side, condition } => {
                write!(out, "|-sideend|{}|{}", self.side(side), side_condition_name(condition))
            }
            BattleEvent::TerrainActivate { target, terrain } => {
                write!(out, "|-activate|{}|move: {}", self.ident(target), terrain_name(terrain))
            }
            BattleEvent::MoveFail { user, move_id, note } => {
                write!(out, "|-fail|{}|move: {}", self.ident(user), dex.move_(move_id).name)?;
                match note {
                    FailNote::None => Ok(()),
                    FailNote::Forme => write!(out, "|[forme]"),
                    FailNote::Weak => write!(out, "|[weak]"),
                }
            }

            // Multi-turn moves.
            BattleEvent::Prepare { user, move_id } => {
                write!(out, "|-prepare|{}|{}", self.ident(user), dex.move_(move_id).name)?;
                if move_id == moves::CHILLY_RECEPTION {
                    out.push_str("|[premajor]");
                }
                Ok(())
            }
            BattleEvent::ChargeStart { actor, move_id } => {
                write!(out, "|-singleturn|{}|move: {}", self.ident(actor), dex.move_(move_id).name)
            }
            BattleEvent::LostFocus { actor } => write!(out, "|cant|{}|Focus Punch|Focus Punch", self.ident(actor)),
            BattleEvent::FatigueConfusion { actor } => write!(out, "|-start|{}|confusion|[fatigue]", self.ident(actor)),
            BattleEvent::UproarStart { actor, upkeep } => {
                write!(out, "|-start|{}|Uproar", self.ident(actor))?;
                if upkeep {
                    out.push_str("|[upkeep]");
                }
                Ok(())
            }
            BattleEvent::UproarEnd { actor } => write!(out, "|-end|{}|Uproar", self.ident(actor)),
            BattleEvent::UproarPreventsSleep { target, own } => {
                write!(out, "|-fail|{}|slp|[from] Uproar", self.ident(target))?;
                if own {
                    out.push_str("|[msg]");
                }
                Ok(())
            }
            // Doom Desire's `-start` has no `move: ` prefix.
            BattleEvent::FutureMoveStart { user, move_id: moves::DOOM_DESIRE } => {
                write!(out, "|-start|{}|Doom Desire", self.ident(user))
            }
            BattleEvent::FutureMoveStart { user, move_id } => {
                write!(out, "|-start|{}|move: {}", self.ident(user), dex.move_(move_id).name)
            }
            BattleEvent::FutureMoveEnd { target, move_id } => {
                write!(out, "|-end|{}|move: {}", self.ident(target), dex.move_(move_id).name)
            }

            // Substitute, trapping, residual volatiles.
            BattleEvent::EffectStart { actor, effect, of, from } => {
                let target = self.ident(actor);
                match effect {
                    VolatileEffect::PartiallyTrapped(m) => {
                        write!(out, "|-activate|{target}|move: {}", dex.move_(m).name)?
                    }
                    VolatileEffect::Trapped => write!(out, "|-activate|{target}|trapped")?,
                    VolatileEffect::DestinyBond => write!(out, "|-singlemove|{target}|Destiny Bond")?,
                    VolatileEffect::GlaiveRush => write!(out, "|-singlemove|{target}|Glaive Rush|[silent]")?,
                    VolatileEffect::Endure => write!(out, "|-singleturn|{target}|move: Endure")?,
                    VolatileEffect::ThroatChop => write!(out, "|-start|{target}|Throat Chop|[silent]")?,
                    VolatileEffect::LeechSeed => write!(out, "|-start|{target}|move: Leech Seed")?,
                    VolatileEffect::Octolock => write!(out, "|-start|{target}|move: Octolock")?,
                    _ => write!(out, "|-start|{target}|{}", effect_name(dex, effect))?,
                }
                if let Some(m) = from {
                    write!(out, "|[from] move: {}", dex.move_(m).name)?;
                }
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::EffectEndFrom { actor, effect, move_id, of } => write!(
                out,
                "|-end|{}|{}|[from] move: {}|[of] {}",
                self.ident(actor),
                effect_name(dex, effect),
                dex.move_(move_id).name,
                self.ident(of)
            ),
            BattleEvent::EffectEnd { actor, effect, silent } => {
                write!(out, "|-end|{}|{}", self.ident(actor), effect_name(dex, effect))?;
                if let VolatileEffect::PartiallyTrapped(_) | VolatileEffect::Octolock = effect {
                    out.push_str("|[partiallytrapped]");
                }
                let always_silent = matches!(
                    effect,
                    VolatileEffect::Yawn
                        | VolatileEffect::Attract
                        | VolatileEffect::SyrupBomb
                        | VolatileEffect::ThroatChop
                );
                if silent || always_silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::EffectActivate { actor, effect, of } => {
                write!(out, "|-activate|{}|move: {}", self.ident(actor), effect_name(dex, effect))?;
                if effect == VolatileEffect::Substitute {
                    out.push_str("|[damage]");
                }
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::EffectCant { actor, effect, move_id } => {
                let name = match effect {
                    VolatileEffect::Attract => "Attract",
                    VolatileEffect::ThroatChop => "move: Throat Chop",
                    VolatileEffect::HealBlock => "move: Heal Block",
                    VolatileEffect::Imprison => "move: Imprison",
                    _ => effect_name(dex, effect),
                };
                write!(out, "|cant|{}|{name}", self.ident(actor))?;
                match move_id {
                    Some(m) => write!(out, "|{}", dex.move_(m).name),
                    None => Ok(()),
                }
            }
            BattleEvent::EffectDamage { target, hp, max_hp, status, effect, of } => {
                let health = self.health(target.pos.side, hp, max_hp, status);
                write!(out, "|-damage|{}|{health}|[from] ", self.ident(target))?;
                match effect {
                    VolatileEffect::PartiallyTrapped(m) => {
                        write!(out, "move: {}|[partiallytrapped]", dex.move_(m).name)?;
                    }
                    _ => out.push_str(effect_name(dex, effect)),
                }
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::EffectHeal { target, hp, max_hp, status, .. } => {
                let health = self.health(target.pos.side, hp, max_hp, status);
                write!(out, "|-heal|{}|{health}|[silent]", self.ident(target))
            }
            BattleEvent::PerishCount { actor, count, silent } => {
                write!(out, "|-start|{}|perish{count}", self.ident(actor))?;
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::FieldActivate { effect } => {
                write!(out, "|-fieldactivate|move: {}", effect_name(dex, effect))
            }

            // Hazards, side, field and slot conditions.
            BattleEvent::GuardStart { actor, condition } => {
                write!(out, "|-singleturn|{}|{}", self.ident(actor), side_condition_plain_name(condition))
            }
            BattleEvent::HazardEnd { side, condition, from, of } => {
                write!(out, "|-sideend|{}|{}", self.side(side), side_condition_plain_name(condition))?;
                if let Some(m) = from {
                    write!(out, "|[from] move: {}", dex.move_(m).name)?;
                }
                if let Some(of) = of {
                    write!(out, "|[of] {}", self.ident(of))?;
                }
                Ok(())
            }
            BattleEvent::ToxicSpikesAbsorbed { side, of } => {
                write!(out, "|-sideend|{}|move: Toxic Spikes|[of] {}", self.side(side), self.ident(of))
            }
            BattleEvent::ConditionDamage { target, hp, max_hp, status, condition } => write!(
                out,
                "|-damage|{}|{}|[from] {}",
                self.ident(target),
                self.health(target.pos.side, hp, max_hp, status),
                side_condition_plain_name(condition)
            ),
            BattleEvent::MoveHeal { target, hp, max_hp, status, move_id } => write!(
                out,
                "|-heal|{}|{}|[from] move: {}",
                self.ident(target),
                self.health(target.pos.side, hp, max_hp, status),
                dex.move_(move_id).name
            ),
            BattleEvent::WishHeal { target, hp, max_hp, status, wisher, wisher_id } => {
                // `[wisher] <source.name>`: the wisher is on the target's side.
                let wisher = self.name(Actor { species: wisher, id: wisher_id, ..target });
                write!(
                    out,
                    "|-heal|{}|{}|[from] move: Wish|[wisher] {wisher}",
                    self.ident(target),
                    self.health(target.pos.side, hp, max_hp, status)
                )
            }
            BattleEvent::ClearAllBoost => write!(out, "|-clearallboost"),
            BattleEvent::SwapSideConditions => write!(out, "|-swapsideconditions"),
            BattleEvent::GravityCant { actor, move_id } => {
                write!(out, "|cant|{}|move: Gravity|{}", self.ident(actor), dex.move_(move_id).name)
            }
            BattleEvent::MagicRoomEnd { of } => write!(out, "|-fieldend|move: Magic Room|[of] {}", self.ident(of)),
            BattleEvent::PledgeWaiting { user, ally } => {
                write!(out, "|-waiting|{}|{}", self.ident(user), self.ident(ally))
            }
            BattleEvent::PledgeCombine => write!(out, "|-combine"),

            // Stat, HP and type manipulation.
            BattleEvent::SetBoost { target, stat, stage, move_id } => {
                write!(
                    out,
                    "|-setboost|{}|{}|{stage}|[from] move: {}",
                    self.ident(target),
                    stat.id(),
                    dex.move_(move_id).name
                )
            }
            BattleEvent::SetHp { target, hp, max_hp, status, silent } => {
                let health = self.health(target.pos.side, hp, max_hp, status);
                write!(out, "|-sethp|{}|{health}|[from] move: Pain Split", self.ident(target))?;
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::CopyBoost { source, target } => {
                write!(out, "|-copyboost|{}|{}|[from] move: Psych Up", self.ident(source), self.ident(target))
            }
            BattleEvent::SwapBoost { source, target, move_id } => {
                write!(out, "|-swapboost|{}|{}|", self.ident(source), self.ident(target))?;
                match move_id {
                    moves::GUARD_SWAP => out.push_str("def, spd|"),
                    moves::POWER_SWAP => out.push_str("atk, spa|"),
                    _ => {}
                }
                write!(out, "[from] move: {}", dex.move_(move_id).name)
            }
            BattleEvent::InvertBoost { target } => {
                write!(out, "|-invertboost|{}|[from] move: Topsy-Turvy", self.ident(target))
            }
            BattleEvent::ClearBoost { target } => write!(out, "|-clearboost|{}", self.ident(target)),
            BattleEvent::MoveActivate { actor, move_id, of } => {
                write!(out, "|-activate|{}|move: {}", self.ident(actor), dex.move_(move_id).name)?;
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::PpDrain { target, move_id, amount } => {
                write!(out, "|-activate|{}|move: Eerie Spell|{}|{amount}", self.ident(target), dex.move_(move_id).name)
            }
            BattleEvent::TypeChange { target, types, silent } => {
                write!(out, "|-start|{}|typechange|{}", self.ident(target), types.primary.name())?;
                if let Some(t) = types.secondary {
                    write!(out, "/{}", t.name())?;
                }
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::TypeCopy { target, of } => {
                write!(
                    out,
                    "|-start|{}|typechange|[from] move: Reflect Type|[of] {}",
                    self.ident(target),
                    self.ident(of)
                )
            }
            BattleEvent::TypeAdd { target, ty, move_id } => {
                write!(out, "|-start|{}|typeadd|{}|", self.ident(target), ty.name())?;
                match move_id {
                    Some(m) => write!(out, "[from] move: {}", dex.move_(m).name),
                    None => write!(out, "[silent]"),
                }
            }
            BattleEvent::ManipulationStart { actor, volatile, silent } => {
                use pikapro::event::ManipulationVolatile;
                write!(out, "|-start|{}|", self.ident(actor))?;
                match volatile {
                    ManipulationVolatile::Stockpile(layers) => write!(out, "stockpile{layers}")?,
                    v => out.push_str(manipulation_volatile_name(v)),
                }
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::ManipulationEnd { actor, volatile, silent } => {
                write!(out, "|-end|{}|{}", self.ident(actor), manipulation_volatile_name(volatile))?;
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::SingleTurn { actor, move_id } => {
                write!(out, "|-singleturn|{}|move: {}", self.ident(actor), dex.move_(move_id).name)
            }
            BattleEvent::Nothing => write!(out, "|-nothing"),
            BattleEvent::ManipulationHeal { target, hp, max_hp, status, from } => {
                use pikapro::event::ManipulationVolatile;
                write!(out, "|-heal|{}|{}", self.ident(target), self.health(target.pos.side, hp, max_hp, status))?;
                match from {
                    None => write!(out, "|[silent]"),
                    Some(ManipulationVolatile::AquaRing) => write!(out, "|[from] Aqua Ring"),
                    Some(ManipulationVolatile::Ingrain) => write!(out, "|[from] Ingrain"),
                    Some(v) => write!(out, "|[from] {}", manipulation_volatile_name(v)),
                }
            }

            // Action order, targeting, move calling.
            BattleEvent::MoveNull { user, move_id, from } => {
                write!(out, "|move|{}|{}|null", self.ident(user), dex.move_(move_id).name)?;
                self.move_from(out, from)
            }
            BattleEvent::CantNoPp { actor, move_id } => {
                let name = move_id.map_or("recharge", |m| dex.move_(m).name);
                write!(out, "|cant|{}|nopp|{name}", self.ident(actor))
            }
            BattleEvent::Swap { actor, slot, move_id } => {
                write!(out, "|swap|{}|{slot}|[from] move: {}", self.ident(actor), dex.move_(move_id).name)
            }
            BattleEvent::ActivateMove { target, move_id, about, amount } => {
                write!(out, "|-activate|{}|move: {}", self.ident(target), dex.move_(move_id).name)?;
                if let Some(m) = about {
                    write!(out, "|{}", dex.move_(m).name)?;
                }
                if amount > 0 {
                    write!(out, "|{amount}")?;
                }
                Ok(())
            }
            BattleEvent::MoveSingleTurn { target, move_id, of } => {
                // Helping Hand's condition names itself without `move: `.
                let prefix = if move_id == moves::HELPING_HAND { "" } else { "move: " };
                write!(out, "|-singleturn|{}|{prefix}{}", self.ident(target), dex.move_(move_id).name)?;
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::Transform { actor, target } => {
                write!(out, "|-transform|{}|{}", self.ident(actor), self.ident(target))
            }
            BattleEvent::MimicStart { actor, move_id } => {
                write!(out, "|-start|{}|Mimic|{}", self.ident(actor), dex.move_(move_id).name)
            }
            BattleEvent::TypeChangeSilent { actor, types } => {
                write!(out, "|-start|{}|typechange|{}", self.ident(actor), types.primary.name())?;
                if let Some(t) = types.secondary {
                    write!(out, "/{}", t.name())?;
                }
                write!(out, "|[silent]")
            }
            // Abilities and items.
            BattleEvent::AbilityShow { actor, ability, note, cause } => {
                write!(out, "|-ability|{}|{}", self.ident(actor), dex.ability(ability).name)?;
                match note {
                    AbilityNote::None => {}
                    AbilityNote::Boost => out.push_str("|boost"),
                    AbilityNote::Replaced(old) => write!(out, "|{}", dex.ability(old).name)?,
                }
                self.cause(out, cause)
            }
            BattleEvent::ItemShow { actor, item, cause } => {
                write!(out, "|-item|{}|{}", self.ident(actor), dex.item(item).name)?;
                self.cause(out, cause)
            }
            BattleEvent::ItemEnd { actor, item, note, from, of } => {
                write!(out, "|-enditem|{}|{}", self.ident(actor), dex.item(item).name)?;
                match note {
                    ItemEndNote::None => {}
                    ItemEndNote::Eat => out.push_str("|[eat]"),
                    ItemEndNote::Weaken => out.push_str("|[weaken]"),
                    ItemEndNote::Gem => out.push_str("|[from] gem"),
                    ItemEndNote::Silent => out.push_str("|[silent]"),
                    ItemEndNote::StealEat(m) => write!(out, "|[from] stealeat|[move] {}", dex.move_(m).name)?,
                    ItemEndNote::GemMove(m) => write!(out, "|[from] gem|[move] {}", dex.move_(m).name)?,
                }
                if let Some(from) = from {
                    write!(out, "|[from] {}", self.effect_name(from))?;
                }
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::FlingNoItem { actor } => {
                write!(out, "|-enditem|{}||[from] move: Fling", self.ident(actor))
            }
            BattleEvent::BoostFrom { target, stat, amount, unboost, cause } => {
                let kind = if unboost { "-unboost" } else { "-boost" };
                write!(out, "|{kind}|{}|{}|{amount}", self.ident(target), stat.id())?;
                self.cause(out, Some(cause))
            }
            BattleEvent::StatusFrom { target, status, cause } => {
                write!(out, "|-status|{}|{}", self.ident(target), status_id(status))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::CureStatusFrom { target, status, cause, silent } => {
                write!(out, "|-curestatus|{}|{}", self.ident(target), status_id(status))?;
                self.cause(out, Some(cause))?;
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::ImmuneFrom { target, detail, cause } => {
                write!(out, "|-immune|{}", self.ident(target))?;
                if let Some(arg) = detail {
                    write!(out, "|{}", self.arg(arg))?;
                }
                self.cause(out, Some(cause))
            }
            BattleEvent::FailFrom { target, what, detail, cause } => {
                write!(out, "|-fail|{}|{}", self.ident(target), self.arg(what))?;
                if let Some(arg) = detail {
                    write!(out, "|{}", self.arg(arg))?;
                }
                self.cause(out, Some(cause))
            }
            BattleEvent::WeatherFrom { weather, cause } => {
                write!(out, "|-weather|{}", weather_name(weather))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::FieldStartFrom { effect, cause } => {
                write!(out, "|-fieldstart|move: {}", field_effect_name(effect))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::ActivateEffect { actor, effect, args, of } => {
                write!(out, "|-activate|{}|{}", self.ident(actor), self.effect_name(effect))?;
                for arg in args.into_iter().flatten() {
                    write!(out, "|{}", self.arg(arg))?;
                }
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::EffectStartEnd { actor, start, what, effect, cause, silent } => {
                let kind = if start { "-start" } else { "-end" };
                write!(out, "|{kind}|{}|", self.ident(actor))?;
                match effect {
                    Some(e) => {
                        out.push_str(&self.effect_name(e));
                        if what != LineArg::Text("") {
                            write!(out, "|{}", self.arg(what))?;
                        }
                    }
                    None => out.push_str(&self.arg(what)),
                }
                self.cause(out, cause)?;
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::Block { actor, effect, arg, of } => {
                write!(out, "|-block|{}|{}", self.ident(actor), self.effect_name(effect))?;
                if let Some(arg) = arg {
                    write!(out, "|{}", self.arg(arg))?;
                }
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::CantFrom { actor, effect, move_id, of } => {
                write!(out, "|cant|{}|{}", self.ident(actor), self.effect_name(effect))?;
                if let Some(m) = move_id {
                    write!(out, "|{}", dex.move_(m).name)?;
                }
                match of {
                    Some(of) => write!(out, "|[of] {}", self.ident(of)),
                    None => Ok(()),
                }
            }
            BattleEvent::FormeChange { actor, species: sp, msg, cause } => {
                write!(out, "|-formechange|{}|{}", self.ident(actor), species(sp))?;
                // Showdown always passes its `message` argument, before the
                // cause (`-formechange|p|X|` or `-formechange|p|X||[from]
                // ability: Y` without one).
                out.push_str(if msg { "|[msg]" } else { "|" });
                self.cause(out, cause)
            }
            BattleEvent::DetailsChange { actor, species: sp, level, tera, silent, gender } => {
                let details = self.details_of(actor, sp, level, gender);
                write!(out, "|detailschange|{}|{details}", self.ident(actor))?;
                if let Some(t) = tera {
                    write!(out, ", tera:{}", t.name())?;
                }
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            BattleEvent::HealSilent { target, hp, max_hp, status } => {
                let health = self.health(target.pos.side, hp, max_hp, status);
                write!(out, "|-heal|{}|{health}|[silent]", self.ident(target))
            }
            BattleEvent::WeatherMoveFail { user, move_id, weather } => {
                let name = match weather {
                    Weather::HarshSun => "Desolate Land",
                    Weather::HeavyRain => "Primordial Sea",
                    w => weather_name(w),
                };
                write!(out, "|-fail|{}|{}|[from] {name}", self.ident(user), dex.move_(move_id).name)
            }
            BattleEvent::WeatherActivate { weather } => {
                let name = match weather {
                    Weather::StrongWinds => "Delta Stream",
                    Weather::HarshSun => "Desolate Land",
                    Weather::HeavyRain => "Primordial Sea",
                    w => weather_name(w),
                };
                write!(out, "|-fieldactivate|{name}")
            }
            BattleEvent::ClearNegativeBoost { target, silent } => {
                write!(out, "|-clearnegativeboost|{}", self.ident(target))?;
                if silent {
                    out.push_str("|[silent]");
                }
                Ok(())
            }
            // Immunities, contact and reactive effects.
            BattleEvent::StartFrom { actor, what, arg, cause } => {
                write!(out, "|-start|{}|{what}", self.ident(actor))?;
                if let Some(arg) = arg {
                    write!(out, "|{}", self.arg(arg))?;
                }
                self.cause(out, Some(cause))
            }
            BattleEvent::SetBoostFrom { target, stat, stage, cause } => {
                write!(out, "|-setboost|{}|{}|{stage}", self.ident(target), stat.id())?;
                self.cause(out, Some(cause))
            }
            BattleEvent::SkillSwapActivate { source, abilities, of } => {
                write!(out, "|-activate|{}|Skill Swap|", self.ident(source))?;
                if let Some((theirs, ours)) = abilities {
                    write!(out, "{}|{}", dex.ability(theirs).name, dex.ability(ours).name)?;
                } else {
                    out.push('|');
                }
                write!(out, "|[of] {}", self.ident(of))
            }
            BattleEvent::FailBy { target, cause } => {
                write!(out, "|-fail|{}", self.ident(target))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::AbilityReplaced { holder, ability, target, old } => write!(
                out,
                "|-activate|{}|ability: {}|{}|[ability] {}",
                self.ident(holder),
                dex.ability(ability).name,
                self.ident(target),
                dex.ability(old).name
            ),
            BattleEvent::ZBroken { target } => write!(out, "|-zbroken|{}", self.ident(target)),
            BattleEvent::ClearBoostFrom { target, cause } => {
                write!(out, "|-clearboost|{}", self.ident(target))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::ClearPositiveBoost { target, source, move_id } => write!(
                out,
                "|-clearpositiveboost|{}|{}|move: {}",
                self.ident(target),
                self.ident(source),
                dex.move_(move_id).name
            ),
            BattleEvent::CopyBoostFrom { source, target, cause } => {
                write!(out, "|-copyboost|{}|{}", self.ident(source), self.ident(target))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::TypeChangeFrom { target, types, cause } => {
                write!(out, "|-start|{}|typechange|{}", self.ident(target), types.primary.name())?;
                if let Some(t) = types.secondary {
                    write!(out, "/{}", t.name())?;
                }
                self.cause(out, Some(cause))
            }
            BattleEvent::TransformFrom { actor, target, cause } => {
                write!(out, "|-transform|{}|{}", self.ident(actor), self.ident(target))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::WeatherFail { source, effect, weather } => {
                let id = match weather {
                    Weather::Sun => "sunnyday",
                    Weather::Rain => "raindance",
                    Weather::Sand => "sandstorm",
                    Weather::Snow => "snowscape",
                    Weather::HarshSun => "desolateland",
                    Weather::HeavyRain => "primordialsea",
                    Weather::StrongWinds => "deltastream",
                };
                match effect {
                    EffectName::Ability(x) => {
                        write!(out, "|-ability|{}|{}|[from] {id}|[fail]", self.ident(source), dex.ability(x).name)
                    }
                    EffectName::Move(m) => {
                        write!(out, "|-fail|{}|{}|[from] {id}", self.ident(source), dex.move_(m).name)
                    }
                    EffectName::Item(x) => {
                        write!(out, "|-fail|{}|{}|[from] {id}", self.ident(source), dex.item(x).name)
                    }
                }
            }
            BattleEvent::FormeChangeSilent { actor, species: sp, cause } => {
                write!(out, "|-formechange|{}|{}|[silent]", self.ident(actor), species(sp))?;
                self.cause(out, Some(cause))
            }
            BattleEvent::ActivateSource { actor, ability } => {
                write!(out, "|-activate|{}|{}|[source]", self.ident(actor), dex.ability(ability).name)
            }
            BattleEvent::AbilityNamed { actor, name } => write!(out, "|-ability|{}|{name}", self.ident(actor)),
            BattleEvent::HealFromSilent { target, hp, max_hp, status, cause } => {
                write!(out, "|-heal|{}|{}", self.ident(target), self.health(target.pos.side, hp, max_hp, status))?;
                self.cause(out, Some(cause))?;
                out.push_str("|[silent]");
                Ok(())
            }
            BattleEvent::AbilityEnd { actor } => write!(out, "|-endability|{}", self.ident(actor)),
            BattleEvent::SkillSwap { source, target, abilities } => {
                write!(out, "|-activate|{}|Skill Swap|", self.ident(source))?;
                if let Some((theirs, ours)) = abilities {
                    write!(out, "{}|{}", dex.ability(theirs).name, dex.ability(ours).name)?;
                } else {
                    out.push('|');
                }
                write!(out, "|[of] {}", self.ident(target))
            }
            BattleEvent::MoveFieldActivate { move_id } => {
                write!(out, "|-fieldactivate|move: {}", dex.move_(move_id).name)
            }
            // Mega Evolution.
            BattleEvent::MegaEvolve { actor, species: sp, item } => {
                write!(out, "|-mega|{}|{}|", self.ident(actor), species(sp))?;
                if let Some(item) = item {
                    out.push_str(dex.item(item).name);
                }
                Ok(())
            }
            BattleEvent::Upkeep => write!(out, "|upkeep"),
            BattleEvent::Win { side } => write!(out, "|win|{}", self.info.players[side.index()].name),
            BattleEvent::TeamSize { side, size } => write!(out, "|teamsize|{}|{size}", side_id(side)),
            BattleEvent::Anim { user, move_id, target } => {
                write!(out, "|-anim|{}|{}|{}", self.ident(user), dex.move_(move_id).name, self.ident(target))
            }
            BattleEvent::Hint { hint } => {
                out.push_str("|-hint|");
                self.hint(out, hint)
            }
            BattleEvent::Message { message } => self.message(out, message),
            BattleEvent::Replace { actor, species: sp, level, gender } => {
                write!(out, "|replace|{}|{}", self.ident(actor), self.details_of(actor, sp, level, gender))
            }
            // Rendered by `entries` and `push_entries`.
            BattleEvent::Poke { .. }
            | BattleEvent::MoveAttr { .. }
            | BattleEvent::Blank
            | BattleEvent::Timestamp
            | BattleEvent::Player { .. } => Ok(()),
            BattleEvent::Tie => write!(out, "|tie"),
            // Not part of Showdown's protocol: RNG trace for conformance
            // debugging (`BattleOptions::with_rng_trace`).
            BattleEvent::Rng { bound, result } => write!(out, "|rng|{bound}|{result}"),
        }
    }

    /// An effect as a line names it (`ability: X`, `item: X`,
    /// `move: X`).
    fn effect_name(&self, effect: EffectName) -> String {
        match effect {
            EffectName::Ability(a) => format!("ability: {}", self.dex.ability(a).name),
            EffectName::Item(i) => format!("item: {}", self.dex.item(i).name),
            EffectName::Move(m) => format!("move: {}", self.dex.move_(m).name),
        }
    }

    /// `[from] <effect>|[of] <pokemon>`.
    fn cause(&self, out: &mut String, cause: Option<Cause>) -> fmt::Result {
        let Some(cause) = cause else { return Ok(()) };
        write!(out, "|[from] {}", self.effect_name(cause.effect))?;
        match cause.of {
            Some(of) => write!(out, "|[of] {}", self.ident(of)),
            None => Ok(()),
        }
    }

    /// An extra argument.
    fn arg(&self, arg: LineArg) -> String {
        match arg {
            LineArg::Move(m) => self.dex.move_(m).name.to_string(),
            LineArg::Item(i) => self.dex.item(i).name.to_string(),
            LineArg::Ability(a) => self.dex.ability(a).name.to_string(),
            LineArg::Species(s) => self.dex.species(s).name.to_string(),
            LineArg::Type(t) => t.name().to_string(),
            LineArg::Actor(a) => self.ident(a).to_string(),
            LineArg::Text(t) => t.to_string(),
        }
    }

    /// A `|move|` line's `[from]` annotation.
    fn move_from(&self, out: &mut String, from: Option<MoveFrom>) -> fmt::Result {
        match from {
            Some(MoveFrom::LockedMove) => write!(out, "|[from] lockedmove"),
            Some(MoveFrom::Move(m)) => write!(out, "|[from] move: {}", self.dex.move_(m).name),
            Some(MoveFrom::Ability(a)) => write!(out, "|[from] ability: {}", self.dex.ability(a).name),
            None => Ok(()),
        }
    }

    /// `p1a: Incineroar` on the field, `p1: Incineroar` once it has left
    /// it (Showdown's `Pokemon.toString`). The name is the nickname of the
    /// Pokémon shown (`actor.id`, the disguise under Illusion), cut
    /// to 20 characters as Showdown does; without one, its base species
    /// (`p2a: Sandslash` for Sandslash-Alola; the forme shows in
    /// `details`).
    ///
    /// A value that writes itself where it is formatted (no `String`).
    pub(crate) fn ident(&self, actor: Actor) -> Ident<'_> {
        let slot = actor.active.then(|| (b'a' + actor.pos.slot) as char);
        Ident { side: side_id(actor.pos.side), slot, name: self.name(actor) }
    }

    /// A Pokémon's name (Showdown's `pokemon.name`).
    pub(crate) fn name(&self, actor: Actor) -> &str {
        crate::team::name_ref(&self.nicknames[actor.pos.side.index()], actor.id, actor.species, self.dex)
    }

    /// A side as Showdown's `Side.toString`: `p1: <player name>`.
    fn side(&self, side: SideId) -> String {
        format!("{}: {}", side_id(side), self.info.players[side.index()].name)
    }

    /// A `-hint`'s text.
    fn hint(&self, out: &mut String, hint: Hint) -> fmt::Result {
        let name = |m| self.dex.move_(m).name;
        match hint {
            Hint::OnlyForme(m) => {
                let forms = match m {
                    moves::AURA_WHEEL => "Morpeko or Morpeko-Hangry",
                    moves::DARK_VOID => "Darkrai",
                    _ => "Hoopa Unbound",
                };
                write!(out, "Only a Pokemon whose form is {forms} can use this move.")
            }
            Hint::FirstTurnOnly(m) => write!(out, "{} only works on your first turn out.", name(m)),
            Hint::PsychicTerrainAirborne => write!(out, "Psychic Terrain doesn't affect airborne Pokémon."),
            Hint::GroundedDoubled(m) => write!(out, "{}'s BP doubled on grounded target.", name(m)),
            Hint::TeraRoost => write!(out, "If a Terastallized Pokemon uses Roost, it remains Flying-type."),
            Hint::WonderRoomDefense { move_id, special } => {
                let sp = if special { "Sp. " } else { "" };
                write!(out, "{} uses {sp}Def boosts when Wonder Room is active.", name(move_id))
            }
            Hint::DarkPrankster => write!(out, "Since gen 7, Dark is immune to Prankster moves."),
            Hint::ShellSideArm(c) => write!(out, "{} Shell Side Arm", category_name(c)),
            Hint::FutureMoveTurnLimit => {
                write!(out, "In Gen 8+, Future attacks will never resolve when used on the 255th turn or later.")
            }
            Hint::FutureMoveMissed { move_id, fainted } => {
                let why = if fainted { "fainted" } else { "the user" };
                write!(out, "{} did not hit because the target is {why}.", name(move_id))
            }
            Hint::WishTurnLimit(turn) => {
                write!(out, "In Gen 8+, Wish will never resolve when used on the {turn}th turn.")
            }
            Hint::CantUseTwice(m) => write!(out, "Some effects can force a Pokemon to use {} again in a row.", name(m)),
            Hint::TransformMimicry => {
                write!(out, "Transform Mimicry changes you to your original un-transformed types.")
            }
            Hint::SwitchAlreadyIn => {
                write!(out, "A switch failed because the Pokémon trying to switch in is already in.")
            }
            Hint::SwitchAtZeroHp => {
                write!(out, "A Pokemon can't switch between when it runs out of HP and when it faints")
            }
            Hint::SkillSwapAllies => {
                write!(out, "Skill Swap does not announce the abilities of the Pokémon when used between allies.")
            }
            Hint::SleepClauseMod => write!(
                out,
                "Sleep Clause Mod prevents players from putting more than one of their opponent's Pokémon to sleep at a time"
            ),
            Hint::IllusionLevelMod => {
                write!(out, "Illusion Level Mod is active, so this Pokémon's true level was hidden.")
            }
            Hint::StockpileTracking => write!(
                out,
                "In Gen 7, Stockpile keeps track of how many times it successfully altered each stat individually."
            ),
        }
    }

    /// A message line.
    fn message(&self, out: &mut String, message: Message) -> fmt::Result {
        match message {
            Message::NaturalCure { side, cured } => {
                let verb = if cured == 1 { "was" } else { "were" };
                let name = &self.info.players[side.index()].name;
                write!(out, "|-message|({cured} of {name}'s pokemon {verb} cured by Natural Cure.)")
            }
            Message::TurnLimit { limit } => write!(out, "|message|It is turn {limit}. You have hit the turn limit!"),
            Message::AutoTieWarning { turns_left, limit } => {
                let turns = if turns_left == 1 { "1 turn".to_string() } else { format!("{turns_left} turns") };
                write!(out, "|bigerror|You will auto-tie if the battle doesn't end in {turns} (on turn {limit}).")
            }
            Message::BerryCycling { loser } => {
                let name = &self.info.players[loser.index()].name;
                write!(
                    out,
                    "|-message|{name}'s team started with the rudimentary means to perform restorative berry-cycling and thus loses."
                )
            }
            Message::SleepClauseMod => out.write_str("|-message|Sleep Clause Mod activated."),
            Message::BerryCyclingBoth => out.write_str(
                "|-message|Each side's team started with the rudimentary means to perform restorative berry-cycling.",
            ),
        }
    }

    /// The details team preview shows (the Team Preview rule's
    /// `onTeamPreview`): formes that could hide information are `-*` (see
    /// [`preview_wildcard`]).
    fn preview_details(&self, species: SpeciesId, details: &str) -> String {
        match preview_wildcard(self.dex, self.mechanics, species) {
            Some(base) => {
                let rest = details.split_once(',').map_or("", |(_, r)| r);
                let sep = if rest.is_empty() { "" } else { "," };
                format!("{}-*{sep}{rest}", self.dex.species(base).name)
            }
            None => details.to_string(),
        }
    }

    /// The Champions mod appends how strong the (in)effectiveness is.
    fn effectiveness_level(&self, out: &mut String, level: u8) -> fmt::Result {
        match self.mechanics {
            Mechanics::Champions => write!(out, "|{level}"),
            Mechanics::ScarletViolet => Ok(()),
        }
    }

    /// Showdown's `getHealth`: exact `hp/max` in the secret line of the
    /// `|split|` block it makes, otherwise the shared form for the
    /// mechanics family; then the status, if any (`45/100 brn`).
    fn health(&self, side: SideId, hp: u16, max_hp: u16, status: Status) -> Health {
        self.split.set(Some(side));
        let shown = if hp == 0 {
            HealthShown::Fainted
        } else if self.secret.get() || self.info.report_exact_hp {
            HealthShown::Exact(hp, max_hp)
        } else {
            let (pct, hint) = hp_shown(self.mechanics, hp, max_hp);
            HealthShown::Percent(pct, hint)
        };
        Health { shown, status }
    }
}

/// [`Renderer::ident`]: `p1a: Name`, written where it is formatted.
#[derive(Clone, Copy)]
pub(crate) struct Ident<'r> {
    side: &'static str,
    /// The slot's letter on the field.
    slot: Option<char>,
    name: &'r str,
}

impl Ident<'_> {
    /// The name after `p1a: `.
    pub(crate) fn name(&self) -> &str {
        self.name
    }

    /// `p1a: ` or `p1: `.
    pub(crate) fn push_prefix(&self, out: &mut String) {
        out.push_str(self.side);
        if let Some(c) = self.slot {
            out.push(c);
        }
        out.push_str(": ");
    }
}

impl fmt::Display for Ident<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.side)?;
        if let Some(c) = self.slot {
            f.write_char(c)?;
        }
        f.write_str(": ")?;
        f.write_str(self.name)
    }
}

/// A health shown in the log ([`Renderer::health`]): `0 fnt`, or the HP
/// (exact or a percentage) and the status.
#[derive(Clone, Copy)]
struct Health {
    shown: HealthShown,
    status: Status,
}

#[derive(Clone, Copy)]
enum HealthShown {
    Fainted,
    Exact(u16, u16),
    Percent(u8, HpHint),
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.shown {
            HealthShown::Fainted => return f.write_str("0 fnt"),
            HealthShown::Exact(hp, max) => {
                fmt::Display::fmt(&hp, f)?;
                f.write_char('/')?;
                fmt::Display::fmt(&max, f)?;
            }
            HealthShown::Percent(pct, hint) => {
                fmt::Display::fmt(&pct, f)?;
                f.write_str("/100")?;
                f.write_str(hint.suffix())?;
            }
        }
        if self.status != Status::None {
            f.write_char(' ')?;
            f.write_str(status_id(self.status))?;
        }
        Ok(())
    }
}

/// [`details`] (and `, shiny`), written where it is formatted.
#[derive(Clone, Copy)]
struct Details {
    name: &'static str,
    level: u8,
    gender: pikapro::Gender,
    shiny: bool,
}

impl fmt::Display for Details {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)?;
        if self.level != 100 {
            f.write_str(", L")?;
            fmt::Display::fmt(&self.level, f)?;
        }
        if self.gender != pikapro::Gender::Genderless {
            f.write_str(", ")?;
            f.write_str(self.gender.letter())?;
        }
        if self.shiny {
            f.write_str(", shiny")?;
        }
        Ok(())
    }
}

/// The colour hint the Champions health bar adds to a percentage on one
/// of its colour boundaries (`20/100y`), so that a client colours the bar
/// as the exact HP would: green above 50%, yellow above 20%, red below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HpHint {
    /// No hint: the percentage alone says the colour.
    #[default]
    None,
    /// `g`: shown as 50%, but above half.
    Green,
    /// `y`: shown as 50% at exactly half or less, or as 20% but above a
    /// fifth.
    Yellow,
    /// `r`: shown as 20% at exactly a fifth or less.
    Red,
}

impl HpHint {
    /// The suffix the protocol writes after `/100` (empty for none).
    pub fn suffix(self) -> &'static str {
        match self {
            HpHint::None => "",
            HpHint::Green => "g",
            HpHint::Yellow => "y",
            HpHint::Red => "r",
        }
    }
}

/// HP as everyone but its owner sees it (the shared line of an HP report,
/// Showdown's `getHealth` without `reportExactHP`): a percentage in
/// `0..=100` and, under Champions mechanics, the health bar's colour hint.
///
/// * Scarlet/Violet (Gen 7+): `ceil(100 * hp / max)`, but never 100 while
///   hurt (99).
/// * Champions: `floor(100 * hp / max)`, at least 1 while standing, with a
///   [`HpHint`] at 20% and 50%.
///
/// A fainted Pokémon (`hp == 0`) is `(0, None)`; the protocol writes it as
/// `0 fnt` without calling this. HP past `max_hp` counts as `max_hp`, so
/// the percentage stays in `0..=100` whatever the input.
pub fn hp_shown(mechanics: Mechanics, hp: u16, max_hp: u16) -> (u8, HpHint) {
    if hp == 0 || max_hp == 0 {
        return (0, HpHint::None);
    }
    // HP past the max (none the engine reports) shows as full.
    let (hp, max) = (hp.min(max_hp) as u32, max_hp as u32);
    match mechanics {
        Mechanics::ScarletViolet => {
            let pct = (100 * hp).div_ceil(max);
            let pct = if pct == 100 && hp < max { 99 } else { pct };
            (pct as u8, HpHint::None)
        }
        Mechanics::Champions => {
            let pct = (100 * hp / max).max(1);
            let hint = match pct {
                20 if hp * 5 > max => HpHint::Yellow,
                20 => HpHint::Red,
                50 if hp * 2 > max => HpHint::Green,
                50 => HpHint::Yellow,
                _ => HpHint::None,
            };
            (pct as u8, hint)
        }
    }
}

/// The base species team preview shows as `<base>-*` instead of
/// `species` (the Team Preview rule's `onTeamPreview`: formes that could
/// hide information), or `None` if preview shows `species` as it is.
///
/// Champions hides Xerneas', Zacian's and Zamazenta's formes, a crowned
/// Zacian or Zamazenta's too (its mod's `teampreview` replaces
/// `(Xerneas|Zacian|Zamazenta)(-[a-zA-Z?-]+)?`). Scarlet/Violet shows a
/// crowned one as it is (`(Zacian|Zamazenta)(?!-Crowned)`) and hides the
/// others, and (without Species Reveal Clause) Greninja's, Gourgeist's,
/// Pumpkaboo's, Xerneas', Silvally's, Urshifu's and Dudunsparce's
/// formes.
pub fn preview_wildcard(dex: &Dex, mechanics: Mechanics, species: SpeciesId) -> Option<SpeciesId> {
    use pikapro_data::gen9::species as s;
    // By base species (the name before its first `-`, as the rule reads
    // it: `wildcard_matches_the_names` checks every species of both dexes).
    let base = dex.species(species).base_species;
    let wild = match mechanics {
        Mechanics::Champions => matches!(base, s::XERNEAS | s::ZACIAN | s::ZAMAZENTA),
        // Zacian and Zamazenta unless crowned; without Species Reveal
        // Clause, these.
        Mechanics::ScarletViolet => matches!(
            base,
            s::ZACIAN
                | s::ZAMAZENTA
                | s::GRENINJA
                | s::GOURGEIST
                | s::PUMPKABOO
                | s::XERNEAS
                | s::SILVALLY
                | s::URSHIFU
                | s::DUDUNSPARCE
        ),
    };
    let crowned = mechanics == Mechanics::ScarletViolet && matches!(species, s::ZACIAN_CROWNED | s::ZAMAZENTA_CROWNED);
    (wild && !crowned).then_some(base)
}

/// `p2a`: a slot as Showdown's `pokemon.getSlot()` names it.
pub(crate) fn slot_id(side: SideId, slot: u8) -> String {
    format!("{}{}", side_id(side), (b'a' + slot) as char)
}

pub(crate) fn category_name(category: pikapro_data::MoveCategory) -> &'static str {
    match category {
        pikapro_data::MoveCategory::Physical => "Physical",
        pikapro_data::MoveCategory::Special => "Special",
        pikapro_data::MoveCategory::Status => "Status",
    }
}

/// A side's id in Showdown's protocol: `p1` or `p2`.
pub fn side_id(side: SideId) -> &'static str {
    match side {
        SideId::P1 => "p1",
        SideId::P2 => "p2",
    }
}

/// The species a Pokémon's `details` show: Showdown's `getUpdatedDetails`
/// shows Greninja-Bond and Rockruff-Dusk as their base species (their
/// formes would give Battle Bond and Own Tempo away): the numeric form of the
/// rule the log's details follow, for views that must know only what is
/// shown.
pub fn details_species(species: SpeciesId) -> SpeciesId {
    use pikapro_data::gen9::species as s;
    match species {
        s::GRENINJA_BOND => s::GRENINJA,
        s::ROCKRUFF_DUSK => s::ROCKRUFF,
        other => other,
    }
}

/// Showdown's `getUpdatedDetails`. Greninja-Bond and Rockruff-Dusk show as
/// their base species ([`details_species`]), then the gender (`, M` / `, F`). `, shiny` comes
/// from the renderer's table (`Renderer::details_of`).
pub(crate) fn details(dex: &pikapro_data::Dex, species: SpeciesId, level: u8, gender: pikapro::Gender) -> String {
    let mut out = dex.species(details_species(species)).name.to_string();
    if level != 100 {
        let _ = write!(out, ", L{level}");
    }
    if gender != pikapro::Gender::Genderless {
        let _ = write!(out, ", {}", gender.letter());
    }
    out
}

/// How Showdown names a volatile in `-start` / `-end` / `-activate`.
pub(crate) fn volatile_name(volatile: Volatile) -> &'static str {
    match volatile {
        Volatile::Confusion => "confusion",
        Volatile::Taunt => "move: Taunt",
        Volatile::Encore => "Encore",
        Volatile::Disable => "Disable",
    }
}

/// How Showdown names a [`ManipulationVolatile`](pikapro::event::ManipulationVolatile) in `-start` / `-end`.
pub(crate) fn manipulation_volatile_name(volatile: pikapro::event::ManipulationVolatile) -> &'static str {
    use pikapro::event::ManipulationVolatile;
    match volatile {
        ManipulationVolatile::AquaRing => "Aqua Ring",
        ManipulationVolatile::Ingrain => "move: Ingrain",
        ManipulationVolatile::Charge => "Charge",
        ManipulationVolatile::FocusEnergy => "move: Focus Energy",
        ManipulationVolatile::DragonCheer => "move: Dragon Cheer",
        ManipulationVolatile::TarShot => "Tar Shot",
        ManipulationVolatile::Stockpile(_) => "Stockpile",
        ManipulationVolatile::PowerTrick => "Power Trick",
        ManipulationVolatile::PowerShift => "Power Shift",
    }
}

/// How Showdown names a volatile effect in `-start` / `-end` /
/// `-damage|...|[from]` (the `move: ` prefix belongs to the name where
/// Showdown writes it).
pub(crate) fn effect_name(dex: &Dex, effect: VolatileEffect) -> &'static str {
    match effect {
        VolatileEffect::Substitute => "Substitute",
        VolatileEffect::PartiallyTrapped(m) => dex.move_(m).name,
        VolatileEffect::Trapped => "trapped",
        VolatileEffect::NoRetreat => "move: No Retreat",
        VolatileEffect::LeechSeed => "Leech Seed",
        VolatileEffect::Yawn => "move: Yawn",
        VolatileEffect::Curse => "Curse",
        VolatileEffect::PerishSong => "Perish Song",
        VolatileEffect::DestinyBond => "Destiny Bond",
        VolatileEffect::Attract => "Attract",
        VolatileEffect::Torment => "Torment",
        VolatileEffect::SaltCure => "Salt Cure",
        VolatileEffect::SyrupBomb => "Syrup Bomb",
        VolatileEffect::ThroatChop => "Throat Chop",
        VolatileEffect::HealBlock => "move: Heal Block",
        VolatileEffect::Imprison => "move: Imprison",
        VolatileEffect::GlaiveRush => "Glaive Rush",
        VolatileEffect::Endure => "Endure",
        VolatileEffect::SmackDown => "Smack Down",
        VolatileEffect::MagnetRise => "Magnet Rise",
        VolatileEffect::FairyLock => "Fairy Lock",
        VolatileEffect::Octolock => "Octolock",
    }
}

/// The weather's Showdown condition name, as `-weather` shows it.
pub(crate) fn weather_name(weather: Weather) -> &'static str {
    match weather {
        Weather::Sun => "SunnyDay",
        Weather::Rain => "RainDance",
        Weather::Sand => "Sandstorm",
        Weather::Snow => "Snowscape",
        Weather::HarshSun => "DesolateLand",
        Weather::HeavyRain => "PrimordialSea",
        Weather::StrongWinds => "DeltaStream",
    }
}

/// A pseudo-weather's or terrain's name, as `-fieldstart` / `-fieldend`
/// write it after `move: `.
pub(crate) fn field_effect_name(effect: FieldEffect) -> &'static str {
    match effect {
        FieldEffect::Terrain(t) => terrain_name(t),
        FieldEffect::TrickRoom => "Trick Room",
        FieldEffect::Gravity => "Gravity",
        FieldEffect::MagicRoom => "Magic Room",
        FieldEffect::WonderRoom => "Wonder Room",
    }
}

pub(crate) fn terrain_name(terrain: Terrain) -> &'static str {
    match terrain {
        Terrain::Electric => "Electric Terrain",
        Terrain::Grassy => "Grassy Terrain",
        Terrain::Misty => "Misty Terrain",
        Terrain::Psychic => "Psychic Terrain",
    }
}

/// How `-sidestart` / `-sideend` name the condition (Reflect has no
/// `move: ` prefix in Showdown).
pub(crate) fn side_condition_name(condition: SideCondition) -> &'static str {
    match condition {
        SideCondition::Reflect => "Reflect",
        SideCondition::LightScreen => "move: Light Screen",
        SideCondition::AuroraVeil => "move: Aurora Veil",
        SideCondition::Tailwind => "move: Tailwind",
        SideCondition::Safeguard => "Safeguard",
        SideCondition::Mist => "Mist",
        SideCondition::WideGuard => "Wide Guard",
        SideCondition::QuickGuard => "Quick Guard",
        SideCondition::StealthRock => "move: Stealth Rock",
        SideCondition::Spikes => "Spikes",
        SideCondition::ToxicSpikes => "move: Toxic Spikes",
        SideCondition::StickyWeb => "move: Sticky Web",
        SideCondition::FirePledge => "Fire Pledge",
        SideCondition::GrassPledge => "Grass Pledge",
        SideCondition::WaterPledge => "Water Pledge",
    }
}

/// A side condition's plain name (Showdown's `condition.name`), as hazard
/// removal and residual damage show it.
pub(crate) fn side_condition_plain_name(condition: SideCondition) -> &'static str {
    let name = side_condition_name(condition);
    name.strip_prefix("move: ").unwrap_or(name)
}

pub(crate) fn status_id(status: Status) -> &'static str {
    match status {
        Status::None => "",
        Status::Burn => "brn",
        Status::Freeze { .. } => "frz",
        Status::Paralysis => "par",
        Status::Poison => "psn",
        Status::Toxic { .. } => "tox",
        Status::Sleep { .. } => "slp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::{Position, event::Actor};
    use pikapro_data::gen9::{moves, species};

    fn actor(side: SideId, slot: u8) -> Actor {
        Actor { pos: Position::new(side, slot), species: species::INCINEROAR, active: true, id: 0 }
    }

    /// A team preview rendered an event at a time reads as rendered whole:
    /// one `|clearpoke`, each Pokémon's own shininess; the next preview
    /// starts over.
    #[test]
    fn a_preview_renders_the_same_in_pieces() {
        let format = Format::champions_vgc();
        let poke = |side, species| BattleEvent::Poke { side, species, level: 50, gender: pikapro::Gender::Male };
        let events = [
            poke(SideId::P1, species::INCINEROAR),
            poke(SideId::P1, species::GARCHOMP),
            poke(SideId::P2, species::INCINEROAR),
            BattleEvent::TeamPreview { bring: Some(4) },
        ];
        let renderer = || Renderer::new(&format, Perspective::Omniscient).with_shiny([vec![false, true], vec![false]]);
        let whole = renderer().log(&events);
        assert_eq!(whole.iter().filter(|l| *l == "|clearpoke").count(), 1, "{whole:?}");
        assert!(whole.iter().any(|l| l.starts_with("|poke|p1|Garchomp, L50, M, shiny")), "{whole:?}");
        let r = renderer();
        for _ in 0..2 {
            let pieces: Vec<String> = events.iter().flat_map(|e| r.log([e])).collect();
            assert_eq!(pieces, whole);
        }
    }

    /// [`preview_wildcard`] reads base species ids; the rule reads names
    /// (kept here as the reference): the same for every species of both
    /// dexes.
    #[test]
    fn wildcard_matches_the_names() {
        fn by_name(dex: &Dex, mechanics: Mechanics, species: SpeciesId) -> Option<SpeciesId> {
            let data = dex.species(species);
            let wild: &[&str] = match mechanics {
                Mechanics::Champions => &["Xerneas", "Zacian", "Zamazenta"],
                Mechanics::ScarletViolet => &[
                    "Zacian",
                    "Zamazenta",
                    "Greninja",
                    "Gourgeist",
                    "Pumpkaboo",
                    "Xerneas",
                    "Silvally",
                    "Urshifu",
                    "Dudunsparce",
                ],
            };
            let base = data.name.split_once('-').map_or(data.name, |(b, _)| b);
            // Only Scarlet/Violet's rule spares the crowned formes.
            let crowned = mechanics == Mechanics::ScarletViolet
                && data.name.ends_with("-Crowned")
                && matches!(base, "Zacian" | "Zamazenta");
            (wild.contains(&base) && !crowned).then_some(data.base_species)
        }
        let mut wild = 0;
        for format in Format::presets() {
            for mechanics in [Mechanics::Champions, Mechanics::ScarletViolet] {
                for k in 0..format.dex.species.len() {
                    let id = SpeciesId(k as u16);
                    let want = by_name(format.dex, mechanics, id);
                    assert_eq!(preview_wildcard(format.dex, mechanics, id), want, "{}", format.dex.species(id).name);
                    wild += usize::from(want.is_some());
                }
            }
        }
        assert!(wild > 40, "{wild} wildcards");
    }

    /// The numeric helpers give exactly the text of a reference that
    /// applies the same rules to strings (`hp_text`, `preview_text`).
    #[test]
    fn visibility_helpers_match_the_text_rules() {
        fn hp_text(mechanics: Mechanics, hp: u16, max_hp: u16) -> String {
            let (hp, max) = (hp as u32, max_hp as u32);
            match mechanics {
                Mechanics::ScarletViolet => {
                    let pct = (100 * hp).div_ceil(max);
                    let pct = if pct == 100 && hp < max { 99 } else { pct };
                    format!("{pct}/100")
                }
                Mechanics::Champions => {
                    let pct = (100 * hp / max).max(1);
                    let hint = match pct {
                        20 if hp * 5 > max => "y",
                        20 => "r",
                        50 if hp * 2 > max => "g",
                        50 => "y",
                        _ => "",
                    };
                    format!("{pct}/100{hint}")
                }
            }
        }
        fn preview_text(mechanics: Mechanics, details: &str) -> String {
            let (species, rest) = details.split_once(',').map_or((details, ""), |(s, r)| (s, r));
            let wild: &[&str] = match mechanics {
                Mechanics::Champions => &["Xerneas", "Zacian", "Zamazenta"],
                Mechanics::ScarletViolet => &[
                    "Zacian",
                    "Zamazenta",
                    "Greninja",
                    "Gourgeist",
                    "Pumpkaboo",
                    "Xerneas",
                    "Silvally",
                    "Urshifu",
                    "Dudunsparce",
                ],
            };
            let base = species.split_once('-').map_or(species, |(b, _)| b);
            // Scarlet/Violet's `(Zacian|Zamazenta)(?!-Crowned)`; Champions'
            // rule hides the crowned formes too.
            let crowned = mechanics == Mechanics::ScarletViolet
                && species.ends_with("-Crowned")
                && matches!(base, "Zacian" | "Zamazenta");
            if wild.contains(&base) && !crowned {
                let sep = if rest.is_empty() { "" } else { "," };
                format!("{base}-*{sep}{rest}")
            } else {
                details.to_string()
            }
        }
        for format in [Format::gen9_vgc(), Format::champions_vgc()] {
            let r = Renderer::new(&format, Perspective::Spectator);
            for max in 1..=400u16 {
                for hp in 1..=max {
                    let (pct, hint) = hp_shown(format.mechanics, hp, max);
                    assert_eq!(format!("{pct}/100{}", hint.suffix()), hp_text(format.mechanics, hp, max), "{hp}/{max}");
                }
            }
            let mut wild = 0;
            for (i, data) in format.dex.species.iter().enumerate() {
                let id = SpeciesId(i as u16);
                for gender in [pikapro::Gender::Male, pikapro::Gender::Genderless] {
                    let d = details(format.dex, id, 50, gender);
                    assert_eq!(r.preview_details(id, &d), preview_text(format.mechanics, &d), "{}", data.name);
                }
                wild += usize::from(preview_wildcard(format.dex, format.mechanics, id).is_some());
            }
            assert!(wild > 0);
        }
    }

    #[test]
    fn renders_lines() {
        let r = Renderer::new(&Format::gen9_singles(), Perspective::Omniscient);
        let user = actor(SideId::P1, 0);
        let target = Actor { pos: Position::new(SideId::P2, 1), species: species::RILLABOOM, active: true, id: 1 };
        assert_eq!(
            r.line(&BattleEvent::Move { user, move_id: moves::KNOCK_OFF, target: Some(target), from: None }),
            "|move|p1a: Incineroar|Knock Off|p2b: Rillaboom"
        );
        let switch = BattleEvent::Switch {
            actor: user,
            team_index: 0,
            level: 50,
            hp: 202,
            max_hp: 202,
            status: Status::None,
            tera: None,
            from: None,
            shown: None,
            gender: pikapro::Gender::Genderless,
        };
        assert_eq!(r.line(&switch), "|switch|p1a: Incineroar|Incineroar, L50|202/202");
        let damage =
            BattleEvent::Damage { target, hp: 0, max_hp: 207, status: Status::None, source: DamageSource::Move };
        assert_eq!(r.line(&damage), "|-damage|p2b: Rillaboom|0 fnt");
        let boost = BattleEvent::Boost { target: user, stat: pikapro_data::BoostId::SpA, amount: 2 };
        assert_eq!(r.line(&boost), "|-boost|p1a: Incineroar|spa|2");
        let unboost = BattleEvent::Unboost { target, stat: pikapro_data::BoostId::Accuracy, amount: 0 };
        assert_eq!(r.line(&unboost), "|-unboost|p2b: Rillaboom|accuracy|0");
    }

    #[test]
    fn renders_field_lines() {
        let r = Renderer::new(&Format::gen9_singles(), Perspective::Omniscient);
        let sand = BattleEvent::Weather { weather: Some(Weather::Sand), upkeep: true };
        assert_eq!(r.line(&sand), "|-weather|Sandstorm|[upkeep]");
        assert_eq!(r.line(&BattleEvent::Weather { weather: None, upkeep: false }), "|-weather|none");
        let tr = BattleEvent::FieldStart { effect: FieldEffect::TrickRoom, of: Some(actor(SideId::P1, 0)) };
        assert_eq!(r.line(&tr), "|-fieldstart|move: Trick Room|[of] p1a: Incineroar");
        let misty = BattleEvent::FieldEnd { effect: FieldEffect::Terrain(Terrain::Misty) };
        assert_eq!(r.line(&misty), "|-fieldend|Misty Terrain");
        let reflect = BattleEvent::SideStart { side: SideId::P2, condition: SideCondition::Reflect };
        assert_eq!(r.line(&reflect), "|-sidestart|p2: p2|Reflect");
        let tailwind = BattleEvent::SideEnd { side: SideId::P1, condition: SideCondition::Tailwind };
        assert_eq!(r.line(&tailwind), "|-sideend|p1: p1|move: Tailwind");
        let chip = BattleEvent::Damage {
            target: actor(SideId::P1, 0),
            hp: 190,
            max_hp: 202,
            status: Status::None,
            source: DamageSource::Weather(Weather::Sand),
        };
        assert_eq!(r.line(&chip), "|-damage|p1a: Incineroar|190/202|[from] Sandstorm");
    }

    #[test]
    fn renders_hazard_and_slot_lines() {
        let r = Renderer::new(&Format::gen9_singles(), Perspective::Omniscient);
        let sr = BattleEvent::SideStart { side: SideId::P2, condition: SideCondition::StealthRock };
        assert_eq!(r.line(&sr), "|-sidestart|p2: p2|move: Stealth Rock");
        let spin = BattleEvent::HazardEnd {
            side: SideId::P1,
            condition: SideCondition::StealthRock,
            from: Some(moves::RAPID_SPIN),
            of: Some(actor(SideId::P1, 0)),
        };
        assert_eq!(r.line(&spin), "|-sideend|p1: p1|Stealth Rock|[from] move: Rapid Spin|[of] p1a: Incineroar");
        let hit = BattleEvent::ConditionDamage {
            target: actor(SideId::P1, 0),
            hp: 150,
            max_hp: 202,
            status: Status::None,
            condition: SideCondition::StealthRock,
        };
        assert_eq!(r.line(&hit), "|-damage|p1a: Incineroar|150/202|[from] Stealth Rock");
        let wish = BattleEvent::WishHeal {
            target: actor(SideId::P1, 0),
            hp: 202,
            max_hp: 202,
            status: Status::None,
            wisher: pikapro_data::gen9::species::RILLABOOM,
            wisher_id: 1,
        };
        assert_eq!(r.line(&wish), "|-heal|p1a: Incineroar|202/202|[from] move: Wish|[wisher] Rillaboom");
        let guard = BattleEvent::GuardStart { actor: actor(SideId::P2, 0), condition: SideCondition::WideGuard };
        assert_eq!(r.line(&guard), "|-singleturn|p2a: Incineroar|Wide Guard");
    }

    #[test]
    fn hides_foe_hp() {
        let damage = |hp, max_hp| BattleEvent::Damage {
            target: actor(SideId::P2, 0),
            hp,
            max_hp,
            status: Status::None,
            source: DamageSource::Move,
        };
        let sv = Format::gen9_singles();
        // The custom game gen9singles mirrors reports exact HP to everyone.
        let custom = Renderer::new(&sv, Perspective::Player(SideId::P1));
        assert_eq!(custom.line(&damage(150, 202)), "|-damage|p2a: Incineroar|150/202");
        let info = BattleInfo { report_exact_hp: false, ..BattleInfo::for_format(&sv) };
        let mine = Renderer::new(&sv, Perspective::Player(SideId::P2)).with_info(info.clone());
        let theirs = Renderer::new(&sv, Perspective::Player(SideId::P1)).with_info(info);
        assert_eq!(mine.line(&damage(101, 202)), "|-damage|p2a: Incineroar|101/202");
        // Showdown Gen 7+: ceil, capped at 99 while hurt.
        assert_eq!(theirs.line(&damage(150, 202)), "|-damage|p2a: Incineroar|75/100");
        assert_eq!(theirs.line(&damage(1, 202)), "|-damage|p2a: Incineroar|1/100");
        assert_eq!(theirs.line(&damage(201, 202)), "|-damage|p2a: Incineroar|99/100");
        assert_eq!(theirs.line(&damage(202, 202)), "|-damage|p2a: Incineroar|100/100");

        let ch = Renderer::new(&Format::champions_vgc(), Perspective::Spectator);
        // Champions: floor, at least 1, colour hints at 20% and 50%.
        assert_eq!(ch.line(&damage(150, 202)), "|-damage|p2a: Incineroar|74/100");
        assert_eq!(ch.line(&damage(1, 202)), "|-damage|p2a: Incineroar|1/100");
        assert_eq!(ch.line(&damage(41, 200)), "|-damage|p2a: Incineroar|20/100y");
        assert_eq!(ch.line(&damage(40, 200)), "|-damage|p2a: Incineroar|20/100r");
        assert_eq!(ch.line(&damage(101, 200)), "|-damage|p2a: Incineroar|50/100g");
        assert_eq!(ch.line(&damage(100, 200)), "|-damage|p2a: Incineroar|50/100y");
    }

    #[test]
    fn writes_split_blocks_and_channels() {
        let r = Renderer::new(&Format::champions_vgc(), Perspective::Omniscient);
        let damage = BattleEvent::Damage {
            target: actor(SideId::P2, 0),
            hp: 150,
            max_hp: 202,
            status: Status::None,
            source: DamageSource::Move,
        };
        let raw = r.raw([&BattleEvent::Blank, &damage]);
        assert_eq!(raw, ["|", "|split|p2", "|-damage|p2a: Incineroar|150/202", "|-damage|p2a: Incineroar|74/100"]);
        assert_eq!(extract_channel(&raw, Perspective::Player(SideId::P2))[1], "|-damage|p2a: Incineroar|150/202");
        assert_eq!(extract_channel(&raw, Perspective::Player(SideId::P1))[1], "|-damage|p2a: Incineroar|74/100");
        assert_eq!(extract_channel(&raw, Perspective::Spectator)[1], "|-damage|p2a: Incineroar|74/100");
        for p in [Perspective::Omniscient, Perspective::Spectator, Perspective::Player(SideId::P1)] {
            let r = Renderer::new(&Format::champions_vgc(), p);
            assert_eq!(r.log([&BattleEvent::Blank, &damage]), extract_channel(&raw, p));
        }
    }

    #[test]
    fn amends_the_last_move_line() {
        use pikapro::event::{MoveAttr, SpreadSlots};
        let r = Renderer::new(&Format::gen9_doubles(), Perspective::Omniscient);
        let user = actor(SideId::P1, 0);
        let target = actor(SideId::P2, 0);
        let mv = BattleEvent::Move { user, move_id: moves::EARTHQUAKE, target: Some(target), from: None };
        let mut slots = SpreadSlots::default();
        slots.slots[0] = pikapro::Position::new(SideId::P1, 1);
        slots.slots[1] = pikapro::Position::new(SideId::P2, 1);
        slots.len = 2;
        let spread = BattleEvent::MoveAttr { attr: MoveAttr::Spread(slots) };
        assert_eq!(
            r.log([&mv, &BattleEvent::Ohko, &spread]),
            ["|move|p1a: Incineroar|Earthquake|p2a: Incineroar|[spread] p1b,p2b", "|-ohko"]
        );
        let still = BattleEvent::MoveAttr { attr: MoveAttr::Still };
        assert_eq!(r.log([&mv, &still]), ["|move|p1a: Incineroar|Earthquake||[still]"]);
        // Nothing to amend in a slice without a move line.
        assert_eq!(r.log([&still]), Vec::<String>::new());
        // Nor in a resolution without one: the move line before `|t:|` was
        // sent.
        let (blank, t) = (BattleEvent::Blank, BattleEvent::Timestamp);
        let whole = r.log([&mv, &blank, &t, &still]);
        assert_eq!(whole[0], "|move|p1a: Incineroar|Earthquake|p2a: Incineroar");
        assert_eq!(whole, [r.log([&mv]), r.log([&blank, &t, &still])].concat());
    }

    #[test]
    fn names_pokemon_by_nickname() {
        let format = Format::gen9_singles();
        let mut sheet = crate::TeamSheet::new(pikapro::testing::sample_team(&format));
        sheet.extras[1].name = "A Very Long Nickname Indeed".into();
        let r = Renderer::new(&format, Perspective::Omniscient).with_nicknames([sheet.nicknames(&format), vec![]]);
        let named = Actor { id: 1, ..actor(SideId::P1, 0) };
        assert_eq!(r.line(&BattleEvent::Faint { actor: named }), "|faint|p1a: A Very Long Nickname");
        assert_eq!(r.line(&BattleEvent::Faint { actor: actor(SideId::P1, 0) }), "|faint|p1a: Incineroar");
    }

    #[test]
    fn renders_status() {
        let r = Renderer::new(&Format::gen9_singles(), Perspective::Omniscient);
        let target = actor(SideId::P2, 0);
        let toxic = Status::Toxic { counter: 2 };
        let damage =
            BattleEvent::Damage { target, hp: 50, max_hp: 202, status: toxic, source: DamageSource::Status(toxic) };
        assert_eq!(r.line(&damage), "|-damage|p2a: Incineroar|50/202 tox|[from] psn");
        let sleep = Status::Sleep { turns: 2 };
        let start = BattleEvent::StatusStart { target, status: sleep, from_move: Some(moves::SPORE) };
        assert_eq!(r.line(&start), "|-status|p2a: Incineroar|slp|[from] move: Spore");
        let start = BattleEvent::StatusStart { target, status: Status::Burn, from_move: None };
        assert_eq!(r.line(&start), "|-status|p2a: Incineroar|brn");
        let cure = BattleEvent::CureStatus { target, status: sleep, from_move: None };
        assert_eq!(r.line(&cure), "|-curestatus|p2a: Incineroar|slp|[msg]");
        let freeze = Status::Freeze { turns: 0 };
        let thaw = BattleEvent::CureStatus { target, status: freeze, from_move: Some(moves::FLARE_BLITZ) };
        assert_eq!(r.line(&thaw), "|-curestatus|p2a: Incineroar|frz|[from] move: Flare Blitz");
        let cant = BattleEvent::StatusCant { actor: target, status: Status::Paralysis };
        assert_eq!(r.line(&cant), "|cant|p2a: Incineroar|par");
        let fail = BattleEvent::StatusFail { target, status: Status::Burn };
        assert_eq!(r.line(&fail), "|-fail|p2a: Incineroar|brn");
    }
}
