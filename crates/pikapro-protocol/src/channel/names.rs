//! The names the renderer writes, read back: each lookup runs the
//! renderer's own naming function over every value, so the two can't
//! drift apart.

use pikapro::event::{EffectName, FieldEffect, Hint, ManipulationVolatile, Volatile, VolatileEffect};
use pikapro::state::{SideCondition, Terrain, Weather};
use pikapro_data::gen9::moves;
use pikapro_data::{Dex, MoveCategory, MoveId};

use crate::log::{
    category_name, effect_name, field_effect_name, manipulation_volatile_name, side_condition_name,
    side_condition_plain_name, terrain_name, volatile_name, weather_name,
};

const WEATHERS: [Weather; 7] = [
    Weather::Sun,
    Weather::Rain,
    Weather::Sand,
    Weather::Snow,
    Weather::HarshSun,
    Weather::HeavyRain,
    Weather::StrongWinds,
];

const TERRAINS: [Terrain; 4] = [Terrain::Electric, Terrain::Grassy, Terrain::Misty, Terrain::Psychic];

const VOLATILES: [Volatile; 4] = [Volatile::Confusion, Volatile::Taunt, Volatile::Encore, Volatile::Disable];

/// Every [`VolatileEffect`] but the partial traps (which name their move).
const EFFECTS: [VolatileEffect; 21] = [
    VolatileEffect::Substitute,
    VolatileEffect::Trapped,
    VolatileEffect::NoRetreat,
    VolatileEffect::LeechSeed,
    VolatileEffect::Yawn,
    VolatileEffect::Curse,
    VolatileEffect::PerishSong,
    VolatileEffect::DestinyBond,
    VolatileEffect::Attract,
    VolatileEffect::Torment,
    VolatileEffect::SaltCure,
    VolatileEffect::SyrupBomb,
    VolatileEffect::ThroatChop,
    VolatileEffect::HealBlock,
    VolatileEffect::Imprison,
    VolatileEffect::GlaiveRush,
    VolatileEffect::Endure,
    VolatileEffect::SmackDown,
    VolatileEffect::MagnetRise,
    VolatileEffect::FairyLock,
    VolatileEffect::Octolock,
];

/// Every [`ManipulationVolatile`] but Stockpile (`stockpile<n>` / `Stockpile`).
const MANIPULATIONS: [ManipulationVolatile; 8] = [
    ManipulationVolatile::AquaRing,
    ManipulationVolatile::Ingrain,
    ManipulationVolatile::Charge,
    ManipulationVolatile::FocusEnergy,
    ManipulationVolatile::DragonCheer,
    ManipulationVolatile::TarShot,
    ManipulationVolatile::PowerTrick,
    ManipulationVolatile::PowerShift,
];

pub(crate) fn weather_by_name(name: &str) -> Option<Weather> {
    WEATHERS.into_iter().find(|&w| weather_name(w) == name)
}

/// The weather a `[from] <id>` names (`sunnyday`), as `WeatherFail` writes it.
pub(crate) fn weather_by_id(id: &str) -> Option<Weather> {
    WEATHERS
        .into_iter()
        .find(|&w| pikapro_data::id_eq(weather_name(w), id) && id.bytes().all(|b| b.is_ascii_lowercase()))
}

/// `-fieldactivate`'s name of a primal weather (or any weather).
pub(crate) fn weather_activate_by_name(name: &str) -> Option<Weather> {
    match name {
        "Delta Stream" => Some(Weather::StrongWinds),
        "Desolate Land" => Some(Weather::HarshSun),
        "Primordial Sea" => Some(Weather::HeavyRain),
        _ => weather_by_name(name),
    }
}

pub(crate) fn terrain_by_name(name: &str) -> Option<Terrain> {
    TERRAINS.into_iter().find(|&t| terrain_name(t) == name)
}

