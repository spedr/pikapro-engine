//! [`Battle`]: the public entry point to the simulator.

use pikapro_data::{GenderRule, Stat, Stats};

use crate::choice::{Choice, ChoiceError};
use crate::event::{Actor, BattleEvent, EventLog};
use crate::format::{Format, Mechanics};
use crate::legal::{self, LegalActions};
use crate::request::Request;
use crate::resolve;
use crate::result::{BattleResult, EndReason};
use crate::rng::{BattleRng, Seed};
use crate::state::{BattleState, MoveSlot, PokemonState, Position, SideId};
use crate::stats::calc_stat;
use crate::team::{Gender, PokemonSet, Team, TeamError};
use crate::util::StackVec;

/// How a [`Battle`] runs: its seed, and what it records.
///
/// ```
/// use pikapro::{BattleOptions, Seed};
/// // Events on (the default), for reading or rendering the battle.
/// let logged = BattleOptions::new(Seed::fast(1));
/// // Events off: fastest, for training.
/// let fast = BattleOptions::new(Seed::fast(1)).without_events();
/// // Showdown's generator, every draw in the events: for conformance.
/// let traced = BattleOptions::new(Seed::ShowdownGen5([1, 2, 3, 4])).with_rng_trace();
/// # let _ = (logged, fast, traced);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BattleOptions {
    /// The seed of the battle's random stream.
    pub seed: Seed,
    /// Record [`BattleEvent`]s. Turn off for maximum throughput.
    pub log_events: bool,
    /// Also record every random draw as a [`BattleEvent::Rng`], interleaved
    /// with the other events. For conformance debugging; needs `log_events`.
    pub trace_rng: bool,
}

impl BattleOptions {
    /// The battle's random stream, tracing its draws if asked to.
    pub(crate) fn rng(&self) -> BattleRng {
        let mut rng = BattleRng::new(self.seed);
        if self.trace_rng && self.log_events {
            rng.enable_trace();
        }
        rng
    }

    /// Options with `seed`, events on and no RNG trace.
    pub fn new(seed: Seed) -> BattleOptions {
        BattleOptions { seed, log_events: true, trace_rng: false }
    }

    /// Don't record events ([`Battle::events`] stays empty). The battle
    /// plays the same.
    pub fn without_events(self) -> BattleOptions {
        BattleOptions { log_events: false, ..self }
    }

    /// Record every random draw as a [`BattleEvent::Rng`] too (with events
    /// on). The battle plays the same.
    pub fn with_rng_trace(self) -> BattleOptions {
        BattleOptions { trace_rng: true, ..self }
    }
}

/// Outcome of a successful [`Battle::submit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Stored; still waiting on the other side.
    Waiting,
    /// All pending choices were in and the battle advanced.
    Resolved,
}

