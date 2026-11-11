//! Teams built around one mechanic: random battles over many seeds with
//! every check of the fuzz, and scripted battles that check the facts
//! the mechanic reveals.

use pikapro::{Battle, BattleRng, Choice, Format, Gimmick, Investment, PokemonSet, SideId, SlotAction, Target, Team};
use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
use pikapro_data::{Stats, Type};

use super::fuzz::{Opts, Seen, run, run_with};
use crate::*;

fn set(
    species: pikapro_data::SpeciesId,
    ability: pikapro_data::AbilityId,
    item: Option<pikapro_data::ItemId>,
    moves: &[pikapro_data::MoveId],
) -> PokemonSet {
    let mut p = PokemonSet::new(species).with_level(50).with_ability(ability).with_moves(moves);
    if let Some(item) = item {
        p = p.with_item(item);
    }
    p.with_investment(Investment::Evs { evs: Stats::new(84, 84, 84, 84, 84, 84), ivs: Stats::splat(31) })
}

fn champions(p: PokemonSet) -> PokemonSet {
    p.with_investment(Investment::StatPoints(Stats::new(11, 11, 11, 11, 11, 11)))
}

/// Filler Pokémon with plain attacks, for teams that need six.
fn filler(n: usize) -> Vec<PokemonSet> {
    let pool = [
        set(s::SNORLAX, a::THICK_FAT, Some(i::LEFTOVERS), &[m::BODY_SLAM, m::PROTECT, m::REST, m::SLEEP_TALK]),
        set(
            s::GARCHOMP,
            a::ROUGH_SKIN,
            Some(i::ROCKY_HELMET),
            &[m::EARTHQUAKE, m::DRAGON_CLAW, m::PROTECT, m::SWORDS_DANCE],
        ),
        set(
            s::AMOONGUSS,
            a::REGENERATOR,
            Some(i::SITRUS_BERRY),
            &[m::SPORE, m::GIGA_DRAIN, m::PROTECT, m::RAGE_POWDER],
        ),
        set(
            s::INCINEROAR,
            a::INTIMIDATE,
            Some(i::SAFETY_GOGGLES),
            &[m::FAKE_OUT, m::FLARE_BLITZ, m::KNOCK_OFF, m::PARTING_SHOT],
        ),
        set(s::GARDEVOIR, a::TRACE, Some(i::CHOICE_SCARF), &[m::MOONBLAST, m::TRICK, m::SKILL_SWAP, m::PSYCHIC]),
        set(s::PIKACHU, a::STATIC, Some(i::LIGHT_BALL), &[m::THUNDERBOLT, m::FAKE_OUT, m::PROTECT, m::KNOCK_OFF]),
    ];
    pool.into_iter().take(n).collect()
}

fn team(mut sets: Vec<PokemonSet>, size: usize, format: &Format) -> Team {
    // Species and item clauses: filler that doesn't repeat.
    for f in filler(6) {
        if sets.len() >= size {
            break;
        }
        if sets.iter().all(|x| x.species != f.species && (x.item.is_none() || x.item != f.item)) {
            sets.push(f);
        }
    }
    if format.stat_system == pikapro::StatSystem::StatPoints {
        sets = sets.into_iter().map(champions).collect();
    }
    Team::new(sets)
}

