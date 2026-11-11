//! Encoder tests: ranges and NaNs over random battles, determinism, and
//! the facts [`MonRef`] works out against the tracker and the engine.

use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Format, Seed, SideId, Team};
use pikapro_data::gen9::{moves, species};
use pikapro_data::{Type, TypeSet};
use pikapro_teams::{Mode, Options};
use pikapro_view::{OwnOverlay, PlayerView, PublicTracker, PublicVolatiles};

use super::ctx::ROWS;
use super::*;
use crate::tables::Tables;

/// The seeds of these tests start here.
const SEED: u64 = 4_400_000;

fn battle(format: &Format, seed: u64, random_teams: bool) -> Battle {
    let teams = if random_teams {
        let g = crate::teams::generator(format, Mode::Sets, Options::default()).unwrap();
        crate::teams::pair(&g, seed).unwrap().map(|s| s.team)
    } else {
        [sample_team(format), sample_team(format)]
    };
    Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap()
}

/// Plays a random battle, calling `check` at each decision point with the
/// battle and the tracker fed every event so far.
fn play(format: &Format, seed: u64, random_teams: bool, check: impl FnMut(&Battle, &PublicTracker)) {
    play_battle(format, battle(format, seed, random_teams), seed, check);
}

/// [`play`] with `teams`.
fn play_teams(format: &Format, teams: [Team; 2], seed: u64, check: impl FnMut(&Battle, &PublicTracker)) {
    let b = Battle::new(format.clone(), teams, BattleOptions::new(Seed::fast(seed))).unwrap();
    play_battle(format, b, seed, check);
}

fn play_battle(format: &Format, mut b: Battle, seed: u64, mut check: impl FnMut(&Battle, &PublicTracker)) {
    let mut t = PublicTracker::for_format(format);
    let mut policy = BattleRng::new(Seed::fast(!seed));
    t.apply_all(b.drain_events().as_slice());
    check(&b, &t);
    while !b.is_over() {
        let choices = SideId::BOTH.map(|s| {
            let legal = b.legal_actions(s);
            (!legal.is_waiting()).then(|| legal.sample(&mut policy))
        });
        for (s, c) in SideId::BOTH.into_iter().zip(choices) {
            if let Some(c) = c {
                b.submit(s, c).unwrap();
            }
        }
        for e in b.drain_events() {
            t.apply(&e);
        }
        check(&b, &t);
    }
}

/// A foe row flagged active that no slot holds (a record an Illusion left
/// behind) is off the field: the encoders read the slots, as for own rows.
#[test]
fn a_foe_row_in_no_slot_is_off_the_field() {
    let format = Format::champions_vgc();
    let tables = Tables::for_format(&format);
    let mut b = battle(&format, SEED, false);
    let mut t = PublicTracker::for_format(&format);
    t.apply_all(b.drain_events().as_slice());
    let mut policy = BattleRng::new(Seed::fast(SEED));
    for s in SideId::BOTH {
        let legal = b.legal_actions(s);
        b.submit(s, legal.sample(&mut policy)).unwrap();
    }
    t.apply_all(b.drain_events().as_slice());
    let foe = &mut t.sides[1];
    let benched =
        (0..foe.mons.len()).find(|&r| foe.slots.iter().all(|s| s.row != Some(r as u8))).expect("a benched foe row");
    foe.mons[benched].flags.set(pikapro_view::MonFlags::ACTIVE, true);
    let own = OwnOverlay::from_request(&b.request_data(SideId::P1));
    let view = PlayerView::new(SideId::P1, &t, &own);
    let ctx = ViewCtx::new(&view, &tables);
    let m = ctx.mon(1, benched);
    assert!(m.present() && !m.is_active() && m.slot.is_none() && m.active.is_none());
    let names = Basic::columns();
    let mut out = vec![f32::NAN; Basic::SIZE];
    Basic.encode(&view, &tables, &mut out);
    let col = names.iter().position(|n| n == &format!("foe{benched}.active")).unwrap();
    assert_eq!(out[col], 0.0);
}

