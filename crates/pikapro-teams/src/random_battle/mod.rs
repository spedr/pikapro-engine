//! Showdown's Gen 9 random-battle team generator, ported exactly: the team
//! a seed gives is the team Showdown's `[Gen 9] Random Battle` or `[Gen 9]
//! Random Doubles Battle` gives for that seed, draw for draw.
//!
//! The port follows `RandomTeams` in
//! `data/random-battles/gen9/teams.ts` (`randomTeam`, `getPokemonPool`,
//! `getPokemonCompatibility`, `randomSet`, `randomMoveset`,
//! `cullMovePool`, `incompatibleMoves`, `queryMoves`, `getAbility`,
//! `shouldCullAbility`, `getPriorityItem`, `getItem`, `getDoublesItem`,
//! `getLevel`, `getForme`), with the JavaScript it relies on (`Set` and
//! object key order, `fastPop`, `sampleNoReplace`, the PRNG's `sample`,
//! `random`, `randomChance` and `shuffle`). Its data is generated from
//! Showdown's `sets.json` and `doubles-sets.json` (`data.rs`).
//!
//! A battle's teams come from a PRNG of their own, one per player, seeded
//! with the player's seed (`Battle.getTeam`: the `seed` of `>player`, or a
//! fresh `sodium` seed Showdown then writes into the input log). Give
//! [`generate`] that seed's generator: `BattleRng::new(Seed::Sodium(..))`
//! or `Seed::ShowdownGen5(..)`, as `pikapro-server` reads Showdown's seed
//! strings. Any other [`BattleRng`] gives a team the generator could give,
//! but not one Showdown can reproduce.
//!
//! ```
//! use pikapro::{BattleRng, Seed};
//! use pikapro_teams::random_battle::{self, RandomBattle};
//!
//! // Showdown's team for the seed `gen5,0001000200030004`.
//! let mut rng = BattleRng::new(Seed::ShowdownGen5([1, 2, 3, 4]));
//! let team = random_battle::generate(RandomBattle::Singles, &mut rng).unwrap();
//! assert_eq!(team.len(), 6);
//! ```

// Integer math only, as in the engine: a seed's team is the same on every
// platform.
#![deny(clippy::float_arithmetic)]

mod data;
mod generator;

pub use data::ADDED_FORMES;

use core::fmt;

use pikapro::{BattleRng, Gender, Investment, PokemonSet, Seed, Team};
use pikapro_data::{AbilityId, ItemId, MoveId, Nature, SpeciesId, Stats, Type};

/// The two formats the generator serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RandomBattle {
    /// `[Gen 9] Random Battle` (`gen9randombattle`): `sets.json`.
    Singles,
    /// `[Gen 9] Random Doubles Battle` (`gen9randomdoublesbattle`):
    /// `doubles-sets.json` and the generator's doubles branches.
    Doubles,
}

impl RandomBattle {
    /// The generator for a format id: `gen9randombattle` or
    /// `gen9randomdoublesbattle`.
    pub fn for_format(id: &str) -> Option<RandomBattle> {
        match id {
            "gen9randombattle" => Some(RandomBattle::Singles),
            "gen9randomdoublesbattle" => Some(RandomBattle::Doubles),
            _ => None,
        }
    }

    /// Showdown's format id.
    pub const fn format_id(self) -> &'static str {
        match self {
            RandomBattle::Singles => "gen9randombattle",
            RandomBattle::Doubles => "gen9randomdoublesbattle",
        }
    }
}

