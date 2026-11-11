//! The feature audit: each derived value of a player's view (a counter,
//! an elapsed duration, PP spent, a flag an event sets) against its
//! *reference*: what a perfect observer of the public log computes under
//! the documented meaning (`pikapro_view::{MonView, ObservedCounters,
//! PublicField}`).
//!
//! Soundness ([`pikapro_view::check::check_sound`]) checks that the facts
//! a view claims are true; this checks that the numbers mean what their
//! documentation says. A reference is computed from the engine's state and
//! events, never from the view:
//!
//! * **exact** references, where the engine holds the public quantity:
//!   `turns_active` (Showdown's `activeTurns`), the toxic counter, the
//!   ends of turn left on a foreseen attack or a Wish, the Rooms' and
//!   Gravity's elapsed turns (their duration is fixed), sleep and freeze
//!   attempts (the `cant|slp` and `cant|frz` lines of the Pokémon since
//!   its status started, checked in turn against Rest's fixed 3 turns and
//!   Champions' 3 attempts to thaw), confusion's checks
//!   (`-activate|<pokemon>|confusion`), Rest's sleep, PP spent on a
//!   revealed move, the last move;
//! * **drift** references for an elapsed duration whose total is hidden
//!   (a Light Clay, a Damp Rock) or drawn: the engine's turns left plus the
//!   view's turns elapsed is the same at every decision point of one
//!   episode of the condition (Taunt, Encore, Disable, Yawn, Heal Block,
//!   Throat Chop, Magnet Rise, a partial trap, Uproar, Slow Start, Syrup
//!   Bomb, Lock-On, screens, Tailwind, the weather, the terrain), between
//!   the residuals and the next `|turn|` too.
//!
//! Every value is read from both players' views as an encoder reads them
//! ([`pikapro_rl::encode::ViewCtx`]): the Pokémon's own row and the
//! opponent's row of it. A duration reads -1 while its condition isn't up.
//!
//! Illusion fools the view as it fools a client: a side with an Illusion
//! user is left out (and PP Pressure took, facing one). What the log
//! leaves in doubt is measured apart, in a rule named `... (info)`, which
//! never fails: Pressure's share of the PP of a move whose target
//! `[still]` hid, in doubles (a charged move's release names it, unless a
//! foe was down, a redirection was up or Ally Switch moved its user), or
//! against a Pressure that never showed (it came in under Neutralizing
//! Gas, which a transformed holder ends without a line); a
//! Choice lock's failure (`|move|`, `[still]`, `-fail`: no PP, no
//! `lastMove`), which a move that failed after spending its PP shows too;
//! and a rampage's first turn, which the flag `vol_locked_move` doesn't
//! show by its documentation (it marks a `[from] lockedmove`
//! continuation). [`FeatureAudit::rules`] counts each
//! rule's checks and differences.
//! `cargo run --release -p pikapro-conformance --bin feature_audit` runs it
//! at scale, over a teams file too.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use pikapro::event::{BattleEvent, FieldEffect};
use pikapro::state::{SideCondition, Volatiles};
use pikapro::{Battle, Gimmick, SideId, Status};
use pikapro_data::gen9::{abilities, moves};
use pikapro_data::{MoveFlags, MoveTarget};
use pikapro_rl::encode::{MonRef, ViewCtx};
use pikapro_rl::tables::Tables;
use pikapro_view::{MonFlags, ObservedCounters, OwnTeam, PlayerView, PublicVolatiles as V, StatusKind};

use super::{Checks, Driver, Failure, Played, Setup, overlays, play};

/// One rule's count.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    /// Values compared.
    pub checked: u64,
    /// Values that differed.
    pub bad: u64,
    /// Battles with at least one difference.
    pub battles: u64,
    /// The first differences: `format seed turn: detail`.
    pub examples: Vec<String>,
}

/// The audit's totals over many battles.
#[derive(Clone, Debug, Default)]
pub struct FeatureAudit {
    pub rules: BTreeMap<&'static str, Tally>,
    /// Informative distributions (not rules): name -> value -> count.
    pub histograms: BTreeMap<&'static str, BTreeMap<i64, u64>>,
    pub battles: u64,
    /// Decision points audited.
    pub points: u64,
    /// Battles with a side left out (an Illusion user).
    pub skipped_sides: u64,
    pub max_examples: usize,
}

impl FeatureAudit {
    pub fn new(max_examples: usize) -> FeatureAudit {
        FeatureAudit { max_examples, ..FeatureAudit::default() }
    }

    /// Adds another audit's totals.
    pub fn merge(&mut self, other: FeatureAudit) {
        for (k, t) in other.rules {
            let e = self.rules.entry(k).or_default();
            e.checked += t.checked;
            e.bad += t.bad;
            e.battles += t.battles;
            for x in t.examples {
                if e.examples.len() < self.max_examples.max(other.max_examples) {
                    e.examples.push(x);
                }
            }
        }
        for (k, h) in other.histograms {
            let e = self.histograms.entry(k).or_default();
            for (v, n) in h {
                *e.entry(v).or_default() += n;
            }
        }
        self.battles += other.battles;
        self.points += other.points;
        self.skipped_sides += other.skipped_sides;
    }

    /// The rules that failed. An `(info)` rule measures a documented
    /// meaning against a more useful one; it never fails.
    pub fn broken(&self) -> Vec<&'static str> {
        self.rules.iter().filter(|(k, t)| t.bad > 0 && !k.ends_with("(info)")).map(|(k, _)| *k).collect()
    }

    /// A text report.
    pub fn report(&self, show: usize) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} battles, {} decision points ({} sides with Illusion left out)",
            self.battles, self.points, self.skipped_sides
        );
        let _ = writeln!(out, "{:<34} {:>10} {:>9} {:>9}  battles%", "rule", "checked", "bad", "battles");
        for (k, t) in &self.rules {
            let pct = if self.battles > 0 { 100.0 * t.battles as f64 / self.battles as f64 } else { 0.0 };
            let _ = writeln!(out, "{k:<34} {:>10} {:>9} {:>9}  {pct:.2}", t.checked, t.bad, t.battles);
            for x in t.examples.iter().take(show) {
                let _ = writeln!(out, "    {x}");
            }
        }
        for (k, h) in &self.histograms {
            let _ = writeln!(out, "{k}: {h:?}");
        }
        out
    }
}

