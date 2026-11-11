# The simulator API

`pikapro`'s lower layer: one `Battle` at a time, or many in a
`BattlePool`, with the omniscient state. For learning agents, start with
[`pikapro.rl`](concepts.md), which works on each player's view. Every
member is in the [API reference](api.md).

## Quick start

```python
import pikapro

team = pikapro.sample_team("gen9vgc")      # Showdown export text; packed teams work too
b = pikapro.Battle("gen9vgc", [team, team], seed=1)

while not b.over:
    for side in (0, 1):
        legal = b.legal_actions(side)    # choice indices, ascending
        if legal:
            b.submit(side, legal[0])     # or "move 1 2, switch 3", or a Choice
print(b.winner, b.result)
print("\n".join(b.log("p1")))            # what player 1's client would show
```

## Battle

```python
b = pikapro.Battle(format, [team1, team2], seed=0, log=True, players=("Alice", "Bob"))
```

- `format`: `"gen9singles"`, `"gen9doubles"`, `"gen9vgc"`, `"gen9ou"`, `"gen9randombattle"`,
  `"gen9randomdoublesbattle"`, `"championsvgc"` or `"championsbss"` (`pikapro.formats()`).
- Teams are Showdown's export or packed format. `pikapro.pack` and
  `pikapro.export` convert between the two.
- `seed`: an int selects the fast xoshiro backend, four 16-bit ints
  Showdown's Gen 5 PRNG, and 32 bytes Showdown's default `sodium` PRNG.
- `log=False` records no events, which is faster for search and training.
- `players`: the players' names in the log, `p1` and `p2` by default.

