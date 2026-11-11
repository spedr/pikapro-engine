//! Reading a player's channel: Showdown's protocol text back into the
//! engine's typed [`BattleEvent`]s, and `|request|` JSON back into
//! [`RequestData`](pikapro::request::RequestData).
//!
//! [`ChannelParser`] is the inverse of the [`Renderer`](crate::Renderer)
//! for one perspective: fed a channel line by line (what Showdown's
//! `getPlayerStreams`, or a websocket, sends a player), it hands the
//! events each line shows to a sink. A view built from those events
//! (`pikapro-view`'s tracker) is the view the typed path builds from the
//! engine's own events, as long as it only reads what the text shows.
//!
//! * **Events.** Each line gives the event the renderer writes it from.
//!   Where several events render to the same text, the parser picks one
//!   that says the same to a reader of the text: text is all it has.
//!   Values the text doesn't carry are left at a neutral value (a switch's
//!   `team_index`, a status's hidden counters; a percentage `74/100` is HP
//!   `74/100`, `20/100y` HP `201/1000`, so that
//!   [`hp_shown`](crate::hp_shown) gives the percentage and hint back).
//! * **Identity.** [`Actor::id`] names a Pokémon as the log shows it:
//!   by its ident and details (`p2a: Nickname`, `Species, L50, F`), stable
//!   for the battle. With team preview a foe's id is the `|poke|` line it
//!   matches (registration order, as the engine's ids); without, the order
//!   of first appearance. The player's own ids are its registration ids,
//!   as the engine's: with team preview the `|poke|` line each matches,
//!   without its team position at the battle's first request (a channel
//!   joined mid-battle works it out: [`ChannelParser::first_request`]). An
//!   Illusion is named as its disguise, as the typed path names it, until
//!   the `|replace|`.
//! * **Real Showdown.** Room and chat lines, `|split|` blocks, `|error|`,
//!   empty `|request|`s and anything else a client may be sent are handled
//!   or ignored as a client would; a line it doesn't know is counted
//!   ([`ChannelParser::unknown_lines`]), never a panic. Numbers no server
//!   sends (HP past its max, a boost of 128) are clamped, and a side's
//!   identity table holds at most 24 Pokémon (Showdown's largest team):
//!   hostile text can't grow it or overflow a count.
//!
//! Nothing is allocated per line, except a Pokémon's name the first time
//! it shows, a player's name (`|player|`), and a first request's read-ahead
//! ([`ChannelParser::first_request`]).

// The JSON reader behind the request parser: public for the conformance
// tools only, not part of the API.
#[doc(hidden)]
pub mod json;
mod names;
mod request;

use pikapro::event::{
    AbilityNote, Actor, CantReason, Cause, DamageSource, EffectName, FailNote, FieldEffect, HealSource, ItemEndNote,
    LineArg, ManipulationVolatile, Message, MoveAnim, MoveAttr, MoveFrom, SpreadSlots, VolatileEffect,
};
use pikapro::state::{SideCondition, Weather};
use pikapro::util::StackVec;
use pikapro::{BattleEvent, Format, Gender, MAX_ACTIVE, MAX_TEAM, Position, SideId, Status};
use pikapro_data::gen9::{abilities, moves, species};
use pikapro_data::{AbilityId, BoostId, Dex, ItemId, MoveId, SpeciesId, Type, TypeSet};

pub use json::JsonError;
pub use request::{ParseError, ParsedRequest, parse_request};

use names::*;
use request::{Details, details_or_standin, parse_request_with, parse_status, species_or_standin};

/// What kind of line [`ChannelParser::feed`] was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LineKind {
    /// A battle line. Its events (none for a line that tells a view
    /// nothing, such as `|-crit|`... which still gives its event) went to
    /// the sink.
    Battle,
    /// `|request|<JSON>`: read it with [`ChannelParser::request`]
    /// ([`request_body`] gives the JSON). An empty one is [`LineKind::Ignored`].
    Request,
    /// `|error|<message>`: `[Unavailable choice]` (a request with
    /// `update: true` follows) or any other error.
    Error { unavailable: bool },
    /// `|turn|<n>` (its event went to the sink).
    Turn(u16),
    /// `|win|<name>` or `|tie|` (its event went to the sink): `None` for a
    /// tie.
    End(Option<SideId>),
    /// `|win|<name>` whose name matches neither player's (a renamed
    /// player, a channel without `|player|` lines): the battle ended, its
    /// winner unknown. No event; counted in
    /// [`ChannelParser::unknown_lines`].
    EndUnknown,
    /// `|start|` (its event went to the sink).
    Start,
    /// `|poke|` (its event went to the sink).
    Poke,
    /// `|teampreview|` (its event went to the sink).
    TeamPreview,
    /// `|player|<side>|<name>|...`.
    Player(SideId),
    /// `|showteam|<side>|<packed team>`: Open Team Sheets (the caller
    /// unpacks the team; no event goes to the sink).
    ShowTeam(SideId),
    /// A line a battle client ignores or only displays (chat, room and
    /// header lines, `|debug|`, a `|split|` marker, the other channel's
    /// line of a `|split|` block).
    Ignored,
    /// A line this parser doesn't know (counted).
    Unknown,
}

/// The JSON of a `|request|` line (`None` for another line or an empty
/// request).
pub fn request_body(line: &str) -> Option<&str> {
    line.strip_prefix("|request|").filter(|b| !b.trim().is_empty())
}

/// A Pokémon a side has shown.
#[derive(Clone, Debug)]
struct Known {
    /// Its name (nickname, or base species), as idents show it.
    name: String,
    /// The species its details show now (a permanent forme change updates
    /// it).
    species: SpeciesId,
    /// The species shown in its slot now (temporary formes too): the
    /// actor's species.
    current: SpeciesId,
    level: u8,
    gender: Gender,
    shiny: bool,
    /// Its `|faint|` was shown (and no revival since).
    fainted: bool,
    /// Its team position in the last request (own Pokémon).
    position: Option<u8>,
    /// The log has shown it (switched in, or named).
    seen: bool,
    /// The max HP its last HP line showed (0 before one).
    max_hp: u16,
    /// It Terastallized.
    terastallized: bool,
    /// Its gender is known (from details).
    gender_known: bool,
    /// Named before the log showed its details (an `[of]` naming a foe
    /// before its switch-in): it holds no `|poke|` line's id until its
    /// switch-in matches one.
    placeholder: bool,
    /// The status its last request showed (own Pokémon).
    status: Status,
    id: u8,
}

/// How a Pokémon first showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    /// Its details (a switch-in).
    Shown,
    /// Its name alone (an ident before its switch-in).
    Named,
    /// Its name alone, the species of the one unclaimed `|poke|` line with
    /// this id.
    NamedPoke(u8),
    /// A request's entry.
    Request,
}

/// A `|poke|` line.
#[derive(Clone, Copy, Debug, Default)]
struct Poke {
    species: SpeciesId,
    wildcard: bool,
    level: u8,
    gender: Gender,
}

/// Most Pokémon a side's identity table holds (Showdown's custom games
/// allow 24 per team). A name past them isn't read: its line is counted
/// as unknown.
const MAX_KNOWN: usize = 24;

/// Ids go up to this (exclusive); `Ids::used` has a bit per id. With at
/// most [`MAX_KNOWN`] Pokémon, and [`MAX_TEAM`] more in a request, one is
/// always free.
const MAX_IDS: u8 = 32;

/// Most own switches kept for [`ChannelParser::first_request`]'s replay.
const MAX_HISTORY: usize = 1 << 16;

/// Most entries of [`ChannelParser`]'s max HP table.
const MAX_HP_ENTRIES: usize = 64;

/// A change of who is in the player's own slots, as its lines show it (for
/// [`ChannelParser::first_request`]).
#[derive(Clone, Copy, Debug)]
enum Moved {
    /// Known Pokémon `k` switched (or was dragged) into `slot`, showing
    /// details species `species`, max HP `max_hp` (the player's own HP
    /// lines are exact: an Illusion's is the Pokémon behind it's) and
    /// whether it Terastallized (`tera:`, the one behind it's too).
    /// `gas`: a Neutralizing Gas had shown by then (under it, an Illusion
    /// user's disguise isn't chosen again: it keeps its last).
    In { slot: u8, k: u8, species: SpeciesId, hp: u16, max_hp: u16, tera: bool, gas: bool },
    /// The one in `slot` Terastallized.
    Tera { slot: u8 },
    /// `|replace|`: the Pokémon in `slot` since its switch-in is `k`.
    Replace { slot: u8, k: u8 },
    /// `|swap|`: two slots traded places (Ally Switch).
    Swap(u8, u8),
    /// The Pokémon in `slot` fainted.
    Faint { slot: u8 },
    /// An HP line showed the one in `slot` (named as known Pokémon `k`,
    /// with details species `species`) at `hp` of max HP `max`.
    Hp { slot: u8, k: u8, species: SpeciesId, hp: u16, max: u16 },
    /// Known Pokémon `k` was revived (Revival Blessing).
    Revive { k: u8 },
}

/// What [`ChannelParser::first_request`]'s read-ahead found for the
/// request's Pokémon (entries by team position).
#[derive(Clone, Copy, Debug)]
struct Replay {
    /// Entry `order[p]` was at team position `p` when the battle started.
    order: [usize; MAX_TEAM],
    /// Per entry, the max HP its HP lines last showed, per details species
    /// (0: none).
    hp: [[(SpeciesId, u16); 4]; MAX_TEAM],
    /// Another order fits the lines as well (an Illusion user never
    /// revealed, with its disguise's max HP): no client could tell which.
    ambiguous: bool,
}

/// A path of [`ChannelParser::replay`]'s search: the events left to undo,
/// what it found so far, and the state after those events.
#[derive(Clone, Copy)]
struct Path {
    ev: usize,
    r: Replay,
    /// The entries fainted (a bit each).
    fainted: u8,
    /// Each entry's max HP as far as known.
    max_hp: [u16; MAX_TEAM],
    /// The entry that Terastallized, once known.
    tera: Option<usize>,
    /// The side's Terastallization happened.
    tera_on: bool,
    /// The HP each entry's last line so far back must show (the HP it had
    /// when it left the field, or has now), or [`NO_NEED`].
    need: [u16; MAX_TEAM],
}

/// No HP a line must show ([`Path::need`]).
const NO_NEED: u16 = u16::MAX;

/// What one side has shown: the identity table.
#[derive(Clone, Debug, Default)]
struct Ids {
    /// At most [`MAX_KNOWN`].
    mons: Vec<Known>,
    pokes: StackVec<Poke, MAX_TEAM>,
    /// Ids given out (bit per id).
    used: u32,
    /// The known Pokémon each slot shows.
    slots: [Option<u8>; MAX_ACTIVE],
    /// The max HP each slot's last HP line showed.
    slot_max: [u16; MAX_ACTIVE],
}

impl Ids {
    fn active(&self, k: usize) -> bool {
        self.slots.contains(&Some(k as u8))
    }

    /// The Pokémon named `name` whose details species is `species` (or
    /// shares its base species), preferring one on the field (`active`) or
    /// off it.
    fn find(&self, dex: &Dex, name: &str, species: Option<SpeciesId>, prefer_active: Option<bool>) -> Option<usize> {
        let base = |s: SpeciesId| dex.species(s).base_species;
        let rank = |k: usize, m: &Known| -> Option<u8> {
            if m.name != name {
                return None;
            }
            let species_rank = match species {
                None => 0,
                Some(s) if m.species == s || m.current == s => 0,
                Some(s) if base(m.species) == base(s) => 1,
                Some(_) => 2,
            };
            // On the field: the one in a slot. Off it: one that just fainted
            // in its slot (not replaced yet) before one on the bench.
            let place = match prefer_active {
                Some(true) => u8::from(!self.active(k)),
                Some(false) => match (self.active(k), m.fainted) {
                    (true, true) => 0,
                    (false, _) => 1,
                    (true, false) => 2,
                },
                None => 0,
            };
            // One the battle has shown before one it never has (a
            // nickname shared with a Pokémon left out at team preview).
            let unseen = u8::from(!m.seen);
            Some(species_rank * 8 + unseen * 4 + place)
        };
        self.mons.iter().enumerate().filter_map(|(k, m)| rank(k, m).map(|r| (r, k))).min().map(|(_, k)| k)
    }

    fn by_id(&self, id: u8) -> Option<usize> {
        self.mons.iter().position(|m| m.id == id)
    }

    /// The lowest id from `from` on not given out.
    fn free_id(&self, from: u8) -> Option<u8> {
        (from..MAX_IDS).find(|&i| self.used & (1 << i) == 0)
    }

    /// The one `|poke|` line not given out whose species is `species`'s
    /// (the same base species), if exactly one is.
    fn unclaimed_poke(&self, dex: &Dex, species: SpeciesId) -> Option<u8> {
        let base = |s: SpeciesId| dex.species(s).base_species;
        let mut fit = (0..self.pokes.len())
            .filter(|&i| self.used & (1 << i) == 0 && base(self.pokes[i].species) == base(species));
        let first = fit.next()?;
        fit.next().is_none().then_some(first as u8)
    }

    /// The `|poke|` line (its index) not given out that details `d` fit
    /// best, if any.
    fn poke_for(&self, dex: &Dex, d: &Details) -> Option<u8> {
        (0..FIT_LEVELS)
            .find_map(|strict| {
                (0..self.pokes.len()).find(|&i| self.used & (1 << i) == 0 && fits(dex, &self.pokes[i], d, strict))
            })
            .map(|i| i as u8)
    }
}

/// Whether details `d` fit a `|poke|` line, loosest last (`strict` up to
/// [`FIT_LEVELS`]): 0 by species, gender and level; 1 by species and
/// gender; then a forme the details show and team preview didn't (a Mega,
/// Terapagos-Terastal): 2 of the line's species itself (Raichu-Mega-Y of
/// Raichu, not of Raichu-Alola), 3 of its base species. A preview wildcard
/// (`Urshifu-*`) fits its every forme.
fn fits(dex: &Dex, p: &Poke, d: &Details, strict: u8) -> bool {
    let base = |s: SpeciesId| dex.species(s).base_species;
    let species = p.species == d.species
        || base(p.species) == base(d.species)
            && (p.wildcard || strict == 2 && p.species == base(d.species) || strict >= 3);
    let gender = strict >= 2 || p.gender == d.gender;
    let level = strict >= 1 || p.level == d.level;
    species && gender && level
}

/// How many levels [`fits`] has.
const FIT_LEVELS: u8 = 4;

/// A request's Pokémon as its entries show them, by team position.
struct Entries<'a> {
    n: usize,
    names: [std::borrow::Cow<'a, str>; MAX_TEAM],
    details: [Details; MAX_TEAM],
    /// Its set's ability is Illusion.
    illusion: [bool; MAX_TEAM],
    /// `0 fnt`.
    fainted: [bool; MAX_TEAM],
    /// Its max HP (0 when fainted: `0 fnt` shows none).
    max_hp: [u16; MAX_TEAM],
    /// It Terastallized.
    terastallized: [bool; MAX_TEAM],
    /// Its HP.
    hp: [u16; MAX_TEAM],
    /// Its set's ability is Regenerator (it heals as it leaves the field,
    /// with no line).
    regenerator: [bool; MAX_TEAM],
}

