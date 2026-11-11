//! Checks 1 and 2a: soundness against the omniscient state, and the
//! per-event no-leak rule; plus the team-order rule without team preview.
//!
//! **Soundness** ([`check`]) runs at every decision point: `pikapro-view`'s
//! own oracle, [`pikapro_view::check::check_sound`] (every claim of the
//! tracker against [`BattleState`](pikapro::BattleState), strict: stat
//! stages, announced volatiles, conditions and the field in both
//! directions), and one thing stricter: that oracle, without history,
//! checks the records of a side with an Illusion user only if they were
//! never on the field, where this check skips only the records that were
//! ever shown as a disguise (seen at a decision point, named by a
//! `|replace|`, dragged in, or left fainted by an Illusion user that
//! fainted disguised) and the Illusion user's own when it shared a record
//! with the Pokémon beside it or fainted disguised, and checks the others
//! with [`pikapro_view::check::check_row`] (the Illusion user's without
//! what it may have missed disguised: an item, its Terastallization).
//!
//! **Per-event rule** ([`Absorber`]): an event that shows player 1,
//! player 2 or a spectator nothing must not change the public tracker;
//! and the species a switch-in's details show is the record's.
//!
//! **Team order** ([`team_order`]): without team preview, rows are in order
//! of first appearance and nothing unseen has a row.

use pikapro::event::Actor;
use pikapro::state::Position;
use pikapro::{Battle, BattleEvent, PokemonState, SideId};
use pikapro_data::gen9::abilities;
use pikapro_protocol::{Perspective, Renderer};
use pikapro_view::PublicTracker;

use super::{Failure, Setup, debug_diff, shows};

/// The channels the per-event rule watches.
pub const WATCHED: [Perspective; 3] =
    [Perspective::Player(SideId::P1), Perspective::Player(SideId::P2), Perspective::Spectator];

fn perspective_name(p: Perspective) -> &'static str {
    match p {
        Perspective::Player(SideId::P1) => "p1",
        Perspective::Player(SideId::P2) => "p2",
        Perspective::Spectator => "spectators",
        Perspective::Omniscient => "omniscient",
    }
}

/// The row events name side `side`'s Pokémon `id` with, if seen.
pub fn row_of(tracker: &PublicTracker, side: SideId, id: u8) -> Option<u8> {
    let actor = Actor { pos: Position { side, slot: 0 }, species: Default::default(), active: true, id };
    tracker.row_of_actor(&actor)
}

/// Feeds a battle's events to a tracker, one at a time, checking the
/// per-event rule, and keeps what the checks need: the spectators' lines
/// so far (for context) and the rows Illusion makes exempt.
#[derive(Debug)]
pub struct Absorber {
    renderers: [Renderer; 3],
    /// Everything the spectators were shown so far.
    pub context: Vec<String>,
    /// Per side, the rows (bit per row) ever shown as an Illusion's
    /// disguise.
    pub taint: [u8; 2],
    /// Per side, the rows ever shown in both slots at once (an Illusion
    /// disguised as the Pokémon beside it).
    shared: [u8; 2],
    illusion: [bool; 2],
    /// Events applied, and events that changed the tracker.
    pub events: u64,
    pub changes: u64,
}

impl Absorber {
    pub fn new(setup: &Setup) -> Absorber {
        let mut context = setup.renderer(Perspective::Spectator).header();
        context.retain(|l| !l.starts_with("|t:|"));
        Absorber {
            renderers: WATCHED.map(|p| setup.renderer(p)),
            context,
            taint: [0; 2],
            shared: [0; 2],
            illusion: SideId::BOTH
                .map(|s| setup.sheets[s.index()].team.sets.iter().any(|set| set.ability == abilities::ILLUSION)),
            events: 0,
            changes: 0,
        }
    }