/// A single battle: format, state, RNG, and the pending decision.
///
/// `Battle` is `Clone + Send + Sync` and owns no heap data besides the event
/// log, so it can be cloned for search and moved across threads freely.
///
/// # Lifecycle
///
/// 1. [`Battle::new`] validates the teams. With team preview both sides get
///    [`Request::TeamPreview`]; otherwise leads are sent out and turn 1
///    starts.
/// 2. Each side with a non-[`Request::Wait`] request calls
///    [`Battle::submit`]. When the last required choice arrives the battle
///    resolves synchronously and issues the next requests.
/// 3. Repeat until [`Battle::result`] is `Some`.
#[derive(Clone, Debug)]
pub struct Battle {
    pub(crate) format: Format,
    pub(crate) state: BattleState,
    pub(crate) rng: BattleRng,
    pub(crate) seed: Seed,
    pub(crate) requests: [Request; 2],
    pub(crate) pending: [Option<Choice>; 2],
    pub(crate) log: EventLog,
    pub(crate) result: Option<BattleResult>,
    /// Pokémon that reached 0 HP during the current action, in order.
    pub(crate) faint_queue: StackVec<resolve::FaintData, 4>,
    /// Showdown's action queue; persists across requests mid-turn.
    pub(crate) queue: resolve::Queue,
    /// A turn is in progress (Showdown's `midTurn`).
    pub(crate) mid_turn: bool,
    /// The move being used and its user (Showdown's `activeMove`).
    pub(crate) active_move: Option<(Position, pikapro_data::MoveId)>,
    /// Showdown's `battle.lastMove` and the active move's
    /// `sourceEffect` and `smartTarget`.
    pub(crate) order: resolve::OrderState,
    /// The active move's fields that abilities and items read or set
    /// (`move.ignoreAbility`, `move.infiltrates`, ...).
    pub(crate) move_state: resolve::MoveState,
    /// Every event an ability or item that has been in this battle could
    /// handle: events outside it and the conditions' registries
    /// (`events::filtered`) are skipped at once, and a Pokémon's ability
    /// and item are looked at only for events in it (`events::quiet`).
    pub(crate) event_filter: u128,
    /// Every event something in this battle could handle with an `Ally`,
    /// `Foe`, `Any` or `Source` handler (the volatiles' and every ability's
    /// and item's that has been in the battle): another Pokémon's handlers
    /// never matter to an event outside it (`events::Masks::locate_event`).
    pub(crate) prefixed_filter: u128,
    /// The event masks of a stretch of events whose handlers change
    /// nothing that has handlers (a damage calculation, a stat), computed
    /// once for all of them (see `events::with_cached_masks`): which ones
    /// (`masks_cached`), and those computed for the stretch.
    pub(crate) masks_cache: resolve::events::Masks,
    pub(crate) masks_cached: resolve::events::Cached,
    /// Counts changes to the side, slot and pseudo-weather conditions,
    /// the weather and the terrain (`conditions_changed`), for
    /// `masks_memo`.
    pub(crate) conditions_version: u32,
    /// The event masks last computed and what they were computed from
    /// (`events::Masks::of` reuses them while that is unchanged);
    /// `MasksMemo::EMPTY` (never valid) before the first.
    pub(crate) masks_memo: resolve::events::MasksMemo,
    /// Counts the handler lists of two or more that events have sorted
    /// (`events::run_located`): a computation that leaves it unchanged
    /// sorted nothing, so it drew nothing and didn't depend on the
    /// holders' stored speeds (the Gen 8 re-sort's speed memo reads it).
    pub(crate) handler_sorts: u32,
    /// Counts the handlers `events::call` has run: between two equal
    /// counts no handler changed anything (`events::each_event_run`).
    pub(crate) handler_calls: u32,
    /// The active slots (bit `2 * side + slot`) whose stored speed is what
    /// computing it again would give, without a draw: set while choices are
    /// resolved right after `update_speed` (see `resolve::with_fresh_speeds`),
    /// 0 otherwise.
    pub(crate) fresh_speeds: u8,
    /// Legal actions for each side's current request, computed once per
    /// request (see [`Battle::refresh_legal`]).
    pub(crate) legal: [LegalActions; 2],
}