impl<'a> Entries<'a> {
    /// `raw`: each entry's name as sent (JSON escapes kept).
    fn of(raw: &[&'a str; MAX_TEAM], data: &pikapro::request::RequestData) -> Entries<'a> {
        let details = |i: usize| {
            let (species, level, gender) = data
                .pokemon
                .get(i)
                .map_or((SpeciesId::default(), 100, Gender::Genderless), |p| (p.species, p.level, p.gender));
            Details { species, level, gender, shiny: false, tera: None, wildcard: false }
        };
        Entries {
            n: data.pokemon.len().min(MAX_TEAM),
            names: core::array::from_fn(|i| json::Str(raw[i]).text()),
            details: core::array::from_fn(details),
            illusion: core::array::from_fn(|i| {
                data.pokemon.get(i).is_some_and(|p| p.base_ability == abilities::ILLUSION)
            }),
            fainted: core::array::from_fn(|i| data.pokemon.get(i).is_some_and(|p| p.hp == 0)),
            terastallized: core::array::from_fn(|i| data.pokemon.get(i).is_some_and(|p| p.terastallized.is_some())),
            max_hp: core::array::from_fn(|i| data.pokemon.get(i).map_or(0, |p| p.max_hp)),
            hp: core::array::from_fn(|i| data.pokemon.get(i).map_or(0, |p| p.hp)),
            regenerator: core::array::from_fn(|i| {
                data.pokemon.get(i).is_some_and(|p| p.base_ability == abilities::REGENERATOR)
            }),
        }
    }
}

/// Reads a player's (or a spectator's) channel. See the module docs.
#[derive(Clone)]
pub struct ChannelParser {
    dex: &'static Dex,
    me: Option<SideId>,
    players: [String; 2],
    sides: [Ids; 2],
    /// Inside a `|split|<side>` block: its side and whether its secret
    /// line was taken (`None` before it).
    split: Option<(SideId, Option<bool>)>,
    unknown: u32,
    /// Names the dex doesn't know.
    unknown_names: u32,
    /// The Pokémon a side chose to revive (Revival Blessing), by id.
    revival: [Option<u8>; 2],
    requests: u32,
    /// Each own Pokémon's max HP by id and species, from the requests (a
    /// fainted one's shows `0 fnt`); at most [`MAX_HP_ENTRIES`].
    max_hp: Vec<(u8, SpeciesId, u16)>,
    /// The channel shows every side's exact HP (a custom game's); the
    /// player's own is exact in any player channel.
    exact_hp: bool,
    /// Who came into the player's own slots, as the lines showed it
    /// (kept only by [`ChannelParser::first_request`]'s read-ahead);
    /// `None` also when it outgrew [`MAX_HISTORY`].
    moved: Option<Vec<Moved>>,
    /// The first request's ids by team position, when its Pokémon's
    /// registration order was worked out from lines read ahead.
    first_ids: Option<[u8; MAX_TEAM]>,
    /// See [`ChannelParser::ambiguous_start`].
    ambiguous_start: bool,
    /// A Neutralizing Gas has shown (for [`Moved::In`]).
    gas: bool,
    /// Per side and slot, the length of `moved` when its Pokémon switched
    /// in: a Neutralizing Gas it shows later was already on the field for
    /// the player's switch-ins since (for [`Moved::In`]).
    entered_at: [[usize; MAX_ACTIVE]; 2],
}

impl core::fmt::Debug for ChannelParser {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelParser")
            .field("dex", &self.dex.name)
            .field("me", &self.me)
            .field("players", &self.players)
            .field("sides", &self.sides)
            .field("split", &self.split)
            .field("unknown", &self.unknown)
            .field("unknown_names", &self.unknown_names)
            .field("requests", &self.requests)
            .finish()
    }
}

/// Up to this many `|`-separated parts are read from a line.
const MAX_PARTS: usize = 16;

/// A line split at `|`: `parts[0]` is the command.
struct Parts<'a> {
    v: [&'a str; MAX_PARTS],
    n: usize,
}

impl<'a> Parts<'a> {
    fn of(line: &'a str) -> Parts<'a> {
        let mut v = [""; MAX_PARTS];
        let mut n = 0;
        // A line starts with `|`; everything after it.
        let body = &line[1..];
        let mut rest = body;
        loop {
            if n == MAX_PARTS - 1 {
                v[n] = rest;
                n += 1;
                break;
            }
            match rest.find('|') {
                Some(i) => {
                    v[n] = &rest[..i];
                    n += 1;
                    rest = &rest[i + 1..];
                }
                None => {
                    v[n] = rest;
                    n += 1;
                    break;
                }
            }
        }
        Parts { v, n }
    }

    fn get(&self, i: usize) -> &'a str {
        if i < self.n { self.v[i] } else { "" }
    }

    /// The positional arguments from `i` on: up to the first `[...]` tag.
    fn positional(&self, i: usize) -> impl Iterator<Item = &'a str> + '_ {
        (i..self.n).map(|k| self.v[k]).take_while(|a| !is_tag(a))
    }

    /// The value of the tag `[name] value`, if present.
    fn tag(&self, name: &str) -> Option<&'a str> {
        (1..self.n).map(|k| self.v[k]).find_map(|a| {
            let rest = a.strip_prefix('[')?.strip_prefix(name)?.strip_prefix(']')?;
            Some(rest.strip_prefix(' ').unwrap_or(rest))
        })
    }

    fn has(&self, flag: &str) -> bool {
        (1..self.n).any(|k| self.v[k] == flag)
    }
}

/// A `[tag]` argument (`[from] ...`, `[silent]`).
fn is_tag(a: &str) -> bool {
    a.starts_with('[') && a.contains(']')
}

impl ChannelParser {
    /// A parser for `format`'s channel of player `me` (`None`: a
    /// spectator's, or a player not known yet: the first request says
    /// which).
    pub fn new(format: &Format, me: Option<SideId>) -> ChannelParser {
        ChannelParser {
            dex: format.dex,
            me,
            players: [String::new(), String::new()],
            sides: [Ids::default(), Ids::default()],
            split: None,
            unknown: 0,
            unknown_names: 0,
            revival: [None; 2],
            requests: 0,
            max_hp: Vec::new(),
            exact_hp: false,
            moved: None,
            first_ids: None,
            ambiguous_start: false,
            gas: false,
            entered_at: [[0; MAX_ACTIVE]; 2],
        }
    }

    /// Says the channel shows every side's exact HP (Showdown's custom
    /// games: [`ShowdownFormat::exact_hp`]). An off-field HP line whose
    /// max HP differs from the one its name last showed is then read as
    /// another Pokémon of that name's; percentages can't tell.
    pub fn set_exact_hp(&mut self, exact: bool) {
        self.exact_hp = exact;
    }

    /// The player whose channel this is, once known.
    pub fn me(&self) -> Option<SideId> {
        self.me
    }

    /// Sets the player whose channel this is (from its `|player|` line).
    pub fn set_me(&mut self, me: SideId) {
        self.me = Some(me);
    }

    /// A player's name as its `|player|` line gave it.
    pub fn player_name(&self, side: SideId) -> &str {
        &self.players[side.index()]
    }

    /// Lines this parser didn't know.
    pub fn unknown_lines(&self) -> u32 {
        self.unknown
    }

    /// Names (species, moves, items, abilities) the format's dex didn't
    /// know, in lines and requests: a newer server's. Each was read as a
    /// stand-in (see the module docs).
    pub fn unknown_names(&self) -> u32 {
        self.unknown_names
    }

    /// The first request came mid-battle ([`ChannelParser::first_request`])
    /// and the lines before it fit two team orders at the battle's start:
    /// an own Illusion user they never revealed, with the max HP of the one
    /// it disguised as. No client can tell which; the ids follow the
    /// Pokémon the lines named.
    pub fn ambiguous_start(&self) -> bool {
        self.ambiguous_start
    }

    /// Requests read so far.
    pub fn requests(&self) -> u32 {
        self.requests
    }

    /// The name side `side`'s Pokémon with id `id` goes by, once shown.
    pub fn name_of(&self, side: SideId, id: u8) -> Option<&str> {
        let ids = &self.sides[side.index()];
        ids.by_id(id).map(|k| ids.mons[k].name.as_str())
    }

    /// The player chose to revive its Pokémon `id` (Revival Blessing): the
    /// next `-heal|<side>: <name>|...|[from] move: Revival Blessing` names
    /// it, whatever other Pokémon of the side share its name.
    pub fn expect_revival(&mut self, side: SideId, id: u8) {
        self.revival[side.index()] = Some(id);
    }

    /// Whether side `side`'s Pokémon with id `id` showed as shiny.
    #[cfg(test)]
    pub(crate) fn shiny_of(&self, side: SideId, id: u8) -> bool {
        let ids = &self.sides[side.index()];
        ids.by_id(id).is_some_and(|k| ids.mons[k].shiny)
    }

    /// Feeds one line (without its newline); the events it shows go to
    /// `sink`.
    pub fn feed(&mut self, line: &str, sink: &mut impl FnMut(BattleEvent)) -> LineKind {
        self.feed_dyn(line, sink)
    }

    fn feed_dyn(&mut self, line: &str, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // `|split|<side>`: the secret line (this side's and the
        // omniscient channel's), then everyone else's.
        if let Some((side, taken)) = self.split {
            match taken {
                None => {
                    let mine = self.me == Some(side);
                    self.split = Some((side, Some(mine)));
                    if !mine {
                        return LineKind::Ignored;
                    }
                }
                Some(taken) => {
                    self.split = None;
                    if taken || line.is_empty() {
                        return LineKind::Ignored;
                    }
                }
            }
        }
        if line.is_empty() {
            return LineKind::Ignored;
        }
        if !line.starts_with('|') {
            // A room id (`>battle-...`) or plain text shown as is.
            return LineKind::Ignored;
        }
        let p = Parts::of(line);
        let kind = self.dispatch(&p, line, sink);
        if kind == LineKind::Unknown {
            self.unknown = self.unknown.saturating_add(1);
        }
        kind
    }

    // -----------------------------------------------------------------
    // Identity.

    fn side_of(text: &str) -> Option<SideId> {
        match text.get(..2)? {
            "p1" => Some(SideId::P1),
            "p2" => Some(SideId::P2),
            _ => None,
        }
    }

    /// The Pokémon an ident (`p2a: Name`, `p2: Name`) names; a new one
    /// if it hasn't shown (from its name).
    fn ident(&mut self, text: &str) -> Option<Actor> {
        let (pos, name) = text.split_once(": ")?;
        let side = Self::side_of(pos)?;
        let slot = match pos.as_bytes().get(2) {
            None => None,
            Some(&c @ b'a'..=b'f') => Some(c - b'a'),
            Some(_) => return None,
        };
        let dex = self.dex;
        let ids = &self.sides[side.index()];
        let k = match slot.and_then(|s| ids.slots.get(s as usize).copied().flatten()) {
            Some(k) if ids.mons[k as usize].name == name => Some(k as usize),
            _ => ids.find(dex, name, None, Some(slot.is_some())),
        };
        let k = match k {
            Some(k) => k,
            None => {
                // Not shown yet (named before its switch-in): its name
                // tells its species unless it is nicknamed. It takes no
                // `|poke|` line's id until its switch-in shows its details,
                // unless its name is the species of exactly one `|poke|`
                // line no Pokémon has claimed: an Illusion user that
                // fainted disguised, named at last by Revival Blessing.
                let species = dex.species_by_name(name).unwrap_or_default();
                let details = Details {
                    species,
                    level: 100,
                    gender: Gender::Genderless,
                    shiny: false,
                    tera: None,
                    wildcard: false,
                };
                let origin = match self.sides[side.index()].unclaimed_poke(dex, species) {
                    Some(id) if dex.species_by_name(name).is_some() => Origin::NamedPoke(id),
                    _ => Origin::Named,
                };
                self.new_mon(side, name, &details, origin)?
            }
        };
        let ids = &self.sides[side.index()];
        let m = &ids.mons[k];
        // Off the field, the engine names a Pokémon by its team position:
        // the slot it fainted in, or (own Pokémon) its request's position.
        let position = slot.unwrap_or_else(|| {
            let fainted_in = ids.slots.iter().position(|s| *s == Some(k as u8));
            fainted_in.map(|s| s as u8).or(m.position).unwrap_or(u8::MAX)
        });
        Some(Actor { pos: Position::new(side, position), species: m.current, active: slot.is_some(), id: m.id })
    }

    /// A slot (`p2a`) as `[spread]` names it.
    fn slot(text: &str) -> Option<Position> {
        let side = Self::side_of(text)?;
        let slot = text.as_bytes().get(2).filter(|c| c.is_ascii_lowercase())? - b'a';
        Some(Position::new(side, slot))
    }

    /// A Pokémon shown for the first time: its index in the table (`None`
    /// once the table holds [`MAX_KNOWN`]).
    ///
    /// Its id: with team preview, the `|poke|` line its details match
    /// (a switch-in's), or one past them (a name alone); else the lowest
    /// free one (order of appearance).
    fn new_mon(&mut self, side: SideId, name: &str, d: &Details, origin: Origin) -> Option<usize> {
        let dex = self.dex;
        let ids = &mut self.sides[side.index()];
        if ids.mons.len() >= MAX_KNOWN {
            return None;
        }
        let id = match origin {
            Origin::Shown => ids.poke_for(dex, d).or_else(|| ids.free_id(0)),
            Origin::Named => ids.free_id(ids.pokes.len() as u8),
            Origin::NamedPoke(id) => Some(id),
            Origin::Request => ids.free_id(0),
        }?;
        ids.used |= 1 << id;
        ids.mons.push(Known {
            name: name.to_string(),
            species: d.species,
            current: d.species,
            level: d.level,
            gender: d.gender,
            shiny: d.shiny,
            fainted: false,
            position: None,
            seen: false,
            max_hp: 0,
            terastallized: false,
            gender_known: !matches!(origin, Origin::Named | Origin::NamedPoke(_)),
            placeholder: matches!(origin, Origin::Named | Origin::NamedPoke(_)),
            status: Status::None,
            id,
        });
        Some(ids.mons.len() - 1)
    }

    /// A Pokémon named before its details showed (see
    /// [`Known::placeholder`]) switched in with details `d`: with team
    /// preview, it takes the `|poke|` line they match.
    fn adopt(&mut self, side: SideId, k: usize, d: &Details) {
        let dex = self.dex;
        let ids = &mut self.sides[side.index()];
        ids.mons[k].placeholder = false;
        if ids.pokes.is_empty() {
            return;
        }
        let old = ids.mons[k].id;
        ids.used &= !(1 << old);
        let id = ids.poke_for(dex, d).unwrap_or(old);
        ids.used |= 1 << id;
        ids.mons[k].id = id;
    }