/// A set as the generator returns it (`randomSet`'s object).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RandomSet {
    /// `species`: the forme it plays as (`getForme`: a cosmetic forme,
    /// Zacian for Zacian-Crowned, ...).
    pub species: SpeciesId,
    /// `speciesId`: the species whose sets it was drawn from.
    pub species_id: SpeciesId,
    /// `name`: its base species (the nickname a battle shows).
    pub name: SpeciesId,
    /// `gender`: `M`, `F` or `N` ([`Gender::Genderless`]).
    pub gender: Gender,
    /// `shiny`: one in 1024.
    pub shiny: bool,
    /// `level`: the species' in the table (`getLevel`).
    pub level: u8,
    /// In the order the generator shuffled them.
    pub moves: Vec<MoveId>,
    /// `ability`.
    pub ability: AbilityId,
    /// `evs`: 85 each, less HP where `randomSet` trims it, 0 Attack or
    /// Speed where it zeroes them.
    pub evs: Stats<u8>,
    /// `ivs`: 31 each, 0 Attack or Speed where the EVs are zeroed.
    pub ivs: Stats<u8>,
    /// `None`: no item (Acrobatics users).
    pub item: Option<ItemId>,
    /// `teraType`.
    pub tera_type: Type,
    /// The set's role (`"Fast Attacker"`, `"Doubles Support"`, ...).
    pub role: &'static str,
}

impl RandomSet {
    /// The set the engine plays: no nature (Showdown's sets have none, a
    /// neutral one), the generator's gender.
    pub fn to_set(&self) -> PokemonSet {
        let mut set = PokemonSet::new(self.species)
            .with_level(self.level)
            .with_ability(self.ability)
            .with_nature(Nature::default())
            .with_investment(Investment::Evs { evs: self.evs, ivs: self.ivs })
            .with_moves(&self.moves)
            .with_tera_type(self.tera_type)
            .with_gender(self.gender);
        set.item = self.item;
        set
    }
}

/// Showdown's seed `gen5,<n in 16 hex digits>`: the Gen 5 LCG starting
/// at `n` (four 16-bit words, most significant first).
pub const fn gen5_seed(n: u64) -> Seed {
    Seed::ShowdownGen5([(n >> 48) as u16, (n >> 32) as u16, (n >> 16) as u16, n as u16])
}

/// The engine's team for generated sets, in the generator's order.
pub fn to_team(sets: &[RandomSet]) -> Team {
    Team::new(sets.iter().map(RandomSet::to_set).collect())
}

/// A team as `tools/showdown-oracle/random-teams.mjs` prints Showdown's:
/// each set's fields joined by `|` (species, the species its sets are
/// from, name, gender, `S` if shiny, level, moves, ability, item, EVs,
/// IVs, Tera type, role; moves, EVs and IVs joined by `,`; by name), the
/// sets joined by `]`.
pub fn team_text(sets: &[RandomSet]) -> String {
    let dex = &pikapro_data::gen9::DEX;
    let stats = |s: &Stats<u8>| s.0.iter().map(u8::to_string).collect::<Vec<_>>().join(",");
    sets.iter()
        .map(|s| {
            let moves: Vec<&str> = s.moves.iter().map(|&m| dex.move_(m).name).collect();
            let gender = match s.gender {
                Gender::Male => "M",
                Gender::Female => "F",
                Gender::Genderless => "N",
            };
            [
                dex.species(s.species).name,
                dex.species(s.species_id).name,
                dex.species(s.name).name,
                gender,
                if s.shiny { "S" } else { "" },
                &s.level.to_string(),
                &moves.join(","),
                dex.ability(s.ability).name,
                s.item.map_or("", |i| dex.item(i).name),
                &stats(&s.evs),
                &stats(&s.ivs),
                s.tera_type.name(),
                s.role,
            ]
            .join("|")
        })
        .collect::<Vec<_>>()
        .join("]")
}

/// Where Showdown's generator throws. Showdown's battle can't start then;
/// neither can a team be built here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerateError {
    /// Showdown's message (`Cannot sample an empty array`, `Index -1 out
    /// of bounds for given array`, `Could not build a random team for
    /// ...`).
    pub message: String,
}

impl fmt::Display for GenerateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GenerateError {}

/// One PRNG draw: `random(bound)` gave `result` (a `random(from, to)` is
/// `to - from` and the result minus `from`), at `site` (the generator's
/// function and what it drew).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Draw {
    /// The `n` of `random(n)`.
    pub bound: u32,
    /// What it gave, in `0..bound`.
    pub result: u32,
    /// The draw's call site.
    pub site: &'static str,
}

/// A team (`randomTeam`) from `rng`, the PRNG Showdown's generator would
/// have for the player's seed.
pub fn generate(format: RandomBattle, rng: &mut BattleRng) -> Result<Vec<RandomSet>, GenerateError> {
    generator::Generator::new(format, rng, None).random_team()
}

