//! [`PublicTracker`]: what a spectator knows, built event by event (the
//! event handlers are in `apply.rs`).

use core::fmt;

use pikapro::event::Actor;
use pikapro::util::StackVec;
use pikapro::{Battle, Format, Gender, MAX_MOVES, MAX_TEAM, Mechanics, SideId, Team};
use pikapro_data::{AbilityId, Dex, ItemId, MoveId, Nature, SpeciesId, Type, TypeSet};

use crate::mon::{AbilityState, ItemState, MonFlags, MonView, RevealedMove};
use crate::side::{ActiveView, PublicField, PublicSide, PublicVolatiles};

/// How HP is shown in the public record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HpReport {
    /// Percentages ([`pikapro_protocol::hp_shown`]), as ladder formats show a
    /// foe's HP. The default, since the ladders show percentages.
    #[default]
    Percent,
    /// Exact HP for both sides: opt-in, and what Showdown's custom games
    /// show (`reportExactHP`, [`pikapro_protocol::BattleInfo::report_exact_hp`]).
    Exact,
}

/// How a tracker reads events: the format's rules that change what is
/// shown. Build it with [`ViewConfig::new`] and the `with_*` methods (or
/// set a field afterwards); new fields may come in a minor version.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct ViewConfig {
    /// The format's dex (species' types and abilities).
    pub dex: &'static Dex,
    pub mechanics: Mechanics,
    /// The format has team preview: `|poke|` lines are shown and fill the
    /// rows. Without it the engine's `Poke` events are ignored (the
    /// protocol doesn't show them) and rows fill in order of appearance.
    pub team_preview: bool,
    /// Active slots per side.
    pub active: u8,
    pub hp: HpReport,
}

impl ViewConfig {
    /// The format's configuration, with percentages.
    pub fn new(format: &Format) -> ViewConfig {
        ViewConfig {
            dex: format.dex,
            mechanics: format.mechanics,
            team_preview: format.team_preview.shown(),
            active: format.active_per_side() as u8,
            hp: HpReport::Percent,
        }
    }

    /// With HP shown as `hp` says.
    pub fn with_hp(self, hp: HpReport) -> ViewConfig {
        ViewConfig { hp, ..self }
    }
}

impl fmt::Debug for ViewConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewConfig")
            .field("dex", &self.dex.name)
            .field("mechanics", &self.mechanics)
            .field("team_preview", &self.team_preview)
            .field("active", &self.active)
            .field("hp", &self.hp)
            .finish()
    }
}

impl PartialEq for ViewConfig {
    fn eq(&self, other: &ViewConfig) -> bool {
        core::ptr::eq(self.dex, other.dex)
            && (self.mechanics, self.team_preview, self.active, self.hp)
                == (other.mechanics, other.team_preview, other.active, other.hp)
    }
}

impl Eq for ViewConfig {}

/// Where the battle is, as the log shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Before team preview or the start.
    #[default]
    Setup,
    /// `|teampreview|` was shown.
    TeamPreview,
    /// `|start|` was shown.
    Battle,
    /// `|win|` or `|tie|` was shown.
    Over,
}

/// How the battle ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Win(SideId),
    Tie,
}

/// One Pokémon of an Open Team Sheet: what Showdown's Open Team Sheets
/// show (species, item, ability, moves, level, gender and Tera type; the
/// nature too in Champions), never EVs or IVs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SheetMon {
    pub species: SpeciesId,
    pub level: u8,
    /// Its gender ([`Gender::Genderless`] for a genderless Pokémon).
    pub gender: Gender,
    pub item: Option<ItemId>,
    pub ability: AbilityId,
    pub moves: StackVec<MoveId, MAX_MOVES>,
    pub tera_type: Type,
    /// Shown in Champions only.
    pub nature: Option<Nature>,
}

