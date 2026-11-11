//! Check 2b: metamorphic no-leak fuzzing.
//!
//! Hidden information of one side `q` is perturbed and the battle replayed
//! with the same choices. For the other player `p`, the views (the public
//! tracker and `p`'s overlay) must be bit-identical up to the first line
//! where `p`'s two channels differ: whatever `p` could not see must not
//! change what `p` knows.
//!
//! Perturbations ([`Kind`]): a move slot (preferably one the Pokémon never
//! used in the base battle), the held item, EVs and IVs or Stat Points
//! (not HP's, so the shown percentages stay put until damage differs), the
//! nature, the ability (another the species can have), and without team
//! preview the team order behind the lead (switch choices follow the
//! Pokémon, not the position).
//!
//! Both battles have every gender fixed first ([`Setup::with_pinned_genders`]),
//! so reordering sets doesn't change the RNG stream.
//!
//! The two channels are compared event by event: each event that shows
//! `p` something is one step (its lines, then the view after it), and each
//! request `p` receives is one (its JSON, then the view with the new
//! overlay, and `p`'s own team as its requests taught it,
//! `pikapro_view::OwnTeam`). The comparison stops at the first step whose
//! text differs, or when the replayed choices stop being valid for the
//! perturbed battle.

use std::collections::VecDeque;

use pikapro::format::StatSystem;
use pikapro::{Battle, BattleEvent, BattleRng, Choice, Investment, Progress, Seed, SideId, SlotAction};
use pikapro_data::{MoveId, Nature, Stat};
use pikapro_protocol::{Perspective, Renderer};
use pikapro_view::{OwnOverlay, OwnTeam, PublicTracker};

use super::{Failure, Setup, debug_diff, masked_debug, overlays};

/// Mixed into the battle seed for the perturbations' own draws.
const META_SALT: u64 = 0x6D65_7461;

/// What a perturbation changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Move,
    Item,
    /// EVs and IVs, or Stat Points (HP's kept).
    Spread,
    Nature,
    Ability,
    /// The team order behind the lead (no team preview only).
    Order,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Move, Kind::Item, Kind::Spread, Kind::Nature, Kind::Ability, Kind::Order];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Move => "move",
            Kind::Item => "item",
            Kind::Spread => "spread",
            Kind::Nature => "nature",
            Kind::Ability => "ability",
            Kind::Order => "order",
        }
    }
}

/// One perturbation and how far the two battles were compared.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub kind: Kind,
    pub side: SideId,
    pub description: String,
    /// Steps compared equal (lines and requests `p` was shown).
    pub steps: usize,
    /// Why the comparison stopped before the end, if it did.
    pub stopped: Option<String>,
}

/// Runs `n` perturbations of `setup`'s battle.
pub fn run(setup: &Setup, n: usize) -> Result<Vec<Outcome>, Failure> {
    run_kinds(setup, n, &Kind::ALL)
}

/// [`run`], with perturbations of the given kinds only.
pub fn run_kinds(setup: &Setup, n: usize, kinds: &[Kind]) -> Result<Vec<Outcome>, Failure> {
    let base = setup.with_pinned_genders();
    let used = moves_used(&base);
    let mut rng = BattleRng::new(Seed::fast(setup.seed ^ META_SALT));
    let mut out = Vec::new();
    for _ in 0..n {
        let Some((perturbed, map, kind, side, description)) = perturb(&base, &used, kinds, &mut rng) else { continue };
        let (steps, stopped) = compare(&base, &perturbed, side, map.as_deref())
            .map_err(|f| Failure { message: format!("{} ({description}): {}", kind.name(), f.message), ..f })?;
        out.push(Outcome { kind, side, description, steps, stopped });
    }
    Ok(out)
}

/// The moves each Pokémon (by side and id) used in the base battle.
fn moves_used(base: &Setup) -> [Vec<Vec<MoveId>>; 2] {
    let mut battle = base.battle();
    let names = base.names();
    let mut driver = super::Driver::random(base.seed);
    let mut log = Vec::new();
    while !battle.is_over() {
        if !matches!(driver.advance(&mut battle, &names, &mut log), Ok(true)) {
            break;
        }
    }
    let mut used = [vec![Vec::new(); 6], vec![Vec::new(); 6]];
    for e in battle.events() {
        if let BattleEvent::Move { user, move_id, .. } = *e
            && let Some(list) = used[user.pos.side.index()].get_mut(user.id as usize)
        {
            list.push(move_id);
        }
    }
    used
}

