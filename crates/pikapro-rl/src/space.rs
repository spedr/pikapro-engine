//! The fixed-size action space of a format: a stable integer for every
//! [`SlotAction`] and every [`Choice`], and masks over it.
//!
//! # Slot actions
//!
//! One active slot's action is an integer in `0..slot_size`:
//!
//! | range | action |
//! |---|---|
//! | `move * G * T + gimmick * T + target` | `move <move+1> [target] [gimmick]` |
//! | `4 * G * T + i` (`i` in `0..6`) | `switch <i+1>` (team *position*, as in Showdown) |
//! | `4 * G * T + 6` | `pass` |
//!
//! * `T` is 1 in singles (no target) and 5 in doubles: `0` no target (a move
//!   that takes none, or Showdown's random target), `1` and `2` the foes in
//!   slots 0 and 1 (Showdown's `1`, `2`), `3` and `4` the user's side's
//!   slots 0 and 1 (Showdown's `-1`, `-2`).
//! * `G` is 1 + the format's gimmicks, in the order Terastallize, Mega
//!   Evolution: gimmick `0` is none, `1` the first allowed gimmick, and so
//!   on. Gen 9 formats have `G = 2` (Tera), Champions `G = 2` (Mega).
//!   Supplied Past stones can permit Mega Evolution in Gen 9 simulator
//!   battles, but RL policies are offered only the format's gimmicks.
//!
//! So `slot_size` is 15 in gen9singles, gen9ou and championsbss, and 47 in
//! gen9doubles, gen9vgc and championsvgc.
//!
//! # Choices
//!
//! A whole side's choice is an integer in `0..size`, with `size = joint_size
//! + preview_size`:
//!
//! * `0..joint_size` (`joint_size = slot_size ^ active`) answer a move or
//!   forced-switch request: `a0 * slot_size + a1` in doubles, `a0` in
//!   singles.
//! * `joint_size..size` answer team preview: the `k`-th ordered selection of
//!   `bring` of the six team positions, in lexicographic order
//!   (`[0,1,2,3]`, `[0,1,2,4]`, ..., `[5,4,3,2]`: 360 for bring 4, 120 for
//!   championsbss's bring 3). In a format whose team preview brings the
//!   whole team (gen9ou), the `k`-th choice leads with team position `k`
//!   and keeps the others in team order, as Showdown's `team <k+1>` does:
//!   6 choices, not the 720 orders ([`ActionSpace::lead_only`]). Formats
//!   without team preview have `preview_size = 0`.
//!
//! A joint index `j < joint_size` decomposes into slot actions as `a0 *
//! slot_size + a1` in doubles (`a0` in singles):
//! [`ActionSpace::split_joint`].
//!
//! The indexing depends only on the format's game type, gimmicks and team
//! preview, never on the teams, so it is stable across battles.
//!
//! # Doubles targets
//!
//! Target codes `3` and `4` are the user's side's slots 0 and 1, absolute:
//! one of them is the user itself (slot 0 targeting code `3` targets
//! itself). [`relative_target`] turns a code into one relative to the
//! acting slot (the user, its ally).
//!
//! # The action table
//!
//! [`ActionSpace::action_table`] describes every factored action index
//! (the slot actions, then the team-preview orders, as the factored mask
//! lays them out) with integer columns ([`action_col`]): a static table a
//! policy can gather from, with no per-step cost.
//!
//! # Why 360 team-preview orders
//!
//! A team-preview choice is an ordered selection (360 for 4 of 6), not a
//! set (15): the order of the back Pokémon is the team order, which sets
//! the team positions `switch i` names, and Illusion disguises its user as
//! the last Pokémon of the party.

use pikapro::legal::LegalActions;
use pikapro::{
    Choice, Format, Gimmick, MAX_ACTIVE, MAX_MOVES, MAX_TEAM, Request, SlotAction, Target, TeamOrder, TeamPreview,
};
use pikapro_view::OwnOverlay;

use crate::vec::{MaskLayout, SwitchBy};

/// Gimmicks in their action-space order.
const GIMMICK_ORDER: [Gimmick; 2] = [Gimmick::Terastallize, Gimmick::MegaEvolve];

/// The largest factored slot mask: two slots of every move, gimmick and
/// target code, the switches and `pass`.
const MAX_SLOT_MASK: usize = MAX_ACTIVE * (MAX_MOVES * (1 + GIMMICK_ORDER.len()) * 5 + MAX_TEAM + 1);