/// The per-Pokémon durations checked for drift: the engine's turns left
/// and the view's turns elapsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Dur {
    Taunt,
    Encore,
    Disable,
    Yawn,
    HealBlock,
    ThroatChop,
    MagnetRise,
    PartialTrap,
    Uproar,
    SlowStart,
    SyrupBomb,
    LockOn,
}

const DURS: [Dur; 12] = [
    Dur::Taunt,
    Dur::Encore,
    Dur::Disable,
    Dur::Yawn,
    Dur::HealBlock,
    Dur::ThroatChop,
    Dur::MagnetRise,
    Dur::PartialTrap,
    Dur::Uproar,
    Dur::SlowStart,
    Dur::SyrupBomb,
    Dur::LockOn,
];

impl Dur {
    fn rule(self) -> &'static str {
        match self {
            Dur::Taunt => "taunt_turns drift",
            Dur::Encore => "encore_turns drift",
            Dur::Disable => "disable_turns drift",
            Dur::Yawn => "yawn_turns drift",
            Dur::HealBlock => "heal_block_turns drift",
            Dur::ThroatChop => "throat_chop_turns drift",
            Dur::MagnetRise => "magnet_rise_turns drift",
            Dur::PartialTrap => "partial_trap_turns drift",
            Dur::Uproar => "uproar_turns drift",
            Dur::SlowStart => "slow_start_turns drift",
            Dur::SyrupBomb => "syrup_bomb_turns drift",
            Dur::LockOn => "lock_on_turns drift",
        }
    }

    fn total(self) -> &'static str {
        match self {
            Dur::Taunt => "total taunt",
            Dur::Encore => "total encore",
            Dur::Disable => "total disable",
            Dur::Yawn => "total yawn",
            Dur::HealBlock => "total heal block",
            Dur::ThroatChop => "total throat chop",
            Dur::MagnetRise => "total magnet rise",
            Dur::PartialTrap => "total partial trap",
            Dur::Uproar => "total uproar",
            Dur::SlowStart => "total slow start",
            Dur::SyrupBomb => "total syrup bomb",
            Dur::LockOn => "total lock-on",
        }
    }

    /// The view's turns elapsed for a row's Pokémon, -1 while the
    /// condition isn't up (or off the field).
    fn elapsed(self, m: &MonRef<'_>) -> i64 {
        let Some(a) = m.active else { return -1 };
        let n: &ObservedCounters = &a.counters;
        let (bit, turns) = match self {
            Dur::Taunt => (V::TAUNT, n.taunt_turns),
            Dur::Encore => (V::ENCORE, n.encore_turns),
            Dur::Disable => (V::DISABLE, n.disable_turns),
            Dur::Yawn => (V::YAWN, n.yawn_turns),
            Dur::HealBlock => (V::HEAL_BLOCK, n.heal_block_turns),
            Dur::ThroatChop => (V::THROAT_CHOP, n.throat_chop_turns),
            Dur::MagnetRise => (V::MAGNET_RISE, n.magnet_rise_turns),
            Dur::PartialTrap => (V::PARTIALLY_TRAPPED, n.partial_trap_turns),
            Dur::Uproar => (V::UPROAR, n.uproar_turns),
            Dur::SlowStart => (V::SLOW_START, n.slow_start_turns),
            Dur::SyrupBomb => (V::SYRUP_BOMB, n.syrup_bomb_turns),
            Dur::LockOn => (V::LOCK_ON, n.lock_on_turns),
        };
        turns_if(a.volatiles.has(bit), turns)
    }

    /// The engine's turns left, if the condition is up.
    fn left(self, m: &pikapro::PokemonState) -> Option<i64> {
        let c = &m.volatile_counters;
        let (bit, n) = match self {
            Dur::Taunt => (Volatiles::TAUNT, c.taunt),
            Dur::Encore => (Volatiles::ENCORE, c.encore),
            Dur::Disable => (Volatiles::DISABLE, c.disable),
            Dur::Yawn => (Volatiles::YAWN, c.yawn),
            Dur::HealBlock => (Volatiles::HEAL_BLOCK, c.heal_block),
            Dur::ThroatChop => (Volatiles::THROAT_CHOP, c.throat_chop),
            Dur::MagnetRise => (Volatiles::MAGNET_RISE, c.magnet_rise),
            Dur::PartialTrap => (Volatiles::PARTIALLY_TRAPPED, c.partially_trapped),
            Dur::Uproar => (Volatiles::UPROAR, c.uproar),
            Dur::SyrupBomb => (Volatiles::SYRUP_BOMB, c.syrup_bomb),
            Dur::LockOn => (Volatiles::LOCK_ON, c.lock_on_turns),
            Dur::SlowStart => {
                let n = c.ability_state.slow_start;
                return (m.ability == abilities::SLOW_START && n > 0).then_some(i64::from(n));
            }
        };
        m.volatiles.has(bit).then_some(i64::from(n))
    }
}

/// A field or side duration checked for drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FieldDur {
    Weather(u8),
    Terrain(u8),
    Side(SideId, u8),
}

