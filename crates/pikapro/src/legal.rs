//! Legal action generation and choice validation.
//!
//! [`LegalActions`] lists, per active slot, every [`SlotAction`] that is legal
//! *on its own*. A few rules couple the slots of a side (no two switches to
//! the same Pokémon, each gimmick once per turn, forced switches must be filled);
//! [`LegalActions::is_legal`] checks those, and [`LegalActions::sample`] /
//! [`LegalActions::joint`] only produce jointly legal choices.

use pikapro_data::MoveTarget;

use crate::battle::Battle;
use crate::choice::{Choice, ChoiceError, SlotAction, Target, TeamOrder};
use crate::format::{Gimmick, Gimmicks};
use crate::request::Request;
use crate::resolve;
use crate::rng::BattleRng;
use crate::state::{MAX_ACTIVE, MAX_MOVES, MAX_TEAM, Position, SideId, SideState, SlotCondition, Volatiles};
use crate::util::StackVec;

/// Upper bound on per-slot actions: 4 moves x 3 targets x 2 (gimmick or not)
/// + 5 switches, rounded up.
pub const MAX_SLOT_ACTIONS: usize = 32;

/// One slot's legal actions ([`LegalActions::slot`]).
pub type SlotActions = StackVec<SlotAction, MAX_SLOT_ACTIONS>;

/// The legal actions for one side's current request.
#[derive(Clone, Copy, Debug, Default)]
pub struct LegalActions {
    request: Request,
    slots: [SlotActions; MAX_ACTIVE],
    active: u8,
    team_len: u8,
    /// Switch targets available (for forced-switch coverage).
    bench: u8,
    /// Per slot and move slot, the targets Showdown's `chooseMove`
    /// also takes for a move the lists offer with a target (doubles): any
    /// location `validTargetLoc` allows, occupied or not, and none at all
    /// (`autoChoose`'s `targetLoc` 0, a random target). Bit 0: no target;
    /// bits 1 and 2: foe slots; bits 3 and 4: own slots.
    extra_targets: [[u8; MAX_MOVES]; MAX_ACTIVE],
}

/// The [`LegalActions::extra_targets`] bit of a target; 0 (never
/// accepted) for a slot beyond the second.
fn target_bit(target: Option<Target>) -> u8 {
    match target {
        None => 1,
        Some(Target::Foe(n)) if n <= 1 => 2 << n,
        Some(Target::Ally(n)) if n <= 1 => 8 << n,
        Some(_) => 0,
    }
}

impl LegalActions {
    /// The actions a player lists from what it knows without the battle
    /// (its request: a client, such as `pikapro-rl`'s channel session): the
    /// request, each active slot's actions, the active slots, the team's
    /// size and the Pokémon that can switch in. [`LegalActions::check`]
    /// then takes the listed actions only (none of the extra targets
    /// Showdown also accepts).
    ///
    /// The inputs may come from an untrusted request, so they are made
    /// consistent rather than trusted:
    ///
    /// * `active` is capped at [`MAX_ACTIVE`] and `team_len` at
    ///   [`MAX_TEAM`];
    /// * a switch to a position outside the team (`Switch(i)` with `i >=
    ///   team_len`) is dropped;
    /// * an active slot of a move or switch request left with no action
    ///   gets [`SlotAction::Pass`] (a request whose every move is disabled
    ///   for a trapped Pokémon; Showdown itself would offer Struggle);
    /// * slots past `active` are emptied, and `bench` is capped at the
    ///   number of Pokémon the lists can switch in.
    pub fn from_slots(
        request: Request,
        mut slots: [SlotActions; MAX_ACTIVE],
        active: u8,
        team_len: u8,
        bench: u8,
    ) -> LegalActions {
        let active = active.min(MAX_ACTIVE as u8);
        let team_len = team_len.min(MAX_TEAM as u8);
        let acting = matches!(request, Request::Move | Request::ForceSwitch { .. });
        let mut targets = 0u8;
        for (i, slot) in slots.iter_mut().enumerate() {
            if i >= active as usize {
                slot.clear();
                continue;
            }
            slot.retain(|a| !matches!(*a, SlotAction::Switch(t) if t >= team_len));
            for a in slot.iter() {
                if let SlotAction::Switch(t) = *a {
                    targets |= 1 << t;
                }
            }
            if acting && slot.is_empty() {
                slot.push(SlotAction::Pass);
            }
        }
        let bench = bench.min(targets.count_ones() as u8);
        LegalActions { request, slots, active, team_len, bench, extra_targets: [[0; MAX_MOVES]; MAX_ACTIVE] }
    }

    /// The request these actions answer ([`Request::Wait`] once the side
    /// has submitted).
    pub fn request(&self) -> Request {
        self.request
    }

    /// The side's team size (the Pokémon a team preview order picks from).
    pub fn team_len(&self) -> usize {
        self.team_len as usize
    }

    /// The side's Pokémon that can switch in (benched, not fainted).
    pub fn bench(&self) -> usize {
        self.bench as usize
    }

    /// True when the side has nothing to decide.
    pub fn is_waiting(&self) -> bool {
        self.request == Request::Wait
    }

    /// Individually legal actions for `slot`. Empty for team preview, and
    /// for a slot the format doesn't have (past
    /// [`active_per_side`](crate::Format::active_per_side)).
    pub fn slot(&self, slot: usize) -> &[SlotAction] {
        self.slots.get(slot).map_or(&[], |s| &s[..])
    }

    /// Whether `choice` answers the request legally, including cross-slot rules.
    pub fn is_legal(&self, choice: &Choice) -> bool {
        self.check(choice).is_ok()
    }

