//! The 18 types, Stellar (a Tera type), the typeless `???` and the Gen 6+
//! type chart.

/// A Pokémon/move type. Discriminants index the type chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Type {
    /// Normal.
    Normal = 0,
    /// Fire.
    Fire,
    /// Water.
    Water,
    /// Electric.
    Electric,
    /// Grass.
    Grass,
    /// Ice.
    Ice,
    /// Fighting.
    Fighting,
    /// Poison.
    Poison,
    /// Ground.
    Ground,
    /// Flying.
    Flying,
    /// Psychic.
    Psychic,
    /// Bug.
    Bug,
    /// Rock.
    Rock,
    /// Ghost.
    Ghost,
    /// Dragon.
    Dragon,
    /// Dark.
    Dark,
    /// Steel.
    Steel,
    /// Fairy.
    Fairy,
    /// The Stellar Tera type (Terapagos; any Pokémon may have it as its
    /// Tera type). No Pokémon has it as a type; a move is Stellar-type as
    /// Tera Blast or Tera Starstorm. Showdown's type chart has it neutral
    /// both ways; it is not in [`Type::ALL`] or the chart.
    Stellar,
    /// Showdown's typeless `???`: what Burn Up and Double Shock leave in
    /// place of the type they remove (`setType` with Fire or Electric
    /// mapped to `???`), and the type of a Revelation Dance whose user has
    /// no other. Neutral both ways (`dex.types.get('???')` has no
    /// `damageTaken`); not in [`Type::ALL`], [`Type::TERA`] or the chart,
    /// and no Pokémon or move has it in the data.
    Unknown,
}

impl Type {
    /// The number of types (Stellar and `???` aside).
    pub const COUNT: usize = 18;
    /// The 18 types, in chart order.
    #[rustfmt::skip]
    pub const ALL: [Type; Self::COUNT] = [
        Type::Normal, Type::Fire, Type::Water, Type::Electric, Type::Grass, Type::Ice,
        Type::Fighting, Type::Poison, Type::Ground, Type::Flying, Type::Psychic, Type::Bug,
        Type::Rock, Type::Ghost, Type::Dragon, Type::Dark, Type::Steel, Type::Fairy,
    ];

    /// Every Tera type: the 18 types and Stellar.
    #[rustfmt::skip]
    pub const TERA: [Type; Self::COUNT + 1] = [
        Type::Normal, Type::Fire, Type::Water, Type::Electric, Type::Grass, Type::Ice,
        Type::Fighting, Type::Poison, Type::Ground, Type::Flying, Type::Psychic, Type::Bug,
        Type::Rock, Type::Ghost, Type::Dragon, Type::Dark, Type::Steel, Type::Fairy, Type::Stellar,
    ];

    /// Showdown's name (`"Fire"`, `"Stellar"`, `"???"`).
    pub fn name(self) -> &'static str {
        match self {
            Type::Stellar => return "Stellar",
            Type::Unknown => return "???",
            _ => {}
        }
        #[rustfmt::skip]
        const NAMES: [&str; Type::COUNT] = [
            "Normal", "Fire", "Water", "Electric", "Grass", "Ice", "Fighting", "Poison", "Ground",
            "Flying", "Psychic", "Bug", "Rock", "Ghost", "Dragon", "Dark", "Steel", "Fairy",
        ];
        NAMES[self as usize]
    }

    /// The type or Tera type named `name`, compared as Showdown IDs.
    pub fn from_name(name: &str) -> Option<Type> {
        let id = crate::NameId::new(name)?;
        Self::TERA.into_iter().find(|t| id.matches(t.name()))
    }

    /// Effectiveness of an attack of type `self` against a single defending
    /// type. Stellar is neutral both ways.
    #[inline]
    pub fn against(self, defender: Type) -> Effectiveness {
        match TYPE_CHART.get(self as usize).and_then(|row| row.get(defender as usize)) {
            Some(&e) => e,
            None => Effectiveness::NEUTRAL,
        }
    }

    /// Effectiveness of an attack of type `self` against a (possibly dual-typed) defender.
    #[inline]
    pub fn against_set(self, defender: TypeSet) -> Effectiveness {
        let a = self.against(defender.primary);
        match defender.secondary {
            Some(t) => a.combine(self.against(t)),
            None => a,
        }
    }
}

/// A Tera type: one of the 18 types, or Stellar (Terapagos).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TeraType {
    /// One of the 18 types (never [`Type::Stellar`]: that is
    /// [`TeraType::Stellar`]).
    Type(Type),
    /// Stellar.
    Stellar,
}