/// The scenarios: a name, a format and both teams.
fn scenarios() -> Vec<(&'static str, Format, [Team; 2])> {
    let singles = Format::gen9_singles();
    let doubles = Format::gen9_doubles();
    let vgc = Format::gen9_vgc();
    let champ = Format::champions_vgc();
    let foe = |f: &Format, n| team(vec![], n, f);
    vec![
        (
            "illusion",
            singles.clone(),
            [
                team(
                    vec![
                        set(
                            s::ZOROARK,
                            a::ILLUSION,
                            Some(i::LIFE_ORB),
                            &[m::NIGHT_DAZE, m::FLAMETHROWER, m::U_TURN, m::KNOCK_OFF],
                        ),
                        set(
                            s::ZOROARK_HISUI,
                            a::ILLUSION,
                            Some(i::FOCUS_SASH),
                            &[m::SHADOW_BALL, m::HYPER_VOICE, m::U_TURN, m::TRICK],
                        ),
                    ],
                    6,
                    &singles,
                ),
                foe(&singles, 6),
            ],
        ),
        (
            "illusion doubles",
            doubles.clone(),
            [
                team(
                    vec![set(
                        s::ZOROARK,
                        a::ILLUSION,
                        Some(i::LIFE_ORB),
                        &[m::NIGHT_DAZE, m::FLAMETHROWER, m::U_TURN, m::PROTECT],
                    )],
                    6,
                    &doubles,
                ),
                foe(&doubles, 6),
            ],
        ),
        (
            "imposter",
            singles.clone(),
            [
                team(vec![set(s::DITTO, a::IMPOSTER, Some(i::CHOICE_SCARF), &[m::TRANSFORM])], 3, &singles),
                team(vec![set(s::DITTO, a::IMPOSTER, Some(i::FOCUS_SASH), &[m::TRANSFORM])], 3, &singles),
            ],
        ),
        (
            "trick and knock off",
            doubles.clone(),
            [
                team(
                    vec![
                        set(
                            s::GARDEVOIR,
                            a::TRACE,
                            Some(i::CHOICE_SCARF),
                            &[m::TRICK, m::SKILL_SWAP, m::MOONBLAST, m::PROTECT],
                        ),
                        set(s::WEAVILE, a::PICKPOCKET, None, &[m::KNOCK_OFF, m::THIEF, m::FAKE_OUT, m::ICE_SHARD]),
                    ],
                    6,
                    &doubles,
                ),
                team(
                    vec![
                        set(
                            s::INCINEROAR,
                            a::INTIMIDATE,
                            Some(i::SITRUS_BERRY),
                            &[m::KNOCK_OFF, m::FAKE_OUT, m::FLARE_BLITZ, m::PARTING_SHOT],
                        ),
                        set(s::KLEFKI, a::MAGICIAN, None, &[m::SWITCHEROO, m::THIEF, m::DAZZLING_GLEAM, m::PROTECT]),
                    ],
                    6,
                    &doubles,
                ),
            ],
        ),
        (
            "skill swap and trace",
            doubles.clone(),
            [
                team(
                    vec![
                        set(
                            s::GARDEVOIR,
                            a::TRACE,
                            Some(i::CHOICE_SPECS),
                            &[m::SKILL_SWAP, m::ROLE_PLAY, m::MOONBLAST, m::PROTECT],
                        ),
                        set(
                            s::COFAGRIGUS,
                            a::MUMMY,
                            Some(i::LEFTOVERS),
                            &[m::SHADOW_BALL, m::WILL_O_WISP, m::PROTECT, m::TRICK_ROOM],
                        ),
                    ],
                    6,
                    &doubles,
                ),
                team(
                    vec![set(
                        s::RUNERIGUS,
                        a::WANDERING_SPIRIT,
                        Some(i::LEFTOVERS),
                        &[m::SHADOW_BALL, m::EARTHQUAKE, m::PROTECT, m::TRICK_ROOM],
                    )],
                    6,
                    &doubles,
                ),
            ],
        ),
        (
            "megas",
            champ.clone(),
            [
                team(
                    vec![
                        set(
                            s::CHARIZARD,
                            a::BLAZE,
                            Some(i::CHARIZARDITE_Y),
                            &[m::HEAT_WAVE, m::AIR_SLASH, m::PROTECT, m::SOLAR_BEAM],
                        ),
                        set(
                            s::GENGAR,
                            a::CURSED_BODY,
                            Some(i::GENGARITE),
                            &[m::SHADOW_BALL, m::SLUDGE_BOMB, m::PROTECT, m::IMPRISON],
                        ),
                    ],
                    6,
                    &champ,
                ),
                team(
                    vec![set(
                        s::KANGASKHAN,
                        a::SCRAPPY,
                        Some(i::KANGASKHANITE),
                        &[m::FAKE_OUT, m::DOUBLE_EDGE, m::SUCKER_PUNCH, m::PROTECT],
                    )],
                    6,
                    &champ,
                ),
            ],
        ),
        (
            "tera formes",
            vgc.clone(),
            [
                team(
                    vec![
                        set(
                            s::OGERPON_WELLSPRING,
                            a::WATER_ABSORB,
                            Some(i::WELLSPRING_MASK),
                            &[m::IVY_CUDGEL, m::HORN_LEECH, m::FOLLOW_ME, m::SPIKY_SHIELD],
                        )
                        .with_tera_type(Type::Water),
                        set(
                            s::TERAPAGOS,
                            a::TERA_SHIFT,
                            Some(i::LEFTOVERS),
                            &[m::TERA_STARSTORM, m::EARTH_POWER, m::PROTECT, m::CALM_MIND],
                        )
                        .with_tera_type(Type::Stellar),
                    ],
                    6,
                    &vgc,
                ),
                foe(&vgc, 6),
            ],
        ),
        (
            "commander",
            doubles.clone(),
            [
                team(
                    vec![
                        set(
                            s::TATSUGIRI,
                            a::COMMANDER,
                            Some(i::CHOICE_SCARF),
                            &[m::DRACO_METEOR, m::MUDDY_WATER, m::ICY_WIND, m::TAUNT],
                        ),
                        set(
                            s::DONDOZO,
                            a::UNAWARE,
                            Some(i::LEFTOVERS),
                            &[m::WAVE_CRASH, m::ORDER_UP, m::PROTECT, m::EARTHQUAKE],
                        ),
                    ],
                    6,
                    &doubles,
                ),
                foe(&doubles, 6),
            ],
        ),
        (
            "revival blessing and sleep talk",
            singles.clone(),
            [
                team(
                    vec![
                        set(
                            s::PAWMOT,
                            a::VOLT_ABSORB,
                            Some(i::FOCUS_SASH),
                            &[m::REVIVAL_BLESSING, m::DOUBLE_SHOCK, m::MACH_PUNCH, m::NUZZLE],
                        ),
                        set(
                            s::SNORLAX,
                            a::THICK_FAT,
                            Some(i::CHESTO_BERRY),
                            &[m::REST, m::SLEEP_TALK, m::BODY_SLAM, m::CURSE],
                        ),
                    ],
                    4,
                    &singles,
                ),
                team(
                    vec![
                        set(
                            s::AMOONGUSS,
                            a::REGENERATOR,
                            Some(i::BLACK_SLUDGE),
                            &[m::SPORE, m::GIGA_DRAIN, m::TOXIC, m::PROTECT],
                        ),
                        set(
                            s::GOTHITELLE,
                            a::SHADOW_TAG,
                            Some(i::LEFTOVERS),
                            &[m::PSYCHIC, m::IMPRISON, m::PROTECT, m::TRICK_ROOM],
                        ),
                    ],
                    4,
                    &singles,
                ),
            ],
        ),
        (
            "regenerator champions",
            champ.clone(),
            [
                team(
                    vec![set(
                        s::AMOONGUSS,
                        a::REGENERATOR,
                        Some(i::SITRUS_BERRY),
                        &[m::SPORE, m::POLLEN_PUFF, m::RAGE_POWDER, m::PROTECT],
                    )],
                    6,
                    &champ,
                ),
                foe(&champ, 6),
            ],
        ),
    ]
}