    /// Validates `choice`. Returns the first rule it breaks.
    ///
    /// Besides the listed actions, this accepts what Showdown's
    /// `chooseMove` does for a move the lists offer with targets: another
    /// target location `validTargetLoc` allows (an empty or fainted slot),
    /// or no target (`default`: a random one). The lists keep one action
    /// per target that is there, for sampling and enumeration.
    pub fn check(&self, choice: &Choice) -> Result<(), ChoiceError> {
        match (self.request, choice) {
            (Request::Wait, _) => Err(ChoiceError::NotRequested),
            (Request::TeamPreview { bring }, Choice::Team(order)) => {
                check_team_order(order, bring.min(self.team_len), self.team_len)
            }
            (Request::Move | Request::ForceSwitch { .. }, Choice::Actions(actions)) => {
                for (slot, action) in actions.iter().enumerate() {
                    let ok = if slot < self.active as usize {
                        listed(&self.slots[slot], action) || self.extra_target(slot, action)
                    } else {
                        *action == SlotAction::Pass
                    };
                    if !ok {
                        return Err(ChoiceError::IllegalAction { slot: slot as u8 });
                    }
                }
                self.check_joint(actions)
            }
            _ => Err(ChoiceError::WrongKind),
        }
    }

    /// `action` aims a listed move (with the same gimmick) at a
    /// target Showdown also accepts.
    fn extra_target(&self, slot: usize, action: &SlotAction) -> bool {
        let SlotAction::Move { index, target, gimmick } = *action else { return false };
        let accepted = self.extra_targets[slot].get(index as usize).is_some_and(|&bits| bits & target_bit(target) != 0);
        accepted
            && self.slots[slot]
                .iter()
                .any(|a| matches!(*a, SlotAction::Move { index: i, gimmick: g, .. } if i == index && g == gimmick))
    }

    fn check_joint(&self, actions: &[SlotAction; MAX_ACTIVE]) -> Result<(), ChoiceError> {
        // Two slots (`MAX_ACTIVE`): the rules compare them directly.
        let [a, b] = *actions;
        // (Compared, not a bit mask: any position is safe.)
        if let (SlotAction::Switch(i), SlotAction::Switch(j)) = (a, b)
            && i == j
        {
            return Err(ChoiceError::DuplicateSwitch);
        }
        // Side.chooseMove keeps separate mega/terastallize flags. A
        // supplied Gen 9 stone permits one of each in the same turn.
        let gimmick = |x: SlotAction| match x {
            SlotAction::Move { gimmick, .. } => gimmick,
            _ => None,
        };
        if let (Some(x), Some(y)) = (gimmick(a), gimmick(b))
            && x == y
        {
            return Err(ChoiceError::MultipleGimmicks);
        }
        if let Request::ForceSwitch { slots } = self.request {
            // Every forced slot must switch while replacements remain.
            let switch = |x: SlotAction| matches!(x, SlotAction::Switch(_)) as u8;
            let switches = switch(a) + switch(b);
            let forced = slots[0] as u8 + slots[1] as u8;
            if switches < forced.min(self.bench) {
                let slot = (0..MAX_ACTIVE).find(|&s| slots[s] && actions[s] == SlotAction::Pass);
                return Err(ChoiceError::MustAct { slot: slot.unwrap_or(0) as u8 });
            }
        }
        Ok(())
    }

    /// A uniformly random jointly legal choice. Uses the caller's RNG, never
    /// the battle's, so sampling does not perturb the simulation.
    ///
    /// # Panics
    /// If the side is waiting, or if no choice is jointly legal. The
    /// engine's lists ([`Battle::legal_actions`]) always have one; lists
    /// given to [`LegalActions::from_slots`] can lack one only if they
    /// contradict each other (both slots of a doubles request listing only
    /// a switch to the same Pokémon). [`LegalActions::joint`] is empty
    /// then.
    pub fn sample(&self, rng: &mut BattleRng) -> Choice {
        match self.request {
            Request::Wait => panic!("sample() called for a side that is waiting"),
            Request::TeamPreview { bring } => {
                let mut order: TeamOrder = (0..self.team_len).collect();
                rng.shuffle(&mut order);
                order.truncate(bring.min(self.team_len) as usize);
                Choice::Team(order)
            }
            Request::Move | Request::ForceSwitch { .. } => {
                // Rejection sampling over the product of per-slot lists is
                // uniform over jointly legal choices and almost always
                // succeeds first try; after 64 misses, enumerate.
                for _ in 0..64 {
                    let mut actions = [SlotAction::Pass; MAX_ACTIVE];
                    for (slot, a) in actions.iter_mut().enumerate().take(self.active as usize) {
                        *a = *rng.sample(&self.slots[slot]);
                    }
                    if self.check_joint(&actions).is_ok() {
                        return Choice::Actions(actions);
                    }
                }
                let all = self.joint();
                *rng.sample(&all)
            }
        }
    }

    /// Every jointly legal choice. For team preview this enumerates ordered
    /// selections (360 for bring-4-of-6), so prefer [`sample`](Self::sample)
    /// in hot loops.
    pub fn joint(&self) -> Vec<Choice> {
        let mut out = Vec::new();
        match self.request {
            Request::Wait => {}
            Request::TeamPreview { bring } => {
                let k = bring.min(self.team_len) as usize;
                let mut order = TeamOrder::new();
                permutations(self.team_len, k, &mut order, &mut out);
            }
            Request::Move | Request::ForceSwitch { .. } => {
                let pass = [SlotAction::Pass];
                let s1: &[SlotAction] = if self.active > 1 { &self.slots[1] } else { &pass };
                for &a in self.slots[0].iter() {
                    for &b in s1 {
                        let actions = [a, b];
                        if self.check_joint(&actions).is_ok() {
                            out.push(Choice::Actions(actions));
                        }
                    }
                }
            }
        }
        out
    }
}

