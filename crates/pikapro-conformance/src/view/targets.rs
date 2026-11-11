//! Targeted generators: the conformance fuzzer's teams (`random_team`)
//! with sets forced for the mechanics that are hardest to view right.
//!
//! Each [`Target`] overwrites one or two random sets per side (keeping
//! their level, nature and spread) with a forced species, ability, moves
//! or item, drawn from what the format's dex and pikapro's pools allow.
//! A target the format can't express (Mega Evolution outside Champions,
//! Commander in singles) leaves the teams as they were: [`Target::applies`]
//! says whether it can, and [`Target::apply`] whether it did.

use pikapro::{BattleRng, Format, PokemonSet, Team};
use pikapro_data::gen9::{abilities, items, moves, species};
use pikapro_data::{AbilityId, ItemId, MoveId, SpeciesId, Type};

use crate::mega_stones;

/// A mechanic to force into the teams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Zoroark or Zoroark-Hisui with Illusion.
    Illusion,
    /// Ditto with Imposter, and a Transform user.
    Imposter,
    /// Trick and Switcheroo users holding items.
    Trick,
    KnockOff,
    SkillSwap,
    /// A species with Trace, with it.
    Trace,
    /// Species holding their Mega Stone (Champions).
    Mega,
    /// Tera Blast users.
    Tera,
    /// Ogerpon (its formes) and Terapagos.
    Ogerpon,
    /// Tatsugiri and Dondozo on one side (doubles).
    Commander,
    RevivalBlessing,
    /// Sleep Talk with Rest, against Spore and Yawn.
    SleepTalk,
    Metronome,
    /// Regenerator with pivoting moves: the no-leak canary (a benched foe's
    /// heal must not show).
    Regenerator,
    /// Curse from a Ghost type and from anything (Champions aims both
    /// differently).
    Curse,
    /// Run Away against trapping moves and abilities (it escapes them in
    /// Champions).
    RunAway,
    /// Eject Button against pivoting moves.
    EjectButton,
    /// Emergency Exit and Wimp Out against pivoting moves.
    EmergencyExit,
    /// Metronome even where the dex has it `Past` (Champions: its list is
    /// the dex's own, which Showdown still plays).
    MetronomePast,
    /// Heal Block and Throat Chop against moves that call healing and
    /// sound moves (Copycat, Sleep Talk).
    HealBlock,
    /// Octolock users, against Clear Body and Big Pecks (Champions).
    Octolock,
    /// Milk Drink users (`adjacentAllyOrSelf` in Champions).
    MilkDrink,
    /// Farfetch'd or Sirfetch'd with a Leek (Champions).
    Leek,
    /// Lucario with Lucarionite Z (Lucario-Mega-Z's Aura Guard), and
    /// contact moves.
    AuraGuard,
    /// Reg M-C's new Mega Stones: Absolite Z, Baxcalibrite, Garchompite Z,
    /// Golisopite, Lucarionite Z, Salamencite.
    NewMegas,
    /// Mega Sceptile and Manectric change Lightning Rod during the turn,
    /// alongside redirection and spread moves (Champions doubles).
    MegaRedirect,
    /// Meganium with its Mega Stone (Mega Sol) using Growth and the weather
    /// moves, next to weather setters (Champions).
    MegaSol,
    /// Ceaseless Edge and Stone Axe users against Rough Skin, Iron Barbs
    /// and Rocky Helmet (the hazard after the user faints, Champions).
    HazardMoves,
    /// Future Sight and Doom Desire users with pivoting moves (the
    /// defining set: any Gen 9 format), against Scovillain's Mega (Spicy
    /// Spray) where the format has it (Champions).
    FutureSight,
    /// Eject Pack holders among stat-dropping moves and Intimidate.
    EjectPack,
    /// Lightning Rod and Storm Drain next to two-turn moves (doubles).
    ChargeRedirect,
    /// Arceus (a forme with its Plate, or the base forme with any Plate)
    /// using Judgment: `cargo conformance` only (the view harnesses play it
    /// in its base forme, `draw_view_teams`). Silvally and Multi-Attack are
    /// `Past` in every format here, so no pool has them.
    Arceus,
    // Combined targets: two or three mechanics that meet
    // in the view, forced together.
    /// Zoroark with Illusion, Ditto with Imposter, and Transform users.
    IllusionImposter,
    /// Zoroark with Tera Blast, next to Ogerpon or Terapagos.
    IllusionTera,
    /// Tatsugiri and Dondozo, Revival Blessing, Ally Switch and Explosion
    /// (doubles).
    CommanderRevival,
    /// Imposter, Transform with Tera Blast, and two Mega users where the
    /// format has Megas.
    TransformGimmick,
    /// Metronome, Copycat and Sleep Talk; spread moves and Instruct;
    /// Oricorio with Dancer; Magic Coat.
    SpreadCalled,
    // Engine mechanics, for `cargo conformance --target NAME`.
    /// Prankster Encore, Quash and After You users, with priority moves: an
    /// Encore after Quash keeps the Quashed action's place (Champions).
    EncoreOrder,
    /// Wish next to Future Sight and Doom Desire users (both in one slot),
    /// and Unaware against Transform (a future move's evasion event).
    WishLong,
    /// No Guard, Lock-On, Perish Song and Teatime against a Tatsugiri and a
    /// Dondozo (doubles).
    CommanderNoGuard,
    /// Desolate Land and Primordial Sea (on any species: only formats whose
    /// Showdown rules accept any ability) against Fire and Water moves.
    /// For `cargo conformance` (and `channel-conformance`): the player view
    /// takes a species' only possible ability as known, which these sets
    /// break, so `view-conformance --source primalweather` reports it
    /// (legal teams can't reach the primal abilities: Groudon's and
    /// Kyogre's Primal formes are `Past` in both dexes).
    PrimalWeather,
    /// Rage Fist, Metal Burst, Comeuppance and Avalanche users with Revival
    /// Blessing and Future Sight: a Pokémon knocked out keeps the hit it
    /// took (`gotAttacked` and `timesAttacked` after `faintMessages`) when
    /// it is revived and comes back.
    RevivalRageFist,
    /// Ditto with Imposter and a Transform user against forme-changing abilities (Disguise, Ice Face, Hunger Switch, Shields Down, Zero to Hero, Stance Change, ...).
    FormeImposter,
    /// Symbiosis holders next to allies holding consumable items (doubles).
    Symbiosis,
    /// Pledge users with After You, Quash, Instruct and Ally Switch (doubles).
    Pledges,
    /// Opportunist and Mirror Herb among boosting moves, Spicy Extract, Defiant and Intimidate.
    Opportunist,
    /// Neutralizing Gas with pivoting moves, against abilities that start again when it leaves.
    NeutralizingGas,
    /// Fling with items that do something when flung, berry thieves, Magic Room, Klutz and Unburden.
    Fling,
    /// Choice items and Assault Vest against Encore, Disable, Taunt, Imprison, Throat Chop and Gravity.
    Struggle,
    /// The Endless Battle Clause (formats that have it: gen9ou): every
    /// Pokémon heals its foe (Heal Pulse, Floral Healing) between weak
    /// attacks, some hold restorative berries, others have Recycle, steal
    /// berries or fling them, so that long battles go stale and the
    /// clause ends them, with a side that could cycle a berry, with two
    /// or with none.
    Endless,
    /// Sleep Clause Mod (the random battles): sleep moves, Yawn, Rest,
    /// Sleep Talk, Effect Spore, Safeguard and the terrains that block
    /// sleep, Psycho Shift to pass a sleep on.
    SleepClause,
    /// Metal Burst, Counter, Mirror Coat, Comeuppance and their kin against pivots, Eject Button and Red Card.
    Retaliate,
    /// Parental Bond on any species (formats that accept any ability) against Focus Sash, berries and on-hit abilities.
    ParentalBond,
    /// Multi-hit moves against abilities and items that react to each hit.
    MultiHit,
    /// Multi-hit moves against more abilities that react to each hit.
    MultiHit2,
    /// Moves that copy, swap or replace abilities, against abilities with special rules.
    AbilityCopy,
    /// Two-turn moves with Power Herb against Gravity, Smack Down, moves that hit semi-invulnerable targets and No Guard.
    TwoTurn,
    /// Destiny Bond, Explosion, Memento, Final Gambit, Healing Wish, Lunar Dance and Perish Song: simultaneous faints.
    SelfKo,
    /// Pinch berries and Quick Claw with Gluttony, Ripen, Cheek Pouch, Harvest and Cud Chew, Recycle and Belch.
    Pinch,
    /// Dancer next to every kind of dance: boosts, Feather Dance, Teeter Dance, the lock of Petal Dance, damaging dances.
    Dancer,
    /// Several Dancers, Copycat and Teeter Dance: an aborted last copy
    /// must preserve the previous action's move for Copycat (doubles).
    DancerCopycat,
    /// Instruct, After You, Quash and Ally Switch with moves that remember something (Gigaton Hammer, Future Sight, Rollout, Round, Fusion Flare, ...) (doubles).
    Instruct,
    /// Follow Me, Rage Powder and Ally Switch against Stalwart, Propeller Tail, Snipe Shot, Storm Drain and Lightning Rod (doubles).
    Redirect,
    /// Prankster, Gale Wings and Triage against Psychic Terrain, Queenly Majesty, Dazzling and Armor Tail, with Quick Claw, Custap Berry and Trick Room.
    Priority,
    /// Reflect, Light Screen and Aurora Veil against Brick Break, Psychic Fangs, Raging Bull, Defog, Infiltrator, Court Change and critical hits.
    Screens,
    /// Weather setters against Cloud Nine and Air Lock, Utility Umbrella, Protosynthesis, Quark Drive, Mimicry, terrains and seeds.
    WeatherWar,
    /// Substitute, Shed Tail and Baton Pass against sound moves, multi-hit moves, Infiltrator, Curse, Leech Seed and Transform.
    Substitute,
    /// Thief, Covet, Trick, Switcheroo and Corrosive Gas, Pickpocket, Magician and Sticky Hold, with items that matter when they change hands.
    ItemSwap,
    /// Psych Up, the swaps and splits, Topsy-Turvy, Clear Smog, Haze, Baton Pass and Costar.
    StatCopy,
    /// Soak, Magic Powder, Forest's Curse, Reflect Type, Conversion, Roost, Double Shock, Burn Up, Protean, Libero and Color Change.
    TypeChange,
    /// Outrage, Raging Fury, Thrash, Uproar, Rollout and Petal Dance against Encore, Disable, Spite, Instruct, Yawn and Rest.
    LockIn,
    /// Endure, Sturdy and Focus Sash against multi-hit moves, False Swipe, recoil and fixed-damage moves.
    Endure,
    /// Counter and Mirror Coat users next to Instruct users, with Metal Burst and Comeuppance: Instruct can give a Pokémon both volatiles (doubles).
    CounterInstruct,
    /// Berries that act in `Update` (status cures, pinch and HP berries, Leppa) against confusion, status moves, Spite and Super Fang, with Gluttony, Ripen, Cud Chew and Harvest.
    BerryUpdate,
    /// Berries that act in `Update` against what stops them: Magic Room, Embargo, Unnerve, Klutz, Neutralizing Gas, Gastro Acid, and the moves that take or eat them.
    BerrySuppress,
    /// The other items that act in `Update` (Mental Herb, Booster Energy, Utility Umbrella) against what sets them off or moves them.
    UpdateItems,
    /// Weather setters against Cloud Nine, Air Lock and Neutralizing Gas coming and going, with Utility Umbrella and Protosynthesis.
    WeatherSuppress,
    /// Trapping abilities and moves (Shadow Tag, Arena Trap, Magnet Pull, Mean Look, Jaw Lock, Octolock, Fairy Lock, partial traps) against Shed Shell, pivots and Baton Pass.
    Trapping,
    /// Many volatiles on one Pokémon at once (Leech Seed, confusion, Taunt, Encore, Torment, Disable, Yawn, Curse, Syrup Bomb, Salt Cure, ...): the order between them.
    VolStack,
    /// More stacked volatiles: Destiny Bond, Grudge, Powder, No Retreat, Electrify, Spotlight, Gastro Acid, with Baton Pass and Perish Song.
    VolStack2,
    /// Imprison against Disable, Encore, Taunt, Torment, Substitute and Skill Swap, with Protect.
    Imprison,
    /// Charge users whose moves change type as they are used, which
    /// Charge's `onAfterMove` reads: to Electric (Charge ends) with an
    /// Electric Tera Blast, Terrain Pulse in Electric Terrain, Galvanize
    /// and Oricorio-Pom-Pom's Revelation Dance; away from it (Charge stays)
    /// with Hunger Switch's Aura Wheel and Liquid Voice's Overdrive. Also
    /// called moves, whose `ActiveMove` the caller's `AfterMove` reads:
    /// Galvanize users' Metronome, Copycat and Sleep Talk (with Rest), and
    /// a Liquid Voice user's Copycat (of Overdrive: Charge stays), with a
    /// Throat Chop that stops a called sound move in `ModifyMove`.
    Charge,
    /// Magic Bounce and Electrify against Charge, reflected status moves
    /// and Copycat: runMove's AfterMove reads the reflected ActiveMove.
    BounceCharge,
    /// PP exhaustion between choice and action, followed by moves that
    /// read failure or slot-use memory (Spite, Eerie Spell, Pressure).
    PpMemory,
    /// Spectral Thief (explicitly admitted Past move), positive and
    /// negative boosts, Substitute and Copycat; engine conformance probes.
    SpectralThief,
    /// Red Card drags a Mold Breaker user out for an entry ability. The
    /// outgoing activePokemon must stop suppressing the other side.
    DragAbility,
    /// Sheer Force users' Copycat and Metronome next to Sparkling Aria and
    /// Will-O-Wisp users (doubles): Sparkling Aria's `onAfterMove` reads
    /// the called move's `hasSheerForce`, and then cures no burn. Also a
    /// Magician Sparkling Aria user (with Fling, to lose its item), whose
    /// `speedSort` of `move.hitTargets` orders the cures.
    CalledAria,
    /// Protean and Libero users of two-turn moves next to Electrify, whose
    /// `onModifyType` makes the move Electric before Protean reads it on
    /// the charging turn (`twoturnmove`'s `runEvent('PrepareHit')`), with
    /// Power Herb, sun and rain (no charging turn), Copycat (a called
    /// two-turn move) and Protect.
    ProteanCharge,
    /// Gastro Acid passed into unsuppressible abilities, followed by
    /// Transform, ability replacement and another Baton Pass.
    GastroPass,
    /// Psycho Shift's dynamic primary status and cure, including called
    /// sleep transfers, status immunities and berries (a Past move).
    PsychoShift,
}