/// [`generate`], recording every draw in `trace`.
pub fn generate_traced(
    format: RandomBattle,
    rng: &mut BattleRng,
    trace: &mut Vec<Draw>,
) -> Result<Vec<RandomSet>, GenerateError> {
    generator::Generator::new(format, rng, Some(trace)).random_team()
}

// ------------------------------------------------------------------ data

/// One species of `sets.json` or `doubles-sets.json`.
#[derive(Debug)]
pub(crate) struct Entry {
    /// Index into [`data::SPECIES`].
    pub species: u16,
    /// `level` (0: none, which `getLevel` makes 80).
    pub level: u8,
    pub sets: &'static [Set],
}

/// One of an entry's sets.
#[derive(Debug)]
pub(crate) struct Set {
    /// Index into [`data::ROLES`].
    pub role: u8,
    pub movepool: &'static [MoveId],
    pub abilities: &'static [AbilityId],
    /// Indices into [`data::TYPES`].
    pub tera_types: &'static [u8],
}

/// A species' `otherFormes`.
#[derive(Debug)]
pub(crate) enum OtherFormes {
    Undefined,
    Ids(&'static [SpeciesId]),
    /// Some are outside the ID space; the generator never samples them.
    Outside,
}

/// The fields of a species the generator reads, as Showdown's `Species`
/// has them.
#[derive(Debug)]
pub(crate) struct SpeciesFacts {
    pub id: SpeciesId,
    pub base_species: SpeciesId,
    /// `types`, as indices into [`data::TYPES`].
    pub types: &'static [u8],
    /// `baseStats`: HP, Attack, Defense, Sp. Atk, Sp. Def, Speed.
    pub base_stats: [u8; 6],
    /// `Object.values(abilities)`.
    pub abilities: &'static [AbilityId],
    /// `gender`: `b'M'`, `b'F'`, `b'N'`, or 0 for none (a ratio).
    pub gender: u8,
    pub nfe: bool,
    /// `requiredItems` (empty: undefined).
    pub required_items: &'static [ItemId],
    pub required_move: Option<MoveId>,
    /// `battleOnly`, when it is one species.
    pub battle_only: Option<SpeciesId>,
    /// `cosmeticFormes` (empty: undefined).
    pub cosmetic_formes: &'static [SpeciesId],
    pub other_formes: OtherFormes,
}

/// The fields of a move the generator reads.
#[derive(Debug)]
pub(crate) struct MoveFacts {
    /// 0 Physical, 1 Special, 2 Status.
    pub category: u8,
    /// Index into [`data::TYPES`].
    pub ty: u8,
    pub base_power: u16,
    pub priority: i8,
    /// 0: `true` (never misses).
    pub accuracy: u8,
    pub flags: MoveFlags,
}

/// JavaScript truthiness tests on a move's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MoveFlags(pub u16);

impl MoveFlags {
    /// `basePowerCallback`.
    pub const BP_CALLBACK: MoveFlags = MoveFlags(1);
    /// `damage || damageCallback`.
    pub const DAMAGE: MoveFlags = MoveFlags(1 << 1);
    /// `multihit` is an array ending in 5.
    pub const MULTIHIT_TO_5: MoveFlags = MoveFlags(1 << 2);
    /// `recoil || hasCrashDamage`.
    pub const RECOIL: MoveFlags = MoveFlags(1 << 3);
    /// `drain`.
    pub const DRAIN: MoveFlags = MoveFlags(1 << 4);
    /// `secondary || hasSheerForceBoost`.
    pub const SECONDARY: MoveFlags = MoveFlags(1 << 5);
    /// `flags.bite`.
    pub const BITE: MoveFlags = MoveFlags(1 << 6);
    /// `flags.punch`.
    pub const PUNCH: MoveFlags = MoveFlags(1 << 7);
    /// `flags.sound`.
    pub const SOUND: MoveFlags = MoveFlags(1 << 8);

    pub const fn has(self, f: MoveFlags) -> bool {
        self.0 & f.0 != 0
    }
}

#[cfg(test)]
mod tests;