    /// A switch-in's Pokémon (`|switch|`, `|drag|`, `|replace|`): known by
    /// name and details, or new (`None` when the table is full).
    fn switch_in(&mut self, side: SideId, slot: u8, name: &str, d: &Details) -> Option<usize> {
        // The one leaving the slot shows its own species again (a Transform
        // ends), unless an Illusion shows it in the other slot too.
        let ids = &mut self.sides[side.index()];
        if let Some(Some(old)) = ids.slots.get(slot as usize).copied()
            && !ids.slots.iter().enumerate().any(|(s, o)| s != slot as usize && *o == Some(old))
        {
            let o = &mut ids.mons[old as usize];
            o.current = o.species;
        }
        let ids = &self.sides[side.index()];
        // The Pokémon of that name and species (a permanent forme change
        // shows as `detailschange`, which updates the species): two of
        // one name and base species are two (Vivillon-Sun and
        // Vivillon-Fancy, both "Vivillon"). Or the species one on the
        // field shows transformed (an Illusion of it shows that). Or, in a
        // forme reached in battle that it kept off the field (Morpeko-Hangry,
        // which an Illusion of it shows too), the one of that base species.
        // Two of one name and species differ by gender (as details show it)
        // or level.
        let dex = self.dex;
        let kept_forme = dex.species(d.species).battle_only;
        let k = ids
            .mons
            .iter()
            .enumerate()
            .filter_map(|(k, m)| {
                let how = if m.species == d.species {
                    0
                } else if m.current == d.species && ids.active(k) {
                    1
                } else if kept_forme && dex.species(m.species).base_species == dex.species(d.species).base_species {
                    2
                } else {
                    return None;
                };
                // Details show the gender (an Illusion's, its disguise's):
                // one of another is another Pokémon.
                let gender = m.gender == d.gender || !m.gender_known;
                (m.name == name && gender).then_some((how, m.level != d.level, k))
            })
            .min()
            .map(|(.., k)| k);
        // The player's own Pokémon are known from its request, which may
        // already show a forme it changed into after the line
        // (Terapagos-Terastal): its base species then. (Before a request,
        // as when reading ahead, two of one name and base species are two:
        // Vivillon-Sun and Vivillon-Modern.)
        // Of two (Raichu, which became Raichu-Mega-Y, and Raichu-Alola; an
        // Ogerpon that became Ogerpon-Teal-Tera, and Ogerpon-Wellspring),
        // the one in a forme reached in battle, whose `|poke|` line shows
        // these details, then of this gender and level.
        let base = |s: SpeciesId| self.dex.species(s).base_species;
        let own = || {
            if self.me != Some(side) || self.requests == 0 {
                return None;
            }
            let dex = self.dex;
            let previewed =
                |m: &Known| ids.pokes.get(m.id as usize).is_some_and(|p| fits(dex, p, d, 1) || p.species == d.species);
            ids.mons
                .iter()
                .enumerate()
                .filter(|(_, m)| m.name == name && base(m.species) == base(d.species))
                .min_by_key(|(k, m)| {
                    let reached = m.species == d.species || dex.species(m.species).battle_only;
                    (!reached, !previewed(m), m.gender != d.gender, m.level != d.level, *k)
                })
                .map(|(k, _)| k)
        };
        // A Terastallized Pokémon (one per side) is the one that
        // Terastallized, whatever forme it shows (a Morpeko keeps Hangry).
        let tera = || {
            d.tera.and_then(|_| {
                ids.mons.iter().position(|m| m.name == name && m.terastallized && base(m.species) == base(d.species))
            })
        };
        // One only named so far (by an `[of]`) is the one switching in.
        let named = || ids.mons.iter().position(|m| m.placeholder && m.name == name);
        let k = match k.or_else(tera).or_else(own).or_else(named) {
            Some(k) => k,
            None => self.new_mon(side, name, d, Origin::Shown)?,
        };
        if self.sides[side.index()].mons[k].placeholder {
            self.adopt(side, k, d);
        }
        let ids = &mut self.sides[side.index()];
        let m = &mut ids.mons[k];
        m.gender = d.gender;
        m.gender_known = true;
        // An Illusion of a transformed Pokémon shows its current species:
        // its own stays.
        if m.current != d.species {
            m.species = d.species;
        }
        m.current = d.species;
        m.shiny |= d.shiny;
        m.fainted = false;
        m.seen = true;
        if let Some(s) = ids.slots.get_mut(slot as usize) {
            *s = Some(k as u8);
        }
        Some(k)
    }

    fn actor_at(&self, side: SideId, slot: u8, k: usize) -> Actor {
        let m = &self.sides[side.index()].mons[k];
        Actor { pos: Position::new(side, slot), species: m.current, active: true, id: m.id }
    }

    fn set_fainted(&mut self, actor: Actor, fainted: bool) {
        let ids = &mut self.sides[actor.pos.side.index()];
        if let Some(k) = ids.by_id(actor.id) {
            ids.mons[k].fainted = fainted;
        }
    }

    /// The species shown in an actor's slot changed (a forme).
    fn set_current(&mut self, actor: Actor, species: SpeciesId, permanent: bool) {
        let ids = &mut self.sides[actor.pos.side.index()];
        if let Some(k) = ids.by_id(actor.id) {
            ids.mons[k].current = species;
            if permanent {
                ids.mons[k].species = species;
            }
        }
    }

    // -----------------------------------------------------------------
    // Requests.

    /// Reads a request (the JSON of a `|request|` line) with this battle's
    /// ids: a Pokémon keeps the id it was first given. The first request
    /// gives the player's Pokémon their registration ids (the engine's
    /// [`PokemonState::id`](pikapro::PokemonState::id)): with team
    /// preview, the `|poke|` line each matches, if the parser read them;
    /// else their team positions, which are registration order at the
    /// battle's first request. It also says whose channel this is if that
    /// wasn't known. A fainted Pokémon's max HP (`0 fnt`) is the one
    /// earlier requests showed.
    ///
    /// A channel read from the middle of a battle (a reconnect: Showdown
    /// sends the room's log so far, then the current request) must go
    /// through [`ChannelParser::first_request`], or its ids follow the
    /// current team order.
    pub fn request(&mut self, json: &str, format: &Format) -> Result<ParsedRequest, ParseError> {
        let first = self.requests == 0;
        let dex = self.dex;
        // Each entry's name (raw JSON text, borrowed), by position.
        let mut raw = [""; MAX_TEAM];
        let mut result = parse_request_with(json, format, |m| {
            if let Some(n) = raw.get_mut(m.position) {
                *n = m.raw_name;
            }
            m.position as u8
        })?;
        let side = result.data.side;
        if self.me.is_none() {
            self.me = Some(side);
        }
        let entries = Entries::of(&raw, &result.data);
        let first_ids = if first {
            self.first_ids.take().unwrap_or_else(|| self.registration(side, &entries).0)
        } else {
            [0; MAX_TEAM]
        };
        let mut taken = 0u32;
        for (i, &first_id) in first_ids.iter().enumerate().take(entries.n) {
            let name: &str = &entries.names[i];
            let d = entries.details[i];
            let Some(p) = result.data.pokemon.get_mut(i) else { continue };
            let ids = &self.sides[side.index()];
            // A known Pokémon not already matched (two of the same name
            // and species keep their order).
            let found = ids
                .mons
                .iter()
                .enumerate()
                .filter(|(_, m)| taken & (1 << m.id) == 0 && m.name == name)
                .min_by_key(|(_, m)| {
                    let base = |s: SpeciesId| dex.species(s).base_species;
                    (
                        if m.species == d.species {
                            0
                        } else if base(m.species) == base(d.species) {
                            1
                        } else {
                            2
                        },
                        // Two of one name and species: their gender and level.
                        m.gender != d.gender,
                        m.level != d.level,
                        m.id,
                    )
                })
                .map(|(k, _)| k);
            let k = match found {
                Some(k) => Some(k),
                None => {
                    let ids = &mut self.sides[side.index()];
                    // First request: registration ids.
                    let id = first_id;
                    if first && ids.used & (1 << id) == 0 && ids.mons.len() < MAX_KNOWN {
                        ids.used |= 1 << id;
                        ids.mons.push(Known {
                            name: name.to_string(),
                            shiny: false,
                            fainted: false,
                            position: None,
                            seen: false,
                            max_hp: 0,
                            terastallized: false,
                            gender_known: true,
                            placeholder: false,
                            status: Status::None,
                            species: d.species,
                            current: d.species,
                            level: d.level,
                            gender: d.gender,
                            id,
                        });
                        Some(ids.mons.len() - 1)
                    } else {
                        self.new_mon(side, name, &d, Origin::Request)
                    }
                }
            };
            let Some(k) = k else {
                // The table is full: an id no other Pokémon has, for this
                // request only.
                let used = self.sides[side.index()].used | taken;
                let id = (0..MAX_IDS).find(|&x| used & (1 << x) == 0).unwrap_or(MAX_IDS - 1);
                p.id = id;
                taken |= 1 << id;
                continue;
            };
            let m = &mut self.sides[side.index()].mons[k];
            m.species = d.species;
            m.level = d.level;
            m.gender = d.gender;
            m.position = Some(i as u8);
            m.status = p.status;
            p.id = m.id;
            taken |= 1 << m.id;
            // A fainted Pokémon's max HP: the one its forme last showed
            // (a forme can revert as it faints), else its last.
            let (id, species) = (m.id, p.species);
            if p.hp == 0 && p.max_hp == 0 {
                let same = self.max_hp.iter().rev().find(|e| e.0 == id && e.1 == species);
                let any = self.max_hp.iter().rev().find(|e| e.0 == id);
                p.max_hp = match (same, any) {
                    (Some(e), _) => e.2,
                    // A forme it reverted to as it fainted (Terapagos): its
                    // max HP follows from another forme's and the level, when
                    // only one spread gives both.
                    (None, Some(&(_, other, max))) => {
                        other_forme_hp(format, other, max, species, p.level).unwrap_or(max)
                    }
                    (None, None) => 0,
                };
            } else {
                self.note_max_hp(id, species, p.max_hp);
            }
        }
        self.requests = self.requests.saturating_add(1);
        self.unknown_names = self.unknown_names.saturating_add(result.unknown_names);
        Ok(result)
    }

    /// [`ChannelParser::request`] for the first request of a channel whose
    /// earlier lines, `before`, weren't fed yet: feed them next. This is
    /// how a player joins a battle that already started (a reconnect):
    /// Showdown sends the room's log so far (each `|split|` block as the
    /// player's line alone), then the current request, whose team order
    /// switches have changed and which, after team preview, lists only the
    /// Pokémon brought. The request is read first because it names the
    /// player's Pokémon, which the lines must be read with (a lead may be
    /// an Illusion only the request sees through).
    ///
    /// The ids are those of a parser that read the battle from its start.
    /// The lines are read ahead (to no sink) for the Pokémon's registration
    /// order: with team preview, the `|poke|` line each matches; without,
    /// the request's order with the switches the lines show undone. And
    /// for a fainted Pokémon's max HP, which the request no longer shows
    /// (the player's own HP lines are exact).
    ///
    /// An own Illusion user that switched in disguised and left without
    /// the log revealing it was its disguise as far as the names show; the
    /// replay tells the two apart by Illusion's rule, the exact HP the
    /// player's own lines show (a Pokémon comes back with the HP it left
    /// with) and `tera:`. Where even those fit two orders (the disguise at
    /// the same HP and max HP), no client can tell:
    /// [`ChannelParser::ambiguous_start`] says so.
    pub fn first_request<S: AsRef<str>>(
        &mut self,
        before: &[S],
        json: &str,
        format: &Format,
    ) -> Result<ParsedRequest, ParseError> {
        if self.requests > 0 {
            return self.request(json, format);
        }
        let mut raw = [""; MAX_TEAM];
        let peek = parse_request_with(json, format, |m| {
            if let Some(n) = raw.get_mut(m.position) {
                *n = m.raw_name;
            }
            0
        })?;
        let side = peek.data.side;
        let entries = Entries::of(&raw, &peek.data);
        let mut ahead = self.clone();
        ahead.me = Some(self.me.unwrap_or(side));
        ahead.moved = Some(Vec::new());
        for line in before {
            ahead.feed(line.as_ref(), &mut |_| {});
        }
        let (ids, replay) = ahead.registration(side, &entries);
        let known = ahead.known_of(side, &entries);
        for (i, k) in known.iter().enumerate().take(entries.n) {
            // The max HP its HP lines last showed, per forme (the replay
            // gives a disguise's lines to the Illusion user behind it), else
            // the last its name showed.
            match replay.as_ref().map(|r| r.hp[i]) {
                Some(hp) => {
                    for &(species, max) in hp.iter().rev().filter(|e| e.1 != 0) {
                        self.note_max_hp(ids[i], species, max);
                    }
                }
                None => {
                    if let Some(m) = k.map(|k| &ahead.sides[side.index()].mons[k])
                        && m.max_hp != 0
                    {
                        self.note_max_hp(ids[i], m.species, m.max_hp);
                    }
                }
            }
        }
        self.first_ids = Some(ids);
        self.ambiguous_start = replay.is_some_and(|r| r.ambiguous);
        self.request(json, format)
    }

    /// Keeps own Pokémon `id`'s max HP as `species`.
    fn note_max_hp(&mut self, id: u8, species: SpeciesId, max: u16) {
        if let Some(e) = self.max_hp.iter_mut().find(|e| e.0 == id && e.1 == species) {
            e.2 = max;
            return;
        }
        if self.max_hp.len() >= MAX_HP_ENTRIES {
            self.max_hp.remove(0);
        }
        self.max_hp.push((id, species, max));
    }

    /// The registration ids of a first request's Pokémon, by team
    /// position: with team preview (the parser read the side's `|poke|`
    /// lines) the line each matches; else their positions, with the
    /// switches read ahead undone ([`ChannelParser::first_request`]). And
    /// that replay of the switches, if the parser read ahead.
    fn registration(&self, side: SideId, e: &Entries<'_>) -> ([u8; MAX_TEAM], Option<Replay>) {
        let dex = self.dex;
        let mut out: [u8; MAX_TEAM] = core::array::from_fn(|i| i as u8);
        let replay = self.replay(side, e);
        let ids = &self.sides[side.index()];
        if !ids.pokes.is_empty() {
            let mut got = [None; MAX_TEAM];
            let mut used = 0u32;
            for strict in 0..FIT_LEVELS {
                for (g, d) in got.iter_mut().zip(&e.details).take(e.n) {
                    if g.is_some() {
                        continue;
                    }
                    let j = (0..ids.pokes.len()).find(|&j| used & (1 << j) == 0 && fits(dex, &ids.pokes[j], d, strict));
                    if let Some(j) = j {
                        *g = Some(j as u8);
                        used |= 1 << j;
                    }
                }
            }
            for (i, (o, g)) in out.iter_mut().zip(got).enumerate().take(e.n) {
                *o = g.unwrap_or_else(|| {
                    let id = (0..MAX_IDS).find(|&x| used & (1 << x) == 0).unwrap_or(i as u8);
                    used |= 1 << id;
                    id
                });
            }
        } else if let Some(r) = &replay {
            for (position, &i) in r.order.iter().enumerate().take(e.n) {
                out[i] = position as u8;
            }
        }
        (out, replay)
    }

    /// Each request entry's Pokémon in this parser's table, if any: by
    /// name, then species (the same before the same base species before
    /// another, over all entries: a nickname can be another's species
    /// name), gender and level.
    fn known_of(&self, side: SideId, e: &Entries<'_>) -> [Option<usize>; MAX_TEAM] {
        let dex = self.dex;
        let base = |s: SpeciesId| dex.species(s).base_species;
        let mut out = [None; MAX_TEAM];
        let mut taken = 0u32;
        // Over all entries, one of its gender and level first: an entry
        // before it mustn't take the one the lines showed of another gender
        // (two Greninja, a male never seen). Then any (a
        // Pokémon named before its switch-in has no gender yet).
        for exact in [true, false] {
            for most in 0..3 {
                for (i, (o, d)) in out.iter_mut().zip(&e.details).enumerate().take(e.n) {
                    if o.is_some() {
                        continue;
                    }
                    let rank = |m: &Known| {
                        if m.species == d.species || m.current == d.species {
                            0
                        } else if base(m.species) == base(d.species) {
                            1
                        } else {
                            2
                        }
                    };
                    let fits = |m: &Known| !exact || m.gender == d.gender && m.level == d.level;
                    *o = self.sides[side.index()]
                        .mons
                        .iter()
                        .enumerate()
                        .filter(|(k, m)| taken & (1 << k) == 0 && m.name == *e.names[i] && rank(m) <= most && fits(m))
                        .min_by_key(|(k, m)| (rank(m), m.gender != d.gender, m.level != d.level, *k))
                        .map(|(k, _)| k);
                    if let Some(k) = *o {
                        taken |= 1 << k;
                    }
                }
            }
        }
        out
    }