impl Battle {
    /// Starts a battle between `teams[0]` (player 1, [`SideId::P1`]) and
    /// `teams[1]` (player 2), as Showdown's `new Battle` with both players
    /// set.
    ///
    /// Each team is checked with [`Team::validate`] (the format's size,
    /// clauses and stat system, known IDs). That is not a legality check:
    /// `pikapro-validate` ports Showdown's team validator. A set without
    /// a gender gets the species' fixed one or, for a species that can be
    /// either, a random draw from the battle's stream, as Showdown does.
    ///
    /// The teams are played as given, as Showdown's simulator plays the
    /// teams it is handed (its `BattleStream`, `simulate-battle`): a set
    /// that lists a battle-only forme (`Floette-Mega @ Floettite`) is that
    /// forme from the start, and doesn't Mega Evolve. A Showdown server
    /// validates a team first, and its validator starts such a set in the
    /// forme it changes from (Floette-Eternal, which Mega Evolves in
    /// battle); to play a team as a server would, play the team
    /// `pikapro-validate` returns (`validate_preset`, `check_preset`).
    ///
    /// With [`Format::team_preview`] both sides are then asked for
    /// [`Request::TeamPreview`]; otherwise the first Pokémon of each team
    /// are sent out and turn 1 starts ([`Request::Move`]).
    ///
    /// # Errors
    ///
    /// The first [`TeamError`] of player 1's team, then of player 2's.
    pub fn new(format: Format, teams: [Team; 2], options: BattleOptions) -> Result<Battle, TeamError> {
        for team in &teams {
            team.validate(&format)?;
        }
        let mut battle = Battle {
            rng: options.rng(),
            seed: options.seed,
            state: BattleState::default(),
            requests: [Request::Wait; 2],
            pending: [None; 2],
            log: EventLog::new(options.log_events),
            result: None,
            faint_queue: StackVec::new(),
            queue: resolve::Queue::default(),
            mid_turn: false,
            active_move: None,
            order: resolve::OrderState::default(),
            move_state: resolve::MoveState::default(),
            event_filter: 0,
            prefixed_filter: 0,
            masks_cache: resolve::events::Masks::NONE,
            masks_cached: resolve::events::Cached::No,
            conditions_version: 0,
            masks_memo: resolve::events::MasksMemo::EMPTY,
            handler_sorts: 0,
            handler_calls: 0,
            fresh_speeds: 0,
            legal: [LegalActions::default(); 2],
            format,
        };
        // Showdown builds p1's Pokémon, then p2's (`setPlayer`), drawing a
        // gender for each set that has none; each player's
        // `|player|` line follows its draws.
        for (side, team) in SideId::BOTH.into_iter().zip(&teams) {
            // Into the default side built in place (no second side to
            // build and copy in).
            let built = &mut battle.state.side_mut(side).team;
            for (id, set) in team.sets.iter().enumerate() {
                built.push(build_pokemon(&battle.format, set, id as u8, &mut battle.rng));
            }
            battle.emit(BattleEvent::Player { side });
        }
        (battle.event_filter, battle.prefixed_filter) = resolve::events::filters(&battle);
        for side in SideId::BOTH {
            for i in 0..battle.state.side(side).team.len() {
                let p = battle.state.side(side).team[i];
                battle.emit(BattleEvent::Poke { side, species: p.species, level: p.level, gender: p.gender });
            }
        }
        let preview = battle.format.team_preview;
        if preview.shown() {
            battle.emit(BattleEvent::TeamPreview { bring: preview.picked() });
            // The picked team size, or without one the side's whole team.
            battle.requests = SideId::BOTH.map(|side| {
                let bring = preview.picked().unwrap_or(battle.state.side(side).team.len() as u8);
                Request::TeamPreview { bring }
            });
        }
        resolve::start(&mut battle);
        battle.refresh_legal();
        Ok(battle)
    }

    /// The format the battle is played in.
    pub fn format(&self) -> &Format {
        &self.format
    }

    /// A side condition, slot condition, pseudo-weather, weather or
    /// terrain started or ended: the memoised event masks are stale.
    /// Everything that sets `field.weather` or `field.terrain` calls it.
    #[inline]
    pub(crate) fn conditions_changed(&mut self) {
        self.conditions_version = self.conditions_version.wrapping_add(1);
    }

    /// The whole battle state (both sides, hidden information included).
    /// `Clone`/`Copy` it for a snapshot.
    pub fn state(&self) -> &BattleState {
        &self.state
    }

    /// The seed of the battle's current random stream: the one it was
    /// created with ([`BattleOptions::seed`]) or, for a [`Battle::fork`],
    /// the fork's. Replaying a battle takes its format, teams, choices and
    /// this seed; replaying a fork takes the original's seed up to the fork
    /// and this one after it.
    pub fn seed(&self) -> Seed {
        self.seed
    }

    /// The current turn: 0 before the first (team preview), then 1, 2, ...
    /// After a tie at the turn limit it is one past the limit, and after
    /// the Endless Battle Clause one past the last turn played (see
    /// [`BattleResult::turns`]).
    pub fn turn(&self) -> u16 {
        self.state.turn
    }

    /// How the battle ended, once it has.
    pub fn result(&self) -> Option<BattleResult> {
        self.result
    }

    /// Whether the battle has ended ([`Battle::result`] is `Some`).
    pub fn is_over(&self) -> bool {
        self.result.is_some()
    }

    /// What `side` must decide now. [`Request::Wait`] after it has submitted,
    /// or when all its move slots pass automatically (fainted, commanding or empty).
    pub fn request(&self, side: SideId) -> Request {
        self.legal[side.index()].request()
    }

    /// Computed only when refreshing the legal-action cache. Between
    /// resolutions, submitting a choice marks that cache as waiting.
    pub(crate) fn next_request(&self, side: SideId) -> Request {
        if self.pending[side.index()].is_some() || self.automatic_move_choice(side) {
            Request::Wait
        } else {
            self.requests[side.index()]
        }
    }