impl TeraType {
    /// The Tera type as a [`Type`] ([`Type::Stellar`] for Stellar).
    pub const fn ty(self) -> Type {
        match self {
            TeraType::Type(t) => t,
            TeraType::Stellar => Type::Stellar,
        }
    }
}

/// A Pokémon's one or two types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypeSet {
    /// The first type.
    pub primary: Type,
    /// The second type, if any.
    pub secondary: Option<Type>,
}

impl TypeSet {
    /// One type.
    pub const fn mono(t: Type) -> Self {
        TypeSet { primary: t, secondary: None }
    }

    /// Two types, `a` first.
    pub const fn dual(a: Type, b: Type) -> Self {
        TypeSet { primary: a, secondary: Some(b) }
    }

    /// Whether `t` is one of them.
    #[inline]
    pub fn contains(self, t: Type) -> bool {
        self.primary == t || self.secondary == Some(t)
    }
}

/// Type effectiveness as a power of two: the damage multiplier is `2^exponent`,
/// or zero if immune. Integer-only so mechanics stay deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effectiveness {
    /// No effect.
    Immune,
    /// `2^n` multiplier, `n` in `-2..=2` for a dual-type defender.
    Exp(i8),
}

impl Effectiveness {
    /// Neutral (`2^0`).
    pub const NEUTRAL: Effectiveness = Effectiveness::Exp(0);

    /// The effectiveness against both of two types.
    #[inline]
    pub fn combine(self, other: Effectiveness) -> Effectiveness {
        match (self, other) {
            (Effectiveness::Exp(a), Effectiveness::Exp(b)) => Effectiveness::Exp(a + b),
            _ => Effectiveness::Immune,
        }
    }

    /// Applies the multiplier to damage the same way Showdown does:
    /// repeated integer doubling/halving.
    #[inline]
    pub fn apply(self, damage: u32) -> u32 {
        match self {
            Effectiveness::Immune => 0,
            Effectiveness::Exp(n) if n >= 0 => damage << n,
            Effectiveness::Exp(n) => damage >> (-n),
        }
    }
}

const fn build_chart() -> [[Effectiveness; Type::COUNT]; Type::COUNT] {
    use Type::*;
    const SE: Effectiveness = Effectiveness::Exp(1);
    const NVE: Effectiveness = Effectiveness::Exp(-1);
    const IM: Effectiveness = Effectiveness::Immune;
    // (attacker, defender, effectiveness); everything else is neutral.
    #[rustfmt::skip]
    const ENTRIES: &[(Type, Type, Effectiveness)] = &[
        (Normal, Rock, NVE), (Normal, Ghost, IM), (Normal, Steel, NVE),
        (Fire, Fire, NVE), (Fire, Water, NVE), (Fire, Grass, SE), (Fire, Ice, SE), (Fire, Bug, SE),
        (Fire, Rock, NVE), (Fire, Dragon, NVE), (Fire, Steel, SE),
        (Water, Fire, SE), (Water, Water, NVE), (Water, Grass, NVE), (Water, Ground, SE),
        (Water, Rock, SE), (Water, Dragon, NVE),
        (Electric, Water, SE), (Electric, Electric, NVE), (Electric, Grass, NVE),
        (Electric, Ground, IM), (Electric, Flying, SE), (Electric, Dragon, NVE),
        (Grass, Fire, NVE), (Grass, Water, SE), (Grass, Grass, NVE), (Grass, Poison, NVE),
        (Grass, Ground, SE), (Grass, Flying, NVE), (Grass, Bug, NVE), (Grass, Rock, SE),
        (Grass, Dragon, NVE), (Grass, Steel, NVE),
        (Ice, Fire, NVE), (Ice, Water, NVE), (Ice, Grass, SE), (Ice, Ice, NVE), (Ice, Ground, SE),
        (Ice, Flying, SE), (Ice, Dragon, SE), (Ice, Steel, NVE),
        (Fighting, Normal, SE), (Fighting, Ice, SE), (Fighting, Poison, NVE),
        (Fighting, Flying, NVE), (Fighting, Psychic, NVE), (Fighting, Bug, NVE),
        (Fighting, Rock, SE), (Fighting, Ghost, IM), (Fighting, Dark, SE), (Fighting, Steel, SE),
        (Fighting, Fairy, NVE),
        (Poison, Grass, SE), (Poison, Poison, NVE), (Poison, Ground, NVE), (Poison, Rock, NVE),
        (Poison, Ghost, NVE), (Poison, Steel, IM), (Poison, Fairy, SE),
        (Ground, Fire, SE), (Ground, Electric, SE), (Ground, Grass, NVE), (Ground, Poison, SE),
        (Ground, Flying, IM), (Ground, Bug, NVE), (Ground, Rock, SE), (Ground, Steel, SE),
        (Flying, Electric, NVE), (Flying, Grass, SE), (Flying, Fighting, SE), (Flying, Bug, SE),
        (Flying, Rock, NVE), (Flying, Steel, NVE),
        (Psychic, Fighting, SE), (Psychic, Poison, SE), (Psychic, Psychic, NVE),
        (Psychic, Dark, IM), (Psychic, Steel, NVE),
        (Bug, Fire, NVE), (Bug, Grass, SE), (Bug, Fighting, NVE), (Bug, Poison, NVE),
        (Bug, Flying, NVE), (Bug, Psychic, SE), (Bug, Ghost, NVE), (Bug, Dark, SE),
        (Bug, Steel, NVE), (Bug, Fairy, NVE),
        (Rock, Fire, SE), (Rock, Ice, SE), (Rock, Fighting, NVE), (Rock, Ground, NVE),
        (Rock, Flying, SE), (Rock, Bug, SE), (Rock, Steel, NVE),
        (Ghost, Normal, IM), (Ghost, Psychic, SE), (Ghost, Ghost, SE), (Ghost, Dark, NVE),
        (Dragon, Dragon, SE), (Dragon, Steel, NVE), (Dragon, Fairy, IM),
        (Dark, Fighting, NVE), (Dark, Psychic, SE), (Dark, Ghost, SE), (Dark, Dark, NVE),
        (Dark, Fairy, NVE),
        (Steel, Fire, NVE), (Steel, Water, NVE), (Steel, Electric, NVE), (Steel, Ice, SE),
        (Steel, Rock, SE), (Steel, Steel, NVE), (Steel, Fairy, SE),
        (Fairy, Fire, NVE), (Fairy, Fighting, SE), (Fairy, Poison, NVE), (Fairy, Dragon, SE),
        (Fairy, Dark, SE), (Fairy, Steel, NVE),
    ];
    let mut chart = [[Effectiveness::NEUTRAL; Type::COUNT]; Type::COUNT];
    let mut i = 0;
    while i < ENTRIES.len() {
        let (a, d, e) = ENTRIES[i];
        chart[a as usize][d as usize] = e;
        i += 1;
    }
    chart
}