/// The switch action index of team position `pos` for a player whose own
/// side is `own` ([`SwitchBy`]): the position itself, or the row of the
/// Pokémon there (its identity, [`OwnMon::id`](pikapro_view::OwnMon::id));
/// `None` if no Pokémon is there.
#[inline]
pub fn switch_index(by: SwitchBy, own: &OwnOverlay, pos: u8) -> Option<usize> {
    match by {
        SwitchBy::Position => Some(pos as usize),
        SwitchBy::Row => own.mons.get(pos as usize).map(|m| m.id as usize),
    }
}

/// The columns of [`ActionTable`] rows, by offset. A column that doesn't
/// apply to the row's kind is -1: 0 is one of its values (a move's slot
/// 0, the target code 0 of a move without a target to choose).
pub mod action_col {
    /// [`action_kind`](super::action_kind): move, switch, pass or team
    /// preview.
    pub const KIND: usize = 0;
    /// A move's slot, 0 to 3; -1 otherwise.
    pub const MOVE_SLOT: usize = 1;
    /// A move's target code (0 none, 1 and 2 the foes in slots 0 and 1, 3
    /// and 4 the user's side's slots 0 and 1; always 0 in singles); -1
    /// otherwise.
    pub const TARGET: usize = 2;
    /// A move's gimmick code (0 none, `i + 1` the space's `i`-th gimmick);
    /// -1 otherwise.
    pub const GIMMICK: usize = 3;
    /// `k` for switch action `k`: team position `k` with `switch_by =
    /// "position"`, own row `k` with `switch_by = "row"`; -1 otherwise.
    pub const SWITCH: usize = 4;
    /// A team-preview order's index (its order is
    /// [`ActionSpace::preview_order`](super::ActionSpace::preview_order));
    /// -1 otherwise.
    pub const PREVIEW: usize = 5;
    /// Columns per row.
    pub const COUNT: usize = 6;
    /// Their names, by offset.
    pub const NAMES: [&str; COUNT] = ["kind", "move_slot", "target", "gimmick", "switch", "preview"];
}

/// The values of [`action_col::KIND`].
pub mod action_kind {
    pub const MOVE: i16 = 0;
    pub const SWITCH: i16 = 1;
    pub const PASS: i16 = 2;
    pub const PREVIEW: i16 = 3;
}

/// A format's factored actions described by integers
/// ([`ActionSpace::action_table`]): row `i < slot_size` is slot action
/// `i`, row `slot_size + k` team-preview order `k`, as the factored mask
/// lays them out. Columns are [`action_col`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionTable {
    /// One row per action, [`action_col::COUNT`] values each.
    pub rows: Vec<[i16; action_col::COUNT]>,
    /// Rows before the team-preview orders: the space's `slot_size`.
    pub slot_size: usize,
}

impl ActionTable {
    /// Rows: `slot_size + preview_size`.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Column `col` of every row.
    pub fn column(&self, col: usize) -> Vec<i16> {
        self.rows.iter().map(|r| r[col]).collect()
    }
}

/// A doubles target code ([`ActionSpace`]'s `0..=4`) relative to the
/// acting `slot`: 0 no target, 1 and 2 the foes in slots 0 and 1, 3 the
/// user itself, 4 its ally. Codes 3 and 4 name the user's side's slots
/// absolutely, so one of them is the user. `None` for a code above 4 or a
/// slot above 1.
#[must_use]
pub const fn relative_target(slot: usize, t: usize) -> Option<usize> {
    match (slot, t) {
        (0..=1, 0..=2) => Some(t),
        (0..=1, 3..=4) => Some(if t - 3 == slot { 3 } else { 4 }),
        _ => None,
    }
}

/// A format's action space (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionSpace {
    /// Active slots per side (1 or 2).
    pub active: usize,
    /// Target codes per move (1 in singles, 5 in doubles).
    pub targets: usize,
    /// The format's gimmicks, in action-space order (gimmick code `i + 1`).
    pub gimmicks: Vec<Gimmick>,
    /// Actions per slot.
    pub slot_size: usize,
    /// Move and forced-switch choices: `slot_size ^ active`.
    pub joint_size: usize,
    /// Team positions a team-preview choice names: the Pokémon brought (0
    /// without team preview), or 1 when the whole team is brought and the
    /// choice names the lead ([`ActionSpace::lead_only`]).
    pub bring: usize,
    /// Whether team preview brings the whole team
    /// ([`TeamPreview::All`]): a preview choice names the lead, and the
    /// other Pokémon follow in team order.
    pub lead_only: bool,
    /// Team-preview choices: ordered selections of `bring` of 6.
    pub preview_size: usize,
    /// All choices: `joint_size + preview_size`.
    pub size: usize,
    /// Team-preview orders, in index order.
    preview: Vec<TeamOrder>,
}

