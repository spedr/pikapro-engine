//! EVs, IVs, Stat Points and natures.

use pikapro::{BattleRng, Investment};
use pikapro_data::gen9::moves;
use pikapro_data::{Dex, MoveCategory, Nature, Stat, Stats};

use crate::sets::SetData;

/// A uniform-mode spread with 31 IVs: the stats in a random order, each
/// given a uniform share of what is left (EVs in steps of 4 up to 252 and
/// 508 in total, or Stat Points up to 32 and 66), never all zero.
pub(crate) fn uniform(rng: &mut BattleRng, stat_points: bool) -> Investment {
    let (cap, total, step) = if stat_points { (32, 66, 1) } else { (63, 127, 4) };
    let mut order = Stat::ALL;
    rng.shuffle(&mut order);
    let mut s = Stats::splat(0u8);
    let mut left = total;
    for stat in order {
        let v = rng.random(left.min(cap) + 1);
        s[stat] = (v * step) as u8;
        left -= v;
    }
    if s.0.iter().all(|&v| v == 0) {
        s[order[0]] = step as u8;
    }
    if stat_points { Investment::StatPoints(s) } else { Investment::Evs { evs: s, ivs: Stats::splat(31) } }
}

/// Uniform-mode IVs: each 31 three times in four, else uniform in 0-30.
pub(crate) fn uniform_ivs(rng: &mut BattleRng) -> Stats<u8> {
    let mut ivs = Stats::splat(31u8);
    for v in &mut ivs.0 {
        if rng.random(4) == 0 {
            *v = rng.random(31) as u8;
        }
    }
    ivs
}

/// `investment` with every IV at 31.
pub(crate) fn max_ivs(investment: Investment) -> Investment {
    match investment {
        Investment::Evs { evs, .. } => Investment::Evs { evs, ivs: Stats::splat(31) },
        sp => sp,
    }
}

fn investment(stat_points: bool, evs: [u16; 6], ivs: Stats<u8>) -> Investment {
    if stat_points {
        // 252 EVs are 32 Stat Points, 128 are 17, 4 are 2 (66 in total);
        // Showdown's 85 are 11.
        let sp = evs.map(|e| match e {
            0 => 0,
            1..=8 => 2,
            128 => 17,
            85 => 11,
            _ => 32,
        });
        Investment::StatPoints(Stats(sp))
    } else {
        Investment::Evs { evs: Stats(evs.map(|e| e as u8)), ivs }
    }
}

/// Showdown's random-battle spread (`randomSet`): 85 EVs or 11 Stat Points
/// everywhere, none in Attack or Speed where the generator zeroed them, a
/// neutral nature.
pub(crate) fn showdown(set: &SetData, stat_points: bool) -> (Nature, Investment) {
    let mut evs = [85u16; 6];
    let mut ivs = Stats::splat(31u8);
    if set.no_attack {
        evs[Stat::Atk as usize] = 0;
        ivs[Stat::Atk] = 0;
    }
    if set.no_speed {
        evs[Stat::Spe as usize] = 0;
        ivs[Stat::Spe] = 0;
    }
    (Nature::Serious, investment(stat_points, evs, ivs))
}