impl Target {
    pub const ALL: [Target; 14] = [
        Target::Illusion,
        Target::Imposter,
        Target::Trick,
        Target::KnockOff,
        Target::SkillSwap,
        Target::Trace,
        Target::Mega,
        Target::Tera,
        Target::Ogerpon,
        Target::Commander,
        Target::RevivalBlessing,
        Target::SleepTalk,
        Target::Metronome,
        Target::Regenerator,
    ];

    /// Targets for `conformance --target` only: `--source targets` cycles
    /// through [`Target::ALL`], whose seeds keep their battles.
    pub const EXTRA: [Target; 76] = [
        Target::Curse,
        Target::RunAway,
        Target::EjectButton,
        Target::EmergencyExit,
        Target::MetronomePast,
        Target::HealBlock,
        Target::Octolock,
        Target::MilkDrink,
        Target::Leek,
        Target::AuraGuard,
        Target::NewMegas,
        Target::MegaRedirect,
        Target::MegaSol,
        Target::HazardMoves,
        Target::FutureSight,
        Target::EjectPack,
        Target::ChargeRedirect,
        Target::Arceus,
        Target::IllusionImposter,
        Target::IllusionTera,
        Target::CommanderRevival,
        Target::TransformGimmick,
        Target::SpreadCalled,
        Target::EncoreOrder,
        Target::WishLong,
        Target::CommanderNoGuard,
        Target::PrimalWeather,
        Target::RevivalRageFist,
        Target::FormeImposter,
        Target::Symbiosis,
        Target::Pledges,
        Target::Opportunist,
        Target::NeutralizingGas,
        Target::Fling,
        Target::Struggle,
        Target::Retaliate,
        Target::ParentalBond,
        Target::MultiHit,
        Target::MultiHit2,
        Target::AbilityCopy,
        Target::TwoTurn,
        Target::SelfKo,
        Target::Pinch,
        Target::Dancer,
        Target::DancerCopycat,
        Target::Instruct,
        Target::Redirect,
        Target::Priority,
        Target::Screens,
        Target::WeatherWar,
        Target::Substitute,
        Target::ItemSwap,
        Target::StatCopy,
        Target::TypeChange,
        Target::LockIn,
        Target::Endure,
        Target::CounterInstruct,
        Target::BerryUpdate,
        Target::BerrySuppress,
        Target::UpdateItems,
        Target::WeatherSuppress,
        Target::Trapping,
        Target::VolStack,
        Target::VolStack2,
        Target::Imprison,
        Target::Charge,
        Target::BounceCharge,
        Target::PpMemory,
        Target::SpectralThief,
        Target::DragAbility,
        Target::CalledAria,
        Target::ProteanCharge,
        Target::GastroPass,
        Target::PsychoShift,
        Target::Endless,
        Target::SleepClause,
    ];