type Perturbed = (Setup, Option<Vec<u8>>, Kind, SideId, String);

/// A random perturbation of one side's hidden information, with the map
/// from base ids to perturbed ids for [`Kind::Order`].
fn perturb(base: &Setup, used: &[Vec<Vec<MoveId>>; 2], kinds: &[Kind], rng: &mut BattleRng) -> Option<Perturbed> {
    let format = &base.format;
    let dex = format.dex;
    let side = SideId::BOTH[rng.random(2) as usize];
    let kinds: Vec<Kind> = kinds
        .iter()
        .copied()
        .filter(|&k| {
            k != Kind::Order || (!format.team_preview.shown() && base.sheets[side.index()].team.sets.len() > 2)
        })
        .collect();
    if kinds.is_empty() {
        return None;
    }
    let kind = kinds[rng.random(kinds.len() as u32) as usize];
    let mut out = base.clone();
    let sheet = &mut out.sheets[side.index()];
    let n = sheet.team.sets.len();
    let id = rng.random(n as u32) as usize;
    let who = format!("{} {}", pikapro_protocol::side_id(side), dex.species(sheet.team.sets[id].species).name);
    let description = match kind {
        Kind::Move => {
            let set = &mut sheet.team.sets[id];
            let pool = crate::move_pool(format);
            let fresh: Vec<MoveId> = pool.into_iter().filter(|m| !set.moves.contains(m)).collect();
            let to = fresh[rng.random(fresh.len() as u32) as usize];
            let unused: Vec<usize> =
                (0..set.moves.len()).filter(|&k| !used[side.index()][id].contains(&set.moves[k])).collect();
            if unused.is_empty() && set.moves.len() < 4 {
                set.moves.push(to);
                return Some((out, None, kind, side, format!("{who}: add {}", dex.move_(to).name)));
            }
            let slot = if unused.is_empty() {
                rng.random(set.moves.len() as u32) as usize
            } else {
                unused[rng.random(unused.len() as u32) as usize]
            };
            let from = set.moves[slot];
            set.moves[slot] = to;
            format!("{who}: {} -> {}", dex.move_(from).name, dex.move_(to).name)
        }
        Kind::Item => {
            let held: Vec<_> = sheet.team.sets.iter().filter_map(|s| s.item).collect();
            let set = &mut sheet.team.sets[id];
            let pool: Vec<_> = crate::item_pool(format).into_iter().filter(|i| !held.contains(i)).collect();
            let to = if set.item.is_some() && (pool.is_empty() || rng.random(4) == 0) {
                None
            } else {
                Some(pool[rng.random(pool.len() as u32) as usize])
            };
            let from = set.item;
            set.item = to;
            let name = |i: Option<pikapro_data::ItemId>| i.map_or("(none)", |i| dex.item(i).name);
            format!("{who}: {} -> {}", name(from), name(to))
        }
        Kind::Spread => {
            let set = &mut sheet.team.sets[id];
            set.investment = match (format.stat_system, set.investment) {
                (StatSystem::EffortValues, Investment::Evs { mut evs, mut ivs }) => {
                    let mut left = 510u32.saturating_sub(evs[Stat::Hp] as u32);
                    for stat in &Stat::ALL[1..] {
                        let v = rng.random(252.min(left) + 1);
                        evs[*stat] = v as u8;
                        left -= v;
                        ivs[*stat] = rng.random(32) as u8;
                    }
                    Investment::Evs { evs, ivs }
                }
                (_, Investment::StatPoints(mut sp)) => {
                    let mut left = 66u32.saturating_sub(sp[Stat::Hp] as u32);
                    for stat in &Stat::ALL[1..] {
                        let v = rng.random(32.min(left) + 1);
                        sp[*stat] = v as u8;
                        left -= v;
                    }
                    Investment::StatPoints(sp)
                }
                (_, other) => other,
            };
            format!("{who}: {:?}", set.investment)
        }
        Kind::Nature => {
            let set = &mut sheet.team.sets[id];
            let from = set.nature;
            let others: Vec<Nature> = Nature::ALL.into_iter().filter(|&n| n != from).collect();
            set.nature = others[rng.random(others.len() as u32) as usize];
            format!("{who}: {} -> {}", from.name(), set.nature.name())
        }
        Kind::Ability => {
            let set = &mut sheet.team.sets[id];
            let slots = dex.species(set.species).abilities;
            let others: Vec<_> = [Some(slots.primary), slots.secondary, slots.hidden]
                .into_iter()
                .flatten()
                .filter(|&a| a != set.ability && crate::IMPLEMENTED_ABILITIES.contains(&a))
                .collect();
            if others.is_empty() {
                return None;
            }
            let from = set.ability;
            set.ability = others[rng.random(others.len() as u32) as usize];
            format!("{who}: {} -> {}", dex.ability(from).name, dex.ability(set.ability).name)
        }
        Kind::Order => {
            // perm[j]: the base id of the set at position j.
            let mut perm: Vec<u8> = (0..n as u8).collect();
            let mut tail = perm[1..].to_vec();
            rng.shuffle(&mut tail);
            perm[1..].copy_from_slice(&tail);
            let sets = perm.iter().map(|&a| sheet.team.sets[a as usize].clone()).collect();
            let extras = perm.iter().map(|&a| sheet.extras.get(a as usize).cloned().unwrap_or_default()).collect();
            sheet.team.sets = sets;
            sheet.extras = extras;
            let mut map = vec![0u8; n];
            for (j, &a) in perm.iter().enumerate() {
                map[a as usize] = j as u8;
            }
            let description = format!("{}: order {perm:?}", pikapro_protocol::side_id(side));
            return Some((out, Some(map), kind, side, description));
        }
    };
    Some((out, None, kind, side, description))
}