/// A pseudo-weather or terrain by its name without `move: `.
pub(crate) fn field_by_name(name: &str) -> Option<FieldEffect> {
    [FieldEffect::TrickRoom, FieldEffect::Gravity, FieldEffect::MagicRoom, FieldEffect::WonderRoom]
        .into_iter()
        .chain(TERRAINS.map(FieldEffect::Terrain))
        .find(|&e| field_effect_name(e) == name)
}

pub(crate) fn volatile_by_name(name: &str) -> Option<Volatile> {
    VOLATILES.into_iter().find(|&v| volatile_name(v) == name)
}

/// A volatile effect by its name as `-start`/`-end`/`cant` write it; a
/// partial trap by its move's plain name.
pub(crate) fn effect_by_name(dex: &Dex, name: &str) -> Option<VolatileEffect> {
    if let Some(e) = EFFECTS.into_iter().find(|&e| effect_name(dex, e) == name) {
        return Some(e);
    }
    partial_trap(dex, name).map(VolatileEffect::PartiallyTrapped)
}

/// A partially trapping move (Bind, Fire Spin, ...) by its name.
pub(crate) fn partial_trap(dex: &Dex, name: &str) -> Option<MoveId> {
    let m = dex.move_by_name(name)?;
    (dex.move_(m).volatile_status == Some("partiallytrapped") && dex.move_(m).name == name).then_some(m)
}

pub(crate) fn manipulation_by_name(name: &str) -> Option<ManipulationVolatile> {
    if name == "Stockpile" {
        return Some(ManipulationVolatile::Stockpile(0));
    }
    MANIPULATIONS.into_iter().find(|&v| manipulation_volatile_name(v) == name)
}

/// A side condition by the name `-sidestart`/`-sideend` write (with the
/// `move: ` prefix where they write one).
pub(crate) fn side_condition_by_name(name: &str) -> Option<SideCondition> {
    SideCondition::ALL.into_iter().find(|&c| side_condition_name(c) == name)
}

pub(crate) fn side_condition_by_plain_name(name: &str) -> Option<SideCondition> {
    SideCondition::ALL.into_iter().find(|&c| side_condition_plain_name(c) == name)
}

pub(crate) fn category_by_name(name: &str) -> Option<MoveCategory> {
    [MoveCategory::Physical, MoveCategory::Special, MoveCategory::Status]
        .into_iter()
        .find(|&c| category_name(c) == name)
}

/// `ability: X`, `item: X`, `move: X`.
pub(crate) fn effect_name_of(dex: &Dex, text: &str) -> Option<EffectName> {
    if let Some(a) = text.strip_prefix("ability: ") {
        return dex.ability_by_name(a).map(EffectName::Ability);
    }
    if let Some(i) = text.strip_prefix("item: ") {
        return dex.item_by_name(i).map(EffectName::Item);
    }
    if let Some(m) = text.strip_prefix("move: ") {
        return dex.move_by_name(m).map(EffectName::Move);
    }
    None
}

/// Texts the engine names with a `&'static str` (`LineArg::Text`,
/// `StartFrom::what`, `AbilityNamed::name`): the parser hands those out.
const TEXTS: &[&str] = &[
    "",
    "unboost",
    "[consumed]",
    "[fromitem]",
    "typechange",
    "confusion",
    "accuracy",
    "Illusion",
    "Attract",
    "Charge",
    "Disable",
    "As One",
    "Slow Start",
    "Protosynthesis",
    "Quark Drive",
    "fallenundefined",
    "fallen1",
    "fallen2",
    "fallen3",
    "fallen4",
    "fallen5",
    "protosynthesisatk",
    "protosynthesisdef",
    "protosynthesisspa",
    "protosynthesisspd",
    "protosynthesisspe",
    "quarkdriveatk",
    "quarkdrivedef",
    "quarkdrivespa",
    "quarkdrivespd",
    "quarkdrivespe",
    "Attack",
    "Defense",
    "Special Attack",
    "Special Defense",
    "Speed",
    "Sp. Atk",
    "Sp. Def",
    "Accuracy",
    "Evasion",
    "evasion",
    "atk",
    "def",
    "spa",
    "spd",
    "spe",
];

