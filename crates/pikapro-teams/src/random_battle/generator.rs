//! The port of `RandomTeams` (`data/random-battles/gen9/teams.ts`), one
//! function per Showdown function, in its order. Comments name Showdown's
//! function; where JavaScript's semantics matter (key order, `Set`
//! insertion order, `fastPop`, short-circuits that skip a draw), the code
//! keeps them.

use pikapro::{BattleRng, Gender, Seed};
use pikapro_data::gen9::{abilities as ab, items as it, moves as mv, species as sp};
use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId, Stats};

use super::data::{self, DOUBLES, EFFECTIVENESS, MOVES, ROLES, SINGLES, SPECIES, STATUS_MOVES};
use super::{Draw, Entry, GenerateError, MoveFacts, MoveFlags, OtherFormes, RandomBattle, RandomSet, SpeciesFacts};

// The type indices (`data::TYPES`, Showdown's `dex.types.names()` order).
const BUG: u8 = 0;
const DARK: u8 = 1;
const DRAGON: u8 = 2;
const ELECTRIC: u8 = 3;
const FAIRY: u8 = 4;
const FIGHTING: u8 = 5;
const FIRE: u8 = 6;
const FLYING: u8 = 7;
const GHOST: u8 = 8;
const GRASS: u8 = 9;
const GROUND: u8 = 10;
const ICE: u8 = 11;
const NORMAL: u8 = 12;
const POISON: u8 = 13;
const PSYCHIC: u8 = 14;
const ROCK: u8 = 15;
const STEEL: u8 = 16;
const STELLAR: u8 = 17;
const WATER: u8 = 18;
const TYPE_COUNT: usize = 19;

const MAX_TEAM_SIZE: usize = 6;
const MAX_MOVE_COUNT: usize = 4;

const RECOVERY_MOVES: &[MoveId] = &[
    mv::HEAL_ORDER,
    mv::MILK_DRINK,
    mv::MOONLIGHT,
    mv::MORNING_SUN,
    mv::RECOVER,
    mv::ROOST,
    mv::SHORE_UP,
    mv::SLACK_OFF,
    mv::SOFT_BOILED,
    mv::STRENGTH_SAP,
    mv::SYNTHESIS,
];
const CONTRARY_MOVES: &[MoveId] = &[
    mv::ARMOR_CANNON,
    mv::CLOSE_COMBAT,
    mv::LEAF_STORM,
    mv::MAKE_IT_RAIN,
    mv::OVERHEAT,
    mv::SPIN_OUT,
    mv::SUPERPOWER,
    mv::V_CREATE,
];
const PHYSICAL_SETUP: &[MoveId] = &[
    mv::BELLY_DRUM,
    mv::BULK_UP,
    mv::COIL,
    mv::CURSE,
    mv::DRAGON_DANCE,
    mv::HONE_CLAWS,
    mv::HOWL,
    mv::MEDITATE,
    mv::POWER_UP_PUNCH,
    mv::SWORDS_DANCE,
    mv::TIDY_UP,
    mv::VICTORY_DANCE,
];
const SPECIAL_SETUP: &[MoveId] = &[
    mv::CALM_MIND,
    mv::CHARGE_BEAM,
    mv::GEOMANCY,
    mv::NASTY_PLOT,
    mv::QUIVER_DANCE,
    mv::TAIL_GLOW,
    mv::TAKE_HEART,
    mv::TORCH_SONG,
];
const MIXED_SETUP: &[MoveId] =
    &[mv::CLANGOROUS_SOUL, mv::GROWTH, mv::HAPPY_HOUR, mv::HOLD_HANDS, mv::NO_RETREAT, mv::SHELL_SMASH, mv::WORK_UP];
const SPEED_SETUP: &[MoveId] =
    &[mv::AGILITY, mv::AUTOTOMIZE, mv::FLAME_CHARGE, mv::ROCK_POLISH, mv::SNOWSCAPE, mv::TRAILBLAZE];
const SETUP: &[MoveId] = &[
    mv::ACID_ARMOR,
    mv::AGILITY,
    mv::AUTOTOMIZE,
    mv::BELLY_DRUM,
    mv::BULK_UP,
    mv::CALM_MIND,
    mv::CLANGOROUS_SOUL,
    mv::COIL,
    mv::COSMIC_POWER,
    mv::CURSE,
    mv::DRAGON_DANCE,
    mv::FLAME_CHARGE,
    mv::GROWTH,
    mv::HONE_CLAWS,
    mv::HOWL,
    mv::IRON_DEFENSE,
    mv::MEDITATE,
    mv::NASTY_PLOT,
    mv::NO_RETREAT,
    mv::POWER_UP_PUNCH,
    mv::QUIVER_DANCE,
    mv::ROCK_POLISH,
    mv::SHELL_SMASH,
    mv::SHELTER,
    mv::SHIFT_GEAR,
    mv::SWORDS_DANCE,
    mv::TAIL_GLOW,
    mv::TAKE_HEART,
    mv::TIDY_UP,
    mv::TRAILBLAZE,
    mv::WORK_UP,
    mv::VICTORY_DANCE,
];
const SPEED_CONTROL: &[MoveId] = &[
    mv::ELECTROWEB,
    mv::GLARE,
    mv::ICY_WIND,
    mv::LOW_SWEEP,
    mv::NUZZLE,
    mv::QUASH,
    mv::TAILWIND,
    mv::THUNDER_WAVE,
    mv::TRICK_ROOM,
];
const NO_STAB: &[MoveId] = &[
    mv::ACID_SPRAY,
    mv::ACCELEROCK,
    mv::AQUA_JET,
    mv::BOUNCE,
    mv::BREAKING_SWIPE,
    mv::BULLET_PUNCH,
    mv::CHATTER,
    mv::CHLOROBLAST,
    mv::CLEAR_SMOG,
    mv::COVET,
    mv::DRAGON_TAIL,
    mv::DOOM_DESIRE,
    mv::ELECTROWEB,
    mv::ERUPTION,
    mv::EXPLOSION,
    mv::FAKE_OUT,
    mv::FEINT,
    mv::FLAME_CHARGE,
    mv::FLIP_TURN,
    mv::FUTURE_SIGHT,
    mv::GRASSY_GLIDE,
    mv::ICE_SHARD,
    mv::ICY_WIND,
    mv::INCINERATE,
    mv::INFESTATION,
    mv::MACH_PUNCH,
    mv::METEOR_BEAM,
    mv::MORTAL_SPIN,
    mv::NUZZLE,
    mv::PLUCK,
    mv::PURSUIT,
    mv::QUICK_ATTACK,
    mv::RAPID_SPIN,
    mv::REVERSAL,
    mv::SELF_DESTRUCT,
    mv::SHADOW_SNEAK,
    mv::SKY_DROP,
    mv::SNARL,
    mv::STRUGGLE_BUG,
    mv::SUCKER_PUNCH,
    mv::TRAILBLAZE,
    mv::U_TURN,
    mv::VACUUM_WAVE,
    mv::VOLT_SWITCH,
    mv::WATER_SHURIKEN,
    mv::WATER_SPOUT,
];
const HAZARDS: &[MoveId] = &[mv::SPIKES, mv::STEALTH_ROCK, mv::STICKY_WEB, mv::TOXIC_SPIKES];
const PROTECT_MOVES: &[MoveId] =
    &[mv::BANEFUL_BUNKER, mv::BURNING_BULWARK, mv::PROTECT, mv::SILK_TRAP, mv::SPIKY_SHIELD];
const PIVOT_MOVES: &[MoveId] =
    &[mv::CHILLY_RECEPTION, mv::FLIP_TURN, mv::PARTING_SHOT, mv::SHED_TAIL, mv::TELEPORT, mv::U_TURN, mv::VOLT_SWITCH];
const MOVE_PAIRS: &[[MoveId; 2]] = &[
    [mv::LIGHT_SCREEN, mv::REFLECT],
    [mv::SLEEP_TALK, mv::REST],
    [mv::PROTECT, mv::WISH],
    [mv::LEECH_SEED, mv::PROTECT],
    [mv::LEECH_SEED, mv::SUBSTITUTE],
];
const PRIORITY_POKEMON: &[SpeciesId] =
    &[sp::BRELOOM, sp::BRUTE_BONNET, sp::CACTURNE, sp::HONCHKROW, sp::MIMIKYU, sp::RAGING_BOLT, sp::SCIZOR];
const NO_LEAD_POKEMON: &[SpeciesId] = &[
    sp::DUGTRIO,
    sp::GOTHITELLE,
    sp::IRON_THORNS,
    sp::KINGAMBIT,
    sp::ZACIAN,
    sp::ZACIAN_CROWNED,
    sp::ZAMAZENTA,
    sp::ZAMAZENTA_CROWNED,
];
const DOUBLES_NO_LEAD_POKEMON: &[SpeciesId] = &[
    sp::BASCULEGION,
    sp::BASCULEGION_F,
    sp::DUGTRIO,
    sp::GOTHITELLE,
    sp::HOUNDSTONE,
    sp::IRON_BUNDLE,
    sp::ROARING_MOON,
    sp::ZACIAN,
    sp::ZACIAN_CROWNED,
    sp::ZAMAZENTA,
    sp::ZAMAZENTA_CROWNED,
];
const DEFENSIVE_TERA_BLAST_USERS: &[SpeciesId] =
    &[sp::ALCREMIE, sp::BELLOSSOM, sp::COMFEY, sp::FEZANDIPITI, sp::FLORGES];

fn error(message: impl Into<String>) -> GenerateError {
    GenerateError { message: message.into() }
}

fn facts(m: MoveId) -> &'static MoveFacts {
    &MOVES[m.index()]
}

fn base_power(m: &MoveFacts) -> bool {
    m.base_power != 0 || m.flags.has(MoveFlags::BP_CALLBACK)
}

/// `fastPop(list, list.indexOf(x))`, which throws on -1.
fn fast_pop_value(list: &mut Vec<MoveId>, x: MoveId) -> Result<MoveId, GenerateError> {
    match list.iter().position(|&m| m == x) {
        Some(i) => Ok(list.swap_remove(i)),
        None => Err(error("Index -1 out of bounds for given array")),
    }
}

/// `if (list.includes(x)) fastPop(list, list.indexOf(x))`.
fn pop_if_present(list: &mut Vec<MoveId>, x: MoveId) {
    if let Some(i) = list.iter().position(|&m| m == x) {
        list.swap_remove(i);
    }
}

/// `dex.getEffectiveness(type, species)`: the sum over the species' types.
fn effectiveness(attack: u8, species: &SpeciesFacts) -> i32 {
    species.types.iter().map(|&d| EFFECTIVENESS[attack as usize][d as usize] as i32).sum()
}

/// `dex.getEffectiveness(type, typeName)`.
fn effectiveness_on_type(attack: u8, defender: u8) -> i32 {
    EFFECTIVENESS[attack as usize][defender as usize] as i32
}

/// A `Set` of move IDs: insertion order, no duplicates.
#[derive(Clone, Debug, Default)]
struct Moves(Vec<MoveId>);

