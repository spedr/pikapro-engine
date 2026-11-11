//! Differential testing against Pokémon Showdown.
//!
//! The loop:
//!
//! 1. [`random_scenario`] plays a battle on pikapro with a
//!    Showdown-compatible seed ([`Seed::ShowdownGen5`]) and random legal
//!    choices, recording the teams and the choice strings.
//! 2. [`run_showdown_with_requests`] replays exactly those inputs on the
//!    real simulator via `tools/showdown-oracle/oracle.mjs`.
//! 3. [`compare`] normalises both raw logs (`|split|` blocks included) and
//!    reports the first divergence, then checks that each perspective's log
//!    is the channel Showdown would send it.
//!
//! Because both engines draw from the same PRNG with the same formulas, any
//! difference in mechanics *or in the order of random draws* shows up as a
//! diff. See docs/CONFORMANCE.md.

pub mod channel;
pub mod channel_fixture;
pub mod channel_fuzz;
pub mod channel_showdown;
pub mod choices;
pub mod cli;
pub mod fixture;
pub mod random_teams;
pub mod teams;
pub mod validate;
pub mod view;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use pikapro::format::StatSystem;
use pikapro::{
    Battle, BattleOptions, BattleRng, Choice, Format, Gender, Gimmick, Investment, PokemonSet, Seed, SideId, Team,
};
use pikapro_data::gen9::{abilities, items, moves};
use pikapro_data::{
    AbilityId, GenderRule, ItemId, MoveCategory, MoveData, MoveId, MoveTarget, Nature, SelfSwitch, SpeciesId, Stat,
    Stats, Type,
};
use pikapro_protocol::{Perspective, Renderer, SetExtras, TeamSheet, export_sheet, extract_channel};

/// A reproducible battle input: everything both simulators need.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub format: Format,
    pub seed: [u16; 4],
    pub teams: [Team; 2],
    /// `(side, choice string)` in submission order, as pikapro saw them.
    pub choices: Vec<(SideId, String)>,
    /// Each side's nicknames as written (`SetExtras::name`) by set
    /// index, empty for none. The teams sent to Showdown carry them
    /// (`Nick (Species)`); [`Scenario::name_table`] is what the renderer and
    /// the choice parser take.
    pub nicknames: [Vec<String>; 2],
    /// Which sets are shiny (`, shiny` in details), by set index.
    pub shiny: [Vec<bool>; 2],
    /// The teams as the players listed them, when they list battle-only
    /// formes (`Floette-Mega @ Floettite`): both simulators validate them
    /// first, as a Showdown server does (the oracle with Showdown's
    /// `TeamValidator` for the format's validation format, pikapro with
    /// pikapro-validate), and `teams` is what pikapro's validator made of
    /// them ([`pikapro_validate::Checked::team`]). `None`: `teams` are
    /// played as given.
    pub listed: Option<[Team; 2]>,
    /// A random battle's players' seeds for Showdown's team generator:
    /// `teams` are pikapro-teams' generator's for them, and Showdown is
    /// given the seeds instead of the teams, so its own generator draws
    /// them ([`random_generated_scenario`]).
    pub team_seeds: Option<[String; 2]>,
}

impl Scenario {
    /// A side's team with its nicknames.
    pub fn sheet(&self, side: SideId) -> TeamSheet {
        self.sheet_of(&self.teams[side.index()], side)
    }

    /// A side's team as listed ([`Scenario::listed`], else as played) with
    /// its nicknames: what the players give the simulators.
    pub fn listed_sheet(&self, side: SideId) -> TeamSheet {
        self.sheet_of(self.listed.as_ref().map_or(&self.teams[side.index()], |l| &l[side.index()]), side)
    }

    fn sheet_of(&self, team: &Team, side: SideId) -> TeamSheet {
        let (names, shiny) = (&self.nicknames[side.index()], &self.shiny[side.index()]);
        let extras = (0..team.sets.len())
            .map(|i| SetExtras {
                name: names.get(i).cloned().unwrap_or_default(),
                shiny: shiny.get(i).copied().unwrap_or(false),
                ..SetExtras::default()
            })
            .collect();
        TeamSheet { team: team.clone(), extras }
    }

    /// The Showdown format the listed teams are validated in
    /// ([`Scenario::listed`]): the preset's validation format.
    pub fn validation_format(&self) -> Option<&'static str> {
        self.listed.as_ref()?;
        pikapro_validate::ValidationFormat::for_preset(self.format.id).map(|v| v.id)
    }

    /// The names each side's Pokémon go by, by `PokemonState::id` (empty:
    /// the base species'), as `TeamSheet::nicknames` gives them.
    pub fn name_table(&self) -> [Vec<String>; 2] {
        SideId::BOTH.map(|side| self.sheet(side).nicknames(&self.format))
    }
}

/// The Showdown format id that mirrors a pikapro format, if there is one.
pub fn showdown_format(format: &Format) -> Option<&'static str> {
    Some(match format.id {
        "gen9singles" => "gen9customgame@@@!Team Preview",
        "gen9doubles" => "gen9doublescustomgame@@@!Team Preview",
        "gen9vgc" => "gen9doublescustomgame@@@Picked Team Size = 4,Adjust Level Down = 50",
        "championsvgc" => "gen9championsvgc2026regmc",
        "championsbss" => "gen9championsbssregmc",
        "gen9ou" => "gen9ou",
        "gen9randombattle" => "gen9randombattle",
        "gen9randomdoublesbattle" => "gen9randomdoublesbattle",
        _ => return None,
    })
}

/// Species whose behaviour pikapro implements completely when they have
/// no ability and no item: every standard species in the format's dex,
/// except formes that need an item pikapro lacks or only exist in battle.
/// (Ogerpon's and Terapagos's forced Tera types and Tera formes are
/// implemented; their sets use the forced type.)
pub fn species_pool(format: &Format) -> Vec<SpeciesId> {
    let dex = format.dex;
    (0..dex.species.len() as u16)
        .map(SpeciesId)
        .filter(|&id| {
            let s = dex.species(id);
            s.nonstandard.is_none()
                && !s.battle_only
                // The cosmetic formes only Showdown's random-battle
                // generator draws (their data is their base species').
                && !pikapro_teams::random_battle::ADDED_FORMES.contains(&id)
                // Formes whose item is implemented (the
                // Arceus Plates, the Origin formes' items).
                && (s.required_items.is_empty() || s.required_items.iter().any(|i| IMPLEMENTED_ITEMS.contains(i)))
        })
        .collect()
}

