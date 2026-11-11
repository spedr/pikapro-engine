//! Stat identifiers and a fixed-size per-stat table.

use core::ops::{Index, IndexMut};

/// One of the six permanent stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Stat {
    /// HP.
    Hp = 0,
    /// Attack.
    Atk = 1,
    /// Defense.
    Def = 2,
    /// Special Attack.
    SpA = 3,
    /// Special Defense.
    SpD = 4,
    /// Speed.
    Spe = 5,
}

impl Stat {
    /// The six stats, in order.
    pub const ALL: [Stat; 6] = [Stat::Hp, Stat::Atk, Stat::Def, Stat::SpA, Stat::SpD, Stat::Spe];

    /// Showdown's short name (`"hp"`, `"atk"`, ...).
    pub const fn id(self) -> &'static str {
        match self {
            Stat::Hp => "hp",
            Stat::Atk => "atk",
            Stat::Def => "def",
            Stat::SpA => "spa",
            Stat::SpD => "spd",
            Stat::Spe => "spe",
        }
    }
}

/// A stat with a stage (Showdown's `BoostID`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BoostId {
    /// Attack.
    Atk = 0,
    /// Defense.
    Def = 1,
    /// Special Attack.
    SpA = 2,
    /// Special Defense.
    SpD = 3,
    /// Speed.
    Spe = 4,
    /// Accuracy.
    Accuracy = 5,
    /// Evasion.
    Evasion = 6,
}

impl BoostId {
    /// The seven, in order.
    pub const ALL: [BoostId; 7] =
        [BoostId::Atk, BoostId::Def, BoostId::SpA, BoostId::SpD, BoostId::Spe, BoostId::Accuracy, BoostId::Evasion];

    /// Showdown's ID (`"atk"`, ..., `"evasion"`), as in `-boost` messages.
    pub const fn id(self) -> &'static str {
        match self {
            BoostId::Atk => "atk",
            BoostId::Def => "def",
            BoostId::SpA => "spa",
            BoostId::SpD => "spd",
            BoostId::Spe => "spe",
            BoostId::Accuracy => "accuracy",
            BoostId::Evasion => "evasion",
        }
    }
}

/// A value per stat, indexable by [`Stat`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Stats<T>(pub [T; 6]);

impl<T: Copy> Stats<T> {
    /// The six values, in [`Stat`] order.
    pub const fn new(hp: T, atk: T, def: T, spa: T, spd: T, spe: T) -> Self {
        Stats([hp, atk, def, spa, spd, spe])
    }

    /// `v` for every stat.
    pub const fn splat(v: T) -> Self {
        Stats([v; 6])
    }
}

impl<T> Index<Stat> for Stats<T> {
    type Output = T;
    #[inline]
    fn index(&self, s: Stat) -> &T {
        &self.0[s as usize]
    }
}

impl<T> IndexMut<Stat> for Stats<T> {
    #[inline]
    fn index_mut(&mut self, s: Stat) -> &mut T {
        &mut self.0[s as usize]
    }
}