    /// `side.isChoiceDone` / `getChoiceIndex` fill every slot with a pass.
    fn automatic_move_choice(&self, side: SideId) -> bool {
        self.requests[side.index()] == Request::Move
            && (0..self.format.active_per_side()).all(|slot| {
                self.state
                    .side(side)
                    .active(slot)
                    .is_none_or(|m| m.is_fainted() || m.volatiles.has(crate::state::Volatiles::COMMANDING))
            })
    }

    /// The legal actions for `side`'s current request. Computed once when
    /// the request is issued; this is a cheap borrow.
    pub fn legal_actions(&self, side: SideId) -> &LegalActions {
        &self.legal[side.index()]
    }

    /// Recomputes the cached legal actions after the requests changed.
    fn refresh_legal(&mut self) {
        // Filled in a local pair and copied in once (returned by value,
        // each side's would be copied out of `legal_actions` and again into
        // place).
        let mut legal = [LegalActions::default(); 2];
        for side in SideId::BOTH {
            legal::fill_legal_actions(self, side, &mut legal[side.index()]);
        }
        self.legal = legal;
    }

    /// Submits `side`'s answer to its current request.
    ///
    /// A replacement clears the side's previous choice before validation,
    /// even if the replacement is invalid, as Showdown's `clearChoice` does.
    /// When this is the last outstanding choice, the battle resolves before returning.
    /// A move request whose slots all pass automatically still accepts an
    /// explicit all-pass choice, as Showdown does, while reporting [`Request::Wait`].
    pub fn submit(&mut self, side: SideId, choice: Choice) -> Result<Progress, ChoiceError> {
        if self.result.is_some() {
            return Err(ChoiceError::BattleOver);
        }
        let i = side.index();
        if self.requests[i] == Request::Wait {
            return Err(ChoiceError::NotRequested);
        }
        self.clear_choice(side);
        if self.automatic_move_choice(side) {
            // `side.choose` clears the automatically completed choice and
            // accepts passes again. They need no storage: `allChoicesDone`
            // fills them when the opponent submits, including on later turns.
            let Choice::Actions(actions) = choice else { return Err(ChoiceError::WrongKind) };
            if let Some(slot) = actions.iter().position(|a| *a != crate::choice::SlotAction::Pass) {
                return Err(ChoiceError::IllegalAction { slot: slot as u8 });
            }
            return Ok(Progress::Waiting);
        }
        self.legal[i].check(&choice)?;
        self.pending[i] = Some(choice);

        // `allChoicesDone` calls each side's `isChoiceDone`, whose
        // `getChoiceIndex` fills passes for fainted and commanding slots.
        // A side consisting entirely of those slots needs no submission.
        for side in SideId::BOTH {
            let i = side.index();
            if self.pending[i].is_none() && self.automatic_move_choice(side) {
                self.pending[i] = Some(Choice::Actions([crate::choice::SlotAction::Pass; crate::state::MAX_ACTIVE]));
            }
        }
        let ready =
            SideId::BOTH.iter().all(|s| self.requests[s.index()] == Request::Wait || self.pending[s.index()].is_some());
        if !ready {
            // `legal::legal_actions` for a side that waits.
            self.legal[i].set_waiting();
            return Ok(Progress::Waiting);
        }
        let choices = [self.pending[0].take(), self.pending[1].take()];
        let requests = core::mem::replace(&mut self.requests, [Request::Wait; 2]);
        self.log.mark_sent();
        resolve::resolve(self, requests, choices);
        self.flush_rng_trace();
        self.refresh_legal();
        Ok(Progress::Resolved)
    }

    /// Clears an answer to the current request before reading a replacement.
    /// Text clients call this before parsing too: a rejected replacement must
    /// not leave the old answer queued. The issued request remains active.
    pub fn clear_choice(&mut self, side: SideId) {
        if self.pending[side.index()].take().is_some() {
            self.legal[side.index()] = legal::issued_actions(self, side);
        }
    }

    /// Turns the event log on or off from now on (the events logged so far
    /// stay). Logging never changes the battle: a copy played only for
    /// its outcome can turn it off.
    pub fn set_logging(&mut self, on: bool) {
        self.log.set_enabled(on);
    }

    /// Ends the battle immediately with `side` losing.
    pub fn forfeit(&mut self, side: SideId) {
        if self.result.is_none() {
            resolve::end(self, Some(side.foe()), EndReason::Forfeit);
            self.refresh_legal();
        }
    }