impl Default for SheetMon {
    fn default() -> SheetMon {
        SheetMon {
            species: SpeciesId::default(),
            level: 0,
            gender: Gender::Genderless,
            item: None,
            ability: AbilityId::NONE,
            moves: StackVec::new(),
            tera_type: Type::Normal,
            nature: None,
        }
    }
}

/// A side's Open Team Sheet, in the order of its `|poke|` lines (so entry
/// `k` describes row `k`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct OpenSheet {
    pub mons: StackVec<SheetMon, MAX_TEAM>,
}

impl OpenSheet {
    /// A team's sheet as Showdown's Open Team Sheets rule shows it at team
    /// preview (`showOpenTeamSheets`), from the sets in registration order
    /// (the `|poke|` order):
    ///
    /// * species, item, ability and moves as the sets give them; Zacian
    ///   holding a Rusted Sword and Zamazenta holding a Rusted Shield show
    ///   crowned, their Iron Head as Behemoth Blade or Bash;
    /// * the level the battle plays, after the format's level rule
    ///   (Showdown's sheet shows `set.level` as its validator adjusted it);
    /// * the gender the set gives, or the species' only one. A gender the
    ///   battle draws at random can't be known from the team: it is
    ///   [`Gender::Genderless`] here, where Showdown shows the drawn one
    ///   ([`OpenSheet::from_battle`] takes it from the battle);
    /// * the Tera type (the set's, else the species' first type, as the
    ///   battle takes it);
    /// * the nature in Champions only; never EVs, IVs or Stat Points.
    pub fn from_team(team: &Team, format: &Format) -> OpenSheet {
        use pikapro_data::gen9::{items, moves, species};
        let champions = format.mechanics == Mechanics::Champions;
        let dex = format.dex;
        let mut sheet = OpenSheet::default();
        for set in team.sets.iter().take(MAX_TEAM) {
            let data = dex.species(set.species);
            let crowned = match (data.base_species, set.item) {
                (species::ZACIAN, Some(items::RUSTED_SWORD)) => Some((species::ZACIAN_CROWNED, moves::BEHEMOTH_BLADE)),
                (species::ZAMAZENTA, Some(items::RUSTED_SHIELD)) => {
                    Some((species::ZAMAZENTA_CROWNED, moves::BEHEMOTH_BASH))
                }
                _ => None,
            };
            let mut mv = set.moves;
            if let Some((_, signature)) = crowned
                && let Some(m) = mv.iter_mut().find(|m| **m == moves::IRON_HEAD)
            {
                *m = signature;
            }
            let gender = set.gender.unwrap_or(match data.gender {
                pikapro_data::GenderRule::Male => Gender::Male,
                pikapro_data::GenderRule::Female => Gender::Female,
                _ => Gender::Genderless,
            });
            let tera = set.tera_type.unwrap_or(data.types.primary);
            sheet.mons.push(SheetMon {
                species: crowned.map_or(set.species, |(s, _)| s),
                level: format.level.apply(set.level),
                gender,
                item: set.item,
                ability: set.ability,
                moves: mv,
                tera_type: tera,
                nature: champions.then_some(set.nature),
            });
        }
        sheet
    }

    /// `side`'s sheet in `battle`, started with `team` for that side:
    /// [`OpenSheet::from_team`] with each Pokémon's gender as the battle
    /// drew it (Showdown's sheet shows `pokemon.gender`).
    pub fn from_battle(team: &Team, battle: &Battle, side: SideId) -> OpenSheet {
        let mut sheet = OpenSheet::from_team(team, battle.format());
        for mon in battle.state().side(side).team.iter() {
            if let Some(entry) = sheet.mons.get_mut(usize::from(mon.id)) {
                entry.gender = mon.gender;
            }
        }
        sheet
    }
}