/// What the audit keeps through one battle.
struct BattleRef {
    format: String,
    seed: u64,
    /// Sleep attempts (`cant|slp`) since it fell asleep, by side and id.
    sleep: [[u8; 6]; 2],
    /// Its sleep is Rest's.
    rest: [[bool; 6]; 2],
    /// Attempts to move while frozen (`cant|frz`) since it froze.
    freeze: [[u8; 6]; 2],
    /// Moves whose PP the log left in doubt, by side, id and the move
    /// whose PP it spent: in doubles, a move whose target `[still]` hid,
    /// against a side with Pressure (a charged move's release names it).
    pp_doubt: BTreeSet<(usize, u8, pikapro_data::MoveId)>,
    /// Charge turns that hid their target, until their release.
    charge_doubt: BTreeSet<(usize, u8, pikapro_data::MoveId)>,
    /// The last move line was such a release: a redirection shown after it
    /// (Storm Drain, Lightning Rod) leaves it in doubt.
    released: Option<(usize, u8, pikapro_data::MoveId)>,
    /// A move line that a Choice lock (or Gorilla Tactics) failed at once
    /// (`choicelock.onBeforeMove`: `|move|`, `[still]`, `-fail`; no PP, no
    /// `lastMove`), which the log doesn't tell from a move that failed
    /// after spending its PP unless the item is known: by side, id and
    /// move, and the Pokémon whose last move is in doubt until it leaves.
    lock_doubt: BTreeSet<(usize, u8, pikapro_data::MoveId)>,
    last_move_doubt: BTreeSet<(usize, u8)>,
    /// The battle is over (the last decision point).
    over: bool,
    /// A Pokémon of this side has shown Pressure (`-ability|...|Pressure`).
    pressure: [bool; 2],
    /// Its set's ability (it may have lost it since: Magic Guard hides
    /// toxic's residuals while it has it).
    set_ability: [[pikapro_data::AbilityId; 6]; 2],
    /// It was seen badly poisoned with Magic Guard or Poison Heal since it
    /// came in (the residuals it hid still count).
    toxic_hidden: [[bool; 6]; 2],
    /// Its set's species is a Mega forme (it starts the battle in it).
    pre_mega: [[bool; 6]; 2],
    /// Confusion checks (`-activate|<pokemon>|confusion`) since it was
    /// confused, by side and id.
    confusion: [[u8; 6]; 2],
    /// The Pokémon (its id) last sent into each slot, by side.
    in_slot: [[u8; 2]; 2],
    /// Each slot's Pokémon fainted (and nothing has come in since).
    down: [[bool; 2]; 2],
    /// A side's Pokémon is the center of attention this turn (Follow Me,
    /// Rage Powder).
    center: [bool; 2],
    /// The Pokémon showed Pressure (`-ability|<pokemon>|Pressure`), by side
    /// and id.
    pressure_shown: [[bool; 6]; 2],
    /// Drift episodes: (side, id, duration) -> total seen.
    mon_totals: BTreeMap<(usize, u8, Dur), i64>,
    field_totals: BTreeMap<FieldDur, i64>,
    /// Rules broken in this battle.
    broken: BTreeSet<&'static str>,
    /// The last `|turn|` and `|upkeep|` event indices.
    last_turn: Option<usize>,
    last_upkeep: Option<usize>,
    illusion: [bool; 2],
}

/// Plays one battle and audits every decision point.
pub fn audit_battle(setup: &Setup, driver: Driver, audit: &mut FeatureAudit) -> Result<Played, Failure> {
    let format = &setup.format;
    let tables = Tables::for_format(format);
    let battle0 = setup.try_battle().map_err(|e| Failure::new("engine", "teams", e))?;
    let illusion =
        SideId::BOTH.map(|s| battle0.state().side(s).team.iter().any(|m| m.base_ability == abilities::ILLUSION));
    let mut r = BattleRef {
        format: format.id.to_string(),
        seed: setup.seed,
        sleep: [[0; 6]; 2],
        rest: [[false; 6]; 2],
        freeze: [[0; 6]; 2],
        pp_doubt: BTreeSet::new(),
        charge_doubt: BTreeSet::new(),
        released: None,
        lock_doubt: BTreeSet::new(),
        last_move_doubt: BTreeSet::new(),
        over: false,
        pressure: [false; 2],
        confusion: [[0; 6]; 2],
        in_slot: [[u8::MAX; 2]; 2],
        down: [[false; 2]; 2],
        center: [false; 2],
        pressure_shown: [[false; 6]; 2],
        toxic_hidden: [[false; 6]; 2],
        set_ability: SideId::BOTH.map(|s| {
            let mut out = [pikapro_data::AbilityId(0); 6];
            for m in battle0.state().side(s).team.iter() {
                out[m.id as usize % 6] = m.base_ability;
            }
            out
        }),
        pre_mega: SideId::BOTH.map(|s| {
            let mut out = [false; 6];
            for m in battle0.state().side(s).team.iter() {
                out[m.id as usize % 6] = format.dex.species(m.species).forme.contains("Mega");
            }
            out
        }),
        mon_totals: BTreeMap::new(),
        field_totals: BTreeMap::new(),
        broken: BTreeSet::new(),
        last_turn: None,
        last_upkeep: None,
        illusion,
    };
    let mut teams = setup.sheets.each_ref().map(|s| OwnTeam::from_team(&s.team, format));
    audit.skipped_sides += illusion.iter().filter(|&&x| x).count() as u64;
    let pressure_team =
        SideId::BOTH.map(|s| battle0.state().side(s).team.iter().any(|m| m.base_ability == abilities::PRESSURE));
    let played = play(setup, driver, Checks::NONE, |u| {
        events(&mut r, u.battle, u.start, pressure_team);
        let own = overlays(u.battle);
        for side in SideId::BOTH {
            teams[side.index()].observe(&own[side.index()]);
        }
        let views = SideId::BOTH.map(|s| PlayerView::new(s, u.tracker, &own[s.index()]).with_team(&teams[s.index()]));
        let ctxs = [ViewCtx::new(&views[0], &tables), ViewCtx::new(&views[1], &tables)];
        audit.points += 1;
        point(&mut r, audit, u.battle, &views, &ctxs);
        Ok(())
    })?;
    audit.battles += 1;
    for rule in &r.broken {
        audit.rules.entry(rule).or_default().battles += 1;
    }
    Ok(played)
}