/// Moves whose Showdown callbacks (`MoveData::unmodelled`) pikapro
/// implements in code, so they are complete despite not being data-only.
/// Grouped by topic, each group with a comment.
pub const IMPLEMENTED_BY_CODE: &[MoveId] = &[
    // Protection (protect.rs).
    moves::PROTECT,
    // Damage variants (effects.rs).
    moves::SEISMIC_TOSS,
    moves::NIGHT_SHADE,
    moves::SUPER_FANG,
    moves::RUINATION,
    moves::FINAL_GAMBIT,
    moves::SHEER_COLD,
    moves::SURGING_STRIKES,
    moves::POPULATION_BOMB,
    moves::TRIPLE_AXEL,
    moves::TRIPLE_KICK,
    moves::WATER_SHURIKEN,
    // Status (status.rs).
    moves::THUNDER_WAVE,
    // Volatiles (volatiles.rs).
    moves::FIRST_IMPRESSION,
    moves::FAKE_OUT,
    moves::TAUNT,
    moves::ENCORE,
    moves::DISABLE,
    // Field (field.rs).
    moves::ELECTRIC_TERRAIN,
    moves::GRASSY_TERRAIN,
    moves::MISTY_TERRAIN,
    moves::PSYCHIC_TERRAIN,
    moves::TRICK_ROOM,
    moves::TAILWIND,
    moves::REFLECT,
    moves::LIGHT_SCREEN,
    moves::AURORA_VEIL,
    // Switching and protection (switching.rs, protect.rs).
    moves::DETECT,
    moves::SPIKY_SHIELD,
    moves::KINGS_SHIELD,
    moves::SILK_TRAP,
    moves::OBSTRUCT,
    moves::BANEFUL_BUNKER,
    moves::BURNING_BULWARK,
    moves::FEINT,
    moves::HYPERSPACE_HOLE,
    moves::ROAR,
    moves::WHIRLWIND,
    moves::TELEPORT,
    moves::PARTING_SHOT,
    // Damage formula.
    moves::ACROBATICS,
    moves::ASSURANCE,
    moves::AVALANCHE,
    moves::BEAT_UP,
    moves::CRUSH_GRIP,
    moves::DRAGON_ENERGY,
    moves::ECHOED_VOICE,
    moves::ELECTRO_BALL,
    moves::ERUPTION,
    moves::WATER_SPOUT,
    moves::FLAIL,
    moves::REVERSAL,
    moves::FURY_CUTTER,
    moves::GRASS_KNOT,
    moves::LOW_KICK,
    moves::HEAT_CRASH,
    moves::HEAVY_SLAM,
    moves::GYRO_BALL,
    moves::HARD_PRESS,
    moves::HEX,
    moves::INFERNAL_PARADE,
    moves::LAST_RESPECTS,
    moves::PAYBACK,
    moves::POWER_TRIP,
    moves::STORED_POWER,
    moves::RAGE_FIST,
    moves::RISING_VOLTAGE,
    moves::STOMPING_TANTRUM,
    moves::TEMPER_FLARE,
    moves::BARB_BARRAGE,
    moves::BRINE,
    moves::COLLISION_COURSE,
    moves::ELECTRO_DRIFT,
    moves::FACADE,
    moves::FICKLE_BEAM,
    moves::FUSION_BOLT,
    moves::FUSION_FLARE,
    moves::GRAV_APPLE,
    moves::LASH_OUT,
    moves::MISTY_EXPLOSION,
    moves::PSYBLADE,
    moves::RETALIATE,
    moves::VENOSHOCK,
    moves::EXPANDING_FORCE,
    moves::AURA_WHEEL,
    moves::IVY_CUDGEL,
    moves::JUDGMENT,
    moves::REVELATION_DANCE,
    moves::TERA_BLAST,
    moves::TERA_STARSTORM,
    moves::TERRAIN_PULSE,
    moves::WEATHER_BALL,
    moves::BLIZZARD,
    moves::HURRICANE,
    moves::THUNDER,
    moves::BLEAKWIND_STORM,
    moves::SANDSEAR_STORM,
    moves::WILDBOLT_STORM,
    moves::GROWTH,
    moves::PRESENT,
    moves::PHOTON_GEYSER,
    moves::SHELL_SIDE_ARM,
    moves::BODY_PRESS,
    moves::FOUL_PLAY,
    moves::PSYSHOCK,
    moves::PSYSTRIKE,
    moves::SECRET_SWORD,
    moves::DARKEST_LARIAT,
    moves::SACRED_SWORD,
    moves::MOONGEIST_BEAM,
    moves::SUNSTEEL_STRIKE,
    moves::FLOWER_TRICK,
    moves::FROST_BREATH,
    moves::WICKED_BLOW,
    moves::STORM_THROW,
    moves::FLYING_PRESS,
    moves::FREEZE_DRY,
    moves::GRASSY_GLIDE,
    moves::ENDEAVOR,
    moves::FALSE_SWIPE,
    // `Unobtainable` in Gen 9, so only Metronome calls it (False Swipe's
    // `onDamage`).
    moves::HOLD_BACK,
    moves::HIGH_JUMP_KICK,
    moves::AXE_KICK,
    moves::SUPERCELL_SLAM,
    moves::STEEL_BEAM,
    moves::CHLOROBLAST,
    moves::STRUGGLE,
    moves::DREAM_EATER,
    moves::CLANGING_SCALES,
    moves::SCALE_SHOT,
    moves::FELL_STINGER,
    moves::ORDER_UP,
    moves::DIAMOND_STORM,
    moves::HYPERSPACE_FURY,
    // Meloetta's forme change.
    moves::RELIC_SONG,
    // The last three: Double Shock and Burn Up leave their user typeless
    // (`Type::Unknown`, `manipulation::self_on_hit`).
    moves::DOUBLE_SHOCK,
    moves::BURN_UP,
    // Multi-turn moves.
    moves::FLY,
    moves::DIG,
    moves::DIVE,
    moves::BOUNCE,
    moves::PHANTOM_FORCE,
    moves::SHADOW_FORCE,
    moves::SKY_ATTACK,
    moves::SOLAR_BEAM,
    moves::SOLAR_BLADE,
    moves::METEOR_BEAM,
    moves::ELECTRO_SHOT,
    moves::FREEZE_SHOCK,
    moves::ICE_BURN,
    moves::OUTRAGE,
    moves::PETAL_DANCE,
    moves::THRASH,
    moves::RAGING_FURY,
    moves::UPROAR,
    moves::ROLLOUT,
    moves::FOCUS_PUNCH,
    moves::BEAK_BLAST,
    moves::FUTURE_SIGHT,
    moves::DOOM_DESIRE,
    moves::COUNTER,
    moves::MIRROR_COAT,
    moves::METAL_BURST,
    moves::COMEUPPANCE,
    moves::CHILLY_RECEPTION,
    // Substitute, trapping, residual volatiles: conditions.rs.
    moves::SUBSTITUTE,
    moves::SHED_TAIL,
    moves::BIND,
    moves::FIRE_SPIN,
    moves::INFESTATION,
    moves::MAGMA_STORM,
    moves::SAND_TOMB,
    moves::SNAP_TRAP,
    moves::THUNDER_CAGE,
    moves::WHIRLPOOL,
    moves::WRAP,
    moves::BLOCK,
    moves::MEAN_LOOK,
    moves::SPIRIT_SHACKLE,
    moves::JAW_LOCK,
    moves::NO_RETREAT,
    moves::FAIRY_LOCK,
    moves::LEECH_SEED,
    moves::YAWN,
    moves::CURSE,
    moves::PERISH_SONG,
    moves::DESTINY_BOND,
    moves::ATTRACT,
    moves::TORMENT,
    moves::SALT_CURE,
    moves::SYRUP_BOMB,
    moves::THROAT_CHOP,
    moves::PSYCHIC_NOISE,
    moves::IMPRISON,
    moves::GLAIVE_RUSH,
    moves::ENDURE,
    moves::SMACK_DOWN,
    moves::MAGNET_RISE,
    // Champions Reg M-C: Octolock's volatile (conditions.rs).
    moves::OCTOLOCK,
    // Hazards, side, field and slot conditions.
    // Hazards (hazards.rs).
    moves::STEALTH_ROCK,
    moves::SPIKES,
    moves::TOXIC_SPIKES,
    moves::STICKY_WEB,
    moves::RAPID_SPIN,
    moves::MORTAL_SPIN,
    moves::DEFOG,
    moves::TIDY_UP,
    moves::CEASELESS_EDGE,
    moves::STONE_AXE,
    // Field and side conditions (field.rs).
    moves::COURT_CHANGE,
    moves::ICE_SPINNER,
    moves::STEEL_ROLLER,
    moves::SAFEGUARD,
    moves::MIST,
    moves::GRAVITY,
    moves::MAGIC_ROOM,
    moves::WONDER_ROOM,
    moves::HAZE,
    moves::WIDE_GUARD,
    moves::QUICK_GUARD,
    moves::MAGNETIC_FLUX,
    moves::HAPPY_HOUR,
    moves::BRICK_BREAK,
    moves::PSYCHIC_FANGS,
    moves::RAGING_BULL,
    // Slot conditions (slots.rs).
    moves::WISH,
    moves::HEALING_WISH,
    moves::LUNAR_DANCE,
    moves::REVIVAL_BLESSING,
    // Pledges (pledges.rs).
    moves::FIRE_PLEDGE,
    moves::GRASS_PLEDGE,
    moves::WATER_PLEDGE,
    // Stat, HP and type manipulation: manipulation.rs.
    moves::ACUPRESSURE,
    moves::BELLY_DRUM,
    moves::CLANGOROUS_SOUL,
    moves::FILLET_AWAY,
    moves::TAKE_HEART,
    moves::PAIN_SPLIT,
    moves::PSYCH_UP,
    moves::PSYCHO_SHIFT,
    moves::HEART_SWAP,
    moves::GUARD_SPLIT,
    moves::GUARD_SWAP,
    moves::POWER_SPLIT,
    moves::POWER_SWAP,
    moves::SPEED_SWAP,
    moves::TOPSY_TURVY,
    moves::REST,
    moves::HEAL_BELL,
    moves::HEAL_PULSE,
    moves::FLORAL_HEALING,
    moves::JUNGLE_HEALING,
    moves::LUNAR_BLESSING,
    moves::MOONLIGHT,
    moves::MORNING_SUN,
    moves::SYNTHESIS,
    moves::SHORE_UP,
    moves::STRENGTH_SAP,
    moves::SOAK,
    moves::MAGIC_POWDER,
    moves::CONVERSION,
    moves::CONVERSION_2,
    moves::REFLECT_TYPE,
    moves::FORESTS_CURSE,
    moves::TRICK_OR_TREAT,
    moves::MEMENTO,
    moves::SPLASH,
    moves::CELEBRATE,
    moves::DARK_VOID,
    moves::AQUA_RING,
    moves::INGRAIN,
    moves::CHARGE,
    moves::FOCUS_ENERGY,
    moves::DRAGON_CHEER,
    moves::ROOST,
    moves::TAR_SHOT,
    moves::MINIMIZE,
    moves::DEFENSE_CURL,
    moves::STOCKPILE,
    moves::SPIT_UP,
    moves::SWALLOW,
    moves::POWER_TRICK,
    moves::POWER_SHIFT,
    moves::ELECTRIFY,
    moves::LOCK_ON,
    moves::CLEAR_SMOG,
    moves::SCALD,
    moves::HYDRO_STEAM,
    moves::MATCHA_GOTCHA,
    moves::SCORCHING_SANDS,
    moves::STEAM_ERUPTION,
    moves::SPARKLING_ARIA,
    moves::ALLURING_VOICE,
    moves::BURNING_JEALOUSY,
    moves::DIRE_CLAW,
    moves::EERIE_SPELL,
    moves::TRI_ATTACK,
    // Action order, targeting, move calling: order.rs.
    moves::AFTER_YOU,
    moves::QUASH,
    moves::INSTRUCT,
    moves::ROUND,
    moves::ALLY_SWITCH,
    moves::HELPING_HAND,
    moves::FOLLOW_ME,
    moves::RAGE_POWDER,
    moves::DRAGON_DARTS,
    moves::SNIPE_SHOT,
    moves::SUCKER_PUNCH,
    moves::THUNDERCLAP,
    moves::UPPER_HAND,
    moves::LAST_RESORT,
    moves::COPYCAT,
    moves::SLEEP_TALK,
    moves::SNORE,
    moves::BATON_PASS,
    moves::POLLEN_PUFF,
    moves::TRANSFORM,
    moves::SKETCH,
    moves::MIMIC,
    moves::SPITE,
    // Metronome calls any move of its list (581 in Gen 9: every standard
    // or `Unobtainable` move with the `metronome` flag, all implemented).
    moves::METRONOME,
    // Item moves: itemmoves.rs.
    moves::KNOCK_OFF,
    moves::THIEF,
    moves::COVET,
    moves::TRICK,
    moves::SWITCHEROO,
    moves::RECYCLE,
    moves::BUG_BITE,
    moves::PLUCK,
    moves::INCINERATE,
    moves::FLING,
    moves::BELCH,
    moves::STUFF_CHEEKS,
    moves::POLTERGEIST,
    moves::CORROSIVE_GAS,
    moves::SKILL_SWAP,
    moves::ROLE_PLAY,
    moves::ENTRAINMENT,
    moves::SIMPLE_BEAM,
    moves::WORRY_SEED,
    moves::GASTRO_ACID,
    moves::DOODLE,
    moves::TEATIME,
];

/// Moves pikapro implements completely: standard in the format's dex, and
/// either in [`IMPLEMENTED_BY_CODE`] or fully described by data the engine
/// models (`data_supported`).
pub fn move_pool(format: &Format) -> Vec<MoveId> {
    let dex = format.dex;
    (0..dex.moves.len() as u16)
        .map(MoveId)
        .filter(|&id| {
            let m = dex.move_(id);
            m.nonstandard.is_none() && (IMPLEMENTED_BY_CODE.contains(&id) || data_supported(m))
        })
        .collect()
}

/// Whether the engine implements everything `m`'s data asks for. Each rule
/// names the module that implements it; loosen a rule together with the
/// mechanic.
fn data_supported(m: &MoveData) -> bool {
    let effect = match m.category {
        // Primary stat changes (boosts.rs): Swords Dance, Growl, ...
        // Healing on the user or its side (effects.rs): Recover, Life Dew.
        // Primary status (status.rs): Will-O-Wisp, Toxic, Hypnosis, ...
        // Primary confusion (volatiles.rs): Confuse Ray, Supersonic, ...
        // Weather (field.rs): Sunny Day, Rain Dance, ...
        MoveCategory::Status => {
            m.boosts.is_some()
                || m.status.is_some()
                || m.volatile_status == Some("confusion")
                || m.weather.is_some()
                // Champions' Milk Drink heals itself or an
                // adjacent ally (`adjacentAllyOrSelf`, targeting.rs).
                || (m.heal.is_some()
                    && matches!(m.target, MoveTarget::User | MoveTarget::Allies | MoveTarget::AdjacentAllyOrSelf))
        }
        // Damage, including OHKO moves (effects.rs).
        _ => m.base_power > 0 || m.ohko,
    };
    let target = matches!(
        m.target,
        MoveTarget::Normal
            | MoveTarget::AdjacentFoe
            | MoveTarget::Any
            | MoveTarget::AllAdjacent
            | MoveTarget::AllAdjacentFoes
            | MoveTarget::RandomNormal
            | MoveTarget::User
            | MoveTarget::Allies
            | MoveTarget::All
            // Ally-only moves (Aromatic Mist, Coaching).
            | MoveTarget::AdjacentAlly
            // Milk Drink in Champions.
            | MoveTarget::AdjacentAllyOrSelf
    );
    m.is_data_only()
        && effect
        && target
        // Secondaries (secondaries.rs): stat changes, status, flinch and
        // confusion.
        && m.secondaries.iter().all(|s| matches!(s.volatile_status, None | Some("flinch" | "confusion")))
        // Volatiles (volatiles.rs): confusion on status moves only, and the
        // recharge turn of Hyper Beam and the like.
        && (m.volatile_status.is_none() || (m.category == MoveCategory::Status && m.volatile_status == Some("confusion")))
        && (m.self_volatile.is_none() || m.self_volatile == Some("mustrecharge"))
        // Switching (switching.rs): U-turn and the like, and dragging out
        // on a hit (Dragon Tail); Baton Pass and Shed Tail copy volatiles,
        // and Roar and Whirlwind come through `IMPLEMENTED_BY_CODE`.
        && matches!(m.self_switch, None | Some(SelfSwitch::Switch))
        && (!m.force_switch || m.category != MoveCategory::Status)
        // Side, slot and pseudo-weather conditions and terrains come
        // through `IMPLEMENTED_BY_CODE`.
        && m.side_condition.is_none()
        && m.slot_condition.is_none()
        && m.terrain.is_none()
        && m.pseudo_weather.is_none()
}