/// The static copy of a text the engine writes, if it is one.
pub(crate) fn static_text(text: &str) -> Option<&'static str> {
    TEXTS.iter().copied().find(|&t| t == text)
}

/// A `-hint` line's text, read back.
pub(crate) fn parse_hint(dex: &Dex, text: &str) -> Option<Hint> {
    let mv = |name: &str| dex.move_by_name(name).filter(|&m| dex.move_(m).name == name);
    Some(match text {
        "Only a Pokemon whose form is Morpeko or Morpeko-Hangry can use this move." => {
            Hint::OnlyForme(moves::AURA_WHEEL)
        }
        "Only a Pokemon whose form is Darkrai can use this move." => Hint::OnlyForme(moves::DARK_VOID),
        "Only a Pokemon whose form is Hoopa Unbound can use this move." => Hint::OnlyForme(moves::HYPERSPACE_FURY),
        "Psychic Terrain doesn't affect airborne Pokémon." => Hint::PsychicTerrainAirborne,
        "If a Terastallized Pokemon uses Roost, it remains Flying-type." => Hint::TeraRoost,
        "Since gen 7, Dark is immune to Prankster moves." => Hint::DarkPrankster,
        "In Gen 8+, Future attacks will never resolve when used on the 255th turn or later." => {
            Hint::FutureMoveTurnLimit
        }
        "Transform Mimicry changes you to your original un-transformed types." => Hint::TransformMimicry,
        "A switch failed because the Pokémon trying to switch in is already in." => Hint::SwitchAlreadyIn,
        "A Pokemon can't switch between when it runs out of HP and when it faints" => Hint::SwitchAtZeroHp,
        "Skill Swap does not announce the abilities of the Pokémon when used between allies." => Hint::SkillSwapAllies,
        "Sleep Clause Mod prevents players from putting more than one of their opponent's Pokémon to sleep at a time" => {
            Hint::SleepClauseMod
        }
        "Illusion Level Mod is active, so this Pokémon's true level was hidden." => Hint::IllusionLevelMod,
        "In Gen 7, Stockpile keeps track of how many times it successfully altered each stat individually." => {
            Hint::StockpileTracking
        }
        _ => {
            if let Some(m) = text.strip_suffix(" only works on your first turn out.") {
                return Some(Hint::FirstTurnOnly(mv(m)?));
            }
            if let Some(m) = text.strip_suffix("'s BP doubled on grounded target.") {
                return Some(Hint::GroundedDoubled(mv(m)?));
            }
            if let Some(rest) = text.strip_suffix(" Def boosts when Wonder Room is active.") {
                let (m, special) = match rest.strip_suffix(" uses Sp.") {
                    Some(m) => (m, true),
                    None => (rest.strip_suffix(" uses")?, false),
                };
                return Some(Hint::WonderRoomDefense { move_id: mv(m)?, special });
            }
            if let Some(c) = text.strip_suffix(" Shell Side Arm") {
                return Some(Hint::ShellSideArm(category_by_name(c)?));
            }
            if let Some(rest) = text.strip_suffix(" did not hit because the target is fainted.") {
                return Some(Hint::FutureMoveMissed { move_id: mv(rest)?, fainted: true });
            }
            if let Some(rest) = text.strip_suffix(" did not hit because the target is the user.") {
                return Some(Hint::FutureMoveMissed { move_id: mv(rest)?, fainted: false });
            }
            if let Some(rest) = text.strip_prefix("In Gen 8+, Wish will never resolve when used on the ") {
                return Some(Hint::WishTurnLimit(rest.strip_suffix("th turn.")?.parse().ok()?));
            }
            if let Some(rest) = text.strip_prefix("Some effects can force a Pokemon to use ") {
                return Some(Hint::CantUseTwice(mv(rest.strip_suffix(" again in a row.")?)?));
            }
            return None;
        }
    })
}