    /// A copy of this battle whose future randomness comes from `seed`.
    /// Useful for determinising rollouts in search. ([`Clone`] gives a copy
    /// that continues the same stream.)
    ///
    /// The copy keeps everything else: the state, the requests and any
    /// choice already submitted for them, the events so far, and the RNG
    /// trace setting. Its [`seed`](Battle::seed) is `seed`.
    pub fn fork(&self, seed: Seed) -> Battle {
        let mut rng = BattleRng::new(seed);
        if self.rng.is_tracing() {
            rng.enable_trace();
        }
        // Field by field, so each field is written straight into the
        // returned battle. `Battle { rng, seed, ..self.clone() }` builds a
        // whole clone first and moves it, a second copy of the battle
        // (about 8 KB) whenever the clone isn't inlined here, which thin
        // LTO decides per build. No `..`: a new field must be listed.
        let Battle {
            format,
            state,
            rng: _,
            seed: _,
            requests,
            pending,
            log,
            result,
            faint_queue,
            queue,
            mid_turn,
            active_move,
            order,
            move_state,
            event_filter,
            prefixed_filter,
            masks_cache,
            masks_cached,
            conditions_version,
            masks_memo,
            handler_sorts,
            handler_calls,
            fresh_speeds,
            legal,
        } = self;
        Battle {
            format: format.clone(),
            state: *state,
            rng,
            seed,
            requests: *requests,
            pending: *pending,
            log: log.clone(),
            result: *result,
            faint_queue: *faint_queue,
            queue: *queue,
            mid_turn: *mid_turn,
            active_move: *active_move,
            order: *order,
            move_state: *move_state,
            event_filter: *event_filter,
            prefixed_filter: *prefixed_filter,
            masks_cache: *masks_cache,
            masks_cached: *masks_cached,
            conditions_version: *conditions_version,
            masks_memo: *masks_memo,
            handler_sorts: *handler_sorts,
            handler_calls: *handler_calls,
            fresh_speeds: *fresh_speeds,
            legal: *legal,
        }
    }

    /// Appends an event, first flushing any traced RNG draws so they appear
    /// where they happened.
    #[inline]
    pub(crate) fn emit(&mut self, event: BattleEvent) {
        self.flush_rng_trace();
        self.log.push(event);
    }

    /// What [`Battle::emit`] does with the event log off (the event is
    /// dropped): for a caller that doesn't make the event then.
    #[inline(always)]
    pub(crate) fn emit_without_log(&mut self) {
        debug_assert!(!self.log.is_enabled());
        self.flush_rng_trace();
    }

    /// Emits the event `make` builds for `actor`, made only for the log
    /// (`log_actor`): with the log off (`None`), what [`Battle::emit`]
    /// does then.
    #[inline(always)]
    pub(crate) fn emit_for(&mut self, actor: Option<Actor>, make: impl FnOnce(Actor) -> BattleEvent) {
        match actor {
            Some(actor) => self.emit(make(actor)),
            None => self.emit_without_log(),
        }
    }

    /// Showdown's `battle.hint` (`|-hint|`).
    pub(crate) fn hint(&mut self, hint: crate::event::Hint) {
        self.emit(BattleEvent::Hint { hint });
    }

    /// Showdown's `attrLastMove`. `[still]` also clears the target
    /// of the last `|move|` event, as it blanks the line's target.
    pub(crate) fn attr_last_move(&mut self, attr: crate::event::MoveAttr) {
        if !self.log.is_enabled() {
            return;
        }
        self.flush_rng_trace();
        if attr == crate::event::MoveAttr::Still {
            self.log.clear_last_move_target();
        }
        self.log.push(BattleEvent::MoveAttr { attr });
    }

    #[inline(always)]
    pub(crate) fn flush_rng_trace(&mut self) {
        if self.rng.is_tracing() {
            self.flush_traced_draws();
        }
    }

    /// [`Battle::flush_rng_trace`] while tracing (out of line: the test is
    /// what every `emit` runs).
    #[inline(never)]
    fn flush_traced_draws(&mut self) {
        for (bound, result) in self.rng.take_trace() {
            self.log.push(BattleEvent::Rng { bound, result });
        }
    }

    /// The events recorded so far (since the last
    /// [`drain_events`](Battle::drain_events)), oldest first; empty with
    /// [`BattleOptions::without_events`]. `pikapro-protocol` renders them
    /// as Showdown's log lines.
    pub fn events(&self) -> &[BattleEvent] {
        self.log.events()
    }