/// Every scenario, 40 seeds each, all checks of the fuzz; half with exact
/// HP.
#[test]
fn targeted_teams_are_sound_and_leak_free() {
    let mut seen = Seen::default();
    for (k, (name, format, teams)) in scenarios().into_iter().enumerate() {
        for i in 0..40u64 {
            let seed = 4_250_000 + k as u64 * 1000 + i;
            let hp = if i % 2 == 0 { HpReport::Percent } else { HpReport::Exact };
            let opts = Opts { hp, ots: i % 4 == 1, ..Opts::default() };
            if let Err(e) = run(&format, teams.clone(), seed, opts, &mut seen) {
                panic!("{name}: {e}");
            }
        }
    }
    eprintln!("targeted: {} battles, {} decision points, {} refused choices", seen.battles, seen.points, seen.rejected);
    assert!(seen.rejected > 0, "Shadow Tag's hidden trap was refused somewhere");
}

/// A scripted battle: `script` picks each side's choice (falling back to
/// a random legal one), all checks run, `check` sees every decision
/// point.
fn scripted(
    format: &Format,
    teams: [Team; 2],
    seed: u64,
    mut script: impl FnMut(&Battle, SideId) -> Option<Choice>,
    mut check: impl FnMut(&Battle, &PublicTracker),
) {
    let mut seen = Seen::default();
    let mut policy = |b: &Battle, side: SideId, rng: &mut BattleRng| {
        script(b, side)
            .filter(|c| b.legal_actions(side).is_legal(c))
            .unwrap_or_else(|| b.legal_actions(side).sample(rng))
    };
    run_with(format, teams, seed, Opts::default(), &mut seen, &mut policy, &mut check).unwrap();
}

fn use_move(index: u8) -> Choice {
    Choice::single(SlotAction::Move { index, target: None, gimmick: None })
}

