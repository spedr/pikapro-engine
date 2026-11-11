# pikapro

A deterministic Pokémon battle simulator in Rust, faithful to
[Pokémon Showdown](https://github.com/smogon/pokemon-showdown): Gen 9
singles and doubles, VGC (Regulation I), and Pokémon Champions VGC and
Battle Spot Singles (Regulation M-C).

This crate is the simulator: battle state, choices, legal actions, turn
resolution, the RNG and the format presets. It has no dependencies
besides `pikapro-data`, formats no text and does no I/O. Showdown's
text protocol (logs, choice strings, team export) is in
[`pikapro-protocol`](https://crates.io/crates/pikapro-protocol).

```rust
use pikapro::data::gen9::{abilities as a, items as i, moves as m, species as s};
use pikapro::data::{Nature, Stats};
use pikapro::{Battle, BattleEvent, BattleOptions, BattleRng, Format, Investment, PokemonSet};
use pikapro::{Seed, SideId, Team};

// A set per Pokémon: IDs from the dex, a nature, EVs and IVs, up to four moves.
let evs = Investment::Evs { evs: Stats::new(252, 252, 0, 0, 4, 0), ivs: Stats::splat(31) };
let set = |species, ability, item, moves: &[_]| {
    PokemonSet::new(species)
        .with_level(50)
        .with_ability(ability)
        .with_item(item)
        .with_nature(Nature::Adamant)
        .with_investment(evs)
        .with_moves(moves)
};
let team = Team::new(vec![
    set(s::GARCHOMP, a::ROUGH_SKIN, i::LIFE_ORB, &[m::EARTHQUAKE, m::DRAGON_CLAW, m::PROTECT]),
    set(s::INCINEROAR, a::INTIMIDATE, i::SITRUS_BERRY, &[m::FAKE_OUT, m::FLARE_BLITZ]),
    set(s::AMOONGUSS, a::REGENERATOR, i::ROCKY_HELMET, &[m::SPORE, m::RAGE_POWDER]),
]);

let format = Format::gen9_doubles();
let options = BattleOptions::new(Seed::fast(42));
let mut battle = Battle::new(format, [team.clone(), team], options).unwrap();
let mut policy = BattleRng::new(Seed::fast(7)); // the policy's own randomness

while !battle.is_over() {
    for side in SideId::BOTH {
        let legal = battle.legal_actions(side); // per-slot legal actions
        if legal.is_waiting() {
            continue;
        }
        battle.submit(side, legal.sample(&mut policy)).unwrap(); // resolves once both are in
    }
}

// What happened is in the events (pikapro-protocol renders them as Showdown's log).
let faints = battle.events().iter().filter(|e| matches!(e, BattleEvent::Faint { .. })).count();
println!("{:?} after {faints} faints", battle.result());
```

Mechanics are ported from Showdown's source, including when and how
often it draws random numbers. Random battles are checked against a
pinned Showdown line by line and draw by draw.

The same format, teams, seed and choices give the same battle, bit for
bit, on every platform and thread count. `BattleState` is plain `Copy`
data with no heap, and `Battle::fork(seed)` copies a battle with a new
random stream, which is what a search needs.

Part of [pikapro-engine](https://github.com/spedr/pikapro-engine): see the
[main README](https://github.com/spedr/pikapro-engine/blob/main/README.md), the
[architecture](https://github.com/spedr/pikapro-engine/blob/main/docs/ARCHITECTURE.md),
[determinism](https://github.com/spedr/pikapro-engine/blob/main/docs/DETERMINISM.md)
and [conformance](https://github.com/spedr/pikapro-engine/blob/main/docs/CONFORMANCE.md)
docs.

MIT licensed. The engine is a port of [Pokémon Showdown](https://github.com/smogon/pokemon-showdown)'s simulator (MIT);
the notices are in `THIRD_PARTY_NOTICES`.
Pokémon and all related names are trademarks of Nintendo, Creatures Inc.,
Game Freak and The Pokémon Company. pikapro is not affiliated with or endorsed by
them, or by Pokémon Showdown.