    /// Applies `events` in order. With `leak`, an event that shows one of
    /// the [`WATCHED`] channels nothing must leave the tracker unchanged.
    pub fn absorb(&mut self, tracker: &mut PublicTracker, events: &[BattleEvent], leak: bool) -> Result<(), Failure> {
        for (i, event) in events.iter().enumerate() {
            let before = *tracker;
            if let BattleEvent::Replace { actor, .. } = *event
                && let Some(row) = tracker.side(actor.pos.side).slots.get(actor.pos.slot as usize).and_then(|s| s.row)
            {
                self.taint[actor.pos.side.index()] |= 1 << row;
            }
            tracker.apply(event);
            self.events += 1;
            // The disguise was on the field in the other slot too: the
            // record the two slots shared mixed what each did, and some of
            // it moved to the Illusion user's record (which the `|replace|`
            // may have created: after it).
            if let BattleEvent::Replace { actor, .. } = *event
                && let si = actor.pos.side.index()
                && let Some(user) = row_of(tracker, actor.pos.side, actor.id)
                && let Some(disguise) = before.side(actor.pos.side).slots[actor.pos.slot as usize].row
                && self.shared[si] & (1 << disguise) != 0
            {
                self.taint[si] |= 1 << user;
            }
            for side in SideId::BOTH {
                let slots = &tracker.side(side).slots;
                if let Some(row) = slots[0].row
                    && slots[1].row == Some(row)
                {
                    self.shared[side.index()] |= 1 << row;
                }
            }
            if let BattleEvent::Drag { actor, .. } = *event
                && self.illusion[actor.pos.side.index()]
                && let Some(row) = row_of(tracker, actor.pos.side, actor.id)
            {
                self.taint[actor.pos.side.index()] |= 1 << row;
            }
            if leak && let Some(f) = self.details(tracker, event) {
                let mut context = self.context.clone();
                context.extend(self.renderers[2].log(&events[..=i]));
                return Err(f.with_context(&context));
            }
            if *tracker == before {
                continue;
            }
            self.changes += 1;
            if !leak {
                continue;
            }
            for (k, renderer) in self.renderers.iter().enumerate() {
                if shows(renderer, event) {
                    continue;
                }
                let mut context = self.context.clone();
                context.extend(self.renderers[2].log(&events[..=i]));
                return Err(Failure::new(
                    "leak",
                    event_name(event),
                    format!(
                        "{event:?} shows {} nothing but changed the view:\n        {}",
                        perspective_name(WATCHED[k]),
                        debug_diff(&before, tracker)
                    ),
                )
                .with_context(&context));
            }
        }
        self.context.extend(self.renderers[2].log(events));
        Ok(())
    }
}

impl Absorber {
    /// The species a `|switch|`, `|drag|`, `|replace|` or `|detailschange|`
    /// line shows must be the record's: the view can't know a forme the
    /// details hide (Greninja-Bond and Rockruff-Dusk show as their base
    /// species).
    fn details(&self, tracker: &PublicTracker, event: &BattleEvent) -> Option<Failure> {
        use BattleEvent as E;
        let actor = match *event {
            E::Switch { actor, .. }
            | E::Drag { actor, .. }
            | E::Replace { actor, .. }
            | E::DetailsChange { actor, .. } => actor,
            _ => return None,
        };
        let lines = self.renderers[2].log(std::slice::from_ref(event));
        let line = lines
            .iter()
            .find(|l| ["|switch|", "|drag|", "|replace|", "|detailschange|"].iter().any(|p| l.starts_with(p)))?;
        let name = line.split('|').nth(3)?.split(", ").next()?;
        let dex = tracker.config.dex;
        let shown = dex.species_by_name(name)?;
        let row = row_of(tracker, actor.pos.side, actor.id)?;
        let rec = tracker.side(actor.pos.side).mon(row)?;
        if rec.species == shown && rec.details_species == shown {
            return None;
        }
        Some(Failure::new(
            "leak",
            "details",
            format!(
                "`{line}` shows {}, but the view has species {} (details {})",
                dex.species(shown).name,
                dex.species(rec.species).name,
                dex.species(rec.details_species).name
            ),
        ))
    }
}

/// An event's variant name.
pub fn event_name(event: &BattleEvent) -> String {
    let s = format!("{event:?}");
    s.split([' ', '{', '(']).next().unwrap_or("").to_string()
}