/// One step of a player's channel: what it was shown and the view after.
struct Step {
    text: String,
    tracker: PublicTracker,
    overlay: Option<OwnOverlay>,
    /// `p`'s own team with what its requests so far showed.
    team: Option<OwnTeam>,
}

/// A battle replayed for the comparison, with `p`'s steps not yet compared.
struct Run {
    battle: Battle,
    tracker: PublicTracker,
    team: OwnTeam,
    renderer: Renderer,
    start: usize,
    steps: VecDeque<Step>,
}

impl Run {
    fn new(setup: &Setup, p: SideId) -> Run {
        let battle = setup.battle();
        Run {
            battle,
            tracker: super::tracker(setup),
            team: OwnTeam::from_team(&setup.sheets[p.index()].team, &setup.format),
            renderer: setup.renderer(Perspective::Player(p)),
            start: 0,
            steps: VecDeque::new(),
        }
    }

    /// Applies the new events and queues `p`'s steps (and `p`'s request).
    fn absorb(&mut self, p: SideId) {
        let events = &self.battle.events()[self.start..];
        for event in events {
            self.tracker.apply(event);
            let lines = self.renderer.log(std::slice::from_ref(event));
            let text = match event {
                BattleEvent::MoveAttr { attr } => format!("(the last |move| line gets {attr:?})"),
                _ if lines.is_empty() => continue,
                _ => lines.join("\n"),
            };
            self.steps.push_back(Step { text, tracker: self.tracker, overlay: None, team: None });
        }
        self.start = self.battle.events().len();
        if !self.battle.is_over() {
            let data = self.battle.request_data(p);
            let overlay = overlays(&self.battle)[p.index()];
            self.team.observe(&overlay);
            self.steps.push_back(Step {
                text: format!("|request|{}", self.renderer.request_json(&data)),
                tracker: self.tracker,
                overlay: Some(overlay),
                team: Some(self.team),
            });
        }
    }
}

