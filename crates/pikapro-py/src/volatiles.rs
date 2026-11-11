//! Names for the engine's volatile bits (`pikapro::state::Volatiles`),
//! which the raw state arrays (`volatiles0..3`) carry as opaque bits:
//! [`volatile_names`]`()[bit]` names bit `bit`, `""` for a bit no volatile
//! uses (Python's `pikapro.VOLATILE_NAMES`).

use pikapro::state::Volatiles;

/// Every volatile: its bit mask and its name (the constant's name in lower
/// case). A test checks it against the engine's source.
pub const VOLATILES: &[(u128, &str)] = &[
    (Volatiles::PROTECT, "protect"),
    (Volatiles::FLINCH, "flinch"),
    (Volatiles::CONFUSION, "confusion"),
    (Volatiles::MUST_RECHARGE, "must_recharge"),
    (Volatiles::TAUNT, "taunt"),
    (Volatiles::ENCORE, "encore"),
    (Volatiles::DISABLE, "disable"),
    (Volatiles::STALL, "stall"),
    (Volatiles::TWO_TURN_MOVE, "two_turn_move"),
    (Volatiles::CHARGING, "charging"),
    (Volatiles::LOCKED_MOVE, "locked_move"),
    (Volatiles::UPROAR, "uproar"),
    (Volatiles::ROLLOUT, "rollout"),
    (Volatiles::PRIORITY_CHARGE, "priority_charge"),
    (Volatiles::COUNTER, "counter"),
    (Volatiles::MIRROR_COAT, "mirror_coat"),
    (Volatiles::SUBSTITUTE, "substitute"),
    (Volatiles::PARTIALLY_TRAPPED, "partially_trapped"),
    (Volatiles::TRAPPED, "trapped"),
    (Volatiles::NO_RETREAT, "no_retreat"),
    (Volatiles::LEECH_SEED, "leech_seed"),
    (Volatiles::YAWN, "yawn"),
    (Volatiles::CURSE, "curse"),
    (Volatiles::PERISH_SONG, "perish_song"),
    (Volatiles::DESTINY_BOND, "destiny_bond"),
    (Volatiles::ATTRACT, "attract"),
    (Volatiles::TORMENT, "torment"),
    (Volatiles::SALT_CURE, "salt_cure"),
    (Volatiles::SYRUP_BOMB, "syrup_bomb"),
    (Volatiles::THROAT_CHOP, "throat_chop"),
    (Volatiles::HEAL_BLOCK, "heal_block"),
    (Volatiles::IMPRISON, "imprison"),
    (Volatiles::GLAIVE_RUSH, "glaive_rush"),
    (Volatiles::ENDURE, "endure"),
    (Volatiles::SMACK_DOWN, "smack_down"),
    (Volatiles::MAGNET_RISE, "magnet_rise"),
    (Volatiles::AQUA_RING, "aqua_ring"),
    (Volatiles::INGRAIN, "ingrain"),
    (Volatiles::CHARGE, "charge"),
    (Volatiles::FOCUS_ENERGY, "focus_energy"),
    (Volatiles::DRAGON_CHEER, "dragon_cheer"),
    (Volatiles::ROOST, "roost"),
    (Volatiles::TAR_SHOT, "tar_shot"),
    (Volatiles::MINIMIZE, "minimize"),
    (Volatiles::DEFENSE_CURL, "defense_curl"),
    (Volatiles::STOCKPILE, "stockpile"),
    (Volatiles::POWER_TRICK, "power_trick"),
    (Volatiles::POWER_SHIFT, "power_shift"),
    (Volatiles::ELECTRIFY, "electrify"),
    (Volatiles::LOCK_ON, "lock_on"),
    (Volatiles::HELPING_HAND, "helping_hand"),
    (Volatiles::FOLLOW_ME, "follow_me"),
    (Volatiles::RAGE_POWDER, "rage_powder"),
    (Volatiles::ALLY_SWITCH, "ally_switch"),
    (Volatiles::TRANSFORMED, "transformed"),
    (Volatiles::MIMIC, "mimic"),
    (Volatiles::FURY_CUTTER, "fury_cutter"),
    (Volatiles::OBSTRUCT, "obstruct"),
    (Volatiles::BURNING_BULWARK, "burning_bulwark"),
    (Volatiles::SILK_TRAP, "silk_trap"),
    (Volatiles::BANEFUL_BUNKER, "baneful_bunker"),
    (Volatiles::KINGS_SHIELD, "kings_shield"),
    (Volatiles::SPIKY_SHIELD, "spiky_shield"),
    (Volatiles::CHOICE_LOCK, "choice_lock"),
    (Volatiles::GASTRO_ACID, "gastro_acid"),
    (Volatiles::COMMANDING, "commanding"),
    (Volatiles::ZEN_MODE, "zen_mode"),
    (Volatiles::PROTOSYNTHESIS, "protosynthesis"),
    (Volatiles::QUARK_DRIVE, "quark_drive"),
    (Volatiles::TRUANT, "truant"),
    (Volatiles::COMMANDED, "commanded"),
    (Volatiles::FLASH_FIRE, "flash_fire"),
    (Volatiles::METRONOME, "metronome"),
    (Volatiles::UNBURDEN, "unburden"),
    (Volatiles::MICLE_BERRY, "micle_berry"),
    (Volatiles::FLING, "fling"),
    (Volatiles::GEM, "gem"),
    (Volatiles::OCTOLOCK, "octolock"),
];

