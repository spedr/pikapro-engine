//! `typing` against the engine: in random battles, every active Pokémon's
//! moves get the type from [`typing::move_type`] (with the facts the
//! engine reads, [`power::move_user`]) that the move pipeline gives them.

use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId, Type};

use super::{events, moves, power};
use crate::battle::Battle;
use crate::state::{Position, SideId};
use crate::team::{PokemonSet, Team};
use crate::{BattleOptions, BattleRng, Format, Seed, typing};

fn set(species: SpeciesId, ability: AbilityId, item: Option<ItemId>, tera: Type, moves: &[MoveId]) -> PokemonSet {
    let s = PokemonSet::new(species).with_level(50).with_ability(ability).with_tera_type(tera).with_moves(moves);
    match item {
        Some(i) => s.with_item(i),
        None => s,
    }
}

/// Teams full of moves whose type depends on the user and the field.
fn teams() -> [Team; 2] {
    [
        Team::new(vec![
            set(
                s::CASTFORM,
                a::FORECAST,
                Some(i::LEFTOVERS),
                Type::Fire,
                &[m::WEATHER_BALL, m::SUNNY_DAY, m::RAIN_DANCE, m::TERA_BLAST],
            ),
            set(
                s::SYLVEON,
                a::PIXILATE,
                Some(i::UTILITY_UMBRELLA),
                Type::Fire,
                &[m::HYPER_VOICE, m::TERA_BLAST, m::WEATHER_BALL, m::SNOWSCAPE],
            ),
            set(
                s::ARCEUS_FIRE,
                a::MULTITYPE,
                Some(i::FLAME_PLATE),
                Type::Water,
                &[m::JUDGMENT, m::ELECTRIFY, m::TERRAIN_PULSE, m::SANDSTORM],
            ),
            set(
                s::ORICORIO_POM_POM,
                a::DANCER,
                None,
                Type::Grass,
                &[m::REVELATION_DANCE, m::ELECTRIFY, m::ELECTRIC_TERRAIN, m::TERA_BLAST],
            ),
            set(
                s::TAUROS_PALDEA_BLAZE,
                a::INTIMIDATE,
                Some(i::AIR_BALLOON),
                Type::Fire,
                &[m::RAGING_BULL, m::TERRAIN_PULSE, m::MISTY_TERRAIN, m::FLYING_PRESS],
            ),
            set(
                s::OGERPON_CORNERSTONE,
                a::STURDY,
                Some(i::CORNERSTONE_MASK),
                Type::Rock,
                &[m::IVY_CUDGEL, m::HYPER_VOICE, m::TERA_BLAST, m::FREEZE_DRY],
            ),
        ]),
        Team::new(vec![
            set(
                s::MORPEKO,
                a::HUNGER_SWITCH,
                None,
                Type::Dark,
                &[m::AURA_WHEEL, m::ELECTRIFY, m::TERRAIN_PULSE, m::PSYCHIC_TERRAIN],
            ),
            set(
                s::TERAPAGOS,
                a::TERA_SHIFT,
                None,
                Type::Stellar,
                &[m::TERA_STARSTORM, m::TERA_BLAST, m::WEATHER_BALL, m::GRASSY_TERRAIN],
            ),
            set(
                s::DELCATTY,
                a::NORMALIZE,
                Some(i::IRON_BALL),
                Type::Normal,
                &[m::THUNDERBOLT, m::WEATHER_BALL, m::HYPER_VOICE, m::TERRAIN_PULSE],
            ),
            set(
                s::PRIMARINA,
                a::LIQUID_VOICE,
                Some(i::UTILITY_UMBRELLA),
                Type::Water,
                &[m::HYPER_VOICE, m::SPARKLING_ARIA, m::WEATHER_BALL, m::GRAVITY],
            ),
            set(
                s::BRONZONG,
                a::LEVITATE,
                None,
                Type::Steel,
                &[m::TERRAIN_PULSE, m::GRAVITY, m::WEATHER_BALL, m::ELECTRIC_TERRAIN],
            ),
            set(
                s::MAGNEZONE,
                a::ANALYTIC,
                None,
                Type::Flying,
                &[m::MAGNET_RISE, m::TERRAIN_PULSE, m::TERA_BLAST, m::REVELATION_DANCE],
            ),
        ]),
    ]
}

/// The type the engine's pipeline gives `move_id` used by the Pokémon at
/// `pos` (on a copy of the battle): the early `ModifyType` (Raging Bull),
/// the move's own `onModifyType`, then `runEvent('ModifyType')`.
fn engine_type(battle: &Battle, pos: Position, move_id: MoveId) -> Type {
    let mut b = battle.clone();
    let foe = Some(Position::new(pos.side.foe(), 0));
    b.active_move = Some((pos, move_id));
    let mut data = moves::typed_data(&b, pos, move_id);
    b.move_state = events::MoveState { active_target: foe, ..Default::default() };
    power::modify_move(&mut b, pos, move_id, &mut data, foe);
    moves::modify_type_event(&mut b, pos, foe, move_id, &mut data);
    data.ty
}

#[test]
fn move_types_match_the_pipeline() {
    let format = Format::gen9_doubles();
    let mut checked = 0;
    let mut changed = 0;
    for seed in 6_500_000..6_500_040u64 {
        let mut b = Battle::new(format.clone(), teams(), BattleOptions::new(Seed::fast(seed))).unwrap();
        let mut rng = BattleRng::new(Seed::fast(!seed));
        let mut decisions = 0;
        while !b.is_over() && decisions < 400 {
            for side in SideId::BOTH {
                for slot in 0..format.active_per_side() {
                    let Some(mon) = b.state().side(side).active(slot).copied() else { continue };
                    if mon.is_fainted() {
                        continue;
                    }
                    let pos = Position::new(side, slot as u8);
                    for mv in mon.moves.iter().map(|x| x.id) {
                        let want = engine_type(&b, pos, mv);
                        let mut facts_battle = b.clone();
                        facts_battle.active_move = Some((pos, mv));
                        let facts = power::move_user(&facts_battle, pos, mv);
                        let got = typing::move_type(format.dex, mv, &facts);
                        assert_eq!(got, Some(want), "seed {seed}: {:?} {:?} with {facts:?}", mon.species, mv);
                        checked += 1;
                        changed += usize::from(want != format.dex.move_(mv).ty);
                    }
                }
            }
            for side in SideId::BOTH {
                let legal = b.legal_actions(side);
                if legal.is_waiting() {
                    continue;
                }
                let choice = legal.sample(&mut rng);
                b.submit(side, choice).expect("a legal choice");
                decisions += 1;
            }
        }
    }
    assert!(checked > 5_000 && changed > 500, "checked {checked}, of which {changed} changed type");
}