/// Compares `p`'s views in the base and perturbed battles, `p` being the
/// foe of the perturbed side `q`. `map` takes base ids to perturbed ids
/// (a reordered team). Returns the steps compared and why it stopped.
fn compare(base: &Setup, perturbed: &Setup, q: SideId, map: Option<&[u8]>) -> Result<(usize, Option<String>), Failure> {
    let p = q.foe();
    let mut a = Run::new(base, p);
    if perturbed.try_battle().is_err() {
        return Ok((0, Some("the perturbed team is invalid".into())));
    }
    let mut b = Run::new(perturbed, p);
    let mut rng = BattleRng::new(Seed::fast(base.seed ^ super::POLICY_SALT));
    let mut shown: Vec<String> = Vec::new();
    let mut steps = 0;
    loop {
        a.absorb(p);
        b.absorb(p);
        while let (Some(x), Some(y)) = (a.steps.front(), b.steps.front()) {
            if x.text != y.text {
                return Ok((steps, None));
            }
            let same_tracker =
                x.tracker == y.tracker || (map.is_some() && masked_debug(&x.tracker) == masked_debug(&y.tracker));
            if !same_tracker {
                return Err(Failure::new(
                    "metamorphic",
                    "tracker",
                    format!(
                        "the public view differs after `{}`:\n        {}",
                        x.text,
                        if map.is_some() {
                            super::text_diff(&masked_debug(&x.tracker), &masked_debug(&y.tracker))
                        } else {
                            debug_diff(&x.tracker, &y.tracker)
                        }
                    ),
                )
                .with_context(&shown));
            }
            if x.overlay != y.overlay {
                return Err(Failure::new(
                    "metamorphic",
                    "overlay",
                    format!(
                        "{}'s overlay differs:\n        {}",
                        pikapro_protocol::side_id(p),
                        debug_diff(&x.overlay, &y.overlay)
                    ),
                )
                .with_context(&shown));
            }
            if x.team != y.team {
                return Err(Failure::new(
                    "metamorphic",
                    "team",
                    format!(
                        "{}'s own team differs:\n        {}",
                        pikapro_protocol::side_id(p),
                        debug_diff(&x.team, &y.team)
                    ),
                )
                .with_context(&shown));
            }
            shown.push(x.text.clone());
            steps += 1;
            a.steps.pop_front();
            b.steps.pop_front();
        }
        if a.battle.is_over() || b.battle.is_over() {
            let stopped = (a.battle.is_over() != b.battle.is_over()).then(|| "one battle ended first".to_string());
            return Ok((steps, stopped));
        }
        // The base battle's choices, replayed on the perturbed one.
        let mut resolved = false;
        for side in SideId::BOTH {
            let legal = a.battle.legal_actions(side);
            if legal.is_waiting() {
                continue;
            }
            let choice = legal.sample(&mut rng);
            let translated = match map {
                Some(map) if side == q => translate(&a.battle, &b.battle, side, choice, map),
                _ => choice,
            };
            let rb = b.battle.submit(side, translated);
            let ra = a.battle.submit(side, choice).expect("a sampled choice is legal");
            match rb {
                Err(e) => return Ok((steps, Some(format!("the perturbed battle rejects {translated:?}: {e}")))),
                Ok(pb) if (pb == Progress::Resolved) != (ra == Progress::Resolved) => {
                    return Ok((steps, Some("the battles resolve at different points".into())));
                }
                Ok(_) => {}
            }
            if ra == Progress::Resolved {
                resolved = true;
                break;
            }
        }
        if !resolved {
            return Ok((steps, Some("no side resolved".into())));
        }
    }
}

/// A base battle's choice for `side` in the perturbed battle, whose team is
/// reordered: a switch sends in the same Pokémon.
fn translate(a: &Battle, b: &Battle, side: SideId, choice: Choice, map: &[u8]) -> Choice {
    let Choice::Actions(mut actions) = choice else { return choice };
    for action in &mut actions {
        if let SlotAction::Switch(i) = *action {
            let base_id = a.state().side(side).team[i as usize].id;
            let id = map[base_id as usize];
            if let Some(j) = b.state().side(side).position_of(id) {
                *action = SlotAction::Switch(j as u8);
            }
        }
    }
    Choice::Actions(actions)
}
