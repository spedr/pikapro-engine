# pikapro-cli

`pikapro`, the command-line tool of
[pikapro-engine](https://github.com/spedr/pikapro-engine), a Pokémon battle
simulator faithful to Pokémon Showdown: watch random battles as
Showdown logs, check teams with Showdown's validator, and measure
throughput.

```sh
cargo install pikapro-cli

pikapro formats                                    # the format presets
pikapro play --format championsvgc --seed 3        # a random battle, as Showdown's log
pikapro play --perspective p1 --requests           # player 1's channel, with its choices and requests
pikapro validate --format gen9vgc team.txt         # a team in Showdown's export format
pikapro bench --format gen9vgc --battles 100000 --threads 8   # battles per second
```

Both players choose uniformly random legal actions; teams default to a
built-in sample team (`--team1`, `--team2` take Showdown export files).
`validate` applies Showdown's team validator (the custom games as
Anything Goes with Species Clause) and the engine's rules. `bench` plays with the event
log off.

Part of pikapro: see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md).

MIT licensed. Built on pikapro, a port of [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s
simulator (MIT); the notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