/// Abilities pikapro implements completely, grouped by topic. A random
/// set gets one of its species' legal abilities from this list, or No
/// Ability.
pub const IMPLEMENTED_ABILITIES: &[AbilityId] = &[
    // Switch-in, field, forme and ability-suppressing abilities.
    abilities::INTIMIDATE,
    abilities::DROUGHT,
    abilities::DRIZZLE,
    abilities::SAND_STREAM,
    abilities::SNOW_WARNING,
    abilities::ELECTRIC_SURGE,
    abilities::GRASSY_SURGE,
    abilities::MISTY_SURGE,
    abilities::PSYCHIC_SURGE,
    abilities::PRESSURE,
    abilities::MOLD_BREAKER,
    abilities::TERAVOLT,
    abilities::TURBOBLAZE,
    abilities::UNNERVE,
    abilities::FRISK,
    abilities::ANTICIPATION,
    abilities::FOREWARN,
    abilities::DOWNLOAD,
    abilities::CURIOUS_MEDICINE,
    abilities::HOSPITALITY,
    abilities::SUPERSWEET_SYRUP,
    abilities::SUPREME_OVERLORD,
    abilities::INTREPID_SWORD,
    abilities::DAUNTLESS_SHIELD,
    abilities::SCREEN_CLEANER,
    abilities::KLUTZ,
    abilities::AIR_LOCK,
    abilities::CLOUD_NINE,
    abilities::NATURAL_CURE,
    abilities::REGENERATOR,
    abilities::STANCE_CHANGE,
    abilities::DISGUISE,
    abilities::ICE_FACE,
    abilities::ZERO_TO_HERO,
    abilities::SHIELDS_DOWN,
    abilities::HUNGER_SWITCH,
    abilities::FORECAST,
    abilities::FLOWER_GIFT,
    abilities::MIMICRY,
    abilities::SCHOOLING,
    abilities::BATTLE_BOND,
    abilities::GULP_MISSILE,
    abilities::EMBODY_ASPECT_CORNERSTONE,
    abilities::EMBODY_ASPECT_HEARTHFLAME,
    abilities::EMBODY_ASPECT_TEAL,
    abilities::EMBODY_ASPECT_WELLSPRING,
    abilities::TERA_SHIFT,
    abilities::TERAFORM_ZERO,
    abilities::TERA_SHELL,
    abilities::POWER_CONSTRUCT,
    abilities::PROTOSYNTHESIS,
    abilities::QUARK_DRIVE,
    abilities::SWORD_OF_RUIN,
    abilities::BEADS_OF_RUIN,
    abilities::TABLETS_OF_RUIN,
    abilities::VESSEL_OF_RUIN,
    abilities::DARK_AURA,
    abilities::FAIRY_AURA,
    abilities::AURA_BREAK,
    abilities::ORICHALCUM_PULSE,
    abilities::HADRON_ENGINE,
    abilities::SLOW_START,
    abilities::TRUANT,
    abilities::AS_ONE_GLASTRIER,
    abilities::AS_ONE_SPECTRIER,
    abilities::COSTAR,
    abilities::IMPOSTER,
    abilities::ILLUSION,
    abilities::COMMANDER,
    abilities::NEUTRALIZING_GAS,
    abilities::TRACE,
    abilities::DESOLATE_LAND,
    abilities::PRIMORDIAL_SEA,
    abilities::DELTA_STREAM,
    abilities::ZEN_MODE,
    abilities::MULTITYPE,
    // RKS System is left out: Silvally isn't in Gen 9.
    // Damage, stat and type modifiers.
    abilities::ADAPTABILITY,
    abilities::AERILATE,
    abilities::ANALYTIC,
    abilities::BATTERY,
    abilities::BATTLE_ARMOR,
    abilities::BLAZE,
    abilities::CHLOROPHYLL,
    abilities::CORROSION,
    abilities::DEFEATIST,
    abilities::DRAGONS_MAW,
    abilities::DRAGONIZE,
    abilities::EELEVATE,
    abilities::FILTER,
    abilities::FIRE_MANE,
    abilities::FLARE_BOOST,
    abilities::FLUFFY,
    abilities::FRIEND_GUARD,
    abilities::FUR_COAT,
    abilities::GALVANIZE,
    abilities::GORILLA_TACTICS,
    abilities::GRASS_PELT,
    abilities::GUTS,
    abilities::HEATPROOF,
    abilities::HEAVY_METAL,
    abilities::HUGE_POWER,
    abilities::HUSTLE,
    abilities::ICE_SCALES,
    abilities::INFILTRATOR,
    abilities::IRON_FIST,
    abilities::LIGHT_METAL,
    abilities::LIQUID_VOICE,
    abilities::LONG_REACH,
    abilities::MARVEL_SCALE,
    abilities::MEGA_LAUNCHER,
    abilities::MERCILESS,
    abilities::MINDS_EYE,
    abilities::MINUS,
    abilities::MULTISCALE,
    abilities::NEUROFORCE,
    abilities::NORMALIZE,
    abilities::OVERGROW,
    abilities::PARENTAL_BOND,
    abilities::PIERCING_DRILL,
    abilities::PIXILATE,
    abilities::PLUS,
    abilities::POWER_SPOT,
    abilities::PRISM_ARMOR,
    abilities::PUNK_ROCK,
    abilities::PURE_POWER,
    abilities::QUICK_FEET,
    abilities::RECKLESS,
    abilities::REFRIGERATE,
    abilities::RIVALRY,
    abilities::ROCKY_PAYLOAD,
    abilities::SAND_FORCE,
    abilities::SAND_RUSH,
    abilities::SCRAPPY,
    abilities::SERENE_GRACE,
    abilities::SHADOW_SHIELD,
    abilities::SHARPNESS,
    abilities::SHEER_FORCE,
    abilities::SHELL_ARMOR,
    abilities::SKILL_LINK,
    abilities::SLUSH_RUSH,
    abilities::SNIPER,
    abilities::SOLAR_POWER,
    abilities::SOLID_ROCK,
    abilities::STAKEOUT,
    abilities::STEELWORKER,
    abilities::STEELY_SPIRIT,
    abilities::STENCH,
    abilities::STRONG_JAW,
    abilities::SUPER_LUCK,
    abilities::SURGE_SURFER,
    abilities::SWARM,
    abilities::SWIFT_SWIM,
    abilities::TECHNICIAN,
    abilities::THICK_FAT,
    abilities::TINTED_LENS,
    abilities::TORRENT,
    abilities::TOUGH_CLAWS,
    abilities::TOXIC_BOOST,
    abilities::TRANSISTOR,
    abilities::UNAWARE,
    abilities::UNSEEN_FIST,
    abilities::WATER_BUBBLE,
    // Aerilate, Dragonize, Eelevate, Fire Mane, Parental Bond, Piercing
    // Drill and the Champions mod's Unseen Fist only exist on Mega
    // Evolutions, which the fuzzer reaches.
    // Mega Evolution: Mega Sol (Meganium-Mega), `resolve/mega.rs`.
    abilities::MEGA_SOL,
    // Immunities, contact and reactive effects, status and
    // boost protection).
    abilities::LEVITATE,
    abilities::STATIC,
    abilities::AFTERMATH,
    abilities::ANGER_POINT,
    abilities::ARMOR_TAIL,
    abilities::AROMA_VEIL,
    abilities::BERSERK,
    abilities::BIG_PECKS,
    abilities::BULLETPROOF,
    abilities::CLEAR_BODY,
    abilities::COMPETITIVE,
    abilities::CONTRARY,
    abilities::CURSED_BODY,
    abilities::CUTE_CHARM,
    abilities::DAMP,
    abilities::DEFIANT,
    abilities::DRY_SKIN,
    abilities::EARTH_EATER,
    abilities::EFFECT_SPORE,
    abilities::ELECTROMORPHOSIS,
    abilities::FLAME_BODY,
    abilities::FLASH_FIRE,
    abilities::FLOWER_VEIL,
    abilities::GOOD_AS_GOLD,
    abilities::GOOEY,
    abilities::HYPER_CUTTER,
    abilities::IMMUNITY,
    abilities::INNARDS_OUT,
    abilities::INNER_FOCUS,
    abilities::INSOMNIA,
    abilities::JUSTIFIED,
    abilities::KEEN_EYE,
    abilities::LEAF_GUARD,
    abilities::LIGHTNING_ROD,
    abilities::LIMBER,
    abilities::MAGIC_BOUNCE,
    abilities::MAGMA_ARMOR,
    abilities::MIRROR_ARMOR,
    abilities::MOTOR_DRIVE,
    abilities::MUMMY,
    abilities::OBLIVIOUS,
    abilities::OVERCOAT,
    abilities::OWN_TEMPO,
    abilities::POISON_POINT,
    abilities::POISON_TOUCH,
    abilities::PURIFYING_SALT,
    abilities::QUEENLY_MAJESTY,
    abilities::ROUGH_SKIN,
    abilities::SAND_SPIT,
    abilities::SAP_SIPPER,
    abilities::SHIELD_DUST,
    abilities::SOUNDPROOF,
    abilities::SPICY_SPRAY,
    abilities::STAMINA,
    abilities::STEADFAST,
    abilities::STURDY,
    abilities::SUCTION_CUPS,
    abilities::SWEET_VEIL,
    abilities::SYNCHRONIZE,
    abilities::TELEPATHY,
    abilities::TOXIC_DEBRIS,
    abilities::VITAL_SPIRIT,
    abilities::VOLT_ABSORB,
    abilities::WANDERING_SPIRIT,
    abilities::WATER_ABSORB,
    abilities::WEAK_ARMOR,
    abilities::WHITE_SMOKE,
    abilities::DAZZLING,
    abilities::STORM_DRAIN,
    abilities::WELL_BAKED_BODY,
    abilities::WIND_RIDER,
    abilities::WIND_POWER,
    abilities::WONDER_GUARD,
    abilities::COTTON_DOWN,
    abilities::TANGLING_HAIR,
    abilities::IRON_BARBS,
    abilities::LINGERING_AROMA,
    abilities::PERISH_BODY,
    abilities::RATTLED,
    abilities::SEED_SOWER,
    abilities::STEAM_ENGINE,
    abilities::THERMAL_EXCHANGE,
    abilities::WATER_COMPACTION,
    abilities::ANGER_SHELL,
    abilities::COLOR_CHANGE,
    abilities::TOXIC_CHAIN,
    abilities::POISON_PUPPETEER,
    abilities::WATER_VEIL,
    abilities::PASTEL_VEIL,
    abilities::FULL_METAL_BODY,
    abilities::GUARD_DOG,
    abilities::SIMPLE,
    abilities::COMATOSE,
    // Residual, priority, accuracy, KO and miscellaneous.
    abilities::ARENA_TRAP,
    abilities::BAD_DREAMS,
    abilities::BEAST_BOOST,
    abilities::CHILLING_NEIGH,
    abilities::COMPOUND_EYES,
    abilities::DANCER,
    abilities::EARLY_BIRD,
    abilities::EMERGENCY_EXIT,
    abilities::GALE_WINGS,
    abilities::GRIM_NEIGH,
    abilities::HEALER,
    abilities::HONEY_GATHER,
    abilities::HYDRATION,
    abilities::ICE_BODY,
    abilities::ILLUMINATE,
    abilities::LIBERO,
    abilities::LIQUID_OOZE,
    abilities::MAGIC_GUARD,
    abilities::MAGNET_PULL,
    abilities::MOODY,
    abilities::MOXIE,
    abilities::MYCELIUM_MIGHT,
    abilities::NO_GUARD,
    abilities::OPPORTUNIST,
    abilities::POISON_HEAL,
    abilities::POWER_OF_ALCHEMY,
    abilities::PRANKSTER,
    abilities::PROPELLER_TAIL,
    abilities::PROTEAN,
    abilities::QUICK_DRAW,
    abilities::RAIN_DISH,
    abilities::RECEIVER,
    abilities::ROCK_HEAD,
    abilities::RUN_AWAY,
    abilities::SAND_VEIL,
    abilities::SHADOW_TAG,
    abilities::SHED_SKIN,
    abilities::SNOW_CLOAK,
    abilities::SOUL_HEART,
    abilities::SPEED_BOOST,
    abilities::STALL,
    abilities::STALWART,
    abilities::TANGLED_FEET,
    abilities::TRIAGE,
    abilities::VICTORY_STAR,
    abilities::WIMP_OUT,
    abilities::WONDER_SKIN,
    // Item-related abilities.
    abilities::BALL_FETCH,
    abilities::CHEEK_POUCH,
    abilities::CUD_CHEW,
    abilities::GLUTTONY,
    abilities::HARVEST,
    abilities::MAGICIAN,
    abilities::PICKPOCKET,
    abilities::PICKUP,
    abilities::RIPEN,
    abilities::STICKY_HOLD,
    abilities::SYMBIOSIS,
    abilities::UNBURDEN,
    // Champions Reg M-C: Lucario-Mega-Z's Aura Guard.
    abilities::AURA_GUARD,
];