impl ActionSpace {
    pub fn new(format: &Format) -> ActionSpace {
        let active = format.active_per_side();
        let targets = if active > 1 { 5 } else { 1 };
        let gimmicks: Vec<Gimmick> = GIMMICK_ORDER.into_iter().filter(|&g| format.gimmicks.contains(g)).collect();
        let slot_size = MAX_MOVES * (1 + gimmicks.len()) * targets + MAX_TEAM + 1;
        let joint_size = slot_size.pow(active as u32);
        let (bring, lead_only) = match format.team_preview {
            TeamPreview::None => (0, false),
            TeamPreview::All => (1, true),
            TeamPreview::Pick(b) => ((b as usize).min(MAX_TEAM), false),
        };
        let mut preview = Vec::new();
        if format.team_preview.shown() {
            permutations(MAX_TEAM as u8, bring, &mut TeamOrder::new(), &mut preview);
        }
        let preview_size = preview.len();
        ActionSpace {
            active,
            targets,
            gimmicks,
            slot_size,
            joint_size,
            bring,
            lead_only,
            preview_size,
            size: joint_size + preview_size,
            preview,
        }
    }

    /// Whether this space can play `format`'s battles: a team preview
    /// order always brings `bring` Pokémon, so every team must have at
    /// least that many (the format's smallest team, `team_size.0`).
    pub fn check_format(&self, format: &Format) -> Result<(), String> {
        let smallest = format.team_size.0 as usize;
        if self.preview_size > 0 && smallest < self.bring {
            return Err(format!(
                "{}: teams of {smallest} Pokémon are smaller than team preview's pick of {}",
                format.id, self.bring
            ));
        }
        Ok(())
    }

    fn gimmick_count(&self) -> usize {
        1 + self.gimmicks.len()
    }

    /// First switch action.
    pub fn switch_base(&self) -> usize {
        MAX_MOVES * self.gimmick_count() * self.targets
    }

    /// The `pass` action.
    pub fn pass_index(&self) -> usize {
        self.switch_base() + MAX_TEAM
    }

    /// A slot action's index, if the space can express it.
    pub fn encode_slot(&self, action: &SlotAction) -> Option<usize> {
        match *action {
            SlotAction::Pass => Some(self.pass_index()),
            SlotAction::Switch(i) => ((i as usize) < MAX_TEAM).then(|| self.switch_base() + i as usize),
            SlotAction::Move { index, target, gimmick } => {
                let index = index as usize;
                if index >= MAX_MOVES {
                    return None;
                }
                let t = match target {
                    None => 0,
                    _ if self.targets == 1 => return None,
                    Some(Target::Foe(n)) if n < 2 => 1 + n as usize,
                    Some(Target::Ally(n)) if n < 2 => 3 + n as usize,
                    Some(_) => return None,
                };
                let g = match gimmick {
                    None => 0,
                    Some(g) => 1 + self.gimmicks.iter().position(|&x| x == g)?,
                };
                Some((index * self.gimmick_count() + g) * self.targets + t)
            }
        }
    }

    /// The slot action with index `i`.
    pub fn decode_slot(&self, i: usize) -> Option<SlotAction> {
        if i >= self.slot_size {
            return None;
        }
        if i == self.pass_index() {
            return Some(SlotAction::Pass);
        }
        if i >= self.switch_base() {
            return Some(SlotAction::Switch((i - self.switch_base()) as u8));
        }
        let t = i % self.targets;
        let g = (i / self.targets) % self.gimmick_count();
        let index = (i / self.targets / self.gimmick_count()) as u8;
        let target = match t {
            0 => None,
            1 | 2 => Some(Target::Foe(t as u8 - 1)),
            _ => Some(Target::Ally(t as u8 - 3)),
        };
        let gimmick = if g == 0 { None } else { Some(self.gimmicks[g - 1]) };
        Some(SlotAction::Move { index, target, gimmick })
    }

    /// The joint index of per-slot action indices (`slots.len() == active`).
    pub fn joint_index(&self, slots: &[usize]) -> Option<usize> {
        if slots.len() != self.active || slots.iter().any(|&a| a >= self.slot_size) {
            return None;
        }
        Some(slots.iter().fold(0, |acc, &a| acc * self.slot_size + a))
    }

    /// The per-slot actions of a joint index.
    pub fn split_joint(&self, mut j: usize) -> [usize; MAX_ACTIVE] {
        let mut out = [self.pass_index(); MAX_ACTIVE];
        for slot in (0..self.active).rev() {
            out[slot] = j % self.slot_size;
            j /= self.slot_size;
        }
        out
    }