/// Reads one update's events: the sleep and freeze attempts, the
/// confusion checks, the PP the log leaves in doubt, the switch-ins that
/// end an episode, and where the turn is. `pressure` is whether a side's
/// team has a Pokémon with Pressure.
fn events(r: &mut BattleRef, battle: &Battle, start: usize, pressure: [bool; 2]) {
    use BattleEvent as E;
    use pikapro::event::MoveFrom;
    let all = battle.events();
    let doubles = battle.format().active_per_side() > 1;
    let dex = battle.format().dex;
    for (i, e) in all.iter().enumerate().skip(start) {
        // A move against a side with an active Pressure that never showed
        // (it came in under Neutralizing Gas, which a transformed holder
        // ends without a line: `neutralizinggas.onEnd`): its PP are in
        // doubt too.
        if let E::Move { user, move_id, from, .. } = *e {
            let foe = user.pos.side.foe();
            let side = battle.state().side(foe);
            let hidden = (0..battle.format().active_per_side()).any(|slot| {
                side.active(slot).is_some_and(|m| {
                    m.ability == abilities::PRESSURE && !r.pressure_shown[foe.index()][m.id as usize % 6]
                })
            });
            if hidden {
                let charged = match from {
                    Some(MoveFrom::Move(caller)) => caller,
                    _ => move_id,
                };
                r.pp_doubt.insert((user.pos.side.index(), user.id, charged));
            }
        }
        match *e {
            // A move whose target `[still]` hid, in doubles, against a side
            // with Pressure: Pressure's share of its PP is in doubt (its
            // release names a charged move's target).
            E::Move { user, move_id, target, from }
                if doubles && (pressure[user.pos.side.foe().index()] || r.pressure[user.pos.side.foe().index()]) =>
            {
                let charged = match from {
                    Some(MoveFrom::Move(caller)) => caller,
                    _ => move_id,
                };
                let key = (user.pos.side.index(), user.id, charged);
                let data = dex.move_(move_id);
                let aimed = matches!(
                    data.target,
                    MoveTarget::Normal
                        | MoveTarget::Any
                        | MoveTarget::AdjacentFoe
                        | MoveTarget::RandomNormal
                        | MoveTarget::Scripted
                );
                let charge = data.flags.contains(MoveFlags::CHARGE);
                r.released = None;
                match (target, from) {
                    // A charge turn: its release may tell.
                    (None, None) if aimed && charge => {
                        // With a foe down, the charge may have gone to
                        // another foe than the one its release names.
                        let f = user.pos.side.foe().index();
                        let down = (0..2).any(|k| r.down[f][k] || r.in_slot[f][k] == u8::MAX) || r.center[f];
                        if down || !r.charge_doubt.insert(key) {
                            r.pp_doubt.insert(key);
                        }
                    }
                    (None, None | Some(MoveFrom::Move(_))) if aimed => {
                        r.pp_doubt.insert(key);
                    }
                    // The release of a charged move names the charge's
                    // target, unless the other foe is down (a fainted
                    // target is replaced by a random foe).
                    (Some(t), Some(MoveFrom::LockedMove)) if charge && r.charge_doubt.remove(&key) => {
                        let clean = t.pos.side != user.pos.side
                            && !r.down[t.pos.side.index()][1 - (t.pos.slot as usize & 1)]
                            && !r.center[t.pos.side.index()];
                        if clean {
                            r.released = Some(key);
                        } else {
                            r.pp_doubt.insert(key);
                        }
                    }
                    _ => {}
                }
            }
            // `|move|<user>|<move>` without a target, `[still]` and `-fail`
            // from a Pokémon a Choice item or Gorilla Tactics locks.
            E::Fail { user } if i >= 2 => {
                let (
                    E::MoveAttr { attr: pikapro::event::MoveAttr::Still },
                    E::Move { user: u, move_id, target: None, from: None },
                ) = (all[i - 1], all[i - 2])
                else {
                    continue;
                };
                if u.pos != user.pos || u.id != user.id {
                    continue;
                }
                let s = user.pos.side.index();
                let locks = battle.state().side(user.pos.side).team.iter().find(|m| m.id == user.id).is_some_and(|m| {
                    m.item.is_some_and(|it| dex.item(it).is_choice) || m.ability == abilities::GORILLA_TACTICS
                });
                if locks {
                    r.lock_doubt.insert((s, user.id, move_id));
                    r.last_move_doubt.insert((s, user.id));
                }
            }
            // Ally Switch: a charge aimed at an ally may now aim at its
            // user, and its release at a random foe.
            E::Swap { actor, .. } => {
                let s = actor.pos.side.index();
                let pending: Vec<_> = r.charge_doubt.iter().copied().filter(|k| k.0 == s).collect();
                for k in pending {
                    r.charge_doubt.remove(&k);
                    r.pp_doubt.insert(k);
                }
            }
            // A redirection that shows after the move line.
            E::ActivateEffect {
                effect: pikapro::event::EffectName::Ability(abilities::STORM_DRAIN | abilities::LIGHTNING_ROD),
                ..
            } => {
                if let Some(key) = r.released.take() {
                    r.pp_doubt.insert(key);
                }
            }
            E::MoveSingleTurn { target, move_id: moves::FOLLOW_ME | moves::RAGE_POWDER, .. } => {
                r.center[target.pos.side.index()] = true;
            }
            E::Turn { .. } => {
                r.last_turn = Some(i);
                r.center = [false; 2];
            }
            E::Upkeep => r.last_upkeep = Some(i),
            E::StatusStart { target, status: Status::Sleep { .. }, from_move } => {
                let (s, id) = (target.pos.side.index(), target.id as usize % 6);
                r.sleep[s][id] = 0;
                r.rest[s][id] = from_move == Some(moves::REST);
            }
            E::StatusFrom { target, status: Status::Sleep { .. }, .. } => {
                let (s, id) = (target.pos.side.index(), target.id as usize % 6);
                r.sleep[s][id] = 0;
                r.rest[s][id] = false;
            }
            E::StatusCant { actor, status: Status::Sleep { .. } } => {
                let (s, id) = (actor.pos.side.index(), actor.id as usize % 6);
                r.sleep[s][id] = r.sleep[s][id].saturating_add(1);
            }
            E::StatusStart { target, status: Status::Freeze { .. }, .. }
            | E::StatusFrom { target, status: Status::Freeze { .. }, .. } => {
                r.freeze[target.pos.side.index()][target.id as usize % 6] = 0;
            }
            E::StatusCant { actor, status: Status::Freeze { .. } } => {
                let (s, id) = (actor.pos.side.index(), actor.id as usize % 6);
                r.freeze[s][id] = r.freeze[s][id].saturating_add(1);
            }
            E::VolatileStart { actor, volatile: pikapro::event::Volatile::Confusion, .. } => {
                r.confusion[actor.pos.side.index()][actor.id as usize % 6] = 0;
            }
            E::StartFrom { actor, what: "confusion", .. } | E::FatigueConfusion { actor } => {
                r.confusion[actor.pos.side.index()][actor.id as usize % 6] = 0;
            }
            E::VolatileActivate { actor, volatile: pikapro::event::Volatile::Confusion } => {
                let (s, id) = (actor.pos.side.index(), actor.id as usize % 6);
                r.confusion[s][id] = r.confusion[s][id].saturating_add(1);
            }
            E::AbilityShow { actor, ability: abilities::PRESSURE, .. } => {
                r.pressure[actor.pos.side.index()] = true;
                r.pressure_shown[actor.pos.side.index()][actor.id as usize % 6] = true;
            }
            // A weather or terrain set again starts a new episode.
            E::Weather { upkeep: false, .. } | E::WeatherFrom { .. } => {
                r.field_totals.retain(|k, _| !matches!(k, FieldDur::Weather(_)));
            }
            E::FieldStart { effect: FieldEffect::Terrain(_), .. }
            | E::FieldStartFrom { effect: FieldEffect::Terrain(_), .. } => {
                r.field_totals.retain(|k, _| !matches!(k, FieldDur::Terrain(_)));
            }
            // A condition that starts (again) on a Pokémon starts a new
            // episode: one can end and start again between two decisions.
            E::VolatileStart { actor, .. }
            | E::EffectStart { actor, .. }
            | E::EffectActivate { actor, .. }
            | E::StartFrom { actor, .. }
            | E::EffectStartEnd { actor, .. }
            | E::UproarStart { actor, upkeep: false }
            | E::MoveActivate { actor, .. }
            | E::Switch { actor, .. }
            | E::Drag { actor, .. }
            | E::Faint { actor } => {
                let s = actor.pos.side.index();
                let passed = match *e {
                    E::Switch { from, .. } => Some(from == Some(moves::BATON_PASS)),
                    E::Drag { .. } => Some(false),
                    _ => None,
                };
                if let Some(passed) = passed
                    && let Some(slot) = r.in_slot[s].get_mut(actor.pos.slot as usize)
                {
                    // Baton Pass passes confusion, with its checks.
                    if passed && let Some(&n) = r.confusion[s].get(*slot as usize % 6) {
                        r.confusion[s][actor.id as usize % 6] = n;
                    }
                    // The Pokémon that left the slot, and the one that came:
                    // their last moves start again.
                    r.last_move_doubt.remove(&(s, *slot));
                    r.last_move_doubt.remove(&(s, actor.id));
                    *slot = actor.id;
                }
                if matches!(e, E::Faint { .. }) {
                    r.last_move_doubt.remove(&(s, actor.id));
                }
                if matches!(e, E::Switch { .. } | E::Drag { .. } | E::Faint { .. })
                    && let Some(d) = r.down[s].get_mut(actor.pos.slot as usize)
                {
                    *d = matches!(e, E::Faint { .. });
                }
                r.mon_totals.retain(|&(ms, id, _), _| !(ms == s && id == actor.id));
                if matches!(e, E::Switch { .. } | E::Drag { .. }) {
                    r.toxic_hidden[s][actor.id as usize % 6] = false;
                }
            }
            _ => {}
        }
    }
}