#[test]
fn illusion_breaks_and_the_disguise_is_restored() {
    let format = Format::gen9_singles();
    let zoroark = set(s::ZOROARK, a::ILLUSION, Some(i::LIFE_ORB), &[m::NIGHT_DAZE]);
    let p1 = Team::new(vec![zoroark, set(s::PIKACHU, a::STATIC, None, &[m::THUNDERBOLT])]);
    let p2 = Team::new(vec![set(s::GARCHOMP, a::ROUGH_SKIN, None, &[m::EARTHQUAKE])]);
    let mut replaced = false;
    scripted(
        &format,
        [p1, p2],
        4_260_001,
        |_, _| Some(use_move(0)),
        |b, t| {
            let side = t.side(SideId::P1);
            let zoroark_on = b.state().side(SideId::P1).active(0).is_some_and(|m| m.id == 0);
            if b.turn() == 1 && zoroark_on {
                // Disguised as Pikachu (id 1): row 0 is Pikachu's.
                assert_eq!(side.mons[0].species, s::PIKACHU);
            }
            if b.turn() == 2 && zoroark_on && b.state().side(SideId::P1).active(0).unwrap().illusion.is_none() {
                replaced = true;
                // Pikachu was never seen after all; Zoroark (row 1) has Night
                // Daze and its Life Orb.
                assert!(!side.mons[0].is_revealed());
                assert!(side.mons[0].moves.is_empty());
                let z = &side.mons[1];
                assert_eq!(z.species, s::ZOROARK);
                assert!(z.revealed(m::NIGHT_DAZE).is_some());
                assert_eq!(z.item, ItemState::Known(i::LIFE_ORB));
                assert_eq!(side.reveal_order.as_ref(), [1]);
            }
        },
    );
    assert!(replaced, "the Illusion broke on the first hit");
}

#[test]
fn imposter_copies_and_keeps_its_own_moves_apart() {
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![set(s::DITTO, a::IMPOSTER, Some(i::CHOICE_SCARF), &[m::TRANSFORM])]);
    let p2 = Team::new(vec![set(s::GARCHOMP, a::ROUGH_SKIN, None, &[m::SWORDS_DANCE, m::EARTHQUAKE])]);
    let mut seen_transformed = false;
    scripted(
        &format,
        [p1, p2],
        4_260_002,
        |_, side| Some(use_move(if side == SideId::P1 { 1 } else { 0 })),
        |b, t| {
            let av = &t.side(SideId::P1).slots[0];
            if av.volatiles.has(PublicVolatiles::TRANSFORMED) {
                seen_transformed = true;
                assert_eq!(av.transformed.map(|x| x.species), Some(s::GARCHOMP));
                assert_eq!(t.types(SideId::P1, 0), Some(pikapro_data::TypeSet::dual(Type::Dragon, Type::Ground)));
                let ditto = t.side(SideId::P1).mon(0).unwrap();
                assert_eq!(ditto.base_ability, AbilityState::Known(a::IMPOSTER));
                assert!(ditto.moves.iter().all(|x| x.id == m::TRANSFORM), "{:?}", ditto.moves);
                if b.turn() >= 2 {
                    assert!(av.copied_moves.contains(&m::EARTHQUAKE));
                }
            }
        },
    );
    assert!(seen_transformed);
}

#[test]
fn trick_and_knock_off_move_items() {
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![set(s::GARDEVOIR, a::TRACE, Some(i::CHOICE_SCARF), &[m::TRICK, m::KNOCK_OFF])]);
    let p2 = Team::new(vec![set(s::SNORLAX, a::THICK_FAT, Some(i::LEFTOVERS), &[m::REST])]);
    let mut turn = 0;
    scripted(
        &format,
        [p1, p2],
        4_260_003,
        |b, side| (side == SideId::P1).then(|| use_move(if b.turn() == 1 { 0 } else { 1 })),
        |b, t| {
            turn = b.turn();
            let (g, x) = (t.side(SideId::P1).mon(0).unwrap(), t.side(SideId::P2).mon(0).unwrap());
            if turn == 2 {
                assert_eq!(g.item, ItemState::Known(i::LEFTOVERS));
                assert_eq!(x.item, ItemState::Known(i::CHOICE_SCARF));
                // Trace copied Thick Fat when it came in.
                assert_eq!(g.base_ability, AbilityState::Known(a::TRACE));
                assert_eq!(x.base_ability, AbilityState::Known(a::THICK_FAT));
            }
        },
    );
    assert!(turn >= 2);
}