/// Soundness of `tracker` against the battle's state, at a decision
/// point: `pikapro-view`'s oracle, then the records of sides with an
/// Illusion user it leaves out, but those shown as a disguise. Adds the
/// disguises seen now to `taint`. Facts named in `skip` aren't reported.
pub fn check(battle: &Battle, tracker: &PublicTracker, taint: &mut [u8; 2], skip: &[&str]) -> Result<(), Failure> {
    let fail = |what: &str, message: String| Err(Failure::new("soundness", what, message));
    if let Err(v) = pikapro_view::check::check_sound(tracker, battle)
        && !skip.contains(&v.what)
    {
        // Reflect Type on a foe whose Illusion is up copies the disguise's
        // types as far as anyone can see (`[of]` names the disguise), and
        // keeps them after the Illusion breaks.
        let foe = v.side.foe();
        let foe_disguised =
            taint[foe.index()] != 0 || battle.state().side(foe).team.iter().any(|m| m.illusion.is_some());
        let barb = v.row.and_then(|r| tracker.side(v.side).mon(r)).is_some_and(|m| holds_barb(m.item));
        if !(matches!(v.what, "types") || v.what == "item" && barb) || !foe_disguised {
            return fail(v.what, v.to_string());
        }
    }
    let illusion_user = |m: &PokemonState| m.base_ability == abilities::ILLUSION;
    for side in SideId::BOTH {
        let st = battle.state().side(side);
        if !st.team.iter().any(illusion_user) {
            continue;
        }
        let pv = tracker.side(side);
        // Disguises: up now, or kept by an Illusion user that left the
        // field or fainted without breaking it.
        for m in st.team.iter() {
            if let Some(d) = m.illusion
                && let Some(row) = row_of(tracker, side, d)
            {
                taint[side.index()] |= 1 << row;
            }
        }
        // An Illusion user that fainted in disguise (Showdown shows no
        // `|replace|` then) left `|faint|` on its disguise's record.
        if st.team.iter().any(|m| m.is_fainted() && illusion_user(m)) {
            for m in st.team.iter().filter(|m| !m.is_fainted()) {
                if let Some(row) = row_of(tracker, side, m.id)
                    && pv.mon(row).is_some_and(|r| r.is_fainted())
                {
                    taint[side.index()] |= 1 << row;
                }
            }
        }
        // An Illusion user that fainted disguised: its own record can't
        // know.
        for m in st.team.iter().filter(|m| m.is_fainted() && illusion_user(m)) {
            if let Some(row) = row_of(tracker, side, m.id)
                && pv.mon(row).is_some_and(|r| !r.is_fainted())
            {
                taint[side.index()] |= 1 << row;
            }
        }
        for m in st.team.iter() {
            let Some(row) = row_of(tracker, side, m.id) else { continue };
            if taint[side.index()] & (1 << row) != 0 {
                continue;
            }
            let Some(rec) = pv.mon(row) else { continue };
            // An Illusion user's own record misses what it did disguised
            // (an Illusion that never broke): the item it gave or got, its
            // Terastallization.
            let missed = |what: &str| {
                illusion_user(m)
                    && (what == "item"
                        || what == "terastallized" && !rec.flags.has(pikapro_view::MonFlags::TERASTALLIZED))
            };
            // An Illusion user that Terastallized disguised showed it under
            // another's name, maybe between two decision points (U-turn,
            // Roar): any record of its side may keep its Tera type.
            let tera = |what: &str| {
                matches!(what, "terastallized" | "tera type")
                    && rec.tera_type.is_some_and(|t| st.team.iter().any(|m| illusion_user(m) && m.tera_type == t))
            };
            // A Sticky Barb moves on contact without a line, unless the
            // attacker holds an item: a foe's disguise can show an item its
            // Illusion user doesn't hold.
            let foe = side.foe();
            let foe_disguised = taint[foe.index()] != 0
                || battle.state().side(foe).team.iter().any(|m| m.illusion.is_some() || illusion_user(m));
            let barb = |what: &str| what == "item" && holds_barb(rec.item) && foe_disguised;
            if let Err((what, detail)) = pikapro_view::check::check_row(rec, m, battle)
                && !skip.contains(&what)
                && !missed(what)
                && !barb(what)
                && !tera(what)
            {
                return fail(what, format!("{side:?} row {row} (a side with Illusion): {what}: {detail}"));
            }
        }
    }
    Ok(())
}

/// A record claims a Sticky Barb (held, or passed on).
fn holds_barb(item: pikapro_view::ItemState) -> bool {
    use pikapro_data::gen9::items::STICKY_BARB;
    matches!(item, pikapro_view::ItemState::Known(STICKY_BARB))
        || matches!(item, pikapro_view::ItemState::Gone { item: Some(STICKY_BARB), .. })
}

/// Without team preview the true team order must not leak: rows are the
/// order of first appearance (`reveal_order` is `0, 1, 2, ...`), and only
/// Pokémon that appeared have one.
pub fn team_order(format: &pikapro::Format, tracker: &PublicTracker, taint: &[u8; 2]) -> Result<(), Failure> {
    if format.team_preview.shown() {
        return Ok(());
    }
    for side in SideId::BOTH {
        let pv = tracker.side(side);
        // A disguise's row, named first, may be revealed for real later.
        let untainted: Vec<u8> =
            pv.reveal_order.iter().copied().filter(|&r| taint[side.index()] & (1 << r) == 0).collect();
        let increasing = untainted.windows(2).all(|w| w[0] < w[1]);
        // A row not revealed can only be a disguise's (an Illusion named
        // it; `|replace|` took the switch-in back).
        let unrevealed_ok = pv.mons.iter().enumerate().all(|(row, m)| {
            let listed = pv.reveal_order.contains(&(row as u8));
            listed == m.is_revealed() && (listed || taint[side.index()] & (1 << row) != 0)
        });
        if !increasing || !unrevealed_ok {
            return Err(Failure::new(
                "team-order",
                "rows",
                format!(
                    "{}: rows {} with reveal order {:?}, revealed {:?}",
                    pikapro_protocol::side_id(side),
                    pv.mons.len(),
                    pv.reveal_order.as_ref(),
                    pv.mons.iter().map(|m| m.is_revealed()).collect::<Vec<_>>()
                ),
            ));
        }
    }
    Ok(())
}