/// `basic` over every decision point of random battles: finite, fully
/// written, deterministic.
#[test]
fn random_battles_encode_in_range() {
    for (format, battles) in [(Format::champions_vgc(), 12), (Format::gen9_vgc(), 6), (Format::gen9_singles(), 6)] {
        let tables = Tables::for_format(&format);
        let mut buf = vec![0.0; Basic::SIZE];
        let mut points = 0;
        for seed in SEED..SEED + battles {
            play(&format, seed, true, |b, t| {
                points += 1;
                for me in SideId::BOTH {
                    let own = OwnOverlay::from_request(&b.request_data(me));
                    let view = PlayerView::new(me, t, &own);
                    buf.fill(f32::NAN);
                    Basic.encode(&view, &tables, &mut buf);
                    let mut again = vec![7.0; Basic::SIZE];
                    Basic.encode(&view, &tables, &mut again);
                    assert!(buf.iter().all(|x| x.is_finite()), "a value left unwritten or NaN");
                    assert_eq!(buf, again, "not deterministic");
                }
            });
        }
        assert!(points > 50, "{}: {points} decision points", format.id);
    }
}

/// `basic` writes a number near 0..1 everywhere, and names every value.
#[test]
fn basic_is_in_range_and_named() {
    let format = Format::champions_vgc();
    let tables = Tables::for_format(&format);
    let names = Basic::columns();
    assert_eq!(names.len(), Basic::SIZE);
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), Basic::SIZE, "unique names");
    let mut out = vec![0.0; Basic::SIZE];
    play(&format, SEED + 3, true, |b, t| {
        for me in SideId::BOTH {
            let own = OwnOverlay::from_request(&b.request_data(me));
            Basic.encode(&PlayerView::new(me, t, &own), &tables, &mut out);
            for (x, name) in out.iter().zip(&names) {
                assert!((-1.0..=1.5).contains(x), "{name} = {x}");
            }
        }
    });
}

#[test]
fn boxed_and_shared_encoders() {
    let boxed: Box<dyn Encoder> = Box::new(Basic);
    assert_eq!(boxed.size(), Basic::SIZE);
    let shared: std::sync::Arc<dyn Encoder> = std::sync::Arc::new(Basic);
    assert_eq!(shared.size(), Basic::SIZE);
}