impl LegalActions {
    /// The legal actions once the side has answered its request:
    /// [`legal_actions`] for a side that waits, without recomputing them
    /// (nothing it depends on changes before the battle resolves).
    pub(crate) fn set_waiting(&mut self) {
        self.request = Request::Wait;
        for slot in &mut self.slots {
            slot.clear();
        }
        self.extra_targets = [[0; MAX_MOVES]; MAX_ACTIVE];
    }
}

/// `list.contains(action)`, inlined (`contains` calls `Iter::any` out of
/// line, shared with other callers).
#[inline(always)]
fn listed(list: &SlotActions, action: &SlotAction) -> bool {
    for a in list.iter() {
        if a == action {
            return true;
        }
    }
    false
}

fn permutations(n: u8, k: usize, order: &mut TeamOrder, out: &mut Vec<Choice>) {
    if order.len() == k {
        out.push(Choice::Team(*order));
        return;
    }
    for i in 0..n {
        if !order.contains(&i) {
            order.push(i);
            permutations(n, k, order, out);
            order.pop();
        }
    }
}

fn check_team_order(order: &TeamOrder, bring: u8, team_len: u8) -> Result<(), ChoiceError> {
    // Like Showdown, accept anything from exactly `bring` entries up to a
    // full ordering; only the first `bring` are used.
    if order.len() < bring as usize || order.len() > team_len as usize {
        return Err(ChoiceError::InvalidTeamOrder);
    }
    for (k, &i) in order.iter().enumerate() {
        if i >= team_len || order[..k].contains(&i) {
            return Err(ChoiceError::InvalidTeamOrder);
        }
    }
    Ok(())
}

/// What rejected choices have revealed to a side about its current
/// request: Showdown's `[Unavailable choice]` updates (`emitChoiceError`
/// with `updateRequestForPokemon`), by active slot. See [`shown_actions`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Revealed {
    /// The Pokémon in the slot was found trapped (`chooseSwitch`: the
    /// request's `maybeTrapped` becomes `trapped: true`).
    pub trapped: [bool; MAX_ACTIVE],
    /// Move slots found disabled (`chooseMove`: the move gets `disabled:
    /// true`), one bit per move index.
    pub disabled: [u8; MAX_ACTIVE],
}

/// The actions `side`'s request *shows* it may take: [`legal_actions`],
/// except for what the request hides from the last active Pokémon
/// (`getMoveRequestData` with `isLastActive`), unless `revealed` says a
/// rejected choice revealed it:
///
/// * a hidden trap (a foe's Shadow Tag, Arena Trap or Magnet Pull: the
///   request says `maybeTrapped`) doesn't remove its switches;
/// * a move only a foe's Imprison disables (`maybeDisabled`; `getMoves`
///   with `restrictData`) stays listed.
///
/// When every move is disabled but the request shows some as usable (only
/// Imprison disables them), the request isn't locked into Struggle: it
/// offers those moves, with the gimmicks (`canMegaEvo`, `canTerastallize`),
/// and not the moves it shows disabled. `chooseMove` turns any move choice
/// into Struggle then (without the gimmick), so [`legal_actions`] takes the
/// moves the request shows disabled too.
///
/// Apart from those, every choice [`legal_actions`] accepts is in this set. A choice this
/// set has and [`legal_actions`] doesn't is one Showdown rejects as an
/// `[Unavailable choice]`, revealing the trap or the disable
/// ([`LegalActions::check`] fails on that slot with
/// [`ChoiceError::IllegalAction`]). For an environment whose masks must not
/// leak what the player can't see.
pub fn shown_actions(battle: &Battle, side: SideId, revealed: &Revealed) -> LegalActions {
    actions(battle, side, Some(revealed))
}

/// Legal actions restricted to the format's gimmicks. With `revealed`,
/// these are [`shown_actions`]; otherwise they are the engine's legal actions.
/// Fixed action spaces use this to omit simulator-only choices from supplied
/// Past items (such as Mega Stones in Gen 9).
pub fn format_actions(battle: &Battle, side: SideId, revealed: Option<&Revealed>) -> LegalActions {
    let mut legal = LegalActions { request: battle.request(side), ..LegalActions::default() };
    fill_actions::<true>(battle, side, revealed, &mut legal);
    legal
}

/// Generates the legal actions for `side`'s current request.
/// [`Battle::legal_actions`] returns the ones generated when the request
/// was issued; this recomputes them (for the benchmarks: not part of the
/// stable API).
#[doc(hidden)]
pub fn legal_actions(battle: &Battle, side: SideId) -> LegalActions {
    actions(battle, side, None)
}

/// The issued request's actions, including after a submission, for parsing
/// a replacement as Showdown's `side.choose` does.
#[doc(hidden)]
pub fn issued_actions(battle: &Battle, side: SideId) -> LegalActions {
    let mut legal = LegalActions { request: battle.issued_request(side), ..LegalActions::default() };
    fill_actions::<false>(battle, side, None, &mut legal);
    legal
}

/// [`legal_actions`] into `legal`, a default [`LegalActions`]
/// (`Battle::refresh_legal`, which fills both sides in place).
pub(crate) fn fill_legal_actions(battle: &Battle, side: SideId, legal: &mut LegalActions) {
    legal.request = battle.next_request(side);
    fill_actions::<false>(battle, side, None, legal);
}

/// [`legal_actions`], or with `shown` [`shown_actions`].
fn actions(battle: &Battle, side: SideId, shown: Option<&Revealed>) -> LegalActions {
    let mut legal = LegalActions { request: battle.request(side), ..LegalActions::default() };
    fill_actions::<false>(battle, side, shown, &mut legal);
    legal
}

