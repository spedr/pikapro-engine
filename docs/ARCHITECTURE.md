# Architecture

The engine, the `pikapro` crate, is a battle simulator only, the
counterpart of Pokémon Showdown's `sim/` directory. Showdown's text
formats, the team validator, the player view, the RL environment, the
Python bindings and the servers are separate crates built on top of it.

## Crates and dependency rules

Each crate depends only on the crates above it in this tree, and on
`pikapro-data`, which everything uses:

```
pikapro-data                    static tables (the `legality` feature: learnsets, events)
└─ pikapro                      the engine
   ├─ pikapro-protocol          Showdown's text formats
   │  ├─ pikapro-server         + teams: the battle stream
   │  │  └─ pikapro-showdown-server   + validate: the websocket server (publish = false)
   │  └─ pikapro-view           the player view
   ├─ pikapro-validate          the team validator (data with `legality`)
   │  └─ pikapro-teams          random teams, Showdown's random-battle generator
   ├─ pikapro-rl                + protocol, view, validate, teams
   │  └─ pikapro-py             + server: the Python package (publish = false)
   └─ pikapro-cli               + protocol, validate
pikapro-conformance, pikapro-bench   dev tools over the others (publish = false)
```

`pikapro-py` adds PyO3 and numpy behind its `python` feature;
`pikapro-showdown-server` adds tokio; `pikapro-bench` adds Criterion.

Rules:

- **No external dependencies** in `pikapro-data`, `pikapro`, `pikapro-protocol`,
  `pikapro-validate`, `pikapro-teams`, `pikapro-server`, `pikapro-view` or `pikapro-rl`.
- **Core never formats text.** It emits typed [`BattleEvent`]s; the protocol
  crate renders them.
- **Core never performs I/O**, reads the clock, or uses thread-locals.

## Core concepts

| concept | type | notes |
|---|---|---|
| Battle | `Battle` | Owns format, state, RNG, pending choices and the event log. `Clone + Send + Sync`. |
| Battle state | `BattleState` | Plain `Copy` data: turn, two `SideState`s, `FieldState`. Cloning it is a snapshot. |
| Player state | `SideState` | Team (in Showdown's order), active slots, side conditions, gimmicks used. |
| Pokémon state | `PokemonState` | Species, level, types, stats, HP, status, boosts, move slots, volatiles. |
| Team (input) | `Team`, `PokemonSet`, `Investment`, `Gender` | What a player registers. Validated once, converted to state once. |
| Move | `pikapro_data::MoveData` + `MoveSlot` | Static data + per-Pokémon PP. |
| Action / choice | `Choice`, `SlotAction`, `Target` | What a player submits. |
| Request | `Request`, `RequestData` | What the battle waits for from a side: team preview, moves, forced switch. |
| Legal actions | `LegalActions` | Per-slot legal actions, joint validation, uniform sampling, enumeration. |
| Turn resolution | `resolve` module | Action queue, speed sort, moves, damage, switching, residuals, faint/win. |
| Battle result | `BattleResult`, `EndReason` | Winner (or tie), reason, turn count. |
| RNG / seed | `BattleRng`, `Seed` | Single randomness source; fast or Showdown-compatible backend. |
| Format / ruleset | `Format`, `Mechanics`, `GameType`, `StatSystem`, `TeamPreview`, `Gimmicks`, `Clauses` | Plain data. Mechanics branch on fields, never on format names. |

### Lifecycle

```
Battle::new(format, [team1, team2], options)
  ├─ validate teams, build SideStates (stats, PP, level rule)
  ├─ team preview? ── yes ─► requests = [TeamPreview; 2]
  └─ no ─► start: switch in leads, turn 1, requests = [Move; 2]

loop:
  battle.request(side) / battle.legal_actions(side)
  battle.submit(side, choice)   # validated; rejected choices change nothing
  └─ when every non-Wait side has submitted: resolve synchronously
       ├─ TeamPreview ─► reorder/truncate teams, start
       ├─ Move        ─► queue + speed sort, run actions, residual, faints,
       │                 then ForceSwitch requests or next turn
       └─ ForceSwitch ─► switch in replacements, next turn
until battle.result() is Some
```

### Turn resolution mirrors Showdown

`resolve/` ports Showdown's control flow, down to the order of random
draws, which is what makes the engine match it exactly
([CONFORMANCE.md](CONFORMANCE.md)):

- **A persistent action queue** (`resolve/queue.rs`, Showdown's
  `battle-queue.ts`). Team picks, switches, gimmicks, moves and the start
  and end of turn are queued actions with Showdown's `order` values,
  committed with `addChoice` and sorted with `speedSort`.
- **`commitChoices` → `turnLoop` → `runAction`** (`resolve/mod.rs`),
  including Showdown's end-of-action processing.
- **Target selection** (`resolve/targeting.rs`).
- **The move pipeline** (`resolve/moves.rs`): `runMove` → `useMove` →
  `trySpreadMoveHit`.
- **The event system** (`resolve/events.rs`): Showdown's `runEvent` /
  `singleEvent` / `eachEvent` without its dynamic dispatch, finding and
  sorting handlers in Showdown's order.

Each family of mechanics has a module in `resolve/`, called by the move
pipeline at Showdown's call sites. Anything data can express is read from
`MoveData`; `MoveData::unmodelled` names what needs code.

Team positions swap on every switch, like Showdown's `side.pokemon`.
`PokemonState::id` is the stable identity (the registration index) that
queued actions and consumers such as RL observations should key on.

### Hidden information

`BattleState` and `BattleEvent` are omniscient. `pikapro-protocol`'s
`Renderer` takes a `Perspective` (omniscient, a player, or spectator) and
shows foe HP as percentages, as Showdown does. `pikapro-view` builds each
player's view from the typed events and the player's own request, never
from the state, and the encoders in `pikapro-rl` read only that view.

## Performance design

- **No heap in state.** Teams, move slots and action lists are fixed-capacity
  inline `StackVec`s, and `BattleState` is `Copy`.
- **Compact IDs.** Species, moves, abilities and items are `u16` indices into
  `&'static` tables.
- **Integer-only math.** `clippy::float_arithmetic` is denied in core and data.
- **Optional logging.** `BattleOptions::without_events()` turns the event log
  into a no-op branch for training runs.
- **Parallel by battle.** `Battle` is `Send + Sync` with no shared mutable
  state; run one battle per thread.
- **Search-friendly.** `Battle::clone()` is a full snapshot;
  `Battle::fork(seed)` clones with a new RNG stream.
- **Legal actions once per request.** They are computed when a request is
  issued and cached on the `Battle`.

## Data

`tools/datagen` generates the tables from Pokémon Showdown, at the
version the conformance oracle uses ([its README](../tools/datagen/README.md)).
`pikapro_data::gen9` and `pikapro_data::champions` share one ID space, so
the constants work with either; `Format::dex` selects one. `MoveData`
represents Showdown's move fields structurally, and
`MoveData::unmodelled` lists by name what it cannot represent, which the
engine implements by hand. The generated files are never edited by hand.

[`BattleEvent`]: ../crates/pikapro/src/event.rs
