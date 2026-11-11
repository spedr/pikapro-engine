# Determinism

**Contract:** the same `Format`, teams, `Seed` and sequence of submitted
choices produce bit-identical `BattleState`, events and result on every
platform, build profile and thread count.

RL pipelines rely on this for replaying episodes and debugging agents, and
conformance testing relies on it for comparing against Showdown.

## Rules for mechanics code

1. **All randomness comes from `battle.rng`.** No `rand`, no `std` randomness,
   no hashing-based randomness. Policies that sample actions use their *own*
   `BattleRng` (see `LegalActions::sample`), never the battle's.
2. **Integer arithmetic only.** Use `stats::modify` (Showdown's 4096-based
   `modify` with pokeRound) and explicit truncation. `clippy::float_arithmetic`
   is denied in `pikapro` and `pikapro-data`.
3. **No unordered iteration.** Don't use `HashMap`/`HashSet` in state or in any
   loop whose order affects behaviour. Iterate sides P1 then P2 and slots left
   to right unless Showdown does otherwise.
4. **No ambient inputs.** No clock, environment variables, thread-locals or
   global mutable state.
5. **Random draw order is part of the behaviour.** Adding, removing or
   reordering a draw changes every later outcome. Match Showdown's order (see
   CONFORMANCE.md) and treat a changed draw order as a behaviour change in
   review.

## RNG backends

`BattleRng` exposes Showdown's derived operations with Showdown's exact
formulas, over one of three sources:

| seed | source | use |
|---|---|---|
| `Seed::Fast(u64)` | xoshiro256++ (SplitMix64 expansion) | default; training (random-battle teams still come from the Gen 5 LCG, as Showdown's do) |
| `Seed::ShowdownGen5([u16; 4])` | Showdown's Gen 5 LCG (`a = 0x5D588B656C078965`, `c = 0x269EC3`) | conformance; identical stream to Showdown's `gen5,…` seeds |
| `Seed::Sodium([u8; 32])` | Showdown's `SodiumRNG`: a ChaCha20 block (nonce `LibsodiumDRG`, counter 0) per draw, keyed by the seed; its first 32 bytes are the next seed, the next 4 (big-endian) the draw | Showdown's default seeds (`sodium,…`), e.g. in `pikapro-server`'s battle stream |

| operation | formula (Showdown `PRNG`) |
|---|---|
| `random(n)` | `floor(next() * n / 2^32)` |
| `random_range(a, b)` | `a + random(b - a)` |
| `chance(k, n)` | `random(n) < k` |
| `sample(xs)` | `xs[random(len)]`: draws even when `len == 1` |
| `shuffle(xs)` | Fisher–Yates from the front, `random(i, len)` |

The `sodium` backend is checked draw for draw against Showdown's `PRNG`
(`rng::tests::sodium_matches_showdown`). Core takes the 32-byte key; the
seed string (`sodium,<hex>`, read with `Utils.bufWriteHex`'s quirks) is
parsed by `pikapro-server`.

## How it's tested

- `crates/pikapro/tests/determinism.rs`: identical inputs → identical
  events/state across all formats; clones and forks are independent; turning
  event logging off doesn't change the simulation; the Showdown backend is
  deterministic.
- `crates/pikapro-conformance/tests/golden.rs`: recorded Showdown battles replay
  with identical logs **and identical RNG draws**.