    /// A choice's index, if the space has one for it. A team order longer
    /// than `bring` counts as its first `bring` entries (only those are
    /// brought, as in Showdown). In a
    /// [`lead_only`](ActionSpace::lead_only) space, where every Pokémon is
    /// brought, only the orders the space can name have an index: a lead
    /// followed by the others in team order
    /// ([`ActionSpace::preview_choice`]).
    pub fn encode(&self, choice: &Choice) -> Option<usize> {
        match choice {
            Choice::Actions(actions) => {
                if actions[self.active..].iter().any(|a| *a != SlotAction::Pass) {
                    return None;
                }
                let mut slots = [0; MAX_ACTIVE];
                for slot in 0..self.active {
                    slots[slot] = self.encode_slot(&actions[slot])?;
                }
                self.joint_index(&slots[..self.active])
            }
            Choice::Team(order) => {
                if self.preview_size == 0 || order.len() < self.bring {
                    return None;
                }
                if self.lead_only && self.preview_choice(order[0] as usize, order.len()) != Some(*choice) {
                    return None;
                }
                preview_rank(&order[..self.bring]).map(|r| self.joint_size + r)
            }
        }
    }

    /// The choice with index `i`. A team-preview choice is the one a full
    /// team of six makes; [`ActionSpace::decode_for`] gives a smaller
    /// team's.
    pub fn decode(&self, i: usize) -> Option<Choice> {
        self.decode_for(i, MAX_TEAM)
    }

    /// The choice with index `i` for a side whose team has `team_len`
    /// Pokémon: `None` for a team-preview choice that names a position the
    /// team doesn't have.
    pub fn decode_for(&self, i: usize, team_len: usize) -> Option<Choice> {
        if i < self.joint_size {
            let slots = self.split_joint(i);
            let mut actions = [SlotAction::Pass; MAX_ACTIVE];
            for slot in 0..self.active {
                actions[slot] = self.decode_slot(slots[slot])?;
            }
            Some(Choice::Actions(actions))
        } else {
            self.preview_choice(i - self.joint_size, team_len)
        }
    }

    /// Team-preview choice `k` (`k < preview_size`) of a team of
    /// `team_len` Pokémon: order `k` ([`ActionSpace::preview_order`]) if
    /// the team has every position it names; in a
    /// [`lead_only`](ActionSpace::lead_only) space, the whole team with
    /// position `k` first and the others in team order (Showdown's
    /// `chooseTeam` fills a short `team` choice up this way).
    pub fn preview_choice(&self, k: usize, team_len: usize) -> Option<Choice> {
        let order = self.preview.get(k)?;
        if !order.iter().all(|&i| (i as usize) < team_len) {
            return None;
        }
        if !self.lead_only {
            return Some(Choice::Team(*order));
        }
        let mut full = *order;
        for i in 0..team_len.min(MAX_TEAM) as u8 {
            if !full.contains(&i) {
                full.push(i);
            }
        }
        Some(Choice::Team(full))
    }

    /// Fills `out` (length `active * slot_size`, cleared first) with the
    /// actions each slot may take on its own. Empty for team preview and
    /// waiting sides.
    pub fn fill_slot_mask(&self, legal: &LegalActions, out: &mut [bool]) {
        out.fill(false);
        if !matches!(legal.request(), Request::Move | Request::ForceSwitch { .. }) {
            return;
        }
        for slot in 0..self.active {
            for action in legal.slot(slot) {
                if let Some(i) = self.encode_slot(action) {
                    out[slot * self.slot_size + i] = true;
                }
            }
        }
    }

    /// Fills `out` (length `size`, cleared first) with the jointly legal
    /// choices.
    pub fn fill_mask(&self, legal: &LegalActions, team_len: usize, out: &mut [bool]) {
        out.fill(false);
        match legal.request() {
            Request::Wait => {}
            Request::TeamPreview { .. } => self.fill_preview_mask(legal, team_len, &mut out[self.joint_size..]),
            Request::Move | Request::ForceSwitch { .. } => {
                for choice in legal.joint() {
                    if let Some(i) = self.encode(&choice) {
                        out[i] = true;
                    }
                }
            }
        }
    }

    /// Fills `out` (length `preview_size`, cleared first) with the legal
    /// team-preview orders, by their index within the preview range. Empty
    /// unless the side is at team preview.
    pub fn fill_preview_mask(&self, legal: &LegalActions, team_len: usize, out: &mut [bool]) {
        out.fill(false);
        if !matches!(legal.request(), Request::TeamPreview { .. }) {
            return;
        }
        // A full team can bring any order (every position is below 6).
        if team_len >= MAX_TEAM {
            out[..self.preview.len()].fill(true);
            return;
        }
        for (k, order) in self.preview.iter().enumerate() {
            out[k] = order.iter().all(|&i| (i as usize) < team_len);
        }
    }