/// A Trick or Switcheroo going on (Showdown's `trick.onHit`: the
/// target's line, `-item` with the user's item or `-enditem` of its own,
/// then the user's), for each Pokémon's own item ([`MonView::own_item`]):
/// one's line names what the other held before.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ItemSwap {
    /// The user and its target.
    pub pair: [Actor; 2],
    /// Each held its own item until now (it had received none before).
    pub held: [bool; 2],
    /// The item each held before the trade, once a line named it.
    pub before: [Option<ItemId>; 2],
}

/// What the tracker remembers between events besides the facts: the last
/// `|move|` line (who used what on whom, for the lines that follow it),
/// and what recent lines left pending (locks, a Dancer, a silent type
/// change).
///
/// Not compared ([`PartialEq`] always holds): two trackers are equal when
/// they hold the same facts and ids, even if what the last lines left
/// pending here would make them read the same next event differently.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ApplyCtx {
    /// The user and target of the last `|move|` line, and its move.
    pub mover: Option<Actor>,
    pub move_id: Option<MoveId>,
    pub move_target: Option<Actor>,
    /// The last `|move|` line's Jaw Lock user showed a `trapped` line that
    /// named no partner: it is trapped once its target's line does.
    pub jaw_lock_pending: bool,
    /// The last move's contact, when its line shows it differs from the
    /// dex (`[anim] Shell Side Arm Physical`).
    pub contact: Option<bool>,
    /// An `-ability|<pokemon>|As One` line just named this Pokémon: the
    /// `-ability|<pokemon>|Unnerve` that follows is As One's, not its
    /// ability.
    pub as_one: Option<Actor>,
    /// The last `-enditem` since the last `|move|` line.
    pub item_ended: Option<(Actor, ItemId)>,
    /// The Trick or Switcheroo whose `-item` / `-enditem` lines follow
    /// (its `-activate|<user>|move: Trick|[of] <target>`).
    pub swap: Option<ItemSwap>,
    /// Dancer just activated for this Pokémon: the move it copies isn't
    /// one of its own, even when `cant` names it (Imprison).
    pub dancing: Option<Actor>,
    /// The move line's user (its side's index and its id), the move whose
    /// PP it spent, and the foe slots with Pressure it didn't take PP for,
    /// as its line named its target: a `[spread]` that follows (Expanding
    /// Force in Psychic Terrain) says Pressure took theirs too.
    pub pressure: Option<(u8, u8, MoveId, u8)>,
    /// The PP Pressure took for the move line, as counted: a `[notarget]`
    /// that follows says it had no target, so Pressure took none.
    pub pressure_taken: Option<(u8, u8, MoveId, u8)>,
    /// Per side and slot, the Pokémon (its id) whose charge turn hid its
    /// target (`[still]`) from Pressure, in doubles, with the charged move
    /// and the foe slots whose Pokémon had Pressure then: its release
    /// (`[from]lockedmove`) names the target's slot. Ally Switch clears
    /// them.
    pub charge_pressure: [[Option<(u8, MoveId, u8)>; pikapro::MAX_ACTIVE]; 2],
    /// The move line was such a release, and took the charge's Pressure
    /// PP (its user's side index and id, the move): a Storm Drain or
    /// Lightning Rod that shows it drew the release gives them back.
    pub released: Option<(u8, u8, MoveId)>,
    /// Cud Chew just activated for this Pokémon: the berry it eats again is
    /// the one it ate last turn, not its held item.
    pub cud_chew: Option<Actor>,
    /// The berry Cud Chew just ate again: its effect's `[from] item:` is
    /// not the held item.
    pub chewed: Option<(Actor, ItemId)>,
    /// Per side, the slots whose Tatsugiri stopped commanding when its
    /// Dondozo fainted in the residuals, while nothing showed that the
    /// `Update` that ends the command ran (the battle may end first: then
    /// it doesn't).
    pub uncommanded: [u8; 2],
    /// After a `|` that no `|t:|` followed: the residuals (or the end).
    pub residual: bool,
    /// Receiver or Power of Alchemy just showed on the ally that fainted:
    /// the `-block` of an Ability Shield names its holder.
    pub shielded: Option<AbilityId>,
    /// Each slot's types were last set by a `[silent]` type change.
    pub silent_types: [[bool; pikapro::MAX_ACTIVE]; 2],
    /// Per side and slot, the residuals since a charge (`-prepare`) or
    /// `-mustrecharge`: both volatiles last two (`duration: 2`), and end
    /// silently when the turn they lock was skipped (Commander cancels
    /// it).
    pub lock_residuals: [[u8; pikapro::MAX_ACTIVE]; 2],
    /// Per side and slot, a lock-in move (Outrage, Uproar) a Metronome
    /// called: its next turns may show without `[from]lockedmove`.
    pub called_lock: [[Option<MoveId>; pikapro::MAX_ACTIVE]; 2],
    /// The turn records ([`PublicSide::this_turn`](crate::PublicSide::this_turn)):
    /// actions shown since the last `|turn|` (the next one's place).
    pub actions: u8,
    /// A `|move|` or `cant` line came since the last `|turn|`: chosen
    /// switches (which come first) are over.
    pub moves_begun: bool,
    /// `|upkeep` came since the last `|turn|`: a switch-in replaces a
    /// fainted Pokémon.
    pub upkept: bool,
    /// Per side and slot: an Eject Button, Eject Pack, Emergency Exit or
    /// Wimp Out just sent the Pokémon there out (its replacement is
    /// forced).
    pub exiting: [[bool; pikapro::MAX_ACTIVE]; 2],
    /// The side and slot whose action the lines are about: its record
    /// takes the move's outcome and the damage it deals.
    pub acting: Option<(usize, usize)>,
    /// The side and slot of the Pokémon using a move now, its turn's action
    /// or not (Showdown's `battle.activePokemon` with an `activeMove` that
    /// isn't external: an Instructed move counts, a Dancer copy or a move
    /// Magic Bounce sent back doesn't). Disable reads it (Cursed Body).
    pub moving: Option<(u8, u8)>,
    /// The side and slot of a Pokémon Instruct just made move again
    /// (`-singleturn|<pokemon>|move: Instruct`): its next `|move|` line is
    /// a move action Instruct queued (`instruct.onHit`:
    /// `queue.prioritizeAction`), not its turn's own, which may still come.
    pub instructed: Option<(u8, u8)>,
    /// The acting move damaged a target.
    pub acting_hit: bool,
    /// The first failure the acting move showed (miss, protected, ...).
    pub acting_failed: bool,
    /// The target of the acting slot's move, written into its record when
    /// the action ends ([`PublicTracker::commit_target`](crate::PublicTracker)):
    /// `[still]`, which the engine adds to the move's line later, blanks
    /// it.
    pub acting_target: Option<(u8, u8, crate::side::TurnTarget)>,
    /// The last `|move|` line was a protection whose effect was already up
    /// (a second Wide Guard or Quick Guard of the turn, an Instructed
    /// Protect), which counted for its user's protect chain at once
    /// (`PublicTracker::guard_again`): a `-fail` right after undoes it.
    pub guard_again: Option<Actor>,
    /// The ability a Pokémon (side, id) just lost to Mummy, Lingering
    /// Aroma, Wandering Spirit and the like, which can still act in the
    /// same move (Toxic Chain's poison after Lingering Aroma took it).
    pub ability_lost: Option<(u8, u8, AbilityId)>,
    /// A protection's user (side and slot), with its protect chain and
    /// whether a protection of its already worked this turn, as they were
    /// before its `|move|` line (kept when the line counted at once:
    /// `PublicTracker::guard_again`); and whether it failed since
    /// (`-fail`) with nothing acting after it. The view ends the chain at
    /// the `-fail`, as a failed `stall` roll does; but if nothing acts
    /// before the residuals, `willAct` failed it before any roll, and
    /// Showdown's `stall` is left as it was (restored at the `|`).
    pub stall: Option<(u8, u8, u8, bool, bool)>,
    /// The last `|move|` line made a Wish, Healing Wish or Lunar Dance
    /// for its user's slot, which had these before (a `-fail` undoes it).
    pub slot_made: Option<(Actor, crate::side::SlotConditions)>,
}