    /// The combined targets (in [`Target::EXTRA`]).
    pub const COMBOS: [Target; 5] = [
        Target::IllusionImposter,
        Target::IllusionTera,
        Target::CommanderRevival,
        Target::TransformGimmick,
        Target::SpreadCalled,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Target::Illusion => "illusion",
            Target::Imposter => "imposter",
            Target::Trick => "trick",
            Target::KnockOff => "knockoff",
            Target::SkillSwap => "skillswap",
            Target::Trace => "trace",
            Target::Mega => "mega",
            Target::Tera => "tera",
            Target::Ogerpon => "ogerpon",
            Target::Commander => "commander",
            Target::RevivalBlessing => "revivalblessing",
            Target::SleepTalk => "sleeptalk",
            Target::Metronome => "metronome",
            Target::Regenerator => "regenerator",
            Target::Curse => "curse",
            Target::RunAway => "runaway",
            Target::EjectButton => "ejectbutton",
            Target::EmergencyExit => "emergencyexit",
            Target::MetronomePast => "metronomepast",
            Target::HealBlock => "healblock",
            Target::Octolock => "octolock",
            Target::MilkDrink => "milkdrink",
            Target::Leek => "leek",
            Target::AuraGuard => "auraguard",
            Target::NewMegas => "newmegas",
            Target::MegaSol => "megasol",
            Target::HazardMoves => "hazardmoves",
            Target::FutureSight => "futuresight",
            Target::EjectPack => "ejectpack",
            Target::ChargeRedirect => "chargeredirect",
            Target::Arceus => "arceus",
            Target::IllusionImposter => "illusionimposter",
            Target::IllusionTera => "illusiontera",
            Target::CommanderRevival => "commanderrevival",
            Target::TransformGimmick => "transformgimmick",
            Target::SpreadCalled => "spreadcalled",
            Target::EncoreOrder => "encoreorder",
            Target::WishLong => "wishlong",
            Target::CommanderNoGuard => "commandernoguard",
            Target::PrimalWeather => "primalweather",
            Target::RevivalRageFist => "revivalragefist",
            Target::FormeImposter => "formeimposter",
            Target::Symbiosis => "symbiosis",
            Target::Pledges => "pledges",
            Target::Opportunist => "opportunist",
            Target::NeutralizingGas => "neutralizinggas",
            Target::Fling => "fling",
            Target::Struggle => "struggle",
            Target::Endless => "endless",
            Target::SleepClause => "sleepclause",
            Target::Retaliate => "retaliate",
            Target::ParentalBond => "parentalbond",
            Target::MultiHit => "multihit",
            Target::MultiHit2 => "multihit2",
            Target::AbilityCopy => "abilitycopy",
            Target::TwoTurn => "twoturn",
            Target::SelfKo => "selfko",
            Target::Pinch => "pinch",
            Target::Dancer => "dancer",
            Target::DancerCopycat => "dancercopycat",
            Target::BounceCharge => "bouncecharge",
            Target::PpMemory => "ppmemory",
            Target::SpectralThief => "spectralthief",
            Target::DragAbility => "dragability",
            Target::MegaRedirect => "megaredirect",
            Target::Instruct => "instruct",
            Target::Redirect => "redirect",
            Target::Priority => "priority",
            Target::Screens => "screens",
            Target::WeatherWar => "weatherwar",
            Target::Substitute => "substitute",
            Target::ItemSwap => "itemswap",
            Target::StatCopy => "statcopy",
            Target::TypeChange => "typechange",
            Target::LockIn => "lockin",
            Target::Endure => "endure",
            Target::CounterInstruct => "counterinstruct",
            Target::BerryUpdate => "berryupdate",
            Target::BerrySuppress => "berrysuppress",
            Target::UpdateItems => "updateitems",
            Target::WeatherSuppress => "weathersuppress",
            Target::Trapping => "trapping",
            Target::VolStack => "volstack",
            Target::VolStack2 => "volstack2",
            Target::Imprison => "imprison",
            Target::Charge => "charge",
            Target::CalledAria => "calledaria",
            Target::ProteanCharge => "proteancharge",
            Target::GastroPass => "gastropass",
            Target::PsychoShift => "psychoshift",
        }
    }

    pub fn from_name(name: &str) -> Option<Target> {
        Target::ALL.into_iter().chain(Target::EXTRA).find(|t| t.name() == name)
    }

    /// Whether the target's mechanic can appear in `format`: its first
    /// forced set (the one that defines it: Leek's Farfetch'd, Emergency
    /// Exit's holder, Octolock's user) has a species, move, item or Mega
    /// Stone the format's dex and pikapro's pools allow.
    pub fn applies(self, format: &Format) -> bool {
        let move_pool = crate::move_pool(format);
        self.forces(format).first().is_some_and(|f| f.possible(format, &move_pool))
    }

    /// Forces the target's sets into both teams. Returns whether its first
    /// forced set (see [`Target::applies`]) landed in either team: a
    /// battle without it doesn't have the mechanic.
    pub fn apply(self, format: &Format, rng: &mut BattleRng, teams: &mut [Team; 2]) -> bool {
        let [a, b] = self.apply_to_each(format, rng, teams);
        a || b
    }

    /// [`Target::apply`], saying for each team whether the defining set
    /// landed in it.
    pub fn apply_to_each(self, format: &Format, rng: &mut BattleRng, teams: &mut [Team; 2]) -> [bool; 2] {
        let move_pool = crate::move_pool(format);
        let forces = self.forces(format);
        let mut landed = [false; 2];
        for (team, landed) in teams.iter_mut().zip(&mut landed) {
            let mut positions: Vec<usize> = (0..team.sets.len()).collect();
            rng.shuffle(&mut positions);
            for (k, (force, &at)) in forces.iter().zip(&positions).enumerate() {
                let set = force.apply(format, &move_pool, rng, team, at);
                *landed |= k == 0 && set;
            }
        }
        landed
    }

    /// The sets the target forces, its defining one first.
    fn forces(self, format: &Format) -> Vec<Force> {
        let pool = crate::species_pool(format);
        let dex = format.dex;
        let with_ability = |a: AbilityId| -> Vec<SpeciesId> {
            pool.iter()
                .copied()
                .filter(|&s| {
                    let slots = dex.species(s).abilities;
                    [Some(slots.primary), slots.secondary, slots.hidden].contains(&Some(a))
                })
                .collect()
        };
        let known =
            |list: &[SpeciesId]| -> Vec<SpeciesId> { list.iter().copied().filter(|s| pool.contains(s)).collect() };
        let doubles = format.active_per_side() > 1;
        let forces: Vec<Force> = match self {
            Target::MegaRedirect if doubles => vec![
                Force::species(known(&[species::SCEPTILE])).mega().moves(&[
                    moves::DRAGON_PULSE,
                    moves::ENERGY_BALL,
                    moves::QUICK_ATTACK,
                    moves::PROTECT,
                ]),
                Force::species(known(&[species::MANECTRIC])).ability(abilities::LIGHTNING_ROD).mega().moves(&[
                    moves::THUNDERBOLT,
                    moves::DISCHARGE,
                    moves::SNARL,
                    moves::PROTECT,
                ]),
                Force::species(known(&[species::CLEFAIRY, species::AMOONGUSS])).moves(&[
                    moves::FOLLOW_ME,
                    moves::RAGE_POWDER,
                    moves::ALLY_SWITCH,
                    moves::HELPING_HAND,
                ]),
                Force::species(known(&[species::GASTRODON])).ability(abilities::STORM_DRAIN).moves(&[
                    moves::SURF,
                    moves::SCALD,
                    moves::ICY_WIND,
                    moves::PROTECT,
                ]),
            ],
            Target::MegaRedirect => Vec::new(),
            Target::PsychoShift => vec![
                Force::any()
                    .moves(&[moves::PSYCHO_SHIFT, moves::REST, moves::SLEEP_TALK, moves::SUBSTITUTE])
                    .outside_pool()
                    .item_from(&[items::FLAME_ORB, items::TOXIC_ORB]),
                Force::any()
                    .moves(&[moves::PSYCHO_SHIFT, moves::WILL_O_WISP, moves::TOXIC, moves::THUNDER_WAVE])
                    .outside_pool(),
                Force::species(with_ability(abilities::SYNCHRONIZE))
                    .ability(abilities::SYNCHRONIZE)
                    .moves(&[moves::PSYCHO_SHIFT, moves::REST, moves::SLEEP_TALK, moves::SPORE])
                    .outside_pool(),
                Force::any()
                    .moves(&[moves::SAFEGUARD, moves::MISTY_TERRAIN, moves::SUBSTITUTE, moves::PROTECT])
                    .item_from(&[items::LUM_BERRY]),
            ],
            Target::GastroPass => vec![
                Force::species(known(&[species::MIMIKYU])).ability(abilities::DISGUISE).moves(&[
                    moves::TRANSFORM,
                    moves::BATON_PASS,
                    moves::GASTRO_ACID,
                    moves::SHADOW_SNEAK,
                ]),
                Force::any().moves(&[moves::GASTRO_ACID, moves::BATON_PASS, moves::SUBSTITUTE, moves::TACKLE]),
                Force::species(known(&[species::PALAFIN])).ability(abilities::ZERO_TO_HERO).moves(&[
                    moves::TRANSFORM,
                    moves::BATON_PASS,
                    moves::GASTRO_ACID,
                    moves::FLIP_TURN,
                ]),
                Force::species(with_ability(abilities::INTIMIDATE)).ability(abilities::INTIMIDATE).moves(&[
                    moves::GASTRO_ACID,
                    moves::BATON_PASS,
                    moves::ROAR,
                    moves::TACKLE,
                ]),
                Force::any().moves(&[moves::TRANSFORM, moves::GASTRO_ACID, moves::SKILL_SWAP, moves::ENTRAINMENT]),
            ],
            Target::Illusion => vec![
                Force::species(known(&[species::ZOROARK, species::ZOROARK_HISUI]))
                    .ability(abilities::ILLUSION)
                    .moves(&[moves::U_TURN]),
            ],
            Target::Imposter => vec![
                Force::species(known(&[species::DITTO])).ability(abilities::IMPOSTER).moves(&[moves::TRANSFORM]),
                Force::any().moves(&[moves::TRANSFORM]),
            ],
            Target::Trick => vec![
                Force::any().moves(&[moves::TRICK]).item_from(TRICK_ITEMS),
                Force::any().moves(&[moves::SWITCHEROO]).item_from(TRICK_ITEMS),
            ],
            Target::KnockOff => vec![Force::any().moves(&[moves::KNOCK_OFF]), Force::any().moves(&[moves::KNOCK_OFF])],
            Target::SkillSwap => {
                vec![Force::any().moves(&[moves::SKILL_SWAP]), Force::any().moves(&[moves::SKILL_SWAP])]
            }
            Target::Trace => vec![Force::species(with_ability(abilities::TRACE)).ability(abilities::TRACE)],
            Target::Mega => {
                let megas: Vec<SpeciesId> =
                    pool.iter().copied().filter(|&s| !mega_stones(format, s).is_empty()).collect();
                vec![Force::species(megas.clone()).mega(), Force::species(megas).mega()]
            }
            Target::Tera => vec![Force::any().moves(&[moves::TERA_BLAST]), Force::any().moves(&[moves::TERA_BLAST])],
            Target::Ogerpon => vec![
                Force::species(known(&[
                    species::OGERPON,
                    species::OGERPON_WELLSPRING,
                    species::OGERPON_HEARTHFLAME,
                    species::OGERPON_CORNERSTONE,
                ])),
                Force::species(known(&[species::TERAPAGOS])),
            ],
            Target::Commander if doubles => vec![
                Force::species(known(&[species::TATSUGIRI, species::TATSUGIRI_DROOPY, species::TATSUGIRI_STRETCHY]))
                    .ability(abilities::COMMANDER),
                Force::species(known(&[species::DONDOZO])),
            ],
            Target::Commander => Vec::new(),
            Target::RevivalBlessing => vec![Force::any().moves(&[moves::REVIVAL_BLESSING])],
            Target::SleepTalk => {
                vec![
                    Force::any().moves(&[moves::SLEEP_TALK, moves::REST]),
                    Force::any().moves(&[moves::SPORE, moves::YAWN]),
                ]
            }
            Target::Metronome => vec![Force::any().moves(&[moves::METRONOME]), Force::any().moves(&[moves::METRONOME])],
            Target::Regenerator => vec![
                Force::species(with_ability(abilities::REGENERATOR)).ability(abilities::REGENERATOR).moves(PIVOTS),
                Force::species(with_ability(abilities::REGENERATOR)).ability(abilities::REGENERATOR).moves(PIVOTS),
            ],
            Target::Curse => {
                let ghosts: Vec<SpeciesId> =
                    pool.iter().copied().filter(|&s| dex.species(s).types.contains(Type::Ghost)).collect();
                vec![Force::species(ghosts).moves(&[moves::CURSE]), Force::any().moves(&[moves::CURSE])]
            }
            Target::RunAway => {
                let mut trappers = with_ability(abilities::SHADOW_TAG);
                trappers.extend(with_ability(abilities::ARENA_TRAP));
                vec![
                    Force::species(with_ability(abilities::RUN_AWAY)).ability(abilities::RUN_AWAY),
                    Force::any().moves(TRAPS),
                    Force::species(trappers),
                ]
            }
            Target::EjectButton => vec![
                Force::any().item_from(&[items::EJECT_BUTTON]),
                Force::any().moves(PIVOTS),
                Force::any().moves(PIVOTS),
            ],
            Target::EmergencyExit => vec![
                Force::species(with_ability(abilities::EMERGENCY_EXIT)).ability(abilities::EMERGENCY_EXIT),
                Force::species(with_ability(abilities::WIMP_OUT)).ability(abilities::WIMP_OUT),
                Force::any().moves(PIVOTS),
            ],
            Target::MetronomePast => vec![
                Force::any().moves(&[moves::METRONOME]).outside_pool(),
                Force::any().moves(&[moves::METRONOME]).outside_pool(),
            ],
            Target::HealBlock => vec![
                Force::any().moves(&[moves::HEAL_BLOCK, moves::THROAT_CHOP, moves::PSYCHIC_NOISE]),
                Force::any().moves(&[moves::COPYCAT, moves::WISH, moves::RECOVER, moves::HYPER_VOICE]),
                Force::any().moves(&[moves::SLEEP_TALK, moves::REST, moves::WISH, moves::SNORE]),
            ],
            Target::Octolock => vec![
                Force::any().moves(&[moves::OCTOLOCK]),
                Force::any().moves(&[moves::OCTOLOCK]),
                Force::species(with_ability(abilities::CLEAR_BODY)).ability(abilities::CLEAR_BODY),
                Force::species(with_ability(abilities::BIG_PECKS)).ability(abilities::BIG_PECKS),
            ],
            Target::MilkDrink => {
                vec![Force::any().moves(&[moves::MILK_DRINK]), Force::any().moves(&[moves::MILK_DRINK])]
            }
            Target::Leek => vec![
                Force::species(known(&[species::FARFETCHD, species::FARFETCHD_GALAR, species::SIRFETCHD]))
                    .item_from(&[items::LEEK]),
            ],
            Target::AuraGuard => vec![
                Force::species(known(&[species::LUCARIO])).mega_from(&[items::LUCARIONITE_Z]),
                Force::any().moves(CONTACT),
                Force::any().moves(CONTACT),
                Force::any().moves(CONTACT),
            ],
            Target::NewMegas => {
                let megas: Vec<SpeciesId> = pool
                    .iter()
                    .copied()
                    .filter(|&s| mega_stones(format, s).iter().any(|i| NEW_STONES.contains(i)))
                    .collect();
                vec![Force::species(megas.clone()).mega_from(NEW_STONES), Force::species(megas).mega_from(NEW_STONES)]
            }
            Target::MegaSol => vec![
                Force::species(known(&[species::MEGANIUM])).mega().moves(MEGA_SOL_MOVES),
                Force::any().moves(WEATHERS),
            ],
            Target::HazardMoves => vec![
                Force::any().moves(&[moves::CEASELESS_EDGE]),
                Force::any().moves(&[moves::STONE_AXE]),
                Force::species(with_ability(abilities::ROUGH_SKIN)).ability(abilities::ROUGH_SKIN),
                Force::species(with_ability(abilities::IRON_BARBS))
                    .ability(abilities::IRON_BARBS)
                    .item_from(&[items::ROCKY_HELMET]),
            ],
            Target::FutureSight => vec![
                Force::any().moves(&[moves::FUTURE_SIGHT, moves::U_TURN]),
                Force::any().moves(&[moves::DOOM_DESIRE, moves::VOLT_SWITCH]),
                Force::species(known(&[species::SCOVILLAIN])).mega(),
            ],
            Target::EjectPack => vec![
                Force::any().item_from(&[items::EJECT_PACK]),
                Force::any().item_from(&[items::EJECT_PACK]),
                Force::any().moves(&[moves::SCREECH, moves::SNARL, moves::PARTING_SHOT]),
                Force::species(with_ability(abilities::INTIMIDATE)).ability(abilities::INTIMIDATE),
            ],
            Target::ChargeRedirect if doubles => vec![
                Force::species(with_ability(abilities::LIGHTNING_ROD)).ability(abilities::LIGHTNING_ROD),
                Force::species(with_ability(abilities::STORM_DRAIN)).ability(abilities::STORM_DRAIN),
                Force::any().moves(&[moves::ELECTRO_SHOT, moves::DIVE, moves::SOLAR_BEAM, moves::METEOR_BEAM]),
                Force::any().moves(&[moves::ELECTRO_SHOT, moves::DIVE, moves::FLY, moves::PHANTOM_FORCE]),
            ],
            Target::ChargeRedirect => Vec::new(),
            Target::IllusionImposter => vec![
                Force::species(known(&[species::ZOROARK, species::ZOROARK_HISUI]))
                    .ability(abilities::ILLUSION)
                    .moves(&[moves::U_TURN, moves::TRANSFORM]),
                Force::species(known(&[species::DITTO])).ability(abilities::IMPOSTER).moves(&[moves::TRANSFORM]),
                Force::any().moves(&[moves::TRANSFORM, moves::U_TURN]),
            ],
            Target::IllusionTera => vec![
                Force::species(known(&[species::ZOROARK, species::ZOROARK_HISUI]))
                    .ability(abilities::ILLUSION)
                    .moves(&[moves::TERA_BLAST, moves::U_TURN]),
                Force::species(known(&[species::OGERPON, species::OGERPON_WELLSPRING, species::TERAPAGOS])),
                Force::any().moves(&[moves::TERA_BLAST]),
            ],
            Target::CommanderRevival if doubles => vec![
                Force::species(known(&[species::TATSUGIRI, species::TATSUGIRI_DROOPY, species::TATSUGIRI_STRETCHY]))
                    .ability(abilities::COMMANDER),
                Force::species(known(&[species::DONDOZO])).moves(&[moves::ALLY_SWITCH, moves::PROTECT]),
                Force::any().moves(&[moves::REVIVAL_BLESSING, moves::ALLY_SWITCH]),
                Force::any().moves(&[moves::ALLY_SWITCH, moves::EXPLOSION]),
            ],
            Target::CommanderRevival => Vec::new(),
            Target::TransformGimmick => {
                let megas: Vec<SpeciesId> =
                    pool.iter().copied().filter(|&s| !mega_stones(format, s).is_empty()).collect();
                let mut v = vec![
                    Force::species(known(&[species::DITTO])).ability(abilities::IMPOSTER).moves(&[moves::TRANSFORM]),
                    Force::any().moves(&[moves::TRANSFORM, moves::TERA_BLAST]),
                ];
                if !megas.is_empty() {
                    v.push(Force::species(megas.clone()).mega());
                    v.push(Force::species(megas).mega());
                } else {
                    v.push(Force::any().moves(&[moves::TERA_BLAST]));
                }
                v
            }
            Target::SpreadCalled => vec![
                Force::any().moves(&[moves::METRONOME, moves::COPYCAT, moves::SLEEP_TALK, moves::REST]),
                Force::any().moves(&[moves::EARTHQUAKE, moves::ROCK_SLIDE, moves::HEAT_WAVE, moves::INSTRUCT]),
                Force::species(known(&[
                    species::ORICORIO,
                    species::ORICORIO_PAU,
                    species::ORICORIO_POM_POM,
                    species::ORICORIO_SENSU,
                ]))
                .ability(abilities::DANCER)
                .moves(&[moves::TEETER_DANCE, moves::REVELATION_DANCE]),
                Force::any().moves(&[moves::MAGIC_COAT, moves::SNARL, moves::ICY_WIND, moves::DAZZLING_GLEAM]),
            ],
            Target::Arceus => vec![
                Force::species(known(ARCEUS_FORMES))
                    .ability(abilities::MULTITYPE)
                    .moves(&[moves::JUDGMENT])
                    .item_from(PLATES),
            ],
            Target::EncoreOrder => vec![
                Force::species(with_ability(abilities::PRANKSTER)).ability(abilities::PRANKSTER).moves(&[
                    moves::ENCORE,
                    moves::QUASH,
                    moves::AFTER_YOU,
                ]),
                Force::species(with_ability(abilities::PRANKSTER))
                    .ability(abilities::PRANKSTER)
                    .moves(&[moves::ENCORE, moves::QUASH]),
                Force::any().moves(&[moves::QUASH, moves::AFTER_YOU, moves::INSTRUCT, moves::ENCORE]),
                Force::any().moves(PRIORITY),
                Force::any().moves(PRIORITY),
            ],
            Target::WishLong => vec![
                Force::any().moves(&[moves::WISH, moves::FUTURE_SIGHT, moves::DOOM_DESIRE]),
                Force::any().moves(&[moves::WISH, moves::FUTURE_SIGHT, moves::PROTECT]),
                Force::any().moves(&[moves::WISH, moves::DOOM_DESIRE]),
                Force::any().moves(&[moves::FUTURE_SIGHT, moves::HEALING_WISH]),
                Force::species(with_ability(abilities::UNAWARE)).ability(abilities::UNAWARE).moves(&[moves::WISH]),
                Force::any().moves(&[moves::TRANSFORM, moves::WISH]),
            ],
            Target::CommanderNoGuard if doubles => vec![
                Force::species(known(&[species::TATSUGIRI, species::TATSUGIRI_DROOPY, species::TATSUGIRI_STRETCHY]))
                    .ability(abilities::COMMANDER)
                    .moves(&[moves::PROTECT]),
                Force::species(known(&[species::DONDOZO])).moves(&[moves::ORDER_UP, moves::PROTECT]),
                Force::species(with_ability(abilities::NO_GUARD)).ability(abilities::NO_GUARD).moves(&[
                    moves::PERISH_SONG,
                    moves::TEATIME,
                    moves::LOCK_ON,
                ]),
                Force::any().moves(&[moves::PERISH_SONG, moves::TEATIME, moves::LOCK_ON]),
            ],
            Target::CommanderNoGuard => Vec::new(),
            // Showdown's custom games (the Gen 9 presets) accept any ability;
            // Champions' validator doesn't (`obtainable_abilities`).
            Target::PrimalWeather if !format.obtainable_abilities => vec![
                Force::any().ability(abilities::PRIMORDIAL_SEA).moves(PRIMAL_MOVES),
                Force::any().ability(abilities::DESOLATE_LAND).moves(PRIMAL_MOVES),
                Force::any().moves(PRIMAL_MOVES).item_from(&[items::UTILITY_UMBRELLA]),
                Force::any().moves(&[moves::SCALD, moves::WILL_O_WISP, moves::TEMPER_FLARE, moves::STOMPING_TANTRUM]),
                Force::species({
                    let mut list = with_ability(abilities::CLOUD_NINE);
                    list.extend(with_ability(abilities::AIR_LOCK));
                    list
                }),
            ],
            Target::PrimalWeather => Vec::new(),
            Target::FormeImposter => vec![
                Force::species(known(&[species::DITTO])).ability(abilities::IMPOSTER).moves(&[moves::TRANSFORM]),
                Force::species(known(FORME_SPECIES)),
                Force::any().moves(&[moves::TRANSFORM, moves::SKILL_SWAP, moves::ROLE_PLAY]),
                Force::species(known(FORME_SPECIES)),
            ],
            Target::Symbiosis if doubles => vec![
                Force::species(with_ability(abilities::SYMBIOSIS))
                    .ability(abilities::SYMBIOSIS)
                    .item_from(SYMBIOSIS_ITEMS),
                Force::any().item_from(CONSUMABLES),
                Force::species(with_ability(abilities::SYMBIOSIS))
                    .ability(abilities::SYMBIOSIS)
                    .item_from(SYMBIOSIS_ITEMS),
                Force::any().item_from(CONSUMABLES).moves(&[
                    moves::FLING,
                    moves::BUG_BITE,
                    moves::KNOCK_OFF,
                    moves::TRICK,
                ]),
            ],
            Target::Symbiosis => Vec::new(),
            Target::Pledges if doubles => vec![
                Force::any().moves(PLEDGES),
                Force::any().moves(PLEDGES),
                Force::any().moves(PLEDGES),
                Force::any().moves(&[moves::AFTER_YOU, moves::QUASH, moves::INSTRUCT, moves::ALLY_SWITCH]),
            ],
            Target::Pledges => Vec::new(),
            Target::Opportunist => vec![
                Force::species(with_ability(abilities::OPPORTUNIST))
                    .ability(abilities::OPPORTUNIST)
                    .item_from(&[items::MIRROR_HERB]),
                Force::any().item_from(&[items::MIRROR_HERB]).moves(&[moves::SPICY_EXTRACT, moves::SWORDS_DANCE]),
                Force::any().moves(&[moves::SHELL_SMASH, moves::SPICY_EXTRACT, moves::DRAGON_DANCE, moves::PSYCH_UP]),
                Force::species(with_ability(abilities::DEFIANT)).ability(abilities::DEFIANT),
                Force::species(with_ability(abilities::INTIMIDATE)).ability(abilities::INTIMIDATE),
                Force::species(with_ability(abilities::OPPORTUNIST)).ability(abilities::OPPORTUNIST),
            ],
            Target::NeutralizingGas => vec![
                Force::species(with_ability(abilities::NEUTRALIZING_GAS))
                    .ability(abilities::NEUTRALIZING_GAS)
                    .moves(PIVOTS),
                Force::species(with_ability(abilities::PROTOSYNTHESIS))
                    .ability(abilities::PROTOSYNTHESIS)
                    .item_from(&[items::BOOSTER_ENERGY]),
                Force::species(with_ability(abilities::INTIMIDATE)).ability(abilities::INTIMIDATE),
                Force::species({
                    let mut list = with_ability(abilities::DROUGHT);
                    list.extend(with_ability(abilities::DRIZZLE));
                    list.extend(with_ability(abilities::SAND_STREAM));
                    list.extend(with_ability(abilities::SNOW_WARNING));
                    list
                }),
                Force::species(with_ability(abilities::IMPOSTER)).ability(abilities::IMPOSTER),
                Force::species(with_ability(abilities::NEUTRALIZING_GAS))
                    .ability(abilities::NEUTRALIZING_GAS)
                    .moves(PIVOTS),
            ],
            Target::Fling => vec![
                Force::any().moves(&[moves::FLING]).item_from(FLING_ITEMS),
                Force::any()
                    .moves(&[moves::FLING, moves::BUG_BITE, moves::PLUCK, moves::INCINERATE])
                    .item_from(FLING_ITEMS),
                Force::any().moves(&[moves::MAGIC_ROOM, moves::TRICK, moves::RECYCLE, moves::KNOCK_OFF]),
                Force::species(with_ability(abilities::KLUTZ))
                    .ability(abilities::KLUTZ)
                    .moves(&[moves::FLING])
                    .item_from(FLING_ITEMS),
                Force::species(with_ability(abilities::UNBURDEN))
                    .ability(abilities::UNBURDEN)
                    .moves(&[moves::FLING])
                    .item_from(FLING_ITEMS),
            ],
            // Only where the clause is: elsewhere there is nothing to
            // force.
            Target::Endless if !format.endless_battle_clause => vec![],
            // Only where the rule is.
            Target::SleepClause if !format.sleep_clause_mod => vec![],
            Target::SleepClause => vec![
                Force::any().moves(&[moves::SPORE, moves::SLEEP_POWDER, moves::YAWN, moves::SAFEGUARD]),
                Force::any().moves(&[moves::HYPNOSIS, moves::SING, moves::YAWN, moves::REST]),
                Force::species(with_ability(abilities::EFFECT_SPORE)).ability(abilities::EFFECT_SPORE).moves(&[
                    moves::SPORE,
                    moves::PROTECT,
                    moves::SLEEP_TALK,
                    moves::REST,
                ]),
                Force::any()
                    .moves(&[moves::SPORE, moves::ELECTRIC_TERRAIN, moves::MISTY_TERRAIN, moves::PSYCHO_SHIFT])
                    .outside_pool(),
                Force::any().moves(&[moves::GRASS_WHISTLE, moves::SLEEP_POWDER, moves::YAWN, moves::SKILL_SWAP]),
                Force::any().moves(&[moves::YAWN, moves::HYPNOSIS, moves::PROTECT]),
            ],
            // No set has both a restorative berry and a way to get one
            // back: whether a team "started with" the two depends on
            // its order (the last Pokémon looked at decides the berry), so
            // the same sets lose, tie with the message or tie without.
            Target::Endless => vec![
                Force::any().item_from(&[items::SITRUS_BERRY, items::ORAN_BERRY]).moves(&[
                    moves::HEAL_PULSE,
                    moves::NUZZLE,
                    moves::FLORAL_HEALING,
                    moves::SKILL_SWAP,
                ]),
                Force::any().moves(&[moves::HEAL_PULSE, moves::NUZZLE, moves::RECYCLE, moves::BUG_BITE]),
                Force::any().item_from(&[items::LEPPA_BERRY]).moves(&[
                    moves::HEAL_PULSE,
                    moves::NUZZLE,
                    moves::FLING,
                    moves::ENTRAINMENT,
                ]),
                Force::any().moves(&[moves::FLORAL_HEALING, moves::NUZZLE, moves::PLUCK, moves::RECYCLE]),
                Force::any().moves(&[moves::HEAL_PULSE, moves::NUZZLE, moves::INGRAIN, moves::BLOCK]),
                Force::any().moves(&[moves::FLORAL_HEALING, moves::NUZZLE, moves::SPLASH, moves::HEAL_PULSE]),
            ],
            Target::Struggle => vec![
                Force::any().item_from(LOCK_ITEMS).moves(LOCK_MOVES),
                Force::any().moves(&[moves::ENCORE, moves::DISABLE, moves::TAUNT, moves::IMPRISON]),
                Force::any().item_from(LOCK_ITEMS).moves(LOCK_MOVES),
                Force::any().moves(&[moves::THROAT_CHOP, moves::GRAVITY, moves::ENCORE, moves::BELCH]),
            ],
            Target::Retaliate => vec![
                Force::any().moves(&[moves::METAL_BURST, moves::COUNTER, moves::MIRROR_COAT, moves::COMEUPPANCE]),
                Force::any().moves(&[moves::U_TURN, moves::VOLT_SWITCH, moves::FLIP_TURN, moves::PARTING_SHOT]),
                Force::any().moves(&[moves::ASSURANCE, moves::AVALANCHE, moves::PAYBACK, moves::DESTINY_BOND]),
                Force::any().item_from(&[items::EJECT_BUTTON, items::RED_CARD]),
                Force::any().moves(&[moves::METAL_BURST, moves::COMEUPPANCE, moves::U_TURN, moves::DRAGON_TAIL]),
            ],
            Target::ParentalBond if !format.obtainable_abilities => vec![
                Force::any().ability(abilities::PARENTAL_BOND).moves(BOND_MOVES),
                Force::any().item_from(BOND_ITEMS),
                Force::any().ability(abilities::PARENTAL_BOND).moves(BOND_MOVES_2),
                Force::species({
                    let mut list = with_ability(abilities::BERSERK);
                    list.extend(with_ability(abilities::ANGER_SHELL));
                    list.extend(with_ability(abilities::DISGUISE));
                    list.extend(with_ability(abilities::STURDY));
                    list
                })
                .item_from(BOND_ITEMS),
            ],
            Target::ParentalBond => Vec::new(),
            Target::MultiHit => vec![
                Force::any().moves(MULTI_HITS).item_from(&[items::LOADED_DICE, items::KINGS_ROCK, items::LIFE_ORB]),
                Force::species(with_ability(abilities::STAMINA)).ability(abilities::STAMINA).item_from(BOND_ITEMS),
                Force::species(with_ability(abilities::WEAK_ARMOR)).ability(abilities::WEAK_ARMOR),
                Force::species(with_ability(abilities::COTTON_DOWN)).ability(abilities::COTTON_DOWN),
                Force::species(with_ability(abilities::ANGER_SHELL)).ability(abilities::ANGER_SHELL),
                Force::any().moves(MULTI_HITS),
            ],
            Target::MultiHit2 => vec![
                Force::any().moves(MULTI_HITS).item_from(&[items::LOADED_DICE, items::KINGS_ROCK, items::LIFE_ORB]),
                Force::species(with_ability(abilities::WANDERING_SPIRIT)).ability(abilities::WANDERING_SPIRIT),
                Force::species(with_ability(abilities::PICKPOCKET)).ability(abilities::PICKPOCKET),
                Force::species(with_ability(abilities::ELECTROMORPHOSIS)).ability(abilities::ELECTROMORPHOSIS),
                Force::species(with_ability(abilities::TOXIC_DEBRIS)).ability(abilities::TOXIC_DEBRIS),
                Force::species(with_ability(abilities::SEED_SOWER)).ability(abilities::SEED_SOWER),
            ],
            Target::AbilityCopy => vec![
                Force::any().moves(&[moves::SKILL_SWAP, moves::ROLE_PLAY, moves::ENTRAINMENT, moves::DOODLE]),
                Force::species({
                    let mut list = with_ability(abilities::RECEIVER);
                    list.extend(with_ability(abilities::POWER_OF_ALCHEMY));
                    list
                }),
                Force::any().moves(&[moves::SIMPLE_BEAM, moves::WORRY_SEED, moves::GASTRO_ACID, moves::SKILL_SWAP]),
                Force::species(with_ability(abilities::ZERO_TO_HERO)).ability(abilities::ZERO_TO_HERO),
                Force::species({
                    let mut list = with_ability(abilities::COMATOSE);
                    list.extend(with_ability(abilities::TRUANT));
                    list.extend(with_ability(abilities::SLOW_START));
                    list.extend(with_ability(abilities::WONDER_GUARD));
                    list
                }),
                Force::species(with_ability(abilities::PROTOSYNTHESIS))
                    .ability(abilities::PROTOSYNTHESIS)
                    .item_from(&[items::BOOSTER_ENERGY]),
            ],
            Target::TwoTurn => vec![
                Force::any().moves(&[moves::FLY, moves::BOUNCE, moves::DIG, moves::DIVE]),
                Force::any().moves(&[moves::GRAVITY, moves::SMACK_DOWN, moves::THUNDER, moves::HURRICANE]),
                Force::any()
                    .moves(&[moves::PHANTOM_FORCE, moves::SOLAR_BEAM, moves::METEOR_BEAM, moves::ELECTRO_SHOT])
                    .item_from(&[items::POWER_HERB]),
                Force::any().moves(&[moves::EARTHQUAKE, moves::SURF, moves::WHIRLPOOL, moves::ENCORE]),
                Force::species(with_ability(abilities::NO_GUARD)).ability(abilities::NO_GUARD),
            ],
            Target::SelfKo => vec![
                Force::any().moves(&[moves::DESTINY_BOND, moves::EXPLOSION, moves::MEMENTO, moves::FINAL_GAMBIT]),
                Force::any().moves(&[
                    moves::HEALING_WISH,
                    moves::LUNAR_DANCE,
                    moves::PERISH_SONG,
                    moves::MISTY_EXPLOSION,
                ]),
                Force::any().item_from(&[items::ROCKY_HELMET, items::LIFE_ORB]),
                Force::any().moves(&[moves::DESTINY_BOND, moves::EXPLOSION, moves::MEMENTO, moves::FINAL_GAMBIT]),
            ],
            Target::Pinch => vec![
                Force::any().item_from(PINCH_ITEMS),
                Force::species({
                    let mut list = with_ability(abilities::GLUTTONY);
                    list.extend(with_ability(abilities::RIPEN));
                    list.extend(with_ability(abilities::CHEEK_POUCH));
                    list.extend(with_ability(abilities::HARVEST));
                    list.extend(with_ability(abilities::CUD_CHEW));
                    list
                })
                .item_from(PINCH_ITEMS),
                Force::any()
                    .moves(&[moves::RECYCLE, moves::BELCH, moves::STUFF_CHEEKS, moves::TEATIME])
                    .item_from(PINCH_ITEMS),
                Force::any().item_from(PINCH_ITEMS),
            ],
            Target::Dancer => vec![
                Force::species(with_ability(abilities::DANCER)).ability(abilities::DANCER).moves(DANCES),
                Force::any().moves(DANCES),
                Force::any().moves(DANCES_2),
                Force::species(with_ability(abilities::DANCER)).ability(abilities::DANCER).moves(DANCES_2),
            ],
            Target::DancerCopycat if doubles => vec![
                Force::species(with_ability(abilities::DANCER)).ability(abilities::DANCER).moves(&[
                    moves::FIERY_DANCE,
                    moves::TEETER_DANCE,
                    moves::FEATHER_DANCE,
                    moves::COPYCAT,
                ]),
                // A second holder on each side models gaining Dancer
                // through Role Play or Trace; the native pool has only
                // Oricorio and Species Clause permits one per team.
                Force::any().ability(abilities::DANCER).moves(&[
                    moves::QUIVER_DANCE,
                    moves::TEETER_DANCE,
                    moves::FEATHER_DANCE,
                    moves::COPYCAT,
                ]),
            ],
            Target::DancerCopycat => Vec::new(),
            Target::Instruct if doubles => vec![
                Force::any().moves(&[moves::INSTRUCT, moves::AFTER_YOU, moves::QUASH, moves::ALLY_SWITCH]),
                Force::any().moves(INSTRUCTED),
                Force::any().moves(INSTRUCTED_2),
                Force::any().moves(&[moves::INSTRUCT, moves::ROUND, moves::FUSION_FLARE, moves::FUSION_BOLT]),
            ],
            Target::Instruct => Vec::new(),
            Target::Redirect if doubles => vec![
                Force::any().moves(&[moves::FOLLOW_ME, moves::RAGE_POWDER, moves::ALLY_SWITCH, moves::SNIPE_SHOT]),
                Force::species({
                    let mut list = with_ability(abilities::STALWART);
                    list.extend(with_ability(abilities::PROPELLER_TAIL));
                    list
                }),
                Force::species({
                    let mut list = with_ability(abilities::STORM_DRAIN);
                    list.extend(with_ability(abilities::LIGHTNING_ROD));
                    list
                }),
                Force::any().moves(&[moves::THUNDERBOLT, moves::SURF, moves::POLLEN_PUFF, moves::BEAT_UP]),
            ],
            Target::Redirect => Vec::new(),
            Target::Priority => vec![
                Force::species(with_ability(abilities::PRANKSTER)).ability(abilities::PRANKSTER),
                Force::any().moves(&[moves::PSYCHIC_TERRAIN, moves::TRICK_ROOM, moves::TAILWIND, moves::AFTER_YOU]),
                Force::species({
                    let mut list = with_ability(abilities::QUEENLY_MAJESTY);
                    list.extend(with_ability(abilities::DAZZLING));
                    list.extend(with_ability(abilities::ARMOR_TAIL));
                    list
                }),
                Force::species({
                    let mut list = with_ability(abilities::GALE_WINGS);
                    list.extend(with_ability(abilities::TRIAGE));
                    list
                })
                .item_from(&[items::QUICK_CLAW, items::CUSTAP_BERRY, items::LAGGING_TAIL]),
                Force::any().moves(PRIORITY).item_from(&[items::QUICK_CLAW, items::CUSTAP_BERRY]),
            ],
            Target::Screens => vec![
                Force::any().moves(&[moves::REFLECT, moves::LIGHT_SCREEN, moves::AURORA_VEIL, moves::SNOWSCAPE]),
                Force::any().moves(&[moves::BRICK_BREAK, moves::PSYCHIC_FANGS, moves::RAGING_BULL, moves::DEFOG]),
                Force::species(with_ability(abilities::INFILTRATOR)).ability(abilities::INFILTRATOR),
                Force::any().moves(&[moves::REFLECT, moves::LIGHT_SCREEN, moves::COURT_CHANGE, moves::FROST_BREATH]),
            ],
            Target::WeatherWar => vec![
                Force::species({
                    let mut list = with_ability(abilities::DROUGHT);
                    list.extend(with_ability(abilities::DRIZZLE));
                    list
                })
                .moves(WEATHER_USERS),
                Force::species({
                    let mut list = with_ability(abilities::SAND_STREAM);
                    list.extend(with_ability(abilities::SNOW_WARNING));
                    list
                })
                .moves(WEATHER_USERS)
                .item_from(&[items::UTILITY_UMBRELLA]),
                Force::species({
                    let mut list = with_ability(abilities::CLOUD_NINE);
                    list.extend(with_ability(abilities::AIR_LOCK));
                    list
                }),
                Force::species({
                    let mut list = with_ability(abilities::PROTOSYNTHESIS);
                    list.extend(with_ability(abilities::QUARK_DRIVE));
                    list.extend(with_ability(abilities::MIMICRY));
                    list
                })
                .item_from(&[items::UTILITY_UMBRELLA, items::BOOSTER_ENERGY, items::ELECTRIC_SEED]),
                Force::any().moves(TERRAIN_USERS).item_from(SEEDS),
            ],
            Target::Substitute => vec![
                Force::any().moves(&[moves::SUBSTITUTE, moves::SHED_TAIL, moves::BATON_PASS, moves::DRAINING_KISS]),
                Force::any().moves(&[moves::BOOMBURST, moves::POPULATION_BOMB, moves::CURSE, moves::LEECH_SEED]),
                Force::species(with_ability(abilities::INFILTRATOR)).ability(abilities::INFILTRATOR),
                Force::any().moves(&[moves::SUBSTITUTE, moves::U_TURN, moves::SUPER_FANG, moves::TRANSFORM]),
            ],
            Target::ItemSwap => vec![
                Force::any().moves(&[moves::THIEF, moves::COVET, moves::SWITCHEROO, moves::CORROSIVE_GAS]),
                Force::species({
                    let mut list = with_ability(abilities::PICKPOCKET);
                    list.extend(with_ability(abilities::MAGICIAN));
                    list
                }),
                Force::species(with_ability(abilities::STICKY_HOLD))
                    .ability(abilities::STICKY_HOLD)
                    .item_from(TRICK_ITEMS),
                Force::any().moves(&[moves::TRICK, moves::KNOCK_OFF, moves::FLING]).item_from(SWAP_ITEMS),
            ],
            Target::StatCopy => vec![
                Force::any().moves(&[moves::PSYCH_UP, moves::POWER_SWAP, moves::GUARD_SWAP, moves::SPEED_SWAP]),
                Force::any().moves(&[moves::POWER_SPLIT, moves::GUARD_SPLIT, moves::TOPSY_TURVY, moves::CLEAR_SMOG]),
                Force::any().moves(&[moves::SWORDS_DANCE, moves::BATON_PASS, moves::HAZE, moves::COACHING]),
                Force::species(with_ability(abilities::COSTAR)).ability(abilities::COSTAR),
            ],
            Target::TypeChange => vec![
                Force::any().moves(&[moves::SOAK, moves::MAGIC_POWDER, moves::FORESTS_CURSE, moves::REFLECT_TYPE]),
                Force::any().moves(&[moves::CONVERSION, moves::CONVERSION_2, moves::ROOST, moves::DOUBLE_SHOCK]),
                Force::species({
                    let mut list = with_ability(abilities::PROTEAN);
                    list.extend(with_ability(abilities::LIBERO));
                    list.extend(with_ability(abilities::COLOR_CHANGE));
                    list
                }),
                Force::any().moves(&[moves::SOAK, moves::TERA_BLAST, moves::BURN_UP, moves::REFLECT_TYPE]),
            ],
            Target::LockIn => vec![
                Force::any().moves(&[moves::OUTRAGE, moves::RAGING_FURY, moves::THRASH, moves::UPROAR]),
                Force::any().moves(&[moves::ROLLOUT, moves::PETAL_DANCE, moves::ENCORE, moves::PROTECT]),
                Force::any().moves(&[moves::ENCORE, moves::DISABLE, moves::SPITE, moves::INSTRUCT]),
                Force::any().moves(&[moves::OUTRAGE, moves::UPROAR, moves::YAWN, moves::REST]),
            ],
            Target::Endure => vec![
                Force::any().moves(&[moves::ENDURE, moves::FALSE_SWIPE, moves::DOUBLE_EDGE, moves::FLAIL]),
                Force::species(with_ability(abilities::STURDY)).ability(abilities::STURDY).item_from(BOND_ITEMS),
                Force::any().moves(MULTI_HITS).item_from(&[items::FOCUS_SASH, items::LOADED_DICE]),
                Force::any().moves(&[moves::ENDEAVOR, moves::SUPER_FANG, moves::FINAL_GAMBIT, moves::PAIN_SPLIT]),
            ],
            Target::CounterInstruct if doubles => vec![
                Force::any().moves(&[moves::COUNTER, moves::MIRROR_COAT, moves::INSTRUCT, moves::METAL_BURST]),
                Force::any().moves(&[moves::INSTRUCT, moves::COUNTER, moves::MIRROR_COAT, moves::COMEUPPANCE]),
                Force::any().moves(&[moves::COUNTER, moves::MIRROR_COAT, moves::INSTRUCT, moves::PROTECT]),
                Force::any().moves(&[moves::INSTRUCT, moves::METAL_BURST, moves::COMEUPPANCE, moves::MIRROR_COAT]),
            ],
            Target::CounterInstruct => Vec::new(),
            Target::BerryUpdate => vec![
                Force::any().item_from(UPDATE_BERRIES).moves(&[
                    moves::CONFUSE_RAY,
                    moves::SWAGGER,
                    moves::SPITE,
                    moves::SUPER_FANG,
                ]),
                Force::species(with_ability(abilities::GLUTTONY))
                    .ability(abilities::GLUTTONY)
                    .item_from(UPDATE_BERRIES),
                Force::species(with_ability(abilities::RIPEN)).ability(abilities::RIPEN).item_from(UPDATE_BERRIES),
                Force::species(with_ability(abilities::CUD_CHEW))
                    .ability(abilities::CUD_CHEW)
                    .item_from(UPDATE_BERRIES),
                Force::species(with_ability(abilities::HARVEST))
                    .ability(abilities::HARVEST)
                    .item_from(UPDATE_BERRIES)
                    .moves(&[moves::SUNNY_DAY, moves::RECYCLE]),
                Force::any().item_from(UPDATE_BERRIES).moves(&[
                    moves::WILL_O_WISP,
                    moves::THUNDER_WAVE,
                    moves::TOXIC,
                    moves::BELLY_DRUM,
                ]),
            ],
            Target::BerrySuppress => vec![
                Force::any().item_from(UPDATE_BERRIES).moves(&[
                    moves::MAGIC_ROOM,
                    moves::EMBARGO,
                    moves::CONFUSE_RAY,
                    moves::SUPER_FANG,
                ]),
                Force::species(with_ability(abilities::UNNERVE)).ability(abilities::UNNERVE).moves(&[
                    moves::U_TURN,
                    moves::WILL_O_WISP,
                    moves::SWAGGER,
                ]),
                Force::species(with_ability(abilities::KLUTZ))
                    .ability(abilities::KLUTZ)
                    .item_from(UPDATE_BERRIES)
                    .moves(&[moves::TRICK, moves::SKILL_SWAP, moves::BESTOW]),
                Force::any().item_from(UPDATE_BERRIES).moves(&[
                    moves::BUG_BITE,
                    moves::PLUCK,
                    moves::INCINERATE,
                    moves::KNOCK_OFF,
                ]),
                Force::any()
                    .moves(&[moves::GASTRO_ACID, moves::EMBARGO, moves::MAGIC_ROOM, moves::TOXIC])
                    .item_from(UPDATE_BERRIES),
                Force::species(with_ability(abilities::NEUTRALIZING_GAS))
                    .ability(abilities::NEUTRALIZING_GAS)
                    .moves(PIVOTS),
            ],
            Target::UpdateItems => {
                vec![
                    Force::any().item_from(&[items::MENTAL_HERB]).moves(&[moves::PROTECT, moves::SUBSTITUTE]),
                    Force::any().moves(&[moves::TAUNT, moves::ENCORE, moves::DISABLE, moves::TORMENT]),
                    Force::species(with_ability(abilities::PROTOSYNTHESIS))
                        .ability(abilities::PROTOSYNTHESIS)
                        .item_from(&[items::BOOSTER_ENERGY])
                        .moves(&[moves::SUNNY_DAY, moves::SKILL_SWAP]),
                    Force::species(with_ability(abilities::QUARK_DRIVE))
                        .ability(abilities::QUARK_DRIVE)
                        .item_from(&[items::BOOSTER_ENERGY])
                        .moves(&[moves::ELECTRIC_TERRAIN, moves::GASTRO_ACID]),
                    Force::any().item_from(&[items::UTILITY_UMBRELLA]).moves(&[
                        moves::SUNNY_DAY,
                        moves::RAIN_DANCE,
                        moves::WEATHER_BALL,
                        moves::TRICK,
                    ]),
                    Force::any()
                        .moves(&[moves::HEAL_BLOCK, moves::ATTRACT, moves::TRANSFORM, moves::TRICK])
                        .item_from(&[items::MENTAL_HERB, items::BOOSTER_ENERGY, items::UTILITY_UMBRELLA]),
                ]
            }
            Target::WeatherSuppress => vec![
                Force::species(with_ability(abilities::DROUGHT)).ability(abilities::DROUGHT).moves(PIVOTS),
                Force::species(with_ability(abilities::CLOUD_NINE)).ability(abilities::CLOUD_NINE).moves(PIVOTS),
                Force::species(with_ability(abilities::DRIZZLE))
                    .ability(abilities::DRIZZLE)
                    .item_from(&[items::UTILITY_UMBRELLA]),
                Force::species(with_ability(abilities::AIR_LOCK))
                    .ability(abilities::AIR_LOCK)
                    .moves(&[moves::SKILL_SWAP, moves::GASTRO_ACID]),
                Force::species(with_ability(abilities::PROTOSYNTHESIS))
                    .ability(abilities::PROTOSYNTHESIS)
                    .item_from(&[items::BOOSTER_ENERGY, items::UTILITY_UMBRELLA]),
                Force::species(with_ability(abilities::NEUTRALIZING_GAS))
                    .ability(abilities::NEUTRALIZING_GAS)
                    .moves(&[moves::U_TURN, moves::SNOWSCAPE, moves::SANDSTORM, moves::WEATHER_BALL]),
            ],
            Target::Trapping => vec![
                Force::species({
                    let mut list = with_ability(abilities::SHADOW_TAG);
                    list.extend(with_ability(abilities::ARENA_TRAP));
                    list.extend(with_ability(abilities::MAGNET_PULL));
                    list
                }),
                Force::any().moves(&[moves::MEAN_LOOK, moves::BLOCK, moves::JAW_LOCK, moves::ANCHOR_SHOT]),
                Force::any().moves(&[moves::SPIRIT_SHACKLE, moves::THOUSAND_WAVES, moves::OCTOLOCK, moves::FAIRY_LOCK]),
                Force::any().moves(&[moves::INFESTATION, moves::WHIRLPOOL, moves::SAND_TOMB, moves::SALT_CURE]),
                Force::any().item_from(&[items::SHED_SHELL]).moves(&[
                    moves::U_TURN,
                    moves::BATON_PASS,
                    moves::PARTING_SHOT,
                    moves::TELEPORT,
                ]),
                Force::any().moves(&[moves::INGRAIN, moves::PERISH_SONG, moves::SNAP_TRAP, moves::BIND]),
            ],
            Target::VolStack => vec![
                Force::any().moves(&[moves::LEECH_SEED, moves::CONFUSE_RAY, moves::TAUNT, moves::ENCORE]),
                Force::any().moves(&[moves::TORMENT, moves::DISABLE, moves::HEAL_BLOCK, moves::EMBARGO]),
                Force::any().moves(&[moves::YAWN, moves::CURSE, moves::ATTRACT, moves::SYRUP_BOMB]),
                Force::any().moves(&[moves::TAR_SHOT, moves::SALT_CURE, moves::SMACK_DOWN, moves::TELEKINESIS]),
                Force::any().moves(&[moves::SUBSTITUTE, moves::AQUA_RING, moves::INGRAIN, moves::MAGNET_RISE]),
                Force::any().moves(&[moves::FOCUS_ENERGY, moves::LASER_FOCUS, moves::CHARGE, moves::STOCKPILE]),
            ],
            Target::VolStack2 => vec![
                Force::any().moves(&[moves::DESTINY_BOND, moves::GRUDGE, moves::POWDER, moves::NIGHTMARE]),
                Force::any().moves(&[moves::NO_RETREAT, moves::ELECTRIFY, moves::SPOTLIGHT, moves::GASTRO_ACID]),
                Force::any().moves(&[moves::LEECH_SEED, moves::TAUNT, moves::TORMENT, moves::BATON_PASS]),
                Force::any().moves(&[moves::INFESTATION, moves::OCTOLOCK, moves::PERISH_SONG, moves::YAWN]),
                Force::any().moves(&[moves::CONFUSE_RAY, moves::ENCORE, moves::SUBSTITUTE, moves::FLATTER]),
                Force::any().moves(&[moves::HEAL_BLOCK, moves::EMBARGO, moves::CURSE, moves::SWAGGER]),
            ],
            Target::Imprison => vec![
                Force::any().moves(&[moves::IMPRISON, moves::DISABLE, moves::PROTECT, moves::ENCORE]),
                Force::any().moves(&[moves::PROTECT, moves::SUBSTITUTE, moves::IMPRISON, moves::TAUNT]),
                Force::any().moves(&[moves::DISABLE, moves::PROTECT, moves::SUBSTITUTE, moves::TORMENT]),
                Force::any().moves(&[moves::IMPRISON, moves::SUBSTITUTE, moves::DISABLE, moves::SKILL_SWAP]),
            ],
            Target::PpMemory => vec![
                Force::any().moves(&[moves::RECOVER, moves::STOMPING_TANTRUM, moves::TEMPER_FLARE, moves::LAST_RESORT]),
                Force::any().moves(&[moves::SPITE, moves::EERIE_SPELL, moves::ENCORE, moves::DISABLE]),
                Force::any().moves(&[moves::INSTRUCT, moves::SPITE, moves::HEAL_PULSE, moves::PROTECT]),
                Force::species(with_ability(abilities::PRESSURE))
                    .ability(abilities::PRESSURE)
                    .moves(&[moves::SPITE, moves::EERIE_SPELL]),
            ],
            Target::DragAbility => vec![
                Force::species(with_ability(abilities::CLEAR_BODY))
                    .ability(abilities::CLEAR_BODY)
                    .item_from(&[items::RED_CARD])
                    .moves(&[moves::TACKLE, moves::PROTECT, moves::TRICK_ROOM, moves::STEALTH_ROCK]),
                Force::species(with_ability(abilities::MOLD_BREAKER)).ability(abilities::MOLD_BREAKER).moves(&[
                    moves::TACKLE,
                    moves::BREAKING_SWIPE,
                    moves::BRICK_BREAK,
                    moves::ROAR,
                ]),
                Force::species(with_ability(abilities::INTIMIDATE)).ability(abilities::INTIMIDATE).moves(&[
                    moves::TACKLE,
                    moves::LEER,
                    moves::PROTECT,
                    moves::ROAR,
                ]),
                Force::species(with_ability(abilities::MIRROR_ARMOR))
                    .ability(abilities::MIRROR_ARMOR)
                    .item_from(&[items::RED_CARD])
                    .moves(&[moves::TACKLE, moves::ROOST, moves::ROAR, moves::PROTECT]),
            ],
            Target::SpectralThief => vec![
                Force::any()
                    .moves(&[moves::SPECTRAL_THIEF, moves::SHELL_SMASH, moves::COPYCAT, moves::SUBSTITUTE])
                    .outside_pool(),
                Force::species(with_ability(abilities::CONTRARY))
                    .ability(abilities::CONTRARY)
                    .moves(&[moves::SPECTRAL_THIEF, moves::CLOSE_COMBAT, moves::COPYCAT, moves::SUBSTITUTE])
                    .outside_pool(),
                Force::species(with_ability(abilities::SIMPLE)).ability(abilities::SIMPLE).moves(&[
                    moves::SHELL_SMASH,
                    moves::CALM_MIND,
                    moves::PROTECT,
                    moves::STORED_POWER,
                ]),
                Force::species(with_ability(abilities::OPPORTUNIST))
                    .ability(abilities::OPPORTUNIST)
                    .moves(&[moves::SPECTRAL_THIEF, moves::CALM_MIND, moves::PROTECT, moves::COPYCAT])
                    .outside_pool(),
            ],
            Target::BounceCharge if doubles => vec![
                Force::species(with_ability(abilities::MAGIC_BOUNCE)).ability(abilities::MAGIC_BOUNCE).moves(&[
                    moves::CHARGE,
                    moves::GROWL,
                    moves::COPYCAT,
                    moves::THUNDERBOLT,
                ]),
                // Electrify is Past but executable in the pinned Gen 9
                // simulator; admit it explicitly rather than filtering it.
                Force::any().moves(&[moves::ELECTRIFY, moves::CHARGE, moves::GROWL, moves::THUNDERBOLT]).outside_pool(),
                Force::any().moves(&[moves::CHARGE, moves::GROWL, moves::COPYCAT, moves::THUNDERBOLT]),
            ],
            Target::BounceCharge => Vec::new(),
            Target::Charge => vec![
                Force::any()
                    .moves(&[moves::CHARGE, moves::TERA_BLAST, moves::TERRAIN_PULSE, moves::ELECTRIC_TERRAIN])
                    .tera(Type::Electric),
                Force::species(with_ability(abilities::GALVANIZE)).ability(abilities::GALVANIZE).moves(&[
                    moves::CHARGE,
                    moves::METRONOME,
                    moves::COPYCAT,
                    moves::BODY_SLAM,
                ]),
                Force::species(with_ability(abilities::GALVANIZE)).ability(abilities::GALVANIZE).moves(&[
                    moves::CHARGE,
                    moves::REST,
                    moves::SLEEP_TALK,
                    moves::HYPER_VOICE,
                ]),
                Force::species(known(&[species::ORICORIO_POM_POM])).moves(&[moves::CHARGE, moves::REVELATION_DANCE]),
                Force::species(known(&[species::MORPEKO])).ability(abilities::HUNGER_SWITCH).moves(&[
                    moves::CHARGE,
                    moves::AURA_WHEEL,
                    moves::THROAT_CHOP,
                ]),
                Force::species(with_ability(abilities::LIQUID_VOICE)).ability(abilities::LIQUID_VOICE).moves(&[
                    moves::CHARGE,
                    moves::OVERDRIVE,
                    moves::COPYCAT,
                    moves::HYPER_VOICE,
                ]),
            ],
            Target::CalledAria if doubles => vec![
                Force::species(with_ability(abilities::SHEER_FORCE)).ability(abilities::SHEER_FORCE).moves(&[
                    moves::COPYCAT,
                    moves::METRONOME,
                    moves::WILL_O_WISP,
                ]),
                Force::species(with_ability(abilities::SHEER_FORCE))
                    .ability(abilities::SHEER_FORCE)
                    .moves(&[moves::COPYCAT, moves::WILL_O_WISP]),
                Force::any().moves(&[moves::SPARKLING_ARIA, moves::WILL_O_WISP]),
                Force::any().moves(&[moves::SPARKLING_ARIA, moves::WILL_O_WISP]),
                Force::any().moves(&[moves::WILL_O_WISP, moves::SPARKLING_ARIA, moves::PROTECT]),
                Force::species(with_ability(abilities::MAGICIAN)).ability(abilities::MAGICIAN).moves(&[
                    moves::SPARKLING_ARIA,
                    moves::WILL_O_WISP,
                    moves::FLING,
                ]),
                Force::any().moves(&[moves::WILL_O_WISP, moves::COPYCAT]),
            ],
            Target::CalledAria => Vec::new(),
            Target::ProteanCharge => vec![
                Force::species(with_ability(abilities::PROTEAN)).ability(abilities::PROTEAN).moves(&[
                    moves::METEOR_BEAM,
                    moves::SOLAR_BEAM,
                    moves::PHANTOM_FORCE,
                    moves::SKY_ATTACK,
                ]),
                Force::any().moves(&[moves::ELECTRIFY, moves::PROTECT, moves::SUNNY_DAY, moves::RAIN_DANCE]),
                Force::species(with_ability(abilities::LIBERO))
                    .ability(abilities::LIBERO)
                    .moves(&[moves::ELECTRO_SHOT, moves::FLY, moves::DIG, moves::COPYCAT])
                    .item_from(&[items::POWER_HERB]),
                Force::any().moves(&[moves::ELECTRIFY, moves::COPYCAT, moves::DIVE, moves::FREEZE_SHOCK]),
                Force::species(with_ability(abilities::PROTEAN)).ability(abilities::PROTEAN).moves(&[
                    moves::ICE_BURN,
                    moves::BOUNCE,
                    moves::SOLAR_BLADE,
                    moves::METEOR_BEAM,
                ]),
            ],
            Target::RevivalRageFist => vec![
                Force::any().moves(&[moves::RAGE_FIST, moves::REVIVAL_BLESSING]),
                Force::any().moves(&[moves::REVIVAL_BLESSING, moves::METAL_BURST, moves::AVALANCHE]),
                Force::any().moves(&[moves::RAGE_FIST, moves::COMEUPPANCE, moves::FUTURE_SIGHT]),
                Force::any().moves(&[moves::RAGE_FIST, moves::REVIVAL_BLESSING]),
            ],
        };
        forces
    }
}