/// [`actions`] into `legal`, a default [`LegalActions`]. One copy, not
/// inlined into its callers (`move_slot_actions` is inlined here, its one
/// caller).
#[inline(never)]
fn fill_actions<const FORMAT_ONLY: bool>(
    battle: &Battle,
    side: SideId,
    shown: Option<&Revealed>,
    legal: &mut LegalActions,
) {
    let request = legal.request;
    let s = battle.state.side(side);
    let active = battle.format.active_per_side();
    legal.active = active as u8;
    legal.team_len = s.team.len() as u8;
    // The Pokémon that can switch in, found once (each slot lists them).
    let bench = bench_bits(s);
    legal.bench = bench.count_ones() as u8;
    match request {
        Request::Wait | Request::TeamPreview { .. } => {}
        Request::Move => {
            for slot in 0..active {
                let (list, extra) = (&mut legal.slots[slot], &mut legal.extra_targets[slot]);
                move_slot_actions::<FORMAT_ONLY>(battle, side, slot, bench, list, extra, shown);
            }
        }
        Request::ForceSwitch { slots } => {
            let forced = slots.iter().filter(|&&f| f).count() as u8;
            let benched = legal.bench;
            for (slot, (list, &must)) in legal.slots.iter_mut().zip(&slots).take(active).enumerate() {
                // Revival Blessing: pick a fainted Pokémon, wherever it is
                // (`chooseSwitch`), or pass while `forcedPassesLeft` allows
                // (fewer healthy benched Pokémon than flagged slots): the
                // flag stays, and the request comes again after the next
                // action, until the condition ends with the turn.
                if must && s.conditions.slots[slot].has(SlotCondition::RevivalBlessing) {
                    (0..s.team.len() as u8).filter(|&i| s.team[i as usize].is_fainted()).for_each(|i| {
                        list.push(SlotAction::Switch(i));
                    });
                    if benched < forced {
                        list.push(SlotAction::Pass);
                    }
                    continue;
                }
                if must {
                    push_switches(bench, list);
                    // With fewer replacements than holes, some slot passes.
                    if benched < forced || list.is_empty() {
                        list.push(SlotAction::Pass);
                    }
                } else {
                    list.push(SlotAction::Pass);
                }
            }
        }
    }
}

/// Fills `out` (empty) with the actions of the Pokémon in `slot`: the legal
/// ones, or with `view` the ones its request shows ([`shown_actions`]).
/// `bench` is the side's [`bench_bits`].
fn move_slot_actions<const FORMAT_ONLY: bool>(
    battle: &Battle,
    side: SideId,
    slot: usize,
    bench: u8,
    out: &mut SlotActions,
    extra: &mut [u8; MAX_MOVES],
    view: Option<&Revealed>,
) {
    let state = &battle.state;
    let s = state.side(side);
    let Some(mon) = s.active(slot) else {
        out.push(SlotAction::Pass);
        return;
    };
    // A commanding Tatsugiri passes (`getChoiceIndex`).
    if mon.volatiles.has(Volatiles::COMMANDING) {
        out.push(SlotAction::Pass);
        return;
    }
    // Recharging (Showdown's locked move `recharge`): the only option is
    // "move 1", with no target, no gimmick and no switching (trapped). The
    // same goes for any other locked move (Outrage, a charged Fly, ...).
    if mon.volatiles.has(Volatiles::MUST_RECHARGE | Volatiles::LOCKING) {
        out.push(SlotAction::Move { index: 0, target: None, gimmick: None });
        return;
    }
    let doubles = battle.format.active_per_side() > 1;
    let usable = mon.has_usable_move();
    // The last active Pokémon's request shows moves only Imprison disables
    // as usable (`getMoves(..., restrictData)`): with no usable move, a
    // choice of any of its moves, aimed as that move, means Struggle.
    let pos = Position::new(side, slot as u8);
    let last_active = (slot + 1..battle.format.active_per_side()).all(|i| s.active(i).is_none());
    let shown = !usable && last_active && (0..mon.moves.len()).any(|i| resolve::request_shows_usable(battle, pos, i));
    // Struggle is a locked move: no gimmick with it (`getMoveRequestData`).
    // But when the request shows moves only Imprison disables, it isn't
    // locked and shows the gimmicks too (`canMegaEvo`, `canTerastallize`),
    // which `chooseMove` accepts and ignores (the choice becomes Struggle,
    // `resolve::add_slot_action`).
    let gimmicks = if usable || shown { available_gimmicks::<FORMAT_ONLY>(battle, s, slot) } else { Gimmicks::NONE };

    // `shown_actions`: the last active Pokémon's request shows a move only
    // Imprison disables as usable, until a rejected choice revealed it.
    let hidden_disable = |index: usize| {
        view.is_some_and(|r| {
            last_active && r.disabled[slot] & (1 << index) == 0 && resolve::request_shows_usable(battle, pos, index)
        })
    };
    for (index, m) in mon.moves.iter().enumerate() {
        // No usable move: move 0 means Struggle (as Showdown's "move 1").
        if usable && (m.pp == 0 || m.disabled) && !hidden_disable(index) || !usable && !shown && index > 0 {
            continue;
        }
        // `shown_actions`: with no usable move, the request still lists the
        // moves only Imprison disables as usable, and the others (no PP, a
        // visible disable) as disabled. `chooseMove` takes any of them
        // (Struggle), but the request offers only the first.
        if view.is_some() && !usable && shown && !resolve::request_shows_usable(battle, pos, index) {
            continue;
        }
        // Without a usable move the choice is Struggle, which takes no
        // target; but a move the request shows (Imprison's hidden disable)
        // is validated as that move (`chooseMove`: a target in doubles),
        // then becomes Struggle.
        let target_kind = if usable || shown {
            // Curse from a non-Ghost type targets the user.
            resolve::request_target(mon, m.id, m.target.unwrap_or(battle.format.dex.move_(m.id).target))
        } else {
            MoveTarget::RandomNormal
        };
        // Each target, without and with each gimmick.
        let mut push = |target: Option<Target>| {
            let index = index as u8;
            out.push(SlotAction::Move { index, target, gimmick: None });
            for g in [Gimmick::Terastallize, Gimmick::MegaEvolve] {
                if gimmicks.contains(g) {
                    out.push(SlotAction::Move { index, target, gimmick: Some(g) });
                }
            }
        };
        if doubles && target_kind.requires_choice() {
            let foes = state.side(side.foe());
            let ally_slot = 1 - slot as u8;
            let has_ally = s.active(ally_slot as usize).is_some();
            let foe_targets = matches!(target_kind, MoveTarget::Normal | MoveTarget::AdjacentFoe | MoveTarget::Any);
            if foe_targets {
                for f in 0..2u8 {
                    if foes.active(f as usize).is_some() {
                        push(Some(Target::Foe(f)));
                    }
                }
            }
            // Showdown's `validTargetLoc` only
            // checks the location, so an `adjacentAlly` move (Helping Hand,
            // Dragon Cheer) aims at the ally's slot even without an ally
            // there (and then fails): it must stay selectable, once.
            if matches!(
                target_kind,
                MoveTarget::Normal | MoveTarget::Any | MoveTarget::AdjacentAlly | MoveTarget::AdjacentAllyOrSelf
            ) && (has_ally || target_kind == MoveTarget::AdjacentAlly)
            {
                push(Some(Target::Ally(ally_slot)));
            }
            if target_kind == MoveTarget::AdjacentAllyOrSelf {
                push(Some(Target::Ally(slot as u8)));
            }
            // What else `chooseMove` takes (`LegalActions::check`).
            extra[index] = doubles_target_bits(slot, target_kind);
        } else {
            push(None);
        }
    }
    // Trapped Pokémon can't switch (`pokemon.trapped` from `TrapPokemon`).
    // The last active Pokémon's request shows only a visible trap
    // (`trapped === true`; a hidden one is `maybeTrapped`), until a
    // rejected switch revealed it.
    let trapped = match view {
        Some(r) if last_active => resolve::is_visibly_trapped(battle, pos) || r.trapped[slot],
        _ => resolve::is_trapped(battle, pos),
    };
    if !trapped {
        push_switches(bench, out);
    }
}

