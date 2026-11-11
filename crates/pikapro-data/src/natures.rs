//! The 25 natures, in canonical (Showdown / in-game index) order.

use crate::Stat;

/// A nature (each variant is the nature of its name).
#[rustfmt::skip]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum Nature {
    #[default]
    Hardy, Lonely, Brave, Adamant, Naughty,
    Bold, Docile, Relaxed, Impish, Lax,
    Timid, Hasty, Serious, Jolly, Naive,
    Modest, Mild, Quiet, Bashful, Rash,
    Calm, Gentle, Sassy, Careful, Quirky,
}

impl Nature {
    /// Every nature, in index order.
    #[rustfmt::skip]
    pub const ALL: [Nature; 25] = [
        Nature::Hardy, Nature::Lonely, Nature::Brave, Nature::Adamant, Nature::Naughty,
        Nature::Bold, Nature::Docile, Nature::Relaxed, Nature::Impish, Nature::Lax,
        Nature::Timid, Nature::Hasty, Nature::Serious, Nature::Jolly, Nature::Naive,
        Nature::Modest, Nature::Mild, Nature::Quiet, Nature::Bashful, Nature::Rash,
        Nature::Calm, Nature::Gentle, Nature::Sassy, Nature::Careful, Nature::Quirky,
    ];

    /// The index ordering encodes the modifiers: `plus = i / 5`, `minus = i % 5`
    /// over `[Atk, Def, Spe, SpA, SpD]`. Equal indices are neutral natures.
    const ORDER: [Stat; 5] = [Stat::Atk, Stat::Def, Stat::Spe, Stat::SpA, Stat::SpD];

    /// `(boosted, lowered)` stats, or `None` for a neutral nature.
    pub const fn modifiers(self) -> Option<(Stat, Stat)> {
        let i = self as usize;
        let (plus, minus) = (i / 5, i % 5);
        if plus == minus { None } else { Some((Self::ORDER[plus], Self::ORDER[minus])) }
    }

    /// Multiplier for `stat` as a fraction over 10 (`11`, `10` or `9`).
    pub const fn multiplier_tenths(self, stat: Stat) -> u16 {
        match self.modifiers() {
            Some((plus, _)) if plus as u8 == stat as u8 => 11,
            Some((_, minus)) if minus as u8 == stat as u8 => 9,
            _ => 10,
        }
    }

    /// Its name (`"Adamant"`).
    pub fn name(self) -> &'static str {
        #[rustfmt::skip]
        const NAMES: [&str; 25] = [
            "Hardy", "Lonely", "Brave", "Adamant", "Naughty",
            "Bold", "Docile", "Relaxed", "Impish", "Lax",
            "Timid", "Hasty", "Serious", "Jolly", "Naive",
            "Modest", "Mild", "Quiet", "Bashful", "Rash",
            "Calm", "Gentle", "Sassy", "Careful", "Quirky",
        ];
        NAMES[self as usize]
    }

    /// The nature named `name`, compared as Showdown IDs.
    pub fn from_name(name: &str) -> Option<Nature> {
        let id = crate::NameId::new(name)?;
        Self::ALL.into_iter().find(|n| id.matches(n.name()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_natures() {
        assert_eq!(Nature::Adamant.modifiers(), Some((Stat::Atk, Stat::SpA)));
        assert_eq!(Nature::Timid.modifiers(), Some((Stat::Spe, Stat::Atk)));
        assert_eq!(Nature::Sassy.modifiers(), Some((Stat::SpD, Stat::Spe)));
        assert_eq!(Nature::Quiet.modifiers(), Some((Stat::SpA, Stat::Spe)));
        assert_eq!(Nature::Serious.modifiers(), None);
        assert_eq!(Nature::Jolly.multiplier_tenths(Stat::Spe), 11);
        assert_eq!(Nature::Jolly.multiplier_tenths(Stat::SpA), 9);
        assert_eq!(Nature::from_name("careful"), Some(Nature::Careful));
        assert_eq!(Nature::ALL.iter().filter(|n| n.modifiers().is_none()).count(), 5);
    }
}