| member | |
|---|---|
| `request_kind(side)` | `"wait"`, `"team_preview"`, `"move"` or `"force_switch"` |
| `request(side)`, `request_json(side)` | the request, typed (`RequestData`) or as Showdown's JSON |
| `legal_actions(side)`, `legal_choices(side)` | the jointly legal choices, as indices or `Choice` objects (`str(c)` is the Showdown string) |
| `action_mask(side)` | bool array of `space.size`: the jointly legal choices |
| `slot_mask(side)`, `slot_actions(side)` | per active slot, the actions legal on their own |
| `submit(side, choice)` | an index, a list of per-slot indices, a Showdown string or a `Choice`. Returns whether the turn resolved. An illegal choice raises `ValueError` and changes nothing |
| `open_team_sheets()` | at team preview, the `\|showteam\|` lines Showdown's room sends when both players accept open team sheets |
| `view(side)` | the side's player view, a `pikapro.rl.BattleView`: what the player has seen |
| `fork(seed=None)` | a copy. With a seed, its randomness is reseeded (determinised rollouts). The state is `Copy`, so the cost is the event log: use `log=False` for search |
| `events(start=0)` | typed events (see below) |
| `log(perspective="omniscient", start=0)` | Showdown log lines for `"omniscient"`, `"p1"`, `"p2"` or `"spectator"` |
| `raw_log(start=0)`, `header()` | Showdown's raw stream with its `\|split\|` blocks; the lines before the first event |
| `teams(packed=False)` | both teams as registered |
| `state()`, `arrays()` | the whole battle state, typed (`BattleState`) or as int32 [arrays](#state-arrays) |
| `turn`, `over`, `winner`, `result`, `seed`, `event_count`, `space`, `format` | |

A choice that runs into something the request hides (a foe's unseen
Shadow Tag, a move only a foe's Imprison disables) is rejected as
Showdown rejects it, with `[Unavailable choice]`, and `request(side)`
then shows the updated request ([honest masks](concepts.md#honest-masks)).

To stream, read `event_count` after a submit and pass it as `start`.
`pikapro.RandomPolicy(seed).choose(battle, side)` is the engine's
uniform random policy.

## Typed values

Events, requests, results and state reach Python as `pikapro.Record`
objects that mirror the Rust types in
[`event.rs`](../../crates/pikapro/src/event.rs), [`request.rs`](../../crates/pikapro/src/request.rs)
and [`state.rs`](../../crates/pikapro/src/state.rs):

```python
ev = next(e for e in b.events() if pikapro.kind(e) == "Damage")
ev._variant              # "Damage": the variant name
ev.target.pos.side       # "P2": a unit variant is a str
ev.target.species        # "Incineroar": IDs are names
ev.hp, ev.max_hp         # exact HP (events are omniscient)
ev.source                # "Move", or Effect(...) for "[from] item: Life Orb"
ev._to_dict()            # plain dicts and lists, the variant under "_variant"
ev._keys()               # the field names
pikapro.kind(ev.source)     # the variant name, for a str or a Record
```

A record's own members start with an underscore, so a field name always
wins. `None`/`Some(x)` become `None`/`x`, and an enum variant named
`None` is the string `"None"`. Converting costs a few microseconds per
event; the fast paths are the pool's arrays.

## Action space

Each format has a fixed action space that depends only on its game type,
gimmicks and team preview, never on the teams (`b.space`,
`pikapro.ActionSpace(format)`).

**Slot actions** (one active slot), `0..slot_size`:

| index | action |
|---|---|
| `(move * G + gimmick) * T + target` | `move <move+1> [target] [gimmick]` |
| `switch_base + i`, `i` in `0..6` | `switch <i+1>` (a team *position*, as in Showdown) |
| `pass_index` (the last) | `pass` |

`T` (`targets`) is 1 in singles and 5 in doubles. In doubles, `0` means
no target (a move that takes none, or Showdown's random target). `1` and
`2` are the foes in slots 1 and 2 (Showdown's `1`, `2`), and `3` and `4`
are the user's side's slots (`-1`, `-2`). `G` is 1 + the format's
gimmicks. Gimmick `0` is none, then comes Terastallization in Gen 9
formats and Mega Evolution in Champions. The RL layer offers only these
format gimmicks. Supplied Mega Stones can also enable Mega Evolution in
Gen 9 simulator battles; use a typed `Choice` or `"move 1 mega"` with
`Battle.submit` for those actions, which the fixed encoding omits.

**Choices** (a side's whole decision), `0..size`:

- `0..joint_size` answer a move or forced-switch request. The index is
  `a0 * slot_size + a1` in doubles and `a0` in singles
  (`space.joint_index`, `space.split`).
- `joint_size..size` answer team preview: the ordered selections of
  `bring` of the six team positions, in lexicographic order
  (`space.preview_order(k)`). In gen9ou, whose team preview brings the
  whole team, there are six: choice `k` leads with position `k` and keeps
  the others in team order, as Showdown's `team <k+1>` does
  (`space.lead_only`). A full order of your own goes in as a string
  (`b.submit(side, "team 352146")`).

| format | `slot_size` | `joint_size` | `preview_size` | `size` |
|---|---|---|---|---|
| gen9singles | 15 | 15 | 0 | 15 |
| gen9doubles | 47 | 2209 | 0 | 2209 |
| gen9vgc | 47 | 2209 | 360 | 2569 |
| gen9ou | 15 | 15 | 6 | 21 |
| gen9randombattle | 15 | 15 | 0 | 15 |
| gen9randomdoublesbattle | 47 | 2209 | 0 | 2209 |
| championsvgc | 47 | 2209 | 360 | 2569 |
| championsbss | 15 | 15 | 120 | 135 |

`space.decode(i)` returns a `Choice`, `space.encode(choice)` an index,
and `space.describe(i)` the Showdown string.

## BattlePool

```python
import numpy as np, pikapro

team = pikapro.sample_team("gen9vgc")
pool = pikapro.BattlePool("gen9vgc", [team, team], n=1024, seed=0, threads=8)
rng = np.random.default_rng(0)

for _ in range(1000):
    masks = pool.masks()                            # bool (n, 2, size)
    actions = my_policy(pool.state(), masks)        # int (n, 2) choice indices
    status, done, winner, end_reason = pool.step(actions)   # GIL released during the step
```

- `step(actions)` takes `(n, 2)` choice indices or `(n, 2, active)` slot
  actions, as an integer array. At team preview, slot 0 holds the order's
  index. It returns:
  - `status` int8 `(n, 2)`: `1` submitted, `0` not asked, `-1` rejected
    with nothing changed, `2` unavailable. A rejected side keeps its
    request.
  - `done` bool `(n,)`, true once, in the step a battle ends.
  - `winner` int8 `(n,)`: `-1` not over, `0`/`1` the winner, `2` a tie.
  - `end_reason` uint8 `(n,)`: `0` not over, `1` all fainted, `2` the turn
    limit, `3` a forfeit, `5` an error.
- `masks("choice" | "slots" | "preview")` return bool arrays of shape
  `(n, 2, size)`, `(n, 2, active, slot_size)` and `(n, 2, preview_size)`,
  with `VecEnv`'s [honest](concepts.md#honest-masks) meaning.
  `requests()` returns int8 `(n, 2)` (`REQUEST_CODES`).
- `state()` returns every battle's raw state: int32 Pokémon `(n, 2, 6, K)`,
  sides `(n, 2, S)` and field `(n, F)`.
- `autoreset` is `"next_step"` (the default), `"same_step"` or `None`, as
  in [`VecEnv`](concepts.md#autoreset). With `None` a battle stays over
  until `reset`. With `"same_step"`, `final_state()` keeps the ended
  episode's terminal state.
- Battle `i`'s `k`-th episode is episode `e = i + k*n`, with a battle seed
  hashed from `(seed, e)`, independent of the thread count. Its teams are
  pair `e % len(pairs)` (`teams` is one pair or a list of pairs), or
  fresh legal teams with `RandomTeams` ([below](#random-teams)).
  `teams="sets"` or `"uniform"` is `RandomTeams(format, mode)`.
  `episodes()` and `seeds()` report them, and `Battle(format, teams,
  seed)` replays one.
- `sample_random()` returns uniformly random actions from the masks.
  `play_random(b)` plays `b` whole battles in Rust.
  `pikapro.sample_slots(pool, rng)` is a numpy random policy.
- `finished()` totals every battle that ended: battles, turns, wins,
  ties and `decisions`.
- A battle that fails ends only its own episode (`end_reason` 5, `winner`
  -1); `take_errors()` says why, as in `VecEnv`
  ([termination](concepts.md#rewards-and-termination)).
- `battle(i)` copies battle `i` as a `Battle` (pass `log=True` to the
  pool to keep events). `reset(indices)` starts new episodes.
- `threads` defaults to the number of CPUs. `spin_us` is how long an idle
  worker polls before it sleeps.

## State arrays

`pikapro.POKEMON_COLUMNS`, `SIDE_COLUMNS` and `FIELD_COLUMNS` name the
columns, and `pikapro.column(name, columns)` gives an index. The arrays
are omniscient and unnormalised. IDs index `pikapro.names("species" |
"moves" | "abilities" | "items" | "types")`, and missing values are `-1`.
Pokémon rows are in current team order (Showdown's: `team[slot]` is the
Pokémon active in `slot`), and `id` is the stable registration index.
`status` codes are `STATUS_CODES`, `weather` codes `WEATHER_CODES`,
`terrain` codes `TERRAIN_CODES`.

`volatiles0..3` hold the volatile bits, 32 per column. Bit positions can
move in any release, so
decode them by name: `pikapro.volatiles(row[v0:v0 + 4])`, or
`pikapro.VOLATILE_NAMES[bit]`.

## Random teams

Except for the random battles' `sets` teams, Showdown's validator accepts
every team `pikapro-teams` draws: gen9vgc as
Regulation I, championsvgc as Regulation M-C, gen9ou as OU, and the custom
games as Anything Goes with Species Clause. The random battles' `sets`
teams are Showdown's random-battle generator's, ported exactly: the team
for a seed is Showdown's for the seed `gen5,<seed in 16 hex digits>`.
Nothing validates them, as on Showdown, and they take no options.

```python
t = pikapro.random_team("championsvgc", "sets", seed=7)   # export text; packed=True for packed
pikapro.validate(t, "championsvgc")                        # [] (problems in Showdown's words)

species = ["Garchomp", "Rillaboom", "Incineroar", "Amoonguss", "Flutter Mane", "Urshifu", "Tornadus"]
rt = pikapro.RandomTeams("gen9vgc", "uniform", team_size=6, species=species)
rt.team(7)                                              # one team, = pikapro_teams' team(7)
pool = pikapro.BattlePool("gen9vgc", rt, n=1024)           # fresh teams every episode
pool.battle(0).teams() == rt.pair(int(pool.seeds()[0])) # True
```

- `mode="uniform"` draws every legal choice uniformly. `mode="sets"` draws
  sets from Showdown's random-battle data.
- The options are `level`, `team_size`, `spreads` (`"competitive"` or
  `"showdown"`), and name lists `species`, `moves`, `abilities` and
  `items` that restrict the draw. Options no team can satisfy raise
  `ValueError`.
- In the episode with battle seed `s`, a pool with `RandomTeams` plays
  the pair `rt.pair(s)`.

For speed, see [performance](performance.md#the-simulator-from-python).
