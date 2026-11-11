# pikapro-view

A player's view of a [pikapro-engine](https://github.com/spedr/pikapro-engine)
battle, meaning its information set: what a Pokémon Showdown client
knows. The view is built from the battle's typed events and the player's
own requests, never from the omniscient battle state, so it can't leak
what the player doesn't see. The foe's HP is the percentage Showdown
shows, a foe's item, ability and moves stay unknown until revealed, and
Illusion fools the view as it fools a client.

- `PublicTracker`: what a spectator knows, updated event by event;
- `OwnOverlay`: what a player's request adds for its own side;
- `OwnTeam`: what the player knows of the team it built;
- `PlayerView`: the three together, which encoders, policies and search
  read.

The view is fuzzed. Every claim it makes is checked against the battle's
state, it is tested for leaks of hidden information, and it is compared
with poke-env's battle tracker fed the same logs. It is not RL code:
bots, search and servers can use it too.

```rust
use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Format, Seed, SideId};
use pikapro_view::{OwnOverlay, PlayerView, PublicTracker};

let format = Format::gen9_vgc();
let teams = [sample_team(&format), sample_team(&format)];
let mut tracker = PublicTracker::for_format(&format);
let mut battle = Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap();
let mut policy = BattleRng::new(Seed::fast(2));
for _ in 0..4 {
    for side in SideId::BOTH {
        let legal = battle.legal_actions(side);
        if !legal.is_waiting() {
            battle.submit(side, legal.sample(&mut policy)).unwrap();
        }
    }
    tracker.apply_all(battle.drain_events().as_slice());
}

let own = OwnOverlay::from_battle(&battle, SideId::P1);
let view = PlayerView::new(SideId::P1, &tracker, &own);
for slot in 0..2 {
    if let Some((row, foe)) = view.foe_side().active(slot) {
        let name = battle.format().dex.species(foe.species).name;
        println!("foe row {row}: {name}, {}% HP", foe.hp.hp); // a percentage: the foe's exact HP is hidden
    }
}
```

Part of pikapro: see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md) and
the [player view's checks](https://github.com/spedr/pikapro-engine/blob/main/docs/CONFORMANCE.md#player-view).

MIT licensed. Built on pikapro, a port of [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s
simulator (MIT); the notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
