//! The name a set battles under, as Showdown's validator leaves
//! `set.name` (`validateSet`).

use pikapro_data::{Dex, SpeciesId};

/// What [`nickname`] makes of a set's nickname.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Nickname {
    /// `set.name` after validation: the nickname cleaned by `Dex.getName`
    /// ([`pikapro_data::get_name`]), or the base species' name when there
    /// is none or it names a species of the same base species.
    pub name: String,
    /// The nickname's length in UTF-16 units when it is over 18 and not the
    /// species' own name: a problem in every format.
    pub too_long: Option<usize>,
    /// It names a species of another base species (not the set's own
    /// species or base species): a problem under Nickname Clause.
    pub names_other_species: bool,
}

/// Showdown's `validateSet` on `set.name`, for a set of `species` (as
/// given) that validation plays as `validated` (after `validateForme`; the
/// same species unless a battle-only forme starts as its base forme):
///
/// * a nickname over 18 UTF-16 units is a problem, unless it is the
///   species' own name, which becomes the base species';
/// * `set.name = dex.getName(set.name)`, then `set.name || baseSpecies`;
/// * a name that, ignoring case, is a species' name: a species of the same
///   base species gives the base species' name, another one is a Nickname
///   Clause problem.
pub fn nickname(dex: &Dex, species: SpeciesId, validated: SpeciesId, nickname: &str) -> Nickname {
    let base = |s: SpeciesId| dex.species(dex.species(s).base_species).name;
    let mut out = Nickname::default();
    let mut given = nickname;
    let len = nickname.encode_utf16().count();
    if len > 18 {
        if nickname == dex.species(species).name {
            given = base(species);
        } else {
            out.too_long = Some(len);
        }
    }
    out.name = pikapro_data::get_name(given);
    if out.name.is_empty() {
        out.name = base(species).to_owned();
    }
    // `dex.species.get(set.name)` with its exact-name check.
    let named =
        dex.species_by_name(&out.name).filter(|&s| dex.species(s).name.to_lowercase() == out.name.to_lowercase());
    if let Some(named) = named {
        if dex.species(named).base_species == dex.species(validated).base_species {
            out.name = base(validated).to_owned();
        } else if named != validated && dex.species(named).name != base(validated) {
            out.names_other_species = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro_data::gen9::species;

    #[test]
    fn names_as_showdowns_validator_leaves_them() {
        let dex = &pikapro_data::gen9::DEX;
        let name = |s, n: &str| nickname(dex, s, s, n);
        assert_eq!(name(species::LANDORUS_THERIAN, "Landorus-Therian").name, "Landorus");
        assert_eq!(name(species::LANDORUS_THERIAN, "landorus").name, "Landorus");
        assert_eq!(name(species::LANDORUS_THERIAN, "").name, "Landorus");
        assert_eq!(name(species::LANDORUS_THERIAN, " \u{2028} ").name, "Landorus");
        assert_eq!(name(species::INCINEROAR, "Bad\rNick\u{2028}X").name, "Bad Nick X");
        let other = name(species::INCINEROAR, "Pikachu");
        assert_eq!((other.name.as_str(), other.names_other_species), ("Pikachu", true));
        let long = name(species::INCINEROAR, "ThisNameIsFarTooLongToKeep");
        assert_eq!((long.name.as_str(), long.too_long), ("ThisNameIsFarTooLo", Some(26)));
    }
}