#[test]
fn mega_evolution_shows_stone_and_ability() {
    let format = Format::champions_vgc();
    let charizard = set(s::CHARIZARD, a::BLAZE, Some(i::CHARIZARDITE_Y), &[m::HEAT_WAVE, m::PROTECT]);
    let p1 = team(vec![charizard], 6, &format);
    let p2 = team(vec![], 6, &format);
    let mut mega = false;
    let mut policy_turn = 0;
    scripted(
        &format,
        [p1, p2],
        4_260_004,
        |b, side| {
            if b.turn() == 0 {
                return Some(Choice::team(&[0, 1, 2, 3]));
            }
            policy_turn = b.turn();
            (side == SideId::P1 && b.turn() == 1).then(|| {
                Choice::doubles(
                    SlotAction::Move { index: 0, target: None, gimmick: Some(Gimmick::MegaEvolve) },
                    SlotAction::Move { index: 0, target: Some(Target::Foe(0)), gimmick: None },
                )
            })
        },
        |_, t| {
            let c = t.side(SideId::P1).mon(0).unwrap();
            if c.flags.has(MonFlags::MEGA) {
                mega = true;
                assert!(t.side(SideId::P1).mega_used);
                assert_eq!(c.item, ItemState::Known(i::CHARIZARDITE_Y));
                assert_eq!(c.details_species, s::CHARIZARD_MEGA_Y);
                assert_eq!(c.base_ability, AbilityState::Known(a::DROUGHT));
            }
        },
    );
    assert!(mega, "Charizard Mega Evolved");
}

fn doubles_move(a: u8, a_target: Option<Target>, b: u8, b_target: Option<Target>, gimmick: Option<Gimmick>) -> Choice {
    Choice::doubles(
        SlotAction::Move { index: a, target: a_target, gimmick },
        SlotAction::Move { index: b, target: b_target, gimmick: None },
    )
}

#[test]
fn tera_formes_show_their_species_and_abilities() {
    let format = Format::gen9_vgc();
    let ogerpon =
        set(s::OGERPON_WELLSPRING, a::WATER_ABSORB, Some(i::WELLSPRING_MASK), &[m::IVY_CUDGEL, m::SPIKY_SHIELD])
            .with_tera_type(Type::Water);
    let terapagos = set(s::TERAPAGOS, a::TERA_SHIFT, Some(i::LEFTOVERS), &[m::TERA_STARSTORM, m::PROTECT])
        .with_tera_type(Type::Stellar);
    let p1 = team(vec![ogerpon, terapagos], 6, &format);
    let p2 = team(vec![], 6, &format);
    let (mut ogerpon_tera, mut terastal) = (false, false);
    scripted(
        &format,
        [p1, p2],
        4_260_010,
        |b, side| {
            if b.turn() == 0 {
                return Some(Choice::team(&[0, 1, 2, 3]));
            }
            (side == SideId::P1 && b.turn() == 1)
                .then(|| doubles_move(0, Some(Target::Foe(0)), 1, None, Some(Gimmick::Terastallize)))
        },
        |_, t| {
            let side = t.side(SideId::P1);
            let (o, p) = (side.mon(0).unwrap(), side.mon(1).unwrap());
            if p.is_revealed() && p.details_species == s::TERAPAGOS_TERASTAL {
                // Tera Shift's forme has only Tera Shell.
                terastal = true;
                assert_eq!(p.base_ability, AbilityState::Known(a::TERA_SHELL));
            }
            if o.flags.has(MonFlags::TERASTALLIZED) && !o.is_fainted() {
                ogerpon_tera = true;
                assert_eq!(o.details_species, s::OGERPON_WELLSPRING_TERA);
                assert_eq!(o.tera_type, Some(Type::Water));
                assert_eq!(o.ability, AbilityState::Known(a::EMBODY_ASPECT_WELLSPRING));
                assert!(side.tera_used);
            }
        },
    );
    assert!(ogerpon_tera && terastal);
}

#[test]
fn commander_links_tatsugiri_and_dondozo() {
    let format = Format::gen9_doubles();
    let p1 = Team::new(vec![
        set(s::TATSUGIRI, a::COMMANDER, None, &[m::DRACO_METEOR]),
        set(s::DONDOZO, a::UNAWARE, None, &[m::WAVE_CRASH, m::ORDER_UP]),
        set(s::SNORLAX, a::THICK_FAT, None, &[m::BODY_SLAM]),
    ]);
    let p2 = Team::new(vec![
        set(s::GARCHOMP, a::ROUGH_SKIN, None, &[m::DRAGON_CLAW]),
        set(s::INCINEROAR, a::INTIMIDATE, None, &[m::FLARE_BLITZ]),
    ]);
    let mut commanding = false;
    scripted(
        &format,
        [p1, p2],
        4_260_011,
        |_, _| None,
        |b, t| {
            let s1 = t.side(SideId::P1);
            let tatsugiri = b.state().side(SideId::P1).team.iter().find(|m| m.id == 0).unwrap();
            let up = tatsugiri.volatiles.has(pikapro::state::Volatiles::COMMANDING) && !tatsugiri.is_fainted();
            if up && b.turn() >= 1 {
                commanding = true;
                assert!(s1.slots[0].volatiles.has(PublicVolatiles::COMMANDING));
                assert!(s1.slots[1].volatiles.has(PublicVolatiles::COMMANDED));
                assert_eq!(s1.mon(0).unwrap().ability, AbilityState::Known(a::COMMANDER));
            }
        },
    );
    assert!(commanding);
}