/// Species whose ability changes their forme (their first ability).
const FORME_SPECIES: &[SpeciesId] = &[
    species::MIMIKYU,
    species::EISCUE,
    species::MORPEKO,
    species::MINIOR,
    species::PALAFIN,
    species::AEGISLASH,
    species::WISHIWASHI,
    species::CRAMORANT,
];

/// Items a Symbiosis holder passes.
const SYMBIOSIS_ITEMS: &[ItemId] = &[
    items::LIFE_ORB,
    items::SITRUS_BERRY,
    items::WEAKNESS_POLICY,
    items::FOCUS_SASH,
    items::CHOICE_SCARF,
    items::EJECT_BUTTON,
    items::BOOSTER_ENERGY,
];

/// Items used up mid-turn.
const CONSUMABLES: &[ItemId] = &[
    items::SITRUS_BERRY,
    items::NORMAL_GEM,
    items::WHITE_HERB,
    items::EJECT_BUTTON,
    items::RED_CARD,
    items::WEAKNESS_POLICY,
    items::FOCUS_SASH,
    items::AIR_BALLOON,
    items::MENTAL_HERB,
    items::POWER_HERB,
    items::ELECTRIC_SEED,
];

const PLEDGES: &[MoveId] = &[moves::FIRE_PLEDGE, moves::GRASS_PLEDGE, moves::WATER_PLEDGE];