    /// The own switches read ahead, undone from the request's order, last
    /// first (Showdown's `switchIn` and `swapPosition` swap two team
    /// positions): the team order at the battle's start (after team
    /// preview), and the max HP each Pokémon's HP lines last showed, per
    /// species (a fainted one's request shows none). `None` if the lines
    /// don't say (a Pokémon they name isn't in the request, or too many
    /// lines).
    ///
    /// A switch-in shows an Illusion user as its disguise (the last
    /// Pokémon not fainted behind it in the team order, Illusion's
    /// `onBeforeSwitchIn`), with its own max HP (the player's own HP lines
    /// are exact). So the one a switch replaced may be an Illusion user the
    /// lines named as another, unless a `|replace|` named it: the undoing
    /// tries it too, and keeps the first order under which every switch-in
    /// shows what its line showed: the Pokémon Illusion's rule gives, its
    /// max HP, the HP it had when it last left the field (Regenerator's
    /// silent heal aside), and `tera:` once it Terastallized. The HP lines
    /// are the true occupant's.
    fn replay(&self, side: SideId, e: &Entries<'_>) -> Option<Replay> {
        let moved = self.moved.as_ref()?;
        let known = self.known_of(side, e);
        let mut entry_of = [None; MAX_KNOWN];
        for (i, k) in known.iter().enumerate().take(e.n) {
            if let Some(k) = *k {
                entry_of[k] = Some(i);
            }
        }
        let entry = |k: u8| entry_of.get(k as usize).copied().flatten();
        // Per event: the switch-in it happened under in its slot (for a
        // switch-in, the one it replaced), and who that one was (a
        // `|replace|` names an Illusion user).
        let mut under = Vec::with_capacity(moved.len());
        let mut occupant = Vec::with_capacity(moved.len());
        let mut confirmed = vec![false; moved.len()];
        let mut last_in = [None::<usize>; MAX_ACTIVE];
        for (ev, m) in moved.iter().enumerate() {
            let (at, who) = match *m {
                Moved::In { slot, k, .. } => (last_in[slot as usize].replace(ev), k),
                Moved::Replace { slot, k } => {
                    if let Some(at) = last_in[slot as usize] {
                        occupant[at] = k;
                        confirmed[at] = true;
                    }
                    (None, k)
                }
                Moved::Hp { slot, k, .. } => (last_in[slot as usize], k),
                Moved::Swap(a, b) => {
                    last_in.swap(a as usize, b as usize);
                    (None, 0)
                }
                Moved::Faint { .. } | Moved::Revive { .. } | Moved::Tera { .. } => (None, 0),
            };
            under.push(at);
            occupant.push(who);
        }
        let n = e.n;
        let illusion = e.illusion;
        // Who a switch-in into `slot` shows, with `order` after it.
        // A Terastallized one (`terastallized`) doesn't take an Ogerpon's or
        // a Terapagos' look.
        let dex = self.dex;
        let shows = |order: &[usize; MAX_TEAM], fainted: u8, slot: usize, terastallized: bool| {
            let x = order[slot];
            if !illusion[x] {
                return x;
            }
            let Some(y) = (slot + 1..n).rev().map(|i| order[i]).find(|&y| fainted & (1 << y) == 0) else { return x };
            let base = dex.species(e.details[y].species).base_species;
            if terastallized && matches!(base, species::OGERPON | species::TERAPAGOS) { x } else { y }
        };
        // Whether a switch-in's max HP is `x`'s: a disguise shows the HP of
        // the one behind it, and a Pokémon its own unless it changed forme
        // since (a request shows the current forme's).
        let hp_fits = |max_hp: &[u16; MAX_TEAM], x: usize, disguised: bool, species: SpeciesId, shown: u16| {
            let same_forme = disguised || e.details[x].species == species;
            shown == 0 || max_hp[x] == 0 || !same_forme || max_hp[x] == shown
        };
        // The one that Terastallized (a switch-in's details show `tera:`
        // from then on, a disguise's too): as its request shows it, else
        // the one in the slot the line named (a fainted one's request may
        // not show it).
        let tera = (0..n).find(|&i| e.terastallized[i]);
        let tera_shown = tera.is_some() || moved.iter().any(|m| matches!(m, Moved::Tera { .. }));
        // Depth-first, from the request's order back. `level` 2 checks each
        // switch-in by Illusion's rule, its max HP and Terastallization, 1
        // by the rule alone, 0 takes the Pokémon the lines named, unchecked
        // (the rule may not have held: Neutralizing Gas). A second order
        // that fits as well makes the replay ambiguous.
        let undo = |level: u8| -> Option<Replay> {
            let check = level > 0;
            let start = Path {
                ev: moved.len(),
                r: Replay {
                    order: core::array::from_fn(|i| i),
                    hp: [[(SpeciesId::default(), 0); 4]; MAX_TEAM],
                    ambiguous: false,
                },
                fainted: (0..n).filter(|&i| e.fainted[i]).fold(0u8, |f, i| f | 1 << i),
                max_hp: e.max_hp,
                tera,
                tera_on: tera_shown,
                need: core::array::from_fn(|i| if e.fainted[i] || e.regenerator[i] { NO_NEED } else { e.hp[i] }),
            };
            let mut stack = vec![start];
            let mut found: Option<Replay> = None;
            let mut budget = 1u32 << 17;
            'paths: while let Some(mut p) = stack.pop() {
                while p.ev > 0 {
                    p.ev -= 1;
                    let Some(b) = budget.checked_sub(1) else { break 'paths };
                    budget = b;
                    match moved[p.ev] {
                        Moved::Swap(a, b) => {
                            if a as usize >= n || b as usize >= n {
                                return None;
                            }
                            p.r.order.swap(a as usize, b as usize);
                        }
                        Moved::Faint { slot } => {
                            if (slot as usize) < n {
                                p.fainted &= !(1 << p.r.order[slot as usize]);
                            }
                        }
                        Moved::Revive { k } => {
                            let x = entry(k)?;
                            p.fainted |= 1 << x;
                            p.need[x] = NO_NEED;
                        }
                        Moved::Tera { slot } => {
                            let &x = p.r.order.get(slot as usize).filter(|_| (slot as usize) < n)?;
                            if level > 1 && *p.tera.get_or_insert(x) != x {
                                continue 'paths;
                            }
                            p.tera_on = false;
                        }
                        Moved::Replace { .. } => {}
                        Moved::Hp { slot, k, species, hp, max } => {
                            if slot as usize >= n {
                                continue;
                            }
                            let x = p.r.order[slot as usize];
                            // Its last line before it left showed the HP it
                            // came back with (or has now).
                            if level > 1 && p.need[x] != NO_NEED && p.need[x] != hp {
                                continue 'paths;
                            }
                            p.need[x] = NO_NEED;
                            // A disguise's line: the one behind it, in its
                            // own forme.
                            let species = if entry(k) == Some(x) { species } else { e.details[x].species };
                            let seen = &mut p.r.hp[x];
                            if !seen.iter().any(|&(s, m)| m != 0 && s == species)
                                && let Some(free) = seen.iter_mut().find(|(_, m)| *m == 0)
                            {
                                *free = (species, max);
                            }
                        }
                        Moved::In { slot, k, hp: came_with, max_hp: shown_hp, species, tera: shown_tera, gas } => {
                            let s = slot as usize;
                            if s >= n {
                                return None;
                            }
                            let x = p.r.order[s];
                            let disguised = entry(k)? != x;
                            // On the bench before, with the HP it came in
                            // with (Regenerator's heal as it left shows no
                            // line).
                            p.need[x] = if e.regenerator[x] { NO_NEED } else { came_with };
                            // Under a Neutralizing Gas, Illusion's rule doesn't
                            // run (`onBeforeSwitchIn`): an Illusion user may
                            // show any disguise it had.
                            let shown = entry(k)?;
                            let rule = shows(&p.r.order, p.fainted, s, shown_tera);
                            if check && rule != shown && !(gas && illusion[x]) {
                                continue 'paths;
                            }
                            if level > 1 {
                                if !hp_fits(&p.max_hp, x, disguised, species, shown_hp) {
                                    continue 'paths;
                                }
                                // Details show `tera:` exactly when it did.
                                let tera_fits = match (shown_tera, p.tera_on) {
                                    (true, true) => *p.tera.get_or_insert(x) == x,
                                    (true, false) => false,
                                    (false, on) => !on || p.tera != Some(x),
                                };
                                if !tera_fits {
                                    continue 'paths;
                                }
                                // A fainted one's: the same at each switch-in
                                // in the forme its request shows.
                                if p.max_hp[x] == 0 && (disguised || e.details[x].species == species) {
                                    p.max_hp[x] = shown_hp;
                                }
                            }
                            // A lead came into an empty slot.
                            let Some(at) = under[p.ev] else { continue };
                            let named = entry(occupant[at])?;
                            let illusions = (0..n).filter(|&y| check && y != named && illusion[y] && !confirmed[at]);
                            // Who it replaced came in showing this max HP.
                            let then = match moved[at] {
                                Moved::In { max_hp, species, .. } => Some((max_hp, species)),
                                _ => None,
                            };
                            let max_hp = p.max_hp;
                            let fits = |y: usize| {
                                level < 2 || then.is_none_or(|(hp, sp)| hp_fits(&max_hp, y, y != named, sp, hp))
                            };
                            let mut before = StackVec::<[usize; MAX_TEAM], MAX_TEAM>::new();
                            for y in core::iter::once(named).chain(illusions).filter(|&y| fits(y)) {
                                // (The same one: revived, it came back into
                                // its own slot, Revival Blessing's switch.)
                                if let Some(at) = p.r.order[..n].iter().position(|&z| z == y)
                                    && !before.is_full()
                                {
                                    let mut o = p.r.order;
                                    o.swap(s, at);
                                    before.push(o);
                                }
                            }
                            // The one the line named next; the others wait.
                            let Some(&next) = before.first() else { continue 'paths };
                            for &o in before.iter().skip(1).rev() {
                                let mut other = p;
                                other.r.order = o;
                                stack.push(other);
                            }
                            p.r.order = next;
                        }
                    }
                }
                match &mut found {
                    None => found = Some(p.r),
                    Some(first) if first.order[..n] != p.r.order[..n] => {
                        first.ambiguous = true;
                        break;
                    }
                    Some(_) => {}
                }
            }
            found
        };
        undo(2).or_else(|| undo(1)).or_else(|| undo(0))
    }

    /// Records a change of who is in the player's own slots (while reading
    /// ahead for [`ChannelParser::first_request`]).
    fn note_moved(&mut self, side: SideId, m: Moved) {
        if self.me != Some(side) {
            return;
        }
        if let Some(v) = &mut self.moved {
            if v.len() >= MAX_HISTORY {
                self.moved = None;
            } else {
                v.push(m);
            }
        }
    }

    // -----------------------------------------------------------------
    // Values.

    /// `hp/max[hint] [status]` or `0 fnt`: (hp, max, status). A percentage
    /// with a Champions colour hint is scaled to `/1000` so that
    /// `hp_shown` gives it back. HP past the max (no server sends it) is
    /// read as the max.
    fn health(text: &str) -> (u16, u16, Status) {
        let (hp, status) = text.split_once(' ').map_or((text, ""), |(h, s)| (h, s));
        let status = parse_status(status);
        let Some((a, b)) = hp.split_once('/') else { return (0, 0, status) };
        let a: u16 = a.parse().unwrap_or(0);
        let (b, hint) = match b.as_bytes().last() {
            Some(c @ (b'g' | b'y' | b'r')) => (&b[..b.len() - 1], Some(*c)),
            _ => (b, None),
        };
        let b: u16 = b.parse().unwrap_or(0);
        if a == 0 && b == 0 {
            // `0 fnt`.
            return (0, 0, status);
        }
        match hint {
            // Champions: floor(100 * hp / max) with a hint at 20% and 50%.
            Some(b'y') if a == 20 => (201, 1000, status),
            Some(b'r') if a == 20 => (200, 1000, status),
            Some(b'g') if a == 50 => (501, 1000, status),
            Some(b'y') if a == 50 => (500, 1000, status),
            Some(_) => (a.min(100) * 10, 1000, status),
            // `hp/0`: no max shown ([`ChannelParser::hp_of`] takes the last).
            None if b == 0 => (a, 0, status),
            None => (a.min(b), b, status),
        }
    }

    /// A name the dex doesn't know (a newer server's): counted, the line
    /// otherwise ignored.
    fn unknown_name(&mut self) -> LineKind {
        self.unknown_names = self.unknown_names.saturating_add(1);
        LineKind::Ignored
    }

    /// A move by name; one the dex doesn't know is counted and read as
    /// Struggle (which reveals nothing of a Pokémon's moves).
    fn mv_any(&mut self, name: &str) -> MoveId {
        self.mv(name).unwrap_or_else(|| {
            self.unknown_names = self.unknown_names.saturating_add(1);
            moves::STRUGGLE
        })
    }

    /// A species by name, or its stand-in ([`species_or_standin`]).
    fn species_any(&mut self, name: &str) -> SpeciesId {
        species_or_standin(self.dex, name, &mut self.unknown_names)
    }

    /// Details, or their stand-in ([`details_or_standin`]).
    fn details_any(&mut self, text: &str) -> Details {
        details_or_standin(self.dex, text, &mut self.unknown_names)
    }

    /// [`ChannelParser::health`] for `actor`: `0 fnt` shows no max HP, so
    /// it keeps the last one shown (exact HP channels), or 100.
    /// An Illusion can show one Pokémon in both slots: an active one's is
    /// its slot's.
    ///
    /// Off the field, a name can be shared (a nickname that is another
    /// species' name): on a line with exact HP (the player's own, or any in
    /// a channel that shows exact HP), the max HP tells which one it is.
    fn hp_of(&mut self, actor: &mut Actor, text: &str) -> (u16, u16, Status) {
        let (hp, max, status) = Self::health(text);
        let exact = self.exact_hp || self.me == Some(actor.pos.side);
        let ids = &mut self.sides[actor.pos.side.index()];
        if !actor.active
            && exact
            && max != 0
            && let Some(k) = ids.by_id(actor.id)
            && ids.mons[k].max_hp != 0
            && ids.mons[k].max_hp != max
            && let Some(other) = ids.mons.iter().find(|m| m.name == ids.mons[k].name && m.max_hp == max)
        {
            actor.id = other.id;
            actor.species = other.current;
        }
        let slot = (actor.active && (actor.pos.slot as usize) < MAX_ACTIVE).then_some(actor.pos.slot as usize);
        let k = ids.by_id(actor.id);
        if max != 0 {
            let species = k.map(|k| {
                ids.mons[k].max_hp = max;
                ids.mons[k].species
            });
            if let Some(s) = slot {
                ids.slot_max[s] = max;
            }
            if let (Some(s), Some(k), Some(species)) = (slot, k, species) {
                self.note_moved(actor.pos.side, Moved::Hp { slot: s as u8, k: k as u8, species, hp, max });
            }
            return (hp, max, status);
        }
        let last = match (slot, k) {
            (Some(s), _) => ids.slot_max[s],
            (None, Some(k)) => ids.mons[k].max_hp,
            (None, None) => 0,
        };
        let max = if last == 0 { 100 } else { last };
        (hp.min(max), max, status)
    }

    fn mv(&self, name: &str) -> Option<MoveId> {
        self.dex.move_by_name(name)
    }

    fn item(&self, name: &str) -> Option<ItemId> {
        self.dex.item_by_name(name)
    }

    fn ability(&self, name: &str) -> Option<AbilityId> {
        self.dex.ability_by_name(name)
    }

    fn species(&self, name: &str) -> Option<SpeciesId> {
        self.dex.species_by_name(name)
    }

    fn boost(name: &str) -> Option<BoostId> {
        BoostId::ALL.into_iter().find(|b| b.id() == name)
    }

    fn effect(&self, text: &str) -> Option<EffectName> {
        effect_name_of(self.dex, text)
    }

    /// `[from] <effect>` with its `[of]`.
    fn cause(&mut self, p: &Parts<'_>) -> Option<Cause> {
        let effect = self.effect(p.tag("from")?)?;
        let of = p.tag("of").and_then(|o| self.ident(o));
        Some(Cause { effect, of })
    }

    fn of(&mut self, p: &Parts<'_>) -> Option<Actor> {
        p.tag("of").and_then(|o| self.ident(o))
    }

    /// An argument of a generic line, its kind guessed as `prefer` says,
    /// then an ident, a move, an item, an ability, a species, a type or a
    /// text.
    fn arg(&mut self, text: &str, prefer: ArgKind) -> LineArg {
        let dex = self.dex;
        let exact_move = |t: &str| dex.move_by_name(t).filter(|&m| dex.move_(m).name == t);
        let exact_item = |t: &str| dex.item_by_name(t).filter(|&i| dex.item(i).name == t);
        let exact_ability = |t: &str| dex.ability_by_name(t).filter(|&a| dex.ability(a).name == t);
        match prefer {
            ArgKind::Move => {
                if let Some(m) = exact_move(text) {
                    return LineArg::Move(m);
                }
            }
            ArgKind::Item => {
                if let Some(i) = exact_item(text) {
                    return LineArg::Item(i);
                }
            }
            ArgKind::Any => {}
        }
        if Self::side_of(text).is_some()
            && text.contains(": ")
            && let Some(a) = self.ident(text)
        {
            return LineArg::Actor(a);
        }
        if let Some(t) = static_text(text) {
            return LineArg::Text(t);
        }
        if let Some(m) = exact_move(text) {
            return LineArg::Move(m);
        }
        if let Some(i) = exact_item(text) {
            return LineArg::Item(i);
        }
        if let Some(a) = exact_ability(text) {
            return LineArg::Ability(a);
        }
        if let Some(s) = dex.species_by_name(text).filter(|&s| dex.species(s).name == text) {
            return LineArg::Species(s);
        }
        if let Some(t) = Type::from_name(text).filter(|t| t.name() == text) {
            return LineArg::Type(t);
        }
        LineArg::Text("")
    }

    // -----------------------------------------------------------------
    // Lines.

    fn dispatch(&mut self, p: &Parts<'_>, line: &str, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        let cmd = p.get(0);
        match cmd {
            "" => {
                if p.n == 1 {
                    sink(BattleEvent::Blank);
                    return LineKind::Battle;
                }
                LineKind::Ignored
            }
            "t:" => {
                sink(BattleEvent::Timestamp);
                LineKind::Battle
            }
            "split" => {
                match Self::side_of(p.get(1)) {
                    Some(side) => self.split = Some((side, None)),
                    None => return LineKind::Unknown,
                }
                LineKind::Ignored
            }
            "request" => {
                if request_body(line).is_some() {
                    LineKind::Request
                } else {
                    LineKind::Ignored
                }
            }
            "error" => LineKind::Error { unavailable: p.get(1).starts_with("[Unavailable choice]") },
            "player" => {
                let Some(side) = Self::side_of(p.get(1)) else { return LineKind::Unknown };
                if p.n > 2 && !p.get(2).is_empty() {
                    self.players[side.index()] = p.get(2).to_string();
                }
                LineKind::Player(side)
            }
            "showteam" => match Self::side_of(p.get(1)) {
                Some(side) => LineKind::ShowTeam(side),
                None => LineKind::Unknown,
            },
            // Room, chat and header lines.
            "j" | "J" | "join" | "l" | "L" | "leave" | "n" | "N" | "name" | "c" | "c:" | "chat" | "raw" | "html"
            | "uhtml" | "uhtmlchange" | "inactive" | "inactiveoff" | "title" | "gametype" | "gen" | "tier"
            | "rated" | "rule" | "badge" | "clearpoke" | "debug" | "seed" | "init" | "deinit" | "noinit" | "users"
            | ":" | "expire" | "b" | "B" | "notify" | "tempnotify" | "tempnotifyoff" | "hidelines" | "unlink"
            | "bigerror" | "message" | "-message" | "timer" | "note" | "askreg" | "sentchoice" | "controlshtml"
            | "fieldhtml" | "selectorhtml" | "dice" | "zoom" | "notice" | "warning" | "rng" => {
                if self.message_line(p, sink) { LineKind::Battle } else { LineKind::Ignored }
            }
            _ => self.battle_line(p, sink),
        }
    }

    /// `-message`, `message` and `bigerror` lines the engine writes.
    fn message_line(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> bool {
        let text = p.get(1);
        let message = match p.get(0) {
            "-message" if text == "Sleep Clause Mod activated." => Message::SleepClauseMod,
            // The Endless Battle Clause's.
            "-message"
                if text
                    == "Each side's team started with the rudimentary means to perform restorative berry-cycling." =>
            {
                Message::BerryCyclingBoth
            }
            "-message"
                if text.ends_with(
                    "'s team started with the rudimentary means to perform restorative berry-cycling and thus loses.",
                ) =>
            {
                let name = text.strip_suffix(
                    "'s team started with the rudimentary means to perform restorative berry-cycling and thus loses.",
                );
                let loser = SideId::BOTH.into_iter().find(|s| name == Some(self.players[s.index()].as_str()));
                let Some(loser) = loser else { return false };
                Message::BerryCycling { loser }
            }
            "-message" => {
                // `(N of <name>'s pokemon was/were cured by Natural Cure.)`
                let Some(rest) = text.strip_prefix('(').and_then(|t| t.strip_suffix(" cured by Natural Cure.)")) else {
                    return false;
                };
                let rest = rest.strip_suffix(" was").or_else(|| rest.strip_suffix(" were")).unwrap_or(rest);
                let Some((n, rest)) = rest.split_once(" of ") else { return false };
                let Some(name) = rest.strip_suffix("'s pokemon") else { return false };
                let Some(side) = SideId::BOTH.into_iter().find(|s| self.players[s.index()] == name) else {
                    return false;
                };
                Message::NaturalCure { side, cured: n.parse().unwrap_or(1) }
            }
            "message" if text.ends_with(". You have hit the turn limit!") => {
                let limit = text.strip_prefix("It is turn ").and_then(|t| t.split('.').next()?.parse().ok());
                let Some(limit) = limit else { return false };
                Message::TurnLimit { limit }
            }
            "bigerror" => {
                let Some(rest) = text.strip_prefix("You will auto-tie if the battle doesn't end in ") else {
                    return false;
                };
                let Some(n) = rest.split(' ').next().and_then(|n| n.parse().ok()) else { return false };
                let limit = rest.split("(on turn ").nth(1).and_then(|t| t.strip_suffix(").")?.parse().ok());
                let Some(limit) = limit else { return false };
                Message::AutoTieWarning { turns_left: n, limit }
            }
            _ => return false,
        };
        sink(BattleEvent::Message { message });
        true
    }

    fn battle_line(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let dex = self.dex;
        let a1 = p.get(1);
        match p.get(0) {
            // Setup and turns.
            "poke" => {
                let Some(side) = Self::side_of(a1) else { return LineKind::Unknown };
                let d = self.details_any(p.get(2));
                let ids = &mut self.sides[side.index()];
                if !ids.pokes.is_full() {
                    ids.pokes.push(Poke { species: d.species, wildcard: d.wildcard, level: d.level, gender: d.gender });
                }
                sink(E::Poke { side, species: d.species, level: d.level, gender: d.gender });
                return LineKind::Poke;
            }
            "teampreview" => {
                // `|teampreview` alone without a picked team size.
                sink(E::TeamPreview { bring: a1.parse().ok() });
                return LineKind::TeamPreview;
            }
            "start" => {
                sink(E::Start);
                return LineKind::Start;
            }
            "turn" => {
                let turn = a1.parse().unwrap_or(0);
                sink(E::Turn { turn });
                return LineKind::Turn(turn);
            }
            "upkeep" => sink(E::Upkeep),
            "win" => {
                // The name as `|player|` gave it (Showdown compares user
                // ids), or without `|player|` lines the sides' default
                // names. Another name is no winner this parser knows: the
                // end is shown, its winner unknown (and the line counted).
                let side = SideId::BOTH
                    .into_iter()
                    .find(|s| !self.players[s.index()].is_empty() && pikapro_data::id_eq(&self.players[s.index()], a1))
                    .or(match a1 {
                        "p1" => Some(SideId::P1),
                        "p2" => Some(SideId::P2),
                        _ => None,
                    });
                let Some(side) = side else {
                    self.unknown = self.unknown.saturating_add(1);
                    return LineKind::EndUnknown;
                };
                sink(E::Win { side });
                return LineKind::End(Some(side));
            }
            "tie" => {
                sink(E::Tie);
                return LineKind::End(None);
            }
            "teamsize" => {
                let Some(side) = Self::side_of(a1) else { return LineKind::Unknown };
                sink(E::TeamSize { side, size: p.get(2).parse().unwrap_or(0) });
            }

            // Switches and identity.
            "switch" | "drag" | "replace" => {
                let Some((pos, name)) = a1.split_once(": ") else { return LineKind::Unknown };
                let (Some(side), Some(slot)) = (Self::side_of(pos), Self::slot(pos)) else { return LineKind::Unknown };
                let d = self.details_any(p.get(2));
                // An Illusion's HP lines showed the Pokémon behind it: its max HP
                // is the one `|replace|` reveals.
                let before = self.sides[side.index()].slots.get(slot.slot as usize).copied().flatten();
                let Some(k) = self.switch_in(side, slot.slot, name, &d) else { return LineKind::Unknown };
                if (slot.slot as usize) < MAX_ACTIVE {
                    let (s, k) = (slot.slot, k as u8);
                    self.entered_at[side.index()][s as usize] = self.moved.as_ref().map_or(0, Vec::len);
                    let (hp, max_hp, _) = Self::health(p.get(3));
                    let m = match p.get(0) {
                        "replace" => Moved::Replace { slot: s, k },
                        _ => Moved::In {
                            slot: s,
                            k,
                            species: d.species,
                            hp,
                            max_hp,
                            tera: d.tera.is_some(),
                            gas: self.gas,
                        },
                    };
                    self.note_moved(side, m);
                }
                if p.get(0) == "replace"
                    && let Some(b) = before
                {
                    let ids = &mut self.sides[side.index()];
                    if ids.mons[k].max_hp == 0 {
                        ids.mons[k].max_hp = ids.mons[b as usize].max_hp;
                    }
                }
                let mut actor = self.actor_at(side, slot.slot, k);
                let (level, gender) = (d.level, d.gender);
                match p.get(0) {
                    "replace" => sink(E::Replace { actor, species: d.species, level, gender }),
                    kind => {
                        let (hp, max_hp, status) = self.hp_of(&mut actor, p.get(3));
                        let tera = d.tera;
                        if kind == "switch" {
                            let from = p.tag("from").and_then(|m| self.mv(m));
                            sink(E::Switch {
                                actor,
                                team_index: 0,
                                level,
                                hp,
                                max_hp,
                                status,
                                tera,
                                from,
                                shown: None,
                                gender,
                            });
                        } else {
                            sink(E::Drag {
                                actor,
                                team_index: 0,
                                level,
                                hp,
                                max_hp,
                                status,
                                tera,
                                shown: None,
                                gender,
                            });
                        }
                    }
                }
            }
            "detailschange" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let d = self.details_any(p.get(2));
                self.set_current(actor, d.species, true);
                sink(E::DetailsChange {
                    actor,
                    species: d.species,
                    level: d.level,
                    tera: d.tera,
                    silent: p.has("[silent]"),
                    gender: d.gender,
                });
            }
            "-formechange" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let species = self.species_any(p.get(2));
                self.set_current(actor, species, false);
                let cause = self.cause(p);
                if p.has("[silent]")
                    && let Some(cause) = cause
                {
                    sink(E::FormeChangeSilent { actor, species, cause });
                } else {
                    sink(E::FormeChange { actor, species, msg: p.has("[msg]"), cause });
                }
            }
            "-mega" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let species = self.species_any(p.get(2));
                let item = self.item(p.get(3));
                sink(E::MegaEvolve { actor, species, item });
            }
            "-terastallize" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let Some(ty) = type_by_name(p.get(2)) else { return LineKind::Unknown };
                let ids = &mut self.sides[actor.pos.side.index()];
                if let Some(k) = ids.by_id(actor.id) {
                    ids.mons[k].terastallized = true;
                }
                if actor.active && (actor.pos.slot as usize) < MAX_ACTIVE {
                    self.note_moved(actor.pos.side, Moved::Tera { slot: actor.pos.slot });
                }
                sink(E::Terastallize { actor, ty });
            }
            "-transform" => {
                let (Some(actor), Some(target)) = (self.ident(a1), self.ident(p.get(2))) else {
                    return LineKind::Unknown;
                };
                // It shows the target's species until it leaves the field.
                self.set_current(actor, target.species, false);
                match self.cause(p) {
                    Some(cause) => sink(E::TransformFrom { actor, target, cause }),
                    None => sink(E::Transform { actor, target }),
                }
            }
            "swap" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let slot: u8 = p.get(2).parse().unwrap_or(0);
                let move_id = p.tag("from").and_then(|f| f.strip_prefix("move: ")).and_then(|m| self.mv(m));
                sink(E::Swap { actor, slot, move_id: move_id.unwrap_or(moves::ALLY_SWITCH) });
                let ids = &mut self.sides[actor.pos.side.index()];
                let (a, b) = (actor.pos.slot as usize, slot as usize);
                if a < MAX_ACTIVE && b < MAX_ACTIVE {
                    ids.slots.swap(a, b);
                    ids.slot_max.swap(a, b);
                    self.note_moved(actor.pos.side, Moved::Swap(a as u8, b as u8));
                }
            }
            "faint" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                self.set_fainted(actor, true);
                if actor.active && (actor.pos.slot as usize) < MAX_ACTIVE {
                    self.note_moved(actor.pos.side, Moved::Faint { slot: actor.pos.slot });
                }
                sink(E::Faint { actor });
            }

            // Moves.
            "move" => {
                let Some(user) = self.ident(a1) else { return LineKind::Unknown };
                let move_id = self.mv_any(p.get(2));
                let from = match p.tag("from") {
                    Some("lockedmove") => Some(MoveFrom::LockedMove),
                    Some(f) => match self.effect(f) {
                        Some(EffectName::Move(m)) => Some(MoveFrom::Move(m)),
                        Some(EffectName::Ability(a)) => Some(MoveFrom::Ability(a)),
                        _ => None,
                    },
                    None => None,
                };
                let target = p.get(3);
                if target == "null" {
                    sink(E::MoveNull { user, move_id, from });
                } else {
                    let target = if is_tag(target) || target.is_empty() { None } else { self.ident(target) };
                    sink(E::Move { user, move_id, target, from });
                }
                self.move_attrs(p, sink);
            }
            "-anim" => {
                let move_id = self.mv_any(p.get(2));
                let (Some(user), Some(target)) = (self.ident(a1), self.ident(p.get(3))) else {
                    return LineKind::Unknown;
                };
                sink(E::Anim { user, move_id, target });
                self.move_attrs(p, sink);
            }
            "-prepare" => {
                let move_id = self.mv_any(p.get(2));
                let Some(user) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::Prepare { user, move_id });
            }
            "-mustrecharge" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::MustRecharge { actor });
            }
            "cant" => return self.cant(p, sink),
            "-singleturn" => return self.singleturn(p, sink),
            "-singlemove" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let effect = match p.get(2) {
                    "Destiny Bond" => VolatileEffect::DestinyBond,
                    "Glaive Rush" => VolatileEffect::GlaiveRush,
                    _ => return LineKind::Unknown,
                };
                let from = p.tag("from").and_then(|f| f.strip_prefix("move: ")).and_then(|m| self.mv(m));
                let of = self.of(p);
                sink(E::EffectStart { actor, effect, of, from });
            }

            // HP and status.
            "-damage" => return self.damage(p, sink),
            "-heal" => return self.heal(p, sink),
            "-sethp" => {
                let Some(mut target) = self.ident(a1) else { return LineKind::Unknown };
                let (hp, max_hp, status) = self.hp_of(&mut target, p.get(2));
                sink(E::SetHp { target, hp, max_hp, status, silent: p.has("[silent]") });
            }
            "-status" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                let status = parse_status(p.get(2));
                match p.tag("from") {
                    Some(f) if f.starts_with("move: ") => {
                        sink(E::StatusStart { target, status, from_move: self.mv(&f[6..]) })
                    }
                    Some(_) => match self.cause(p) {
                        Some(cause) => sink(E::StatusFrom { target, status, cause }),
                        None => sink(E::StatusStart { target, status, from_move: None }),
                    },
                    None => sink(E::StatusStart { target, status, from_move: None }),
                }
            }
            "-curestatus" => {
                let Some(mut target) = self.ident(a1) else { return LineKind::Unknown };
                let status = parse_status(p.get(2));
                // Off the field (Heal Bell), two of the player's own of one
                // name: the one its request showed with that status.
                if !target.active && self.me == Some(target.pos.side) {
                    let ids = &self.sides[target.pos.side.index()];
                    let kind = core::mem::discriminant(&status);
                    if let Some(k) = ids.by_id(target.id)
                        && core::mem::discriminant(&ids.mons[k].status) != kind
                        && let Some(m) = ids
                            .mons
                            .iter()
                            .find(|m| m.name == ids.mons[k].name && core::mem::discriminant(&m.status) == kind)
                    {
                        (target.id, target.species) = (m.id, m.current);
                    }
                }
                match p.tag("from") {
                    Some(f) if f.starts_with("move: ") => {
                        sink(E::CureStatus { target, status, from_move: self.mv(&f[6..]) })
                    }
                    Some(_) => match self.cause(p) {
                        Some(cause) => sink(E::CureStatusFrom { target, status, cause, silent: p.has("[silent]") }),
                        None => sink(E::CureStatus { target, status, from_move: None }),
                    },
                    None => sink(E::CureStatus { target, status, from_move: None }),
                }
            }

            // Stat stages.
            "-boost" | "-unboost" => {
                let unboost = p.get(0) == "-unboost";
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                let Some(stat) = Self::boost(p.get(2)) else { return LineKind::Unknown };
                // At most 12 stages can change (-6 to +6).
                let amount = p.get(3).parse::<u8>().unwrap_or(0).min(12);
                match self.cause(p) {
                    Some(cause) => sink(E::BoostFrom { target, stat, amount, unboost, cause }),
                    None if unboost => sink(E::Unboost { target, stat, amount }),
                    None => sink(E::Boost { target, stat, amount }),
                }
            }
            "-setboost" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                let Some(stat) = Self::boost(p.get(2)) else { return LineKind::Unknown };
                let stage = p.get(3).parse().unwrap_or(0);
                match p.tag("from") {
                    Some(f) if f.starts_with("move: ") => {
                        let move_id = self.mv(&f[6..]).unwrap_or(moves::BELLY_DRUM);
                        sink(E::SetBoost { target, stat, stage, move_id });
                    }
                    _ => match self.cause(p) {
                        Some(cause) => sink(E::SetBoostFrom { target, stat, stage, cause }),
                        None => sink(E::SetBoost { target, stat, stage, move_id: moves::BELLY_DRUM }),
                    },
                }
            }
            "-clearboost" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                match self.cause(p) {
                    Some(cause) => sink(E::ClearBoostFrom { target, cause }),
                    None => sink(E::ClearBoost { target }),
                }
            }
            "-clearallboost" => sink(E::ClearAllBoost),
            "-clearnegativeboost" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::ClearNegativeBoost { target, silent: p.has("[silent]") });
            }
            "-clearpositiveboost" => {
                let (Some(target), Some(source)) = (self.ident(a1), self.ident(p.get(2))) else {
                    return LineKind::Unknown;
                };
                let Some(move_id) = p.get(3).strip_prefix("move: ").and_then(|m| self.mv(m)) else {
                    return LineKind::Unknown;
                };
                sink(E::ClearPositiveBoost { target, source, move_id });
            }
            "-invertboost" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::InvertBoost { target });
            }
            "-copyboost" => {
                let (Some(source), Some(target)) = (self.ident(a1), self.ident(p.get(2))) else {
                    return LineKind::Unknown;
                };
                match p.tag("from") {
                    Some("move: Psych Up") | None => sink(E::CopyBoost { source, target }),
                    Some(_) => match self.cause(p) {
                        Some(cause) => sink(E::CopyBoostFrom { source, target, cause }),
                        None => sink(E::CopyBoost { source, target }),
                    },
                }
            }
            "-swapboost" => {
                let (Some(source), Some(target)) = (self.ident(a1), self.ident(p.get(2))) else {
                    return LineKind::Unknown;
                };
                let move_id = p
                    .tag("from")
                    .and_then(|f| f.strip_prefix("move: "))
                    .and_then(|m| self.mv(m))
                    .unwrap_or(moves::HEART_SWAP);
                sink(E::SwapBoost { source, target, move_id });
            }

            // Hits.
            "-crit" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::Crit { target });
            }
            "-supereffective" | "-resisted" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                let level = p.get(2).parse().unwrap_or(0);
                if p.get(0) == "-supereffective" {
                    sink(E::SuperEffective { target, level });
                } else {
                    sink(E::Resisted { target, level });
                }
            }
            "-immune" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                if p.has("[ohko]") {
                    sink(E::OhkoImmune { target });
                } else if let Some(cause) = self.cause(p) {
                    let detail = p.positional(2).next().map(|d| self.arg(d, ArgKind::Any));
                    sink(E::ImmuneFrom { target, detail, cause });
                } else {
                    sink(E::Immune { target });
                }
            }
            "-hitcount" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::HitCount { target, hits: p.get(2).parse().unwrap_or(0) });
            }
            "-ohko" => sink(E::Ohko),
            "-miss" => {
                let (Some(user), Some(target)) = (self.ident(a1), self.ident(p.get(2))) else {
                    return LineKind::Unknown;
                };
                sink(E::Miss { user, target });
            }
            "-fail" => return self.fail(p, sink),
            "-block" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let Some(effect) = self.effect(p.get(2)) else { return LineKind::Unknown };
                let arg = p.positional(3).next().map(|a| self.arg(a, ArgKind::Any));
                let of = self.of(p);
                sink(E::Block { actor, effect, arg, of });
            }

            // Volatiles.
            "-activate" => return self.activate(p, sink),
            "-start" => return self.start(p, sink),
            "-end" => return self.end(p, sink),
            "-endability" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::AbilityEnd { actor });
            }

            // Abilities and items.
            "-ability" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let name = p.get(2);
                if p.has("[fail]")
                    && let (Some(ability), Some(weather)) = (self.ability(name), p.tag("from").and_then(weather_by_id))
                {
                    sink(E::WeatherFail { source: actor, effect: EffectName::Ability(ability), weather });
                    return LineKind::Battle;
                }
                let Some(ability) = self.ability(name).filter(|&a| dex.ability(a).name == name) else {
                    if let Some(name) = static_text(name) {
                        sink(E::AbilityNamed { actor, name });
                        return LineKind::Battle;
                    }
                    return self.unknown_name();
                };
                let note = match p.positional(3).next() {
                    Some("boost") => AbilityNote::Boost,
                    Some(old) => self.ability(old).map_or(AbilityNote::None, AbilityNote::Replaced),
                    None => AbilityNote::None,
                };
                let cause = self.cause(p);
                if ability == abilities::NEUTRALIZING_GAS {
                    self.gas = true;
                    // It was on the field, suppressing Illusion (Showdown's
                    // `ignoringAbility` looks at every active Pokémon), for
                    // the switch-ins since it came in: at the start, the
                    // leads after it.
                    let (si, slot) = (actor.pos.side.index(), actor.pos.slot as usize);
                    let from = self.entered_at[si].get(slot).copied().unwrap_or(usize::MAX);
                    if let Some(v) = &mut self.moved {
                        for m in v.iter_mut().skip(from) {
                            if let Moved::In { gas, .. } = m {
                                *gas = true;
                            }
                        }
                    }
                }
                sink(E::AbilityShow { actor, ability, note, cause });
            }
            "-item" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                let Some(item) = self.item(p.get(2)) else { return self.unknown_name() };
                let cause = self.cause(p);
                sink(E::ItemShow { actor, item, cause });
            }
            "-enditem" => {
                let Some(actor) = self.ident(a1) else { return LineKind::Unknown };
                // Fling after its item was used up (a Blunder Policy that
                // missed) names no item.
                if p.get(2).is_empty() && p.tag("from") == Some("move: Fling") {
                    sink(E::FlingNoItem { actor });
                    return LineKind::Battle;
                }
                let Some(item) = self.item(p.get(2)) else { return self.unknown_name() };
                let mut note = ItemEndNote::None;
                let mut from = None;
                for k in 3..p.n {
                    let a = p.v[k];
                    match a {
                        "[eat]" => note = ItemEndNote::Eat,
                        "[weaken]" => note = ItemEndNote::Weaken,
                        "[silent]" => note = ItemEndNote::Silent,
                        // Master names the move (`[move] <move>`); 0.11.11
                        // didn't.
                        "[from] gem" => {
                            note = match p.tag("move").and_then(|m| self.mv(m)) {
                                Some(m) => ItemEndNote::GemMove(m),
                                None => ItemEndNote::Gem,
                            };
                        }
                        "[from] stealeat" => {
                            let m = p.tag("move").and_then(|m| self.mv(m)).unwrap_or(moves::BUG_BITE);
                            note = ItemEndNote::StealEat(m);
                        }
                        _ => {
                            if let Some(f) = a.strip_prefix("[from] ") {
                                from = self.effect(f);
                            }
                        }
                    }
                }
                let of = self.of(p);
                sink(E::ItemEnd { actor, item, note, from, of });
            }

            // Field and sides.
            "-weather" => {
                if a1 == "none" {
                    sink(E::Weather { weather: None, upkeep: false });
                    return LineKind::Battle;
                }
                let Some(weather) = weather_by_name(a1) else { return LineKind::Unknown };
                match self.cause(p) {
                    Some(cause) => sink(E::WeatherFrom { weather, cause }),
                    None => sink(E::Weather { weather: Some(weather), upkeep: p.has("[upkeep]") }),
                }
            }
            "-fieldstart" => {
                let Some(effect) = a1.strip_prefix("move: ").and_then(field_by_name) else { return LineKind::Unknown };
                if p.tag("from").is_some()
                    && let Some(cause) = self.cause(p)
                {
                    sink(E::FieldStartFrom { effect, cause });
                } else {
                    let of = self.of(p);
                    sink(E::FieldStart { effect, of });
                }
            }
            "-fieldend" => {
                let name = a1.strip_prefix("move: ").unwrap_or(a1);
                let Some(effect) = field_by_name(name) else { return LineKind::Unknown };
                if effect == FieldEffect::MagicRoom
                    && let Some(of) = self.of(p)
                {
                    sink(E::MagicRoomEnd { of });
                } else {
                    sink(E::FieldEnd { effect });
                }
            }
            "-fieldactivate" => {
                if let Some(name) = a1.strip_prefix("move: ") {
                    match effect_by_name(dex, name) {
                        Some(effect) => sink(E::FieldActivate { effect }),
                        None => match self.mv(name) {
                            Some(move_id) => sink(E::MoveFieldActivate { move_id }),
                            None => return LineKind::Unknown,
                        },
                    }
                } else {
                    let Some(weather) = weather_activate_by_name(a1) else { return LineKind::Unknown };
                    sink(E::WeatherActivate { weather });
                }
            }
            "-sidestart" | "-sideend" => {
                let Some(side) = Self::side_of(a1) else { return LineKind::Unknown };
                let name = p.get(2);
                if p.get(0) == "-sidestart" {
                    let Some(condition) = side_condition_by_name(name) else { return LineKind::Unknown };
                    sink(E::SideStart { side, condition });
                } else if name == "move: Toxic Spikes"
                    && let Some(of) = self.of(p)
                {
                    sink(E::ToxicSpikesAbsorbed { side, of });
                } else if let Some(condition) = side_condition_by_name(name).filter(|_| p.tag("from").is_none()) {
                    sink(E::SideEnd { side, condition });
                } else if let Some(condition) = side_condition_by_plain_name(name) {
                    // A hazard a move removed (`[from] move: Rapid Spin`):
                    // Spikes is written the same way by both lines, so the
                    // cause tells them apart.
                    let from = p.tag("from").and_then(|f| f.strip_prefix("move: ")).and_then(|m| self.mv(m));
                    let of = self.of(p);
                    sink(E::HazardEnd { side, condition, from, of });
                } else {
                    return LineKind::Unknown;
                }
            }
            "-swapsideconditions" => sink(E::SwapSideConditions),
            "-waiting" => {
                let (Some(user), Some(ally)) = (self.ident(a1), self.ident(p.get(2))) else {
                    return LineKind::Unknown;
                };
                sink(E::PledgeWaiting { user, ally });
            }
            "-combine" => sink(E::PledgeCombine),
            "-nothing" => sink(E::Nothing),
            "-zbroken" => {
                let Some(target) = self.ident(a1) else { return LineKind::Unknown };
                sink(E::ZBroken { target });
            }
            "-hint" => match parse_hint(dex, a1) {
                Some(hint) => sink(E::Hint { hint }),
                None => return LineKind::Ignored,
            },
            // Lines of other generations or mechanics: shown, nothing a
            // view keeps.
            "-center" | "-notarget" | "-primal" | "-burst" | "-zpower" | "-candynamax" => return LineKind::Ignored,
            _ => return LineKind::Unknown,
        }
        LineKind::Battle
    }

    /// A `|move|` or `|-anim|` line's annotations (`attrLastMove`).
    fn move_attrs(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) {
        for k in 3..p.n {
            let a = p.v[k];
            let attr = match a {
                "[still]" => MoveAttr::Still,
                "[miss]" => MoveAttr::Miss,
                "[notarget]" => MoveAttr::NoTarget,
                _ => {
                    if let Some(list) = a.strip_prefix("[spread] ") {
                        let mut slots = SpreadSlots::default();
                        for s in list.split(',') {
                            if let Some(pos) = Self::slot(s)
                                && (slots.len as usize) < slots.slots.len()
                            {
                                slots.slots[slots.len as usize] = pos;
                                slots.len += 1;
                            }
                        }
                        MoveAttr::Spread(slots)
                    } else if let Some(anim) = a.strip_prefix("[anim] ") {
                        let anim = if anim == "Fickle Beam All Out" {
                            MoveAnim::FickleBeamAllOut
                        } else if let Some(t) = anim.strip_prefix("Ivy Cudgel ").and_then(Type::from_name) {
                            MoveAnim::IvyCudgel(t)
                        } else if let Some(c) = anim.strip_prefix("Shell Side Arm ").and_then(category_by_name) {
                            MoveAnim::ShellSideArm(c)
                        } else if let Some(t) = anim.strip_prefix("Tera Blast ").and_then(Type::from_name) {
                            MoveAnim::TeraBlast(t)
                        } else {
                            continue;
                        };
                        MoveAttr::Anim(anim)
                    } else {
                        continue;
                    }
                }
            };
            sink(BattleEvent::MoveAttr { attr });
        }
    }

    fn cant(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let Some(actor) = self.ident(p.get(1)) else { return LineKind::Unknown };
        let reason = p.get(2);
        let move_arg = p.positional(3).next();
        let mv = |s: &Self| move_arg.and_then(|m| s.mv(m));
        match reason {
            "par" | "slp" | "frz" => sink(E::StatusCant { actor, status: parse_status(reason) }),
            "flinch" => sink(E::Cant { actor, reason: CantReason::Flinch }),
            "recharge" => sink(E::Cant { actor, reason: CantReason::Recharge }),
            "move: Taunt" if mv(self).is_some() => {
                sink(E::Cant { actor, reason: CantReason::Taunt(mv(self).unwrap_or_default()) })
            }
            "Disable" if mv(self).is_some() => {
                sink(E::Cant { actor, reason: CantReason::Disable(mv(self).unwrap_or_default()) })
            }
            "Focus Punch" => sink(E::LostFocus { actor }),
            // `recharge` is no dex move: `None`.
            "nopp" => sink(E::CantNoPp { actor, move_id: mv(self) }),
            "move: Gravity" => sink(E::GravityCant { actor, move_id: mv(self).unwrap_or_default() }),
            "Attract" | "move: Throat Chop" | "move: Heal Block" | "move: Imprison" => {
                let effect = match reason {
                    "Attract" => VolatileEffect::Attract,
                    "move: Throat Chop" => VolatileEffect::ThroatChop,
                    "move: Heal Block" => VolatileEffect::HealBlock,
                    _ => VolatileEffect::Imprison,
                };
                sink(E::EffectCant { actor, effect, move_id: mv(self) });
            }
            _ => {
                if let Some(effect) = self.effect(reason) {
                    let of = self.of(p);
                    sink(E::CantFrom { actor, effect, move_id: mv(self), of });
                } else if let Some(effect) = effect_by_name(self.dex, reason) {
                    sink(E::EffectCant { actor, effect, move_id: mv(self) });
                } else {
                    return LineKind::Unknown;
                }
            }
        }
        LineKind::Battle
    }

    fn singleturn(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let Some(actor) = self.ident(p.get(1)) else { return LineKind::Unknown };
        let what = p.get(2);
        match what {
            "Protect" => sink(E::ProtectStart { actor, move_id: moves::PROTECT }),
            "move: Protect" => sink(E::ProtectStart { actor, move_id: moves::SPIKY_SHIELD }),
            "move: Endure" => sink(E::EffectStart { actor, effect: VolatileEffect::Endure, of: None, from: None }),
            "Helping Hand" => {
                let of = self.of(p);
                sink(E::MoveSingleTurn { target: actor, move_id: moves::HELPING_HAND, of });
            }
            _ => {
                if let Some(condition) = side_condition_by_plain_name(what)
                    .filter(|c| matches!(c, SideCondition::WideGuard | SideCondition::QuickGuard))
                {
                    sink(E::GuardStart { actor, condition });
                    return LineKind::Battle;
                }
                let Some(move_id) = what.strip_prefix("move: ").and_then(|m| self.mv(m)) else {
                    return LineKind::Unknown;
                };
                let of = self.of(p);
                match move_id {
                    moves::FOCUS_PUNCH | moves::BEAK_BLAST | moves::SHELL_TRAP if of.is_none() => {
                        sink(E::ChargeStart { actor, move_id })
                    }
                    moves::ROOST | moves::ELECTRIFY if of.is_none() => sink(E::SingleTurn { actor, move_id }),
                    _ => sink(E::MoveSingleTurn { target: actor, move_id, of }),
                }
            }
        }
        LineKind::Battle
    }

    fn damage(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let dex = self.dex;
        let Some(mut target) = self.ident(p.get(1)) else { return LineKind::Unknown };
        let (hp, max_hp, status) = self.hp_of(&mut target, p.get(2));
        let Some(from) = p.tag("from") else {
            sink(E::Damage { target, hp, max_hp, status, source: DamageSource::Move });
            return LineKind::Battle;
        };
        let source = match from {
            "psn" if matches!(status, Status::Toxic { .. }) => DamageSource::Status(Status::Toxic { counter: 0 }),
            "psn" | "brn" | "tox" | "frz" | "par" | "slp" => DamageSource::Status(parse_status(from)),
            "recoil" => DamageSource::Recoil,
            "Recoil" => DamageSource::MoveRecoil,
            "confusion" => DamageSource::Confusion,
            "Spiky Shield" => match self.of(p) {
                Some(of) => DamageSource::MoveEffect { move_id: moves::SPIKY_SHIELD, of },
                None => DamageSource::Move,
            },
            _ => {
                if let Some(species) = from.strip_prefix("pokemon: ").and_then(|s| self.species(s)) {
                    DamageSource::Species(species)
                } else if let Some(m) = from.strip_prefix("move: ").filter(|_| p.has("[partiallytrapped]")) {
                    let effect = VolatileEffect::PartiallyTrapped(self.mv(m).unwrap_or_default());
                    let of = self.of(p);
                    sink(E::EffectDamage { target, hp, max_hp, status, effect, of });
                    return LineKind::Battle;
                } else if let Some(cause) = self.effect(from).map(|effect| (effect, p.tag("of"))) {
                    let of = cause.1.and_then(|o| self.ident(o));
                    DamageSource::Effect(Cause { effect: cause.0, of })
                } else if let Some(weather) = weather_by_name(from) {
                    DamageSource::Weather(weather)
                } else if let Some(effect) = effect_by_name(dex, from) {
                    let of = self.of(p);
                    sink(E::EffectDamage { target, hp, max_hp, status, effect, of });
                    return LineKind::Battle;
                } else if let Some(condition) = side_condition_by_plain_name(from) {
                    sink(E::ConditionDamage { target, hp, max_hp, status, condition });
                    return LineKind::Battle;
                } else if let Some(m) =
                    self.mv(from).filter(|_| from.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
                {
                    DamageSource::MoveCondition(m)
                } else {
                    DamageSource::Move
                }
            }
        };
        sink(E::Damage { target, hp, max_hp, status, source });
        LineKind::Battle
    }

    fn heal(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let Some(mut target) = self.ident(p.get(1)) else { return LineKind::Unknown };
        // Revival Blessing names a fainted Pokémon by name only: the player
        // knows which of its own it chose.
        if p.tag("from") == Some("move: Revival Blessing")
            && let Some(id) = self.revival[target.pos.side.index()].take()
        {
            let ids = &self.sides[target.pos.side.index()];
            if let Some(k) =
                ids.by_id(id).filter(|&k| Some(ids.mons[k].name.as_str()) == p.get(1).split_once(": ").map(|x| x.1))
            {
                target.id = id;
                target.species = ids.mons[k].current;
            }
        } else if p.tag("from") == Some("move: Revival Blessing") && !target.active {
            // A foe's: of two Pokémon of one name, the fainted one.
            let ids = &self.sides[target.pos.side.index()];
            let name = p.get(1).split_once(": ").map_or("", |x| x.1);
            if ids.by_id(target.id).is_some_and(|k| !ids.mons[k].fainted)
                && let Some(k) = ids.mons.iter().position(|m| m.name == name && m.fainted)
            {
                let slot = ids.slots.iter().position(|s| *s == Some(k as u8)).map(|s| s as u8);
                target.id = ids.mons[k].id;
                target.species = ids.mons[k].current;
                target.pos = Position::new(target.pos.side, slot.or(ids.mons[k].position).unwrap_or(u8::MAX));
            }
        }
        let (hp, max_hp, status) = self.hp_of(&mut target, p.get(2));
        let silent = p.has("[silent]");
        let Some(from) = p.tag("from") else {
            if silent {
                sink(E::HealSilent { target, hp, max_hp, status });
            } else {
                sink(E::Heal { target, hp, max_hp, status, source: HealSource::Move });
            }
            return LineKind::Battle;
        };
        match from {
            "drain" => {
                let Some(of) = self.of(p) else { return LineKind::Unknown };
                sink(E::Heal { target, hp, max_hp, status, source: HealSource::Drain { of } });
            }
            "move: Wish" if p.tag("wisher").is_some() => {
                let name = p.tag("wisher").unwrap_or("");
                // The wisher is on the target's side.
                let dex = self.dex;
                let ids = &self.sides[target.pos.side.index()];
                let (wisher, wisher_id) = match ids.find(dex, name, None, None) {
                    Some(k) => (ids.mons[k].species, ids.mons[k].id),
                    None => (self.species(name).unwrap_or_default(), 0),
                };
                sink(E::WishHeal { target, hp, max_hp, status, wisher, wisher_id });
            }
            "Aqua Ring" => {
                sink(E::ManipulationHeal { target, hp, max_hp, status, from: Some(ManipulationVolatile::AquaRing) })
            }
            "Ingrain" => {
                sink(E::ManipulationHeal { target, hp, max_hp, status, from: Some(ManipulationVolatile::Ingrain) })
            }
            _ => {
                if let Some(t) = terrain_by_name(from) {
                    sink(E::Heal { target, hp, max_hp, status, source: HealSource::Terrain(t) });
                } else if let Some(m) = from.strip_prefix("move: ").and_then(|m| self.mv(m)) {
                    if m == moves::REVIVAL_BLESSING {
                        self.set_fainted(target, false);
                        if let Some(k) = self.sides[target.pos.side.index()].by_id(target.id) {
                            self.note_moved(target.pos.side, Moved::Revive { k: k as u8 });
                        }
                    }
                    sink(E::MoveHeal { target, hp, max_hp, status, move_id: m });
                } else if let Some(cause) = self.cause(p) {
                    if silent {
                        sink(E::HealFromSilent { target, hp, max_hp, status, cause });
                    } else {
                        sink(E::Heal { target, hp, max_hp, status, source: HealSource::Effect(cause) });
                    }
                } else if let Some(v) = manipulation_by_name(from) {
                    sink(E::ManipulationHeal { target, hp, max_hp, status, from: Some(v) });
                } else {
                    sink(E::Heal { target, hp, max_hp, status, source: HealSource::Move });
                }
            }
        }
        LineKind::Battle
    }

    fn fail(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let Some(target) = self.ident(p.get(1)) else { return LineKind::Unknown };
        let mut pos = p.positional(2);
        let what = pos.next();
        let detail = pos.next();
        match (what, p.tag("from")) {
            (None, None) => sink(E::Fail { user: target }),
            (Some("heal"), None) => sink(E::HealFail { target }),
            (Some("slp"), Some("Uproar")) => sink(E::UproarPreventsSleep { target, own: p.has("[msg]") }),
            (Some(w), None) if w.starts_with("move: ") => {
                let Some(move_id) = self.mv(&w[6..]) else { return LineKind::Unknown };
                let note = if p.has("[forme]") {
                    FailNote::Forme
                } else if p.has("[weak]") {
                    FailNote::Weak
                } else {
                    FailNote::None
                };
                sink(E::MoveFail { user: target, move_id, note });
            }
            (Some(w), None) => {
                let status = parse_status(w);
                if status == Status::None {
                    return LineKind::Unknown;
                }
                sink(E::StatusFail { target, status });
            }
            (Some(w), Some(f)) if weather_by_id(f).is_some() && detail.is_none() => {
                let weather = weather_by_id(f).unwrap_or(Weather::Sun);
                let effect = match (self.mv(w).filter(|&m| self.dex.move_(m).name == w), self.item(w)) {
                    (Some(m), _) => EffectName::Move(m),
                    (None, Some(i)) => EffectName::Item(i),
                    _ => return LineKind::Unknown,
                };
                sink(E::WeatherFail { source: target, effect, weather });
            }
            // `primordialsea` / `desolateland`'s `onTryMove`.
            (Some(w), Some(f @ ("Primordial Sea" | "Desolate Land"))) if detail.is_none() => {
                let Some(move_id) = self.mv(w).filter(|&m| self.dex.move_(m).name == w) else {
                    return LineKind::Unknown;
                };
                let weather = if f == "Primordial Sea" { Weather::HeavyRain } else { Weather::HarshSun };
                sink(E::WeatherMoveFail { user: target, move_id, weather });
            }
            (None, Some(_)) => match self.cause(p) {
                Some(cause) => sink(E::FailBy { target, cause }),
                None => sink(E::Fail { user: target }),
            },
            (Some(w), Some(_)) => {
                let what = self.arg(w, ArgKind::Any);
                let detail = detail.map(|d| self.arg(d, ArgKind::Any));
                match self.cause(p) {
                    Some(cause) => sink(E::FailFrom { target, what, detail, cause }),
                    None => sink(E::Fail { user: target }),
                }
            }
        }
        LineKind::Battle
    }

    fn activate(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let dex = self.dex;
        let Some(actor) = self.ident(p.get(1)) else { return LineKind::Unknown };
        let what = p.get(2);
        // `-activate|<source>|Skill Swap|<theirs>|<ours>|[of] <target>`.
        if what == "Skill Swap" {
            let (a, b) = (p.get(3), p.get(4));
            let abilities = match (self.ability(a), self.ability(b)) {
                (Some(x), Some(y)) if !a.is_empty() => Some((x, y)),
                _ => None,
            };
            let Some(target) = self.of(p) else { return LineKind::Unknown };
            sink(E::SkillSwap { source: actor, target, abilities });
            return LineKind::Battle;
        }
        // `-activate|<holder>|ability: X|<target>|[ability] <old>`.
        if let Some(old) = p.tag("ability")
            && let Some(ability) = what.strip_prefix("ability: ").and_then(|a| self.ability(a))
            && let (Some(target), Some(old)) = (self.ident(p.get(3)), self.ability(old))
        {
            sink(E::AbilityReplaced { holder: actor, ability, target, old });
            return LineKind::Battle;
        }
        if p.has("[source]")
            && let Some(ability) = self.ability(what)
        {
            sink(E::ActivateSource { actor, ability });
            return LineKind::Battle;
        }
        if let Some(v) = volatile_by_name(what) {
            sink(E::VolatileActivate { actor, volatile: v });
            return LineKind::Battle;
        }
        if what == "trapped" {
            let of = self.of(p);
            let from = p.tag("from").and_then(|f| f.strip_prefix("move: ")).and_then(|m| self.mv(m));
            sink(E::EffectStart { actor, effect: VolatileEffect::Trapped, of, from });
            return LineKind::Battle;
        }
        if let Some(name) = what.strip_prefix("move: ") {
            match name {
                "Struggle" => {
                    sink(E::StruggleStart { actor });
                    return LineKind::Battle;
                }
                "Protect" => {
                    sink(E::Protected { target: actor });
                    return LineKind::Battle;
                }
                "Feint" => {
                    sink(E::ProtectionBroken { target: actor, move_id: moves::FEINT });
                    return LineKind::Battle;
                }
                "Eerie Spell" => {
                    let move_id = self.mv(p.get(3)).unwrap_or_default();
                    sink(E::PpDrain { target: actor, move_id, amount: p.get(4).parse().unwrap_or(0) });
                    return LineKind::Battle;
                }
                _ => {}
            }
            if p.has("[broken]") {
                let Some(move_id) = self.mv(name) else { return LineKind::Unknown };
                sink(E::ProtectionBroken { target: actor, move_id });
                return LineKind::Battle;
            }
            if let Some(terrain) = terrain_by_name(name) {
                sink(E::TerrainActivate { target: actor, terrain });
                return LineKind::Battle;
            }
            if let Some(m) = partial_trap(dex, name) {
                let of = self.of(p);
                let from = p.tag("from").and_then(|f| f.strip_prefix("move: ")).and_then(|m| self.mv(m));
                sink(E::EffectStart { actor, effect: VolatileEffect::PartiallyTrapped(m), of, from });
                return LineKind::Battle;
            }
            if let Some(effect) = effect_by_name(dex, name) {
                let of = self.of(p);
                sink(E::EffectActivate { actor, effect, of });
                return LineKind::Battle;
            }
            let Some(move_id) = self.mv(name) else { return LineKind::Unknown };
            let mut args = p.positional(3);
            match move_id {
                moves::SKETCH | moves::SPITE | moves::MIMIC => {
                    let about = args.next().and_then(|m| self.mv(m));
                    let amount = args.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                    sink(E::ActivateMove { target: actor, move_id, about, amount });
                    return LineKind::Battle;
                }
                moves::POLTERGEIST => {
                    let item = args.next().map(|i| self.arg(i, ArgKind::Item));
                    let of = self.of(p);
                    sink(E::ActivateEffect { actor, effect: EffectName::Move(move_id), args: [item, None], of });
                    return LineKind::Battle;
                }
                _ => {}
            }
            let first = args.next();
            if let Some(first) = first {
                // `move: X|<move>[|<n>]`.
                let about = self.mv(first);
                let amount = args.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                if about.is_some() {
                    sink(E::ActivateMove { target: actor, move_id, about, amount });
                } else {
                    let a0 = self.arg(first, ArgKind::Any);
                    let of = self.of(p);
                    sink(E::ActivateEffect { actor, effect: EffectName::Move(move_id), args: [Some(a0), None], of });
                }
                return LineKind::Battle;
            }
            let of = self.of(p);
            sink(E::MoveActivate { actor, move_id, of });
            return LineKind::Battle;
        }
        let Some(effect) = self.effect(what) else { return LineKind::Unknown };
        let prefer = match effect {
            EffectName::Ability(abilities::FOREWARN) => ArgKind::Move,
            EffectName::Ability(abilities::SYMBIOSIS) => ArgKind::Item,
            EffectName::Item(_) => ArgKind::Move,
            _ => ArgKind::Any,
        };
        let mut args = [None; 2];
        let mut k = 0;
        for i in 3..p.n {
            let a = p.v[i];
            if a.starts_with("[of] ") || a.starts_with("[from] ") || k == 2 {
                continue;
            }
            // Only the arguments the engine writes: texts in brackets
            // (`[consumed]`, `[fromitem]`) are arguments too.
            args[k] = Some(self.arg(a, prefer));
            k += 1;
        }
        let of = self.of(p);
        sink(E::ActivateEffect { actor, effect, args, of });
        LineKind::Battle
    }

    fn start(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let dex = self.dex;
        let Some(actor) = self.ident(p.get(1)) else { return LineKind::Unknown };
        let what = p.get(2);
        let silent = p.has("[silent]");
        let from = p.tag("from");
        let from_effect = from.and_then(|f| self.effect(f));
        let from_move = match from_effect {
            Some(EffectName::Move(m)) => Some(m),
            _ => None,
        };
        let cause_is_effect = matches!(from_effect, Some(EffectName::Ability(_) | EffectName::Item(_)));
        if let Some(n) = what.strip_prefix("perish").and_then(|n| n.parse().ok()) {
            sink(E::PerishCount { actor, count: n, silent });
            return LineKind::Battle;
        }
        if let Some(n) = what.strip_prefix("stockpile").and_then(|n| n.parse().ok()) {
            sink(E::ManipulationStart { actor, volatile: ManipulationVolatile::Stockpile(n), silent });
            return LineKind::Battle;
        }
        match what {
            "typechange" => {
                let types = p.get(3);
                if from == Some("move: Reflect Type") {
                    let Some(of) = self.of(p) else { return LineKind::Unknown };
                    sink(E::TypeCopy { target: actor, of });
                    return LineKind::Battle;
                }
                let mut ts = types.split('/').filter_map(type_by_name);
                let Some(primary) = ts.next() else { return LineKind::Unknown };
                let types = TypeSet { primary, secondary: ts.next() };
                if cause_is_effect && let Some(cause) = self.cause(p) {
                    sink(E::TypeChangeFrom { target: actor, types, cause });
                } else {
                    sink(E::TypeChange { target: actor, types, silent });
                }
                return LineKind::Battle;
            }
            "typeadd" => {
                let Some(ty) = type_by_name(p.get(3)) else { return LineKind::Unknown };
                sink(E::TypeAdd { target: actor, ty, move_id: from_move });
                return LineKind::Battle;
            }
            "Mimic" => {
                let Some(move_id) = self.mv(p.get(3)) else { return LineKind::Unknown };
                sink(E::MimicStart { actor, move_id });
                return LineKind::Battle;
            }
            "confusion" if p.has("[fatigue]") => {
                sink(E::FatigueConfusion { actor });
                return LineKind::Battle;
            }
            "Uproar" => {
                sink(E::UproarStart { actor, upkeep: p.has("[upkeep]") });
                return LineKind::Battle;
            }
            "Doom Desire" => {
                sink(E::FutureMoveStart { user: actor, move_id: moves::DOOM_DESIRE });
                return LineKind::Battle;
            }
            "move: Future Sight" => {
                sink(E::FutureMoveStart { user: actor, move_id: moves::FUTURE_SIGHT });
                return LineKind::Battle;
            }
            _ => {}
        }
        // `-start|<pokemon>|<what>[|<arg>]|[from] ability: X|[of] Y`.
        if cause_is_effect
            && let Some(w) = static_text(what).filter(|w| matches!(*w, "Attract" | "confusion" | "Disable" | "Charge"))
        {
            let arg = p.positional(3).next().map(|a| self.arg(a, ArgKind::Move));
            let Some(cause) = self.cause(p) else { return LineKind::Unknown };
            sink(E::StartFrom { actor, what: w, arg, cause });
            return LineKind::Battle;
        }
        if let Some(v) = volatile_by_name(what) {
            let move_id = p.positional(3).next().and_then(|m| self.mv(m));
            sink(E::VolatileStart { actor, volatile: v, move_id });
            return LineKind::Battle;
        }
        if let Some(v) = manipulation_by_name(what) {
            sink(E::ManipulationStart { actor, volatile: v, silent });
            return LineKind::Battle;
        }
        if let Some(effect) = effect_by_name(dex, what)
            .or_else(|| (what == "move: Leech Seed").then_some(VolatileEffect::LeechSeed))
            .or_else(|| (what == "move: Octolock").then_some(VolatileEffect::Octolock))
            .filter(|e| !matches!(e, VolatileEffect::PartiallyTrapped(_)))
        {
            let of = self.of(p);
            sink(E::EffectStart { actor, effect, of, from: from_move });
            return LineKind::Battle;
        }
        self.start_end(actor, true, p, sink)
    }

    fn end(&mut self, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        use BattleEvent as E;
        let dex = self.dex;
        let Some(actor) = self.ident(p.get(1)) else { return LineKind::Unknown };
        let what = p.get(2);
        let silent = p.has("[silent]");
        match what {
            "Uproar" => {
                sink(E::UproarEnd { actor });
                return LineKind::Battle;
            }
            "move: Future Sight" | "move: Doom Desire" => {
                let move_id = if what.ends_with("Sight") { moves::FUTURE_SIGHT } else { moves::DOOM_DESIRE };
                sink(E::FutureMoveEnd { target: actor, move_id });
                return LineKind::Battle;
            }
            _ => {}
        }
        if let Some(v) = volatile_by_name(what) {
            sink(E::VolatileEnd { actor, volatile: v });
            return LineKind::Battle;
        }
        if let Some(effect) = effect_by_name(dex, what) {
            if let Some(m) = p.tag("from").and_then(|f| f.strip_prefix("move: ")).and_then(|m| self.mv(m))
                && let Some(of) = self.of(p)
            {
                sink(E::EffectEndFrom { actor, effect, move_id: m, of });
            } else {
                sink(E::EffectEnd { actor, effect, silent });
            }
            return LineKind::Battle;
        }
        if let Some(v) = manipulation_by_name(what) {
            sink(E::ManipulationEnd { actor, volatile: v, silent });
            return LineKind::Battle;
        }
        self.start_end(actor, false, p, sink)
    }

    /// `-start` / `-end` of abilities, items and the texts the engine
    /// writes (`EffectStartEnd`).
    fn start_end(&mut self, actor: Actor, start: bool, p: &Parts<'_>, sink: &mut dyn FnMut(BattleEvent)) -> LineKind {
        let what = p.get(2);
        let silent = p.has("[silent]");
        let (effect, what) = match self.effect(what) {
            Some(e) => {
                let arg = p.positional(3).next().map_or(LineArg::Text(""), |a| self.arg(a, ArgKind::Any));
                (Some(e), arg)
            }
            None => match static_text(what) {
                Some(t) => (None, LineArg::Text(t)),
                None => return LineKind::Unknown,
            },
        };
        let cause = self.cause(p);
        sink(BattleEvent::EffectStartEnd { actor, start, what, effect, cause, silent });
        LineKind::Battle
    }
}