impl Moves {
    fn add(&mut self, m: MoveId) {
        if !self.0.contains(&m) {
            self.0.push(m);
        }
    }

    fn has(&self, m: MoveId) -> bool {
        self.0.contains(&m)
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

/// `MoveCounter`'s keys other than the type names.
#[derive(Clone, Copy)]
enum Key {
    Damage,
    Technician,
    SkillLink,
    Recoil,
    Drain,
    Stab,
    StabTera,
    StrongJaw,
    IronFist,
    Sound,
    Priority,
    SheerForce,
    Inaccurate,
    Recovery,
    Contrary,
    PhysicalSetup,
    SpecialSetup,
    MixedSetup,
    SpeedSetup,
    SpeedControl,
    Setup,
    Hazards,
    Physical,
    Special,
    Status,
}

const KEYS: usize = Key::Status as usize + 1;

/// `MoveCounter` (a `Multiset` whose `get` of a missing key is 0), with
/// `damagingMoves` (a `Set` of move objects, by the move data they are).
#[derive(Clone, Debug, Default)]
struct Counter {
    keys: [u32; KEYS],
    types: [u32; TYPE_COUNT],
    damaging_moves: Vec<MoveId>,
}

impl Counter {
    fn get(&self, k: Key) -> u32 {
        self.keys[k as usize]
    }

    fn add(&mut self, k: Key) {
        self.keys[k as usize] += 1;
    }

    fn ty(&self, t: u8) -> u32 {
        self.types[t as usize]
    }

    fn add_damaging(&mut self, m: MoveId) {
        if !self.damaging_moves.contains(&m) {
            self.damaging_moves.push(m);
        }
    }
}

/// `teamDetails`.
#[derive(Clone, Copy, Debug, Default)]
struct TeamDetails {
    rain: bool,
    sun: bool,
    sand: bool,
    snow: bool,
    status_cure: bool,
    spikes: u32,
    toxic_spikes: bool,
    stealth_rock: bool,
    sticky_web: bool,
    defog: bool,
    rapid_spin: bool,
    screens: bool,
    tera_blast: bool,
}

/// A set's role, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Role(&'static str);

impl Role {
    fn is(self, name: &str) -> bool {
        self.0 == name
    }

    fn any(self, names: &[&str]) -> bool {
        names.contains(&self.0)
    }

    fn contains(self, part: &str) -> bool {
        self.0.contains(part)
    }
}

/// What `getPriorityItem` returns: `undefined`, `""` or an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Undefined,
    Item(Option<ItemId>),
}

impl Pick {
    const fn item(i: ItemId) -> Pick {
        Pick::Item(Some(i))
    }
}

/// The moveset's context: what `randomMoveset` passes every helper.
struct Ctx<'s> {
    types: &'s [u8],
    abilities: &'static [AbilityId],
    td: TeamDetails,
    species: &'static SpeciesFacts,
    is_lead: bool,
    tera: u8,
    role: Role,
    doubles: bool,
}

impl Ctx<'_> {
    fn has_type(&self, t: u8) -> bool {
        self.types.contains(&t)
    }

    fn has_ability(&self, a: AbilityId) -> bool {
        self.abilities.contains(&a)
    }

    fn atk(&self) -> u8 {
        self.species.base_stats[1]
    }
}

pub(crate) struct Generator<'a> {
    doubles: bool,
    table: &'static [Entry],
    rng: &'a mut BattleRng,
    trace: Option<&'a mut Vec<Draw>>,
}

impl<'a> Generator<'a> {
    pub(crate) fn new(format: RandomBattle, rng: &'a mut BattleRng, trace: Option<&'a mut Vec<Draw>>) -> Self {
        let doubles = format == RandomBattle::Doubles;
        Generator { doubles, table: if doubles { &DOUBLES } else { &SINGLES }, rng, trace }
    }

    // ------------------------------------------------------------ PRNG

    /// `prng.random(n)`.
    fn random(&mut self, n: u32, site: &'static str) -> u32 {
        let result = self.rng.random(n);
        if let Some(t) = self.trace.as_deref_mut() {
            t.push(Draw { bound: n, result, site });
        }
        result
    }

    /// `randomChance(numerator, denominator)`.
    fn random_chance(&mut self, numerator: u32, denominator: u32, site: &'static str) -> bool {
        self.random(denominator, site) < numerator
    }

    /// `sample(items)`, which throws on an empty array.
    fn sample<T: Copy>(&mut self, items: &[T], site: &'static str) -> Result<T, GenerateError> {
        if items.is_empty() {
            return Err(error("Cannot sample an empty array"));
        }
        Ok(items[self.random(items.len() as u32, site) as usize])
    }

    /// `sampleNoReplace(list)`: a random element, removed with `fastPop`.
    fn sample_no_replace<T: Copy>(&mut self, list: &mut Vec<T>, site: &'static str) -> Option<T> {
        if list.is_empty() {
            return None;
        }
        let i = self.random(list.len() as u32, site) as usize;
        Some(list.swap_remove(i))
    }

    /// `prng.shuffle(items)`: Fisher-Yates from the front, `random(i, len)`.
    fn shuffle<T>(&mut self, items: &mut [T], site: &'static str) {
        let end = items.len() as u32;
        let mut start = 0;
        while start + 1 < end {
            let next = start + self.random(end - start, site);
            items.swap(start as usize, next as usize);
            start += 1;
        }
    }

    // ------------------------------------------------------- queryMoves

    /// `getMoveType`: the type a move counts as for STAB and coverage.
    fn move_type(&self, m: MoveId, ctx: &Ctx<'_>) -> u8 {
        let move_ = facts(m);
        if m == mv::TERA_BLAST {
            return ctx.tera;
        }
        if [mv::JUDGMENT, mv::MULTI_ATTACK, mv::REVELATION_DANCE].contains(&m) {
            return ctx.species.types[0];
        }
        let id = ctx.species.id;
        if m == mv::RAGING_BULL {
            match id {
                sp::TAUROS_PALDEA_COMBAT => return FIGHTING,
                sp::TAUROS_PALDEA_BLAZE => return FIRE,
                sp::TAUROS_PALDEA_AQUA => return WATER,
                _ => {}
            }
        }
        if m == mv::IVY_CUDGEL {
            match id {
                sp::OGERPON_WELLSPRING => return WATER,
                sp::OGERPON_HEARTHFLAME => return FIRE,
                sp::OGERPON_CORNERSTONE => return ROCK,
                _ => {}
            }
        }
        if move_.ty == NORMAL {
            if ctx.has_ability(ab::AERILATE) {
                return FLYING;
            }
            if ctx.has_ability(ab::GALVANIZE) {
                return ELECTRIC;
            }
            if ctx.has_ability(ab::PIXILATE) {
                return FAIRY;
            }
            if ctx.has_ability(ab::REFRIGERATE) {
                return ICE;
            }
        }
        move_.ty
    }

    /// `queryMoves`.
    fn query_moves(&self, moves: &Moves, ctx: &Ctx<'_>) -> Counter {
        let mut counter = Counter::default();
        if moves.size() == 0 {
            return counter;
        }
        let mut categories = [0u32; 3];
        for &moveid in &moves.0 {
            // Gen 6+: Nature Power counts as Tri Attack.
            let data_id = if moveid == mv::NATURE_POWER { mv::TRI_ATTACK } else { moveid };
            let move_ = facts(data_id);
            let move_type = self.move_type(data_id, ctx);
            if move_.flags.has(MoveFlags::DAMAGE) {
                counter.add(Key::Damage);
                counter.add_damaging(data_id);
            } else {
                categories[move_.category as usize] += 1;
            }
            if moveid == mv::LOW_KICK
                || (move_.base_power != 0 && move_.base_power <= 60 && ![mv::NUZZLE, mv::RAPID_SPIN].contains(&moveid))
            {
                counter.add(Key::Technician);
            }
            if move_.flags.has(MoveFlags::MULTIHIT_TO_5) {
                counter.add(Key::SkillLink);
            }
            if move_.flags.has(MoveFlags::RECOIL) {
                counter.add(Key::Recoil);
            }
            if move_.flags.has(MoveFlags::DRAIN) {
                counter.add(Key::Drain);
            }
            if base_power(move_) {
                if !NO_STAB.contains(&moveid) || (PRIORITY_POKEMON.contains(&ctx.species.id) && move_.priority > 0) {
                    counter.types[move_type as usize] += 1;
                    if ctx.has_type(move_type) {
                        counter.add(Key::Stab);
                    }
                    if ctx.tera == move_type {
                        counter.add(Key::StabTera);
                    }
                    counter.add_damaging(data_id);
                }
                if move_.flags.has(MoveFlags::BITE) {
                    counter.add(Key::StrongJaw);
                }
                if move_.flags.has(MoveFlags::PUNCH) {
                    counter.add(Key::IronFist);
                }
                if move_.flags.has(MoveFlags::SOUND) {
                    counter.add(Key::Sound);
                }
                if move_.priority > 0 || (moveid == mv::GRASSY_GLIDE && ctx.has_ability(ab::GRASSY_SURGE)) {
                    counter.add(Key::Priority);
                }
            }
            if move_.flags.has(MoveFlags::SECONDARY) {
                counter.add(Key::SheerForce);
            }
            if move_.accuracy != 0 && move_.accuracy < 90 {
                counter.add(Key::Inaccurate);
            }
            for (list, key) in [
                (RECOVERY_MOVES, Key::Recovery),
                (CONTRARY_MOVES, Key::Contrary),
                (PHYSICAL_SETUP, Key::PhysicalSetup),
                (SPECIAL_SETUP, Key::SpecialSetup),
                (MIXED_SETUP, Key::MixedSetup),
                (SPEED_SETUP, Key::SpeedSetup),
                (SPEED_CONTROL, Key::SpeedControl),
                (SETUP, Key::Setup),
                (HAZARDS, Key::Hazards),
            ] {
                if list.contains(&moveid) {
                    counter.add(key);
                }
            }
        }
        counter.keys[Key::Physical as usize] = categories[0];
        counter.keys[Key::Special as usize] = categories[1];
        counter.keys[Key::Status as usize] = categories[2];
        counter
    }

    // ----------------------------------------------------- cullMovePool