/// Items that do something when flung.
const FLING_ITEMS: &[ItemId] = &[
    items::KINGS_ROCK,
    items::RAZOR_FANG,
    items::FLAME_ORB,
    items::TOXIC_ORB,
    items::LIGHT_BALL,
    items::POISON_BARB,
    items::WHITE_HERB,
    items::MENTAL_HERB,
    items::SITRUS_BERRY,
    items::LUM_BERRY,
    items::LIECHI_BERRY,
    items::IRON_BALL,
];

/// Items that lock or limit the move choice.
const LOCK_ITEMS: &[ItemId] = &[items::CHOICE_BAND, items::CHOICE_SPECS, items::CHOICE_SCARF, items::ASSAULT_VEST];

/// Moves that can't be used twice in a row, a sound move and one Gravity
/// stops.
const LOCK_MOVES: &[MoveId] = &[moves::GIGATON_HAMMER, moves::BLOOD_MOON, moves::HYPER_VOICE, moves::HIGH_JUMP_KICK];

/// Moves whose second Parental Bond hit is special.
const BOND_MOVES: &[MoveId] = &[moves::SEISMIC_TOSS, moves::SUPER_FANG, moves::KNOCK_OFF, moves::BODY_PRESS];
const BOND_MOVES_2: &[MoveId] = &[moves::DOUBLE_SHOCK, moves::POWER_UP_PUNCH, moves::CLOSE_COMBAT, moves::FAKE_OUT];