    /// The indices of the jointly legal choices, ascending.
    pub fn legal_indices(&self, legal: &LegalActions, team_len: usize) -> Vec<usize> {
        let mut mask = vec![false; self.size];
        self.fill_mask(legal, team_len, &mut mask);
        mask.iter().enumerate().filter(|&(_, &b)| b).map(|(i, _)| i).collect()
    }

    /// Team-preview order `k` (`k < preview_size`): the `bring` team
    /// positions choice `k` names ([`ActionSpace::preview_choice`] is the
    /// choice itself).
    pub fn preview_order(&self, k: usize) -> Option<&TeamOrder> {
        self.preview.get(k)
    }

    /// Every factored action described by integers (see [`ActionTable`]):
    /// the slot actions, then the team-preview orders. Static: build it
    /// once.
    pub fn action_table(&self) -> ActionTable {
        let mut rows = Vec::with_capacity(self.slot_size + self.preview_size);
        for i in 0..self.slot_size {
            let mut r = [-1i16; action_col::COUNT];
            match self.decode_slot(i).expect("a slot action index") {
                SlotAction::Move { index, .. } => {
                    r[action_col::KIND] = action_kind::MOVE;
                    r[action_col::MOVE_SLOT] = i16::from(index);
                    r[action_col::TARGET] = (i % self.targets) as i16;
                    r[action_col::GIMMICK] = ((i / self.targets) % self.gimmick_count()) as i16;
                }
                SlotAction::Switch(k) => {
                    r[action_col::KIND] = action_kind::SWITCH;
                    r[action_col::SWITCH] = i16::from(k);
                }
                SlotAction::Pass => r[action_col::KIND] = action_kind::PASS,
            }
            rows.push(r);
        }
        for k in 0..self.preview_size {
            let mut r = [-1i16; action_col::COUNT];
            r[action_col::KIND] = action_kind::PREVIEW;
            r[action_col::PREVIEW] = k as i16;
            rows.push(r);
        }
        ActionTable { rows, slot_size: self.slot_size }
    }

    /// [`ActionSpace::fill_slot_mask`], with switch `i` (a team position)
    /// at the switch index `switch_index(i)` instead of `i` (`None`: left
    /// out). Returns the fewest switches a joint action must make (the
    /// forced-switch rule of [`ActionSpace::pair_legal`]): `bench` is the
    /// side's Pokémon that can switch in.
    pub fn fill_slot_mask_by(
        &self,
        legal: &LegalActions,
        bench: usize,
        switch_index: impl Fn(u8) -> Option<usize>,
        out: &mut [bool],
    ) -> u8 {
        out.fill(false);
        let forced = match legal.request() {
            Request::Move => 0,
            Request::ForceSwitch { slots } => slots[..self.active].iter().filter(|&&f| f).count(),
            _ => return 0,
        };
        for slot in 0..self.active {
            for action in legal.slot(slot) {
                let i = match *action {
                    SlotAction::Switch(p) => switch_index(p).map(|k| self.switch_base() + k),
                    _ => self.encode_slot(action),
                };
                if let Some(i) = i.filter(|&i| i < self.slot_size) {
                    out[slot * self.slot_size + i] = true;
                }
            }
        }
        // `check_joint`: every forced slot switches while replacements
        // remain. Only one replacement for two holes needs a rule beyond
        // the per-slot masks (with more, neither slot may pass).
        forced.min(bench) as u8
    }

    /// Fills a player's mask row (`layout.width(self)` values) with the
    /// actions `legal` lists (what its request shows), switches named as
    /// `by` says (`own`: the player's side); returns the fewest switches a
    /// joint action must make ([`StepOut::min_switches`](crate::vec::StepOut)).
    /// All false for a side that waits. The vector environment and the
    /// channel session both write their masks with this.
    pub fn fill_player_mask(
        &self,
        layout: MaskLayout,
        by: SwitchBy,
        legal: &LegalActions,
        own: &OwnOverlay,
        row: &mut [bool],
    ) -> u8 {
        let slots = self.active * self.slot_size;
        let index = |pos: u8| switch_index(by, own, pos);
        match (layout, legal.request()) {
            (_, Request::Wait) => {
                row.fill(false);
                0
            }
            (layout, Request::TeamPreview { .. }) => {
                let off = if layout == MaskLayout::Factored { slots } else { self.joint_size };
                row[..off].fill(false);
                self.fill_preview_mask(legal, legal.team_len(), &mut row[off..]);
                0
            }
            (MaskLayout::Factored, _) => {
                row[slots..].fill(false);
                self.fill_slot_mask_by(legal, legal.bench(), index, &mut row[..slots])
            }
            (MaskLayout::Joint, _) => {
                let mut mask = [false; MAX_SLOT_MASK];
                let min = self.fill_slot_mask_by(legal, legal.bench(), index, &mut mask[..slots]);
                self.fill_joint_from_slots(&mask, min, &mut row[..self.joint_size]);
                row[self.joint_size..].fill(false);
                min
            }
        }
    }