#[test]
fn revival_blessing_takes_a_faint_back() {
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![
        set(s::PIKACHU, a::STATIC, None, &[m::THUNDERBOLT]),
        set(s::PAWMOT, a::VOLT_ABSORB, None, &[m::REVIVAL_BLESSING]),
    ])
    .clone();
    let mut p1 = p1;
    p1.sets[0] = p1.sets[0].clone().with_level(1);
    let p2 = Team::new(vec![set(s::GARCHOMP, a::ROUGH_SKIN, None, &[m::EARTHQUAKE])]);
    let (mut fainted, mut revived) = (false, false);
    scripted(
        &format,
        [p1, p2],
        4_260_012,
        |b, side| {
            (side == SideId::P1 && b.state().side(SideId::P1).active(0).is_some_and(|m| m.id == 1)).then(|| use_move(0))
        },
        |_, t| {
            let side = t.side(SideId::P1);
            let pikachu = side.mon(0).unwrap();
            fainted |= pikachu.is_fainted();
            if fainted && !pikachu.is_fainted() {
                revived = true;
                assert_eq!(side.fainted, side.mons.iter().filter(|m| m.is_fainted()).count() as u8);
            }
        },
    );
    assert!(fainted && revived);
}

#[test]
fn sleep_talk_counts_the_turns_asleep() {
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![set(s::SNORLAX, a::THICK_FAT, None, &[m::REST, m::SLEEP_TALK, m::BODY_SLAM])]);
    let p2 = Team::new(vec![set(s::GARCHOMP, a::ROUGH_SKIN, None, &[m::DRAGON_CLAW])]);
    let mut asleep_turns = 0;
    scripted(
        &format,
        [p1, p2],
        4_260_013,
        |b, side| (side == SideId::P1).then(|| use_move(if b.turn() == 1 { 0 } else { 1 })),
        |_, t| {
            let x = t.side(SideId::P1).mon(0).unwrap();
            if x.status == StatusKind::Sleep {
                asleep_turns = asleep_turns.max(x.status_turns);
                // D: Sleep Talk calls one of its own moves: revealed, but
                // not used from its slot.
                if let Some(r) = x.revealed(m::BODY_SLAM) {
                    assert_eq!(r.uses, 0, "Sleep Talk's call is not a use");
                }
            }
        },
    );
    assert!(asleep_turns >= 1);
}

/// Regenerator heals a Pokémon that switches out without a line (Scarlet
/// and Violet): the view keeps the HP last shown.
#[test]
fn regenerator_does_not_leak() {
    let format = Format::gen9_singles();
    let p2 = Team::new(vec![
        set(s::AMOONGUSS, a::REGENERATOR, None, &[m::GIGA_DRAIN]),
        set(s::SNORLAX, a::THICK_FAT, None, &[m::BODY_SLAM]),
    ]);
    let p1 = Team::new(vec![set(s::CHARIZARD, a::BLAZE, None, &[m::FLAMETHROWER])]);
    let mut checked = false;
    scripted(
        &format,
        [p1, p2],
        4_260_014,
        |b, side| {
            let amoonguss_in = b.state().side(SideId::P2).active(0).is_some_and(|m| m.id == 0);
            match side {
                SideId::P2 if b.turn() == 2 && amoonguss_in => Some(Choice::single(SlotAction::Switch(1))),
                SideId::P2 => Some(use_move(0)),
                _ => Some(use_move(0)),
            }
        },
        |b, t| {
            let side = b.state().side(SideId::P2);
            let amoonguss = side.team.iter().find(|m| m.id == 0).unwrap();
            let view = t.side(SideId::P2).mon(0).unwrap();
            if !view.is_active() && view.is_revealed() && !amoonguss.is_fainted() && amoonguss.hp < amoonguss.max_hp() {
                let (pct, _) = pikapro_protocol::hp_shown(format.mechanics, amoonguss.hp, amoonguss.max_hp());
                checked |= view.hp.hp < pct as u16;
            }
        },
    );
    assert!(checked, "Amoonguss switched out hurt and healed unseen");
}