impl PartialEq for ApplyCtx {
    fn eq(&self, _: &ApplyCtx) -> bool {
        true
    }
}

impl Eq for ApplyCtx {}

/// Everything a spectator of the battle knows, built only from the
/// engine's [`BattleEvent`](pikapro::BattleEvent)s (never from its
/// state), in the order the protocol shows them. One tracker serves both
/// players and spectators; a player's view adds its own side's request
/// ([`PlayerView`](crate::PlayerView)).
///
/// Events name Pokémon as shown (`Actor::species` and `Actor::id` are the
/// disguise's under Illusion), so the tracker is fooled by Illusion
/// exactly as a client is, until the `|replace|`.
///
/// Feed it events only after [`Battle::submit`](pikapro::Battle::submit)
/// returns: the engine amends the last `|move|` event while a turn
/// resolves.
///
/// Plain `Copy` data without heap: a fork for search is
/// `(battle.fork(seed), tracker)`.
///
/// `==` compares the facts and each side's ids (the map from the ids
/// events name Pokémon by to rows), not what the last lines left pending
/// for the next ones. Without team preview a foe's ids are the engine's
/// registration ids on the typed path but the order of appearance in a
/// channel's parser: compare rows there, not ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicTracker {
    pub config: ViewConfig,
    /// By [`SideId::index`].
    pub sides: [PublicSide; 2],
    pub field: PublicField,
    /// The last `|turn|` shown (0 before the first).
    pub turn: u16,
    pub phase: Phase,
    /// Set by `|win|` or `|tie|`.
    pub outcome: Option<Outcome>,
    /// Open Team Sheets, when the environment plays with them
    /// ([`PublicTracker::set_open_sheet`]); `None` otherwise (closed sheets,
    /// the ladder default).
    pub ots: [Option<OpenSheet>; 2],
    pub(crate) ctx: ApplyCtx,
}

