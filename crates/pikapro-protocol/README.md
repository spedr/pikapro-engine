# pikapro-protocol

Pokémon Showdown's text formats for
[pikapro-engine](https://github.com/spedr/pikapro-engine), a Pokémon battle
simulator faithful to Pokémon Showdown. The simulator works in typed
values. This crate turns them into the text that Showdown clients,
poke-env and replays read, and parses that text back:

- logs: battle events as Showdown's protocol lines for any perspective
  (omniscient, a player, spectators), identical to Showdown's channels,
  `|split|` blocks included;
- choices: Showdown's choice strings (`move 1 2 terastallize,
  switch 3`), parsed the way Showdown's `side.choose` parses them;
- requests: Showdown's `|request|` JSON, byte for byte;
- teams: the export and packed formats;
- channels: a parser for a player's channel text, for playing on a
  Showdown server.

```rust
use pikapro::{Battle, BattleOptions, Format, Seed, SideId};
use pikapro_protocol::{Perspective, Renderer, import_team, parse_choice_for};

let format = Format::gen9_singles();
let team = import_team(
    "Garchomp @ Choice Scarf\nAbility: Rough Skin\nEVs: 252 Atk / 4 SpD / 252 Spe\nJolly Nature\n- Earthquake\n- Dragon Claw\n",
    &format,
)
.unwrap();
let mut battle = Battle::new(format, [team.clone(), team], BattleOptions::new(Seed::fast(1))).unwrap();
for side in SideId::BOTH {
    let choice = parse_choice_for(&battle, side, "move dragonclaw", &[]).unwrap();
    battle.submit(side, choice).unwrap();
}

// Player 1's channel, as Showdown sends it to player 1. (This custom game
// shows exact HP to everyone; championsvgc shows the foe's as a percentage.)
let renderer = Renderer::new(battle.format(), Perspective::Player(SideId::P1));
for line in renderer.log(battle.events()) {
    println!("{line}");
}
```

Part of pikapro: see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md) and
[conformance](https://github.com/spedr/pikapro-engine/blob/main/docs/CONFORMANCE.md)
(how the output is checked against Showdown's).

MIT licensed. The text formats follow [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s (MIT);
the notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