/// Items that react to a hit.
const BOND_ITEMS: &[ItemId] = &[
    items::FOCUS_SASH,
    items::SITRUS_BERRY,
    items::WEAKNESS_POLICY,
    items::ROCKY_HELMET,
    items::AIR_BALLOON,
    items::RED_CARD,
];

/// Multi-hit moves (fixed and variable counts, smart-targeting).
const MULTI_HITS: &[MoveId] = &[moves::POPULATION_BOMB, moves::BULLET_SEED, moves::TRIPLE_AXEL, moves::DRAGON_DARTS];

/// Pinch berries and Quick Claw.
const PINCH_ITEMS: &[ItemId] = &[
    items::CUSTAP_BERRY,
    items::QUICK_CLAW,
    items::MICLE_BERRY,
    items::JABOCA_BERRY,
    items::ROWAP_BERRY,
    items::KEE_BERRY,
    items::MARANGA_BERRY,
    items::ENIGMA_BERRY,
    items::STARF_BERRY,
    items::LANSAT_BERRY,
    items::LEPPA_BERRY,
];

/// Dance moves.
const DANCES: &[MoveId] = &[moves::QUIVER_DANCE, moves::FEATHER_DANCE, moves::TEETER_DANCE, moves::PETAL_DANCE];
const DANCES_2: &[MoveId] = &[moves::FIERY_DANCE, moves::REVELATION_DANCE, moves::VICTORY_DANCE, moves::AQUA_STEP];