/// The name of each of the 128 volatile bits, `""` for an unused one.
pub fn volatile_names() -> Vec<&'static str> {
    let mut names = vec![""; 128];
    for &(mask, name) in VOLATILES {
        names[mask.trailing_zeros() as usize] = name;
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro::{Battle, BattleOptions, BattleRng, Format, Seed, SideId};

    /// Every `1 << n` constant of `impl Volatiles` in the engine's source
    /// has its name here, and each name is one bit of its own.
    #[test]
    fn every_volatile_constant_is_named() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pikapro/src/state.rs");
        let text = std::fs::read_to_string(path).unwrap();
        let block = text.split("impl Volatiles {").nth(1).unwrap().split("\n}\n").next().unwrap();
        let mut consts = 0;
        for line in block.lines() {
            let Some(rest) = line.trim().strip_prefix("pub const ") else { continue };
            let Some((name, value)) = rest.split_once(": u128 = ") else { continue };
            if !value.starts_with("1 << ") {
                continue;
            }
            consts += 1;
            let lower = name.to_ascii_lowercase();
            assert!(VOLATILES.iter().any(|&(_, n)| n == lower), "{name} has no name");
        }
        assert_eq!(consts, VOLATILES.len());
        let names = volatile_names();
        let mut all = 0u128;
        for &(mask, name) in VOLATILES {
            assert_eq!(mask.count_ones(), 1, "{name}");
            assert_eq!(all & mask, 0, "{name} shares a bit");
            all |= mask;
            assert_eq!(names[mask.trailing_zeros() as usize], name);
        }
    }

    /// Fuzz: random battles in every preset, with random legal teams (seeds
    /// 4,600,000 on): every volatile bit a Pokémon ever has is named.
    #[test]
    fn every_volatile_seen_is_named() {
        let names = volatile_names();
        let mut seen = 0u128;
        for format in Format::presets() {
            let g =
                crate::teams::generator(format, pikapro_teams::Mode::Sets, pikapro_teams::Options::default()).unwrap();
            for seed in 4_600_000..4_600_030u64 {
                let [a, b] = crate::teams::pair(&g, seed).unwrap();
                let mut options = BattleOptions::new(Seed::fast(seed));
                options.log_events = false;
                let mut battle = Battle::new(format.clone(), [a.team, b.team], options).unwrap();
                let mut rng = BattleRng::new(Seed::fast(!seed));
                while !battle.is_over() {
                    for side in SideId::BOTH {
                        for mon in battle.state().side(side).team.iter() {
                            seen |= mon.volatiles.bits();
                        }
                        let legal = battle.legal_actions(side);
                        if !legal.is_waiting() && !battle.is_over() {
                            let choice = legal.sample(&mut rng);
                            battle.submit(side, choice).unwrap();
                        }
                    }
                }
            }
        }
        for (bit, name) in names.iter().enumerate() {
            if seen >> bit & 1 == 1 {
                assert!(!name.is_empty(), "volatile bit {bit} has no name");
            }
        }
        assert!(seen.count_ones() > 10, "{seen:#x}");
    }
}