/// `TYPE_CHART[attacker][defender]`.
pub static TYPE_CHART: [[Effectiveness; Type::COUNT]; Type::COUNT] = build_chart();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_shape() {
        let flat = TYPE_CHART.iter().flatten();
        let count = |want: Effectiveness| flat.clone().filter(|&&e| e == want).count();
        assert_eq!(count(Effectiveness::Immune), 8);
        assert_eq!(count(Effectiveness::Exp(1)), 51);
        assert_eq!(count(Effectiveness::Exp(-1)), 61);
    }

    #[test]
    fn dual_types() {
        let incineroar = TypeSet::dual(Type::Fire, Type::Dark);
        assert_eq!(Type::Water.against_set(incineroar), Effectiveness::Exp(1));
        assert_eq!(Type::Psychic.against_set(incineroar), Effectiveness::Immune);
        let rillaboom = TypeSet::mono(Type::Grass);
        assert_eq!(Type::Fire.against_set(rillaboom), Effectiveness::Exp(1));
        let gholdengo = TypeSet::dual(Type::Steel, Type::Ghost);
        assert_eq!(Type::Normal.against_set(gholdengo), Effectiveness::Immune);
        assert_eq!(Type::Dark.against_set(gholdengo), Effectiveness::Exp(1));
        let amoonguss = TypeSet::dual(Type::Grass, Type::Poison);
        assert_eq!(Type::Grass.against_set(amoonguss), Effectiveness::Exp(-2));
        assert_eq!(Effectiveness::Exp(-2).apply(100), 25);
        assert_eq!(Effectiveness::Exp(2).apply(100), 400);
    }

    #[test]
    fn stellar_is_neutral() {
        for t in Type::ALL {
            assert_eq!(Type::Stellar.against(t), Effectiveness::NEUTRAL);
            assert_eq!(t.against(Type::Stellar), Effectiveness::NEUTRAL);
        }
        assert_eq!(Type::from_name("stellar"), Some(Type::Stellar));
        assert_eq!(Type::Stellar.name(), "Stellar");
        assert_eq!(TeraType::Stellar.ty(), Type::Stellar);
    }

    #[test]
    fn unknown_is_neutral() {
        for t in Type::TERA {
            assert_eq!(Type::Unknown.against(t), Effectiveness::NEUTRAL);
            assert_eq!(t.against(Type::Unknown), Effectiveness::NEUTRAL);
        }
        assert_eq!(Type::Unknown.name(), "???");
        // Neither a Tera type nor a name a team can give.
        assert_eq!(Type::from_name("???"), None);
    }
}