/// Moves Instruct repeats with something to remember.
const INSTRUCTED: &[MoveId] = &[moves::GIGATON_HAMMER, moves::FUTURE_SIGHT, moves::BEAT_UP, moves::ROLLOUT];
const INSTRUCTED_2: &[MoveId] = &[moves::HELPING_HAND, moves::ECHOED_VOICE, moves::ACUPRESSURE, moves::DECORATE];

/// Weather moves and those weather changes.
const WEATHER_USERS: &[MoveId] = &[moves::WEATHER_BALL, moves::SOLAR_BEAM, moves::ELECTRO_SHOT, moves::HYDRO_STEAM];

/// Terrain moves and those terrain changes.
const TERRAIN_USERS: &[MoveId] =
    &[moves::TERRAIN_PULSE, moves::ELECTRIC_TERRAIN, moves::PSYCHIC_TERRAIN, moves::EXPANDING_FORCE];

const SEEDS: &[ItemId] = &[items::ELECTRIC_SEED, items::GRASSY_SEED, items::MISTY_SEED, items::PSYCHIC_SEED];

/// Items that change hands.
const SWAP_ITEMS: &[ItemId] = &[
    items::CHOICE_SCARF,
    items::BOOSTER_ENERGY,
    items::LEFTOVERS,
    items::BLACK_SLUDGE,
    items::AIR_BALLOON,
    items::SITRUS_BERRY,
    items::EJECT_BUTTON,
    items::IRON_BALL,
];

/// Priority moves (Encore and Quash reorder them).
const PRIORITY: &[MoveId] = &[moves::FAKE_OUT, moves::SUCKER_PUNCH, moves::GRASSY_GLIDE, moves::EXTREME_SPEED];

/// Fire and Water moves, damaging and not, for the primal weathers.
const PRIMAL_MOVES: &[MoveId] = &[moves::FLAMETHROWER, moves::HYDRO_PUMP, moves::WEATHER_BALL, moves::WILL_O_WISP];

const TRICK_ITEMS: &[ItemId] = &[
    items::CHOICE_SCARF,
    items::CHOICE_SPECS,
    items::FLAME_ORB,
    items::TOXIC_ORB,
    items::LEFTOVERS,
    items::IRON_BALL,
    items::LAGGING_TAIL,
    items::STICKY_BARB,
];

/// The berries with an `onUpdate` (the status cures, the pinch berries,
/// the HP berries and Leppa).
const UPDATE_BERRIES: &[ItemId] = &[
    items::AGUAV_BERRY,
    items::APICOT_BERRY,
    items::ASPEAR_BERRY,
    items::CHERI_BERRY,
    items::CHESTO_BERRY,
    items::FIGY_BERRY,
    items::GANLON_BERRY,
    items::IAPAPA_BERRY,
    items::LANSAT_BERRY,
    items::LEPPA_BERRY,
    items::LIECHI_BERRY,
    items::LUM_BERRY,
    items::MAGO_BERRY,
    items::ORAN_BERRY,
    items::PECHA_BERRY,
    items::PERSIM_BERRY,
    items::PETAYA_BERRY,
    items::RAWST_BERRY,
    items::SALAC_BERRY,
    items::SITRUS_BERRY,
    items::STARF_BERRY,
    items::WIKI_BERRY,
];

const PIVOTS: &[MoveId] = &[moves::U_TURN, moves::VOLT_SWITCH, moves::FLIP_TURN];

/// Trapping moves (those in the format's pool are forced).
const TRAPS: &[MoveId] = &[moves::MEAN_LOOK, moves::SPIRIT_SHACKLE, moves::FIRE_SPIN];
/// Reg M-C's new Mega Stones.
const NEW_STONES: &[ItemId] = &[
    items::ABSOLITE_Z,
    items::BAXCALIBRITE,
    items::GARCHOMPITE_Z,
    items::GOLISOPITE,
    items::LUCARIONITE_Z,
    items::SALAMENCITE,
];

/// Arceus and its formes (a forme gets its own Plate, `requiredItems`).
const ARCEUS_FORMES: &[SpeciesId] = &[
    species::ARCEUS,
    species::ARCEUS_BUG,
    species::ARCEUS_DARK,
    species::ARCEUS_DRAGON,
    species::ARCEUS_ELECTRIC,
    species::ARCEUS_FAIRY,
    species::ARCEUS_FIGHTING,
    species::ARCEUS_FIRE,
    species::ARCEUS_FLYING,
    species::ARCEUS_GHOST,
    species::ARCEUS_GRASS,
    species::ARCEUS_GROUND,
    species::ARCEUS_ICE,
    species::ARCEUS_POISON,
    species::ARCEUS_PSYCHIC,
    species::ARCEUS_ROCK,
    species::ARCEUS_STEEL,
    species::ARCEUS_WATER,
];

const PLATES: &[ItemId] = &[
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
];

/// Contact moves for Aura Guard to halve.
const CONTACT: &[MoveId] = &[moves::CLOSE_COMBAT, moves::KNOCK_OFF, moves::BODY_SLAM, moves::DRAIN_PUNCH];

/// Growth and the moves Mega Sol changes.
const MEGA_SOL_MOVES: &[MoveId] = &[moves::GROWTH, moves::SOLAR_BEAM, moves::SYNTHESIS, moves::WEATHER_BALL];

const WEATHERS: &[MoveId] = &[moves::RAIN_DANCE, moves::SNOWSCAPE, moves::SANDSTORM, moves::SUNNY_DAY];