/// A Showdown format this crate can read, as a pikapro [`Format`]
/// ([`showdown_format`] makes them).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ShowdownFormat {
    /// The format whose dex, mechanics, game type, gimmicks and team
    /// preview the battle has.
    pub format: Format,
    /// The channel shows exact HP for everyone (Showdown's custom games,
    /// `debug` formats); otherwise a foe's HP is a percentage.
    pub exact_hp: bool,
    /// Games in a series: `Some(3)` for a `…bo3` format (Showdown's
    /// `Best of = 3` rule, played through a `game-bestof3-…` room whose
    /// games are battles of their own); `None` for one game.
    pub best_of: Option<u8>,
    /// How the format shows Open Team Sheets (`|showteam|`).
    pub open_sheets: OpenSheets,
    /// The server validates a player's team with `Obtainable` (the VGC and
    /// Champions formats; pikapro-showdown-server's `gen9vgc` and
    /// `championsvgc`), and plays it as its validator leaves it: a listed
    /// battle-only forme (`Floette-Mega @ Floettite`) starts as the forme
    /// it changes from. The custom games don't validate, and the random
    /// battles' teams are the server's.
    pub validated: bool,
}

/// Whether a format's players see each other's team sheets
/// (`data/rulesets.ts`: `Open Team Sheets`, `Force Open Team Sheets`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpenSheets {
    /// Never.
    #[default]
    No,
    /// Offered at team preview, and shown if both players accept
    /// (`/acceptopenteamsheets`): `Open Team Sheets`.
    Offered,
    /// Always shown at team preview: `Force Open Team Sheets` (the `…bo3`
    /// Champions formats).
    Forced,
}