/// [`SideState::bench`](crate::state::SideState::bench) as bits: bit `i`
/// for team position `i`.
#[inline]
fn bench_bits(s: &SideState) -> u8 {
    // Keep this tiny scan in the generator: `Iterator::fold` is otherwise
    // outlined when shared by the two gimmick specializations.
    let mut bits = 0;
    for i in 0..s.team.len() as u8 {
        if !s.is_active(i) && !s.team[i as usize].is_fainted() {
            bits |= 1 << i;
        }
    }
    bits
}

/// Pushes a switch to each team position of `bench` ([`bench_bits`]), in
/// order.
#[inline]
fn push_switches(mut bench: u8, out: &mut SlotActions) {
    while bench != 0 {
        out.push(SlotAction::Switch(bench.trailing_zeros() as u8));
        bench &= bench - 1;
    }
}

/// [`LegalActions::extra_targets`]' bits of a move of `kind` used from
/// `slot` in doubles: no target (bit 0) and each location Showdown's
/// `validTargetLoc` allows (bits 1 to 4: foe slots 1 and 2, own slots 1
/// and 2). A table for the kinds that take a target.
#[inline]
fn doubles_target_bits(slot: usize, kind: MoveTarget) -> u8 {
    let i = match kind {
        MoveTarget::Normal => 0,
        MoveTarget::AdjacentFoe => 1,
        MoveTarget::AdjacentAlly => 2,
        MoveTarget::AdjacentAllyOrSelf => 3,
        MoveTarget::Any => 4,
        _ => return target_bits_of(slot as u8, kind),
    };
    DOUBLES_TARGET_BITS[slot][i]
}

/// [`doubles_target_bits`] for the kinds of moves that take a target,
/// by slot.
const DOUBLES_TARGET_BITS: [[u8; 5]; 2] = {
    use MoveTarget as T;
    let kinds = [T::Normal, T::AdjacentFoe, T::AdjacentAlly, T::AdjacentAllyOrSelf, T::Any];
    let mut table = [[0; 5]; 2];
    let mut slot = 0;
    while slot < 2 {
        let mut i = 0;
        while i < kinds.len() {
            table[slot][i] = target_bits_of(slot as u8, kinds[i]);
            i += 1;
        }
        slot += 1;
    }
    table
};

// The table is doubles': two slots a side, Showdown's `numSlots` of 2.
const _: () = assert!(MAX_ACTIVE == 2, "DOUBLES_TARGET_BITS assumes two active slots a side");

/// [`doubles_target_bits`], computed.
const fn target_bits_of(slot: u8, kind: MoveTarget) -> u8 {
    let mut bits = 1;
    let locs = [(1, 2), (2, 4), (-1, 8), (-2, 16)];
    let mut i = 0;
    while i < locs.len() {
        if resolve::valid_target_loc_in(MAX_ACTIVE as i8, locs[i].0, slot, kind) {
            bits |= locs[i].1;
        }
        i += 1;
    }
    bits
}