    /// The choice a player's actions name (`layout.actions(self)` values:
    /// slot-action indices with factored masks, at team preview the
    /// order's index in slot 0; one choice index with joint masks), if its
    /// mask ([`ActionSpace::fill_player_mask`] for `legal`) allows it. A
    /// switch goes to the team position the action names (`by`).
    #[must_use]
    pub fn decode_player(
        &self,
        layout: MaskLayout,
        by: SwitchBy,
        legal: &LegalActions,
        own: &OwnOverlay,
        acts: &[i64],
    ) -> Option<Choice> {
        let index = |a: i64| usize::try_from(a).ok();
        let first = index(*acts.first()?);
        // Team preview: an order's index, in the preview part.
        let preview = |k: usize| self.preview_choice(k, legal.team_len());
        let mut slots = [0usize; MAX_ACTIVE];
        match (layout, legal.request()) {
            (_, Request::Wait) => return None,
            (MaskLayout::Factored, Request::TeamPreview { .. }) => return preview(first?),
            (MaskLayout::Joint, Request::TeamPreview { .. }) => return preview(first?.checked_sub(self.joint_size)?),
            (MaskLayout::Factored, _) => {
                if acts.len() < self.active {
                    return None;
                }
                for (s, &a) in slots.iter_mut().zip(acts) {
                    *s = index(a)?;
                }
            }
            (MaskLayout::Joint, _) => slots = self.split_joint(first.filter(|&a| a < self.joint_size)?),
        }
        let mut mask = [false; MAX_SLOT_MASK];
        let width = self.active * self.slot_size;
        let min = self.fill_slot_mask_by(legal, legal.bench(), |pos| switch_index(by, own, pos), &mut mask[..width]);
        if !self.pair_legal(&mask, min, slots[0], slots[1]) {
            return None;
        }
        let mut actions = [SlotAction::Pass; MAX_ACTIVE];
        for (action, &a) in actions.iter_mut().zip(&slots[..self.active]) {
            *action = match self.decode_slot(a)? {
                SlotAction::Switch(k) if by == SwitchBy::Row => {
                    SlotAction::Switch(own.by_id(k).map(|(pos, _)| pos as u8)?)
                }
                other => other,
            };
        }
        Some(Choice::Actions(actions))
    }

    /// Whether slot actions `a0` and `a1` make a legal doubles choice,
    /// given the side's factored slot masks (`active * slot_size` values)
    /// and `min_switches` ([`StepOut::min_switches`](crate::vec::StepOut)):
    /// each is in its slot's mask, they don't switch to the same Pokémon,
    /// at most one uses a gimmick, and together they make at least
    /// `min_switches` switches. This is [`LegalActions::check`] for the
    /// actions the masks list. In singles, `a1` is ignored.
    pub fn pair_legal(&self, slot_masks: &[bool], min_switches: u8, a0: usize, a1: usize) -> bool {
        let in_mask = |slot: usize, a: usize| a < self.slot_size && slot_masks[slot * self.slot_size + a];
        if !in_mask(0, a0) {
            return false;
        }
        if self.active < 2 {
            return true;
        }
        if !in_mask(1, a1) {
            return false;
        }
        let switch = |a: usize| (self.switch_base()..self.pass_index()).contains(&a);
        let gimmick = |a: usize| a < self.switch_base() && !(a / self.targets).is_multiple_of(self.gimmick_count());
        let same_switch = switch(a0) && a0 == a1;
        let two_gimmicks = gimmick(a0) && gimmick(a1);
        let switches = usize::from(switch(a0)) + usize::from(switch(a1));
        !same_switch && !two_gimmicks && switches >= min_switches as usize
    }

    /// Fills `out` (`slot_size` values) with the second slot's actions
    /// that make a legal choice with the first slot's `a0`
    /// ([`ActionSpace::pair_legal`]), for autoregressive policies. Empty
    /// if `a0` itself isn't legal.
    pub fn pair_mask(&self, slot_masks: &[bool], min_switches: u8, a0: usize, out: &mut [bool]) {
        for (a1, o) in out.iter_mut().enumerate().take(self.slot_size) {
            *o = self.pair_legal(slot_masks, min_switches, a0, a1);
        }
    }