/// The pikapro format a Showdown format id plays as, for reading its
/// channel (not for simulating it: the view needs the dex, mechanics, game
/// type, gimmicks and team preview, and those match exactly):
///
/// | Showdown id | format | HP shown | series, open sheets | teams validated |
/// |---|---|---|---|---|
/// | `gen9championsvgc2026regmc`, `…regmb` | `championsvgc` | percent | offered | yes |
/// | `gen9championsvgc2026regmcbo3`, `…regmbbo3` | `championsvgc` | percent | best of 3, forced | yes |
/// | `gen9championsbssregmc`, `…regmb` | `championsbss` | percent | | yes |
/// | `gen9vgc2025regi` and every `gen9vgc20…` regulation | `gen9vgc` | percent | offered | yes |
/// | a `gen9vgc20…bo3` id | `gen9vgc` | percent | best of 3, offered | yes |
/// | `gen9ou` | `gen9ou` | percent | | yes |
/// | `gen9customgame@@@!Team Preview` | `gen9singles` | exact | | |
/// | `gen9doublescustomgame@@@!Team Preview` | `gen9doubles` | exact | | |
/// | `gen9doublescustomgame@@@Picked Team Size = 4,…` | `gen9vgc` | exact | | |
/// | `gen9randombattle`, `gen9unratedrandombattle`, `gen9randombattleblitz` | `gen9randombattle` | percent | | |
/// | `gen9randomdoublesbattle` | `gen9randomdoublesbattle` | percent | | |
/// | the preset ids (`gen9singles`, `gen9doubles`, `gen9vgc`, `gen9ou`, `gen9randombattle`, `gen9randomdoublesbattle`, `championsvgc`, `championsbss`) | that preset | as its Showdown format | | `gen9vgc`, `gen9ou`, `championsvgc`, `championsbss` |
///
/// A `Best of = N` custom rule (a challenge's `…@@@Best of = 3`) makes any
/// of them a series of `N` games.
///
/// Reg M-B is still challengeable on Showdown master: its `championsregmb`
/// mod inherits the `champions` scripts (Reg M-C's mechanics) and changes
/// only legality and two moves' PP (Wish and Strength Sap have 10, not
/// M-C's 5). Its channel reads as `championsvgc` (`championsbss` for BSS
/// Reg M-B); only what comes from the dex's PP of those two moves differs.
///
/// A custom game's rules follow `@@@` as Showdown's challenge command
/// takes them. Plain `gen9customgame` and `gen9doublescustomgame` have
/// team preview on Showdown, which the `gen9singles` and `gen9doubles`
/// presets don't (their team preview request can't be answered in those
/// action spaces), so they aren't mapped (`None`). pikapro-showdown-server
/// lists its no-preview games under those ids: read them as the presets'
/// ids.
pub fn showdown_format(id: &str) -> Option<ShowdownFormat> {
    let (base, rules) = id.split_once("@@@").unwrap_or((id, ""));
    let base = pikapro_data::to_id(base);
    let has_rule = |rule: &str| rules.split(',').any(|r| pikapro_data::id_eq(r, rule));
    let plain = |format: Format, exact_hp: bool| ShowdownFormat {
        format,
        exact_hp,
        best_of: None,
        open_sheets: OpenSheets::No,
        validated: false,
    };
    let exact = |format: Format| Some(plain(format, true));
    let percent = |format: Format| Some(plain(format, false));
    // pikapro-showdown-server's validated presets.
    let validated = |f: Option<ShowdownFormat>| f.map(|f| ShowdownFormat { validated: true, ..f });
    // The VGC ladders offer sheets; their Champions series force them.
    let vgc = |format: Format, series: bool, forced: bool| {
        Some(ShowdownFormat {
            best_of: series.then_some(3),
            open_sheets: if forced { OpenSheets::Forced } else { OpenSheets::Offered },
            validated: true,
            ..plain(format, false)
        })
    };
    // A challenge's `Best of = N` rule (`bestof.onValidateRule`: an odd
    // number from 3 to 9).
    let custom_best_of = rules.split(',').find_map(|r| {
        let (name, value) = r.split_once('=')?;
        let n: u8 = value.trim().parse().ok()?;
        (pikapro_data::id_eq(name, "bestof") && n % 2 == 1 && (3..=9).contains(&n)).then_some(n)
    });
    let found = match base.as_str() {
        "gen9championsvgc2026regmc" | "gen9championsvgc2026regmb" => vgc(Format::champions_vgc(), false, false),
        "gen9championsvgc2026regmcbo3" | "gen9championsvgc2026regmbbo3" => vgc(Format::champions_vgc(), true, true),
        "championsvgc" => validated(percent(Format::champions_vgc())),
        // Battle Spot Singles: Flat Rules and VGC Timer, no Open Team Sheets
        // and no series format of its own.
        "gen9championsbssregmc" | "gen9championsbssregmb" | "championsbss" => {
            validated(percent(Format::champions_bss()))
        }
        // Smogon's OverUsed: the preset's id is Showdown's.
        "gen9ou" => validated(percent(Format::gen9_ou())),
        "gen9customgame" if has_rule("!Team Preview") => exact(Format::gen9_singles()),
        "gen9doublescustomgame" if has_rule("!Team Preview") => exact(Format::gen9_doubles()),
        "gen9doublescustomgame" if has_rule("Picked Team Size = 4") => exact(Format::gen9_vgc()),
        "gen9singles" => exact(Format::gen9_singles()),
        "gen9doubles" => exact(Format::gen9_doubles()),
        "gen9vgc" => validated(exact(Format::gen9_vgc())),
        "gen9randombattle" | "gen9unratedrandombattle" | "gen9randombattleblitz" => {
            percent(Format::gen9_random_battle())
        }
        "gen9randomdoublesbattle" => percent(Format::gen9_random_doubles_battle()),
        b if b.starts_with("gen9vgc20") => vgc(Format::gen9_vgc(), b.ends_with("bo3"), false),
        _ => None,
    };
    found.map(|f| ShowdownFormat { best_of: custom_best_of.or(f.best_of), ..f })
}