    /// `cullMovePool`.
    fn cull_move_pool(&mut self, moves: &Moves, pool: &mut Vec<MoveId>, ctx: &Ctx<'_>) -> Result<(), GenerateError> {
        let max = MAX_MOVE_COUNT;
        if moves.size() + pool.len() <= max {
            return Ok(());
        }
        if moves.size() == max - 2 {
            let mut unpaired = pool.clone();
            for pair in MOVE_PAIRS {
                if pool.contains(&pair[0]) && pool.contains(&pair[1]) {
                    fast_pop_value(&mut unpaired, pair[0])?;
                    fast_pop_value(&mut unpaired, pair[1])?;
                }
            }
            if unpaired.len() == 1 {
                fast_pop_value(pool, unpaired[0])?;
            }
        }
        if moves.size() == max - 1 {
            for pair in MOVE_PAIRS {
                if pool.contains(&pair[0]) && pool.contains(&pair[1]) {
                    fast_pop_value(pool, pair[0])?;
                    fast_pop_value(pool, pair[1])?;
                }
            }
        }
        let td = ctx.td;
        let done = |moves: &Moves, pool: &Vec<MoveId>| moves.size() + pool.len() <= max;
        if td.screens {
            if pool.contains(&mv::AURORA_VEIL) && !ctx.doubles {
                pop_if_present(pool, mv::AURORA_VEIL);
            }
            if pool.len() >= max + 2 {
                pop_if_present(pool, mv::REFLECT);
                pop_if_present(pool, mv::LIGHT_SCREEN);
            }
        }
        if td.sticky_web {
            pop_if_present(pool, mv::STICKY_WEB);
            if done(moves, pool) {
                return Ok(());
            }
        }
        if td.stealth_rock {
            pop_if_present(pool, mv::STEALTH_ROCK);
            if done(moves, pool) {
                return Ok(());
            }
        }
        if td.defog || td.rapid_spin {
            pop_if_present(pool, mv::DEFOG);
            pop_if_present(pool, mv::RAPID_SPIN);
            if done(moves, pool) {
                return Ok(());
            }
        }
        if td.toxic_spikes {
            pop_if_present(pool, mv::TOXIC_SPIKES);
            if done(moves, pool) {
                return Ok(());
            }
        }
        if td.spikes >= 2 {
            pop_if_present(pool, mv::SPIKES);
            if done(moves, pool) {
                return Ok(());
            }
        }
        if td.status_cure {
            pop_if_present(pool, mv::HEAL_BELL);
            if done(moves, pool) {
                return Ok(());
            }
        }
        if ctx.doubles {
            // In order of decreasing generalizability.
            let pairs: &[(&[MoveId], &[MoveId])] = &[
                (SPEED_CONTROL, SPEED_CONTROL),
                (HAZARDS, HAZARDS),
                (&[mv::ROCK_SLIDE], &[mv::STONE_EDGE]),
                (SETUP, &[mv::FAKE_OUT, mv::HELPING_HAND]),
                (PROTECT_MOVES, &[mv::WIDE_GUARD]),
                (&[mv::FIERY_DANCE, mv::FIRE_BLAST], &[mv::HEAT_WAVE]),
                (&[mv::DAZZLING_GLEAM], &[mv::FLEUR_CANNON, mv::MOONBLAST]),
                (&[mv::POISON_GAS], &[mv::TOXIC_SPIKES, mv::WILL_O_WISP]),
                (RECOVERY_MOVES, &[mv::HEAL_PULSE, mv::LIFE_DEW]),
                (&[mv::HEAL_PULSE], &[mv::LIFE_DEW]),
                (&[mv::HAZE], &[mv::ICY_WIND]),
                (&[mv::HYDRO_PUMP, mv::MUDDY_WATER], &[mv::MUDDY_WATER, mv::SCALD]),
                (&[mv::DISABLE], &[mv::ENCORE]),
                (&[mv::FREEZE_DRY], &[mv::ICE_BEAM]),
                (&[mv::ENERGY_BALL], &[mv::LEAF_STORM]),
                (&[mv::EARTH_POWER], &[mv::SANDSEAR_STORM]),
                (PROTECT_MOVES, PROTECT_MOVES),
                (&[mv::COACHING], &[mv::HELPING_HAND, mv::HOWL, mv::POLLEN_PUFF]),
            ];
            for &(a, b) in pairs {
                incompatible_moves(moves, pool, a, b);
            }
            if !ctx.role.is("Offensive Protect") {
                incompatible_moves(moves, pool, PROTECT_MOVES, &[mv::FLIP_TURN, mv::U_TURN]);
            }
        }
        let pairs: &[(&[MoveId], &[MoveId])] = &[
            // These moves don't mesh well with other aspects of the set
            (&STATUS_MOVES, &[mv::HEALING_WISH, mv::SWITCHEROO, mv::TRICK]),
            (SETUP, PIVOT_MOVES),
            (SETUP, HAZARDS),
            (SETUP, &[mv::DEFOG, mv::NUZZLE, mv::TOXIC, mv::YAWN, mv::HAZE]),
            (PHYSICAL_SETUP, PHYSICAL_SETUP),
            (&[mv::SUBSTITUTE], PIVOT_MOVES),
            (SPEED_SETUP, &[mv::AQUA_JET, mv::REST, mv::TRICK_ROOM]),
            (&[mv::CURSE], &[mv::IRON_DEFENSE, mv::RAPID_SPIN]),
            (&[mv::DRAGON_DANCE], &[mv::DRACO_METEOR]),
            (&[mv::YAWN], &[mv::ROAR]),
            (&[mv::TRICK], &[mv::U_TURN]),
            // These attacks are redundant with each other
            (&[mv::PSYCHIC, mv::PSYCHIC_NOISE], &[mv::PSYSHOCK, mv::PSYCHIC_NOISE]),
            (&[mv::SURF], &[mv::HYDRO_PUMP]),
            (&[mv::LIQUIDATION], &[mv::WAVE_CRASH]),
            (&[mv::AQUA_JET], &[mv::FLIP_TURN]),
            (&[mv::GIGA_DRAIN], &[mv::LEAF_STORM]),
            (&[mv::POWER_WHIP], &[mv::HORN_LEECH]),
            (&[mv::AIR_SLASH], &[mv::HURRICANE]),
            (&[mv::KNOCK_OFF], &[mv::FOUL_PLAY]),
            (&[mv::THROAT_CHOP], &[mv::CRUNCH, mv::LASH_OUT]),
            (&[mv::DOUBLE_EDGE], &[mv::BODY_SLAM, mv::HEADBUTT]),
            (&[mv::FIRE_BLAST, mv::MAGMA_STORM], &[mv::FIERY_DANCE, mv::FLAMETHROWER, mv::LAVA_PLUME]),
            (&[mv::THUNDER_PUNCH], &[mv::WILD_CHARGE]),
            (&[mv::THUNDERBOLT], &[mv::DISCHARGE]),
            (&[mv::GUNK_SHOT], &[mv::DIRE_CLAW, mv::POISON_JAB, mv::SLUDGE_BOMB]),
            (&[mv::AURA_SPHERE], &[mv::FOCUS_BLAST]),
            (&[mv::CLOSE_COMBAT], &[mv::DRAIN_PUNCH]),
            (&[mv::DRAGON_PULSE, mv::SPACIAL_REND], &[mv::DRACO_METEOR]),
            (&[mv::DRAGON_CLAW], &[mv::OUTRAGE]),
            (&[mv::HEAVY_SLAM], &[mv::FLASH_CANNON]),
            (&[mv::ALLURING_VOICE], &[mv::DAZZLING_GLEAM]),
            // These status moves are redundant with each other
            (&[mv::TAUNT], &[mv::DISABLE]),
            (&[mv::THUNDER_WAVE, mv::TOXIC], &[mv::THUNDER_WAVE, mv::WILL_O_WISP]),
            (&[mv::THUNDER_WAVE, mv::TOXIC, mv::WILL_O_WISP], &[mv::TOXIC_SPIKES]),
            // Assorted hardcodes: Landorus and Thundurus
            (&[mv::NASTY_PLOT], &[mv::ROCK_SLIDE, mv::KNOCK_OFF]),
            // Persian
            (&[mv::SWITCHEROO], &[mv::FAKE_OUT]),
            // Amoonguss
            (&[mv::TOXIC], &[mv::CLEAR_SMOG]),
            // Chansey and Blissey
            (&[mv::HEAL_BELL], &[mv::STEALTH_ROCK]),
            // Araquanid and Magnezone
            (&[mv::MIRROR_COAT], &[mv::HYDRO_PUMP, mv::BODY_PRESS]),
        ];
        for &(a, b) in pairs {
            incompatible_moves(moves, pool, a, b);
        }
        if !ctx.has_type(ICE) {
            incompatible_moves(moves, pool, &[mv::ICE_BEAM], &[mv::ICY_WIND]);
        }
        if !ctx.doubles {
            incompatible_moves(moves, pool, &[mv::TAUNT], &[mv::ENCORE]);
        }
        if !ctx.has_type(DARK) && ctx.tera != DARK {
            incompatible_moves(moves, pool, &[mv::KNOCK_OFF], &[mv::SUCKER_PUNCH]);
        }
        if !ctx.has_ability(ab::PRANKSTER) {
            incompatible_moves(moves, pool, &[mv::THUNDER_WAVE], &[mv::YAWN]);
        }
        let id = ctx.species.id;
        if id == sp::BARRASKEWDA {
            incompatible_moves(moves, pool, &[mv::PSYCHIC_FANGS, mv::THROAT_CHOP], &[mv::POISON_JAB, mv::THROAT_CHOP]);
        }
        if id == sp::QUAGSIRE {
            incompatible_moves(moves, pool, &[mv::SPIKES], &[mv::ICE_BEAM]);
        }
        if id == sp::CYCLIZAR {
            incompatible_moves(moves, pool, &[mv::TAUNT], &[mv::KNOCK_OFF]);
        }
        if id == sp::COALOSSAL {
            incompatible_moves(moves, pool, &[mv::FLAMETHROWER], &[mv::OVERHEAT]);
        }
        Ok(())
    }

    /// `addMove`: adds the move, takes it out of the pool and culls it.
    fn add_move(
        &mut self,
        m: MoveId,
        moves: &mut Moves,
        pool: &mut Vec<MoveId>,
        ctx: &Ctx<'_>,
    ) -> Result<Counter, GenerateError> {
        moves.add(m);
        fast_pop_value(pool, m)?;
        let counter = self.query_moves(moves, ctx);
        self.cull_move_pool(moves, pool, ctx)?;
        Ok(counter)
    }

    // ---------------------------------------------------- randomMoveset

    /// `moveEnforcementCheckers[type]`: whether a STAB move of `ty` is
    /// still wanted.
    fn enforcement(&self, ty: u8, pool: &[MoveId], counter: &Counter, ctx: &Ctx<'_>) -> bool {
        let in_pool = |m: MoveId| pool.contains(&m);
        let none = |t: u8| counter.ty(t) == 0;
        match ty {
            BUG => {
                in_pool(mv::MEGAHORN)
                    || in_pool(mv::X_SCISSOR)
                    || (none(BUG) && (ctx.has_type(ELECTRIC) || ctx.has_type(PSYCHIC)))
            }
            DARK => {
                if counter.ty(DARK) < 2 && PRIORITY_POKEMON.contains(&ctx.species.id) && ctx.role.is("Wallbreaker") {
                    return true;
                }
                none(DARK)
            }
            DRAGON | ELECTRIC | FAIRY | FIGHTING | FIRE | FLYING | GHOST | GROUND => none(ty),
            GRASS => {
                none(GRASS)
                    && (in_pool(mv::LEAF_STORM)
                        || ctx.atk() >= 100
                        || ctx.has_type(ELECTRIC)
                        || ctx.has_ability(ab::SEED_SOWER))
            }
            ICE => in_pool(mv::FREEZE_DRY) || in_pool(mv::BLIZZARD) || none(ICE),
            NORMAL => in_pool(mv::BOOMBURST) || in_pool(mv::HYPER_VOICE),
            POISON => {
                if ctx.has_type(GROUND) {
                    return false;
                }
                none(POISON)
            }
            PSYCHIC => {
                if (ctx.doubles || ctx.species.id == sp::BRUXISH) && in_pool(mv::PSYCHIC_FANGS) {
                    return true;
                }
                if ctx.species.id == sp::HOOPA_UNBOUND && in_pool(mv::PSYCHIC) {
                    return true;
                }
                if [DARK, STEEL, WATER].iter().any(|&t| ctx.has_type(t)) {
                    return false;
                }
                none(PSYCHIC)
            }
            ROCK => none(ROCK) && ctx.atk() >= 80,
            STEEL => {
                none(STEEL)
                    && (ctx.doubles || ctx.atk() >= 90 || in_pool(mv::GIGATON_HAMMER) || in_pool(mv::MAKE_IT_RAIN))
            }
            WATER => none(WATER) && (!ctx.has_type(GROUND) || ctx.doubles),
            // No checker (Stellar).
            _ => false,
        }
    }