/// Items pikapro implements completely, grouped by topic.
pub const IMPLEMENTED_ITEMS: &[ItemId] = &[
    // Held items that stay held.
    items::LEFTOVERS,
    items::CHOICE_BAND,
    items::CHOICE_SCARF,
    items::CHOICE_SPECS,
    items::LIFE_ORB,
    // Champions-legal.
    items::BIG_ROOT,
    items::BLACK_BELT,
    items::BLACK_GLASSES,
    items::BRIGHT_POWDER,
    items::CHARCOAL,
    items::DAMP_ROCK,
    items::DRAGON_FANG,
    items::EXPERT_BELT,
    items::FAIRY_FEATHER,
    items::FOCUS_BAND,
    items::HARD_STONE,
    items::HEAT_ROCK,
    items::ICY_ROCK,
    items::IRON_BALL,
    items::KINGS_ROCK,
    items::LIGHT_BALL,
    items::LIGHT_CLAY,
    items::MAGNET,
    items::METAL_COAT,
    items::METRONOME,
    items::MIRACLE_SEED,
    items::MUSCLE_BAND,
    items::MYSTIC_WATER,
    items::NEVER_MELT_ICE,
    items::POISON_BARB,
    items::QUICK_CLAW,
    items::SCOPE_LENS,
    items::SHARP_BEAK,
    items::SHED_SHELL,
    items::SHELL_BELL,
    items::SILK_SCARF,
    items::SILVER_POWDER,
    items::SMOOTH_ROCK,
    items::SOFT_SAND,
    items::SPELL_TAG,
    items::TWISTED_SPOON,
    items::WIDE_LENS,
    items::WISE_GLASSES,
    items::ZOOM_LENS,
    // The rest of Gen 9's.
    items::ASSAULT_VEST,
    items::EVIOLITE,
    items::ROCKY_HELMET,
    items::BLACK_SLUDGE,
    items::TOXIC_ORB,
    items::FLAME_ORB,
    items::COVERT_CLOAK,
    items::CLEAR_AMULET,
    items::SAFETY_GOGGLES,
    items::UTILITY_UMBRELLA,
    items::ABILITY_SHIELD,
    items::PROTECTIVE_PADS,
    items::HEAVY_DUTY_BOOTS,
    items::TERRAIN_EXTENDER,
    items::BINDING_BAND,
    items::GRIP_CLAW,
    items::LAGGING_TAIL,
    items::RING_TARGET,
    items::FLOAT_STONE,
    items::LOADED_DICE,
    items::PUNCHING_GLOVE,
    items::RAZOR_CLAW,
    items::RAZOR_FANG,
    items::POWER_ANKLET,
    items::POWER_BAND,
    items::POWER_BELT,
    items::POWER_BRACER,
    items::POWER_LENS,
    items::POWER_WEIGHT,
    items::DRACO_PLATE,
    items::DREAD_PLATE,
    items::EARTH_PLATE,
    items::FIST_PLATE,
    items::FLAME_PLATE,
    items::ICICLE_PLATE,
    items::INSECT_PLATE,
    items::IRON_PLATE,
    items::MEADOW_PLATE,
    items::MIND_PLATE,
    items::PIXIE_PLATE,
    items::SKY_PLATE,
    items::SPLASH_PLATE,
    items::SPOOKY_PLATE,
    items::STONE_PLATE,
    items::TOXIC_PLATE,
    items::ZAP_PLATE,
    items::ADAMANT_ORB,
    items::ADAMANT_CRYSTAL,
    items::LUSTROUS_ORB,
    items::LUSTROUS_GLOBE,
    items::GRISEOUS_ORB,
    items::GRISEOUS_CORE,
    items::SOUL_DEW,
    // Rusted Sword and Rusted Shield crown Zacian and Zamazenta
    // (`items::held::battle_start`; Intrepid Sword and Dauntless Shield are
    // abilities, listed with them).
    items::RUSTED_SWORD,
    items::RUSTED_SHIELD,
    items::CORNERSTONE_MASK,
    items::HEARTHFLAME_MASK,
    items::WELLSPRING_MASK,
    items::DESTINY_KNOT,
    // No battle effect (Poké Balls, evolution items, sweets, ...).
    items::BEAST_BALL,
    items::CHERISH_BALL,
    items::DIVE_BALL,
    items::DREAM_BALL,
    items::DUSK_BALL,
    items::FAST_BALL,
    items::FRIEND_BALL,
    items::GREAT_BALL,
    items::HEAL_BALL,
    items::HEAVY_BALL,
    items::LEVEL_BALL,
    items::LOVE_BALL,
    items::LURE_BALL,
    items::LUXURY_BALL,
    items::MASTER_BALL,
    items::MOON_BALL,
    items::NEST_BALL,
    items::NET_BALL,
    items::PARK_BALL,
    items::POKE_BALL,
    items::PREMIER_BALL,
    items::QUICK_BALL,
    items::REPEAT_BALL,
    items::SAFARI_BALL,
    items::SPORT_BALL,
    items::STRANGE_BALL,
    items::TIMER_BALL,
    items::ULTRA_BALL,
    items::AUSPICIOUS_ARMOR,
    items::BERRY_SWEET,
    items::BIG_NUGGET,
    items::BOTTLE_CAP,
    items::CHIPPED_POT,
    items::CLOVER_SWEET,
    items::CRACKED_POT,
    items::DAWN_STONE,
    items::DRAGON_SCALE,
    items::DUBIOUS_DISC,
    items::DUSK_STONE,
    items::ELECTIRIZER,
    items::FIRE_STONE,
    items::FLOWER_SWEET,
    items::GALARICA_CUFF,
    items::GALARICA_WREATH,
    items::GOLD_BOTTLE_CAP,
    items::ICE_STONE,
    items::LEAF_STONE,
    items::LOVE_SWEET,
    items::MAGMARIZER,
    items::MALICIOUS_ARMOR,
    items::MASTERPIECE_TEACUP,
    items::METAL_ALLOY,
    items::MOON_STONE,
    items::OVAL_STONE,
    items::PRETTY_FEATHER,
    items::PRISM_SCALE,
    items::PROTECTOR,
    items::RARE_BONE,
    items::REAPER_CLOTH,
    items::RIBBON_SWEET,
    items::SHINY_STONE,
    items::STAR_SWEET,
    items::STRAWBERRY_SWEET,
    items::SUN_STONE,
    items::SWEET_APPLE,
    items::SYRUPY_APPLE,
    items::TART_APPLE,
    items::THUNDER_STONE,
    items::UNREMARKABLE_TEACUP,
    items::UP_GRADE,
    items::WATER_STONE,
    // Berries, consumed items, item and ability transfer.
    items::SITRUS_BERRY,
    items::FOCUS_SASH,
    items::ABSORB_BULB,
    items::ADRENALINE_ORB,
    items::AGUAV_BERRY,
    items::AIR_BALLOON,
    items::APICOT_BERRY,
    items::ASPEAR_BERRY,
    items::BABIRI_BERRY,
    items::BLUNDER_POLICY,
    items::CELL_BATTERY,
    items::CHARTI_BERRY,
    items::CHERI_BERRY,
    items::CHESTO_BERRY,
    items::CHILAN_BERRY,
    items::CHOPLE_BERRY,
    items::COBA_BERRY,
    items::COLBUR_BERRY,
    items::CUSTAP_BERRY,
    items::EJECT_BUTTON,
    items::EJECT_PACK,
    items::ELECTRIC_SEED,
    items::ENIGMA_BERRY,
    items::FIGY_BERRY,
    items::GANLON_BERRY,
    items::GRASSY_SEED,
    items::GREPA_BERRY,
    items::HABAN_BERRY,
    items::HONDEW_BERRY,
    items::IAPAPA_BERRY,
    items::JABOCA_BERRY,
    items::KASIB_BERRY,
    items::KEBIA_BERRY,
    items::KEE_BERRY,
    items::KELPSY_BERRY,
    items::LANSAT_BERRY,
    items::LEPPA_BERRY,
    items::LIECHI_BERRY,
    items::LUM_BERRY,
    items::LUMINOUS_MOSS,
    items::MAGO_BERRY,
    items::MARANGA_BERRY,
    items::MENTAL_HERB,
    items::MICLE_BERRY,
    items::MIRROR_HERB,
    items::MISTY_SEED,
    items::NORMAL_GEM,
    items::OCCA_BERRY,
    items::ORAN_BERRY,
    items::PASSHO_BERRY,
    items::PAYAPA_BERRY,
    items::PECHA_BERRY,
    items::PERSIM_BERRY,
    items::PETAYA_BERRY,
    items::POMEG_BERRY,
    items::POWER_HERB,
    items::PSYCHIC_SEED,
    items::QUALOT_BERRY,
    items::RAWST_BERRY,
    items::RED_CARD,
    items::RINDO_BERRY,
    items::ROOM_SERVICE,
    items::ROSELI_BERRY,
    items::ROWAP_BERRY,
    items::SALAC_BERRY,
    items::SHUCA_BERRY,
    items::SNOWBALL,
    items::STARF_BERRY,
    items::STICKY_BARB,
    items::TAMATO_BERRY,
    items::TANGA_BERRY,
    items::THROAT_SPRAY,
    items::WACAN_BERRY,
    items::WEAKNESS_POLICY,
    items::WHITE_HERB,
    items::WIKI_BERRY,
    items::YACHE_BERRY,
    // Booster Energy starts Protosynthesis and Quark Drive.
    items::BOOSTER_ENERGY,
    // Mega Evolution: the Mega Stones of Champions. `item_pool`
    // leaves them out; `random_team` gives a species its own.
    items::ABOMASITE,
    items::ABSOLITE,
    items::AERODACTYLITE,
    items::AGGRONITE,
    items::ALAKAZITE,
    items::ALTARIANITE,
    items::AMPHAROSITE,
    items::AUDINITE,
    items::BANETTITE,
    items::BARBARACITE,
    items::BEEDRILLITE,
    items::BLASTOISINITE,
    items::BLAZIKENITE,
    items::CAMERUPTITE,
    items::CHANDELURITE,
    items::CHARIZARDITE_X,
    items::CHARIZARDITE_Y,
    items::CHESNAUGHTITE,
    items::CHIMECHITE,
    items::CLEFABLITE,
    items::CRABOMINITE,
    items::DELPHOXITE,
    items::DRAGALGITE,
    items::DRAGONINITE,
    items::DRAMPANITE,
    items::EELEKTROSSITE,
    items::EMBOARITE,
    items::EXCADRITE,
    items::FALINKSITE,
    items::FERALIGITE,
    items::FLOETTITE,
    items::FROSLASSITE,
    items::GALLADITE,
    items::GARCHOMPITE,
    items::GARDEVOIRITE,
    items::GENGARITE,
    items::GLALITITE,
    items::GLIMMORANITE,
    items::GOLURKITE,
    items::GRENINJITE,
    items::GYARADOSITE,
    items::HAWLUCHANITE,
    items::HERACRONITE,
    items::HOUNDOOMINITE,
    items::KANGASKHANITE,
    items::LOPUNNITE,
    items::LUCARIONITE,
    items::MALAMARITE,
    items::MANECTITE,
    items::MAWILITE,
    items::MEDICHAMITE,
    items::MEGANIUMITE,
    items::MEOWSTICITE,
    items::METAGROSSITE,
    items::PIDGEOTITE,
    items::PINSIRITE,
    items::PYROARITE,
    items::RAICHUNITE_X,
    items::RAICHUNITE_Y,
    items::SABLENITE,
    items::SCEPTILITE,
    items::SCIZORITE,
    items::SCOLIPITE,
    items::SCOVILLAINITE,
    items::SCRAFTINITE,
    items::SHARPEDONITE,
    items::SKARMORITE,
    items::SLOWBRONITE,
    items::STARAPTITE,
    items::STARMINITE,
    items::STEELIXITE,
    items::SWAMPERTITE,
    items::TYRANITARITE,
    items::VENUSAURITE,
    items::VICTREEBELITE,
    // Champions Reg M-C: the six new Mega Stones, and Leek.
    items::ABSOLITE_Z,
    items::BAXCALIBRITE,
    items::GARCHOMPITE_Z,
    items::GOLISOPITE,
    items::LUCARIONITE_Z,
    items::SALAMENCITE,
    items::LEEK,
];

/// The implemented items the format allows (not `Past` in its dex).
pub fn item_pool(format: &Format) -> Vec<ItemId> {
    IMPLEMENTED_ITEMS
        .iter()
        .copied()
        .filter(|&i| {
            let data = format.dex.item(i);
            data.nonstandard.is_none() && data.mega.is_empty() && !data.is_z_crystal
        })
        .collect()
}

/// The Mega Stones that work for `species` in a format with Mega
/// Evolution (standard in its dex).
pub(crate) fn mega_stones(format: &Format, species: SpeciesId) -> Vec<ItemId> {
    if !format.gimmicks.contains(Gimmick::MegaEvolve) {
        return Vec::new();
    }
    IMPLEMENTED_ITEMS
        .iter()
        .copied()
        .filter(|&i| {
            let data = format.dex.item(i);
            data.nonstandard.is_none() && data.mega.iter().any(|&(base, _)| base == species)
        })
        .collect()
}