impl PublicTracker {
    /// A tracker before any event.
    pub fn new(config: ViewConfig) -> PublicTracker {
        PublicTracker {
            config,
            sides: [PublicSide::default(); 2],
            field: PublicField::default(),
            turn: 0,
            phase: Phase::Setup,
            outcome: None,
            ots: [None; 2],
            ctx: ApplyCtx::default(),
        }
    }

    /// A tracker for `format` with percentages, before any event.
    pub fn for_format(format: &Format) -> PublicTracker {
        PublicTracker::new(ViewConfig::new(format))
    }

    pub fn side(&self, side: SideId) -> &PublicSide {
        &self.sides[side.index()]
    }

    /// The row of the Pokémon an event names (`actor.id` is its shown
    /// identity: the disguise's under Illusion), once the log has shown it.
    /// For code that reads events alongside the view; the rows of a side
    /// are [`PublicSide::mons`].
    pub fn row_of_actor(&self, actor: &Actor) -> Option<u8> {
        self.sides[actor.pos.side.index()].row_of(actor.id)
    }

    /// The types the log shows for the Pokémon in `side`'s active `slot`
    /// (Showdown's `getTypes`): its Tera type once Terastallized (but a
    /// Stellar one keeps its types), else the types a type change gave it
    /// (Soak, Reflect Type, Transform, Protean, ...), else its shown
    /// species' types; without Flying while it roosts. The type Forest's
    /// Curse or Trick-or-Treat added is [`ActiveView::added_type`]. `None`
    /// for an empty slot.
    pub fn types(&self, side: SideId, slot: usize) -> Option<TypeSet> {
        let s = self.side(side);
        let (_, mon) = s.active(slot)?;
        Some(self.types_of(mon, &s.slots[slot]))
    }

