//! Competitive-style sets from Showdown's random-battle data.
//!
//! `tools/datagen` runs Showdown's own random-battle generators
//! (`data/random-battles/{gen9,champions}/teams.js`: `randomSet` picks the
//! role, moves, ability, item and Tera type from the random-battle data)
//! 32 times per species from a fixed seed and keeps the distinct sets that
//! Showdown's team validator accepts in the target validation format:
//!
//! | table | Showdown generator | validated as | used for |
//! |---|---|---|---|
//! | [`GEN9`] | `gen9randombattle` (singles sets) | `gen9anythinggoes@@@Species Clause`, level 100 | `gen9singles`, `gen9ou` (whose own validator then drops the sets its tier bans) |
//! | [`GEN9_DOUBLES`] | `gen9randomdoublesbattle` | `gen9anythinggoes@@@Species Clause`, level 100 | `gen9vgc`, `gen9doubles` |
//! | [`CHAMPIONS`] | `gen9championsrandombattle` | `gen9championsvgc2026regmc`, level 50 | `championsvgc`, `championsbss` |
//!
//! Levels, EVs, IVs, natures and genders are not kept: [`crate`] sets them
//! for the format it builds teams for. The random battles' own teams are
//! Showdown's generator's ([`crate::random_battle`]), not these.

use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId, Type};

mod champions;
mod gen9;
mod gen9doubles;

pub use champions::SETS as CHAMPIONS;
pub use gen9::SETS as GEN9;
pub use gen9doubles::SETS as GEN9_DOUBLES;

/// A generated table of sets.
#[derive(Debug)]
pub struct SetTable {
    /// The Showdown format whose generator drew the sets.
    pub source: &'static str,
    /// The Showdown format whose validator accepted them.
    pub validated_as: &'static str,
    /// Draws per species.
    pub draws: u32,
    /// Distinct sets drawn, before validation.
    pub distinct: u32,
    /// Showdown's role names (`"Doubles Support"`, ...), by [`SetData::role`].
    pub roles: &'static [&'static str],
    /// Packed rows: species, item, ability, four moves ([`NONE`] for none),
    /// Tera type (index into [`Type::TERA`], 31 for none) `| role << 5`,
    /// flags ([`SetData::no_attack`], [`SetData::no_speed`]).
    pub rows: &'static [[u16; 9]],
}

/// An absent item or move in a row.
pub const NONE: u16 = 0xffff;

impl SetTable {
    /// The number of sets.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether it has no set.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Set `i`.
    pub fn get(&self, i: usize) -> SetData {
        SetData::decode(&self.rows[i], self.roles)
    }

    /// Every set, in table order.
    pub fn iter(&self) -> impl Iterator<Item = SetData> + '_ {
        self.rows.iter().map(|r| SetData::decode(r, self.roles))
    }
}

/// One set of a [`SetTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetData {
    /// The species it starts the battle as (a Mega's base species).
    pub species: SpeciesId,
    /// The held item.
    pub item: Option<ItemId>,
    /// The ability.
    pub ability: AbilityId,
    moves: [MoveId; 4],
    move_count: u8,
    /// Showdown's Tera type for it (`None` in the Champions table).
    pub tera_type: Option<Type>,
    /// Showdown's role (`"Fast Attacker"`, `"Doubles Support"`, ...).
    pub role: &'static str,
    /// The generator gave it no Attack investment: no move uses Attack.
    pub no_attack: bool,
    /// The generator gave it no Speed investment (Trick Room, Gyro Ball,
    /// Metal Burst).
    pub no_speed: bool,
}

impl SetData {
    fn decode(row: &[u16; 9], roles: &'static [&'static str]) -> SetData {
        let id = |v: u16| (v != NONE).then_some(v);
        let mut moves = [MoveId(0); 4];
        let mut move_count = 0;
        for &m in &row[3..7] {
            if let Some(m) = id(m) {
                moves[move_count] = MoveId(m);
                move_count += 1;
            }
        }
        let tera = (row[7] & 31) as usize;
        SetData {
            species: SpeciesId(row[0]),
            item: id(row[1]).map(ItemId),
            ability: AbilityId(row[2]),
            moves,
            move_count: move_count as u8,
            tera_type: Type::TERA.get(tera).copied(),
            role: roles[(row[7] >> 5) as usize],
            no_attack: row[8] & 1 != 0,
            no_speed: row[8] & 2 != 0,
        }
    }

    /// Its moves, in the order the generator shuffled them.
    pub fn moves(&self) -> &[MoveId] {
        &self.moves[..self.move_count as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pikapro_data::gen9::{self, abilities, items, species};

    #[test]
    fn tables_decode() {
        for table in [&GEN9, &GEN9_DOUBLES, &CHAMPIONS] {
            assert!(table.len() > 1000, "{}", table.source);
            for set in table.iter() {
                assert!((1..=4).contains(&set.moves().len()));
                assert!(set.species.index() < gen9::DEX.species.len());
                assert!(!set.role.is_empty());
            }
        }
        assert!(CHAMPIONS.iter().all(|s| s.tera_type.is_none()));
        assert!(GEN9_DOUBLES.iter().all(|s| s.tera_type.is_some()));
    }

    #[test]
    fn megas_start_as_their_base_species() {
        let charizard = CHAMPIONS
            .iter()
            .filter(|s| s.species == species::CHARIZARD)
            .filter_map(|s| s.item)
            .any(|i| i == items::CHARIZARDITE_X || i == items::CHARIZARDITE_Y);
        assert!(charizard);
        let venusaur = CHAMPIONS.iter().find(|s| s.species == species::VENUSAUR).expect("Venusaur");
        assert!([abilities::CHLOROPHYLL, abilities::OVERGROW].contains(&venusaur.ability));
    }
}