/// A random team drawn from the conformance pools: each Pokémon gets one
/// of its species' legal abilities that pikapro implements (No Ability
/// if none) and, 60% of the time, an implemented item the format allows
/// (no duplicates under Item Clause). In a format with Mega
/// Evolution, a species with a Mega Stone holds it 60% of the time (the
/// other items as before otherwise).
pub fn random_team(format: &Format, rng: &mut BattleRng) -> Team {
    let mut pool = species_pool(format);
    let move_pool = move_pool(format);
    let item_pool = item_pool(format);
    rng.shuffle(&mut pool);
    let size = format.team_size.1.min(6) as usize;
    // Where team preview brings the whole team (gen9ou), a team of any
    // size plays: a quarter of them have one to five Pokémon.
    let size = if brings_all(format) && rng.random(4) == 0 { 1 + rng.random(5) as usize } else { size };
    let mut used_items: Vec<ItemId> = Vec::new();
    let sets = pool
        .into_iter()
        .take(size)
        .map(|sp| {
            let mut ms = move_pool.clone();
            rng.shuffle(&mut ms);
            let n = 1 + rng.random(4) as usize;
            let investment = match format.stat_system {
                StatSystem::EffortValues => Investment::Evs { evs: spread(rng, 252, 510), ivs: Stats::splat(31) },
                StatSystem::StatPoints => Investment::StatPoints(spread(rng, 32, 66)),
            };
            let slots = format.dex.species(sp).abilities;
            let legal: Vec<AbilityId> = [Some(slots.primary), slots.secondary, slots.hidden]
                .into_iter()
                .flatten()
                .filter(|a| IMPLEMENTED_ABILITIES.contains(a))
                .collect();
            let ability =
                if legal.is_empty() { abilities::NO_ABILITY } else { legal[rng.random(legal.len() as u32) as usize] };
            // Item Clause (VGC, Battle Spot): no two Pokémon hold the same item.
            let free = |i: &ItemId| !(format.clauses.item && used_items.contains(i));
            let stones: Vec<ItemId> = mega_stones(format, sp).into_iter().filter(free).collect();
            let choices: Vec<ItemId> = item_pool.iter().copied().filter(free).collect();
            let item = if !stones.is_empty() && rng.random(10) < 6 {
                Some(stones[rng.random(stones.len() as u32) as usize])
            } else if choices.is_empty() || rng.random(10) < 4 {
                None
            } else {
                Some(choices[rng.random(choices.len() as u32) as usize])
            };
            let mut set = PokemonSet::new(sp)
                .with_level(fuzz_level(format, rng))
                .with_ability(ability)
                .with_nature(Nature::ALL[rng.random(25) as usize])
                .with_tera_type(tera_type(format, sp, rng))
                .with_investment(investment)
                .with_moves(&ms[..n]);
            if let Some(item) = item {
                used_items.push(item);
                set = set.with_item(item);
            }
            set.gender = random_gender(format, sp, rng);
            set
        })
        .collect();
    Team::new(sets)
}

/// The level of the fuzzer's sets: 50, or 100 in gen9ou, whose sets play
/// at their own level (100 by default) with the cartridge's overflows
/// (damage wraps at 16 bits, which level 100 reaches far more often).
fn fuzz_level(format: &Format, rng: &mut BattleRng) -> u8 {
    if format.random_teams {
        // The random-battle species' levels (62 to 100): an Illusion's
        // disguise and its user rarely agree.
        return 62 + rng.random(39) as u8;
    }
    if brings_all(format) { 100 } else { 50 }
}

/// Team preview without a picked team size: the tier formats (gen9ou).
fn brings_all(format: &Format) -> bool {
    format.team_preview == pikapro::TeamPreview::All
}

/// Half the sets leave their gender to the battle (a species' fixed
/// one, or Showdown's draw), the others name a gender the species can have
/// (never `N`, which the export format can't write).
fn random_gender(format: &Format, species: SpeciesId, rng: &mut BattleRng) -> Option<Gender> {
    if rng.random(2) == 0 {
        return None;
    }
    match format.dex.species(species).gender {
        GenderRule::Male => Some(Gender::Male),
        GenderRule::Female => Some(Gender::Female),
        GenderRule::Genderless => None,
        GenderRule::Ratio { .. } => Some(*rng.sample(&[Gender::Male, Gender::Female])),
    }
}

/// A random Tera type (Stellar included), or the species' forced one
/// (Ogerpon, Terapagos: `requiredTeraType`), drawn either way.
fn tera_type(format: &Format, species: SpeciesId, rng: &mut BattleRng) -> Type {
    let ty = Type::TERA[rng.random(Type::TERA.len() as u32) as usize];
    format.dex.species(species).required_tera_type.map_or(ty, |t| t.ty())
}

fn spread(rng: &mut BattleRng, max: u32, total: u32) -> Stats<u8> {
    let mut s = Stats::splat(0u8);
    let mut left = total;
    let mut order = Stat::ALL;
    rng.shuffle(&mut order);
    for stat in order {
        let v = rng.random(max.min(left) + 1);
        s[stat] = v as u8;
        left -= v;
    }
    s
}

/// Plays a random battle on pikapro and records it as a scenario.
pub fn random_scenario(format: &Format, seed: u64) -> Scenario {
    scenario_as(format, seed, None, false).0
}

/// [`scenario_as`] (not mirrored) with the teams of [`draw_view_teams`]: what `channel-conformance` plays.
pub fn view_scenario(format: &Format, seed: u64, target: Option<view::Target>) -> Scenario {
    let (rng, teams, _) = draw_view_teams(format, seed, target);
    play_random(format, seed, rng, teams)
}

/// Battle `seed`'s two teams, as `cargo conformance` draws them:
/// [`random_team`] twice from the seed's stream, then `target`'s sets
/// (whether they landed). Arceus and Silvally keep their formes with any
/// item, so the raw-log comparison covers Judgment and Multi-Attack with
/// and without a Plate or Memory. Returns the stream, to draw the
/// battle's choices from.
pub fn draw_teams(format: &Format, seed: u64, target: Option<view::Target>) -> (BattleRng, [Team; 2], bool) {
    draw_teams_as(format, seed, target, false)
}

/// [`draw_teams`], with p2's team a copy of p1's if `mirror` (after
/// `target`'s sets, from the same stream): every Pokémon meets its double,
/// so ties in speed (the speed sorts' draws, Room Service, Unaware
/// holders) come up far more often. A mirrored battle has the target
/// if p1's team got the defining set.
pub fn draw_teams_as(
    format: &Format,
    seed: u64,
    target: Option<view::Target>,
    mirror: bool,
) -> (BattleRng, [Team; 2], bool) {
    let mut rng = BattleRng::new(Seed::fast(seed));
    let mut teams = [random_team(format, &mut rng), random_team(format, &mut rng)];
    let landed = target.map_or([false; 2], |t| t.apply_to_each(format, &mut rng, &mut teams));
    if mirror {
        teams[1] = teams[0].clone();
        return (rng, teams, landed[0]);
    }
    (rng, teams, landed[0] || landed[1])
}

/// A random battle played on pikapro and recorded as a scenario: the
/// fuzzer's teams, with sets forced for a mechanic if `target`
/// ([`view::Target`]: `target.apply` from the same stream) and mirrored if
/// `mirror` ([`draw_teams_as`], `cargo conformance --mirror`), then the
/// random choices. Also returns whether the target's defining set landed
/// ([`view::Target::apply`]; true without a target).
pub fn scenario_as(format: &Format, seed: u64, target: Option<view::Target>, mirror: bool) -> (Scenario, bool) {
    let (rng, teams, landed) = draw_teams_as(format, seed, target, mirror);
    (play_random(format, seed, rng, teams), landed || target.is_none())
}

/// [`draw_teams`] for the player-view harnesses (`view-conformance
/// --source fuzz` or a target, `channel-conformance`): Arceus and Silvally
/// in their base forme. A forme without its Plate or Memory has details
/// naming a type it doesn't have (Multitype follows the item), which no
/// client can see through, and legal teams can't reach. They play their
/// base forme without an item (or not at all, if it's there).
pub fn draw_view_teams(format: &Format, seed: u64, target: Option<view::Target>) -> (BattleRng, [Team; 2], bool) {
    let (rng, mut teams, landed) = draw_teams(format, seed, target);
    use pikapro_data::gen9::species::{ARCEUS, SILVALLY};
    for team in teams.iter_mut() {
        let mut i = 0;
        while i < team.sets.len() {
            let species = team.sets[i].species;
            let base = format.dex.species(species).base_species;
            if matches!(base, ARCEUS | SILVALLY) {
                if species != base && team.sets.iter().any(|s| s.species == base) {
                    team.sets.remove(i);
                    continue;
                }
                team.sets[i].species = base;
                team.sets[i].item = None;
            }
            i += 1;
        }
    }
    (rng, teams, landed)
}

/// Plays a battle between `teams` with random choices from `rng` (the
/// stream the teams were drawn from) and records it as a scenario.
fn play_random(format: &Format, seed: u64, rng: BattleRng, teams: [Team; 2]) -> Scenario {
    let nicknames = random_nicknames(&teams, seed);
    let shiny = random_shiny(&teams, seed);
    play_random_with(format, rng, teams, nicknames, shiny)
}

/// [`play_random`] with the teams' nicknames and shiny sets given.
fn play_random_with(
    format: &Format,
    mut rng: BattleRng,
    teams: [Team; 2],
    nicknames: [Vec<String>; 2],
    shiny: [Vec<bool>; 2],
) -> Scenario {
    let words = [0, 1, 2, 3].map(|_| rng.random(1 << 16) as u16);
    let mut battle = new_battle(format, &teams, words);
    let mut s = Scenario {
        format: format.clone(),
        seed: words,
        teams,
        choices: Vec::new(),
        nicknames,
        shiny,
        listed: None,
        team_seeds: None,
    };
    let names = s.name_table();
    while !battle.is_over() {
        for side in SideId::BOTH {
            let legal = battle.legal_actions(side);
            if legal.is_waiting() {
                continue;
            }
            let choice = legal.sample(&mut rng);
            // Sometimes spelled another way, or `default`.
            let (text, choice) = choices::spell_choice(&battle, side, choice, &names[side.index()], &mut rng);
            s.choices.push((side, text));
            battle.submit(side, choice).expect("sampled choices are legal");
        }
    }
    s
}

/// Nicknames for about a third of the Pokémon, drawn from their own
/// stream so the teams and choices of a seed stay what they were. The
/// pool has spaces, punctuation, a non-ASCII letter, another species'
/// name and a name Showdown cuts to 20 characters.
pub fn random_nicknames(teams: &[Team; 2], seed: u64) -> [Vec<String>; 2] {
    const POOL: &[&str] = &[
        "Sparky",
        "Mr. Bubbles",
        "Sir Hops-a-Lot",
        "Zoë",
        "Pikachu",
        "Chomp",
        "Big Tony's Pal",
        "ThisNameIsFarTooLongToKeep",
        "Nibbles",
        "X",
    ];
    let mut rng = BattleRng::new(Seed::fast(seed ^ 0x6E69_636B));
    teams.each_ref().map(|team| {
        let mut pool: Vec<&str> = POOL.to_vec();
        rng.shuffle(&mut pool);
        let mut names = pool.into_iter();
        team.sets
            .iter()
            .map(|_| if rng.random(3) == 0 { names.next().unwrap_or("").to_string() } else { String::new() })
            .collect()
    })
}

/// One Pokémon in eight is shiny, drawn from its own stream.
pub fn random_shiny(teams: &[Team; 2], seed: u64) -> [Vec<bool>; 2] {
    let mut rng = BattleRng::new(Seed::fast(seed ^ 0x7368_696E));
    teams.each_ref().map(|team| team.sets.iter().map(|_| rng.random(8) == 0).collect())
}

/// `format` without its team clauses, for the battles compared with
/// Showdown: its simulator, the oracle, never runs the team validator, so
/// a fuzzed team with two formes of one species plays as drawn.
pub fn oracle_format(format: &Format) -> Format {
    Format { clauses: pikapro::Clauses::default(), ..format.clone() }
}

pub(crate) fn new_battle(format: &Format, teams: &[Team; 2], seed: [u16; 4]) -> Battle {
    try_new_battle(format, teams, seed).expect("valid teams")
}

/// The battle the oracle is compared with: [`oracle_format`], Showdown's
/// Gen 5 stream from `seed`, the draws traced.
fn try_new_battle(format: &Format, teams: &[Team; 2], seed: [u16; 4]) -> Result<Battle, pikapro::TeamError> {
    let options = BattleOptions::new(Seed::ShowdownGen5(seed)).with_rng_trace();
    Battle::new(oracle_format(format), teams.clone(), options)
}

/// Replays a scenario on pikapro and renders the raw log, `|split|`
/// blocks included, as Showdown's `update` chunks carry it.
pub fn run_ours(s: &Scenario) -> Vec<String> {
    render_raw(s, &replay(s, false))
}

/// A replayed scenario.
#[derive(Debug)]
pub struct Replay {
    pub battle: Battle,
    /// Where the events of each resolution end: Showdown's `update`
    /// chunks, the first being `Battle::new`'s. A slice is rendered on its
    /// own, as Showdown never amends a line it has sent.
    pub chunk_ends: Vec<usize>,
    /// The requests sent after each chunk (`|request|<side>|<JSON>`),
    /// if asked for.
    pub requests: Vec<Vec<String>>,
}

impl Replay {
    /// The events of each chunk.
    pub fn chunks(&self) -> impl Iterator<Item = &[pikapro::BattleEvent]> {
        let events = self.battle.events();
        let starts = core::iter::once(0).chain(self.chunk_ends.iter().copied());
        starts.zip(self.chunk_ends.iter().copied()).map(move |(a, b)| &events[a..b])
    }
}