    pub(crate) fn types_of(&self, mon: &MonView, slot: &ActiveView) -> TypeSet {
        let tera = mon.tera_type.filter(|_| mon.flags.has(MonFlags::TERASTALLIZED));
        shown_types(self.config.dex.species(mon.species).types, tera, Some(slot))
    }

    /// Shows both players `side`'s Open Team Sheet (at team preview): the
    /// sheet is kept in [`PublicTracker::ots`] and its facts fill the
    /// side's rows (entry `k` is row `k`, the `|poke|` order): the species
    /// (a preview wildcard's forme, as the details will show it), the item
    /// (or none), the ability, the moves (with no uses) and the Tera type.
    /// What the log has already shown is kept. It may come before the
    /// `|poke|` lines: each row is filled as it appears.
    ///
    /// Showdown sends the sheets at team preview. One fed after `|start|`
    /// (a client that sends it late) still fills what can't have changed,
    /// but not the item of a Pokémon that has been on the field: the
    /// sheet shows the item it started with, which it may have lost or
    /// traded since, silently (a Sticky Barb moving on contact).
    ///
    /// Without team preview there are no sheets (Showdown offers them at
    /// team preview, and rows are then in order of appearance, not the
    /// sheet's): the call does nothing.
    pub fn set_open_sheet(&mut self, side: SideId, sheet: OpenSheet) {
        if !self.config.team_preview {
            return;
        }
        self.ots[side.index()] = Some(sheet);
        let dex = self.config.dex;
        let started = matches!(self.phase, Phase::Battle | Phase::Over);
        let s = &mut self.sides[side.index()];
        for (row, entry) in sheet.mons.iter().enumerate() {
            let Some(mon) = s.mons.get_mut(row) else { break };
            let item = !started || !mon.is_revealed();
            fill_from_sheet(dex, mon, entry, started, item);
        }
    }

    /// `|start|`: a Zacian or Zamazenta whose sheet shows it crowned (its
    /// Rusted Sword or Shield) is crowned now (`Pokemon`'s `onStart` in
    /// Showdown: the forme team preview hid, with the sheet's help).
    pub(crate) fn crown_at_start(&mut self) {
        for (side, sheet) in self.sides.iter_mut().zip(self.ots) {
            let Some(sheet) = sheet else { continue };
            for (mon, entry) in side.mons.iter_mut().zip(sheet.mons.iter()) {
                if crowned(entry.species) && !mon.is_revealed() && mon.species == base_of_crowned(entry.species) {
                    (mon.species, mon.details_species) = (entry.species, entry.species);
                }
            }
        }
    }
}

/// A Pokémon's types as Showdown's `getTypes` gives them, from what a
/// view knows: `tera`, the type it Terastallized into (a Stellar one keeps
/// its types); else its active slot's type change (Soak, Reflect Type,
/// Transform, Protean, ...); else `species`, the types of the species it
/// shows; without Flying while it roosts (Normal if nothing is left). The
/// type Forest's Curse or Trick-or-Treat added is apart
/// ([`ActiveView::added_type`]). [`PublicTracker::types`] and the
/// encoders read types through this.
pub fn shown_types(species: TypeSet, tera: Option<Type>, active: Option<&ActiveView>) -> TypeSet {
    if let Some(t) = tera.filter(|&t| t != Type::Stellar) {
        return TypeSet::mono(t);
    }
    let types = active.and_then(|a| a.types).unwrap_or(species);
    // Roost's `onType`.
    if !active.is_some_and(|a| a.volatiles.has(PublicVolatiles::ROOST)) {
        return types;
    }
    match (types.primary, types.secondary) {
        (Type::Flying, Some(t)) | (t, Some(Type::Flying)) => TypeSet::mono(t),
        (Type::Flying, None) => TypeSet::mono(Type::Normal),
        _ => types,
    }
}