/// The rows of one engine Pokémon: `(ego, row)` in each player's
/// view (its own row, then the opponent's row of it), if it has them.
fn rows(views: &[PlayerView<'_>; 2], side: SideId, id: u8) -> [(usize, usize, Option<usize>); 2] {
    let public = views[side.index()].own_row(id).map(usize::from);
    // Own row k is the Pokémon with id k; the foe's is its public row.
    [(side.index(), 0, Some(id as usize)), (side.foe().index(), 1, public)]
}

struct Where {
    tag: String,
    turn: u16,
}

impl Where {
    fn at(&self, detail: impl std::fmt::Display) -> String {
        format!("{} turn {}: {detail}", self.tag, self.turn)
    }
}

fn tally(r: &mut BattleRef, audit: &mut FeatureAudit, rule: &'static str, ok: bool, example: impl FnOnce() -> String) {
    let t = audit.rules.entry(rule).or_default();
    t.checked += 1;
    if !ok {
        t.bad += 1;
        r.broken.insert(rule);
        if t.examples.len() < audit.max_examples {
            t.examples.push(example());
        }
    }
}

fn histogram(audit: &mut FeatureAudit, name: &'static str, v: i64) {
    *audit.histograms.entry(name).or_default().entry(v).or_default() += 1;
}

/// A duration: turns elapsed, -1 when not up.
fn turns_if(on: bool, n: u8) -> i64 {
    if on { i64::from(n) } else { -1 }
}

/// A row's status turns: the turns its sleep, freeze or bad poison was
/// observed to last, -1 with another status or none.
fn status_turns(m: &MonRef<'_>) -> i64 {
    turns_if(matches!(m.status(), StatusKind::Sleep | StatusKind::Freeze | StatusKind::Toxic), m.status_turns())
}

/// A row's Pokémon is asleep from Rest.
fn sleep_from_rest(m: &MonRef<'_>) -> bool {
    m.status() == StatusKind::Sleep && m.public.is_some_and(|p| p.flags.has(MonFlags::SLEEP_FROM_REST))
}

fn point(
    r: &mut BattleRef,
    audit: &mut FeatureAudit,
    battle: &Battle,
    views: &[PlayerView<'_>; 2],
    ctxs: &[ViewCtx<'_>; 2],
) {
    let state = battle.state();
    let format = battle.format();
    let turn = state.turn;
    // Between the residuals and the next `|turn|` (a replacement for a
    // Pokémon that fainted in them): the engine counted the turn down, the
    // `|turn|` counters haven't counted it yet.
    let post_upkeep = match (r.last_upkeep, r.last_turn) {
        (Some(u), Some(t)) => u > t,
        (Some(_), None) => true,
        _ => false,
    };
    let over = battle.is_over();
    r.over = over;
    for side in SideId::BOTH {
        let s = state.side(side);
        let si = side.index();
        // `mega_used` both ways: the side's own row and the foe's.
        for (ego, which) in [(si, 0usize), (side.foe().index(), 1)] {
            if format.gimmicks.contains(Gimmick::MegaEvolve) {
                let v = ctxs[ego].sides[which].mega_used;
                let truth = s.gimmicks_used.contains(Gimmick::MegaEvolve);
                // A side whose set started Mega Evolved can Mega Evolve
                // another Pokémon.
                if which == 1 && s.team.iter().any(|m| r.pre_mega[si][m.id as usize % 6]) {
                    let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
                    let ex = w.at(format!("{side:?}: a Mega forme set, and a Mega Evolution"));
                    tally(r, audit, "one Mega per side (info)", !truth, || ex);
                }
                let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
                let ex = w.at(format!("{side:?} ego{ego}: mega_used {v} vs {truth}"));
                tally(r, audit, "mega_used", v == truth, || ex);
            }
        }
        // Side conditions with a duration: turns left + turns elapsed is
        // constant while up.
        for c in SideCondition::ALL {
            if c.is_hazard() || matches!(c, SideCondition::WideGuard | SideCondition::QuickGuard) {
                continue;
            }
            let left = s.conditions.turns(c);
            let key = FieldDur::Side(side, c as u8);
            let shown = ctxs[si].sides[0].conditions[c as usize];
            let v = turns_if(shown.layers > 0, shown.turns);
            if left == 0 || v < 0 {
                r.field_totals.remove(&key);
                continue;
            }
            let total = i64::from(left) + v;
            drift_field(r, audit, key, total, "side condition turns drift", turn, &format!("{side:?} {c:?}"));
            histogram(audit, "total side condition", total);
        }
        // Foreseen attacks and Wish: turns left, exact.
        for slot in 0..format.active_per_side() {
            let v = ctxs[si].sides[0].future[slot].turns_left().map_or(-1, i64::from);
            if let Some(f) = s.future_moves[slot] {
                // It lands in the residuals of turn `ending_turn + 1`;
                // after this turn's residuals, this turn's end is past.
                let truth = i64::from(f.ending_turn) - i64::from(turn) + 2 - i64::from(post_upkeep);
                let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
                let ex = w.at(format!("{side:?} slot {slot}: future_turns {v} vs {truth}"));
                tally(r, audit, "future_turns", v == truth || over, || ex);
            }
            let slots = &s.conditions.slots[slot];
            if slots.has(pikapro::state::SlotCondition::Wish) && !over {
                let v = ctxs[si].sides[0].slot_conditions[slot].wish_turns_left().map_or(-1, i64::from);
                // `starting_turn` is the turn before it was made; it comes
                // true in the residuals of the turn after.
                let truth = i64::from(slots.wish.starting_turn) + 3 - i64::from(turn) - i64::from(post_upkeep);
                let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
                let ex = w.at(format!("{side:?} slot {slot}: wish_turns {v} vs {truth}"));
                tally(r, audit, "wish_turns", v == truth, || ex);
            }
        }
        if r.illusion[si] {
            continue;
        }
        // Benched Pokémon keep their sleep and toxic counts (sleep's goes on
        // when it comes back; toxic's restarts then).
        for (pos, mon) in s.team.iter().enumerate() {
            if s.is_active(pos as u8) || mon.is_fainted() {
                continue;
            }
            let id = mon.id;
            for (ego, which, row) in rows(views, side, id) {
                let Some(row) = row else { continue };
                let m = ctxs[ego].mon(which, row);
                if !m.present() {
                    continue;
                }
                let st = status_turns(m);
                let shown_rest = sleep_from_rest(m);
                let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
                let rest = matches!(mon.status, Status::Sleep { .. }) && r.rest[si][id as usize % 6];
                let ex = w.at(format!("{side:?} benched id {id}: sleep_from_rest {shown_rest} vs {rest}"));
                tally(r, audit, "sleep_from_rest", shown_rest == rest, || ex);
                match mon.status {
                    Status::Sleep { turns } if m.status() == StatusKind::Sleep => {
                        let attempts = i64::from(r.sleep[si][id as usize % 6]);
                        let ex = w.at(format!(
                            "{side:?} benched id {id}: sleep status_turns {st} vs {attempts} ({turns} left)"
                        ));
                        tally(r, audit, "status_turns sleep", st == attempts, || ex);
                    }
                    Status::Freeze { .. } if m.status() == StatusKind::Freeze => {
                        let attempts = i64::from(r.freeze[si][id as usize % 6]);
                        let ex = w.at(format!("{side:?} benched id {id}: freeze status_turns {st} vs {attempts}"));
                        tally(r, audit, "status_turns freeze", st == attempts, || ex);
                    }
                    Status::Toxic { counter } if m.status() == StatusKind::Toxic => {
                        let hides = |a| matches!(a, abilities::MAGIC_GUARD | abilities::POISON_HEAL);
                        let hidden = r.toxic_hidden[si][id as usize % 6]
                            || hides(mon.ability)
                            || hides(r.set_ability[si][id as usize % 6]);
                        let ex = w.at(format!("{side:?} benched id {id}: toxic status_turns {st} vs {counter}"));
                        if !hidden {
                            tally(r, audit, "status_turns toxic", st == i64::from(counter), || ex);
                        }
                    }
                    _ => {}
                }
            }
        }
        for slot in 0..format.active_per_side() {
            let Some(mon) = s.active(slot) else { continue };
            if mon.is_fainted() {
                continue;
            }
            let id = mon.id;
            for (ego, which, row) in rows(views, side, id) {
                let Some(row) = row else { continue };
                let m = ctxs[ego].mon(which, row);
                let who = if which == 0 { "own" } else { "foe" };
                let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
                let at = |what: String| w.at(format!("{side:?} slot {slot} {who} row {row}: {what}"));
                // turns_active: Showdown's `activeTurns`.
                let v = m.active.map_or(-1, |a| i64::from(a.turns_active));
                let truth = i64::from(mon.active_turns.min(255));
                let ex = at(format!("turns_active {v} vs {truth}"));
                tally(r, audit, "turns_active", v == truth || over, || ex);
                // status_turns.
                let st = status_turns(m);
                match mon.status {
                    Status::Toxic { counter } => {
                        let hides = |a| matches!(a, abilities::MAGIC_GUARD | abilities::POISON_HEAL);
                        if hides(mon.ability) {
                            r.toxic_hidden[si][id as usize % 6] = true;
                        }
                        let hidden = r.toxic_hidden[si][id as usize % 6] || hides(r.set_ability[si][id as usize % 6]);
                        let ex = at(format!("toxic status_turns {st} vs {counter}"));
                        if !hidden {
                            tally(r, audit, "status_turns toxic", st == i64::from(counter) || over, || ex);
                        }
                    }
                    Status::Sleep { turns } => {
                        let attempts = i64::from(r.sleep[si][id as usize % 6]);
                        let ex = at(format!("sleep status_turns {st} vs {attempts} attempts ({turns} left)"));
                        tally(r, audit, "status_turns sleep", st == attempts, || ex);
                        // The reference against the state: Rest's 3 turns,
                        // two attempts. Not for Early Bird, which counts
                        // twice while it has it (a Skill Swap, Neutralizing
                        // Gas or Gastro Acid in the middle change that).
                        let early_bird = mon.ability == abilities::EARLY_BIRD
                            || r.set_ability[si][id as usize % 6] == abilities::EARLY_BIRD;
                        if r.rest[si][id as usize % 6] && which == 0 && !early_bird {
                            let ok = 3 - i64::from(turns) == attempts;
                            let ex = at(format!("rest: {attempts} attempts, {turns} left"));
                            tally(r, audit, "sleep reference (rest)", ok, || ex);
                        }
                        if which == 0 {
                            histogram(audit, "sleep attempts at a decision", attempts);
                        }
                    }
                    // Attempts to move while frozen (`cant|frz`); Champions
                    // counts them down from 3 (`frz.onBeforeMove`).
                    Status::Freeze { turns } => {
                        let attempts = i64::from(r.freeze[si][id as usize % 6]);
                        let ex = at(format!("freeze status_turns {st} vs {attempts} attempts ({turns} left)"));
                        tally(r, audit, "status_turns freeze", st == attempts, || ex);
                        if format.mechanics == pikapro::Mechanics::Champions && which == 0 {
                            let ex = at(format!("freeze: {attempts} attempts, {turns} left"));
                            tally(r, audit, "freeze reference (champions)", 3 - i64::from(turns) == attempts, || ex);
                            histogram(audit, "freeze attempts at a decision (champions)", attempts);
                        }
                    }
                    _ => {}
                }
                // Rest's sleep.
                let rest = matches!(mon.status, Status::Sleep { .. }) && r.rest[si][id as usize % 6];
                let shown_rest = sleep_from_rest(m);
                let ex = at(format!("sleep_from_rest {shown_rest} vs {rest}"));
                tally(r, audit, "sleep_from_rest", shown_rest == rest, || ex);
                if which == 0 && matches!(mon.status, Status::Sleep { .. }) {
                    histogram(audit, "asleep from Rest at a decision", i64::from(rest));
                }
                // Per-Pokémon durations.
                for d in DURS {
                    let v = d.elapsed(m);
                    let key = (si, id, d);
                    let Some(left) = d.left(mon) else {
                        if which == 1 {
                            r.mon_totals.remove(&key);
                        }
                        let ex = at(format!("{:?} shown ({v}) but not up", d));
                        tally(r, audit, "duration shown while not up", v < 0 || over, || ex);
                        continue;
                    };
                    if v < 0 {
                        // Not shown: soundness's business for the volatiles
                        // the log always shows.
                        continue;
                    }
                    // Slow Start's counter skips the residuals of the turn it
                    // came in (`if (pokemon.activeTurns)`); from the next
                    // `|turn|` on both count alike.
                    if which != 1 || d == Dur::SlowStart && mon.active_turns == 0 {
                        continue;
                    }
                    // Counted at the residuals, as the engine counts it down:
                    // the same between the residuals and the next `|turn|`.
                    let elapsed = v;
                    let total = left + elapsed;
                    histogram(audit, d.total(), total);
                    match r.mon_totals.get(&key) {
                        Some(&t) => {
                            let ex = at(format!("{:?}: {left} left + {elapsed} elapsed = {total}, was {t}", d));
                            tally(r, audit, d.rule(), t == total || over, || ex);
                            if post_upkeep && !over {
                                let ex = at(format!(
                                    "{:?} after the residuals: {left} left + {elapsed} = {total}, was {t}",
                                    d
                                ));
                                tally(r, audit, "durations after the residuals", t == total, || ex);
                            }
                        }
                        None => {
                            r.mon_totals.insert(key, total);
                        }
                    }
                }
                // PP spent on each revealed move (own moves only: not
                // while transformed).
                if mon.volatile_counters.transform.is_none() {
                    for mv in m.moves() {
                        let Some(shown) = mv.slot.id() else { continue };
                        let spent = i64::from(mv.pp_spent);
                        let Some(slot) = mon.moves.iter().find(|m| m.id == shown) else { continue };
                        if !mv.revealed {
                            continue;
                        }
                        if mon.volatiles.has(Volatiles::MIMIC)
                            && mon.volatile_counters.mimic_original.id != slot.id
                            && mon.moves.get(mon.volatile_counters.mimic_slot as usize).map(|m| m.id) == Some(slot.id)
                        {
                            continue;
                        }
                        let truth = i64::from(slot.max_pp) - i64::from(slot.pp);
                        let name = format.dex.move_(slot.id).name;
                        let ex = at(format!("{name} pp_spent {spent} vs {truth}"));
                        // Pressure's share of a move whose target `[still]`
                        // hid, in doubles, is in doubt.
                        let foe = state.side(side.foe());
                        let pressure = r.pressure[side.foe().index()]
                            || foe
                                .team
                                .iter()
                                .any(|m| m.ability == abilities::PRESSURE || m.base_ability == abilities::PRESSURE);
                        // An Illusion on the foe's side fools the view about
                        // which foe has Pressure, as it fools a client.
                        let key = (si, id, slot.id);
                        let rule = if r.lock_doubt.contains(&key) {
                            "pp_spent (a Choice lock's failure) (info)"
                        } else if r.pp_doubt.contains(&key) || r.charge_doubt.contains(&key) {
                            "pp_spent (Pressure the log hides) (info)"
                        } else if pressure && r.illusion[side.foe().index()] {
                            "pp_spent (Pressure, foe has Illusion) (info)"
                        } else if pressure {
                            "pp_spent (foe has Pressure)"
                        } else {
                            "pp_spent"
                        };
                        tally(r, audit, rule, spent == truth || over, || ex);
                    }
                }
                // last_move: Showdown's `lastMove` ("close to" by its doc).
                let lm = m.active.and_then(|a| a.last_move);
                let truth = mon.last_move;
                if which == 1 && mon.volatile_counters.transform.is_none() {
                    let name = |t: Option<pikapro_data::MoveId>| t.map_or("none", |id| format.dex.move_(id).name);
                    let ex = at(format!("last_move {} vs {}", name(lm), name(truth)));
                    let rule = if r.last_move_doubt.contains(&(si, id)) {
                        "last_move (a Choice lock's failure) (info)"
                    } else {
                        "last_move"
                    };
                    tally(r, audit, rule, lm == truth || over, || ex);
                }
                // Supreme Overlord's fallen.
                if mon.ability == abilities::SUPREME_OVERLORD
                    && !mon.volatiles.has(Volatiles::GASTRO_ACID)
                    && which == 1
                {
                    let v = m.active.map_or(0, |a| i64::from(a.counters.fallen));
                    let truth = i64::from(mon.volatile_counters.ability_state.fallen.min(5));
                    let ex = at(format!("fallen {v} vs {truth}"));
                    tally(r, audit, "fallen", v == truth || over, || ex);
                }
                // A rampage the view doesn't show (documented: the view shows
                // what its last move was).
                if which == 1 && mon.volatiles.has(Volatiles::LOCKED_MOVE) {
                    let ex = at("locked, not shown".into());
                    let shown = m.active.is_some_and(|a| a.volatiles.has(V::LOCKED_MOVE));
                    tally(r, audit, "locked_move shown (info)", shown, || ex);
                }
                // Confusion's checks (`-activate|...|confusion`, one per move
                // attempt: what ends it, Showdown's `time`).
                let confusion =
                    m.active.map_or(-1, |a| turns_if(a.volatiles.has(V::CONFUSION), a.counters.confusion_checks));
                if mon.volatiles.has(Volatiles::CONFUSION) && confusion >= 0 {
                    let checks = i64::from(r.confusion[si][id as usize % 6]);
                    let v = confusion;
                    let ex = at(format!("confusion_checks {v} vs {checks}"));
                    tally(r, audit, "confusion_checks", v == checks, || ex);
                    if which == 0 {
                        histogram(audit, "confusion checks at a decision", checks);
                    }
                }
                // The Mega flag: in a Mega forme (not transformed into one).
                if format.gimmicks.contains(Gimmick::MegaEvolve) && mon.volatile_counters.transform.is_none() {
                    let truth = format.dex.species(mon.species).forme.contains("Mega");
                    let v = m.is_mega();
                    let ex = at(format!("mega {v} vs {truth}"));
                    // A set whose species is a Mega forme starts the battle
                    // in it (Showdown's validator makes it the base forme:
                    // `validateForme`, `set.species = species.battleOnly`).
                    let pre = r.pre_mega[si][id as usize % 6];
                    let rule = if pre { "mega (set species is a Mega forme)" } else { "mega" };
                    tally(r, audit, rule, v == truth, || ex);
                }
            }
        }
    }
    // The field.
    let f = &state.field;
    let fv = ctxs[0].view.field();
    let weather_shown = turns_if(fv.weather.is_some(), fv.weather_turns);
    let terrain_shown = turns_if(fv.terrain.is_some(), fv.terrain_turns);
    // A new weather or terrain is a new episode (the old one ended).
    let weather = f.weather.map(|w| FieldDur::Weather(w as u8));
    r.field_totals.retain(|k, _| !matches!(k, FieldDur::Weather(_)) || Some(*k) == weather);
    if let Some(key) = weather
        && f.weather_turns > 0
        && weather_shown >= 0
    {
        let total = i64::from(f.weather_turns) + weather_shown;
        drift_field(r, audit, key, total, "weather_turns drift", turn, &format!("{:?}", f.weather));
        histogram(audit, "total weather", total);
    }
    let terrain = f.terrain.map(|t| FieldDur::Terrain(t as u8));
    r.field_totals.retain(|k, _| !matches!(k, FieldDur::Terrain(_)) || Some(*k) == terrain);
    if let Some(key) = terrain
        && terrain_shown >= 0
    {
        let total = i64::from(f.terrain_turns) + terrain_shown;
        drift_field(r, audit, key, total, "terrain_turns drift", turn, &format!("{:?}", f.terrain));
        histogram(audit, "total terrain", total);
    }
    for (name, left, shown) in [
        ("trick_room", f.trick_room_turns, fv.trick_room),
        ("gravity", f.gravity_turns, fv.gravity),
        ("magic_room", f.magic_room_turns, fv.magic_room),
        ("wonder_room", f.wonder_room_turns, fv.wonder_room),
    ] {
        let v = shown.map_or(-1, i64::from);
        if left == 0 {
            let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
            let ex = w.at(format!("{name} {v} while down"));
            tally(r, audit, "room shown while down", v < 0 || over, || ex);
            continue;
        }
        let w = Where { tag: format!("{} {}", r.format, r.seed), turn };
        let ex = w.at(format!("{name}: {v} elapsed, {left} left"));
        tally(r, audit, "room turns", v + i64::from(left) == 5 || over, || ex);
    }
}

fn drift_field(
    r: &mut BattleRef,
    audit: &mut FeatureAudit,
    key: FieldDur,
    total: i64,
    rule: &'static str,
    turn: u16,
    what: &str,
) {
    match r.field_totals.get(&key) {
        Some(&t) => {
            let ex = format!("{} {} turn {turn}: {what}: total {total}, was {t}", r.format, r.seed);
            tally(r, audit, rule, t == total || r.over, || ex);
        }
        None => {
            r.field_totals.insert(key, total);
        }
    }
}