/// Replays a scenario on pikapro, with the requests sent after each
/// resolution if `requests`. Panics if pikapro rejects the teams or a
/// recorded choice ([`try_replay`] reports it instead).
pub fn replay(s: &Scenario, requests: bool) -> Replay {
    try_replay(s, requests).unwrap_or_else(|e| panic!("{e}"))
}

/// [`replay`], or why pikapro can't replay the scenario: invalid teams,
/// or a recorded choice it can't read or rejects (a fixture recorded
/// before a change made that choice illegal). Doesn't panic for those, so
/// callers can report them under `panic = "abort"` (the release profile).
pub fn try_replay(s: &Scenario, requests: bool) -> Result<Replay, String> {
    let mut battle = try_new_battle(&s.format, &s.teams, s.seed).map_err(|e| format!("invalid teams: {e}"))?;
    let json = renderer(s, Perspective::Omniscient);
    let names = s.name_table();
    let send = |b: &Battle| -> Vec<String> {
        if !requests || b.is_over() {
            return Vec::new();
        }
        SideId::BOTH
            .iter()
            .map(|&side| {
                format!("|request|{}|{}", pikapro_protocol::side_id(side), json.request_json(&b.request_data(side)))
            })
            .collect()
    };
    let mut chunk_ends = vec![battle.events().len()];
    let mut sent = vec![send(&battle)];
    for (side, text) in &s.choices {
        // As Showdown's `side.choose` reads it (with the names).
        let choice: Choice = pikapro_protocol::parse_choice_for(&battle, *side, text, &names[side.index()])
            .map_err(|e| format!("recorded choice {text:?}: {e}"))?;
        let progress =
            battle.submit(*side, choice).map_err(|e| format!("recorded choice {text:?} is illegal: {e:?}"))?;
        if progress == pikapro::Progress::Resolved {
            chunk_ends.push(battle.events().len());
            sent.push(send(&battle));
        }
    }
    Ok(Replay { battle, chunk_ends, requests: sent })
}

/// The silent-replay check: replays `s` twice more, once as [`replay`]
/// does (events and RNG trace on) and once with events off
/// (`BattleOptions::without_events`: no log, no trace), and requires
/// both to read and accept every choice alike and to reach the same
/// `BattleState` after each one; the logged one must also end where
/// `replayed` (the first replay) ended. Returns the first difference.
/// Anything the engine does only when it logs (or traces) and that
/// changes the battle shows up here, even where the log matches
/// Showdown's.
pub fn silent_replay_differs(s: &Scenario, replayed: &Replay) -> Option<String> {
    let new = |events: bool| {
        let options = BattleOptions::new(Seed::ShowdownGen5(s.seed));
        let options = if events { options.with_rng_trace() } else { options.without_events() };
        Battle::new(oracle_format(&s.format), s.teams.clone(), options).ok()
    };
    let (mut logged, mut silent) = (new(true)?, new(false)?);
    if logged.state() != silent.state() {
        return Some("silent replay: the states differ after Battle::new".into());
    }
    let names = s.name_table();
    for (k, (side, text)) in s.choices.iter().enumerate() {
        let names = &names[side.index()];
        // The first replay read and accepted every choice.
        let c1 = pikapro_protocol::parse_choice_for(&logged, *side, text, names).ok()?;
        let c2 = match pikapro_protocol::parse_choice_for(&silent, *side, text, names) {
            Ok(c) => c,
            Err(e) => return Some(format!("silent replay: choice {k} {text:?} unreadable: {e}")),
        };
        let p1 = logged.submit(*side, c1).ok()?;
        let p2 = match silent.submit(*side, c2) {
            Ok(p) => p,
            Err(e) => return Some(format!("silent replay: choice {k} {text:?} rejected: {e:?}")),
        };
        if p1 != p2 || logged.state() != silent.state() {
            let turn = logged.state().turn;
            return Some(format!("silent replay: the states differ after choice {k} ({side:?} {text:?}), turn {turn}"));
        }
    }
    if logged.state() != replayed.battle.state() {
        return Some("silent replay: a second logged replay ends in another state".into());
    }
    None
}

/// A caught panic's message (`std::panic::catch_unwind`'s payload).
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Writes `text` to `path` through a temporary file in the same directory
/// and a rename, so an interrupted run never leaves a half-written file.
pub fn write_atomically(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The raw log of a replay: the header, then each chunk and the requests
/// sent after it (if the replay kept them).
pub fn render_raw(s: &Scenario, replay: &Replay) -> Vec<String> {
    let renderer = renderer(s, Perspective::Omniscient);
    let mut raw = renderer.header();
    for (chunk, requests) in replay.chunks().zip(&replay.requests) {
        raw.extend(renderer.raw(chunk));
        raw.extend(requests.iter().cloned());
    }
    raw
}

/// The scenario's renderer for a perspective (its format's Showdown
/// header, the scenario's nicknames).
pub fn renderer(s: &Scenario, perspective: Perspective) -> Renderer {
    Renderer::new(&s.format, perspective).with_nicknames(s.name_table()).with_shiny(s.shiny.clone())
}

/// The four channels Showdown's `getPlayerStreams` sends.
pub const PERSPECTIVES: [Perspective; 4] =
    [Perspective::Omniscient, Perspective::Spectator, Perspective::Player(SideId::P1), Perspective::Player(SideId::P2)];

/// Checks each perspective's log from `Renderer::log` of a replayed
/// scenario against the channel Showdown's `extractChannelMessages` takes
/// from the (normalised) raw log `expected`. Returns the first difference.
pub fn check_replay_channels(
    s: &Scenario,
    replay: &Replay,
    expected: &[String],
    rng: bool,
) -> Option<(Perspective, Divergence)> {
    for perspective in PERSPECTIVES {
        let renderer = renderer(s, perspective);
        let mut ours = renderer.header();
        for chunk in replay.chunks() {
            ours.extend(renderer.log(chunk));
        }
        let ours = normalize(&ours, rng);
        let theirs = extract_channel(expected, perspective);
        if let Some(d) = first_divergence(&ours, &theirs) {
            return Some((perspective, d));
        }
    }
    None
}

#[derive(Debug)]
pub enum OracleError {
    /// Node or the oracle's dependencies are missing.
    Unavailable(String),
    /// Showdown rejected a choice or crashed.
    Failed { status: Option<i32>, stderr: String, log: Vec<String> },
}

impl std::fmt::Display for OracleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OracleError::Unavailable(m) => write!(f, "oracle unavailable: {m}"),
            OracleError::Failed { status, stderr, .. } => write!(f, "oracle failed ({status:?}): {stderr}"),
        }
    }
}

impl std::error::Error for OracleError {}

pub fn oracle_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/showdown-oracle")
}

/// Whether `node` and the oracle's `node_modules` are present.
pub fn oracle_available() -> bool {
    oracle_dir().join("node_modules/pokemon-showdown").is_dir()
        && Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Runs the scenario on Pokémon Showdown and returns its omniscient log,
/// with every request Showdown sends interleaved as
/// `|request|<side>|<JSON>` (the oracle's `PIKAPRO_ORACLE_REQUESTS`):
/// [`normalize`] drops them, [`requests`] reads them. With `trace`, the
/// log includes every PRNG draw (see [`normalize`]).
pub fn run_showdown_with_requests(s: &Scenario, trace: bool) -> Result<Vec<String>, OracleError> {
    let formatid = showdown_format(&s.format)
        .ok_or_else(|| OracleError::Unavailable(format!("no Showdown format for {}", s.format.id)))?;
    let json = scenario_json(s, formatid);
    let mut cmd = Command::new("node");
    if trace {
        cmd.env("PIKAPRO_ORACLE_TRACE", "1");
    }
    cmd.env("PIKAPRO_ORACLE_REQUESTS", "1");
    let mut child = cmd
        .arg(oracle_dir().join("oracle.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| OracleError::Unavailable(e.to_string()))?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(json.as_bytes())
        .map_err(|e| OracleError::Unavailable(e.to_string()))?;
    let out = child.wait_with_output().map_err(|e| OracleError::Unavailable(e.to_string()))?;
    let log: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect();
    if !out.status.success() {
        return Err(OracleError::Failed {
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            log,
        });
    }
    Ok(log)
}

/// The oracle's JSON input for a scenario.
pub fn scenario_json(s: &Scenario, formatid: &str) -> String {
    let choices: Vec<String> =
        s.choices.iter().map(|(side, text)| format!("{} {text}", pikapro_protocol::side_id(*side))).collect();
    // Showdown's Champions mod reads Stat Points from the EV field.
    // With the nicknames (`Nick (Species)`).
    let team = |side: SideId| export_sheet(&s.listed_sheet(side), &s.format);
    let seed: String = s.seed.iter().map(|w| format!("{w:04x}")).collect();
    let validate = s.validation_format().map_or(String::new(), |v| format!(",\"validate\":{}", quote(v)));
    // Generated teams: Showdown's generator draws them from the seeds.
    let teams = match &s.team_seeds {
        Some([a, b]) => format!("\"teamSeeds\":[{},{}]", quote(a), quote(b)),
        None => format!("\"teams\":[{},{}]", quote(&team(SideId::P1)), quote(&team(SideId::P2))),
    };
    let mut json = String::new();
    let _ = write!(
        json,
        "{{\"formatid\":{},\"seed\":{},{teams},\"choices\":[{}]{validate}}}",
        quote(formatid),
        quote(&format!("gen5,{seed}")),
        choices.iter().map(|c| quote(c)).collect::<Vec<_>>().join(",")
    );
    json
}

pub(crate) use pikapro_protocol::json_string as quote;

/// Lines left out of the comparison: Showdown's `|debug|` lines,
/// which it writes only in debug formats (the custom games three presets
/// are checked against) and which trace its implementation (handlers
/// suppressed, damage modifiers), not the battle; the requests interleaved
/// into the logs, which [`compare_requests`] checks.
const NOT_COMPARED: &[&str] = &["debug", "request"];

/// Brings a raw log to the compared form: every line, `|split|` blocks
/// included, except `NOT_COMPARED`; `|t:|` timestamps without their time
/// (core has no clock).
///
/// With `rng`, RNG trace lines are kept and brought to one form,
/// `|rng|<bound>|<result>`: pikapro's `random(n)` and Showdown's
/// `random(n)` / `random(from, to)` (as `to - from` and `result - from`).
pub fn normalize(lines: &[String], rng: bool) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('|').collect();
            let cmd = fields.get(1).copied().unwrap_or("");
            if cmd == "rng" {
                return if rng { Some(normalize_rng(&fields)) } else { None };
            }
            if NOT_COMPARED.contains(&cmd) {
                return None;
            }
            if cmd == "t:" {
                return Some("|t:|".into());
            }
            Some(line.clone())
        })
        .collect()
}

fn normalize_rng(fields: &[&str]) -> String {
    // Ours: ["", "rng", bound, result]
    if fields.len() == 4 {
        return fields.join("|");
    }
    // Oracle: ["", "rng", "random", args, result, call site]
    let args: Vec<u64> = fields.get(3).unwrap_or(&"").split(',').filter_map(|a| a.parse().ok()).collect();
    let result: u64 = fields.get(4).and_then(|r| r.parse().ok()).unwrap_or(0);
    let site = fields.get(5).unwrap_or(&"");
    match args[..] {
        [n] => format!("|rng|{n}|{result}"),
        [from, to] => format!("|rng|{}|{}", to - from, result - from),
        _ => format!("|rng|?|{result} ({site})"),
    }
}

/// The first point where two normalised logs disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub index: usize,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    /// Up to 8 agreeing lines before the divergence.
    pub context: Vec<String>,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for line in &self.context {
            writeln!(f, "      {line}")?;
        }
        writeln!(f, "  ours  {}", self.ours.as_deref().unwrap_or("<end of log>"))?;
        write!(f, "  sd    {}", self.theirs.as_deref().unwrap_or("<end of log>"))
    }
}

/// Compares two raw logs after [`normalize`]. With `rng`, RNG draws must
/// match too (requires a traced oracle run; pikapro's side always traces).
pub fn compare(ours: &[String], theirs: &[String], rng: bool) -> Option<Divergence> {
    first_divergence(&normalize(ours, rng), &normalize(theirs, rng))
}

/// The first line where two normalised logs differ.
pub fn first_divergence(a: &[String], b: &[String]) -> Option<Divergence> {
    let index = (0..a.len().max(b.len())).find(|&i| a.get(i) != b.get(i))?;
    Some(Divergence {
        index,
        ours: a.get(index).cloned(),
        theirs: b.get(index).cloned(),
        context: a[index.saturating_sub(8)..index].to_vec(),
    })
}