    /// Fills `out` (`joint_size` values) with the joint choices the
    /// factored slot masks allow ([`ActionSpace::pair_legal`]), without
    /// allocating.
    pub fn fill_joint_from_slots(&self, slot_masks: &[bool], min_switches: u8, out: &mut [bool]) {
        out.fill(false);
        if self.active < 2 {
            out[..self.slot_size].copy_from_slice(&slot_masks[..self.slot_size]);
            return;
        }
        for a0 in (0..self.slot_size).filter(|&a| slot_masks[a]) {
            for a1 in (0..self.slot_size).filter(|&a| slot_masks[self.slot_size + a]) {
                out[a0 * self.slot_size + a1] = self.pair_legal(slot_masks, min_switches, a0, a1);
            }
        }
    }
}

/// The rank of an ordered selection of distinct positions below 6 among all
/// ordered selections of its length, in lexicographic order.
fn preview_rank(order: &[u8]) -> Option<usize> {
    let n = MAX_TEAM;
    let k = order.len();
    let mut used = 0u8;
    let mut rank = 0;
    for (pos, &x) in order.iter().enumerate() {
        if x as usize >= n || used & (1 << x) != 0 {
            return None;
        }
        // Selections that start like this one but with a smaller unused
        // position here: each is followed by P(n - pos - 1, k - pos - 1).
        let smaller = (0..x).filter(|&y| used & (1 << y) == 0).count();
        rank += smaller * falling(n - pos - 1, k - pos - 1);
        used |= 1 << x;
    }
    Some(rank)
}

/// `n * (n - 1) * ... * (n - k + 1)`.
fn falling(n: usize, k: usize) -> usize {
    (0..k).map(|i| n - i).product()
}