fn available_gimmicks<const FORMAT_ONLY: bool>(battle: &Battle, side: &SideState, slot: usize) -> Gimmicks {
    let mut g = Gimmicks::NONE;
    let allowed = battle.format.gimmicks;
    let Some(mon) = side.active(slot) else { return g };
    // `canTerastallize`.
    if allowed.contains(Gimmick::Terastallize) && resolve::tera::can_terastallize(&battle.format, side, mon) {
        g.insert(Gimmick::Terastallize);
    }
    // `canMegaEvo`, until one Pokémon of the side has Mega Evolved
    // (Showdown then sets every one's `canMegaEvo` to false).
    // Side.chooseMove checks canMegaEvo without a format gate: supplied
    // Past stones can expose this choice in a Gen 9 simulator battle.
    if (!FORMAT_ONLY || allowed.contains(Gimmick::MegaEvolve))
        && !side.gimmicks_used.contains(Gimmick::MegaEvolve)
        && mon.can_mega_evo.is_some()
    {
        g.insert(Gimmick::MegaEvolve);
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BattleOptions, Format, PokemonSet, Seed, Team};
    use pikapro_data::gen9::{moves, species};

    fn move_(index: u8, target: Option<Target>) -> SlotAction {
        SlotAction::Move { index, target, gimmick: None }
    }

    /// A doubles battle where p1 has one Pokémon (no ally) with Helping
    /// Hand and Tackle.
    fn alone() -> Battle {
        let p1 = Team::new(vec![PokemonSet::new(species::PIKACHU).with_moves(&[moves::HELPING_HAND, moves::TACKLE])]);
        let foe = || PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE]);
        let p2 = Team::new(vec![foe(), foe()]);
        Battle::new(Format::gen9_doubles(), [p1, p2], BattleOptions::new(Seed::fast(1))).unwrap()
    }

    #[test]
    fn an_ally_move_without_an_ally_is_listed_once() {
        let battle = alone();
        let legal = battle.legal_actions(SideId::P1);
        let helping_hand: Vec<_> =
            legal.slot(0).iter().filter(|a| matches!(a, SlotAction::Move { index: 0, gimmick: None, .. })).collect();
        assert_eq!(helping_hand, [&move_(0, Some(Target::Ally(1)))]);
        assert_eq!(legal.slot(1), &[SlotAction::Pass]);
        // A slot the format doesn't have has no actions.
        assert!(legal.slot(2).is_empty());
        assert!(legal.slot(usize::MAX).is_empty());
    }

    #[test]
    fn targets_beyond_the_second_slot_are_rejected() {
        let battle = alone();
        let legal = battle.legal_actions(SideId::P1);
        let choice = |a| Choice::Actions([a, SlotAction::Pass]);
        assert!(legal.is_legal(&choice(move_(1, Some(Target::Foe(1))))));
        assert!(legal.is_legal(&choice(move_(1, None))), "no target: a random one");
        for n in 2..=255 {
            for t in [Target::Foe(n), Target::Ally(n)] {
                assert!(!legal.is_legal(&choice(move_(1, Some(t)))), "{t:?}");
                assert!(!legal.is_legal(&choice(move_(0, Some(t)))), "{t:?}");
            }
        }
    }

    /// Answers team preview with the team's order, if asked.
    fn skip_preview(battle: &mut Battle) {
        for side in SideId::BOTH {
            if let Request::TeamPreview { .. } = battle.request(side) {
                let order: Vec<u8> = (0..battle.state.side(side).team.len() as u8).collect();
                battle.submit(side, Choice::team(&order)).unwrap();
            }
        }
    }

    fn has_switch(list: &[SlotAction]) -> bool {
        list.iter().any(|a| matches!(a, SlotAction::Switch(_)))
    }

    #[test]
    fn shown_actions_keep_switches_under_a_hidden_trap() {
        let p1 = Team::new(vec![
            PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE]),
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE]),
        ]);
        let shadow_tag = pikapro_data::gen9::abilities::SHADOW_TAG;
        let p2 =
            Team::new(vec![PokemonSet::new(species::WOBBUFFET).with_ability(shadow_tag).with_moves(&[moves::SPLASH])]);
        let mut battle = Battle::new(Format::gen9_singles(), [p1, p2], BattleOptions::new(Seed::fast(1))).unwrap();
        skip_preview(&mut battle);
        // Shadow Tag traps from the end of the first turn.
        battle.submit(SideId::P1, Choice::single(move_(0, None))).unwrap();
        battle.submit(SideId::P2, Choice::single(move_(0, None))).unwrap();
        let data = battle.request_data(SideId::P1);
        let crate::request::RequestKind::Move { active } = data.kind else { panic!("{data:?}") };
        let a = active[0].unwrap();
        assert!(a.maybe_trapped && !a.trapped, "{a:?}");
        // The abilities a foe's species could have, in a format with
        // Obtainable Abilities (Showdown's `endTurn`): Wobbuffet's Shadow
        // Tag, whatever ability it has, unless the format bans the ability
        // (gen9ou).
        let maybe_trapped_by_telepathy = |format: Format| {
            let telepathy = pikapro_data::gen9::abilities::TELEPATHY;
            let p1 = Team::new(vec![
                PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE]),
                PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE]),
            ]);
            let p2 = Team::new(vec![
                PokemonSet::new(species::WOBBUFFET).with_ability(telepathy).with_moves(&[moves::SPLASH]),
            ]);
            let mut battle = Battle::new(format, [p1, p2], BattleOptions::new(Seed::fast(1))).unwrap();
            skip_preview(&mut battle);
            battle.submit(SideId::P1, Choice::single(move_(0, None))).unwrap();
            battle.submit(SideId::P2, Choice::single(move_(0, None))).unwrap();
            let data = battle.request_data(SideId::P1);
            let crate::request::RequestKind::Move { active } = data.kind else { panic!("{data:?}") };
            active[0].unwrap().maybe_trapped
        };
        assert!(!maybe_trapped_by_telepathy(Format::gen9_singles()));
        assert!(maybe_trapped_by_telepathy(Format { obtainable_abilities: true, ..Format::gen9_singles() }));
        assert!(!maybe_trapped_by_telepathy(Format::gen9_ou()));
        let no_bans = Format { banned_trapping_abilities: &[], ..Format::gen9_ou() };
        assert!(maybe_trapped_by_telepathy(no_bans));
        let legal = battle.legal_actions(SideId::P1);
        let shown = shown_actions(&battle, SideId::P1, &Revealed::default());
        assert!(!has_switch(legal.slot(0)) && has_switch(shown.slot(0)));
        let switch = Choice::single(SlotAction::Switch(1));
        assert!(shown.is_legal(&switch));
        assert_eq!(legal.check(&switch), Err(ChoiceError::IllegalAction { slot: 0 }));
        let revealed = Revealed { trapped: [true, false], ..Revealed::default() };
        assert_eq!(shown_actions(&battle, SideId::P1, &revealed).slot(0), legal.slot(0));
        // Nothing is hidden from the foe: the same actions.
        let foe = shown_actions(&battle, SideId::P2, &Revealed::default());
        assert_eq!(foe.slot(0), battle.legal_actions(SideId::P2).slot(0));
    }

    #[test]
    fn shown_actions_keep_moves_only_imprison_disables() {
        let p1 = Team::new(vec![
            PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE, moves::SPLASH]),
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE, moves::SPLASH]),
        ]);
        let p2 = Team::new(vec![
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::IMPRISON, moves::TACKLE]),
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::SPLASH]),
        ]);
        let mut battle = Battle::new(Format::gen9_doubles(), [p1, p2], BattleOptions::new(Seed::fast(1))).unwrap();
        skip_preview(&mut battle);
        let splash = move_(1, None);
        battle.submit(SideId::P1, Choice::doubles(splash, splash)).unwrap();
        battle.submit(SideId::P2, Choice::doubles(move_(0, None), move_(0, None))).unwrap();
        let data = battle.request_data(SideId::P1);
        let crate::request::RequestKind::Move { active } = data.kind else { panic!("{data:?}") };
        assert!(!active[0].unwrap().maybe_disabled && active[1].unwrap().maybe_disabled, "{active:?}");
        let tackle = |slot: usize, l: &LegalActions| l.slot(slot).iter().any(|a| *a == move_(0, Some(Target::Foe(0))));
        let legal = *battle.legal_actions(SideId::P1);
        let shown = shown_actions(&battle, SideId::P1, &Revealed::default());
        // The first slot's request shows the disable; the last active
        // Pokémon's doesn't.
        assert!(!tackle(0, &legal) && !tackle(0, &shown));
        assert!(!tackle(1, &legal) && tackle(1, &shown));
        let choice = Choice::doubles(splash, move_(0, Some(Target::Foe(0))));
        assert!(shown.is_legal(&choice));
        assert_eq!(legal.check(&choice), Err(ChoiceError::IllegalAction { slot: 1 }));
        let revealed = Revealed { disabled: [0, 1], ..Revealed::default() };
        assert_eq!(shown_actions(&battle, SideId::P1, &revealed).slot(1), legal.slot(1));
    }

    #[test]
    fn a_request_showing_moves_only_imprison_disables_offers_the_gimmick() {
        // The last active Pokémon's only move is disabled by a foe's
        // Imprison: its request shows the move and `canMegaEvo`, and
        // `chooseMove` takes a Mega Evolution with it as plain Struggle.
        let venusaurite = pikapro_data::gen9::items::VENUSAURITE;
        // Champions VGC: six Pokémon each, the first four brought.
        let filler = |s| PokemonSet::new(s).with_moves(&[moves::SPLASH]);
        let fillers = [species::PIKACHU, species::CHARIZARD, species::BLASTOISE, species::GENGAR];
        let mut p1 = vec![
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::SPLASH]),
            PokemonSet::new(species::VENUSAUR).with_item(venusaurite).with_moves(&[moves::TACKLE]),
        ];
        let mut p2 = vec![
            PokemonSet::new(species::SNORLAX).with_moves(&[moves::IMPRISON, moves::TACKLE]),
            PokemonSet::new(species::GYARADOS).with_moves(&[moves::SPLASH]),
        ];
        p1.extend(fillers.map(filler));
        p2.extend(fillers.map(filler));
        let sp = crate::team::Investment::StatPoints(pikapro_data::Stats::splat(0));
        let teams = [p1, p2].map(|t| Team::new(t.into_iter().map(|s| s.with_investment(sp)).collect()));
        let mut battle = Battle::new(Format::champions_vgc(), teams, BattleOptions::new(Seed::fast(1))).unwrap();
        skip_preview(&mut battle);
        battle.submit(SideId::P1, Choice::doubles(move_(0, None), move_(0, Some(Target::Foe(1))))).unwrap();
        battle.submit(SideId::P2, Choice::doubles(move_(0, None), move_(0, None))).unwrap();
        let data = battle.request_data(SideId::P1);
        let crate::request::RequestKind::Move { active } = data.kind else { panic!("{data:?}") };
        let a = active[1].unwrap();
        assert!(a.maybe_disabled && a.can_mega_evo, "{a:?}");
        let mega = SlotAction::Move { index: 0, target: Some(Target::Foe(0)), gimmick: Some(Gimmick::MegaEvolve) };
        let shown = shown_actions(&battle, SideId::P1, &Revealed::default());
        assert!(shown.slot(1).contains(&mega), "{:?}", shown.slot(1));
        let choice = Choice::doubles(move_(0, None), mega);
        assert!(battle.legal_actions(SideId::P1).is_legal(&choice));
        battle.submit(SideId::P1, choice).unwrap();
        battle.submit(SideId::P2, Choice::doubles(move_(1, Some(Target::Foe(0))), move_(0, None))).unwrap();
        let venusaur = battle.state.side(SideId::P1).active(1).unwrap();
        assert_eq!(venusaur.species, species::VENUSAUR, "no Mega Evolution with Struggle");
        assert_eq!(venusaur.last_move, Some(moves::STRUGGLE));
    }

    #[test]
    fn shown_actions_contain_the_legal_ones() {
        // Random battles in every preset: the shown set holds every legal
        // action, and more only when the request says `maybe*`.
        for format in Format::presets() {
            for seed in 0..8 {
                let teams = [crate::testing::sample_team(format), crate::testing::sample_team(format)];
                let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
                let mut rng = BattleRng::new(Seed::fast(!seed));
                while !battle.is_over() {
                    for side in SideId::BOTH {
                        let legal = *battle.legal_actions(side);
                        let shown = shown_actions(&battle, side, &Revealed::default());
                        let maybe = match battle.request_data(side).kind {
                            crate::request::RequestKind::Move { active } => {
                                active.iter().flatten().any(|a| a.maybe_trapped || a.maybe_disabled)
                            }
                            _ => false,
                        };
                        for slot in 0..MAX_ACTIVE {
                            // (With no usable move, the moves the request
                            // shows disabled are legal but not shown.)
                            let struggling = battle.state.side(side).active(slot).is_some_and(|m| !m.has_usable_move());
                            assert!(struggling || legal.slot(slot).iter().all(|a| shown.slot(slot).contains(a)));
                            assert!(maybe || legal.slot(slot) == shown.slot(slot), "{}", format.id);
                        }
                        if !legal.is_waiting() {
                            battle.submit(side, legal.sample(&mut rng)).unwrap();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn slot_lists_have_no_duplicates() {
        // Random battles in every preset: each list names an action once,
        // so `sample` is uniform over them.
        for format in Format::presets() {
            for seed in 0..8 {
                let teams = [crate::testing::sample_team(format), crate::testing::sample_team(format)];
                let mut battle = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
                let mut rng = BattleRng::new(Seed::fast(!seed));
                while !battle.is_over() {
                    for side in SideId::BOTH {
                        let legal = battle.legal_actions(side);
                        for slot in 0..MAX_ACTIVE {
                            let list = legal.slot(slot);
                            for (i, a) in list.iter().enumerate() {
                                assert!(!list[i + 1..].contains(a), "{} {a:?} twice in {list:?}", format.id);
                            }
                        }
                        if !legal.is_waiting() {
                            let choice = legal.sample(&mut rng);
                            battle.submit(side, choice).unwrap();
                        }
                    }
                }
            }
        }
    }

    /// `from_slots` takes lists from untrusted input: none of these
    /// panics, in debug or release.
    #[test]
    fn from_slots_validates_its_inputs() {
        let mut rng = BattleRng::new(Seed::fast(1));
        // An active slot with no listed action gets a pass.
        let legal = LegalActions::from_slots(Request::Move, [SlotActions::new(), SlotActions::new()], 1, 6, 0);
        assert_eq!(legal.slot(0), &[SlotAction::Pass]);
        assert_eq!(legal.sample(&mut rng), Choice::Actions([SlotAction::Pass; MAX_ACTIVE]));
        // A team preview over 9 Pokémon: the team is capped at 6.
        let legal = LegalActions::from_slots(
            Request::TeamPreview { bring: 4 },
            [SlotActions::new(), SlotActions::new()],
            2,
            9,
            0,
        );
        assert_eq!(legal.team_len(), MAX_TEAM);
        let order = |o: &[u8]| Choice::Team(o.iter().copied().collect());
        assert_eq!(legal.check(&order(&[8, 0, 1, 2])), Err(ChoiceError::InvalidTeamOrder));
        assert_eq!(legal.check(&order(&[5, 0, 1, 2])), Ok(()));
        assert_eq!(legal.check(&order(&[1, 0, 1, 2])), Err(ChoiceError::InvalidTeamOrder));
        // A listed switch to position 8 is dropped, and the bench with it.
        let mut s = SlotActions::new();
        s.push(SlotAction::Switch(8));
        s.push(SlotAction::Switch(200));
        let legal = LegalActions::from_slots(Request::Move, [s, SlotActions::new()], 1, 6, 1);
        assert_eq!(legal.slot(0), &[SlotAction::Pass]);
        assert_eq!(legal.bench(), 0);
        assert_eq!(legal.sample(&mut rng), Choice::Actions([SlotAction::Pass; MAX_ACTIVE]));
        let choice = Choice::Actions([SlotAction::Switch(200), SlotAction::Switch(200)]);
        assert_eq!(legal.check(&choice), Err(ChoiceError::IllegalAction { slot: 0 }));
        // Slots past `active` are emptied; `active` is capped.
        let mut s = SlotActions::new();
        s.push(SlotAction::Switch(2));
        let legal = LegalActions::from_slots(Request::ForceSwitch { slots: [true, false] }, [s, s], 7, 6, 3);
        assert_eq!(legal.slot(1), &[SlotAction::Switch(2)]);
        assert_eq!(legal.bench(), 1);
        let legal = LegalActions::from_slots(Request::ForceSwitch { slots: [true, false] }, [s, s], 1, 6, 3);
        assert!(legal.slot(1).is_empty());
        assert_eq!(legal.sample(&mut rng), Choice::Actions([SlotAction::Switch(2), SlotAction::Pass]));
    }
}