    /// `randomMoveset`.
    fn random_moveset(&mut self, pool: &mut Vec<MoveId>, ctx: &Ctx<'_>) -> Result<Moves, GenerateError> {
        let mut moves = Moves::default();
        let mut counter = self.query_moves(&moves, ctx);
        self.cull_move_pool(&moves, pool, ctx)?;
        if pool.len() <= MAX_MOVE_COUNT {
            for &m in pool.iter() {
                moves.add(m);
            }
            return Ok(moves);
        }
        let role = ctx.role;
        let species = ctx.species;
        let spe = species.base_stats[5];

        // Moves that a role or a species always gets.
        if role.is("Tera Blast user") {
            counter = self.add_move(mv::TERA_BLAST, &mut moves, pool, ctx)?;
        }
        if let Some(m) = species.required_move {
            counter = self.add_move(m, &mut moves, pool, ctx)?;
        }
        if pool.contains(&mv::FACADE) && ctx.has_ability(ab::GUTS) {
            counter = self.add_move(mv::FACADE, &mut moves, pool, ctx)?;
        }
        for m in [mv::NIGHT_SHADE, mv::REVELATION_DANCE, mv::REVIVAL_BLESSING, mv::STICKY_WEB] {
            if pool.contains(&m) {
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }
        if pool.contains(&mv::TRICK_ROOM) && role.is("Doubles Wallbreaker") {
            counter = self.add_move(mv::TRICK_ROOM, &mut moves, pool, ctx)?;
        }
        if role.is("Bulky Support") && !ctx.td.defog && !ctx.td.rapid_spin {
            if pool.contains(&mv::RAPID_SPIN) {
                counter = self.add_move(mv::RAPID_SPIN, &mut moves, pool, ctx)?;
            }
            if pool.contains(&mv::DEFOG) {
                counter = self.add_move(mv::DEFOG, &mut moves, pool, ctx)?;
            }
        }
        if [mv::DRAGON_TAIL, mv::ROAR, mv::WHIRLWIND].iter().any(|m| pool.contains(m))
            && spe <= 60
            && pool.contains(&mv::STEALTH_ROCK)
            && !ctx.td.stealth_rock
        {
            counter = self.add_move(mv::STEALTH_ROCK, &mut moves, pool, ctx)?;
        }
        if !ctx.td.screens && pool.contains(&mv::AURORA_VEIL) {
            counter = self.add_move(mv::AURORA_VEIL, &mut moves, pool, ctx)?;
        }
        if !ctx.doubles
            && ctx.types.len() == 1
            && (ctx.has_type(NORMAL) || ctx.has_type(FIGHTING))
            && pool.contains(&mv::KNOCK_OFF)
        {
            counter = self.add_move(mv::KNOCK_OFF, &mut moves, pool, ctx)?;
        }
        if species.id == sp::SMEARGLE && pool.contains(&mv::SPORE) {
            counter = self.add_move(mv::SPORE, &mut moves, pool, ctx)?;
        }
        if ctx.doubles {
            for m in [mv::MORTAL_SPIN, mv::SPORE] {
                if pool.contains(&m) {
                    counter = self.add_move(m, &mut moves, pool, ctx)?;
                }
            }
            if pool.contains(&mv::FAKE_OUT) && spe <= 50 {
                counter = self.add_move(mv::FAKE_OUT, &mut moves, pool, ctx)?;
            }
            if pool.contains(&mv::TAILWIND) && (ctx.has_ability(ab::PRANKSTER) || ctx.has_ability(ab::GALE_WINGS)) {
                counter = self.add_move(mv::TAILWIND, &mut moves, pool, ctx)?;
            }
        }

        // Priority STAB.
        if role.any(&["Bulky Attacker", "Bulky Setup", "Wallbreaker", "Doubles Wallbreaker"])
            || PRIORITY_POKEMON.contains(&species.id)
        {
            let priority: Vec<MoveId> = pool
                .iter()
                .copied()
                .filter(|&m| {
                    let move_ = facts(m);
                    ctx.has_type(self.move_type(m, ctx))
                        && (move_.priority > 0 || (m == mv::GRASSY_GLIDE && ctx.has_ability(ab::GRASSY_SURGE)))
                        && base_power(move_)
                })
                .collect();
            if !priority.is_empty() {
                let m = self.sample(&priority, "randomMoveset priority STAB")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }

        // STAB for each of the species' types.
        for &ty in ctx.types {
            let mut stab: Vec<MoveId> = pool
                .iter()
                .copied()
                .filter(|&m| !NO_STAB.contains(&m) && base_power(facts(m)) && self.move_type(m, ctx) == ty)
                .collect();
            while self.enforcement(ty, pool, &counter, ctx) {
                let Some(m) = self.sample_no_replace(&mut stab, "randomMoveset type STAB") else { break };
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }

        // Tera STAB.
        if counter.get(Key::StabTera) == 0 && !role.any(&["Bulky Support", "Doubles Support"]) {
            let stab: Vec<MoveId> = pool
                .iter()
                .copied()
                .filter(|&m| !NO_STAB.contains(&m) && base_power(facts(m)) && ctx.tera == self.move_type(m, ctx))
                .collect();
            if !stab.is_empty() {
                let m = self.sample(&stab, "randomMoveset Tera STAB")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }

        // Any STAB.
        if counter.get(Key::Stab) == 0 {
            let stab: Vec<MoveId> = pool
                .iter()
                .copied()
                .filter(|&m| !NO_STAB.contains(&m) && base_power(facts(m)) && ctx.has_type(self.move_type(m, ctx)))
                .collect();
            if !stab.is_empty() {
                let m = self.sample(&stab, "randomMoveset STAB")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }

        if role.any(&["Bulky Support", "Bulky Attacker", "Bulky Setup"]) {
            let recovery: Vec<MoveId> = pool.iter().copied().filter(|m| RECOVERY_MOVES.contains(m)).collect();
            if !recovery.is_empty() {
                let m = self.sample(&recovery, "randomMoveset recovery")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }
        if role.is("AV Pivot") {
            let pivots: Vec<MoveId> =
                pool.iter().copied().filter(|m| [mv::U_TURN, mv::VOLT_SWITCH].contains(m)).collect();
            if !pivots.is_empty() {
                let m = self.sample(&pivots, "randomMoveset pivot")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }
        if role.contains("Setup") || role.is("Tera Blast user") {
            let non_speed: Vec<MoveId> =
                pool.iter().copied().filter(|m| SETUP.contains(m) && !SPEED_SETUP.contains(m)).collect();
            if !non_speed.is_empty() {
                let m = self.sample(&non_speed, "randomMoveset setup")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            } else {
                let setup: Vec<MoveId> = pool.iter().copied().filter(|m| SETUP.contains(m)).collect();
                if !setup.is_empty() {
                    let m = self.sample(&setup, "randomMoveset speed setup")?;
                    counter = self.add_move(m, &mut moves, pool, ctx)?;
                }
            }
        }
        if role.is("Doubles Support") {
            for m in [mv::FAKE_OUT, mv::FOLLOW_ME, mv::RAGE_POWDER] {
                if pool.contains(&m) {
                    counter = self.add_move(m, &mut moves, pool, ctx)?;
                }
            }
            let control: Vec<MoveId> = pool.iter().copied().filter(|m| SPEED_CONTROL.contains(m)).collect();
            if !control.is_empty() {
                let m = self.sample(&control, "randomMoveset speed control")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }
        if role.contains("Protect") {
            let protect: Vec<MoveId> = pool.iter().copied().filter(|m| PROTECT_MOVES.contains(m)).collect();
            if !protect.is_empty() {
                let m = self.sample(&protect, "randomMoveset protect")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }

        // An attack, if there is none.
        if counter.damaging_moves.is_empty() {
            let attacks: Vec<MoveId> =
                pool.iter().copied().filter(|&m| !NO_STAB.contains(&m) && facts(m).category != 2).collect();
            if !attacks.is_empty() {
                let m = self.sample(&attacks, "randomMoveset attack")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }

        // Coverage, if there is only one attack.
        if !role.any(&["AV Pivot", "Fast Support", "Bulky Support", "Bulky Protect", "Doubles Support"])
            && counter.damaging_moves.len() == 1
        {
            // `damagingMoves.values().next().value.type`: the move's own type.
            let current = facts(counter.damaging_moves[0]).ty;
            let coverage: Vec<MoveId> = pool
                .iter()
                .copied()
                .filter(|&m| !NO_STAB.contains(&m) && base_power(facts(m)) && current != self.move_type(m, ctx))
                .collect();
            if !coverage.is_empty() {
                let m = self.sample(&coverage, "randomMoveset coverage")?;
                counter = self.add_move(m, &mut moves, pool, ctx)?;
            }
        }

        // The rest at random, with their pairs.
        while moves.size() < MAX_MOVE_COUNT && !pool.is_empty() {
            if moves.size() + pool.len() <= MAX_MOVE_COUNT {
                for &m in pool.iter() {
                    moves.add(m);
                }
                break;
            }
            let m = self.sample(pool, "randomMoveset filler")?;
            counter = self.add_move(m, &mut moves, pool, ctx)?;
            for pair in MOVE_PAIRS {
                if m == pair[0] && pool.contains(&pair[1]) {
                    counter = self.add_move(pair[1], &mut moves, pool, ctx)?;
                }
                if m == pair[1] && pool.contains(&pair[0]) {
                    counter = self.add_move(pair[0], &mut moves, pool, ctx)?;
                }
            }
        }
        let _ = counter;
        Ok(moves)
    }

    // ------------------------------------------------------- abilities

    /// `shouldCullAbility`.
    fn should_cull_ability(&self, ability: AbilityId, moves: &Moves, counter: &Counter, ctx: &Ctx<'_>) -> bool {
        let td = ctx.td;
        match ability {
            // Abilities which are primarily useful for certain moves or with team support
            ab::CHLOROPHYLL | ab::SOLAR_POWER => !td.sun,
            ab::DEFIANT => ctx.species.id == sp::THUNDURUS && counter.get(Key::Status) != 0,
            ab::HYDRATION | ab::SWIFT_SWIM => !td.rain,
            ab::IRON_FIST => counter.get(Key::IronFist) == 0,
            ab::SKILL_LINK => counter.get(Key::SkillLink) == 0,
            ab::OVERGROW => counter.ty(GRASS) == 0,
            ab::PRANKSTER => counter.get(Key::Status) == 0,
            ab::SAND_FORCE | ab::SAND_RUSH => !td.sand,
            ab::SLUSH_RUSH => !td.snow,
            ab::SWARM => counter.ty(BUG) == 0,
            ab::TORRENT => counter.ty(WATER) == 0 && !moves.has(mv::FLIP_TURN),
            _ => false,
        }
    }

    /// `getAbility`.
    fn get_ability(&mut self, moves: &Moves, counter: &Counter, ctx: &Ctx<'_>) -> Result<AbilityId, GenerateError> {
        let abilities = ctx.abilities;
        let id = ctx.species.id;
        if abilities.len() <= 1 {
            return abilities.first().copied().ok_or_else(|| error("a set without abilities"));
        }
        if id == sp::DRIFBLIM {
            return Ok(if moves.has(mv::DEFOG) { ab::AFTERMATH } else { ab::UNBURDEN });
        }
        if ctx.has_ability(ab::FLASH_FIRE) && effectiveness_on_type(FIRE, ctx.tera) >= 1 {
            return Ok(ab::FLASH_FIRE);
        }
        if (id == sp::THUNDURUS || id == sp::TORNADUS) && counter.get(Key::Physical) == 0 {
            return Ok(ab::PRANKSTER);
        }
        if id == sp::SWAMPERT && (counter.ty(WATER) != 0 || moves.has(mv::FLIP_TURN)) {
            return Ok(ab::TORRENT);
        }
        if id == sp::TOUCANNON && counter.get(Key::SkillLink) != 0 {
            return Ok(ab::SKILL_LINK);
        }
        if ctx.has_ability(ab::SLUSH_RUSH) && moves.has(mv::SNOWSCAPE) {
            return Ok(ab::SLUSH_RUSH);
        }
        if id == sp::GOLDUCK && ctx.td.rain {
            return Ok(ab::SWIFT_SWIM);
        }
        let allowed: Vec<AbilityId> =
            abilities.iter().copied().filter(|&a| !self.should_cull_ability(a, moves, counter, ctx)).collect();
        if !allowed.is_empty() {
            return self.sample(&allowed, "getAbility");
        }
        let weather: Vec<AbilityId> = abilities
            .iter()
            .copied()
            .filter(|a| {
                [
                    ab::CHLOROPHYLL,
                    ab::HYDRATION,
                    ab::SAND_FORCE,
                    ab::SAND_RUSH,
                    ab::SLUSH_RUSH,
                    ab::SOLAR_POWER,
                    ab::SWIFT_SWIM,
                ]
                .contains(a)
            })
            .collect();
        if !weather.is_empty() {
            return self.sample(&weather, "getAbility weather");
        }
        self.sample(abilities, "getAbility any")
    }

    // ----------------------------------------------------------- items

    /// `getPriorityItem`.
    fn get_priority_item(
        &mut self,
        ability: AbilityId,
        moves: &Moves,
        counter: &Counter,
        ctx: &Ctx<'_>,
    ) -> Result<Pick, GenerateError> {
        let species = ctx.species;
        let id = species.id;
        let role = ctx.role;
        let doubles = ctx.doubles;
        let spe = species.base_stats[5];
        let has = |m: MoveId| moves.has(m);
        if !doubles {
            if role.is("Fast Bulky Setup") && (ability == ab::QUARK_DRIVE || ability == ab::PROTOSYNTHESIS) {
                return Ok(Pick::item(it::BOOSTER_ENERGY));
            }
            if id == sp::LOKIX {
                return Ok(Pick::item(if role.is("Fast Attacker") { it::SILVER_POWDER } else { it::LIFE_ORB }));
            }
        }
        if !species.required_items.is_empty() {
            if species.base_species == sp::ARCEUS {
                return Ok(Pick::item(species.required_items[0]));
            }
            return Ok(Pick::item(self.sample(species.required_items, "getPriorityItem requiredItems")?));
        }
        if id == sp::PIKACHU {
            return Ok(Pick::item(it::LIGHT_BALL));
        }
        if role.is("AV Pivot") {
            return Ok(Pick::item(it::ASSAULT_VEST));
        }
        if id == sp::REGIELEKI {
            return Ok(Pick::item(it::MAGNET));
        }
        if ctx.has_type(NORMAL) && has(mv::DOUBLE_EDGE) && has(mv::FAKE_OUT) {
            return Ok(Pick::item(it::SILK_SCARF));
        }
        if id == sp::FROSLASS
            || has(mv::POPULATION_BOMB)
            || (ability == ab::HUSTLE
                && counter.get(Key::Setup) != 0
                && !doubles
                && self.random_chance(1, 2, "getPriorityItem Wide Lens"))
        {
            return Ok(Pick::item(it::WIDE_LENS));
        }
        if id == sp::SMEARGLE {
            return Ok(Pick::item(it::FOCUS_SASH));
        }
        if has(mv::CLANGOROUS_SOUL) || (id == sp::TOXTRICITY && has(mv::SHIFT_GEAR)) {
            return Ok(Pick::item(it::THROAT_SPRAY));
        }
        if (species.base_species == sp::MAGEARNA && role.is("Tera Blast user"))
            || ((id == sp::CALYREX_ICE || id == sp::NECROZMA_DUSK_MANE) && doubles)
        {
            return Ok(Pick::item(it::WEAKNESS_POLICY));
        }
        if [mv::DRAGON_ENERGY, mv::LAST_RESPECTS, mv::WATER_SPOUT].iter().any(|&m| has(m)) {
            return Ok(Pick::item(it::CHOICE_SCARF));
        }
        if !doubles && (ability == ab::IMPOSTER || (id == sp::MAGNEZONE && role.is("Fast Attacker"))) {
            return Ok(Pick::item(it::CHOICE_SCARF));
        }
        if id == sp::RAMPARDOS && (role.is("Fast Attacker") || doubles) {
            return Ok(Pick::item(it::CHOICE_SCARF));
        }
        if id == sp::PALKIA && counter.get(Key::Status) != 0 {
            return Ok(Pick::item(it::LUSTROUS_ORB));
        }
        if has(mv::COURT_CHANGE) || (!doubles && (id == sp::LUVDISC || (id == sp::TERAPAGOS && !has(mv::REST)))) {
            return Ok(Pick::item(it::HEAVY_DUTY_BOOTS));
        }
        if [ab::CHEEK_POUCH, ab::CUD_CHEW, ab::HARVEST, ab::RIPEN].contains(&ability)
            || has(mv::BELLY_DRUM)
            || has(mv::FILLET_AWAY)
        {
            return Ok(Pick::item(it::SITRUS_BERRY));
        }
        if [mv::HEALING_WISH, mv::SWITCHEROO, mv::TRICK].iter().any(|&m| has(m)) {
            if (60..=108).contains(&spe)
                && !role.is("Wallbreaker")
                && !role.is("Doubles Wallbreaker")
                && counter.get(Key::Priority) == 0
            {
                return Ok(Pick::item(it::CHOICE_SCARF));
            }
            return Ok(Pick::item(if counter.get(Key::Physical) > counter.get(Key::Special) {
                it::CHOICE_BAND
            } else {
                it::CHOICE_SPECS
            }));
        }
        if counter.get(Key::Status) != 0 && (id == sp::LATIAS || id == sp::LATIOS) {
            return Ok(Pick::item(it::SOUL_DEW));
        }
        if id == sp::SCYTHER && !doubles {
            return Ok(Pick::item(if ctx.is_lead && !has(mv::U_TURN) { it::EVIOLITE } else { it::HEAVY_DUTY_BOOTS }));
        }
        if ability == ab::POISON_HEAL || ability == ab::QUICK_FEET {
            return Ok(Pick::item(it::TOXIC_ORB));
        }
        if species.nfe {
            return Ok(Pick::item(it::EVIOLITE));
        }
        if (ability == ab::GUTS || has(mv::FACADE)) && !has(mv::SLEEP_TALK) {
            return Ok(Pick::item(if ctx.has_type(FIRE) || ability == ab::TOXIC_BOOST {
                it::TOXIC_ORB
            } else {
                it::FLAME_ORB
            }));
        }
        if ability == ab::MAGIC_GUARD || (ability == ab::SHEER_FORCE && counter.get(Key::SheerForce) != 0) {
            return Ok(Pick::item(it::LIFE_ORB));
        }
        if ability == ab::ANGER_SHELL {
            let items = [it::EXPERT_BELT, it::LUM_BERRY, it::SCOPE_LENS, it::SITRUS_BERRY];
            return Ok(Pick::item(self.sample(&items, "getPriorityItem Anger Shell")?));
        }
        if has(mv::DRAGON_DANCE) && doubles {
            return Ok(Pick::item(it::CLEAR_AMULET));
        }
        if counter.get(Key::SkillLink) != 0 && ability != ab::SKILL_LINK && id != sp::BRELOOM {
            return Ok(Pick::item(it::LOADED_DICE));
        }
        if ability == ab::UNBURDEN {
            return Ok(Pick::item(if has(mv::CLOSE_COMBAT) || has(mv::LEAF_STORM) {
                it::WHITE_HERB
            } else {
                it::SITRUS_BERRY
            }));
        }
        if has(mv::SHELL_SMASH) && ability != ab::WEAK_ARMOR {
            return Ok(Pick::item(it::WHITE_HERB));
        }
        if has(mv::METEOR_BEAM) || (has(mv::ELECTRO_SHOT) && !ctx.td.rain) {
            return Ok(Pick::item(it::POWER_HERB));
        }
        if has(mv::ACROBATICS) && ability != ab::PROTOSYNTHESIS {
            return Ok(Pick::Item(None));
        }
        if has(mv::AURORA_VEIL) || (has(mv::LIGHT_SCREEN) && has(mv::REFLECT)) {
            return Ok(Pick::item(it::LIGHT_CLAY));
        }
        if ability == ab::GLUTTONY {
            let berries = [it::AGUAV_BERRY, it::FIGY_BERRY, it::IAPAPA_BERRY, it::MAGO_BERRY, it::WIKI_BERRY];
            return Ok(Pick::item(self.sample(&berries, "getPriorityItem Gluttony")?));
        }
        if id == sp::GIRATINA && !doubles && has(mv::REST) && !has(mv::SLEEP_TALK) {
            return Ok(Pick::item(it::LEFTOVERS));
        }
        if has(mv::REST) && !has(mv::SLEEP_TALK) && ability != ab::NATURAL_CURE && ability != ab::SHED_SKIN {
            return Ok(Pick::item(it::CHESTO_BERRY));
        }
        if id != sp::YANMEGA && effectiveness(ROCK, species) >= 2 && (!ctx.has_type(FLYING) || !doubles) {
            return Ok(Pick::item(it::HEAVY_DUTY_BOOTS));
        }
        Ok(Pick::Undefined)
    }

    /// `getDoublesItem`.
    fn get_doubles_item(
        &mut self,
        ability: AbilityId,
        moves: &Moves,
        counter: &Counter,
        ctx: &Ctx<'_>,
    ) -> Result<ItemId, GenerateError> {
        let species = ctx.species;
        let id = species.id;
        let role = ctx.role;
        let [hp, _, def, _, spd, spe] = species.base_stats.map(u32::from);
        let has = |m: MoveId| moves.has(m);
        let size = moves.size() as u32;
        let scarf_reqs = counter.get(Key::Priority) == 0
            && ability != ab::SPEED_BOOST
            && !role.is("Doubles Wallbreaker")
            && (60..=108).contains(&spe)
            && self.random_chance(1, 2, "getDoublesItem scarfReqs");
        let offensive_role =
            role.any(&["Doubles Fast Attacker", "Doubles Wallbreaker", "Doubles Setup Sweeper", "Offensive Protect"]);
        if id == sp::URSALUNA_BLOODMOON && has(mv::PROTECT) {
            return Ok(it::SILK_SCARF);
        }
        if has(mv::FLIP_TURN) && has(mv::PROTECT) && (has(mv::AQUA_JET) || has(mv::JET_PUNCH)) {
            return Ok(it::MYSTIC_WATER);
        }
        if counter.get(Key::SpeedSetup) != 0 && role.is("Doubles Bulky Setup") {
            return Ok(it::WEAKNESS_POLICY);
        }
        if has(mv::BLIZZARD) && ability != ab::SNOW_WARNING && !ctx.td.snow {
            return Ok(it::BLUNDER_POLICY);
        }
        if role.is("Choice Item user") {
            if scarf_reqs || has(mv::FINAL_GAMBIT) || id == sp::JIRACHI {
                return Ok(it::CHOICE_SCARF);
            }
            return Ok(if counter.get(Key::Physical) > counter.get(Key::Special) {
                it::CHOICE_BAND
            } else {
                it::CHOICE_SPECS
            });
        }
        if counter.get(Key::Physical) >= size
            && [mv::FAKE_OUT, mv::FEINT, mv::FIRST_IMPRESSION, mv::RAPID_SPIN, mv::SUCKER_PUNCH]
                .iter()
                .all(|&m| !has(m))
            && (has(mv::FLIP_TURN) || has(mv::U_TURN) || role.is("Doubles Wallbreaker"))
        {
            return Ok(if scarf_reqs { it::CHOICE_SCARF } else { it::CHOICE_BAND });
        }
        if ((counter.get(Key::Special) >= size && (has(mv::VOLT_SWITCH) || role.is("Doubles Wallbreaker")))
            || (counter.get(Key::Special) + 1 >= size && (has(mv::U_TURN) || has(mv::FLIP_TURN))))
            && !has(mv::ELECTROWEB)
        {
            return Ok(if scarf_reqs { it::CHOICE_SCARF } else { it::CHOICE_SPECS });
        }
        if spe <= 70 && (has(mv::RAGE_POWDER) || has(mv::FOLLOW_ME)) {
            return Ok(it::ROCKY_HELMET);
        }
        let rock = effectiveness(ROCK, species);
        if ability == ab::INTIMIDATE && rock >= 1 && (!ctx.has_type(FLYING) || rock >= 2) {
            return Ok(it::HEAVY_DUTY_BOOTS);
        }
        if role.is("Doubles Support")
            && ability == ab::PRANKSTER
            && has(mv::TAILWIND)
            && self.random_chance(1, 4, "getDoublesItem Covert Cloak")
        {
            return Ok(it::COVERT_CLOAK);
        }
        if (role.is("Bulky Protect") && counter.get(Key::Setup) != 0)
            || [mv::IRON_DEFENSE, mv::COIL, mv::ACID_ARMOR, mv::WISH].iter().any(|&m| has(m))
            || (counter.get(Key::Recovery) != 0
                && !has(mv::STRENGTH_SAP)
                && counter.get(Key::SpeedControl) == 0
                && !offensive_role)
            || (PROTECT_MOVES.iter().any(|&m| has(m)) && has(mv::LEECH_SEED))
            || id == sp::REGIGIGAS
        {
            return Ok(it::LEFTOVERS);
        }
        if has(mv::HYPER_VOICE) && !ctx.has_type(NORMAL) {
            return Ok(it::THROAT_SPRAY);
        }
        if role.is("Doubles Fast Attacker") && counter.get(Key::Recoil) == 0 && hp + def + spd <= 230 {
            return Ok(it::FOCUS_SASH);
        }
        if (offensive_role || (role.is("Tera Blast user") && (spe >= 80 || has(mv::TRICK_ROOM))))
            && !has(mv::FAKE_OUT)
            && (!has(mv::U_TURN) || ctx.has_type(BUG) || ability == ab::LIBERO)
            && ((!has(mv::ICY_WIND) && !has(mv::ELECTROWEB)) || id == sp::IRON_BUNDLE)
        {
            let booster = (ability == ab::QUARK_DRIVE || ability == ab::PROTOSYNTHESIS)
                && !ctx.is_lead
                && id != sp::IRON_VALIANT
                && [mv::DRACO_METEOR, mv::FIRST_IMPRESSION, mv::U_TURN, mv::VOLT_SWITCH].iter().all(|&m| !has(m));
            return Ok(if booster { it::BOOSTER_ENERGY } else { it::LIFE_ORB });
        }
        if ctx.is_lead
            && (id == sp::GLIMMORA
                || (role.any(&["Doubles Wallbreaker", "Offensive Protect"]) && hp + def + spd <= 230))
        {
            return Ok(it::FOCUS_SASH);
        }
        if role.any(&["Doubles Fast Attacker", "Doubles Wallbreaker", "Offensive Protect"]) && has(mv::FAKE_OUT) {
            return Ok(if rock >= 1 { it::HEAVY_DUTY_BOOTS } else { it::CLEAR_AMULET });
        }
        if counter.get(Key::Status) == 0 {
            return Ok(it::ASSAULT_VEST);
        }
        Ok(it::SITRUS_BERRY)
    }

    /// `getItem`.
    fn get_item(&mut self, ability: AbilityId, moves: &Moves, counter: &Counter, ctx: &Ctx<'_>) -> ItemId {
        let species = ctx.species;
        let id = species.id;
        let role = ctx.role;
        let [hp, atk, def, spa, spd, spe] = species.base_stats.map(u32::from);
        let has = |m: MoveId| moves.has(m);
        let size = moves.size() as u32;
        let life_orb_reqs = [mv::FLAME_CHARGE, mv::NUZZLE, mv::RAPID_SPIN].iter().all(|&m| !has(m));
        if id != sp::JIRACHI
            && counter.get(Key::Physical) >= size
            && [mv::DRAGON_TAIL, mv::FAKE_OUT, mv::FIRST_IMPRESSION, mv::FLAME_CHARGE, mv::RAPID_SPIN, mv::TRAILBLAZE]
                .iter()
                .all(|&m| !has(m))
        {
            let scarf_reqs = !role.is("Wallbreaker")
                && (atk >= 100 || ability == ab::HUGE_POWER || ability == ab::PURE_POWER)
                && (60..=108).contains(&spe)
                && ability != ab::SPEED_BOOST
                && counter.get(Key::Priority) == 0;
            return if scarf_reqs && self.random_chance(1, 2, "getItem Choice Scarf (physical)") {
                it::CHOICE_SCARF
            } else {
                it::CHOICE_BAND
            };
        }
        if counter.get(Key::Special) >= size
            || (counter.get(Key::Special) + 1 >= size && [mv::FLIP_TURN, mv::U_TURN].iter().any(|&m| has(m)))
        {
            let scarf_reqs = !role.is("Wallbreaker")
                && spa >= 100
                && (60..=108).contains(&spe)
                && ability != ab::SPEED_BOOST
                && ability != ab::TINTED_LENS
                && !has(mv::U_TURN)
                && counter.get(Key::Priority) == 0;
            return if scarf_reqs && self.random_chance(1, 2, "getItem Choice Scarf (special)") {
                it::CHOICE_SCARF
            } else {
                it::CHOICE_SPECS
            };
        }
        if counter.get(Key::SpeedSetup) != 0 && counter.get(Key::PhysicalSetup) == 0 && role.is("Bulky Setup") {
            return it::WEAKNESS_POLICY;
        }
        if counter.get(Key::Status) == 0 && !role.any(&["Fast Attacker", "Wallbreaker", "Tera Blast user"]) {
            return it::ASSAULT_VEST;
        }
        if id == sp::GOLEM {
            return if counter.get(Key::SpeedSetup) != 0 { it::WEAKNESS_POLICY } else { it::CUSTAP_BERRY };
        }
        if has(mv::SUBSTITUTE) {
            return it::LEFTOVERS;
        }
        if has(mv::STICKY_WEB) && ctx.is_lead && hp + def + spd <= 235 {
            return it::FOCUS_SASH;
        }
        if effectiveness(ROCK, species) >= 1 {
            return it::HEAVY_DUTY_BOOTS;
        }
        if has(mv::CHILLY_RECEPTION)
            || (role.is("Fast Support")
                && PIVOT_MOVES.iter().chain(&[mv::DEFOG, mv::MORTAL_SPIN, mv::RAPID_SPIN]).any(|&m| has(m))
                && !ctx.has_type(FLYING)
                && ability != ab::LEVITATE)
        {
            return it::HEAVY_DUTY_BOOTS;
        }
        if has(mv::DRAGON_DANCE) && role.is("Bulky Setup") {
            return it::WEAKNESS_POLICY;
        }
        if ability == ab::ROUGH_SKIN
            || (ability == ab::REGENERATOR
                && (role.is("Bulky Support") || role.is("Bulky Attacker"))
                && hp + def >= 180
                && self.random_chance(1, 2, "getItem Rocky Helmet (Regenerator)"))
            || (ability != ab::REGENERATOR
                && counter.get(Key::Setup) == 0
                && counter.get(Key::Recovery) != 0
                && effectiveness(FIGHTING, species) < 1
                && hp + def > 200
                && self.random_chance(1, 2, "getItem Rocky Helmet"))
        {
            return it::ROCKY_HELMET;
        }
        if has(mv::OUTRAGE) && counter.get(Key::Setup) != 0 {
            return it::LUM_BERRY;
        }
        if has(mv::PROTECT) && ability != ab::SPEED_BOOST {
            return it::LEFTOVERS;
        }
        if role.is("Fast Support")
            && ctx.is_lead
            && counter.get(Key::Recovery) == 0
            && counter.get(Key::Recoil) == 0
            && (counter.get(Key::Hazards) != 0 || counter.get(Key::Setup) != 0)
            && hp + def + spd < 258
        {
            return it::FOCUS_SASH;
        }
        if counter.get(Key::Setup) == 0 && ability != ab::LEVITATE && effectiveness(GROUND, species) >= 2 {
            return it::AIR_BALLOON;
        }
        if role.any(&["Bulky Attacker", "Bulky Support", "Bulky Setup"]) {
            return it::LEFTOVERS;
        }
        if id == sp::PAWMOT && has(mv::NUZZLE) {
            return it::LEPPA_BERRY;
        }
        if role.is("Fast Support") || role.is("Fast Bulky Setup") {
            let attacks = counter.get(Key::Physical) + counter.get(Key::Special);
            return if attacks > counter.get(Key::Status) && life_orb_reqs { it::LIFE_ORB } else { it::LEFTOVERS };
        }
        if role.is("Tera Blast user") && DEFENSIVE_TERA_BLAST_USERS.contains(&id) {
            return it::LEFTOVERS;
        }
        if life_orb_reqs && role.any(&["Fast Attacker", "Setup Sweeper", "Tera Blast user", "Wallbreaker"]) {
            return it::LIFE_ORB;
        }
        it::LEFTOVERS
    }

    // ------------------------------------------------------- randomSet

    /// `getLevel` (no Adjust Level in these formats).
    fn level(entry: &Entry) -> u8 {
        if entry.level != 0 { entry.level } else { 80 }
    }

    /// `getForme`: the forme a set plays as.
    fn forme(&mut self, species: &SpeciesFacts) -> Result<SpeciesId, GenerateError> {
        if let Some(f) = species.battle_only {
            return Ok(f);
        }
        if !species.cosmetic_formes.is_empty() {
            let mut formes = vec![species.id];
            formes.extend_from_slice(species.cosmetic_formes);
            return self.sample(&formes, "getForme cosmetic");
        }
        // No Gigantamax formes in the tables.
        let base = species.base_species;
        if [sp::DUDUNSPARCE, sp::MAUSHOLD, sp::POLTEAGEIST, sp::SINISTCHA, sp::ZARUDE].contains(&base) {
            // `[species.name].concat(species.otherFormes)`: an undefined
            // `otherFormes` is an element of its own.
            let formes: Vec<Option<SpeciesId>> = match species.other_formes {
                OtherFormes::Ids(ids) => core::iter::once(species.id).chain(ids.iter().copied()).map(Some).collect(),
                OtherFormes::Undefined => vec![Some(species.id), None],
                OtherFormes::Outside => return Err(error("getForme: a forme outside the ID space")),
            };
            return self.sample(&formes, "getForme otherFormes")?.ok_or_else(|| error("getForme: undefined forme"));
        }
        if base == sp::BASCULIN {
            return self.sample(&[sp::BASCULIN, sp::BASCULIN_BLUE_STRIPED], "getForme Basculin");
        }
        if base == sp::MAGEARNA {
            return self.sample(&[sp::MAGEARNA, sp::MAGEARNA_ORIGINAL], "getForme Magearna");
        }
        if base == sp::PIKACHU {
            let formes = [
                sp::PIKACHU,
                sp::PIKACHU_ORIGINAL,
                sp::PIKACHU_HOENN,
                sp::PIKACHU_SINNOH,
                sp::PIKACHU_UNOVA,
                sp::PIKACHU_KALOS,
                sp::PIKACHU_ALOLA,
                sp::PIKACHU_PARTNER,
                sp::PIKACHU_WORLD,
            ];
            return self.sample(&formes, "getForme Pikachu");
        }
        Ok(species.id)
    }

    /// `randomSet`.
    fn random_set(&mut self, entry: &Entry, td: TeamDetails, is_lead: bool) -> Result<RandomSet, GenerateError> {
        let species = &SPECIES[entry.species as usize];
        let doubles = self.doubles;
        let forme = self.forme(species)?;
        let possible: Vec<&super::Set> = entry
            .sets
            .iter()
            .filter(|set| {
                let role = ROLES[set.role as usize];
                let paradox = set.abilities.contains(&ab::PROTOSYNTHESIS) || set.abilities.contains(&ab::QUARK_DRIVE);
                if is_lead && paradox && role == "Fast Bulky Setup" {
                    return false;
                }
                !(td.tera_blast && role == "Tera Blast user")
            })
            .collect();
        let set = self.sample(&possible, "randomSet set")?;
        let role = Role(ROLES[set.role as usize]);
        let mut pool: Vec<MoveId> = set.movepool.to_vec();
        let tera = self.sample(set.tera_types, "randomSet teraType")?;
        let mut evs = [85u8; 6];
        let mut ivs = [31u8; 6];
        let ctx = Ctx { types: species.types, abilities: set.abilities, td, species, is_lead, tera, role, doubles };
        let moves = self.random_moveset(&mut pool, &ctx)?;
        let counter = self.query_moves(&moves, &ctx);
        let ability = self.get_ability(&moves, &counter, &ctx)?;
        let item = match self.get_priority_item(ability, &moves, &counter, &ctx)? {
            Pick::Item(item) => item,
            Pick::Undefined if doubles => Some(self.get_doubles_item(ability, &moves, &counter, &ctx)?),
            Pick::Undefined => Some(self.get_item(ability, &moves, &counter, &ctx)),
        };
        let level = Self::level(entry);

        // HP EVs: a number of HP that suits the item and moves.
        let sr_immunity = ability == ab::MAGIC_GUARD || item == Some(it::HEAVY_DUTY_BOOTS);
        let mut sr_weakness = if sr_immunity { 0 } else { effectiveness(ROCK, species) };
        if [mv::AXE_KICK, mv::HIGH_JUMP_KICK, mv::JUMP_KICK, mv::SUPERCELL_SLAM].iter().any(|&m| moves.has(m)) {
            sr_weakness = 2;
        }
        let sitrus = item == Some(it::SITRUS_BERRY);
        while evs[0] > 1 {
            let base = 2 * u32::from(species.base_stats[0]) + u32::from(ivs[0]) + u32::from(evs[0]) / 4 + 100;
            let hp = base * u32::from(level) / 100 + 10;
            if (moves.has(mv::SUBSTITUTE) && sitrus) || species.id == sp::MINIOR {
                if hp % 4 == 0 {
                    break;
                }
            } else if (moves.has(mv::BELLY_DRUM) || moves.has(mv::FILLET_AWAY) || moves.has(mv::SHED_TAIL))
                && (sitrus || ability == ab::GLUTTONY)
            {
                if hp % 2 == 0 {
                    break;
                }
            } else if moves.has(mv::SUBSTITUTE) && moves.has(mv::ENDEAVOR) {
                if hp % 4 > 0 {
                    break;
                }
            } else {
                if doubles {
                    break;
                }
                if sr_weakness <= 0
                    || ability == ab::REGENERATOR
                    || item == Some(it::LEFTOVERS)
                    || item == Some(it::LIFE_ORB)
                {
                    break;
                }
                // `4 / srWeakness`: srWeakness is 1 or 2 here.
                let step = 4 / sr_weakness as u32;
                if !sitrus && hp % step > 0 {
                    break;
                }
                if sitrus && hp % step == 0 {
                    break;
                }
            }
            evs[0] -= 4;
        }

        // No Attack investment without a move that uses it.
        let no_attack = moves.0.iter().all(|&m| {
            let move_ = facts(m);
            if move_.flags.has(MoveFlags::DAMAGE) {
                return true;
            }
            if m == mv::SHELL_SIDE_ARM {
                return false;
            }
            if m == mv::TERA_BLAST
                && (species.id == sp::PORYGON2
                    || ability == ab::CONTRARY
                    || ability == ab::DEFIANT
                    || moves.has(mv::SHIFT_GEAR)
                    || species.base_stats[1] > species.base_stats[3])
            {
                return false;
            }
            move_.category != 0 || m == mv::BODY_PRESS || m == mv::FOUL_PLAY
        });
        if no_attack && !moves.has(mv::TRANSFORM) {
            evs[1] = 0;
            ivs[1] = 0;
        }
        if moves.has(mv::GYRO_BALL) || moves.has(mv::TRICK_ROOM) {
            evs[5] = 0;
            ivs[5] = 0;
        }

        let mut shuffled = moves.0.clone();
        self.shuffle(&mut shuffled, "randomSet shuffle moves");
        let gender = if species.base_species == sp::GRENINJA {
            Gender::Male
        } else {
            match species.gender {
                b'M' => Gender::Male,
                b'F' => Gender::Female,
                b'N' => Gender::Genderless,
                _ => {
                    if self.random(2, "randomSet gender") != 0 {
                        Gender::Female
                    } else {
                        Gender::Male
                    }
                }
            }
        };
        let shiny = self.random_chance(1, 1024, "randomSet shiny");
        Ok(RandomSet {
            species: forme,
            species_id: species.id,
            name: species.base_species,
            gender,
            shiny,
            level,
            moves: shuffled,
            ability,
            evs: Stats(evs),
            ivs: Stats(ivs),
            item,
            tera_type: data::TYPES[tera as usize],
            role: role.0,
        })
    }

    // ------------------------------------------------------ randomTeam

    /// `getPokemonPool` (no exclusions, not monotype): the table's species
    /// grouped by base species in key order, and each base species up to
    /// three times (one per three of its species).
    fn pokemon_pool(&self) -> (Vec<(SpeciesId, Vec<&'static Entry>)>, Vec<usize>) {
        let mut pool: Vec<(SpeciesId, Vec<&'static Entry>)> = Vec::new();
        for entry in self.table {
            let base = SPECIES[entry.species as usize].base_species;
            match pool.iter_mut().find(|(b, _)| *b == base) {
                Some((_, list)) => list.push(entry),
                None => pool.push((base, vec![entry])),
            }
        }
        let mut base_pool = Vec::new();
        for (i, (base, list)) in pool.iter().enumerate() {
            let weight = if *base == sp::SQUAWKABILLY { 1 } else { list.len().div_ceil(3).min(3) };
            base_pool.extend(core::iter::repeat_n(i, weight));
        }
        (pool, base_pool)
    }

    /// `getPokemonCompatibility`.
    fn compatible(&self, species: SpeciesId, team: &[RandomSet]) -> bool {
        const WEB_SETTERS: &[SpeciesId] = &[
            sp::ARIADOS,
            sp::SMEARGLE,
            sp::MASQUERAIN,
            sp::KRICKETUNE,
            sp::LEAVANNY,
            sp::GALVANTULA,
            sp::VIKAVOLT,
            sp::RIBOMBEE,
            sp::ARAQUANID,
            sp::SPIDOPS,
        ];
        const SCREEN_SETTERS: &[SpeciesId] = &[sp::MEOWSTIC, sp::GRIMMSNARL, sp::NINETALES_ALOLA, sp::ABOMASNOW];
        const DOUBLES_WEB_SETTERS: &[SpeciesId] =
            &[sp::ARIADOS, sp::KRICKETUNE, sp::LEAVANNY, sp::GALVANTULA, sp::VIKAVOLT, sp::ARAQUANID, sp::SPIDOPS];
        const DOUBLES_SCREEN_SETTERS: &[SpeciesId] =
            &[sp::MEOWSTIC, sp::KLEFKI, sp::GRIMMSNARL, sp::NINETALES_ALOLA, sp::ABOMASNOW];
        const SUN_SETTERS: &[SpeciesId] = &[sp::NINETALES, sp::TORKOAL, sp::GROUDON, sp::KORAIDON];
        const RAIN_SETTERS: &[SpeciesId] = &[sp::POLITOED, sp::PELIPPER, sp::KYOGRE];
        const SAND_SETTERS: &[SpeciesId] = &[sp::TYRANITAR, sp::HIPPOWDON];
        const SNOW_SETTERS: &[SpeciesId] = &[sp::NINETALES_ALOLA, sp::ABOMASNOW];
        const NOT_SUN: &[SpeciesId] =
            &[sp::POLITOED, sp::PELIPPER, sp::KYOGRE, sp::TYRANITAR, sp::HIPPOWDON, sp::NINETALES_ALOLA, sp::ABOMASNOW];
        const NOT_RAIN: &[SpeciesId] = &[sp::TYRANITAR, sp::HIPPOWDON, sp::NINETALES_ALOLA, sp::ABOMASNOW];
        const PLUS_MINUS: &[SpeciesId] = &[sp::MINUN, sp::PLUSLE, sp::PACHIRISU, sp::RAICHU];
        let singles: &[(&[SpeciesId], &[SpeciesId])] = &[
            // These Pokemon with support roles are considered too similar to each other.
            (&[sp::BLISSEY], &[sp::CHANSEY]),
            (&[sp::ILLUMISE], &[sp::VOLBEAT]),
            // These combinations are prevented to avoid double webs or screens.
            (WEB_SETTERS, WEB_SETTERS),
            (SCREEN_SETTERS, SCREEN_SETTERS),
            // Prevent Dry Skin + sun setting ability
            (&[sp::TOXICROAK], SUN_SETTERS),
        ];
        let doubles: &[(&[SpeciesId], &[SpeciesId])] = &[
            (&[sp::ILLUMISE], &[sp::VOLBEAT]),
            (PLUS_MINUS, PLUS_MINUS),
            (DOUBLES_WEB_SETTERS, DOUBLES_WEB_SETTERS),
            (DOUBLES_SCREEN_SETTERS, DOUBLES_SCREEN_SETTERS),
            (&[sp::TOXICROAK], SUN_SETTERS),
            // Prevent conflicting weather abilities from generating together
            (SUN_SETTERS, NOT_SUN),
            (RAIN_SETTERS, NOT_RAIN),
            (SAND_SETTERS, SNOW_SETTERS),
            // Prevent conflicting terrain abilities from generating together
            (&[sp::PINCURCHIN, sp::MIRAIDON], &[sp::INDEEDEE, sp::INDEEDEE_F, sp::RILLABOOM, sp::ARBOLIVA]),
            (&[sp::RILLABOOM, sp::ARBOLIVA], &[sp::INDEEDEE, sp::INDEEDEE_F]),
        ];
        for &(a, b) in if self.doubles { doubles } else { singles } {
            if b.contains(&species) && team.iter().any(|m| a.contains(&m.species_id)) {
                return false;
            }
            if a.contains(&species) && team.iter().any(|m| b.contains(&m.species_id)) {
                return false;
            }
        }
        true
    }

    /// `randomTeam`.
    pub(crate) fn random_team(mut self) -> Result<Vec<RandomSet>, GenerateError> {
        let seed = self.rng.current_seed();
        let doubles = self.doubles;
        // `this.sample(typePool)`: drawn, then unused outside monotype.
        let type_pool: Vec<u8> = (0..TYPE_COUNT as u8).filter(|&t| t != STELLAR).collect();
        self.sample(&type_pool, "randomTeam type")?;
        let mut team: Vec<RandomSet> = Vec::with_capacity(MAX_TEAM_SIZE);
        let mut base_formes: Vec<SpeciesId> = Vec::new();
        let mut type_count = [0u32; TYPE_COUNT];
        let mut weaknesses = [0u32; TYPE_COUNT];
        let mut double_weaknesses = [0u32; TYPE_COUNT];
        let mut freeze_dry = 0u32;
        let mut td = TeamDetails::default();
        let mut max_level = 0u32;
        let (pool, mut base_pool) = self.pokemon_pool();
        let mut leads = if doubles { 2 } else { 1 };
        // `Math.round(maxTeamSize / 6) || 1`.
        let limit = 1;
        while !base_pool.is_empty() && team.len() < MAX_TEAM_SIZE {
            let base = self.sample_no_replace(&mut base_pool, "randomTeam base species").expect("not empty");
            let entry: &Entry = self.sample(&pool[base].1, "randomTeam species")?;
            let species = &SPECIES[entry.species as usize];
            if base_formes.contains(&species.base_species) {
                continue;
            }
            if [sp::OGERPON, sp::OGERPON_HEARTHFLAME, sp::TERAPAGOS].contains(&species.id) && td.tera_blast {
                continue;
            }
            if species.base_species == sp::ZOROARK && team.len() + 1 >= MAX_TEAM_SIZE {
                continue;
            }
            let types = species.types;
            let ice = effectiveness(ICE, species);
            let weak_to_freeze_dry = ice > 0 || (ice > -2 && types.contains(&WATER));
            // Limit two of any type, three weak to any type, one doubly
            // weak to any type.
            if types.iter().any(|&t| type_count[t as usize] >= 2 * limit) {
                continue;
            }
            let mut skip = false;
            for t in 0..TYPE_COUNT as u8 {
                let e = effectiveness(t, species);
                if e > 0 && weaknesses[t as usize] >= 3 * limit {
                    skip = true;
                    break;
                }
                if e > 1 && double_weaknesses[t as usize] >= limit {
                    skip = true;
                    break;
                }
            }
            if skip {
                continue;
            }
            let fire_absorbing = species.abilities.iter().any(|&a| a == ab::DRY_SKIN || a == ab::FLUFFY);
            if effectiveness(FIRE, species) == 0 && fire_absorbing && weaknesses[FIRE as usize] >= 3 * limit {
                continue;
            }
            if weak_to_freeze_dry && freeze_dry >= 4 * limit {
                continue;
            }
            // One level-100 Pokémon at most.
            if Self::level(entry) == 100 && max_level >= limit {
                continue;
            }
            if !self.compatible(species.id, &team) {
                continue;
            }
            let set = if leads > 0 {
                let no_lead = if doubles {
                    DOUBLES_NO_LEAD_POKEMON.contains(&species.id)
                } else {
                    NO_LEAD_POKEMON.contains(&species.id)
                };
                if no_lead {
                    if team.len() + leads == MAX_TEAM_SIZE {
                        continue;
                    }
                    let set = self.random_set(entry, td, false)?;
                    team.push(set);
                    team.last()
                } else {
                    let set = self.random_set(entry, td, true)?;
                    team.insert(0, set);
                    leads -= 1;
                    team.first()
                }
            } else {
                let set = self.random_set(entry, td, false)?;
                team.push(set);
                team.last()
            }
            .expect("just added");
            if team.len() == MAX_TEAM_SIZE {
                break;
            }
            let (ability, level) = (set.ability, set.level);
            let has = |m: MoveId| set.moves.contains(&m);
            let rain = ability == ab::DRIZZLE || has(mv::RAIN_DANCE);
            let sun = ability == ab::DROUGHT || ability == ab::ORICHALCUM_PULSE || has(mv::SUNNY_DAY);
            let snow = ability == ab::SNOW_WARNING || has(mv::SNOWSCAPE) || has(mv::CHILLY_RECEPTION);
            let spikes = has(mv::SPIKES) || has(mv::CEASELESS_EDGE);
            let toxic_spikes = has(mv::TOXIC_SPIKES) || ability == ab::TOXIC_DEBRIS;
            let stealth_rock = has(mv::STEALTH_ROCK) || has(mv::STONE_AXE);
            let rapid_spin = has(mv::RAPID_SPIN) || has(mv::MORTAL_SPIN);
            let screens = has(mv::AURORA_VEIL) || (has(mv::REFLECT) && has(mv::LIGHT_SCREEN));
            let tera_blast = set.role == "Tera Blast user"
                || [sp::OGERPON, sp::OGERPON_HEARTHFLAME, sp::TERAPAGOS].contains(&species.id);
            let (heal_bell, sticky_web, defog) = (has(mv::HEAL_BELL), has(mv::STICKY_WEB), has(mv::DEFOG));

            base_formes.push(species.base_species);
            for &t in types {
                type_count[t as usize] += 1;
            }
            for t in 0..TYPE_COUNT {
                let e = effectiveness(t as u8, species);
                if e > 0 {
                    weaknesses[t] += 1;
                }
                if e > 1 {
                    double_weaknesses[t] += 1;
                }
            }
            if (ability == ab::DRY_SKIN || ability == ab::FLUFFY) && effectiveness(FIRE, species) == 0 {
                weaknesses[FIRE as usize] += 1;
            }
            if weak_to_freeze_dry {
                freeze_dry += 1;
            }
            if level == 100 {
                max_level += 1;
            }
            td.rain |= rain;
            td.sun |= sun;
            td.sand |= ability == ab::SAND_STREAM;
            td.snow |= snow;
            td.status_cure |= heal_bell;
            if spikes {
                td.spikes += 1;
            }
            td.toxic_spikes |= toxic_spikes;
            td.stealth_rock |= stealth_rock;
            td.sticky_web |= sticky_web;
            td.defog |= defog;
            td.rapid_spin |= rapid_spin;
            td.screens |= screens;
            td.tera_blast |= tera_blast;
        }
        if team.len() < MAX_TEAM_SIZE {
            // `${this.format}` is its name; the seed is `prng.getSeed()`.
            let format = if doubles { "[Gen 9] Random Doubles Battle" } else { "[Gen 9] Random Battle" };
            let seed = match seed {
                Some(Seed::ShowdownGen5([a, b, c, d])) => format!("{a},{b},{c},{d}"),
                Some(Seed::Sodium(key)) => {
                    format!("sodium,{}", key.iter().map(|b| format!("{b:02x}")).collect::<String>())
                }
                _ => "?".to_string(),
            };
            return Err(error(format!("Could not build a random team for {format} (seed={seed})")));
        }
        Ok(team)
    }
}

/// `incompatibleMoves`: for each move of the set (in its order) in `b`,
/// takes the moves of `a` out of the pool, and those of `b` for one in
/// `a`, until the set and the pool are only four moves.
fn incompatible_moves(moves: &Moves, pool: &mut Vec<MoveId>, a: &[MoveId], b: &[MoveId]) {
    let done = |pool: &Vec<MoveId>| moves.size() + pool.len() <= MAX_MOVE_COUNT;
    if done(pool) {
        return;
    }
    for &m1 in &moves.0 {
        if b.contains(&m1) {
            for &m2 in a {
                if m1 != m2 && pool.contains(&m2) {
                    pop_if_present(pool, m2);
                    if done(pool) {
                        return;
                    }
                }
            }
        }
        if a.contains(&m1) {
            for &m2 in b {
                if m1 != m2 && pool.contains(&m2) {
                    pop_if_present(pool, m2);
                    if done(pool) {
                        return;
                    }
                }
            }
        }
    }
}