/// The max HP of `species` at `level` for the spreads (IV plus a quarter
/// of the EVs) that give `other` the max HP `max`, if they all agree.
fn other_forme_hp(format: &Format, other: SpeciesId, max: u16, species: SpeciesId, level: u8) -> Option<u16> {
    use pikapro::stats::calc_stat;
    use pikapro_data::{Nature, Stat};
    if format.stat_system != pikapro::format::StatSystem::EffortValues {
        return None;
    }
    let base = |s: SpeciesId| format.dex.species(s).base_stats[Stat::Hp];
    let (b1, b2) = (base(other), base(species));
    let mut found = None;
    for x in 0..=94u8 {
        let (iv, ev) = (x.min(31), (x - x.min(31)) * 4);
        if calc_stat(Stat::Hp, b1, iv, ev, level, Nature::Hardy) != max {
            continue;
        }
        let hp = calc_stat(Stat::Hp, b2, iv, ev, level, Nature::Hardy);
        match found {
            None => found = Some(hp),
            Some(h) if h != hp => return None,
            Some(_) => {}
        }
    }
    found
}

/// A type by name, the typeless `???` included.
pub(crate) fn type_by_name(name: &str) -> Option<Type> {
    if name == "???" { Some(Type::Unknown) } else { Type::from_name(name) }
}

/// Which kind a generic argument is read as first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ArgKind {
    Any,
    Move,
    Item,
}

#[cfg(test)]
mod tests;