// ---------------------------------------------------------------------------
// Requests.

/// [`run_ours`], with the request each side gets at each decision point
/// interleaved into the log as `|request|<side>|<JSON>`, after the chunk
/// before it, the way [`run_showdown_with_requests`] shows Showdown's.
pub fn run_ours_with_requests(s: &Scenario) -> Vec<String> {
    render_raw(s, &replay(s, true))
}

/// A request of a log (see [`requests`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentRequest {
    /// `p1` or `p2`.
    pub side: String,
    /// The JSON.
    pub json: String,
    /// The last compared log line before it (`|turn|3`, ...): where it was
    /// sent, which [`compare_requests`] checks. Fixtures don't store it.
    pub after: String,
}

/// The requests interleaved into a log, in order.
pub fn requests(lines: &[String]) -> Vec<SentRequest> {
    let mut after = String::new();
    let mut out = Vec::new();
    for line in lines {
        if let Some(rest) = line.strip_prefix("|request|") {
            let (side, json) = rest.split_once('|').unwrap_or(("", rest));
            out.push(SentRequest { side: side.into(), json: json.into(), after: after.clone() });
        } else if let Some(l) = normalize(std::slice::from_ref(line), false).pop() {
            // The raw log's blank and `|split|` lines say nothing.
            if !l.is_empty() && !l.starts_with("|split|") {
                after = l;
            }
        }
    }
    out
}

/// The first request that differs between two logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDivergence {
    /// Which request (0-based, both sides counted).
    pub index: usize,
    pub ours: Option<SentRequest>,
    pub theirs: Option<SentRequest>,
}

impl std::fmt::Display for RequestDivergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (Some(ours), Some(theirs)) = (&self.ours, &self.theirs) else {
            let (who, r) = match (&self.ours, &self.theirs) {
                (Some(r), None) => ("only ours", r),
                (None, Some(r)) => ("only Showdown's", r),
                _ => return write!(f, "request {}: none", self.index),
            };
            return write!(f, "request {} ({who}): {} after {}\n  {}", self.index, r.side, r.after, r.json);
        };
        if ours.side == theirs.side && ours.json == theirs.json {
            return write!(
                f,
                "request {} ({}) is sent after {} here, after {} in Showdown's log",
                self.index, ours.side, ours.after, theirs.after
            );
        }
        let at = ours.json.bytes().zip(theirs.json.bytes()).take_while(|(a, b)| a == b).count();
        let after = if theirs.after.is_empty() { &ours.after } else { &theirs.after };
        writeln!(
            f,
            "request {} ({} / {}, after {after}) differs at {}:",
            self.index,
            ours.side,
            theirs.side,
            json_path(&theirs.json, at)
        )?;
        writeln!(f, "  ours  {}", excerpt(&ours.json, at))?;
        write!(f, "  sd    {}", excerpt(&theirs.json, at))
    }
}

/// Compares the requests of two logs (see [`requests`]).
/// Each request must also be sent at the same point of the log (after the
/// same line).
pub fn compare_requests(ours: &[String], theirs: &[String]) -> Option<RequestDivergence> {
    compare_requests_by(&requests(ours), &requests(theirs), true)
}

/// Compares two lists of requests: side and JSON, not where each was
/// sent (fixtures don't store it).
pub fn compare_request_lists(ours: &[SentRequest], theirs: &[SentRequest]) -> Option<RequestDivergence> {
    compare_requests_by(ours, theirs, false)
}

fn compare_requests_by(ours: &[SentRequest], theirs: &[SentRequest], placed: bool) -> Option<RequestDivergence> {
    let same =
        |a: &SentRequest, b: &SentRequest| a.side == b.side && a.json == b.json && (!placed || a.after == b.after);
    let index = (0..ours.len().max(theirs.len())).find(|&i| match (ours.get(i), theirs.get(i)) {
        (Some(a), Some(b)) => !same(a, b),
        _ => true,
    })?;
    Some(RequestDivergence { index, ours: ours.get(index).cloned(), theirs: theirs.get(index).cloned() })
}

/// Up to 70 bytes of `s` on each side of byte `at`, with `⟨` marking it.
fn excerpt(s: &str, at: usize) -> String {
    let floor = |mut i: usize| {
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (start, at, end) = (floor(at.saturating_sub(70)), floor(at.min(s.len())), floor((at + 70).min(s.len())));
    format!(
        "{}{}⟨{}{}",
        if start > 0 { "…" } else { "" },
        &s[start..at],
        &s[at..end],
        if end < s.len() { "…" } else { "" }
    )
}

/// The JSON path (`side.pokemon[2].condition`) of the value at byte `at`.
fn json_path(s: &str, at: usize) -> String {
    enum Frame {
        Object(String),
        Array(usize),
    }
    let bytes = s.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut key_next = false;
    let mut i = 0;
    while i < at.min(bytes.len()) {
        match bytes[i] {
            b'{' => {
                stack.push(Frame::Object(String::new()));
                key_next = true;
            }
            b'[' => stack.push(Frame::Array(0)),
            b'}' | b']' => {
                stack.pop();
            }
            b',' => match stack.last_mut() {
                Some(Frame::Array(n)) => *n += 1,
                Some(Frame::Object(_)) => key_next = true,
                None => {}
            },
            b'"' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] != b'"' {
                    j += if bytes[j] == b'\\' { 2 } else { 1 };
                }
                if key_next {
                    if let Some(Frame::Object(key)) = stack.last_mut() {
                        *key = s[i + 1..j.min(s.len())].to_string();
                    }
                    key_next = false;
                }
                i = j;
            }
            _ => {}
        }
        i += 1;
    }
    let mut path = String::new();
    for frame in &stack {
        match frame {
            Frame::Object(key) => {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
            }
            Frame::Array(n) => {
                let _ = write!(path, "[{n}]");
            }
        }
    }
    if path.is_empty() { "the top".into() } else { path }
}

// ---------------------------------------------------------------------------
// Legal teams (`cargo conformance --legal uniform|sets`).
//
// The battle fuzzer's own teams are not legal VGC teams (see
// CONFORMANCE.md). With `--legal`, pikapro-teams draws the teams instead, from
// what the engine implements (`species_pool`, `move_pool`,
// `IMPLEMENTED_ABILITIES`, `IMPLEMENTED_ITEMS`), so every team is one
// Showdown's validator accepts. The choices are drawn as for
// `random_scenario`.

/// A pikapro-teams generator for `format` restricted to what pikapro
/// implements.
pub fn legal_generator(
    format: &Format,
    mode: pikapro_teams::Mode,
) -> Result<pikapro_teams::TeamGenerator, pikapro_teams::Error> {
    // A random battle's sets are Showdown's generator's, unrestricted.
    if format.random_teams && mode == pikapro_teams::Mode::Sets {
        return pikapro_teams::TeamGenerator::new(format, mode);
    }
    let items: Vec<ItemId> = IMPLEMENTED_ITEMS
        .iter()
        .copied()
        .filter(|&i| format.dex.item(i).nonstandard.is_none() && !format.dex.item(i).is_z_crystal)
        .collect();
    let restrict = pikapro_teams::Restrict {
        species: Some(species_pool(format)),
        moves: Some(move_pool(format)),
        abilities: Some(IMPLEMENTED_ABILITIES.to_vec()),
        items: Some(items),
    };
    pikapro_teams::TeamGenerator::with_options(format, mode, pikapro_teams::Options { restrict, ..Default::default() })
}

/// [`random_scenario`] with both teams from `generator` (drawn from the
/// seed's stream, then the battle's choices).
pub fn random_legal_scenario(generator: &pikapro_teams::TeamGenerator, seed: u64) -> Scenario {
    if let Some(kind) = generator.random_battle() {
        return random_generated_scenario(generator.format(), kind, seed);
    }
    let mut rng = BattleRng::new(Seed::fast(seed));
    let mut redraws = pikapro_teams::Redraws::default();
    let mut team = || generator.team_from(&mut rng, &mut redraws).expect("a legal team");
    let teams = [team(), team()];
    play_random(generator.format(), seed, rng, teams)
}

/// A random battle between the teams Showdown's generator draws for two
/// players' seeds (p1's a `gen5` seed, p2's a `sodium` one, both drawn from
/// `seed`), with random choices: pikapro plays pikapro-teams' teams for the
/// seeds, Showdown its own generator's ([`Scenario::team_seeds`]).
pub fn random_generated_scenario(
    format: &Format,
    kind: pikapro_teams::random_battle::RandomBattle,
    seed: u64,
) -> Scenario {
    let mut seeds = BattleRng::new(Seed::fast(seed ^ 0x7465_616D_5365_6564));
    let mut draw = || (u64::from(seeds.next_u32()) << 32) | u64::from(seeds.next_u32());
    let p1 = random_teams::seed(random_teams::SeedKind::Gen5, draw());
    let p2 = random_teams::seed(random_teams::SeedKind::Sodium, draw());
    let generate = |seed: Seed| {
        let mut rng = BattleRng::new(seed);
        pikapro_teams::random_battle::generate(kind, &mut rng).expect("Showdown's generator gives a team")
    };
    let (a, b) = (generate(p1.0), generate(p2.0));
    let shiny = [&a, &b].map(|t| t.iter().map(|s| s.shiny).collect());
    let teams = [&a, &b].map(|t| pikapro_teams::random_battle::to_team(t));
    let mut s = play_random_with(format, BattleRng::new(Seed::fast(seed)), teams, [vec![], vec![]], shiny);
    s.team_seeds = Some([p1.1, p2.1]);
    s
}

/// [`random_legal_scenario`] with the teams' sets listed as the battle-only
/// formes they can become where one can (`cargo conformance --listed`):
/// a Mega Stone holder as its Mega (`Charizard-Mega-Y @ Charizardite Y`,
/// with the Mega's ability half the time where the set has the base
/// forme's first), Palafin as Palafin-Hero, Mimikyu as Mimikyu-Busted,
/// Ogerpon as its Tera forme, ... Both simulators validate the listed
/// teams and play what validation makes of them ([`Scenario::listed`]):
/// the drawn teams, each Mega Evolving with its side's one Mega Evolution.
/// The choices are those of the drawn teams' battle, drawn as for
/// [`random_legal_scenario`], and a set is listed only if the team (with its
/// nicknames, which the validator reads) stays valid. Returns whether a set
/// was listed; if not, or if a drawn team's nicknames make it invalid
/// (`Pikachu` on another species), the scenario is
/// [`random_legal_scenario`]'s.
pub fn random_listed_scenario(generator: &pikapro_teams::TeamGenerator, seed: u64) -> (Scenario, bool) {
    let mut s = random_legal_scenario(generator, seed);
    let format = generator.format();
    let validator = generator.validator();
    let mut rng = BattleRng::new(Seed::fast(seed ^ 0x6C69_7374));
    let mut listed = s.teams.clone();
    let mut landed = false;
    for (side, team) in listed.iter_mut().enumerate() {
        let sheet = s.sheet(SideId::BOTH[side]);
        let info: Vec<pikapro_validate::SetInfo> = sheet
            .team
            .sets
            .iter()
            .zip(&sheet.extras)
            .map(|(set, e)| pikapro_validate::SetInfo::new(set, &e.name, e.shiny))
            .collect();
        if validator.validate_with(team, &info).is_err() {
            return (s, false);
        }
        for i in 0..team.sets.len() {
            let set = &team.sets[i];
            let formes = listable_formes(format, validator, set);
            if formes.is_empty() || rng.random(4) == 0 {
                continue;
            }
            let (forme, mega_ability) = formes[rng.random(formes.len() as u32) as usize];
            let mut listed_set = set.clone();
            listed_set.species = forme;
            if let Some(a) = mega_ability.filter(|_| rng.random(2) == 0) {
                listed_set.ability = a;
            }
            // Only what validation makes the drawn set again.
            let mut probe = team.clone();
            probe.sets[i] = listed_set.clone();
            let (problems, played) = validator.check_set(&listed_set, &info[i]);
            if problems.is_empty() && played == *set && validator.validate_with(&probe, &info).is_ok() {
                team.sets[i] = listed_set;
                landed = true;
            }
        }
    }
    if landed {
        s.listed = Some(listed);
    }
    (s, landed)
}