fn permutations(n: u8, k: usize, order: &mut TeamOrder, out: &mut Vec<TeamOrder>) {
    if order.len() == k {
        out.push(*order);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        let s = ActionSpace::new(&Format::gen9_singles());
        assert_eq!((s.slot_size, s.joint_size, s.preview_size, s.size), (15, 15, 0, 15));
        let d = ActionSpace::new(&Format::gen9_vgc());
        assert_eq!((d.slot_size, d.joint_size, d.preview_size, d.size), (47, 47 * 47, 360, 47 * 47 + 360));
        let c = ActionSpace::new(&Format::champions_vgc());
        assert_eq!(c.gimmicks, vec![Gimmick::MegaEvolve]);
        assert_eq!(c.size, d.size);
    }

    #[test]
    fn slot_round_trip() {
        for format in Format::presets() {
            let s = ActionSpace::new(format);
            for i in 0..s.slot_size {
                let a = s.decode_slot(i).unwrap();
                assert_eq!(s.encode_slot(&a), Some(i), "{} {i} {a:?}", format.id);
            }
            assert_eq!(s.decode_slot(s.slot_size), None);
        }
    }

    #[test]
    fn choice_round_trip() {
        for format in Format::presets() {
            let s = ActionSpace::new(format);
            for i in (0..s.size).step_by(7).chain([s.size - 1]) {
                let c = s.decode(i).unwrap();
                assert_eq!(s.encode(&c), Some(i), "{} {i} {c:?}", format.id);
            }
            assert_eq!(s.decode(s.size), None);
        }
    }

    /// Every row of the action table describes the action its index
    /// decodes to.
    #[test]
    fn action_table_describes_each_action() {
        for format in Format::presets() {
            let s = ActionSpace::new(format);
            let t = s.action_table();
            assert_eq!(t.len(), s.slot_size + s.preview_size, "{}", format.id);
            for (i, r) in t.rows.iter().enumerate() {
                let rebuilt = match r[action_col::KIND] {
                    action_kind::MOVE => {
                        assert_eq!((r[action_col::SWITCH], r[action_col::PREVIEW]), (-1, -1));
                        let target = match r[action_col::TARGET] {
                            0 => None,
                            t @ 1..=2 => Some(Target::Foe(t as u8 - 1)),
                            t @ 3..=4 => Some(Target::Ally(t as u8 - 3)),
                            t => panic!("target {t}"),
                        };
                        let g = r[action_col::GIMMICK] as usize;
                        let gimmick = if g == 0 { None } else { Some(s.gimmicks[g - 1]) };
                        let action = SlotAction::Move { index: r[action_col::MOVE_SLOT] as u8, target, gimmick };
                        assert_eq!(s.decode_slot(i), Some(action), "{} {i}", format.id);
                        continue;
                    }
                    action_kind::SWITCH => SlotAction::Switch(r[action_col::SWITCH] as u8),
                    action_kind::PASS => SlotAction::Pass,
                    action_kind::PREVIEW => {
                        let k = r[action_col::PREVIEW] as usize;
                        assert_eq!(i, s.slot_size + k);
                        assert!(s.preview_order(k).is_some());
                        assert_eq!(r[..action_col::PREVIEW], [action_kind::PREVIEW, -1, -1, -1, -1]);
                        continue;
                    }
                    k => panic!("kind {k}"),
                };
                assert_eq!(r[action_col::MOVE_SLOT..=action_col::GIMMICK], [-1, -1, -1]);
                assert_eq!(s.decode_slot(i), Some(rebuilt), "{} {i}", format.id);
            }
            let kinds = t.column(action_col::KIND);
            assert_eq!(kinds.iter().filter(|&&k| k == action_kind::SWITCH).count(), MAX_TEAM);
            assert_eq!(kinds.iter().filter(|&&k| k == action_kind::PASS).count(), 1);
            assert_eq!(kinds.iter().filter(|&&k| k == action_kind::PREVIEW).count(), s.preview_size);
            if s.targets == 1 {
                assert!(t.column(action_col::TARGET).iter().all(|&x| x <= 0));
            }
        }
        // Champions: the gimmick column names Mega Evolution.
        let c = ActionSpace::new(&Format::champions_vgc());
        let t = c.action_table();
        let mega = t.rows.iter().position(|r| r[action_col::GIMMICK] == 1).unwrap();
        assert!(matches!(c.decode_slot(mega), Some(SlotAction::Move { gimmick: Some(Gimmick::MegaEvolve), .. })));
    }

    #[test]
    fn relative_targets() {
        assert_eq!([0, 1, 2, 3, 4].map(|t| relative_target(0, t)), [0, 1, 2, 3, 4].map(Some));
        assert_eq!([0, 1, 2, 3, 4].map(|t| relative_target(1, t)), [0, 1, 2, 4, 3].map(Some));
        assert_eq!(relative_target(0, 5), None);
        assert_eq!(relative_target(2, 0), None);
    }

    #[test]
    fn preview_ranks_are_lexicographic() {
        let s = ActionSpace::new(&Format::gen9_vgc());
        for k in 0..s.preview_size {
            assert_eq!(preview_rank(s.preview_order(k).unwrap()), Some(k));
        }
        assert_eq!(s.encode(&Choice::team(&[0, 1, 2, 3])), Some(s.joint_size));
        assert_eq!(s.encode(&Choice::team(&[5, 4, 3, 2, 1, 0])), Some(s.size - 1));
        assert_eq!(s.encode(&Choice::team(&[0, 0, 1, 2])), None);
    }

    /// gen9ou's team preview: six choices, each a lead; the rest of the
    /// team follows in order, for teams of any size.
    #[test]
    fn a_lead_only_preview_names_the_lead() {
        let s = ActionSpace::new(&Format::gen9_ou());
        assert!(s.lead_only);
        assert_eq!((s.slot_size, s.joint_size, s.bring, s.preview_size, s.size), (15, 15, 1, 6, 21));
        assert_eq!(s.decode(s.joint_size + 2), Some(Choice::team(&[2, 0, 1, 3, 4, 5])));
        assert_eq!(s.decode_for(s.joint_size, 6), Some(Choice::team(&[0, 1, 2, 3, 4, 5])));
        assert_eq!(s.preview_choice(2, 4), Some(Choice::team(&[2, 0, 1, 3])));
        assert_eq!(s.preview_choice(0, 1), Some(Choice::team(&[0])));
        assert_eq!(s.preview_choice(4, 4), None);
        assert_eq!(s.preview_choice(6, 6), None);
        for k in 0..6 {
            assert_eq!(s.preview_order(k).map(|o| o.to_vec()), Some(vec![k as u8]));
            let choice = s.preview_choice(k, 6).unwrap();
            assert_eq!(s.encode(&choice), Some(s.joint_size + k));
        }
        // Smaller teams' orders have their lead's index too; an order
        // the space can't name has none.
        assert_eq!(s.encode(&Choice::team(&[2, 0, 1, 3])), Some(s.joint_size + 2));
        assert_eq!(s.encode(&Choice::team(&[0])), Some(s.joint_size));
        assert_eq!(s.encode(&Choice::team(&[3, 5, 4, 2, 1, 0])), None);
        assert_eq!(s.encode(&Choice::team(&[3, 0, 1, 2, 4, 4])), None);
        // The spaces that pick are not lead-only.
        for f in [Format::gen9_vgc(), Format::champions_bss(), Format::gen9_singles()] {
            assert!(!ActionSpace::new(&f).lead_only, "{}", f.id);
        }
    }
}