/// Effectiveness: a Ground move against a Flying type is 0, a Water move
/// against Fire is 2x (0.5 once divided by 4).
#[test]
fn effectiveness_reads_the_chart() {
    let tables = Tables::for_format(&Format::gen9_vgc());
    let e = Types { set: pikapro_data::TypeSet::dual(Type::Fire, Type::Flying), added: None };
    assert_eq!(e.effect16(Type::Ground, &tables), 0);
    assert_eq!(e.effect16(Type::Water, &tables), 32);
    assert_eq!(e.effect16(Type::Rock, &tables), 64);
    // The sixteenths chart is the dex's (`Type::against`), and is exact
    // past two types.
    for attack in Type::TERA {
        for a in Type::ALL {
            for b in Type::ALL {
                let set = pikapro_data::TypeSet::dual(a, b);
                for added in [None, Some(Type::Grass)] {
                    let t = Types { set, added };
                    let e = attack.against_set(set);
                    let want = match added.map_or(e, |x| e.combine(attack.against(x))) {
                        pikapro_data::Effectiveness::Immune => 0,
                        pikapro_data::Effectiveness::Exp(n) => (16.0 * 2f32.powi(n.into())) as u32,
                    };
                    assert_eq!(t.effect16(attack, &tables), want, "{attack:?} vs {a:?}/{b:?} + {added:?}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------
// The facts `MonRef` works out, against the tracker and the state.

/// `MonRef::types` is Showdown's `getTypes` order (the tracker's `types`):
/// a Tera type first (without the added type), unless Stellar; then a
/// type change; then Roost's Flying removal. Set by hand on a foe slot.
#[test]
fn types_follow_get_types() {
    let format = Format::gen9_singles();
    let tables = Tables::for_format(&format);
    let b = battle(&format, SEED, false);
    let mut t = PublicTracker::for_format(&format);
    t.apply_all(b.events());
    let own = OwnOverlay::from_request(&b.request_data(SideId::P1));
    let (row, _) = t.sides[1].active(0).expect("a foe lead");
    let row = row as usize;
    let check = |t: &PublicTracker, want: TypeSet, added: Option<Type>, what: &str| {
        let view = PlayerView::new(SideId::P1, t, &own);
        let ctx = ViewCtx::new(&view, &tables);
        let types = ctx.active_in(1, 0).unwrap().types(&tables).unwrap();
        assert_eq!((types.set, types.added), (want, added), "{what}");
        assert_eq!(t.types(SideId::P2, 0), Some(want), "{what}: the tracker");
    };
    let changed = TypeSet::dual(Type::Dragon, Type::Flying);
    t.sides[1].slots[0].types = Some(changed);
    t.sides[1].slots[0].added_type = Some(Type::Grass);
    check(&t, changed, Some(Type::Grass), "a type change");
    t.sides[1].mons[row].tera_type = Some(Type::Steel);
    t.sides[1].mons[row].flags.set(pikapro_view::MonFlags::TERASTALLIZED, true);
    check(&t, TypeSet::mono(Type::Steel), None, "Tera over a type change");
    t.sides[1].mons[row].tera_type = Some(Type::Stellar);
    check(&t, changed, Some(Type::Grass), "Stellar keeps the types");
    t.sides[1].slots[0].volatiles.set(PublicVolatiles::ROOST);
    check(&t, TypeSet::mono(Type::Dragon), Some(Type::Grass), "roosting");
    t.sides[1].slots[0].types = Some(TypeSet::mono(Type::Flying));
    check(&t, TypeSet::mono(Type::Normal), Some(Type::Grass), "roosting, pure Flying");
}

/// Over random battles, the types of every active Pokémon are the
/// tracker's and the engine's (not under Illusion, which shows the
/// disguise's; not fainted).
#[test]
fn types_match_the_tracker_and_the_engine() {
    let mut checked = 0;
    for (format, battles) in [(Format::gen9_singles(), 10), (Format::gen9_vgc(), 6), (Format::champions_vgc(), 6)] {
        let tables = Tables::for_format(&format);
        for seed in 6_100_000..6_100_000 + battles {
            play(&format, seed, true, |b, t| {
                for me in SideId::BOTH {
                    let own = OwnOverlay::from_request(&b.request_data(me));
                    let view = PlayerView::new(me, t, &own);
                    let ctx = ViewCtx::new(&view, &tables);
                    for (ego, side) in [(0, me), (1, me.foe())] {
                        for slot in 0..ctx.active_slots() {
                            let (Some(m), Some(state)) = (ctx.active_in(ego, slot), b.state().side(side).active(slot))
                            else {
                                continue;
                            };
                            if state.is_fainted() || state.illusion.is_some() {
                                continue;
                            }
                            let ours = m.types(&tables).unwrap().set;
                            let at = format!("{} seed {seed}, {me:?}'s view, ego {ego} slot {slot}", format.id);
                            assert_eq!(Some(ours), t.types(side, slot), "{at}: the tracker");
                            assert_eq!(ours, state.types, "{at}: the engine");
                            checked += 1;
                        }
                    }
                }
            });
        }
    }
    assert!(checked > 1000, "{checked} checked");
}

/// The PP estimate's maximum is the request's (own active moves), in both
/// mechanics: Champions' `(min(pp, 20) / 5 + 1) * 4`, Scarlet/Violet's
/// `pp * 8 / 5`.
#[test]
fn max_pp_is_the_requests() {
    for format in [Format::gen9_vgc(), Format::champions_vgc()] {
        let tables = Tables::for_format(&format);
        let mut checked = 0;
        for seed in 6_100_100..6_100_104 {
            play(&format, seed, true, |b, t| {
                for me in SideId::BOTH {
                    let own = OwnOverlay::from_request(&b.request_data(me));
                    let view = PlayerView::new(me, t, &own);
                    let ctx = ViewCtx::new(&view, &tables);
                    for slot in 0..ctx.active_slots() {
                        let Some(m) = ctx.active_in(0, slot) else { continue };
                        // Copied moves (Transform, Mimic) have their own
                        // maximum, 5 in Transform's case.
                        let copied = PublicVolatiles::TRANSFORMED | PublicVolatiles::MIMIC;
                        if m.active.is_some_and(|a| a.volatiles.0 & copied != 0) {
                            continue;
                        }
                        for mv in m.moves() {
                            let (Some(id), Some(max)) = (mv.slot.id(), mv.max_pp) else { continue };
                            assert_eq!(
                                tables.move_facts(id).max_pp,
                                max,
                                "{}: {}",
                                format.id,
                                tables.dex.move_(id).name
                            );
                            checked += 1;
                        }
                    }
                }
            });
        }
        assert!(checked > 100, "{}: {checked} checked", format.id);
    }
    // The foe's estimate: PP spent over that maximum.
    let tables = Tables::for_format(&Format::champions_vgc());
    let agility = Format::champions_vgc().dex.move_by_name("Agility").unwrap();
    let mv = MoveInfo { slot: MoveKnowledge::Known(agility), pp_spent: 8, ..MoveInfo::default() };
    assert_eq!(tables.move_facts(agility).max_pp, 20);
    assert_eq!(mv.pp_frac(&tables), 12.0 / 20.0);
}

/// A foe whose forme team preview hid (`Urshifu-*`) has only the
/// types all its formes share.
#[test]
fn wildcard_formes_have_the_types_their_formes_share() {
    use pikapro::{BattleEvent, Gender};
    let format = Format::gen9_vgc();
    let tables = Tables::for_format(&format);
    let mut t = PublicTracker::for_format(&format);
    for s in [species::URSHIFU_RAPID_STRIKE, species::GRENINJA] {
        t.apply(&BattleEvent::Poke { side: SideId::P2, species: s, level: 50, gender: Gender::Male });
    }
    let own = OwnOverlay { side: SideId::P1, ..OwnOverlay::default() };
    let view = PlayerView::new(SideId::P1, &t, &own);
    let ctx = ViewCtx::new(&view, &tables);
    let urshifu = ctx.mon(1, 0);
    assert_eq!(urshifu.types(&tables), None, "the forme is hidden");
    assert_eq!(urshifu.wildcard_types(&tables), Some(([Some(Type::Fighting), None], false)), "Dark or Water");
    let greninja = ctx.mon(1, 1);
    assert_eq!(greninja.wildcard_types(&tables), Some(([Some(Type::Water), Some(Type::Dark)], true)), "every forme's");
}

/// With the player's team, own rows are complete: the nature,
/// the Pokémon left out at team preview (not `brought`, their sets), and a
/// benched Pokémon's PP (the last a request showed).
#[test]
fn own_rows_come_from_the_team() {
    let format = Format::gen9_vgc();
    let tables = Tables::for_format(&format);
    let teams = [sample_team(&format), sample_team(&format)];
    let mut b = Battle::new(format.clone(), teams.clone(), BattleOptions::new(Seed::fast(6_500_200))).unwrap();
    let mut t = PublicTracker::for_format(&format);
    t.apply_all(b.drain_events().as_slice());
    let mut policy = BattleRng::new(Seed::fast(6_500_200));
    for s in SideId::BOTH {
        let legal = b.legal_actions(s);
        b.submit(s, legal.sample(&mut policy)).unwrap();
    }
    t.apply_all(b.drain_events().as_slice());
    let me = SideId::P1;
    let own = OwnOverlay::from_request(&b.request_data(me));
    let mut team = pikapro_view::OwnTeam::from_team(&teams[0], &format);
    team.observe(&own);
    let view = PlayerView::new(me, &t, &own).with_team(&team);
    let ctx = ViewCtx::new(&view, &tables);
    let brought: Vec<u8> = own.mons.iter().map(|m| m.id).collect();
    for (id, set) in teams[0].sets.iter().enumerate() {
        let row = ctx.mon(0, id);
        assert_eq!(row.nature(), Some(set.nature), "row {id}");
        let is_brought = brought.contains(&(id as u8));
        assert_eq!(row.brought && row.own_mon.is_some(), is_brought);
        let m = team.get(id as u8).unwrap();
        assert_eq!(row.exact_stats().map(|s| s[5]), Some(m.stats[5]));
        if !is_brought {
            let item = set.item.map_or(pikapro_view::ItemState::NoItem, pikapro_view::ItemState::Known);
            assert_eq!(row.item(), item);
            // Left out: full HP, every PP.
            assert_eq!(row.hp(), (m.stats[0], m.stats[0]));
            let first = row.moves()[0];
            assert_eq!((first.pp, first.max_pp), (Some(m.max_pp[0]), Some(m.max_pp[0])));
            assert!(row.team_pos.is_none() && row.slot.is_none() && !row.is_active());
        }
    }
    assert_eq!(own.mons.len(), 4);
}

/// The PP of an own Pokémon's move outside a request that lists its moves
/// (a forced switch of the other slot, a wait, a locked or recharge turn,
/// team preview) is the team's last, never unknown while the team has it.
#[test]
fn own_active_pp_comes_from_the_team_outside_move_requests() {
    let mut outside = 0;
    for format in [Format::gen9_vgc(), Format::champions_vgc(), Format::gen9_singles()] {
        let tables = Tables::for_format(&format);
        let g = crate::teams::generator(&format, Mode::Sets, Options::default()).unwrap();
        for seed in 6_100_700..6_100_715 {
            let teams = crate::teams::pair(&g, seed).unwrap().map(|s| s.team);
            let mut own_teams = [0, 1].map(|i| pikapro_view::OwnTeam::from_team(&teams[i], &format));
            play_teams(&format, teams, seed, |b, t| {
                for me in SideId::BOTH {
                    let own = OwnOverlay::from_request(&b.request_data(me));
                    let team = &mut own_teams[me.index()];
                    team.observe(&own);
                    let view = PlayerView::new(me, t, &own).with_team(team);
                    let ctx = ViewCtx::new(&view, &tables);
                    for (pos, m) in own.mons.iter().enumerate() {
                        let listed =
                            own.active.get(pos).and_then(|a| a.as_ref()).is_some_and(
                                |a| matches!(&a.moves, pikapro_view::OwnMoves::Moves(ms) if !ms.is_empty()),
                            );
                        let row = ctx.mon(0, m.id as usize);
                        if listed || row.is_fainted() {
                            continue;
                        }
                        let tm = team.get(m.id).unwrap();
                        let slots = row.moves();
                        for (k, slot) in slots.iter().enumerate().take(m.moves.len()) {
                            if tm.moves.get(k) != Some(&m.moves[k]) {
                                continue;
                            }
                            assert_eq!(slot.pp, tm.last_pp(k), "{} seed {seed} {me:?}", format.id);
                        }
                        outside += usize::from(row.is_active());
                    }
                }
            });
        }
    }
    assert!(outside > 50, "{outside} active rows outside a move request");
}

/// Nothing is on the field at team preview: no own row is active or has a
/// slot (though the request marks the first ones `active`).
#[test]
fn nothing_is_on_the_field_at_team_preview() {
    for format in [Format::gen9_vgc(), Format::champions_vgc()] {
        let tables = Tables::for_format(&format);
        let teams = [sample_team(&format), sample_team(&format)];
        let b = Battle::new(format.clone(), teams.clone(), BattleOptions::new(Seed::fast(6_100_800))).unwrap();
        let mut t = PublicTracker::for_format(&format);
        t.apply_all(b.events());
        assert_eq!(t.phase, pikapro_view::Phase::TeamPreview);
        let own = OwnOverlay::from_request(&b.request_data(SideId::P1));
        assert!(own.mons.iter().any(|m| m.active), "the request marks the first ones active");
        let team = pikapro_view::OwnTeam::from_team(&teams[0], &format);
        let view = PlayerView::new(SideId::P1, &t, &own).with_team(&team);
        let ctx = ViewCtx::new(&view, &tables);
        assert_eq!(ctx.slot_rows, [[None; pikapro::MAX_ACTIVE]; 2]);
        for side in 0..2 {
            for row in 0..ROWS {
                let m = ctx.mon(side, row);
                if !m.present() {
                    continue;
                }
                assert!(!m.is_active() && m.slot.is_none() && m.active.is_none());
                if side == 0 {
                    // PP from the team, not unknown.
                    assert!(m.moves()[0].pp.is_some_and(|pp| pp > 0), "{}: own{row}", format.id);
                }
            }
        }
    }
}

/// A preview wildcard's moves are known only from an Open Team Sheet,
/// which shows its forme.
#[test]
fn a_sheet_shows_a_wildcards_forme() {
    use pikapro::{BattleEvent, Gender};
    let format = Format::gen9_vgc();
    let tables = Tables::for_format(&format);
    let mut t = PublicTracker::for_format(&format);
    let urshifu = species::URSHIFU_RAPID_STRIKE;
    t.apply(&BattleEvent::Poke { side: SideId::P2, species: urshifu, level: 50, gender: Gender::Male });
    assert!(t.sides[1].mons[0].flags.has(pikapro_view::MonFlags::FORME_HIDDEN));
    let mut sheet = pikapro_view::OpenSheet::default();
    let mut entry = pikapro_view::SheetMon { species: urshifu, level: 50, ..Default::default() };
    entry.moves.push(moves::SURGING_STRIKES);
    sheet.mons.push(entry);
    t.set_open_sheet(SideId::P2, sheet);
    let own = OwnOverlay { side: SideId::P1, ..OwnOverlay::default() };
    let view = PlayerView::new(SideId::P1, &t, &own);
    let ctx = ViewCtx::new(&view, &tables);
    assert!(!ctx.mon(1, 0).forme_hidden());
    assert_eq!(ctx.mon(1, 0).moves()[0].slot, MoveKnowledge::Known(moves::SURGING_STRIKES));
}