    /// Removes and returns buffered events (e.g. to stream them per turn).
    pub fn drain_events(&mut self) -> std::vec::Drain<'_, BattleEvent> {
        self.log.drain()
    }
}

/// The `Pokemon` constructor's `genders[set.gender] ||
/// this.species.gender || this.battle.sample(['M', 'F'])` (`N` becoming
/// genderless). The draw ignores the species' gender ratio.
fn gender(format: &Format, set: &PokemonSet, rng: &mut BattleRng) -> Gender {
    if let Some(g) = set.gender {
        return g;
    }
    fixed_gender(format.dex.species(set.species).gender).unwrap_or_else(|| *rng.sample(&[Gender::Male, Gender::Female]))
}

/// The species' only gender (`species.gender`), if it has one.
pub(crate) fn fixed_gender(rule: GenderRule) -> Option<Gender> {
    match rule {
        GenderRule::Male => Some(Gender::Male),
        GenderRule::Female => Some(Gender::Female),
        GenderRule::Genderless => Some(Gender::Genderless),
        GenderRule::Ratio { .. } => None,
    }
}

/// A set's Pokémon as the battle starts with it (Showdown's `Pokemon`
/// constructor), drawing its gender if the set and the species give none.
/// Always inlined into [`Battle::new`], its one caller.
#[inline(always)]
fn build_pokemon(format: &Format, set: &PokemonSet, id: u8, rng: &mut BattleRng) -> PokemonState {
    let gender = gender(format, set, rng);
    let dex = format.dex;
    let species = dex.species(set.species);
    let level = format.level.apply(set.level);
    let (evs, ivs) = set.investment.effective();
    let mut stats = Stats::splat(0u16);
    for s in Stat::ALL {
        stats[s] = calc_stat(s, species.base_stats[s], ivs[s], evs[s], level, set.nature);
    }
    let moves = set
        .moves
        .iter()
        .map(|&id| {
            let pp = move_max_pp(format.mechanics, dex.move_(id));
            MoveSlot { id, pp, max_pp: pp, disabled: false, target: None }
        })
        .collect();
    // Arceus's type is its Plate's with Multitype.
    let types = crate::resolve::abilities::switchin::multitype_types(format, set).unwrap_or(species.types);
    PokemonState {
        id,
        species: set.species,
        level,
        base_types: types,
        species_types: species.types,
        types,
        // The simulator's `this.set.teraType || this.types[0]` (the
        // validator fills in a forced one: pikapro-validate's team).
        tera_type: set.tera_type.unwrap_or(species.types.primary),
        tera_blocked: crate::resolve::tera::blocked_at_start(format, set),
        ability: set.ability,
        base_ability: set.ability,
        set_ability: set.ability,
        base_species: set.species,
        set_species: set.species,
        item: set.item,
        hp: stats[Stat::Hp],
        // Showdown initialises the stored speed to the raw Speed stat.
        speed: stats[Stat::Spe] as i32,
        stats,
        set_stats: stats,
        evs,
        ivs,
        nature: set.nature,
        moves,
        // `apparentType` starts as the forme's own types (an Arceus of
        // another type reveals it at the end of a turn).
        volatile_counters: crate::state::VolatileCounters {
            apparent_types: (types != species.types).then_some(species.types),
            ..Default::default()
        },
        // `canMegaEvo`, from the set's species and item.
        can_mega_evo: crate::resolve::mega::can_mega_evo(format, set.species, set.item),
        // (What the Endless Battle Clause reads of the set: only worked
        // out in a format that has the clause.)
        endless: if format.endless_battle_clause {
            crate::resolve::endless::of_set(set)
        } else {
            crate::state::EndlessState::default()
        },
        gender,
        ..PokemonState::default()
    }
}

/// A move's maximum PP in a set under `mechanics` (Showdown's
/// `calculatePP` with every PP Up): moves with `noPPBoosts` (Struggle,
/// Revival Blessing) keep exactly their base PP; the others get
/// `pp * 8 / 5` in Scarlet/Violet and, in Showdown's `champions` mod,
/// `(min(pp, 20) / 5 + 1) * 4`.
pub fn move_max_pp(mechanics: Mechanics, data: &pikapro_data::MoveData) -> u8 {
    if data.unmodelled.contains(&"noPPBoosts") { data.pp } else { boosted_pp(mechanics, data.pp) }
}

