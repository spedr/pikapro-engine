# pikapro-teams

Random Pokémon teams for
training and evaluating agents on
[pikapro-engine](https://github.com/spedr/pikapro-engine), a Pokémon battle
simulator faithful to Pokémon Showdown. Each team is drawn
deterministically from a seed, in one of two modes:

- `Mode::Uniform`: every legal choice uniformly (species and forme,
  ability, item, learnset moves, nature, EVs or Stat Points, IVs, Tera
  type, gender);
- `Mode::Sets`: sets from Showdown's own random-battle data, with a
  spread chosen by the set's role. In `gen9randombattle` and
  `gen9randomdoublesbattle` it is Showdown's random-battle team generator
  ported exactly: the same seed gives Showdown's team
  (`pikapro_teams::random_battle`).

Every other set is checked with
[`pikapro-validate`](https://crates.io/crates/pikapro-validate) and
drawn again if it fails; Species Clause, Item Clause and the
restricted-legendary limit hold while drawing. Showdown validates no
generated random-battle team, and neither does pikapro-teams.

```rust
use pikapro::Format;
use pikapro_teams::{Mode, TeamGenerator};

let generator = TeamGenerator::new(&Format::champions_vgc(), Mode::Sets).unwrap();
let team = generator.team(2_300_000).unwrap();
assert_eq!(team.sets.len(), 6);
assert!(generator.validator().validate(&team).is_ok());
assert_eq!(team, generator.team(2_300_000).unwrap()); // the same seed, the same team
```

Part of pikapro: see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md).

MIT licensed. The random-battle sets and generator come from [Pokémon Showdown](https://github.com/smogon/pokemon-showdown) (MIT);
the notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
