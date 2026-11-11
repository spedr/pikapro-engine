//! What the public log says about one Pokémon: [`MonView`] and its parts.

use pikapro::{Gender, Status};
use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId, Type};
use pikapro_protocol::HpHint;

use pikapro::util::StackVec;

/// Revealed moves kept per Pokémon. A Pokémon has at most four, but under
/// Illusion the disguise's record also collects the moves Zoroark uses
/// until the `|replace|`, as a client's does, so the record has room for
/// more.
pub const MAX_REVEALED_MOVES: usize = 8;

/// A non-volatile status as the log names it (`brn`, `slp`, ...), without
/// the engine's hidden counters (sleep turns left, the toxic stage).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StatusKind {
    #[default]
    None = 0,
    Burn = 1,
    Freeze = 2,
    Paralysis = 3,
    Poison = 4,
    /// Badly poisoned (`tox`).
    Toxic = 5,
    Sleep = 6,
}

impl StatusKind {
    /// Every kind, by discriminant.
    pub const ALL: [StatusKind; 7] = [
        StatusKind::None,
        StatusKind::Burn,
        StatusKind::Freeze,
        StatusKind::Paralysis,
        StatusKind::Poison,
        StatusKind::Toxic,
        StatusKind::Sleep,
    ];

    /// The kind of an engine status. The engine's counters are dropped:
    /// they are exactly what a player can't see.
    pub const fn of(status: Status) -> StatusKind {
        match status {
            Status::None => StatusKind::None,
            Status::Burn => StatusKind::Burn,
            Status::Freeze { .. } => StatusKind::Freeze,
            Status::Paralysis => StatusKind::Paralysis,
            Status::Poison => StatusKind::Poison,
            Status::Toxic { .. } => StatusKind::Toxic,
            Status::Sleep { .. } => StatusKind::Sleep,
        }
    }
}

/// HP as a player sees it: a fraction `hp / max`, as the protocol writes
/// it (`45/100`, `118/135`).
///
/// For a foe (and for everything in the shared [`PublicTracker`](crate::PublicTracker)
/// under [`HpReport::Percent`](crate::HpReport::Percent)) `max` is 100 and
/// `hp` the shown percentage ([`pikapro_protocol::hp_shown`]: SV rounds up
/// and caps at 99 while hurt; Champions rounds down, is at least 1 while
/// standing, and adds [`hint`](Self::hint)). Under
/// [`HpReport::Exact`](crate::HpReport::Exact) they are the exact values.
/// A player's exact HP for its own Pokémon is in its
/// [`OwnOverlay`](crate::OwnOverlay).
///
/// `max == 0` means no HP has been shown yet (a Pokémon seen only at team
/// preview, which as far as anyone knows is at full HP).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HpView {
    pub hp: u16,
    pub max: u16,
    /// The Champions health bar's colour hint on a percentage at 20% or
    /// 50% ([`HpHint::None`] otherwise, and always under SV mechanics or
    /// exact reports).
    pub hint: HpHint,
}

impl HpView {
    /// Whether any HP has been shown.
    pub const fn is_known(self) -> bool {
        self.max != 0
    }
}

/// How an item left its holder (`-enditem` and what caused it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ItemLoss {
    /// Used up (`-enditem` without `[eat]`: a Focus Sash, a gem with
    /// `[from] gem`, a Booster Energy, an Air Balloon popped).
    #[default]
    Consumed,
    /// A berry eaten (`[eat]`), by its holder.
    Eaten,
    /// Removed by Knock Off (`[from] move: Knock Off`).
    KnockedOff,
    /// Taken by another Pokémon (Thief, Covet, Magician, Pickpocket; Bug
    /// Bite and Pluck, which eat it: `[from] stealeat`).
    Stolen,
    /// Thrown by Fling.
    Flung,
    /// Burnt by Incinerate.
    Incinerated,
    /// Given away by Trick or Switcheroo for nothing, or by Bestow.
    Traded,
    /// Removed by a way not listed above (Corrosive Gas, ...).
    Other,
}

/// What is known about a Pokémon's held item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ItemState {
    /// Nothing revealed yet.
    #[default]
    Unknown,
    /// Holding this item: revealed by `-item`, an item-caused event
    /// (`[from] item: Leftovers`), Mega Evolution (its stone), Trick or
    /// Switcheroo, or an Open Team Sheet.
    Known(ItemId),
    /// Known to hold nothing from the start (an Open Team Sheet with no
    /// item).
    NoItem,
    /// Held an item and lost it (`-enditem`): the item if the log named
    /// it, and how. Acrobatics and Unburden care.
    Gone { item: Option<ItemId>, how: ItemLoss },
}

