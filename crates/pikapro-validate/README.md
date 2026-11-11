# pikapro-validate

A port of Pokémon Showdown's team validator (`sim/team-validator.ts`) for
[pikapro-engine](https://github.com/spedr/pikapro-engine), a Pokémon battle
simulator faithful to Pokémon Showdown. It answers "would Showdown accept
this team in this format?", and reports each problem in Showdown's own
words. It is checked by differential fuzzing against Showdown's
validator.

| pikapro preset | validated as |
|---|---|
| `championsvgc` | `gen9championsvgc2026regmc` ([Gen 9 Champions] VGC 2026 Reg M-C) |
| `championsbss` | `gen9championsbssregmc` ([Gen 9 Champions] BSS Reg M-C) |
| `gen9vgc` | `gen9vgc2025regi` ([Gen 9] VGC 2025 Reg I) |
| `gen9ou` | `gen9ou` ([Gen 9] OU: the Uber and AG tiers and the format's bans; OHKO, Evasion and Sleep Moves Clauses) |
| `gen9singles`, `gen9doubles` | `gen9anythinggoes@@@Species Clause` (Smogon's legality floor, no tier bans) |
| `gen9randombattle` | `gen9anythinggoes@@@Species Clause` (supplied teams only) |
| `gen9randomdoublesbattle` | `gen9anythinggoes@@@Species Clause` (supplied teams only) |

The random-format mapping validates supplied teams. Generated random-battle
teams (Showdown's generator, `pikapro-teams`' `Mode::Sets`) bypass
validation, as in Showdown, and may fail this validator while remaining
playable by the simulator.

```rust
use pikapro::{Investment, PokemonSet, Team};
use pikapro_data::gen9::{abilities, moves, species};
use pikapro_data::{Nature, Stats};
use pikapro_validate::{ValidationFormat, Validator};

let validator = Validator::new(ValidationFormat::gen9_vgc_2025_reg_i());
let set = PokemonSet::new(species::GARCHOMP)
    .with_level(50)
    .with_ability(abilities::ROUGH_SKIN)
    .with_nature(Nature::Jolly)
    .with_investment(Investment::Evs { evs: Stats::new(4, 252, 0, 0, 0, 252), ivs: Stats::splat(31) })
    .with_moves(&[moves::EARTHQUAKE, moves::SPORE]);
let problems = validator.validate(&Team::new(vec![set])).unwrap_err();
let messages = validator.messages(&problems);
assert_eq!(messages[0], "You must bring at least 4 Pokémon (your team has 1).");
assert_eq!(messages[1], "Garchomp can't learn Spore.");
```

Showdown's validator also changes the sets it checks, and a Showdown
server plays the changed team. `Validator::check` returns that team, as
`Checked::team` (and `validate_preset` as its `Ok`). For example, a set
that lists a battle-only forme (`Floette-Mega @ Floettite`) starts as
the forme it changes from (Floette-Eternal with Flower Veil), which Mega
Evolves in battle. Give the engine this team to play as
Showdown does.

Part of pikapro: see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md) and
[conformance](https://github.com/spedr/pikapro-engine/blob/main/docs/CONFORMANCE.md#team-validation)
(how it is fuzzed against Showdown).

MIT licensed. A port of [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s team validator (MIT);
the notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
