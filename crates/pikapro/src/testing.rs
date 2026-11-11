//! Helpers for tests, examples and benchmarks. Not part of the simulation API.

use pikapro_data::gen9::{abilities as a, items as i, moves as m, species as s};
use pikapro_data::{Nature, Stats, Type};

use crate::format::{Format, Mechanics, StatSystem};
use crate::team::{Investment, PokemonSet, Team};
use crate::{Battle, Position, SideId};

/// The Speed of the Pokémon in `side`'s active `slot` now, as Showdown's
/// `getStat('spe')` gives it (its stage and `ModifySpe` handlers, at most
/// 10000; before Trick Room's inversion), computed on a copy of `battle`
/// (a handler sort may draw). `None` for an empty slot or a fainted
/// Pokémon. For tests of what an observer infers about Speed.
pub fn active_speed(battle: &Battle, side: SideId, slot: usize) -> Option<u32> {
    let pos = Position::new(side, u8::try_from(slot).ok()?);
    if battle.state.at(pos).is_none_or(|m| m.is_fainted()) {
        return None;
    }
    let mut copy = battle.clone();
    Some(crate::resolve::events::get_stat(&mut copy, pos, pikapro_data::Stat::Spe, false, false))
}

/// A fixed six-Pokémon team for `format` that its own validation accepts
/// (`pikapro-validate`'s `validate_preset`, and Showdown's
/// `validate-team`):
///
/// * Champions (`championsvgc`, `championsbss`): legal in VGC 2026 Reg M-C
///   and BSS Reg M-C, with two Mega Stone holders (Charizard, Garchomp) and
///   Stat Points;
/// * Scarlet/Violet (`gen9vgc`, `gen9singles`, `gen9doubles`): legal in
///   VGC 2025 Reg I and in Anything Goes with Species Clause (the custom
///   games' rules), with a Tera Type each and EVs;
/// * `gen9ou` (by the format's id): legal in `[Gen 9] OU`, at level 100.
/// * the random battles: a sample team to play with, like the custom games'
///   (Showdown draws a random battle's teams for the players).
///
/// Every other set is level 50, the level VGC-style formats battle at. In
/// Anything Goes (level 100 allowed) Showdown flags a level 50 set whose
/// EVs sum to a multiple of 4 as a likely mistake, so one EV in each Gen 9
/// spread is odd (5 or 1), which changes no stat.
pub fn sample_team(format: &Format) -> Team {
    if pikapro_data::id_eq(format.id, "gen9ou") {
        return ou_sample_team();
    }
    let invest = |evs: [u8; 6], sp: [u8; 6]| match format.stat_system {
        StatSystem::EffortValues => {
            Investment::Evs { evs: Stats::new(evs[0], evs[1], evs[2], evs[3], evs[4], evs[5]), ivs: Stats::splat(31) }
        }
        StatSystem::StatPoints => Investment::StatPoints(Stats::new(sp[0], sp[1], sp[2], sp[3], sp[4], sp[5])),
    };
    let set = |species, ability, item, nature, evs, sp, moves: &[_]| {
        PokemonSet::new(species)
            .with_level(50)
            .with_ability(ability)
            .with_item(item)
            .with_nature(nature)
            .with_investment(invest(evs, sp))
            .with_moves(moves)
    };
    let sets = match format.mechanics {
        Mechanics::Champions => vec![
            set(
                s::INCINEROAR,
                a::INTIMIDATE,
                i::SITRUS_BERRY,
                Nature::Careful,
                [0; 6],
                [32, 2, 0, 0, 32, 0],
                &[m::FAKE_OUT, m::FLARE_BLITZ, m::PARTING_SHOT, m::PROTECT],
            ),
            set(
                s::CHARIZARD,
                a::BLAZE,
                i::CHARIZARDITE_Y,
                Nature::Timid,
                [0; 6],
                [2, 0, 0, 32, 0, 32],
                &[m::HEAT_WAVE, m::AIR_SLASH, m::SOLAR_BEAM, m::PROTECT],
            ),
            set(
                s::GARCHOMP,
                a::ROUGH_SKIN,
                i::GARCHOMPITE,
                Nature::Jolly,
                [0; 6],
                [2, 32, 0, 0, 0, 32],
                &[m::EARTHQUAKE, m::DRAGON_CLAW, m::ROCK_SLIDE, m::PROTECT],
            ),
            set(
                s::GHOLDENGO,
                a::GOOD_AS_GOLD,
                i::LIFE_ORB,
                Nature::Modest,
                [0; 6],
                [32, 0, 0, 32, 2, 0],
                &[m::MAKE_IT_RAIN, m::SHADOW_BALL, m::NASTY_PLOT, m::PROTECT],
            ),
            set(
                s::RILLABOOM,
                a::GRASSY_SURGE,
                i::MIRACLE_SEED,
                Nature::Adamant,
                [0; 6],
                [32, 32, 0, 0, 2, 0],
                &[m::FAKE_OUT, m::GRASSY_GLIDE, m::WOOD_HAMMER, m::PROTECT],
            ),
            set(
                s::SINISTCHA,
                a::HOSPITALITY,
                i::LEFTOVERS,
                Nature::Calm,
                [0; 6],
                [32, 0, 2, 0, 32, 0],
                &[m::MATCHA_GOTCHA, m::RAGE_POWDER, m::STRENGTH_SAP, m::PROTECT],
            ),
        ],
        Mechanics::ScarletViolet => vec![
            set(
                s::INCINEROAR,
                a::INTIMIDATE,
                i::SITRUS_BERRY,
                Nature::Careful,
                [252, 5, 0, 0, 252, 0],
                [0; 6],
                &[m::KNOCK_OFF, m::FLAMETHROWER, m::PROTECT, m::HEAT_WAVE],
            )
            .with_tera_type(Type::Ghost),
            set(
                s::RILLABOOM,
                a::GRASSY_SURGE,
                i::ASSAULT_VEST,
                Nature::Adamant,
                [252, 252, 0, 0, 5, 0],
                [0; 6],
                &[m::FAKE_OUT, m::GRASSY_GLIDE, m::KNOCK_OFF, m::U_TURN],
            )
            .with_tera_type(Type::Fire),
            set(
                s::FLUTTER_MANE,
                a::PROTOSYNTHESIS,
                i::BOOSTER_ENERGY,
                Nature::Timid,
                [5, 0, 0, 252, 0, 252],
                [0; 6],
                &[m::MOONBLAST, m::SHADOW_BALL, m::DAZZLING_GLEAM, m::PROTECT],
            )
            .with_tera_type(Type::Fairy),
            set(
                s::GARCHOMP,
                a::ROUGH_SKIN,
                i::CHOICE_SCARF,
                Nature::Jolly,
                [5, 252, 0, 0, 0, 252],
                [0; 6],
                &[m::EARTHQUAKE, m::DRAGON_CLAW, m::ROCK_SLIDE],
            )
            .with_tera_type(Type::Steel),
            set(
                s::GHOLDENGO,
                a::GOOD_AS_GOLD,
                i::LIFE_ORB,
                Nature::Modest,
                [252, 0, 0, 252, 5, 0],
                [0; 6],
                &[m::FLASH_CANNON, m::SHADOW_BALL, m::PROTECT],
            )
            .with_tera_type(Type::Steel),
            set(
                s::AMOONGUSS,
                a::REGENERATOR,
                i::ROCKY_HELMET,
                Nature::Calm,
                [252, 0, 5, 0, 252, 0],
                [0; 6],
                &[m::SLUDGE_BOMB, m::ENERGY_BALL, m::PROTECT],
            )
            .with_tera_type(Type::Water),
        ],
    };
    Team::new(sets)
}