/// One forced set.
#[derive(Clone, Debug, Default)]
struct Force {
    /// Candidates (empty: keep the set's species).
    species: Option<Vec<SpeciesId>>,
    ability: Option<AbilityId>,
    moves: &'static [MoveId],
    items: &'static [ItemId],
    mega: bool,
    /// Force the moves even if they aren't in the format's pool.
    outside_pool: bool,
    /// With `mega`: only these stones (empty: any that works).
    mega_items: &'static [ItemId],
    /// The Tera type (unless the species requires one).
    tera: Option<Type>,
}

impl Force {
    fn any() -> Force {
        Force::default()
    }

    fn species(list: Vec<SpeciesId>) -> Force {
        Force { species: Some(list), ..Force::default() }
    }

    fn ability(self, a: AbilityId) -> Force {
        Force { ability: Some(a), ..self }
    }

    fn moves(self, m: &'static [MoveId]) -> Force {
        Force { moves: m, ..self }
    }

    fn item_from(self, items: &'static [ItemId]) -> Force {
        Force { items, ..self }
    }

    fn mega(self) -> Force {
        Force { mega: true, ..self }
    }

    fn outside_pool(self) -> Force {
        Force { outside_pool: true, ..self }
    }

    fn mega_from(self, stones: &'static [ItemId]) -> Force {
        Force { mega: true, mega_items: stones, ..self }
    }

    fn tera(self, ty: Type) -> Force {
        Force { tera: Some(ty), ..self }
    }

    /// The species candidates that fit `mega_from` (all of them otherwise).
    fn candidates(&self, format: &Format, list: &[SpeciesId]) -> Vec<SpeciesId> {
        list.iter()
            .copied()
            .filter(|&s| {
                // `mega_from`: a species one of the stones works for.
                if self.mega && !self.mega_items.is_empty() {
                    let stones = mega_stones(format, s);
                    return self.mega_items.iter().any(|i| stones.contains(i));
                }
                true
            })
            .collect()
    }

    /// The forced moves pikapro plays in `format` (all of them with
    /// `outside_pool`).
    fn forced_moves(&self, move_pool: &[MoveId]) -> Vec<MoveId> {
        self.moves.iter().copied().filter(|m| self.outside_pool || move_pool.contains(m)).collect()
    }

    /// The forced items the format's dex has as standard.
    fn forced_items(&self, format: &Format) -> Vec<ItemId> {
        self.items.iter().copied().filter(|&i| format.dex.item(i).nonstandard.is_none()).collect()
    }

    /// Whether every part of this set can be forced in `format` (ignoring
    /// the team it lands in).
    fn possible(&self, format: &Format, move_pool: &[MoveId]) -> bool {
        let species: Vec<SpeciesId> = match &self.species {
            Some(list) => self.candidates(format, list),
            None => Vec::new(),
        };
        (self.species.is_none() || !species.is_empty())
            && (self.moves.is_empty() || !self.forced_moves(move_pool).is_empty())
            && (self.items.is_empty() || !self.forced_items(format).is_empty())
            && (!self.mega || species.iter().any(|&s| !mega_stones(format, s).is_empty()))
    }

    /// Forces this set at `at`. Returns whether all of it landed: its
    /// species, ability, at least one of its moves, and its item or Mega
    /// Stone.
    fn apply(&self, format: &Format, move_pool: &[MoveId], rng: &mut BattleRng, team: &mut Team, at: usize) -> bool {
        let dex = format.dex;
        let old = team.sets[at].clone();
        let species = match &self.species {
            Some(list) => {
                let fresh: Vec<SpeciesId> = self
                    .candidates(format, list)
                    .into_iter()
                    .filter(|&s| {
                        let base = dex.species(s).base_species;
                        !team
                            .sets
                            .iter()
                            .enumerate()
                            .any(|(i, set)| i != at && dex.species(set.species).base_species == base)
                    })
                    .collect();
                if fresh.is_empty() {
                    return false;
                }
                fresh[rng.random(fresh.len() as u32) as usize]
            }
            None => old.species,
        };
        let mut set = PokemonSet::new(species)
            .with_level(old.level)
            .with_nature(old.nature)
            .with_investment(old.investment)
            .with_ability(self.ability.unwrap_or(if species == old.species {
                old.ability
            } else {
                first_ability(format, species)
            }));
        set.gender = if species == old.species { old.gender } else { None };
        set.tera_type = match dex.species(species).required_tera_type {
            Some(t) => Some(t.ty()),
            None => self.tera.or(old.tera_type),
        };
        // The forced moves (those pikapro implements), then the set's own.
        let mut ms = self.forced_moves(move_pool);
        if self.moves == PIVOTS && !ms.is_empty() {
            ms = vec![ms[rng.random(ms.len() as u32) as usize]];
        }
        let moves_landed = self.moves.is_empty() || !ms.is_empty();
        for &m in old.moves.iter() {
            if ms.len() < 4 && !ms.contains(&m) {
                ms.push(m);
            }
        }
        set = set.with_moves(&ms);
        let required = dex.species(species).required_items.first().copied();
        if let Some(item) = required {
            // Item Clause: the set that needs it gets it.
            for (i, other) in team.sets.iter_mut().enumerate() {
                if i != at && other.item == Some(item) {
                    other.item = None;
                }
            }
        }
        let held: Vec<ItemId> =
            team.sets.iter().enumerate().filter(|&(i, _)| i != at).filter_map(|(_, s)| s.item).collect();
        let free = |i: &ItemId| !held.contains(i);
        set.item = if required.is_some() {
            required
        } else if self.mega {
            let stones: Vec<ItemId> = mega_stones(format, species)
                .into_iter()
                .filter(free)
                .filter(|i| self.mega_items.is_empty() || self.mega_items.contains(i))
                .collect();
            stones.first().copied()
        } else if !self.items.is_empty() {
            // (Only items the format has: Eject Pack is Past in Champions.)
            let choices: Vec<ItemId> = self.forced_items(format).into_iter().filter(free).collect();
            choices.get(rng.random(choices.len().max(1) as u32) as usize).copied()
        } else if species == old.species {
            old.item
        } else {
            None
        };
        let item_landed = if self.mega {
            set.item.is_some_and(|i| dex.item(i).mega.iter().any(|&(base, _)| base == species))
        } else {
            self.items.is_empty() || set.item.is_some_and(|i| self.items.contains(&i))
        };
        team.sets[at] = set;
        moves_landed && item_landed
    }
}

/// The first ability of `species` pikapro implements, or No Ability.
fn first_ability(format: &Format, species: SpeciesId) -> AbilityId {
    let slots = format.dex.species(species).abilities;
    [Some(slots.primary), slots.secondary, slots.hidden]
        .into_iter()
        .flatten()
        .find(|a| crate::IMPLEMENTED_ABILITIES.contains(a))
        .unwrap_or(abilities::NO_ABILITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep every Gen 9 target's dropped move visible in a reviewed list.
    /// A target can report that it landed while only one listed move did;
    /// that must never silently hide a new unsupported standard mechanic.
    #[test]
    fn gen9_targets_only_filter_known_nonstandard_moves() {
        let expected = [
            moves::HEAL_BLOCK,
            moves::OCTOLOCK,
            moves::MAGIC_COAT,
            moves::GRASS_WHISTLE,
            moves::CORROSIVE_GAS,
            moves::BURN_UP,
            moves::EMBARGO,
            moves::BESTOW,
            moves::ANCHOR_SHOT,
            moves::THOUSAND_WAVES,
            moves::SNAP_TRAP,
            moves::TELEKINESIS,
            moves::LASER_FOCUS,
            moves::GRUDGE,
            moves::POWDER,
            moves::NIGHTMARE,
            moves::ELECTRIFY,
            moves::SPOTLIGHT,
        ];
        let mut seen = Vec::new();
        for format in [Format::gen9_ou(), Format::gen9_random_battle(), Format::gen9_random_doubles_battle()] {
            let pool = crate::move_pool(&format);
            for target in Target::ALL.into_iter().chain(Target::EXTRA) {
                for force in target.forces(&format) {
                    for &m in force.moves {
                        if force.outside_pool || pool.contains(&m) {
                            continue;
                        }
                        let data = format.dex.move_(m);
                        assert!(data.nonstandard.is_some(), "{} silently drops standard {}", target.name(), data.name);
                        assert!(expected.contains(&m), "{} drops unaudited {}", target.name(), data.name);
                        if !seen.contains(&m) {
                            seen.push(m);
                        }
                    }
                }
            }
        }
        let unused: Vec<_> =
            expected.iter().filter(|m| !seen.contains(m)).map(|m| pikapro_data::gen9::DEX.move_(*m).name).collect();
        assert!(unused.is_empty(), "update the audited exclusions when targets change: {unused:?}");
    }

    /// `conformance --target` refuses a target the format can't express,
    /// and doesn't record a battle it didn't land in.
    #[test]
    fn targets_apply_where_the_format_expresses_them() {
        let formats = [Format::gen9_singles(), Format::gen9_doubles(), Format::gen9_vgc(), Format::champions_vgc()];
        for t in Target::ALL.into_iter().chain(Target::EXTRA) {
            let row: Vec<bool> = formats.iter().map(|f| t.applies(f)).collect();
            eprintln!("{:>16} {row:?}", t.name());
        }
        let champions = Format::champions_vgc();
        // Champions has no Terastallization, no Ogerpon, Terapagos,
        // Tatsugiri, Dondozo or Arceus, and
        // Metronome and Eject Pack are Past (MetronomePast forces it).
        // Nor any ability outside the species' (the primal weathers,
        // Parental Bond), the Pledges, Mirror Herb, Neutralizing Gas or
        // Dancer, nor a species with Shadow Tag, Arena Trap or Magnet Pull
        // (trapping's defining set), or the status orbs PsychoShift
        // forces. And no Endless Battle Clause.
        let past = [
            Target::Endless,
            Target::SleepClause,
            Target::Tera,
            Target::Ogerpon,
            Target::Commander,
            Target::Metronome,
            Target::EjectPack,
            Target::Arceus,
            Target::CommanderRevival,
            Target::CommanderNoGuard,
            Target::PrimalWeather,
            Target::Pledges,
            Target::Opportunist,
            Target::NeutralizingGas,
            Target::ParentalBond,
            Target::Dancer,
            Target::DancerCopycat,
            Target::Trapping,
            Target::PsychoShift,
        ];
        for t in Target::ALL.into_iter().chain(Target::EXTRA) {
            assert_eq!(t.applies(&champions), !past.contains(&t), "{} in championsvgc", t.name());
        }
        for f in &formats[..3] {
            for t in
                [Target::Leek, Target::Mega, Target::NewMegas, Target::MegaRedirect, Target::MegaSol, Target::AuraGuard]
            {
                assert!(!t.applies(f), "{} in {}", t.name(), f.id);
            }
            assert!(Target::EjectPack.applies(f), "ejectpack in {}", f.id);
            assert!(Target::Arceus.applies(f), "arceus in {}", f.id);
            assert!(Target::FutureSight.applies(f), "futuresight in {}", f.id);
        }
        // The Endless Battle Clause's target: only where the clause is.
        for f in Format::presets() {
            assert_eq!(Target::Endless.applies(f), f.endless_battle_clause, "endless in {}", f.id);
        }
        assert!(Target::Endless.applies(&Format::gen9_ou()));
        // Sleep Clause Mod's: only where the rule is.
        for f in Format::presets() {
            assert_eq!(Target::SleepClause.applies(f), f.sleep_clause_mod, "sleepclause in {}", f.id);
        }
        assert!(!Target::Commander.applies(&Format::gen9_singles()));
        assert!(!Target::CommanderNoGuard.applies(&Format::gen9_singles()));
        assert!(Target::CommanderNoGuard.applies(&Format::gen9_doubles()));
        for f in &formats[..3] {
            assert!(Target::PrimalWeather.applies(f), "primalweather in {}", f.id);
        }
        assert!(!Target::ChargeRedirect.applies(&Format::gen9_singles()));
        assert!(Target::ChargeRedirect.applies(&Format::gen9_doubles()));
    }

    /// `apply` says whether the defining set landed.
    #[test]
    fn apply_reports_whether_the_target_landed() {
        let format = Format::champions_vgc();
        let landed = (0..20)
            .filter(|&seed| {
                let mut rng = BattleRng::new(pikapro::Seed::fast(seed));
                let mut teams = [crate::random_team(&format, &mut rng), crate::random_team(&format, &mut rng)];
                let landed = Target::Leek.apply(&format, &mut rng, &mut teams);
                let has = teams.iter().any(|t| t.sets.iter().any(|s| s.item == Some(items::LEEK)));
                assert_eq!(landed, has, "seed {seed}");
                landed
            })
            .count();
        assert!(landed >= 15, "{landed} of 20");
    }
}