/// A Pokémon's own item (its set's, the one it was brought with) once it
/// has held another's: [`MonView::item`] is what it holds now, which an
/// item moved between Pokémon makes no longer its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OwnItem {
    /// Every item the log showed it hold was its own: [`MonView::item`]
    /// (held, or lost: `Gone`) is its set's.
    #[default]
    Held,
    /// It received another Pokémon's item (Trick, Switcheroo, Bestow,
    /// Thief, Covet, Magician, Pickpocket, Pickup, Symbiosis, a Sticky
    /// Barb): what it holds or lost since says nothing of its set. Its own
    /// item as the log showed it then: the one it held until that trade
    /// (shown before, or on the Pokémon it went to: Trick's other
    /// `-item`), else `None` (it held none then, or the log never named
    /// it).
    Moved(Option<ItemId>),
}

impl OwnItem {
    /// Its own item as far as the log showed it, given what it holds or
    /// lost now (`item`); `None` when the log never named it.
    pub fn of(self, item: ItemState) -> Option<ItemId> {
        match (self, item) {
            (OwnItem::Moved(own), _) => own,
            (OwnItem::Held, ItemState::Known(i) | ItemState::Gone { item: Some(i), .. }) => Some(i),
            (OwnItem::Held, _) => None,
        }
    }
}

/// What is known about a Pokémon's ability.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AbilityState {
    /// Nothing revealed yet.
    #[default]
    Unknown,
    /// Revealed by `-ability`, an ability-caused event (`[from] ability:
    /// X`), Skill Swap or Trace, an Open Team Sheet, or implied by a
    /// species with only one possible ability.
    Known(AbilityId),
}

impl AbilityState {
    /// The ability, if known.
    pub const fn known(self) -> Option<AbilityId> {
        match self {
            AbilityState::Known(a) => Some(a),
            AbilityState::Unknown => None,
        }
    }
}

/// A move a Pokémon has been seen using.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RevealedMove {
    pub id: MoveId,
    /// Times it was used from the Pokémon's own move slot (`|move|` lines
    /// without `[from]`; a locked-in continuation (`[from] lockedmove`)
    /// counts once, when it was chosen).
    pub uses: u8,
    /// PP the uses cost as far as the log shows: one per use, one more
    /// against each foe with Pressure that was known at the time and that
    /// the move targeted (the one its line names, every foe of a spread
    /// move or of a `[spread]` Expanding Force, Dragon Darts' two, a
    /// charged move's target named at its release, the lone foe in
    /// singles; in doubles, a target `[still]` hid is in doubt), never
    /// past the move's maximum; less what a Leppa Berry or Lunar Dance
    /// gave back.
    pub pp_spent: u8,
}

/// Bits of [`MonView::flags`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MonFlags(pub u16);

impl MonFlags {
    /// Listed at team preview (`|poke|`).
    pub const PREVIEWED: u16 = 1 << 0;
    /// Team preview showed a wildcard (`Urshifu-*`,
    /// [`pikapro_protocol::preview_wildcard`]): [`MonView::species`] is the
    /// base species until a switch-in shows the forme.
    pub const FORME_HIDDEN: u16 = 1 << 1;
    /// Has been on the field (switched in, dragged in, or named by a
    /// `|replace|`).
    pub const REVEALED: u16 = 1 << 2;
    /// On the field now (one of its side's [`ActiveView::row`](crate::ActiveView::row)s).
    pub const ACTIVE: u16 = 1 << 3;
    /// Fainted (`|faint|`); cleared by Revival Blessing.
    pub const FAINTED: u16 = 1 << 4;
    /// Terastallized (`-terastallize`).
    pub const TERASTALLIZED: u16 = 1 << 5;
    /// Mega Evolved (`-mega`).
    pub const MEGA: u16 = 1 << 6;
    /// Was the disguise of an Illusion that has since broken: some of the
    /// facts this record collected while disguised were moved to the
    /// Pokémon behind it (see [`PublicSide::entered`](crate::PublicSide::entered)).
    pub const WAS_DISGUISE: u16 = 1 << 7;
    /// Its sleep is Rest's (`-status|<pokemon>|slp|[from] move: Rest`),
    /// which lasts exactly 3 turns (Showdown's `rest.onHit` sets
    /// `statusState.time = 3`); cleared when its status changes.
    pub const SLEEP_FROM_REST: u16 = 1 << 8;