/// The battle-only formes `set` can be listed as (whose `battleOnly` is its
/// species and whose required item, ability and move it has), each with
/// the forme's own ability if it differs and the set has the base forme's
/// first (a Mega's, which validation gives back).
fn listable_formes(
    format: &Format,
    validator: &pikapro_validate::Validator,
    set: &PokemonSet,
) -> Vec<(SpeciesId, Option<AbilityId>)> {
    use pikapro_data::legality::BattleOnly;
    let dex = format.dex;
    let leg = validator.format().legality;
    (0..dex.species.len() as u16)
        .map(SpeciesId)
        .filter(|&f| {
            let l = leg.species(f);
            let data = dex.species(f);
            l.battle_only == BattleOnly::Species(set.species)
                && data.nonstandard.is_none()
                && (data.required_items.is_empty() || set.item.is_some_and(|i| data.required_items.contains(&i)))
                && l.required_ability.is_none_or(|a| a == set.ability)
                && l.required_move.is_none_or(|m| set.moves.contains(&m))
        })
        .map(|f| {
            let own = dex.species(f).abilities.primary;
            let base_first = dex.species(set.species).abilities.primary;
            (f, (own != set.ability && set.ability == base_first).then_some(own))
        })
        .collect()
}

#[cfg(test)]
mod legal_tests {
    use super::*;

    #[test]
    fn legal_scenarios_have_valid_teams_the_engine_implements() {
        for format in [Format::champions_vgc(), Format::gen9_vgc()] {
            for mode in pikapro_teams::Mode::ALL {
                let g = legal_generator(&format, mode).expect("generator");
                let s = random_legal_scenario(&g, 2_300_000);
                let moves = move_pool(&format);
                for team in &s.teams {
                    assert!(g.validator().validate(team).is_ok(), "{} {mode:?}", format.id);
                    for set in &team.sets {
                        assert!(set.moves.iter().all(|m| moves.contains(m)));
                        assert!(IMPLEMENTED_ABILITIES.contains(&set.ability));
                    }
                }
                assert!(!s.choices.is_empty());
                assert_eq!(random_legal_scenario(&g, 2_300_000).choices, s.choices);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_showdown_noise() {
        let raw: Vec<String> = [
            "|t:|1790293201",
            "|gametype|singles",
            "|",
            "|split|p1",
            "|switch|p1a: Garchomp|Garchomp, L50, M|184/184",
            "|switch|p1a: Garchomp|Garchomp, L50, M|100/100",
            "|move|p1a: Garchomp|Earthquake|p2a: Pikachu|[spread] p2a,p2b",
            "|debug|Spread modifier: 0.75",
            "|upkeep",
        ]
        .map(String::from)
        .to_vec();
        let normalized = normalize(&raw, false);
        assert_eq!(
            normalized,
            [
                "|t:|",
                "|gametype|singles",
                "|",
                "|split|p1",
                "|switch|p1a: Garchomp|Garchomp, L50, M|184/184",
                "|switch|p1a: Garchomp|Garchomp, L50, M|100/100",
                "|move|p1a: Garchomp|Earthquake|p2a: Pikachu|[spread] p2a,p2b",
                "|upkeep"
            ]
        );
    }

    /// Battles where an Eject Pack switch brings in the Pokémon the other
    /// slot's queued switch is for and it faints on entry, so the queued
    /// switch brings it in fainted (`OrderState::stale_active`): Showdown
    /// throws at the end of that turn, pikapro plays on to the end. In the
    /// mirror with seed 40,120,201 a spread move is aimed at the slot the
    /// stale switch left.
    #[test]
    fn stale_switches_to_a_fainted_pokemon_play_to_the_end() {
        let cases = [
            ("gen9doubles", false, 40_108_351),
            ("gen9doubles", false, 40_139_474),
            ("gen9doubles", false, 40_152_190),
            ("gen9doubles", true, 40_120_201),
            ("gen9doubles", true, 40_193_425),
            ("gen9vgc", false, 40_101_342),
            ("gen9vgc", true, 40_192_682),
        ];
        for (format, mirror, seed) in cases {
            let format = Format::by_id(format).unwrap();
            let (s, _) = scenario_as(&format, seed, Some(view::Target::EjectPack), mirror);
            let replayed = replay(&s, true);
            let log = render_raw(&s, &replayed);
            assert!(
                log.iter().any(|l| l.starts_with("|switch|") && l.ends_with("|0 fnt")),
                "{} {seed}: no stale switch",
                format.id
            );
            assert!(log.iter().any(|l| l.starts_with("|win|") || l == "|tie"), "{} {seed}: no end", format.id);
            assert_eq!(silent_replay_differs(&s, &replayed), None, "{} {seed}", format.id);
        }
    }

    /// A generated battle sends Showdown the players' seeds, not teams,
    /// and its teams are the generator's for them.
    #[test]
    fn generated_scenarios_send_team_seeds() {
        let format = Format::gen9_random_doubles_battle();
        let g = legal_generator(&format, pikapro_teams::Mode::Sets).unwrap();
        let s = random_legal_scenario(&g, 5);
        let [a, b] = s.team_seeds.clone().expect("team seeds");
        assert!(a.starts_with("gen5,") && b.starts_with("sodium,"), "{a} {b}");
        let json = scenario_json(&s, "gen9randomdoublesbattle");
        assert!(json.contains(&format!("\"teamSeeds\":[\"{a}\",\"{b}\"]")) && !json.contains("\"teams\""), "{json}");
        let seed = random_teams::parse_seed(&b).unwrap();
        let sets = pikapro_teams::random_battle::generate(
            pikapro_teams::random_battle::RandomBattle::Doubles,
            &mut BattleRng::new(seed),
        )
        .unwrap();
        assert_eq!(s.teams[1], pikapro_teams::random_battle::to_team(&sets));
        assert_eq!(s.shiny[1], sets.iter().map(|x| x.shiny).collect::<Vec<_>>());
        // The fixture form round-trips, the seeds with it.
        let (parsed, _) = fixture::parse(&fixture::write(&s, &[])).unwrap();
        assert_eq!(parsed.team_seeds, s.team_seeds);
        assert_eq!(parsed.teams.each_ref().map(|t| t.sets.len()), [6, 6]);
    }

    #[test]
    fn nicknames_reach_the_export() {
        let format = Format::gen9_singles();
        let mut s = random_scenario(&format, 3);
        s.nicknames[0] = vec!["Chomp".into()];
        let json = scenario_json(&s, "x");
        let species = format.dex.species(s.teams[0].sets[0].species).name;
        assert!(json.contains(&format!("Chomp ({species})")), "{json}");
        assert_eq!(s.name_table()[0][0], "Chomp");
    }

    #[test]
    fn normalizes_rng_traces() {
        let lines: Vec<String> =
            ["|rng|random|24|21|site", "|rng|random|16,|3|site", "|rng|random|2,6|4|site", "|rng|24|21"]
                .map(String::from)
                .to_vec();
        assert_eq!(normalize(&lines, true), ["|rng|24|21", "|rng|16|3", "|rng|4|2", "|rng|24|21"]);
        assert!(normalize(&lines, false).is_empty());
    }

    #[test]
    fn pools_cover_the_dex() {
        for format in Format::presets() {
            let (species, moves) = (species_pool(format), move_pool(format));
            println!("{}: {} species, {} moves", format.id, species.len(), moves.len());
            // Guards against a schema or filter change silently shrinking
            // what the fuzzer exercises.
            assert!(species.len() >= 250, "{}: {} species", format.id, species.len());
            assert!(moves.len() >= 40, "{}: {} moves", format.id, moves.len());
            assert!(moves.contains(&moves::PROTECT));
        }
    }

    /// Pool counts alone can hide an unimplemented callback. Audit every
    /// standard Gen 9 row, including moves without the Metronome flag,
    /// and every harvested random-battle set before generator filtering.
    #[test]
    fn gen9_pools_do_not_hide_standard_or_random_set_mechanics() {
        let format = Format::gen9_ou();
        let dex = format.dex;
        let (species, moves, items) = (species_pool(&format), move_pool(&format), item_pool(&format));
        for (i, m) in dex.moves.iter().enumerate() {
            if m.nonstandard.is_none() {
                assert!(moves.contains(&MoveId(i as u16)), "standard move omitted: {} ({:?})", m.name, m.unmodelled);
            }
        }
        for (i, s) in dex.species.iter().enumerate() {
            let id = SpeciesId(i as u16);
            if s.nonstandard.is_none() && !s.battle_only && !pikapro_teams::random_battle::ADDED_FORMES.contains(&id) {
                assert!(species.contains(&id), "standard species omitted: {}", s.name);
                for a in [Some(s.abilities.primary), s.abilities.secondary, s.abilities.hidden].into_iter().flatten() {
                    assert!(
                        IMPLEMENTED_ABILITIES.contains(&a),
                        "{}'s ability omitted: {}",
                        s.name,
                        dex.ability(a).name
                    );
                }
            }
        }
        for (i, item) in dex.items.iter().enumerate() {
            if item.nonstandard.is_none() && item.mega.is_empty() && !item.is_z_crystal {
                assert!(items.contains(&ItemId(i as u16)), "standard item omitted: {}", item.name);
            }
        }
        // RKS System's only native species, Silvally, is Past in Gen 9;
        // its fail-copy/transfer flags prevent obtaining it from another
        // standard species. Keep this exemption explicit, not a count.
        for (i, ability) in dex.abilities.iter().enumerate() {
            let id = AbilityId(i as u16);
            if ability.nonstandard.is_none() && !IMPLEMENTED_ABILITIES.contains(&id) {
                assert_eq!(id, abilities::RKS_SYSTEM, "standard ability omitted: {}", ability.name);
            }
        }
        for table in [&pikapro_teams::sets::GEN9, &pikapro_teams::sets::GEN9_DOUBLES] {
            for set in table.iter() {
                assert!(species.contains(&set.species), "{}: {}", table.source, dex.species(set.species).name);
                assert!(
                    IMPLEMENTED_ABILITIES.contains(&set.ability),
                    "{}: {}",
                    table.source,
                    dex.ability(set.ability).name
                );
                for m in set.moves() {
                    assert!(moves.contains(m), "{}: {}", table.source, dex.move_(*m).name);
                }
                if let Some(item) = set.item {
                    assert!(items.contains(&item), "{}: {}", table.source, dex.item(item).name);
                }
            }
        }
    }

    /// Metronome can call any standard or `Unobtainable` move with the
    /// `metronome` flag (Burn Up, Corrosive Gas, Cut and Hold Back are
    /// `Unobtainable` in Gen 9), so each must be implemented.
    #[test]
    fn metronome_calls_only_implemented_moves() {
        for format in Format::presets() {
            let dex = format.dex;
            for id in (0..dex.moves.len() as u16).map(MoveId) {
                let m = dex.move_(id);
                let callable = m.flags.contains(pikapro_data::MoveFlags::METRONOME)
                    && matches!(m.nonstandard, None | Some(pikapro_data::Nonstandard::Unobtainable));
                if callable {
                    assert!(IMPLEMENTED_BY_CODE.contains(&id) || data_supported(m), "{}: {}", format.id, m.name);
                }
            }
        }
    }

    #[test]
    fn fixtures_round_trip() {
        for format in Format::presets() {
            let s = random_scenario(format, 3);
            let text = fixture::write(&s, &["|start".into()]);
            let (parsed, expected) = fixture::parse(&text).unwrap();
            assert_eq!(parsed.teams, s.teams);
            assert_eq!(parsed.choices, s.choices);
            assert_eq!(parsed.seed, s.seed);
            assert_eq!(expected, ["|start"]);
        }
    }

    /// `--listed`: teams listing battle-only formes are written as listed
    /// and played as pikapro-validate leaves them: the drawn teams.
    #[test]
    fn listed_scenarios_round_trip() {
        let format = Format::champions_vgc();
        let g = legal_generator(&format, pikapro_teams::Mode::Sets).unwrap();
        let (s, landed) = random_listed_scenario(&g, 2_400_197);
        assert!(landed);
        let listed = s.listed.clone().unwrap();
        assert_ne!(listed, s.teams);
        assert_eq!(random_legal_scenario(&g, 2_400_197).teams, s.teams);
        assert!(scenario_json(&s, "x").contains("\"validate\":\"gen9championsvgc2026regmc\""));
        let text = fixture::write(&s, &["|start".into()]);
        assert!(text.contains("-Mega @ "), "{text}");
        let (parsed, _) = fixture::parse(&text).unwrap();
        assert_eq!(parsed.teams, s.teams);
        assert_eq!(parsed.listed, s.listed);
    }

    #[test]
    fn scenarios_replay_identically() {
        for format in Format::presets() {
            let s = random_scenario(format, 7);
            assert_eq!(run_ours(&s), run_ours(&s));
            assert!(scenario_json(&s, "x").starts_with("{\"formatid\":\"x\""));
        }
    }
}

/// The workspace README's Rust example, run as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../../../README.md")]
struct ReadmeDoctests;