/// A foe's Shadow Tag the player can't see: the request allows the switch,
/// the battle refuses it, and the overlay learns it as Showdown's updated
/// request says.
#[test]
fn a_hidden_trap_is_learnt_from_the_refusal() {
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![
        set(s::PIKACHU, a::STATIC, None, &[m::THUNDERBOLT]),
        set(s::SNORLAX, a::THICK_FAT, None, &[m::BODY_SLAM]),
    ]);
    let p2 = Team::new(vec![set(s::GOTHITELLE, a::SHADOW_TAG, None, &[m::PSYCHIC])]);
    let mut b =
        Battle::new(format.clone(), [p1, p2], pikapro::BattleOptions::new(pikapro::Seed::fast(4_260_015))).unwrap();
    let mut t = PublicTracker::for_format(&format);
    t.apply_all(b.drain_events().as_slice());
    let mut own = OwnOverlay::from_battle(&b, SideId::P1);
    let a = own.active[0].unwrap();
    assert!(a.maybe_trapped && !a.trapped, "{a:?}");
    let switch = Choice::single(SlotAction::Switch(1));
    assert!(!b.legal_actions(SideId::P1).is_legal(&switch));
    assert_eq!(b.submit(SideId::P1, switch), Err(pikapro::ChoiceError::IllegalAction { slot: 0 }));
    let before = own;
    assert!(own.reject(&b, 0, Unavailable::Switch));
    let a = own.active[0].unwrap();
    assert!(a.trapped && !a.maybe_trapped && own.known_trapped[0] && own.updated);
    // A session learns the same from the updated request's text.
    let mut update = before;
    update.active[0] = own.active[0];
    let refused = [SlotAction::Switch(1), SlotAction::Pass];
    assert!(update.learn_update(&before, &refused, false));
    assert_eq!(update, own);
    // The next request forgets it.
    b.submit(SideId::P1, use_move(0)).unwrap();
    b.submit(SideId::P2, use_move(0)).unwrap();
    t.apply_all(b.drain_events().as_slice());
    own = OwnOverlay::from_battle(&b, SideId::P1);
    assert!(!own.known_trapped[0]);
    // A second refusal of the same thing changes nothing: Showdown's
    // `[Invalid choice]`.
    let mut again = own;
    again.reject(&b, 0, Unavailable::Switch);
    assert!(!again.reject(&b, 0, Unavailable::Switch));
}

/// Imprison hides which moves it disables: the request shows the move, the
/// battle refuses it, and the overlay disables it as Showdown's update
/// does.
#[test]
fn an_imprisoned_move_is_learnt_from_the_refusal() {
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![set(s::PIKACHU, a::STATIC, None, &[m::PROTECT, m::THUNDERBOLT])]);
    let p2 = Team::new(vec![set(s::GENGAR, a::CURSED_BODY, None, &[m::IMPRISON, m::PROTECT])]);
    let mut b =
        Battle::new(format.clone(), [p1, p2], pikapro::BattleOptions::new(pikapro::Seed::fast(4_260_016))).unwrap();
    let mut t = PublicTracker::for_format(&format);
    b.submit(SideId::P1, use_move(1)).unwrap();
    b.submit(SideId::P2, use_move(0)).unwrap();
    t.apply_all(b.drain_events().as_slice());
    let mut own = OwnOverlay::from_battle(&b, SideId::P1);
    let a = own.active[0].unwrap();
    assert!(a.maybe_disabled, "{a:?}");
    let OwnMoves::Moves(ms) = a.moves else { panic!("{a:?}") };
    assert!(!ms[0].disabled, "the request hides Imprison's disable");
    assert!(!b.legal_actions(SideId::P1).is_legal(&use_move(0)));
    let before = own;
    assert!(own.reject(&b, 0, Unavailable::Move { index: 0 }));
    let OwnMoves::Moves(ms) = own.active[0].unwrap().moves else { unreachable!() };
    assert!(ms[0].disabled && own.known_disabled[0] == 1);
    assert!(!ms[1].disabled);
    // A session learns the same from the updated request's text.
    let mut update = before;
    update.active[0] = own.active[0];
    let refused = [SlotAction::Move { index: 0, target: None, gimmick: None }, SlotAction::Pass];
    assert!(update.learn_update(&before, &refused, false));
    assert_eq!(update, own);
    let _ = t;
}

fn switch_to(position: u8) -> Choice {
    Choice::single(SlotAction::Switch(position))
}