    #[inline]
    pub const fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    #[inline]
    pub fn set(&mut self, bit: u16, on: bool) {
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

/// Everything the public log has said about one Pokémon, persisting while
/// it is benched. Per-switch-in facts (boosts, volatiles) live in its
/// slot's [`ActiveView`](crate::ActiveView).
///
/// The record describes the Pokémon *as shown*: under Illusion the log
/// names the disguise, so its record is the one that changes (see
/// [`PublicSide::entered`](crate::PublicSide::entered)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MonView {
    /// The species shown now: the details' species at switch-in, then
    /// changed by `-formechange` (temporary formes too: Mimikyu-Busted,
    /// Palafin-Hero, Eiscue-Noice), `detailschange` and `-mega`. Before its
    /// first switch-in, the team preview species (the base species if
    /// [`MonFlags::FORME_HIDDEN`]).
    pub species: SpeciesId,
    /// The species its details (`|switch|`, `|poke|`, `|detailschange|`)
    /// show: changes only permanently (`detailschange`, Mega Evolution, a
    /// switch-in that reveals a hidden forme), never with `-formechange`.
    pub details_species: SpeciesId,
    pub level: u8,
    pub gender: Gender,
    pub flags: MonFlags,
    /// The last HP shown for it (kept while benched).
    pub hp: HpView,
    /// The status last shown (`-status`, `-curestatus`, or after the HP in
    /// any HP report such as `|switch|` or `-damage`).
    pub status: StatusKind,
    /// Turns the status has been *observed* to last, counted from events,
    /// never from the engine's counters: for sleep, the Pokémon's attempts
    /// to move while asleep since it fell asleep (its `cant|slp` lines: a
    /// Sleep Talk or Snore turn shows one, then the move); for freeze, its
    /// attempts to move while frozen (`cant|frz`), which Champions counts
    /// down (it thaws by the third at the latest: `frz.onBeforeMove`,
    /// `startTime = 3`); for toxic, the end-of-turn poison damages (or
    /// Poison Heal's heals) since it was badly poisoned or last switched
    /// in, at most 15 like Showdown's toxic counter, which it equals unless
    /// Magic Guard hid some. 0 for the other statuses.
    pub status_turns: u8,
    pub item: ItemState,
    /// Its own item (the set's) once it has held another's ([`OwnItem`]).
    pub own_item: OwnItem,
    /// The ability it has now, as last shown (Skill Swap, Trace, Mummy and
    /// the like change it).
    pub ability: AbilityState,
    /// Its own ability (the set's), when the log or a sheet showed it.
    pub base_ability: AbilityState,
    /// Its Tera type, once shown (`-terastallize`, or an Open Team Sheet).
    pub tera_type: Option<Type>,
    /// Moves used from its own move slots, in the order first seen. Moves
    /// it only called (Metronome, `[from] move: X`; but Sleep Talk calls
    /// one of its own, kept with 0 uses), Struggle,
    /// moves used while transformed and Mimic's copy are not kept. A move
    /// it was seen choosing but not using (`cant|...|Disable|<move>`,
    /// `cant|...|nopp`, Imprison, Gravity, Throat Chop, Heal Block) or that
    /// Forewarn, Eerie Spell, Spite or a Leppa Berry named is kept with
    /// 0 uses.
    pub moves: StackVec<RevealedMove, MAX_REVEALED_MOVES>,
    /// The move Sketch copied permanently into Sketch's slot
    /// (`-activate|<pokemon>|move: Sketch|<move>`). Kept out of
    /// [`MonView::moves`], like Mimic's copy.
    pub sketched: Option<MoveId>,
}

impl MonView {
    pub fn is_fainted(&self) -> bool {
        self.flags.has(MonFlags::FAINTED)
    }

    pub fn is_active(&self) -> bool {
        self.flags.has(MonFlags::ACTIVE)
    }

    pub fn is_revealed(&self) -> bool {
        self.flags.has(MonFlags::REVEALED)
    }

    /// The revealed move `id`, if seen.
    pub fn revealed(&self, id: MoveId) -> Option<&RevealedMove> {
        self.moves.iter().find(|m| m.id == id)
    }
}
