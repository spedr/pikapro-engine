# pikapro-server

Pokémon Showdown's battle stream on
[pikapro-engine](https://github.com/spedr/pikapro-engine), a Pokémon battle
simulator faithful to Pokémon Showdown.

- `BattleStream` speaks the text protocol of Showdown's `BattleStream`
  (`sim/battle-stream.ts`). You write `>start`, `>player` and choice
  lines and read back the `update`, `sideupdate` and `end` messages
  Showdown would send for the same input. `PlayerStreams` splits it into
  Showdown's omniscient, spectator and player channels.
- The `pikapro-simulate` binary is a drop-in for
  `pokemon-showdown simulate-battle` over stdin and stdout.

Seeds are Showdown's, including its default `sodium` generator, so a
seeded battle plays out as it does on Showdown. In `gen9randombattle`
and `gen9randomdoublesbattle` a player without a team gets Showdown's
generated team for its seed (`>player p1 {"name":"Alice","seed":"sodium,…"}`),
so a Showdown input log replays its battle. The stream is diffed
against Showdown's own, message for message.

```rust
use pikapro_server::{BattleStream, Message, StreamOptions};

let mut stream = BattleStream::new(StreamOptions::default());
let team = "Pikachu||lightball|static|thunderbolt,quickattack|Timid|,,,252,,252||||50|";
stream.write(r#">start {"formatid":"gen9customgame@@@!Team Preview","seed":"sodium,0123"}"#).unwrap();
stream.write(&format!(r#">player p1 {{"name":"Alice","team":"{team}"}}"#)).unwrap();
stream.write(&format!(r#">player p2 {{"name":"Bob","team":"{team}"}}"#)).unwrap();
stream.write(">p1 move thunderbolt\n>p2 move 2").unwrap();
for message in stream.drain() {
    if let Message::SideUpdate { data, .. } = &message {
        assert!(data.starts_with("|request|"));
    }
    println!("{message}\n");
}
```

```sh
cargo install pikapro-server
pikapro-simulate < input.txt        # as `pokemon-showdown simulate-battle`
```

Part of pikapro: see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md) and
[conformance](https://github.com/spedr/pikapro-engine/blob/main/docs/CONFORMANCE.md#battle-stream).
For a websocket server that Showdown clients and poke-env connect to,
see `pikapro-showdown-server` in the repository.

MIT licensed. A port of [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s battle stream (MIT);
the notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