/// [`sample_team`] for `gen9ou`: six Pokémon of the OU tier at level 100.
fn ou_sample_team() -> Team {
    let set = |species, ability, item, nature, evs: [u8; 6], attack_iv: u8, tera, moves: &[_]| {
        let evs = Stats::new(evs[0], evs[1], evs[2], evs[3], evs[4], evs[5]);
        let mut ivs = Stats::splat(31);
        ivs[pikapro_data::Stat::Atk] = attack_iv;
        PokemonSet::new(species)
            .with_ability(ability)
            .with_item(item)
            .with_nature(nature)
            .with_investment(Investment::Evs { evs, ivs })
            .with_tera_type(tera)
            .with_moves(moves)
    };
    Team::new(vec![
        set(
            s::GARCHOMP,
            a::ROUGH_SKIN,
            i::ROCKY_HELMET,
            Nature::Jolly,
            [252, 4, 0, 0, 0, 252],
            31,
            Type::Steel,
            &[m::EARTHQUAKE, m::STEALTH_ROCK, m::DRAGON_TAIL, m::SPIKES],
        ),
        set(
            s::GHOLDENGO,
            a::GOOD_AS_GOLD,
            i::AIR_BALLOON,
            Nature::Timid,
            [0, 0, 0, 252, 4, 252],
            0,
            Type::Fairy,
            &[m::MAKE_IT_RAIN, m::SHADOW_BALL, m::NASTY_PLOT, m::RECOVER],
        ),
        set(
            s::GREAT_TUSK,
            a::PROTOSYNTHESIS,
            i::BOOSTER_ENERGY,
            Nature::Jolly,
            [0, 252, 4, 0, 0, 252],
            31,
            Type::Ice,
            &[m::HEADLONG_RUSH, m::ICE_SPINNER, m::RAPID_SPIN, m::CLOSE_COMBAT],
        ),
        set(
            s::KINGAMBIT,
            a::SUPREME_OVERLORD,
            i::LEFTOVERS,
            Nature::Adamant,
            [252, 252, 0, 0, 4, 0],
            31,
            Type::Dark,
            &[m::KOWTOW_CLEAVE, m::SUCKER_PUNCH, m::SWORDS_DANCE, m::IRON_HEAD],
        ),
        set(
            s::DRAGAPULT,
            a::INFILTRATOR,
            i::CHOICE_SPECS,
            Nature::Timid,
            [0, 0, 0, 252, 4, 252],
            31,
            Type::Ghost,
            &[m::DRACO_METEOR, m::SHADOW_BALL, m::U_TURN, m::FLAMETHROWER],
        ),
        set(
            s::TOXAPEX,
            a::REGENERATOR,
            i::BLACK_SLUDGE,
            Nature::Bold,
            [252, 0, 252, 0, 4, 0],
            0,
            Type::Fairy,
            &[m::TOXIC, m::RECOVER, m::HAZE, m::SURF],
        ),
    ])
}
