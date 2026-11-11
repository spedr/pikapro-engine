//! Stat formulas (Gen 3+). Pure integer functions.

use pikapro_data::{Nature, Stat};

/// Computes a permanent stat from base stat, IV, EV, level and nature.
///
/// HP:     `floor((2B + IV + floor(EV/4)) * L / 100) + L + 10`
/// Others: `floor((floor((2B + IV + floor(EV/4)) * L / 100) + 5) * nature)`
pub fn calc_stat(stat: Stat, base: u8, iv: u8, ev: u8, level: u8, nature: Nature) -> u16 {
    let core = (2 * base as u32 + iv as u32 + ev as u32 / 4) * level as u32 / 100;
    if stat == Stat::Hp {
        // Shedinja's base HP of 1 always yields 1 HP.
        if base == 1 { 1 } else { (core + level as u32 + 10) as u16 }
    } else {
        ((core + 5) * nature.multiplier_tenths(stat) as u32 / 10) as u16
    }
}

/// Converts Champions Stat Points to the EV-equivalent used by the stat
/// formula: the first point is worth 4 EVs, each further point 8, so 32 SP
/// equals 252 EVs.
///
/// The stat formula's `floor(EV / 4)` of this is `2 * sp - 1` for `sp >= 1`:
/// the Champions mod's `statModify` term `max(2 * evs - 1, 0)`, where its
/// `evs` are Stat Points (`data/mods/champions/scripts.ts`).
pub const fn stat_points_to_evs(sp: u8) -> u8 {
    if sp == 0 {
        0
    } else {
        let evs = 4 + 8 * (sp as u16 - 1);
        if evs > 252 { 252 } else { evs as u8 }
    }
}

/// Applies a stat stage (-6..=6) to a stat, as Showdown does:
/// positive stages multiply by `(2+n)/2`, negative divide by `(2+n)/2`.
#[inline]
pub fn apply_stage(value: u16, stage: i8) -> u16 {
    let v = value as u32;
    let n = stage.clamp(-6, 6);
    if n >= 0 { (v * (2 + n as u32) / 2) as u16 } else { (v * 2 / (2 + (-n) as u32)) as u16 }
}

/// Multiplies `value` by `modifier / 4096` with Showdown's rounding
/// (`modify` / pokeRound: halves round down).
#[inline]
pub fn modify(value: u32, modifier: u32) -> u32 {
    (value * modifier + 2047) / 4096
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_stats() {
        // Incineroar, level 50, 31 IV, 252 HP EVs: 202 HP.
        assert_eq!(calc_stat(Stat::Hp, 95, 31, 252, 50, Nature::Careful), 202);
        // Garchomp, level 50, 31 IV, 252 Spe EVs, Jolly: 169 Spe.
        assert_eq!(calc_stat(Stat::Spe, 102, 31, 252, 50, Nature::Jolly), 169);
        // Level 100, neutral nature, 0 EV/IV Pikachu speed: 2*90*1 + 5 = 185.
        assert_eq!(calc_stat(Stat::Spe, 90, 0, 0, 100, Nature::Hardy), 185);
    }

    #[test]
    fn stat_points() {
        assert_eq!(stat_points_to_evs(0), 0);
        assert_eq!(stat_points_to_evs(1), 4);
        assert_eq!(stat_points_to_evs(2), 12);
        assert_eq!(stat_points_to_evs(32), 252);
    }

    #[test]
    fn stages_and_modify() {
        assert_eq!(apply_stage(100, 1), 150);
        assert_eq!(apply_stage(100, -1), 66);
        assert_eq!(apply_stage(100, 6), 400);
        assert_eq!(apply_stage(100, -6), 25);
        assert_eq!(modify(100, 3072), 75); // spread 0.75x
        assert_eq!(modify(3, 6144), 4); // 4.5 rounds down
        assert_eq!(modify(5, 6144), 7); // 7.5 rounds down
    }
}