/// The types known of a Pokémon whose forme team preview hid
/// ([`MonFlags::FORME_HIDDEN`]: `Urshifu-*`): the types every forme of the
/// base species `base` has, the base's order kept, `None` past them; and
/// whether those are all its types (every forme has exactly them).
/// Showdown's Team Preview rule hides the forme in the details
/// (`onTeamPreview`, [`pikapro_protocol::preview_wildcard`]), so only what all
/// of them share is a fact until a switch-in shows the forme: Fighting of
/// Urshifu (Dark or Water), Fairy of Zacian (Crowned: Fairy/Steel), Water
/// and Dark of Greninja (exactly), nothing of Silvally.
pub fn wildcard_types(dex: &Dex, base: SpeciesId) -> ([Option<Type>; 2], bool) {
    let own = dex.species(base).types;
    let mut shared = [Some(own.primary), own.secondary];
    let mut exact = true;
    for s in dex.species.iter().filter(|s| s.base_species == base) {
        exact &= s.types == own;
        for t in &mut shared {
            if t.is_some_and(|t| !s.types.contains(t)) {
                *t = None;
            }
        }
    }
    // Keep the order, known types first.
    if shared[0].is_none() {
        shared = [shared[1], None];
    }
    (shared, exact)
}

/// The crowned formes, which the battle's start makes (and sheets show).
fn crowned(species: SpeciesId) -> bool {
    use pikapro_data::gen9::species as s;
    matches!(species, s::ZACIAN_CROWNED | s::ZAMAZENTA_CROWNED)
}

fn base_of_crowned(species: SpeciesId) -> SpeciesId {
    use pikapro_data::gen9::species as s;
    if species == s::ZAMAZENTA_CROWNED { s::ZAMAZENTA } else { s::ZACIAN }
}

/// Fills a record with what its Open Team Sheet entry shows. A team
/// preview wildcard's forme is the one the details will show (Greninja-Bond
/// shows as Greninja, `details_species`); a crowned Zacian or Zamazenta
/// is crowned only once the battle has `started`. `item`: whether the
/// sheet's item can still be the one it holds.
pub(crate) fn fill_from_sheet(dex: &Dex, mon: &mut MonView, entry: &SheetMon, started: bool, item: bool) {
    if mon.flags.has(MonFlags::FORME_HIDDEN) && dex.species(entry.species).base_species == mon.species {
        let shown = if crowned(entry.species) && !started {
            base_of_crowned(entry.species)
        } else {
            pikapro_protocol::details_species(entry.species)
        };
        mon.species = shown;
        mon.details_species = shown;
        mon.flags.set(MonFlags::FORME_HIDDEN, false);
    }
    if item && mon.item == ItemState::Unknown {
        mon.item = entry.item.map_or(ItemState::NoItem, ItemState::Known);
    }
    if mon.base_ability == AbilityState::Unknown {
        mon.base_ability = AbilityState::Known(entry.ability);
        if mon.ability == AbilityState::Unknown && !mon.flags.has(MonFlags::ACTIVE) {
            mon.ability = mon.base_ability;
        }
    }
    for &m in entry.moves.iter() {
        if mon.revealed(m).is_none() && !mon.moves.is_full() {
            mon.moves.push(RevealedMove { id: m, uses: 0, pp_spent: 0 });
        }
    }
    if mon.tera_type.is_none() {
        mon.tera_type = Some(entry.tera_type);
    }
}

/// The ability a species must have, if it has only one possible one.
pub(crate) fn single_ability(dex: &Dex, species: SpeciesId) -> Option<AbilityId> {
    let a = dex.species(species).abilities;
    let only = |x: Option<AbilityId>| x.is_none_or(|x| x == a.primary);
    (only(a.secondary) && only(a.hidden) && only(a.special)).then_some(a.primary)
}