/// An Illusion disguised as the Pokémon it replaces (Illusion copies the
/// last party member, which the switch made the one leaving): that one
/// left the field, and what it had only while out is gone. When the Illusion breaks, its record is benched and marked
/// a disguise; `check_sound` checks `ACTIVE` against the slots all along.
#[test]
fn an_illusion_of_the_pokemon_it_replaces_benches_it() {
    let format = Format::gen9_singles();
    let raichu = set(s::RAICHU, a::STATIC, None, &[m::THUNDERBOLT]);
    let zoroark = set(s::ZOROARK, a::ILLUSION, None, &[m::NIGHT_DAZE]);
    let p1 = Team::new(vec![raichu, zoroark]);
    let p2 = Team::new(vec![set(s::GARCHOMP, a::ROUGH_SKIN, None, &[m::WORRY_SEED, m::DRAGON_CLAW])]);
    let mut broke = false;
    scripted(
        &format,
        [p1, p2],
        8_400_101,
        |b, side| match (b.turn(), side) {
            (1, _) => Some(use_move(0)),
            (2, SideId::P1) => Some(switch_to(1)),
            (_, SideId::P2) => Some(use_move(1)),
            _ => None,
        },
        |b, t| {
            let side = t.side(SideId::P1);
            if b.turn() == 2 {
                assert_eq!(side.mons[0].ability, AbilityState::Known(a::INSOMNIA), "Worry Seed");
            }
            let zoroark = b.state().side(SideId::P1).active(0).filter(|p| p.id == 1);
            if b.turn() == 3 && zoroark.is_some_and(|z| z.illusion.is_none()) {
                broke = true;
                let raichu = &side.mons[0];
                assert!(!raichu.is_active(), "{:?}", raichu.flags);
                assert!(raichu.flags.has(MonFlags::WAS_DISGUISE));
                assert_ne!(raichu.ability, AbilityState::Known(a::INSOMNIA), "left the field");
                assert_eq!(side.slots[0].row, Some(1));
                assert!(side.mons[1].is_active());
            }
        },
    );
    assert!(broke, "the Illusion broke on turn 2");
}

/// Without team preview a disguise of a Pokémon not seen yet makes its
/// record; when the Illusion breaks, the record keeps who it showed (a
/// party member), benched and unrevealed, never blank.
#[test]
fn a_disguise_first_seen_keeps_its_identity() {
    let format = Format::gen9_singles();
    let zoroark = set(s::ZOROARK, a::ILLUSION, None, &[m::NIGHT_DAZE]);
    let p1 = Team::new(vec![zoroark, set(s::PIKACHU, a::STATIC, None, &[m::THUNDERBOLT])]);
    let p2 = Team::new(vec![set(s::GARCHOMP, a::ROUGH_SKIN, None, &[m::DRAGON_CLAW])]);
    let mut broke = false;
    scripted(
        &format,
        [p1, p2],
        8_400_102,
        |_, _| Some(use_move(0)),
        |b, t| {
            let zoroark = b.state().side(SideId::P1).active(0).filter(|p| p.id == 0);
            if zoroark.is_some_and(|z| z.illusion.is_none()) && !broke {
                broke = true;
                let pikachu = &t.side(SideId::P1).mons[0];
                assert_eq!((pikachu.species, pikachu.level), (s::PIKACHU, 50));
                assert!(!pikachu.is_revealed() && !pikachu.is_active());
                assert!(pikachu.flags.has(MonFlags::WAS_DISGUISE));
                assert_eq!(pikachu.base_ability, AbilityState::Unknown, "Pikachu has two abilities");
            }
        },
    );
    assert!(broke, "the Illusion broke");
}

/// A Protect Instructed after one that worked fails on `willAct` when
/// nothing acts after it: Showdown's `stall` stays (`duration: 2`), so the
/// chain goes on into the next turn.
/// `check_sound` compares the chain with the engine's at every decision.
#[test]
fn an_instructed_protect_keeps_the_chain() {
    let format = Format::gen9_doubles();
    let p1 = Team::new(vec![
        set(s::SNORLAX, a::THICK_FAT, None, &[m::PROTECT, m::BODY_SLAM]),
        set(s::JOLTEON, a::VOLT_ABSORB, None, &[m::THUNDERBOLT]),
    ]);
    let p2 = Team::new(vec![
        set(s::STAKATAKA, a::BEAST_BOOST, None, &[m::INSTRUCT]),
        set(s::ELECTRODE, a::STATIC, None, &[m::THUNDERBOLT]),
    ]);
    let mut kept = false;
    scripted(
        &format,
        [p1, p2],
        8_400_103,
        |b, side| match (b.turn(), side) {
            (1, SideId::P1) => Some(doubles_move(0, None, 0, Some(Target::Foe(1)), None)),
            (1, SideId::P2) => Some(doubles_move(0, Some(Target::Foe(0)), 0, Some(Target::Foe(1)), None)),
            _ => None,
        },
        |b, t| {
            if b.turn() == 2 {
                kept = true;
                let snorlax = &t.side(SideId::P1).slots[0];
                assert_eq!(snorlax.counters.protect_chain, 1, "the stall of turn 1 lasts");
            }
        },
    );
    assert!(kept);
}