/// A competitive-style spread for a set, by its role and moves.
///
/// - Attackers (every role but supports): 252 in the attacking stat its
///   moves use most (Body Press and Foul Play don't count; ties go to the
///   higher base stat), 252 Speed and 4 HP, or 252 HP and 4 Defense under
///   Trick Room; bulky roles (`Bulky ...`, `... Protect` but Offensive)
///   put the Speed EVs in HP. Nature: + the attacking stat (+ Speed for
///   `Fast` roles), - the other attacking stat (- Speed under Trick Room,
///   - Defense when both are used).
/// - Supports (`Support` roles, or no attacking move): 252 HP and 128 in
///   each defence (Speed instead of the defences for `Fast Support`), a
///   nature raising Defense (Speed when fast) and lowering the unused
///   attacking stat.
/// - IVs: 0 Attack when no move uses it, 0 Speed under Trick Room, else 31.
///
/// Stat Points: 32 for 252, 17 for 128, 2 for 4.
pub(crate) fn competitive(dex: &Dex, set: &SetData, stat_points: bool) -> (Nature, Investment) {
    let (mut physical, mut special) = (0, 0);
    for &m in set.moves() {
        if m == moves::BODY_PRESS || m == moves::FOUL_PLAY {
            continue;
        }
        match dex.move_(m).category {
            MoveCategory::Physical => physical += 1,
            MoveCategory::Special => special += 1,
            MoveCategory::Status => {}
        }
    }
    let base = dex.species(set.species).base_stats;
    let attack = if physical > special || (physical == special && base[Stat::Atk] >= base[Stat::SpA]) {
        Stat::Atk
    } else {
        Stat::SpA
    };
    let other = if attack == Stat::Atk { Stat::SpA } else { Stat::Atk };
    let role = set.role;
    let trick_room = set.no_speed;
    let fast = role.contains("Fast");
    let support = role.contains("Support") || physical + special == 0;
    let bulky = role.contains("Bulky") || (role.contains("Protect") && !role.contains("Offensive"));
    let mut evs = [0u16; 6];
    let mut ivs = Stats::splat(31u8);
    let e = |evs: &mut [u16; 6], s: Stat, v: u16| evs[s as usize] = v;
    let nature;
    if support {
        e(&mut evs, Stat::Hp, 252);
        // The unused attacking stat (Attack when neither is used).
        let lower = if physical > 0 && special == 0 { Stat::SpA } else { Stat::Atk };
        if fast && !trick_room {
            e(&mut evs, Stat::Spe, 252);
            e(&mut evs, Stat::Def, 4);
            nature = nature_of(Stat::Spe, lower);
        } else {
            e(&mut evs, Stat::Def, 128);
            e(&mut evs, Stat::SpD, 128);
            nature = nature_of(Stat::Def, if trick_room { Stat::Spe } else { lower });
        }
    } else {
        e(&mut evs, attack, 252);
        if trick_room || bulky {
            e(&mut evs, Stat::Hp, 252);
            e(&mut evs, Stat::Def, 4);
        } else {
            e(&mut evs, Stat::Spe, 252);
            e(&mut evs, Stat::Hp, 4);
        }
        let lower = if trick_room {
            Stat::Spe
        } else if physical > 0 && special > 0 {
            Stat::Def
        } else {
            other
        };
        let raise = if fast && !trick_room { Stat::Spe } else { attack };
        nature = nature_of(raise, lower);
    }
    if set.no_attack {
        ivs[Stat::Atk] = 0;
    }
    if trick_room {
        ivs[Stat::Spe] = 0;
    }
    (nature, investment(stat_points, evs, ivs))
}

/// The nature raising `plus` and lowering `minus`.
fn nature_of(plus: Stat, minus: Stat) -> Nature {
    Nature::ALL.into_iter().find(|n| n.modifiers() == Some((plus, minus))).unwrap_or(Nature::Serious)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::Seed;

    #[test]
    fn uniform_spreads_are_within_limits() {
        let mut rng = BattleRng::new(Seed::fast(1));
        for _ in 0..2000 {
            match uniform(&mut rng, false) {
                Investment::Evs { evs, .. } => {
                    let total: u32 = evs.0.iter().map(|&v| v as u32).sum();
                    assert!(total > 0 && total <= 508 && evs.0.iter().all(|&v| v <= 252 && v % 4 == 0));
                }
                Investment::StatPoints(_) => unreachable!(),
            }
            match uniform(&mut rng, true) {
                Investment::StatPoints(sp) => {
                    let total: u32 = sp.0.iter().map(|&v| v as u32).sum();
                    assert!(total > 0 && total <= 66 && sp.0.iter().all(|&v| v <= 32));
                }
                Investment::Evs { .. } => unreachable!(),
            }
        }
    }

    #[test]
    fn natures_by_modifier() {
        assert_eq!(nature_of(Stat::Atk, Stat::SpA), Nature::Adamant);
        assert_eq!(nature_of(Stat::Spe, Stat::Atk), Nature::Timid);
        assert_eq!(nature_of(Stat::SpA, Stat::Spe), Nature::Quiet);
        assert_eq!(nature_of(Stat::Def, Stat::Atk), Nature::Bold);
    }

    #[test]
    fn competitive_spreads_fill_the_limits() {
        let dex = &pikapro_data::gen9::DEX;
        for table in [&crate::sets::GEN9, &crate::sets::GEN9_DOUBLES, &crate::sets::CHAMPIONS] {
            for set in table.iter() {
                for sp in [false, true] {
                    let (_, inv) = competitive(dex, &set, sp);
                    let (vals, limit) = match inv {
                        Investment::Evs { evs, .. } => (evs.0, 508),
                        Investment::StatPoints(s) => (s.0, 66),
                    };
                    let total: u32 = vals.iter().map(|&v| v as u32).sum();
                    assert_eq!(total, limit, "{set:?}");
                }
            }
        }
    }
}