/// Maximum PP for base PP `base` with every PP Up, which sets always get.
/// It doesn't know the move: [`move_max_pp`] also keeps `noPPBoosts`
/// moves at their base PP.
pub(crate) fn boosted_pp(mechanics: Mechanics, base: u8) -> u8 {
    match mechanics {
        Mechanics::ScarletViolet => (base as u16 * 8 / 5) as u8,
        // Showdown's `champions` mod: base PP is capped at 20, then
        // `(pp / 5 + 1) * 4`.
        Mechanics::Champions => (base.min(20) / 5 + 1) * 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}

    #[test]
    fn battle_is_send_sync_clone() {
        assert_send_sync::<Battle>();
    }

    #[test]
    fn automatic_passes_require_an_issued_move_request() {
        use crate::choice::SlotAction;
        use crate::state::Volatiles;
        use pikapro_data::gen9::{moves as m, species as s};

        let team = Team::new(vec![PokemonSet::new(s::MEW).with_moves(&[m::SPLASH]); 2]);
        let mut format = Format::gen9_doubles();
        format.team_preview = crate::format::TeamPreview::None;
        let battle = Battle::new(format, [team.clone(), team], BattleOptions::new(Seed::fast(1))).unwrap();
        let pass = Choice::Actions([SlotAction::Pass; 2]);
        // Every combination of empty, fainted and commanding slots.
        for side in SideId::BOTH {
            for slots in [[0, 0], [0, 1], [0, 2], [1, 0], [1, 1], [1, 2], [2, 0], [2, 1], [2, 2]] {
                let mut b = battle.clone();
                for (slot, kind) in slots.into_iter().enumerate() {
                    let s = b.state.side_mut(side);
                    match kind {
                        0 => s.active[slot] = None,
                        1 => s.team[slot].hp = 0,
                        _ => s.team[slot].volatiles.set(Volatiles::COMMANDING),
                    }
                }
                b.refresh_legal();
                assert_eq!(b.request(side), Request::Wait);
                assert!(b.legal_actions(side).joint().is_empty());
                let before = b.state;
                assert_eq!(b.submit(side, pass), Ok(Progress::Waiting));
                assert_eq!(b.submit(side, pass), Ok(Progress::Waiting));
                assert_eq!(b.state, before);
                assert_eq!(b.pending[side.index()], None);

                // The compatibility path must not accept a real wait,
                // a required replacement, or a choice after battle end.
                b.requests[side.index()] = Request::Wait;
                b.refresh_legal();
                assert_eq!(b.submit(side, pass), Err(ChoiceError::NotRequested));
                b.requests[side.index()] = Request::ForceSwitch { slots: [true; 2] };
                b.state.side_mut(side).team[0].hp = 1;
                b.state.side_mut(side).active = [None; 2];
                b.refresh_legal();
                assert!(b.submit(side, pass).is_err());
                b.forfeit(side);
                assert_eq!(b.submit(side, pass), Err(ChoiceError::BattleOver));
            }
        }
    }

    #[test]
    fn a_side_with_only_fainted_or_commanding_slots_needs_no_submission() {
        use crate::choice::{SlotAction, Target};
        use crate::format::TeamPreview;
        use crate::state::Volatiles;
        use pikapro_data::gen9::{abilities as a, moves as m, species as s};

        for preset in Format::presets().iter().filter(|f| f.active_per_side() == 2) {
            for automatic_side in SideId::BOTH {
                for commander_slot in 0..2 {
                    for submits_first in SideId::BOTH {
                        let mut format = preset.clone();
                        format.team_preview = TeamPreview::None;
                        format.team_size = (1, 6);
                        let stat_system = format.stat_system;
                        let set = |species, ability, moves: &[_]| {
                            let mut set = PokemonSet::new(species).with_ability(ability).with_moves(moves);
                            if stat_system == crate::format::StatSystem::StatPoints {
                                set.investment = crate::Investment::StatPoints(Stats::splat(0));
                            }
                            set
                        };
                        let bystander = || set(s::MEW, a::NO_GUARD, &[m::SPLASH]);
                        let dondozo = set(s::DONDOZO, a::UNAWARE, &[m::DOODLE, m::MEMENTO]);
                        let tatsugiri = set(s::TATSUGIRI, a::COMMANDER, &[m::SPLASH]);
                        let pair =
                            if commander_slot == 0 { vec![tatsugiri, dondozo] } else { vec![dondozo, tatsugiri] };
                        let teams = if automatic_side == SideId::P2 {
                            [Team::new(vec![bystander()]), Team::new(pair)]
                        } else {
                            [Team::new(pair), Team::new(vec![bystander()])]
                        };
                        let mut b = Battle::new(format, teams, BattleOptions::new(Seed::fast(57_002_000))).unwrap();
                        let p1 = Choice::single(SlotAction::Move { index: 0, target: None, gimmick: None });
                        let p2 = |index| {
                            let mut actions = [SlotAction::Pass; crate::state::MAX_ACTIVE];
                            actions[1 - commander_slot] =
                                SlotAction::Move { index, target: Some(Target::Foe(0)), gimmick: None };
                            Choice::Actions(actions)
                        };
                        for index in [0, 1] {
                            b.submit(automatic_side.foe(), p1).unwrap();
                            assert_eq!(b.submit(automatic_side, p2(index)), Ok(Progress::Resolved));
                        }
                        // Doodle replaced Commander with No Guard; Memento
                        // fainted Dondozo, leaving Tatsugiri inside it.
                        let survivor = &b.state.side(automatic_side).team[commander_slot];
                        assert_eq!(survivor.ability, a::NO_GUARD);
                        assert!(survivor.volatiles.has(Volatiles::COMMANDING));
                        assert!(b.state.side(automatic_side).team[1 - commander_slot].is_fainted());
                        assert_eq!(b.request(automatic_side), Request::Wait);
                        assert_eq!(b.legal_actions(automatic_side).request(), Request::Wait);
                        // Showdown still sends `active`, with no noCancel; only
                        // the decision API reports that no submission is needed.
                        let data = b.request_data(automatic_side);
                        assert!(matches!(data.kind, crate::request::RequestKind::Move { .. }));
                        assert!(data.answered);
                        assert!(!data.no_cancel);
                        assert!(b.legal_actions(automatic_side).joint().is_empty());
                        assert_eq!(b.submit(automatic_side, Choice::team(&[0, 1])), Err(ChoiceError::WrongKind));
                        for slot in 0..2 {
                            for action in
                                [SlotAction::Switch(0), SlotAction::Move { index: 0, target: None, gimmick: None }]
                            {
                                let mut actions = [SlotAction::Pass; 2];
                                actions[slot] = action;
                                assert_eq!(
                                    b.submit(automatic_side, Choice::Actions(actions)),
                                    Err(ChoiceError::IllegalAction { slot: slot as u8 })
                                );
                            }
                        }
                        let turn = b.state.turn;
                        let pass = Choice::Actions([SlotAction::Pass; 2]);
                        // A redundant pass is accepted, but never becomes a
                        // new decision or changes the simulation or its RNG.
                        let mut without_pass = b.clone();
                        if submits_first == automatic_side {
                            assert_eq!(b.submit(automatic_side, pass), Ok(Progress::Waiting));
                            assert_eq!(b.submit(automatic_side, pass), Ok(Progress::Waiting));
                            assert_eq!(b.state.turn, turn);
                        }
                        assert_eq!(
                            b.submit(automatic_side.foe(), p1),
                            Ok(Progress::Resolved),
                            "{} slot {commander_slot}",
                            preset.id
                        );
                        assert_eq!(without_pass.submit(automatic_side.foe(), p1), Ok(Progress::Resolved));
                        assert_eq!(b.state, without_pass.state);
                        assert_eq!(b.events(), without_pass.events());
                        assert_eq!(b.rng.next_u32(), without_pass.rng.next_u32());
                        assert_eq!(b.state.turn, turn + 1);
                        assert_eq!(b.request(automatic_side), Request::Wait);
                        assert_eq!(b.submit(automatic_side, pass), Ok(Progress::Waiting));
                        assert_eq!(b.state.turn, turn + 1, "no stale action can enter the next turn");
                        assert_eq!(b.pending[automatic_side.index()], None);
                        assert_eq!(b.request(automatic_side), Request::Wait);
                        assert!(b.legal_actions(automatic_side).joint().is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn pp_rules() {
        let (sv, ch) = (Mechanics::ScarletViolet, Mechanics::Champions);
        assert_eq!([35, 15, 10, 5].map(|pp| boosted_pp(sv, pp)), [56, 24, 16, 8]);
        assert_eq!([35, 15, 10, 5].map(|pp| boosted_pp(ch, pp)), [20, 16, 12, 8]);
    }
}
