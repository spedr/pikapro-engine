# API reference

<!-- Generated from the docstrings; don't edit by hand. -->

Every public name of the stable modules, from
their docstrings. Signatures leave out type annotations: the package ships
them (`py.typed`, `_pikapro.pyi`) for your editor and type checker.

- [`pikapro`](#pikapro)
- [`pikapro.rl`](#pikaprorl)
- [`pikapro.rl.view`](#pikaprorlview)
- [`pikapro.rl.teams`](#pikaprorlteams)
- [`pikapro.rl.eval`](#pikaprorleval)
- [`pikapro.rl.gym`](#pikaprorlgym)
- [`pikapro.rl.pettingzoo`](#pikaprorlpettingzoo)
- [`pikapro.rl.sb3`](#pikaprorlsb3)
- [`pikapro.compat.poke_env`](#pikaprocompatpoke_env)
- [`pikapro.showdown`](#pikaproshowdown)

## pikapro

pikapro: Python bindings for pikapro, a deterministic Pokémon battle simulator.

The engine is a port of Pokémon Showdown's simulator (Gen 9, VGC, Pokémon
Champions) in Rust. This package exposes it at close to native speed:

* `Battle` - one battle: requests (typed and as Showdown's JSON),
  legal actions (as lists and numpy masks over a fixed action space),
  choices (indices, Showdown strings or `Choice` objects), cheap
  `Battle.fork`, typed events and Showdown log lines for any
  perspective.
* `BattlePool` - many battles stepped per call on Rust threads with
  the GIL released; masks, actions and raw state as numpy arrays.
* `ActionSpace` - the stable integer indexing of actions per format.
* `pikapro.rl` - the reinforcement-learning environment: a fused vector
  environment over player views (not the omniscient state), the
  `basic` observation, Gymnasium / PettingZoo / Stable-Baselines3
  adapters, team pools and evaluation (`docs/RL_GUIDE.md`).

Quick start:

```python
import pikapro
team = pikapro.sample_team("gen9vgc")
b = pikapro.Battle("gen9vgc", [team, team], seed=1)
while not b.over:
    for side in (0, 1):
        legal = b.legal_actions(side)
        if legal:
            b.submit(side, legal[0])
print(b.winner, "\n".join(b.log("p1")))
```

### `pikapro.ACTION_SPACE_VERSION`

`int`: `1`

The version of the action layout: `ActionSpace`'s slot-action, choice and team-preview indexing, and the factored and joint masks of `BattlePool` and `pikapro.rl.VecEnv` built on it. Bumped whenever an index would mean a different action; a policy trained on one version must not be used with another.

### `pikapro.FIELD_COLUMNS`

`list`: `<list of 12>`

The columns of the field row of the raw state arrays.

### `pikapro.POKEMON_COLUMNS`

`list`: `<list of 52>`

The columns of the Pokémon rows of the raw state arrays (`Battle.arrays`, `BattlePool.state`).

### `pikapro.REQUEST_CODES`

`list`: `['wait', 'team_preview', 'move', 'force_switch']`

The codes of `BattlePool.requests`, by value.

### `pikapro.SIDE_COLUMNS`

`list`: `<list of 21>`

The columns of the side rows of the raw state arrays.

### `pikapro.STATUS_CODES`

`list`: `['none', 'brn', 'frz', 'par', 'psn', 'tox', 'slp']`

The `status` column's codes, by value.

### `pikapro.TERRAIN_CODES`

`list`: `['Electric', 'Grassy', 'Misty', 'Psychic']`

The `terrain` column's codes, by value (-1: none).

### `pikapro.VOLATILE_NAMES`

`list`: `<list of 128>`

The name of each of the 128 volatile bits (`""` for an unused one).

### `pikapro.WEATHER_CODES`

`list`: `['Sun', 'Rain', 'Sand', 'Snow', 'HarshSun', 'HeavyRain', 'StrongWinds']`

The `weather` column's codes, by value (-1: none).

### class `pikapro.ActionSpace`

```python
class ActionSpace(format)
```

A format's action space: stable integers for slot actions and choices.

Slot action `i` of `slot_size`: moves first (`move * G * T + gimmick *
T + target`, with `T = targets` and `G = 1 + len(gimmicks)`), then
`switch_base + position` for the six team positions, then `pass_index`.
Choice `i` of `size`: `0..joint_size` are per-slot actions combined
(`a0 * slot_size + a1` in doubles, `a0` in singles), `joint_size..size`
team-preview orders in lexicographic order. See `docs/python/simulator.md`.

#### `ActionSpace.active`

*attribute*

Active slots per side: 1 in singles, 2 in doubles.

#### `ActionSpace.bring`

*attribute*

Team positions a team-preview choice names: the Pokémon brought to
battle (0 without team preview), or 1 when `lead_only`.

#### `ActionSpace.decode(index)`

The `Choice` of choice index `index` (`IndexError` out of range).

#### `ActionSpace.decode_slot(index)`

The `SlotAction` of slot-action index `index` (`IndexError` out of
range).

#### `ActionSpace.describe(index)`

The Showdown choice string of choice `index`.

#### `ActionSpace.encode(choice)`

The choice index of `choice` (`ValueError` if the space has no such
choice).

#### `ActionSpace.encode_slot(action)`

The slot-action index of `action` (`ValueError` if the space has no such
action).

#### `ActionSpace.gimmicks`

*attribute*

The format's gimmicks after gimmick `0` (none): `["terastallize"]` in
Gen 9, `["mega"]` in Champions.

#### `ActionSpace.joint_index(slots)`

The choice index of per-slot action indices.

#### `ActionSpace.joint_size`

*attribute*

Choices that answer a move or forced-switch request: `slot_size **
active`.

#### `ActionSpace.lead_only`

*attribute*

Whether team preview brings the whole team (gen9ou): a preview
choice names the lead, and the others follow in team order.

#### `ActionSpace.pair_legal(slot_masks, min_switches, a0, a1)`

Whether slot actions `a0` and `a1` make a legal doubles choice given
a side's factored slot masks (`active * slot_size` bools, or a whole
factored mask row) and its `min_switches`: both in their slot's mask,
not two switches to one Pokémon, at most one gimmick, and at least
`min_switches` switches. In singles `a1` is ignored.

#### `ActionSpace.pair_mask(masks, min_switches, a0)`

Batched `pair_legal` for autoregressive policies: for factored masks
`(..., W)` (W at least `active * slot_size`), `min_switches` `(...)`
and first-slot actions `a0` `(...)`, the second slot's actions that
complete a legal choice, bool `(..., slot_size)` (all false where
`a0` itself isn't allowed).

#### `ActionSpace.pass_index`

*attribute*

The slot-action index of `pass` (the last).

#### `ActionSpace.preview_order(k)`

Team-preview order `k` (`0 <= k < preview_size`).

#### `ActionSpace.preview_size`

*attribute*

Team-preview choices: the ordered selections of `bring` of the six team
positions (0 without team preview).

#### `ActionSpace.size`

*attribute*

Every choice: `joint_size + preview_size`.

#### `ActionSpace.slot_size`

*attribute*

Slot actions per active slot: moves with their targets and gimmicks, six
switches and pass.

#### `ActionSpace.split(index)`

The per-slot action indices of a choice index below `joint_size`.

#### `ActionSpace.switch_base`

*attribute*

The slot-action index of `switch 1` (switches are `switch_base + i` for
team position `i`).

#### `ActionSpace.targets`

*attribute*

Target choices per move (`T`): 1 in singles; 5 in doubles (none, the
foes in slots 1 and 2, the user's side's slots).

### class `pikapro.Battle`

```python
class Battle(format, teams, seed=None, *, log=True, players=None)
```

One battle of `format` between two teams (Showdown's export or packed
format).

`seed`: an int selects the fast RNG; four 16-bit ints Showdown's Gen 5
PRNG; 32 bytes its default sodium PRNG (ChaCha20). The same format,
teams, seed and choices give the same battle, bit for bit. `log=False`
records no events (faster, for search). `players` names the players in
the log (`p1` and `p2` by default).

Each side answers its request (`request`, `request_kind`,
`legal_actions`, `action_mask`) with `submit`: a choice index, per-slot
indices, a Showdown choice string or a `Choice`. The turn resolves
once both sides have chosen. `fork` copies the battle, `log` renders
Showdown's protocol for any perspective, `events` returns typed
events, and `view` the player's view.

#### `Battle.action_mask(side)`

Jointly legal choices as a bool array of `space.size`.

#### `Battle.arrays()`

The raw state as int32 arrays: Pokémon `(2, 6, len(POKEMON_COLUMNS))`,
sides `(2, len(SIDE_COLUMNS))`, field `(len(FIELD_COLUMNS),)`.

#### `Battle.choice_index(side, choice)`

The index of a choice (any form `submit` takes).

#### `Battle.event_count`

*attribute*

Number of events so far.

#### `Battle.events(start=0)`

The typed events from `start` on (`BattleEvent` records).

#### `Battle.forfeit(side)`

Ends the battle with `side` losing.

#### `Battle.fork(seed=None)`

A copy. Without `seed` it continues with the same random stream
(identical future); with one, its future randomness comes from
`seed` (a determinised rollout).

#### `Battle.format`

*attribute*

The format id.

#### `Battle.header()`

The lines before the first event (`|t:|`, `|gametype|`).

#### `Battle.legal_actions(side)`

Indices of every jointly legal choice, ascending.

#### `Battle.legal_choices(side)`

Every jointly legal choice, typed, in index order.

#### `Battle.log(perspective=None, start=0)`

The log lines a perspective (`"omniscient"`, `"p1"`, `"p2"`,
`"spectator"`) reads, for the events from `start` on. Start at 0 or
at an `event_count` read between submits.

#### `Battle.open_team_sheets()`

`battle.showOpenTeamSheets()` at team preview: the `|showteam|p1|...`
and `|showteam|p2|...` lines (each side's species, items, abilities,
moves, levels, genders and Tera types, and natures in Champions, in
Showdown's packed format), as Showdown's room sends them when both
players accept open team sheets. Raises `ValueError` without team
preview.

#### `Battle.over`

*attribute*

Whether the battle has ended (`result` says how).

#### `Battle.parse_choice(side, choice)`

Parses a Showdown choice string against the current request.

#### `Battle.players`

*attribute*

The players' names, as the log shows them (`|player|`, `|win|`):
`["p1", "p2"]` unless given.

#### `Battle.raw_log(start=0)`

Showdown's raw stream from event `start`: every line once, with
`|split|pN` blocks (the secret line, then the public one).

#### `Battle.request(side)`

Everything Showdown's `|request|` says, as a typed record
(`RequestData`). After `side` has submitted, it still describes the
request it answered, with `answered` true, while `request_kind` says
`"wait"`. After an `[Unavailable choice]`, it is the updated request
Showdown sends (`trapped`, the move `disabled`).

#### `Battle.request_json(side)`

The request as Showdown's `|request|` JSON (after an `[Unavailable
choice]`, the updated request it sends again, with `update: true`).

#### `Battle.request_kind(side)`

`"wait"`, `"team_preview"`, `"move"` or `"force_switch"`.

#### `Battle.result`

*attribute*

`BattleResult(winner=..., reason=..., turns=...)`, or `None`.

#### `Battle.seed`

*attribute*

The battle's seed, as given: an int, four 16-bit ints or 32 bytes.

#### `Battle.slot_actions(side)`

Per active slot, the slot-action indices legal on their own.

#### `Battle.slot_mask(side)`

Each slot's own legal actions, a bool array `(active, slot_size)`.

#### `Battle.space`

*attribute*

The format's `ActionSpace`.

#### `Battle.state()`

The whole battle state, typed (`BattleState`).

#### `Battle.submit(side, choice)`

Submits a choice: a choice index, a list of per-slot action indices,
a Showdown choice string (parsed as Showdown's `side.choose` does)
or a `Choice`. Returns whether the battle resolved (both sides had
chosen). A replacement clears the previous answer before validation,
including when it raises `ValueError` for an illegal choice. The
exception is a choice Showdown locks to avoid revealing hidden trapping
or disabling information: it cannot be changed. The simulated battle
state is unchanged by a rejection.

#### `Battle.teams(packed=False)`

Both players' teams as registered, in Showdown's export format (or
packed).

#### `Battle.turn`

*attribute*

The current turn (0 before the first).

#### `Battle.view(side)`

`side`'s player view as a `pikapro.rl.BattleView`
(`pikapro.rl.BattleView.from_battle(battle, side)`): what the player has seen. Built from the events so far
(the battle must record them) and the side's request, with the side's own team as every request so far
showed it (a benched Pokémon's PP is the last its requests showed), as `pikapro.rl.VecEnv`'s view keeps it.
A `Battle` doesn't know open team sheets or a series.

#### `Battle.winner`

*attribute*

`0` or `1` once a side has won, else `None` (also for a tie).

### class `pikapro.BattlePool`

```python
class BattlePool(format, teams, n, seed=0, threads=None, *, log=False, autoreset='next_step', spin_us=100)
```

`n` battles of `format` stepped together on Rust threads, with the GIL
released: masks, actions and the raw (omniscient) state as numpy arrays.

The masks and `sample_random` are what each side's request shows, as
`pikapro.rl.VecEnv`'s masks: a trap by a foe's unseen ability, or a move
only a foe's Imprison disables, stays allowed until a choice reveals it
(`step` status 2, Showdown's `[Unavailable choice]`); the side's masks
show it from then on, until its next request. `state()` is omniscient.

`teams` is one pair of team texts, a list of pairs (episode `e` plays
pair `e % len`), or a `RandomTeams` (fresh teams every episode);
`"sets"` (or `"random"`) and `"uniform"` stand for `RandomTeams(format,
mode)`, as in `pikapro.rl.VecEnv`.
A battle that fails (an engine panic) ends its episode alone, with end
reason 5, and the others go on; `take_errors()` says why.
Battle `i`'s `k`-th episode is episode `e = i + k * n`, played with
seed `pikapro_rl::env::episode_seed(seed, e)` (a hash of the two, as
`pikapro.rl.VecEnv`'s; `seeds()` lists the current ones), whatever the
number of `threads`. `autoreset` is `"next_step"` (the default, as
`pikapro.rl.VecEnv`'s: an ended battle stays over for the step that
ended it and starts its next episode at the next step, which ignores
its actions), `"same_step"` (an ended battle starts its next episode
within the step; `final_state` keeps the terminal state), or `None` or
`"off"` (the same: an ended battle stays over until `reset()`). For
the reinforcement-learning environment over player views, see
`pikapro.rl.VecEnv`.

#### `BattlePool.autoreset`

*attribute*

`"same_step"`, `"next_step"` or `None` (also for `"off"`).

#### `BattlePool.battle(i)`

A copy of battle `i` as a `Battle`.

#### `BattlePool.episodes()`

The episode each battle is playing (battle `i`'s `k`-th is `i + k *
n`), uint64 `(n,)`.

#### `BattlePool.final_state()`

The terminal state of the battles that ended in the last `step`
(with same-step autoreset, whose next episode has already started):
a dict of `pokemon` `(n, 2, 6, len(POKEMON_COLUMNS))`, `sides` `(n, 2,
len(SIDE_COLUMNS))` and `field` `(n, len(FIELD_COLUMNS))` int32 (as
`state()`), `episode` uint64 `(n,)`, `turns` uint16 `(n,)` and
`ended` bool `(n,)`; only the rows `ended` flags are set.

#### `BattlePool.finished()`

Totals over every battle finished so far (`decisions`: the choices
submitted in them).

#### `BattlePool.format`

*attribute*

The format id.

#### `BattlePool.masks(kind='choice')`

Masks of the actions each side's request shows, bool: `"choice"`
`(n, 2, size)` (jointly legal
choices), `"slots"` `(n, 2, active, slot_size)` (each slot's own
actions; all false at team preview), `"preview"` `(n, 2,
preview_size)` (team-preview orders).

#### `BattlePool.n`

*attribute*

The number of battles.

#### `BattlePool.play_random(battles)`

Plays `battles` whole battles with the built-in random policy
entirely in Rust. Returns totals and `elapsed` seconds. Checks for
Ctrl-C (Python's signal handlers) every few hundred battles; the
battles played until then stay played. With next-step autoreset, a
battle that ended in the last step starts its next episode here
(if it plays any of the `battles`), not at the next step.

#### `BattlePool.requests()`

Each side's request, int8 `(n, 2)`: `0` wait, `1` team preview, `2`
move, `3` forced switch (`REQUEST_CODES`).

#### `BattlePool.reset(indices=None)`

Starts the next episode in the given battles (all by default).

#### `BattlePool.sample_random(mode='choice')`

Uniformly random actions from the built-in policy among the ones
the masks allow, in the shape `step` takes for `mode` (`"choice"` or
`"slots"`); `-1` for sides with nothing to decide.

#### `BattlePool.seeds()`

Each battle's seed (hashed from the pool's seed and the episode, as
`pikapro.rl.VecEnv`'s), uint64 `(n,)`:
`Battle(format, teams, seed)` replays it.

#### `BattlePool.space`

*attribute*

The format's `ActionSpace`.

#### `BattlePool.state()`

The raw state as int32 arrays: Pokémon `(n, 2, 6,
len(POKEMON_COLUMNS))`, sides `(n, 2, len(SIDE_COLUMNS))`, field
`(n, len(FIELD_COLUMNS))`.

#### `BattlePool.step(actions)`

Submits one action per battle and side: an int array `(n, 2)` of
choice indices, or `(n, 2, active)` of slot-action indices (at team
preview, slot 0 holds the preview order's index within
`0..preview_size`). Values for sides with nothing to decide are
ignored. Returns `(status, done, winner, end_reason)`: int8 `(n, 2)`
(`1` submitted, `0` not asked, `-1` rejected, battle unchanged, `2`
unavailable: the request showed it but hid why the battle refuses
it, nothing submitted, and the masks now show it), bool
`(n,)` (the battle ended in this step), int8 `(n,)` (`-1` not over,
`0`/`1` the winner, `2` a tie) and uint8 `(n,)` (`0` not over, `1`
all fainted, `2` the turn limit, `3` a forfeit, `5` an error), the
last three for the episode that ended. With same-step autoreset its
next episode has already started (its terminal state is in
`final_state()`); with next-step autoreset it starts in the next
step, which ignores the battle's actions.

A battle that fails (an engine panic, or an episode that can't
start) doesn't raise: its episode ends with end reason 5, no winner
and statuses 0 (a failed start is reported at the step after), the
other battles' results are returned, and `take_errors()` says what
went wrong.

#### `BattlePool.take_errors()`

The battles that failed since the last call to this, as
`(battle, episode, message)`: the episode (under way, or one that
couldn't start and is skipped) ended with end reason 5 (see
`step`). At most 1,024 are kept.

#### `BattlePool.threads`

*attribute*

The number of threads stepping the battles.

#### `BattlePool.__len__()`

### class `pikapro.Choice`

```python
class Choice
```

A side's whole choice: one action per active slot, or a team order.

`str(choice)` is the Showdown choice string (`move 1 2, switch 3`,
`team 1234`).

#### `Choice.actions`

*attribute*

The slot actions (empty for a team order).

#### `Choice.from_actions(actions)`

*staticmethod*

One action per active slot (one in singles, two in doubles).

#### `Choice.kind`

*attribute*

`"actions"` or `"team"`.

#### `Choice.order`

*attribute*

The team order (empty for actions).

#### `Choice.team(order)`

*staticmethod*

A team-preview answer: team positions (0-based), the first `bring`
of which are brought, in that order.

#### `Choice.__str__()`

### `pikapro.column(name, columns=<list>)`

The index of a column of the raw state arrays
(`POKEMON_COLUMNS`, `SIDE_COLUMNS` or `FIELD_COLUMNS`).

### `pikapro.export(team, format)`

A team (export or packed) in Showdown's export format.

### `pikapro.formats()`

The preset format ids.

### `pikapro.kind(value)`

The variant name of a typed value: `value` itself for a unit
variant (a `str` such as `"Burn"`), `value._variant` for a
`Record`.

### `pikapro.names(table, format='gen9vgc')`

Every name of a table, indexed by ID: `"species"`, `"moves"`,
`"abilities"`, `"items"` or `"types"`. The ID space is shared by all
formats.

### `pikapro.pack(team, format)`

Validates a team (export or packed) against the format's structural
rules and returns it in the packed format.

### `pikapro.random_team(format, mode='uniform', seed=0, *, packed=False, **options)`

A random team from `pikapro-teams`, deterministic from `seed`.

Teams pass Showdown's validator, except for `mode="sets"` in
`gen9randombattle` and `gen9randomdoublesbattle`: that is
Showdown's random-battle team generator, ported exactly, whose teams
nothing validates. The team for `seed` is Showdown's for the seed
`gen5,<seed in 16 hex digits>`; it takes no `options`.

`mode` is `"uniform"` (every legal choice drawn uniformly) or
`"sets"` (sets from Showdown's random-battle data). `options` are
`RandomTeams`'s (`level`, `team_size`, `spreads`, and
`species`/`moves`/`abilities`/`items` name lists). The generator
is built once per process and reused. Returns export text, or the
packed format with `packed=True`.

### class `pikapro.RandomPolicy`

```python
class RandomPolicy(seed)
```

The built-in uniformly random policy (`LegalActions::sample`), with its
own RNG: `pikapro play --seed S` uses `RandomPolicy(S ^ 0x5EED)`.

#### `RandomPolicy.choose(battle, side)`

A uniformly random jointly legal choice for `side`, or `None` if it
has nothing to decide.

### class `pikapro.RandomTeams`

```python
class RandomTeams(format, mode='uniform', *, level=None, team_size=None, spreads='competitive', species=None, moves=None, abilities=None, items=None)
```

Random teams from `pikapro-teams`, for one preset and mode.

Teams pass Showdown's validator, except for `mode="sets"` in
`gen9randombattle` and `gen9randomdoublesbattle`: that is Showdown's
random-battle team generator, ported exactly, whose teams nothing
validates. `team(seed)` is Showdown's team for the seed
`gen5,<seed in 16 hex digits>`; it takes no options.

`mode` is `"uniform"` (every legal choice drawn uniformly) or `"sets"`
(sets from Showdown's random-battle data). Options: `level`,
`team_size`, `spreads` (`"competitive"` or `"showdown"`), and lists of
names restricting `species`, `moves`, `abilities` and `items`. The
generator is built once per process for each preset, mode and options.
Pass it as a `BattlePool`'s `teams` for fresh teams every episode.

#### `RandomTeams.format`

*attribute*

The format id.

#### `RandomTeams.mode`

*attribute*

`"uniform"` or `"sets"`.

#### `RandomTeams.pair(seed, packed=False)`

Both teams a `BattlePool` with these teams plays in the episode
whose battle seed is `seed` (see `BattlePool.seeds()`).

#### `RandomTeams.team(seed, packed=False)`

The team for `seed` (`pikapro_teams::TeamGenerator::team`), in export
format (or packed).

### class `pikapro.Record`

```python
class Record
```

A typed engine value: a struct, or an enum variant with fields.

Fields are attributes (`event.actor.species`) and items
(`event["hp"]`), and always win: the record's own members start with an
underscore (`_variant`, `_keys()`, `_to_dict()`), which no field does.
`_variant` is the Rust type or variant name (`"Switch"`, `"Actor"`,
`"Sleep"`). IDs are names, `None`/`Some` are `None`/the value, unit
variants are strings (`"P1"`, `"Burn"`, and `"None"` for an enum's own
`None` variant, such as `Status::None`), and a variant with unnamed
fields has them as `value` (one) or `values` (several). A record without
fields compares equal to its variant name as a string.

#### `Record._keys()`

The field names, in declaration order.

#### `Record._to_dict()`

The record as plain dicts and lists, recursively, with its variant
name under `"_variant"`.

#### `Record._variant`

*attribute*

The Rust type or variant name.

#### `Record.__contains__(key)`

#### `Record.__getitem__(key)`

### `pikapro.sample_slots(pool, rng)`

Random per-slot actions for every battle of `pool`, in the
`(n, 2, active)` shape `BattlePool.step` takes, from the
jointly legal choice masks (`BattlePool.masks`).

Slot 0 picks uniformly among its actions that some slot-1 action
completes; slot 1 then uniformly among those (the choice mask's
`(slot_size, slot_size)` block is the engine's pairing rule). Team
preview picks a uniformly random order; `-1` for sides with nothing
to decide. A Python-side random policy for benchmarking (masks out and
actions in through numpy every step); it is not uniform over joint
choices, and never picks an action `step` rejects.

### `pikapro.sample_team(format)`

The built-in sample team for `format`, in Showdown's export format.

### `pikapro.sample_uniform(masks, rng)`

A uniformly random True index along the last axis of a bool array;
`-1` where a row has none. Vectorised: the argmax of uniform noise
over the True entries (the False ones get `-1`, below any noise).

### class `pikapro.SlotAction`

```python
class SlotAction(kind, index=None, target=None, gimmick=None)
```

One active slot's action.

`SlotAction("move", 0, target=1, gimmick="terastallize")`,
`SlotAction("switch", 2)` (team position 2, 0-based), `SlotAction("pass")`.
`target` is Showdown's target location (`1`, `2`: foes; `-1`, `-2`: the
user's side); `gimmick` is `"terastallize"` or `"mega"`.

#### `SlotAction.gimmick`

*attribute*

The move's gimmick, `"terastallize"` or `"mega"`; `None` for none (or
not a move).

#### `SlotAction.index`

*attribute*

The move slot (0-based) or the team position switched to.

#### `SlotAction.kind`

*attribute*

`"move"`, `"switch"` or `"pass"`.

#### `SlotAction.target`

*attribute*

Showdown's target location of a move: `1`, `2` a foe, `-1`, `-2` the
user's side; `None` for no target (or not a move).

### `pikapro.to_id(text)`

Showdown's `toID`: the text lowercased, with everything but `a-z` and
`0-9` removed (`"Flabébé"` -> `"flabb"`, `"[Gen 9] VGC 2025 Reg I"` ->
`"gen9vgc2025regi"`). The engine's own (`pikapro_data::to_id`).

### `pikapro.validate(team, format)`

A team's problems in `format`, in Showdown's words: Showdown's team
validator (for gen9vgc and championsvgc their Regulations, for
championsbss Champions BSS Reg M-C, for gen9ou [Gen 9] OU, for the
custom games and the random battles Anything Goes with Species Clause), then the engine's own
rules. Empty if the team is
valid. Raises `ValueError` if the text doesn't parse.

### `pikapro.volatiles(bits)`

The names of the volatiles set in a Pokémon's volatile bits: the
four `volatiles0..3` columns of the raw state arrays (32 bits each,
as int32), or one 128-bit int. `VOLATILE_NAMES[bit]` names each bit
(`""` for an unused one).

## pikapro.rl

pikapro.rl: a fast, text-free reinforcement-learning environment.

An environment, not a bot: it answers "what happened, what can I do, what
did I get". Which features to encode, the network and the algorithm are
yours. The guide is `docs/RL_GUIDE.md`.

* `VecEnv` - the native fast path: `n` battles stepped per call
  on Rust threads with the GIL released, observations, masks, rewards and
  termination for both players written into numpy buffers allocated (or
  bound, `VecEnv.bind`) once.
* Observations: `"basic"`, a small float vector for a plain network.
  For features of your own, write a function over `BattleView`
  (`embed_all`), or an encoder in Rust (`docs/python/encoders.md`).
* `ChannelSession` - one battle read from Showdown's text (a
  player's channel and requests), with the view, observation and mask at
  each decision, and actions as `/choose` text:
  for playing on a Showdown server (`pikapro.showdown`);
  `ChannelSeries` carries a best-of series from one game's session
  to the next, as the environment's `series` mode plays it.
* `BattleView` - one player's view, read-only and poke-env-shaped:
  what the player has seen, for a Python `embed_battle`.
* `pikapro.rl.teams` - tournament team pools (pokepaste lists).
* `pikapro.rl.eval` - win rates over mirrored games, cross-evaluation.
* Adapters (optional dependencies, imported lazily):
  `pikapro.rl.gym` (Gymnasium `VectorEnv` and `Env`),
  `pikapro.rl.pettingzoo` (a `ParallelEnv`), `pikapro.rl.sb3`
  (a Stable-Baselines3 `VecEnv` for MaskablePPO).

Quick start:

```python
import numpy as np
import pikapro.rl as rl

env = rl.VecEnv("championsvgc", n=256, opponent="simple_heuristics")
out = env.reset(seed=0)
rng = np.random.default_rng(0)
for _ in range(100):
    out = env.step(env.sample_actions(rng))   # your policy here
    # out.obs, out.masks, out.reward, out.terminated, ...
```

### `pikapro.rl.ACTION_KINDS`

`dict`: `{'move': 0, 'switch': 1, 'pass': 2, 'preview': 3}`

The values of `action_table`'s `kind` column.

### `pikapro.rl.ACTION_TABLE_COLUMNS`

`tuple`: `('kind', 'move_slot', 'target', 'gimmick', 'switch', 'preview')`

The columns of `action_table`, by offset.

### `pikapro.rl.BUILTIN_POLICIES`

`tuple`: `('random', 'max_base_power', 'simple_heuristics')`

The built-in policies, poke-env's three baseline players, played in Rust: the one list the environment's opponents, `pikapro.rl.eval` and `pikapro.showdown` take.

### `pikapro.rl.END_REASONS`

`list`: `['none', 'all_fainted', 'turn_limit', 'forfeit', 'max_turns', 'error', 'endless_battle']`

`end_reason` codes, by value.

### `pikapro.rl.OBSERVATIONS`

`list`: `['basic']`

The values of `VecEnv(observation=...)`: the `basic` float vector.

### `pikapro.rl.OPPONENTS`

`list`: `['random', 'max_base_power', 'simple_heuristics', 'external']`

The built-in opponents (`BUILTIN_POLICIES`) and `"external"` (the caller plays player 1).

### `pikapro.rl.RANDOM_ACTION`

`int`: `-2`

An action value that lets the environment choose: a uniformly random choice among the ones the player's mask allows.

### `pikapro.rl.STATUS`

`dict`: `{'ignored': 0, 'submitted': 1, 'rejected': -1, 'unavailable': 2}`

`status` codes: what became of a player's action.

### `pikapro.rl.action_table(format)`

What every factored action index of `format` does, as static
integer columns a policy can gather from (no per-step cost).

Row `i < slot_size` is slot action `i` (a factored mask row's
`slot * slot_size + i`), row `slot_size + k` team-preview order
`k` (the factored mask's preview part, in the same order). The
columns (`ACTION_TABLE_COLUMNS`), each an int16 array over the
rows, are -1 where they don't apply:

* `kind` - `ACTION_KINDS`: 0 move, 1 switch, 2 pass, 3 team
  preview;
* `move_slot` - the move's slot, 0 to 3;
* `target` - the move's target code: 0 none, 1 and 2 the foes in
  slots 0 and 1, 3 and 4 the user's side's slots 0 and 1 (absolute:
  one is the user; `relative_target`); always 0 in singles;
* `gimmick` - 0 none, `i + 1` the space's `i`-th gimmick
  (`pikapro.ActionSpace.gimmicks`);
* `switch` - `k` for switch action `k`: own row `k` with
  `switch_by="row"` (the default), team position `k` with
  `switch_by="position"`;
* `preview` - the team-preview order's index.

Also `"preview_orders"`, int16 `(preview_size, bring)`: each
order's team positions (`pikapro.ActionSpace.preview_order`). The
arrays are read-only. With joint masks, a choice `j < joint_size`
is `a0 * slot_size + a1` (`pikapro.ActionSpace.split`).

### class `pikapro.rl.BattleView`

```python
class BattleView(view, format, *, mask=None, masks='factored', switch_by='row', player_role=None, series=None)
```

One player's view of a battle, poke-env-shaped and read-only.

Get one from a `pikapro.Battle` (`from_battle`, or
`battle.view(side)`), from a `pikapro.rl.VecEnv`
(`from_env`, or `env.view(i)`) or from a
`pikapro.rl.ChannelSession` (`session.view`). In doubles,
per-slot members are lists (`active_pokemon[slot]`), as poke-env's
`DoubleBattle` has them.

#### `BattleView.active_pokemon`

*property*

The player's active Pokémon (a list by slot in doubles).

#### `BattleView.all_active_pokemons`

*property*

Every active slot: the player's, then the foe's (`None` for
an empty one).

#### `BattleView.available_moves`

*property*

The moves the request allows (per slot in doubles).

#### `BattleView.available_switches`

*property*

The Pokémon the player may switch in (per slot in doubles).

#### `BattleView.can_mega_evolve`

*property*

Whether the player may Mega Evolve (per slot in doubles;
Champions; always `False` without Mega Evolution).

#### `BattleView.can_tera`

*property*

Whether the player may Terastallize (per slot in doubles;
always `False` without Terastallization).

#### `BattleView.doubles`

*property*

Whether the format has two active slots per side.

#### `BattleView.fields`

*property*

Terrain and pseudo-weathers: `{name: turns elapsed}`.

#### `BattleView.finished`

*property*

Whether the battle has ended.

#### `BattleView.force_switch`

*property*

Slots that must switch (a forced-switch request).

#### `BattleView.from_battle(battle, side)`

*classmethod*

`side`'s view of a `pikapro.Battle` (which must record
events), with its request's legal actions as the mask (switches by
team position). `side` is 0 or 1, or `"p1"` or `"p2"` in any
case, as for the battle's own methods; anything else raises
`ValueError` (`2`, `"p3"`) or `TypeError` (a bool).

#### `BattleView.from_env(env, i, player=0)`

*classmethod*

Player `player`'s view of battle `i` of a
`pikapro.rl.VecEnv`, as it is now, with the last step's
mask.

#### `BattleView.lost`

*property*

`True` if the player lost, `False` if it won or tied;
`None` while the battle goes on.

#### `BattleView.maybe_trapped`

*property*

Whether the request says the active Pokémon may be trapped by
something unseen, such as a foe's Shadow Tag (per slot in
doubles). Its switches stay allowed; see the honest masks in
`docs/python/concepts.md`.

#### `BattleView.opponent_active_pokemon`

*property*

The foe's active Pokémon (a list by slot in doubles; `None`
for an empty slot).

#### `BattleView.opponent_side_conditions`

*property*

The foe's side conditions: `{id: layers}`.

#### `BattleView.opponent_team`

*property*

The foe's Pokémon seen so far (previewed or revealed).

#### `BattleView.opponent_used_mega_evolve`

*property*

Whether the foe has Mega Evolved (Champions).

#### `BattleView.opponent_used_tera`

*property*

Whether the foe has used its Terastallization.

#### `BattleView.side_conditions`

*property*

The player's side conditions: `{id: layers}`.

#### `BattleView.team`

*property*

The player's Pokémon its request lists, by identifier (`"p1:
Garchomp"`), in own row order: after team preview, the ones
brought.

#### `BattleView.teampreview`

*property*

Whether the battle is at team preview.

#### `BattleView.trapped`

*property*

Whether the request says the active Pokémon is trapped (per
slot in doubles).

#### `BattleView.turn`

*property*

The current turn (0 at team preview).

#### `BattleView.used_mega_evolve`

*property*

Whether the player has Mega Evolved (Champions).

#### `BattleView.used_tera`

*property*

Whether the player has used its Terastallization.

#### `BattleView.weather`

*property*

`{weather: turns elapsed}` (empty without weather).

#### `BattleView.won`

*property*

`True` if the player won, `False` if it lost or tied;
`None` while the battle goes on.

### class `pikapro.rl.ChannelSeries`

```python
class ChannelSeries(format_id, best_of=None)
```

A best-of series played on Showdown, one `ChannelSession` per
game, as `pikapro.rl.VecEnv`'s `series` mode plays it: the
same two teams on the same sides every game, with open sheets.

`format_id` is the series' Showdown format
(`"gen9championsvgc2026regmcbo3"`) or its preset; `best_of` the
games (default: the format's, else 3). Each game's session is made with
`series=` this object, which gives it the score, the game number and
the earlier games' public records (what the environment's
`PlayerView.series` holds; the built-in encoders encode each game
alone, so the observation is a single game's with open sheets). When
the game ends, `record` its session: the series moves on to the
next game, or is settled.

```python
series = ChannelSeries("gen9championsvgc2026regmcbo3")
while not series.over:
    session = ChannelSession("gen9championsvgc2026regmcbo3", username=me, series=series)
    ...  # feed the game's battle room, choose
    series.record(session)
series.result  # "p1", "p2" or "tie"
```

Wins are by side (Showdown keeps each player's side through a series);
`ChannelSession.series_row` gives them from the player's side.

#### `ChannelSeries.best_of`

*property*

The series' length (3, 5, 7 or 9 games).

#### `ChannelSeries.game`

*property*

The game to play next (1-based); once the series is over, its
last game.

#### `ChannelSeries.over`

*property*

The series is settled.

#### `ChannelSeries.record(session, winner=None, tie=False)`

Records a game: the session's result and its public record at
the end, then moves to the next game unless the series is over. A
session that didn't read the game's end (the room closed first)
needs the result the server gave: `winner` (`"p1"`/`"p2"`) or
`tie=True`; so does one whose `|win|` named neither player.
`ValueError` for a session of another game, or once the series is
over.

#### `ChannelSeries.recorded`

*property*

Games recorded so far.

#### `ChannelSeries.result`

*property*

`"p1"` or `"p2"` (the series' winner), `"tie"`, or `None`
while it goes on. A player wins at `(best_of - ties) // 2 + 1`
wins, as Showdown's `BestOfGame` counts.

#### `ChannelSeries.settle(winner=None, tie=False)`

Ends the series as the server did (the series room's `|win|` or
`|tie`), whatever the games' score: `winner` (`"p1"`/`"p2"`)
or `tie=True`. Showdown ends a series early when a player forfeits
it, loses a game by disconnection, or stays away between games; the
game's room shows only the game's `|win|`. A no-op for a series
already settled so; `ValueError` for one settled otherwise.

#### `ChannelSeries.ties`

*property*

Games tied.

#### `ChannelSeries.wins`

*property*

Games won by p1 and by p2.

### class `pikapro.rl.ChannelSession`

```python
class ChannelSession(format_id, *, observation='basic', masks='factored', switch_by='row', username=None, hp='percent', team=None, series=None)
```

One battle, as one player sees it, read from Showdown's text.

`format_id` is the Showdown format (`"gen9championsvgc2026regmc"`,
`"gen9vgc2025regi"`, `"gen9randombattle"`, ...; a custom game's
rules after `@@@`) or a pikapro preset id (`"championsvgc"`, ...);
`ValueError` for one it can't read exactly. `observation`,
`masks` and `switch_by` are `pikapro.rl.VecEnv`'s options of
the same names, and `hp` its `hp` (`"exact"` only where the
channel shows exact HP: custom games). `username` is the player's
Showdown name: its `|player|` line says which side is its own
(without it, the first request does). `team` is the team the player
plays (Showdown's export or packed format): what no request shows
comes from it, the natures and, once the battle started, the Pokémon
left out at team preview, as in `pikapro.rl.VecEnv`. Without it,
the session knows what the requests showed (not the natures).
`series` makes the battle a game of a best-of series
(`ChannelSeries`, at the game it plays next).

Feed it every line the battle's room sends (`feed`,
`feed_many`): room and chat lines are ignored, lines it doesn't
know are kept in `unknown_lines`. When `needs_action`,
read `obs` and `mask`, choose an action as for the
environment, and send `/choose` with `choice`'s text (or let a
built-in policy choose: `choose_builtin`).

Before the battle's first request, its lines are held: the request
names the player's own Pokémon, which the lines must be read with.

#### `ChannelSession.actions_per_player`

*property*

Values in an action: the active slots with factored masks, 1
with joint masks.

#### `ChannelSession.choice(action)`

Showdown's `/choose` text (what follows `/choose `) for an
action, as `pikapro.rl.VecEnv.step` takes one player's: the
slot-action indices with factored masks (at team preview, the
order's index first), one choice index with joint masks. Switches
name rows or positions as `switch_by` says. `actions_per_player`
values (at team preview, the order's index alone may be given).
`ValueError` for another number of values, or if the mask doesn't
allow the action.

#### `ChannelSession.choose_builtin(name, seed=0)`

A built-in policy's choice (`"random"`, `"max_base_power"`,
`"simple_heuristics"`: the environment's opponents), as `/choose`
text; `seed` seeds its randomness.

#### `ChannelSession.error`

*property*

Why the last unreadable request couldn't be read, if any.

#### `ChannelSession.feed(line)`

Feeds one line of the channel (or several joined by `\n`).

#### `ChannelSession.feed_many(lines)`

Feeds lines in order (each may hold several joined by `\n`).

#### `ChannelSession.finished`

*property*

`|win|` or `|tie|` was read.

#### `ChannelSession.format`

*property*

The pikapro format the battle is read as (`"championsvgc"`, ...).

#### `ChannelSession.mask`

*property*

The mask of the current request (`(mask_width,)` bool), as the
environment's mask row for this player. It stays valid until the
next request, answered or not (a choice Showdown refused as
invalid is made again from it); all false outside a decision.

#### `ChannelSession.mask_width`

*property*

The mask's length (`pikapro.rl.VecEnv.mask_width`).

#### `ChannelSession.min_switches`

*property*

The fewest switches a joint action must make (the environment's
`min_switches`).

#### `ChannelSession.needs_action`

*property*

The player must choose now (a request that asks for a choice,
not yet answered).

#### `ChannelSession.obs`

*property*

The observation, as one player's row of `pikapro.rl.VecEnv`'s
`obs`: a float32 array. A fresh copy.

#### `ChannelSession.observation`

*property*

What `obs` holds, as `pikapro.rl.VecEnv.observation`.

#### `ChannelSession.request_kind`

*property*

`"none"` (no request yet), `"wait"`, `"teampreview"`,
`"move"` or `"switch"` (a forced switch).

#### `ChannelSession.series_row`

*property*

In a series, `pikapro.rl.VecEnv`'s `series` row for this
player (as the environment's player 0): `(4,)` uint8, the game,
the player's wins, the opponent's, and the series' result (0 going
on, 1 this player won it, 2 the opponent did, 3 a tie), with this
game's result once it ended. `None` outside a series.

#### `ChannelSession.settle_series(winner=None, tie=False)`

The series ended as the server says (the series room's `|win|`
or `|tie`: `winner` `"p1"`/`"p2"`, or `tie=True`), whatever
this game showed: `series_row` shows it settled.
`ValueError` outside a series, or for one the games settled
otherwise.

#### `ChannelSession.side`

*property*

The player's side, `"p1"` or `"p2"`, once known.

#### `ChannelSession.turn`

*property*

The last `|turn|` read (0 before the first).

#### `ChannelSession.unknown_count`

*property*

How many lines this session didn't know.

#### `ChannelSession.unknown_lines`

*property*

Lines this session didn't know (the first 1000; their number is
`unknown_count`).

#### `ChannelSession.unknown_names`

*property*

Species, moves, items and abilities the format's dex didn't know
(a newer server's), each read as a stand-in.

#### `ChannelSession.view`

*property*

The player's view as a `pikapro.rl.BattleView`, with the
mask (and in a series, its `series`).

#### `ChannelSession.winner`

*property*

The winner's name as the channel gives it (`None` for a tie or
before the end).

#### `ChannelSession.winner_side`

*property*

The winner's side, `"p1"` or `"p2"`.

### `pikapro.rl.embed_all(env, fn, player=0)`

An observation of your own in Python, poke-env's `embed_battle`:
`fn` on each battle's `BattleView` for `player`, stacked.
It costs Python per battle per step; `"basic"` and an encoder
written in Rust don't.

### class `pikapro.rl.MoveView`

```python
class MoveView(move)
```

A move as the player knows it (poke-env's `Move`): `id`,
`name`, `base_power`, `type`, `category`, `accuracy`
(a fraction, `True` if it never misses), `priority`, `target`,
`current_pp` / `max_pp` (own Pokémon: the request's while active,
else the last a request showed; `None` for the foe's),
`disabled`, `slot` (its move slot) and `uses`.

#### `MoveView.accuracy`

*attribute*

#### `MoveView.base_power`

*attribute*

#### `MoveView.category`

*attribute*

#### `MoveView.current_pp`

*attribute*

#### `MoveView.disabled`

*attribute*

#### `MoveView.id`

*attribute*

#### `MoveView.max_pp`

*attribute*

#### `MoveView.name`

*attribute*

#### `MoveView.priority`

*attribute*

#### `MoveView.slot`

*attribute*

#### `MoveView.target`

*attribute*

#### `MoveView.type`

*attribute*

#### `MoveView.uses`

*attribute*

### class `pikapro.rl.PokemonView`

```python
class PokemonView(battle, side, row)
```

One Pokémon as the player knows it (poke-env's `Pokemon`).

#### `PokemonView.ability`

*property*

The ability's id, `None` while unknown.

#### `PokemonView.active`

*property*

Whether it is on the field now.

#### `PokemonView.base_species`

*property*

The base species' id (`"urshifu"`); `None` if unknown.

#### `PokemonView.base_stats`

*property*

The species' base stats by name (`hp` to `spe`); empty
while the species is unknown.

#### `PokemonView.boosts`

*property*

Stat stages by name (`atk`, `def`, `spa`, `spd`,
`spe`, `accuracy`, `evasion`), -6 to 6.

#### `PokemonView.current_hp`

*property*

HP as the player knows it: exact for its own Pokémon, the shown
value (a percentage by default) for the foe's.

#### `PokemonView.current_hp_fraction`

*property*

`current_hp / max_hp`; 1.0 before any HP was shown, 0 fainted.

#### `PokemonView.damage_multiplier(attack)`

The type effectiveness of `attack` (a type name or a move)
against this Pokémon's current types.

#### `PokemonView.effects`

*property*

Its public volatiles (active only): `"substitute"`,
`"confusion"`, `"taunt"`, ...

#### `PokemonView.fainted`

*property*

Whether it has fainted.

#### `PokemonView.first_turn`

*property*

Whether it is active and switched in this turn (Fake Out,
First Impression).

#### `PokemonView.gender`

*property*

`"male"`, `"female"` or `"genderless"`.

#### `PokemonView.identifier(role)`

poke-env's identifier for it under `role` (`"p1"` or
`"p2"`): `"p1: Garchomp"`.

#### `PokemonView.is_mega`

*property*

Whether it has Mega Evolved (Champions).

#### `PokemonView.is_terastallized`

*property*

Whether it has Terastallized.

#### `PokemonView.item`

*property*

poke-env's convention: `"unknown_item"` while unknown, `""`
for no item (or a lost one), else the item's id.

#### `PokemonView.level`

*property*

Its level (as shown in the details for a foe).

#### `PokemonView.max_hp`

*property*

The maximum of `current_hp`: its exact max HP for its
own Pokémon, the shown scale (100 by default) for the foe's.

#### `PokemonView.moves`

*property*

The known moves by id: every own move; a foe's revealed ones (or
its Open Team Sheet's).

#### `PokemonView.must_recharge`

*property*

Whether it must recharge this turn (after Hyper Beam and the
like).

#### `PokemonView.own`

*property*

Whether it is the player's own Pokémon (`False` for a
foe's).

#### `PokemonView.protect_counter`

*property*

Consecutive successful protections (Protect, Detect, ...) so
far.

#### `PokemonView.revealed`

*property*

Whether it has been on the field (a foe's Pokémon only seen at
team preview is not revealed).

#### `PokemonView.slot`

*property*

The active slot it is in.

#### `PokemonView.species`

*property*

The species id shown now (`"urshifurapidstrike"`); `""` if
unknown.

#### `PokemonView.species_name`

*property*

The species' display name shown now
(`"Urshifu-Rapid-Strike"`); `None` if unknown.

#### `PokemonView.stats`

*property*

Exact stats for its own Pokémon (`None` for a foe's).

#### `PokemonView.status`

*property*

`None`, `"brn"`, `"frz"`, `"par"`, `"psn"`, `"tox"`
or `"slp"` (`"fnt"` when fainted).

#### `PokemonView.status_counter`

*property*

Turns the status was observed to last (sleep, toxic).

#### `PokemonView.tera_type`

*property*

Its Tera type if known (its own, an Open Team Sheet's, or
shown by Terastallizing); `None` otherwise, and in a format
without Terastallization.

#### `PokemonView.type_1`

*property*

The first of `types`; `None` while unknown.

#### `PokemonView.type_2`

*property*

The second of `types`; `None` for a single type or
while unknown.

#### `PokemonView.types`

*property*

Its current types (Tera and type changes applied), then an added
type; empty while unknown.

### `pikapro.rl.relative_target(slot, target)`

A doubles target code (0 to 4) relative to the acting `slot`: 0 no
target, 1 and 2 the foes in slots 0 and 1, 3 the user itself, 4 its
ally (codes 3 and 4 are the user's side's slots, absolutely).

### `pikapro.rl.replay(format, record, *, log=True)`

Plays a `VecEnv.record` again through `pikapro.Battle`:
the same teams, seed and choices give the same battle. Its
`pikapro.Battle.view` for a side is the view the environment gave
that side's player (the battle remembers each side's own team as its
requests showed it, as the environment does), for an environment with
`hp="percent"`, no open team sheets and no series, and unless a
choice was refused as unavailable in the episode (the record holds the
accepted choices only). `record(i)` mid-episode replays the episode
so far.

### class `pikapro.rl.Step`

```python
class Step(buffers)
```

What `VecEnv.reset` and `VecEnv.step` return: the
environment's buffers, by attribute (and item). **The arrays are the
environment's own, overwritten by the next call**: copy what you keep.

* `obs` - float32 `(n, 2, size)`: each player's observation
  (`VecEnv.observation` names its values). Axis 1 is the player,
  and each player's view is ego-centric (its own rows first).
* `masks` bool `(n, 2, width)`; `min_switches` uint8 `(n, 2)`.
* `reward` float32 `(n, 2)`; `terminated`, `truncated` bool
  `(n,)`; `end_reason` uint8 `(n,)` (`END_REASONS`);
  `winner` int8 `(n,)` (`-1` none, `0`/`1` the winning
  player, `2` a tie).
* `needs_action` bool `(n, 2)`; `status` int8 `(n, 2)`
  (`STATUS`); `agent_side` uint8 `(n,)`; `episode` uint64
  `(n,)`.
* series mode: `series` uint8 `(n, 4)` (game, player 0's wins,
  player 1's wins, result: 1 player 0 won the series, 2 player 1).
  One row per battle, in player 0's frame (the agent's, and what
  `ChannelSession.series_row` gives each player for itself): for
  player 1, swap the wins and the result, or read
  `BattleView.series`, which does.
* same-step autoreset: `final_obs` (as `obs`), `final_episode`,
  `final_turns` and `final_series`: the ended episodes' last
  observations (rows of battles that didn't end are stale).

#### `Step.agent_side`

*attribute*

#### `Step.as_dict()`

The buffers by name, as a dict (the same arrays, not copies).

#### `Step.done`

*property*

`terminated | truncated` (a new array).

#### `Step.end_reason`

*attribute*

#### `Step.episode`

*attribute*

#### `Step.final_episode`

*attribute*

#### `Step.final_obs`

*attribute*

#### `Step.final_series`

*attribute*

#### `Step.final_turns`

*attribute*

#### `Step.keys()`

The names of the buffers this step has (`None` ones left
out).

#### `Step.masks`

*attribute*

#### `Step.min_switches`

*attribute*

#### `Step.needs_action`

*attribute*

#### `Step.obs`

*attribute*

#### `Step.reward`

*attribute*

#### `Step.series`

*attribute*

#### `Step.status`

*attribute*

#### `Step.terminated`

*attribute*

#### `Step.truncated`

*attribute*

#### `Step.winner`

*attribute*

#### `Step.__getitem__(key)`

#### `Step.__iter__()`

### class `pikapro.rl.TeamPool`

```python
class TeamPool(texts, format, labels=None)
```

A validated pool of teams for one format (a tournament team pool): the
teams that import and pass the format's validator, and the rejects in
Showdown's words. `pikapro.rl.teams` builds them from pokepaste lists,
export texts and directories.

#### `TeamPool.format`

*attribute*

The format id the pool was validated for.

#### `TeamPool.labels`

*attribute*

The label of each team kept, in order.

#### `TeamPool.rejected`

*attribute*

The teams that didn't make it: `(index, label, problems)`, problems
in Showdown's words.

#### `TeamPool.team(i, packed=False)`

Team `i` in export format (or packed).

#### `TeamPool.teams(packed=False)`

Every team, in export format (or packed).

#### `TeamPool.__len__()`

### class `pikapro.rl.VecEnv`

```python
class VecEnv(format, n=1, *, threads=None, seed=0, teams=None, opponent='random', reward='terminal', ko_weight=None, max_turns=None, observation='basic', masks='factored', switch_by='row', autoreset='next_step', ots=False, ots_view='both', side_randomization=False, series=None, observe_builtin=False, preview='learned', hp='percent', record=False, shards_per_thread=8, spin_us=50)
```

`n` battles of `format` stepped together: the native fast path.

Rows are by *player*: player 0 is the agent (its actions always come
from the caller), player 1 its opponent (a built-in policy, or the
caller with `opponent="external"`). Each call is one request round;
observations, masks and rewards come out for both players, so self-play
is one batched forward pass.

Options:

* `threads` (default: every core), `seed` (episode `e` of battle
  `e % n` plays with a battle seed hashed from `(seed, e)`, which
  `record` gives; any thread count gives the same episodes, and
  base seeds `s` and `s + 1` give unrelated ones).
* `teams`: `None` (random `"sets"` teams), `"sets"` /
  `"uniform"` / a `pikapro.RandomTeams`, a pair of team texts
  (fixed teams), a list of pairs (episode `e` plays pair `e %
  len`), a `TeamPool` (both players draw from it: a tournament
  pool, `pikapro.rl.teams`), or two pools (one per player).
* `opponent`: `"random"`, `"max_base_power"`,
  `"simple_heuristics"`, `"external"`, or a mixture drawn per
  episode (`{"random": 1, "simple_heuristics": 3}`); changeable
  between calls with `set_opponent`.
* `reward`: `"terminal"` (+1 win, -1 loss, 0 tie) or `"ko"`
  (terminal plus `ko_weight`, 0.1 by default, per foe knocked out,
  minus per own: the return is the result plus `ko_weight` times the
  final KO margin). `ko_weight` without `reward="ko"` raises
  `ValueError`.
* `max_turns`: cut an episode (`truncated`, end reason 4) past it.
* `observation`: `OBSERVATIONS`: `"basic"` (a float vector
  for a plain network, see `docs/python/encoders.md`).
* `masks`: `"factored"` (each slot's own actions, then the team
  preview orders; actions `(n, 2, active)`) or `"joint"` (every
  joint choice; actions `(n, 2)`).
* `switch_by`: `"row"` (switch `k` sends in own observation row
  `k`, stable for the battle) or `"position"` (Showdown's team
  position).
* `autoreset`: `"next_step"` (the step that ends an episode returns
  its terminal observation; the next step starts the next episode and
  ignores the battle's actions) or `"same_step"` (the next episode
  starts at once; the terminal observation is in `final_obs`).
* `ots`: Open Team Sheets: `False`, `True` or a probability per
  episode. Only in a format with team preview, where Showdown shows
  the sheets (`ValueError` otherwise).
  `side_randomization`: player 0 plays p1 or p2 at random.
* `ots_view`: who sees the open sheets: `"both"` (the default) or
  `"player0"` (player 0 alone; player 1 plays as with closed
  sheets), for measuring what a sheet is worth.
* `series`: `None`, or `"bo3"` / `"bo5"` / `"bo7"` /
  `"bo9"` or the number of games (Showdown's best-of rules, open
  sheets, so a format with team preview; the score in
  `Step.series`).
* `observe_builtin`: encode a built-in opponent's view too (its rows
  are zeros otherwise).
* `preview`: `"learned"` (the player chooses the team order),
  `"random"` or `"fixed"`. `hp`: foe HP as `"percent"` (the
  ladder) or `"exact"`. `record`: keep each episode's choices for
  `record`.

`config` gives the options back by these names:
`VecEnv(**env.config)` makes an environment configured the same.

#### `VecEnv.battle(i)`

A copy of battle `i` as a `pikapro.Battle` (for inspection
and `pikapro.Battle.fork`; its event log holds only the events the
environment hasn't consumed).

#### `VecEnv.bind(**arrays)`

Binds output buffers by name (see `buffer_specs` for names,
shapes and dtypes): each must have the buffer's dtype and number of
values, be C-contiguous and writeable, and share no memory with
another buffer (`ValueError`). Steps then write straight
into them, with no copy: for instance numpy views of pinned torch
tensors (`torch.empty(shape, dtype=torch.float32).pin_memory()
.numpy()`), which `tensor.to("cuda", non_blocking=True)` then
moves. Buffers not given stay the environment's. All or nothing:
if one array is refused, none is bound (the buffers stay as they
were before the call).

#### `VecEnv.buffers`

*property*

The bound buffers by name (the arrays steps write into).

#### `VecEnv.close()`

Does nothing: the threads stop when the environment is
garbage-collected. Here for the Gymnasium-style interface.

#### `VecEnv.config`

*property*

The environment's resolved configuration as a dict, by the
names the constructor takes, so `VecEnv(**env.config)` makes one
configured the same: `format`, `n`, `seed` (the base seed of
the episodes under way: `reset` with a seed changes it),
`teams` (the team source, a `pikapro.RandomTeams` for the
string names), `opponent` (the constructor's, or the last one
`set_opponent` gave every battle), `reward` (with
`ko_weight` for `"ko"`), `observation`, `threads`, and the
other options.

#### `VecEnv.decode(i, player, actions)`

The `pikapro.Choice` `actions` (one player's, as
`step` takes them) make for battle `i`'s current request
(switches as team positions; `str()` is Showdown's choice
string).

#### `VecEnv.describe(i, player, actions)`

Showdown's choice string for `decode`.

#### `VecEnv.last`

*property*

The buffers as the last call left them.

#### `VecEnv.pair_mask(masks, min_switches, a0)`

For factored masks `(..., width)`, `min_switches` `(...)`
and slot-0 actions `a0` `(...)`: the slot-1 actions that make a
legal choice with them, bool `(..., slot_size)` (autoregressive
policies).

#### `VecEnv.record(i, previous=False)`

Battle `i`'s current (or `previous`) episode, with
`record=True`: `episode`, `seed`, `teams` (p1's and p2's,
packed), `agent_side` and `choices` (`(side, Choice)` in
order). `replay` plays it again through `pikapro.Battle`.

#### `VecEnv.reset(seed=None)`

Starts a new episode in every battle. With `seed`, episodes
are numbered from 0 again with that base seed (reproducible);
without, each battle starts its next episode.

#### `VecEnv.sample_actions(rng, players=(0, 1))`

Uniformly random allowed actions for the given players, from the
last step's masks, in the full `(n, 2, actions_per_player)` shape
(`-1` where a player isn't asked). In factored doubles, slot 1
draws among the actions that complete a legal choice with slot 0's
(`pair_mask`), and a slot-0 action nothing completes is left
to the environment (`RANDOM_ACTION`). Vectorised numpy: for
tests, baselines and exploration.

#### `VecEnv.set_opponent(opponent, which=None)`

Changes who plays player 1 in the given battles (all by default),
from each one's next episode on (in series mode, its next series):
a name, `"external"`, or a mixture `{name: weight}`. A curriculum is a few lines:

```python
if iteration == 100:
    env.set_opponent("external")   # self-play from here on
```

#### `VecEnv.set_pairs(positions, pairs)`

Replaces entries of the list of team pairs the environment was
made with (`teams=[(p1, p2), ...]`): `pairs[j]` goes to position
`positions[j]`, for the episodes started from now on (an episode
under way keeps its teams; in series mode, for the series started
from now on). The list keeps its length: episode `e`
still plays position `e % len`. Raises (changing nothing) for a
team the format refuses, a position out of range, or another kind
of team source.

#### `VecEnv.side_of(i, player)`

The side (0: p1, 1: p2) `player` plays in battle `i`'s episode.

#### `VecEnv.step(actions)`

One request round of every battle. `actions`: ints, `(n, 2,
actions_per_player)` for both players (`(n, 2)` with one action
per player), or player 0's alone `(n, actions_per_player)` (`(n,)`
with one). Values for players that aren't asked are ignored;
`RANDOM_ACTION` plays a random allowed one. An int64
C-contiguous array of the full shape goes to Rust without a copy.

#### `VecEnv.take_errors()`

The battles that failed since the last call, in order, as
`(battle, episode, message)`: a panic (an engine bug, a policy or
encoder that raised in Rust) or an error while stepping one ends
its episode with `end_reason` 5 (`"error"`, `truncated`, no
winner, zeroed observation) instead of failing the call, and the
battle starts its next episode at the next step. At most 1,024 are
kept.

#### `VecEnv.threads`

*property*

The number of threads stepping the battles.

#### `VecEnv.view(i, player=0)`

Player `player`'s view of battle `i` as a `BattleView`,
with its current mask.

#### `VecEnv.__len__()`

Also here: `ACTION_SPACE_VERSION` (see `pikapro`).

## pikapro.rl.view

A read-only, poke-env-shaped facade over one player's view.

`BattleView` answers with names shaped like poke-env's
`AbstractBattle` / `Pokemon` / `Move` (about 30 members, chosen from
poke-env's baseline players and examples). It is what a Python
`embed_battle` reads (`pikapro.rl.embed_all`); an encoder written
in Rust is the fast path. Values are plain strings and numbers (lowercase
ids, `"fire"`, `"brn"`), not poke-env's enums.

It holds only what the player knows: the view is built from the log and
the player's request, never from the battle's state.

Also here: `BattleView` (see `pikapro.rl`), `MoveView` (see `pikapro.rl`), `PokemonView` (see `pikapro.rl`).

## pikapro.rl.teams

Team pools: tournament teams from pokepaste links, export texts or a
directory, imported and validated against a format.

A pool is a `pikapro.rl.TeamPool`: the teams that import and pass the
format's validator (Showdown's: championsvgc and gen9vgc their
Regulations, gen9ou OU, the random battles and custom games Anything Goes
with Species Clause), and the rejects with their problems in Showdown's words.
Pass it as a `pikapro.rl.VecEnv`'s `teams` (both players draw from
it), or two pools (one per player):

```python
pool = rl.teams.from_pokepaste_list("teams.txt", "~/.cache/pikapro-pastes")
print(len(pool), "teams;", len(pool.rejected), "rejected")
env = rl.VecEnv("championsvgc", n=1024, teams=pool)
```

The pokepaste list is a text file with one link per line
(`https://pokepast.es/<id>`); blank lines and `#` comments are
skipped. Each paste is fetched once, from its raw text endpoint
(`https://pokepast.es/<id>/raw`), into `cache_dir`; later loads read
the cache and never touch the network.

### `pikapro.rl.teams.fetch_paste(url, cache_dir, *, timeout=10.0, fetch=True, opener=None)`

A paste's team text: from `cache_dir/<id>.txt` if cached, else
fetched from `https://pokepast.es/<id>/raw` (with `timeout`
seconds) and cached. With `fetch=False` a missing paste is an error
instead. `opener(url, timeout) -> bytes` replaces the HTTP client (for
tests and proxies). Raises `PokepasteError` (also for a line
that isn't a pokepaste link).

### `pikapro.rl.teams.from_dir(directory, format, pattern='*.txt')`

A pool from the files matching `pattern` in `directory` (one team
per file, in name order; labels are the file names).

### `pikapro.rl.teams.from_export_texts(texts, format, labels=None)`

A pool from team texts (Showdown's export or packed format).

### `pikapro.rl.teams.from_pokepaste_list(path, cache_dir, format='championsvgc', *, timeout=10.0, fetch=True, opener=None)`

A tournament team pool from a pokepaste list (see the module docs):
each paste fetched once into `cache_dir`, then imported and validated
for `format`. The pool's `labels` are the links; `rejected` lists
`(index, link, problems)` with problems in Showdown's words.

### `pikapro.rl.teams.paste_id(url)`

The id of a pokepaste link (`https://pokepast.es/<id>`, with or
without `/raw`); raises `ValueError` for anything else.

### class `pikapro.rl.teams.PokepasteError`

```python
class PokepasteError(...)  # bases: RuntimeError
```

A pokepaste that couldn't be fetched (with the URL and why).

### `pikapro.rl.teams.read_pokepaste_list(path)`

The links of a pokepaste list: one per line, blank lines and `#`
comments (whole-line or trailing) skipped.

### `pikapro.rl.teams.split_teams(text)`

Several export teams in one text, split on lines `=== ... ===`
(Showdown's teambuilder backup format); one team if there are none.

Also here: `TeamPool` (see `pikapro.rl`).

## pikapro.rl.eval

Evaluation: win rates over mirrored games, with Wilson intervals, and
poke-env-style cross-evaluation tables.

* Built-in policies (`"random"`, `"max_base_power"`,
  `"simple_heuristics"`) play in Rust (`pikapro_rl::eval`): every episode
  is played twice with the same battle seed, the policies swapping seats
  (and so teams), so luck of the draw cancels.
* A policy of your own is a callable over *batched* observations and
  masks: one call per step for all battles. It plays on a
  `pikapro.rl.VecEnv` as player 0; each episode is played twice, the
  second time with the teams swapped (the agent keeps its seat; seats
  don't matter to the simulator beyond the teams). The opponent is a
  built-in policy or another callable (self-play against a frozen
  checkpoint). An opponent callable sees everything from player 1's side:
  its `step` rows swapped, and an `env` whose per-player helpers
  (`last`, `sample_actions`, `view`, `decode`, `describe`,
  `side_of`) take player 0 to be itself:

```python
def policy(step, env):              # -> (n, actions_per_player) ints
    logits = net(torch.as_tensor(step.obs[:, 0]))
    logits[~torch.as_tensor(step.masks[:, 0])] = -inf
    return logits.argmax(-1).numpy()

r = rl.eval.evaluate(policy, "random", "championsvgc", episodes=500,
                     observation="basic", masks="joint")
print(r)   # 0.912 [0.887, 0.932] (912-88-0)
```

### `pikapro.rl.eval.cross_evaluate(policies, format, episodes, *, teams=None, seed=0, threads=None, hp='percent', batch=0)`

Every built-in policy against every other over `episodes`
episodes, each played twice with seats swapped (`2 * episodes`
games per pairing), in Rust. `batch`: battles held at once (0:
every episode, up to 4,096); the results don't depend on it. For
poke-env `Player` objects, see
`pikapro.compat.poke_env.cross_evaluate`.

### class `pikapro.rl.eval.CrossTable`

```python
class CrossTable(names, records)
```

`records[i][j]`: policy `i`'s record against `j` (`None` on
the diagonal).

#### `CrossTable.get(a, b)`

The record of policy `a` against policy `b` (`None` when
`a == b`).

#### `CrossTable.win_rates()`

poke-env's `cross_evaluate` shape: `[a][b]` is `a`'s win
rate against `b`.

#### `CrossTable.__str__()`

### `pikapro.rl.eval.evaluate(policy, opponent='random', format='championsvgc', episodes=100, *, teams=None, seed=0, threads=None, n=None, mirrored=True, hp='percent', **env_kwargs)`

`policy`'s record against `opponent` over `episodes` episodes
(twice each when `mirrored`). Each is a built-in name or a callable
`(step, env) -> actions`: `step` is the `pikapro.rl.Step` (an
opponent callable sees the players' rows swapped, so `[:, 0]` is its
own), and the actions are `(n, actions_per_player)` ints (`(n,)`
with one action per player). Two built-in names play in Rust
(`cross_evaluate`, `n` battles at once), always mirrored and
with no environment options: `mirrored=False` raises `ValueError`
and `env_kwargs` raise `TypeError` there. Otherwise the games play
on a `pikapro.rl.VecEnv` of `n` battles (default `min(episodes,
256)`) built with `env_kwargs` (`observation`, `masks`,
`max_turns`, ...); an episode whose battle fails there is counted in
`WinRecord.errors` (not played again), and a failure in Rust
raises `RuntimeError`.

`"external"` (alone or in a mixture) and `series=` raise
`ValueError`: pass a callable opponent for self-play, and evaluate
series on a `pikapro.rl.VecEnv` yourself. Either autoreset mode
works. If a battle stops moving (its asked players' actions rejected
100 steps in a row, or no episode ended in `20 * max_turns + 1000`
steps, 1,000 turns without `max_turns`), `RuntimeError` says
which.

### class `pikapro.rl.eval.WinRecord`

```python
class WinRecord(wins=0, losses=0, ties=0, turn_limit=0, turns=0, errors=0)
```

One policy's games against another. `errors` counts the
episodes whose battle failed, which are in no other count.

#### `WinRecord.errors`

*attribute, default `0`*

#### `WinRecord.flipped()`

The same games from the opponent's side (wins and losses
swapped).

#### `WinRecord.games`

*property*

Games played to a result: wins, losses and ties (not
`errors`).

#### `WinRecord.losses`

*attribute, default `0`*

#### `WinRecord.significantly_better`

*property*

The 95% interval lies above one half.

#### `WinRecord.ties`

*attribute, default `0`*

#### `WinRecord.turn_limit`

*attribute, default `0`*

#### `WinRecord.turns`

*attribute, default `0`*

#### `WinRecord.wilson(z=1.96)`

An interval for `win_rate` at the normal quantile `z`
(1.96 for 95%), computed by Rust's `pikapro_rl::eval::Record::wilson`.
A tie is neither a win nor a loss, so it is the Wilson score
interval of the win share of the decisive games, mapped onto the
score rate by the share of ties (`0.5 + (x - 0.5) * decisive /
games`). Without ties it is the plain Wilson interval; `(0, 1)`
when no game was decisive.

#### `WinRecord.win_rate`

*property*

Wins plus half the ties, per game.

#### `WinRecord.wins`

*attribute, default `0`*

#### `WinRecord.__str__()`

## pikapro.rl.gym

Gymnasium adapters (`pip install 'pikapro[gym]'`): a `VectorEnv` over
`pikapro.rl.VecEnv` and a single-battle `Env`.

The agent is player 0; player 1 is a built-in opponent (`"random"`,
`"max_base_power"`, `"simple_heuristics"` or a mixture), changeable
with `set_opponent`. `"external"` raises `ValueError` wherever it is
given (the constructor, a mixture, `set_opponent`): nothing would play
player 1. For self-play, use `pikapro.rl.VecEnv` directly
(`opponent="external"`) or `pikapro.rl.pettingzoo`.

* Observations are dicts `{"observation": ..., "action_mask": ...}`:
  the observation's float vector and the player's mask (int8,
  `MultiBinary`, as `space.sample(mask=...)` takes it).
* Actions are one `Discrete` index per battle: the joint choice index
  (`masks="joint"`, the default here: exact, including doubles pairing
  and team preview). `action_masks()` returns the masks, as sb3-contrib's
  `MaskablePPO` asks for them.
* Autoreset is Gymnasium 1.x's `NEXT_STEP`: the step that ends an episode
  returns its terminal observation; the next step starts the next episode
  (its action is ignored, its reward 0). The masks are honest there too:
  on the step that ends a battle, its mask row (in the observation and in
  `action_masks()`) is all False, since nothing can be chosen. A masked
  categorical built on such a row gives NaN logits, so replace the row
  (for example with any one action allowed) before sampling.
* `info` holds `winner`, `end_reason`, `status` and `episode_id`
  (the episode number, `pikapro.rl.Step.episode`); no key clashes with
  the `"episode"` statistics Gymnasium's `RecordEpisodeStatistics`
  adds. A vector env's infos are copied arrays with Gymnasium's `_key`
  masks.
* Episode `e` after `reset(seed=s)` plays with a battle seed hashed
  from `(s, e)`, so envs seeded `s` and `s + 1` (as `SyncVectorEnv`
  and Stable-Baselines3 seed their sub-envs) play unrelated battles.

### class `pikapro.rl.gym.GymEnv`

```python
class GymEnv(format='championsvgc', **kwargs)  # bases: Env
```

One battle of `format` as a single-agent `gymnasium.Env` (a
`pikapro.rl.VecEnv` of one battle, one thread): dict observations
`{"observation", "action_mask"}`, a `Discrete` action (the joint
choice index), and `action_masks()` for `MaskablePPO`. Keywords go
to `pikapro.rl.VecEnv`.

#### `GymEnv.action_masks()`

`(n_actions,)` bool: the actions allowed now (for
`MaskablePPO`).

#### `GymEnv.metadata`

*attribute, default `{'render_modes': []}`*

#### `GymEnv.reset(*, seed=None, options=None)`

Starts the next episode (`seed`: episodes renumbered from 0
with that base seed; without one, the next episode of the base
seed in use: the constructor's `seed=`, 0 by default, until a
seeded reset replaces it). Returns `(observation, info)`.

#### `GymEnv.set_opponent(opponent)`

Changes player 1's policy from the next episode on (see
`pikapro.rl.VecEnv.set_opponent`): a built-in policy or a
mixture of them (`"external"` raises `ValueError`).

#### `GymEnv.step(action)`

Plays one joint choice index. Returns `(observation, reward,
terminated, truncated, info)`; `info` has `winner`,
`end_reason`, `status` and `episode_id`.

### class `pikapro.rl.gym.GymVectorEnv`

```python
class GymVectorEnv(format, num_envs=1, *, copy=True, **kwargs)  # bases: VectorEnv
```

`num_envs` battles of `format` as a `gymnasium.vector.VectorEnv`
(see the module docs). `copy` (default True) returns copies of the
environment's buffers; `copy=False` returns views, overwritten by the
next step (the fast path). Other keywords go to `pikapro.rl.VecEnv`
(`teams`, `opponent`, `reward`, `observation`, ...).

#### `GymVectorEnv.action_masks()`

`(num_envs, n_actions)` bool: each battle's allowed actions.

#### `GymVectorEnv.close_extras(**kwargs)`

Closes the underlying `pikapro.rl.VecEnv`.

#### `GymVectorEnv.reset(*, seed=None, options=None)`

Starts a new episode in every battle (`seed`: episodes
renumbered from 0 with that base seed; without one, each battle's
next episode of the base seed in use: the constructor's `seed=`
until a seeded reset replaces it). The battles share one base
seed, so of Gymnasium's per-environment lists only the one an int
`s` stands for is taken (`[s, s + 1, ...]`, or every entry
`None`); another raises `ValueError`. Returns `(observations,
infos)`.

#### `GymVectorEnv.set_opponent(opponent, which=None)`

Changes player 1's policy from each battle's next episode on
(see `pikapro.rl.VecEnv.set_opponent`): a built-in policy or
a mixture of them (`"external"` raises `ValueError`).

#### `GymVectorEnv.step(actions)`

One step of every battle: `actions` is one joint choice
index per battle. Returns `(observations, rewards, terminations,
truncations, infos)`; `infos` has `winner`, `end_reason`,
`status` and `episode_id` per battle (copies, with `_key`
masks).

### `pikapro.rl.gym.observation_space(env)`

The Gymnasium space of one player's observation of `env` (a
`pikapro.rl.VecEnv`): `Dict(observation, action_mask)`.

## pikapro.rl.pettingzoo

A PettingZoo `ParallelEnv` (`pip install 'pikapro[pettingzoo]'`): one
battle, agents `"p1"` and `"p2"` (players 0 and 1 of a
`pikapro.rl.VecEnv` with `opponent="external"`; with
`side_randomization` they may play either side).

Each agent observes `{"observation": ..., "action_mask": ...}` (its own
ego-centric view) and acts with one `Discrete` index, the joint choice
index. Every step takes an action for every live agent; an agent that
isn't asked anything (the other side's forced switch, say) has an all-False
mask and its action is ignored. When the battle ends, both agents are done
and `agents` empties; `reset()` starts the next battle.

### class `pikapro.rl.pettingzoo.ParallelEnv`

```python
class ParallelEnv(format='championsvgc', **kwargs)  # bases: ParallelEnv
```

One battle of `format` for two agents (see the module docs).
Keywords go to `pikapro.rl.VecEnv` (`teams`, `reward`,
`observation`, `max_turns`, ...; `masks` must be `"joint"` and
`autoreset` `"next_step"`).

#### `ParallelEnv.action_masks(agent)`

`(n_actions,)` bool: the actions `agent` may take now (all
False when it isn't asked).

#### `ParallelEnv.action_space(agent)`

The action space of `agent` (the same for both):
`Discrete` joint choice indices.

#### `ParallelEnv.close()`

Closes the underlying `pikapro.rl.VecEnv`.

#### `ParallelEnv.metadata`

*attribute, default `{'name': 'pikapro_v0', 'render_modes': [], 'is_parallelizable': True}`*

#### `ParallelEnv.observation_space(agent)`

The observation space of `agent` (the same for both):
`Dict(observation, action_mask)`.

#### `ParallelEnv.render()`

Does nothing: there is no rendering (`pikapro.Battle.log` is
the text log).

#### `ParallelEnv.reset(seed=None, options=None)`

Starts the next battle (`seed`: episodes renumbered from 0
with that base seed; without one, the next episode of the base
seed in use: the constructor's `seed=`, 0 by default, until a
seeded reset replaces it). Returns `(observations, infos)` by
agent.

#### `ParallelEnv.state()`

Not available: the battle's full state is hidden information.
Raises `NotImplementedError`.

#### `ParallelEnv.step(actions)`

One step: `actions` maps each live agent to a joint choice
index (ignored for an agent that isn't asked). Returns
`(observations, rewards, terminations, truncations, infos)` by
agent; when the battle ends, `infos` has `winner` and
`end_reason` and `agents` empties.

## pikapro.rl.sb3

A Stable-Baselines3 `VecEnv` (`pip install 'pikapro[sb3]'`) over
`pikapro.rl.VecEnv`, for PPO and sb3-contrib's `MaskablePPO`.

SB3 resets an ended environment within the same step and passes the
episode's last observation in `infos[i]["terminal_observation"]`, so the
environment runs with `autoreset="same_step"`. The agent is player 0
against a built-in opponent (changeable with `SB3VecEnv.set_opponent`,
also through `env_method("set_opponent", ...)`); `"external"` raises
`ValueError` wherever it is given, as nothing would play player 1 (for
self-play, use `pikapro.rl.pettingzoo` or `pikapro.rl.VecEnv`). Observations are the
`basic` float vector (`MlpPolicy`); the action is the joint choice
index; `action_masks()` gives MaskablePPO the masks:

```python
from sb3_contrib import MaskablePPO
env = SB3VecEnv("championsvgc", num_envs=64, opponent="simple_heuristics")
model = MaskablePPO("MlpPolicy", env).learn(200_000)
```

### class `pikapro.rl.sb3.SB3VecEnv`

```python
class SB3VecEnv(format='championsvgc', num_envs=1, **kwargs)  # bases: VecEnv
```

`num_envs` battles of `format` as an SB3 `VecEnv` (see the
module docs). Keywords go to `pikapro.rl.VecEnv`.

#### `SB3VecEnv.action_masks()`

`(num_envs, n_actions)` bool.

#### `SB3VecEnv.close()`

Closes the underlying `pikapro.rl.VecEnv`.

#### `SB3VecEnv.env_is_wrapped(wrapper_class, indices=None)`

SB3's `env_is_wrapped`: always False (the battles aren't
Gymnasium environments).

#### `SB3VecEnv.env_method(method_name, *method_args, indices=None, **method_kwargs)`

SB3's `env_method`. `"action_masks"` returns each battle's
mask and `"set_opponent"` changes the given battles' opponent;
any other method of this object is called once.

#### `SB3VecEnv.get_attr(attr_name, indices=None)`

SB3's `get_attr`: the attribute of this object, once per
index.

#### `SB3VecEnv.reset()`

Starts a new episode in every battle (with the seed
`seed` set: episodes renumbered from 0 with that base seed;
else each battle's next episode of the base seed in use: the
constructor's `seed=` until a seeded reset replaces it) and
returns player 0's observations.

#### `SB3VecEnv.seed(seed=None)`

Sets the base seed the next `reset` uses (episodes
renumbered from 0); `None` unsets it.

#### `SB3VecEnv.set_attr(attr_name, value, indices=None)`

SB3's `set_attr`: sets the attribute on this object.

#### `SB3VecEnv.set_opponent(opponent, which=None)`

Changes player 1's policy from each battle's next episode on
(see `pikapro.rl.VecEnv.set_opponent`): a built-in policy or
a mixture of them (`"external"` raises `ValueError`).

#### `SB3VecEnv.step_async(actions)`

Stores the actions (one joint choice index per battle) for
`step_wait`.

#### `SB3VecEnv.step_wait()`

Steps every battle. Returns `(observations, rewards, dones,
infos)`; an ended battle has already started its next episode,
and its `infos[i]` has `terminal_observation`,
`TimeLimit.truncated`, `winner` and `end_reason`. A battle
that failed (`end_reason` 5, a zeroed `terminal_observation`)
has started its next episode too.

## pikapro.compat.poke_env

poke-env compatibility: poke-env's own battle trackers and unmodified
poke-env players on `pikapro.Battle`, in-process, with no server.

poke-env is an optional dependency (`pip install 'pikapro[poke-env]'`); it
is imported when a function here first needs it.

* `Feeder` keeps a poke-env `Battle`/`DoubleBattle` in sync with
  one side, from that side's channel lines and requests, through poke-env's
  own parsing.
* `play` and `cross_evaluate` let poke-env `Player` objects
  play each other; `make_player` creates one for a pikapro format.
* `order_to_index`, `index_to_order`,
  `action_to_index` and `index_to_action` convert between
  poke-env's orders, `SinglesEnv`/`DoublesEnv` actions and pikapro's
  `pikapro.ActionSpace` indices, through Showdown choice strings.
* `snapshot` is poke-env's tracked state as plain JSON
  (`tools/view-oracle/SCHEMA.md`).

Formats map to the Showdown ids poke-env players use (`SHOWDOWN_FORMATS`).

### `pikapro.compat.poke_env.SHOWDOWN_FORMATS`

`dict`: `<dict of 8>`

### `pikapro.compat.poke_env.UNKNOWN`

`str`: `'?'`

### `pikapro.compat.poke_env.action_to_index(action, battle, pikapro_battle, side, *, fake=False, strict=True)`

A poke-env `SinglesEnv`/`DoublesEnv` action (an int, or a pair
in doubles) to a `pikapro.ActionSpace` index: the env's own
`action_to_order` on the poke-env `battle`, then
`order_to_index` on `pikapro_battle`.

### class `pikapro.compat.poke_env.BattleRecord`

```python
class BattleRecord(index, seed, teams, winner=None, turns=0, reason=None, invalid=<factory>, unavailable=<factory>, invalid_messages=<factory>, quirks=<factory>, fallbacks=<factory>, stalls=0, decisions=<factory>, warnings=<factory>, error=None, error_line=None)
```

One battle the driver played.

#### `BattleRecord.error`

*attribute, default `None`*

#### `BattleRecord.error_line`

*attribute, default `None`*

#### `BattleRecord.reason`

*attribute, default `None`*

#### `BattleRecord.stalls`

*attribute, default `0`*

#### `BattleRecord.turns`

*attribute, default `0`*

#### `BattleRecord.winner`

*attribute, default `None`*

### `pikapro.compat.poke_env.choice_text(message)`

The Showdown choice in a poke-env message: `"/choose move 1"` ->
`"move 1"`, `"/team 1234"` -> `"team 1234"`. `None` for
`/forfeit`; `""` for no choice.

### `pikapro.compat.poke_env.choice_to_order(choice, battle)`

The poke-env order for a `pikapro.Choice`, with the moves and
Pokémon it names taken from the request the poke-env `battle` last
parsed. A team order is a `"/team 3142"` string, as poke-env's
`teampreview` returns.

### `pikapro.compat.poke_env.cross_evaluate(players, format, n_battles, *, seed=0, **kwargs)`

poke-env's `cross_evaluate`, in-process: every pair plays
`n_battles` (the first of the pair on side 1, seeds from `seed`),
and `results[a][b]` is `a`'s win rate against `b` (`None` on
the diagonal). Like poke-env, it resets the players' battles after
each pair. `kwargs` go to `play`. It keeps poke-env's
signature for poke-env players; the built-in policies' table, played
in Rust over mirrored games, is `pikapro.rl.eval.cross_evaluate`.

### class `pikapro.compat.poke_env.Decision`

```python
class Decision(maybe_default=False)
```

A request poke-env would answer now (`_handle_battle_request`).
`maybe_default`: it follows an `[Invalid choice]` error.

#### `Decision.maybe_default`

*attribute, default `False`*

### class `pikapro.compat.poke_env.Feeder`

```python
class Feeder(format, side, *, player=None, register=False, battle_tag=None, logger=None, save_replays=False, workarounds=True, username=None)
```

A poke-env battle object kept in sync with one side of a battle.

`format` is a pikapro format (`"championsvgc"`) or its Showdown id;
`side` is `0`/`"p1"` or `1`/`"p2"`. Pass `player` (a
poke-env `Player`) to take its settings (open team sheets, strict
tracking, its teambuilder's team as `teampreview_team`, doubles or
singles) and, with `register=True`, to add the battle to
`player.battles` so its win counters see it.

The battle knows its player by its name in the log: `username`, which
must be the name `pikapro.Battle` gives that side (`players=`);
by default the side's name (`p1` or `p2`), pikapro's default.

With `workarounds` (the default), lines Showdown sends but poke-env
misreads are rewritten first to an equivalent form poke-env reads
(`workaround`; counted in `quirks`).

Feed it with `receive`, the lines the side's client gets in the
order it gets them (a channel's log lines and `|request|<json>`
lines; `receive_request` wraps the latter). After a request that
needs an answer, `pending` is a `Decision` and
`decide` asks the player for the choice message.

#### `Feeder.decide(player=None)`

Asks `player` (default: the feeder's) for the pending decision,
as `Player._handle_battle_request` does, and returns the message
it would send (`"/choose move 1, switch 3"`, `"/team 1234"`,
`"/forfeit"`; `""` for no message). Clears `pending`.

#### `Feeder.receive(lines)`

Feeds lines, in order, as one client message's body (the lines
after `>battle-...`). Returns `pending`.

#### `Feeder.receive_error(message)`

Feeds Showdown's `|error|` line for a rejected choice
(`message` is `"[Invalid choice] ..."` or
`"[Unavailable choice] ..."`).

#### `Feeder.receive_request(request)`

Feeds a `|request|` line: `request` is the JSON text (or a
dict, dumped as Showdown writes it).

### class `pikapro.compat.poke_env.FeederError`

```python
class FeederError(line, cause)  # bases: RuntimeError
```

poke-env raised while parsing a line. `line` is the line (or the
`|request|` line) it choked on; `__cause__` is poke-env's error.

### `pikapro.compat.poke_env.index_to_action(index, battle, space, *, fake=False, strict=True)`

A `pikapro.ActionSpace` index to a poke-env `SinglesEnv` action
(an int) or `DoublesEnv` action (an int64 array of two), through
`index_to_order` and the env's own `order_to_action`.

### `pikapro.compat.poke_env.index_to_order(index, battle, space)`

The poke-env order for `pikapro.ActionSpace` index `index`
(`space`: the format's space, or a `pikapro.Battle`), for the
poke-env `battle` that holds the request it answers.

### `pikapro.compat.poke_env.make_player(format, **kwargs)`

Creates a poke-env player of class `cls` for a pikapro format, ready
for the in-process driver: `battle_format` is the format's Showdown
id, and it doesn't connect to a server (`start_listening=False`).

### `pikapro.compat.poke_env.order_to_index(order, battle, side)`

The `pikapro.ActionSpace` index of a poke-env order (a
`BattleOrder` or its message, `"/choose move thunderbolt 1"`,
`"/team 3142"`), read against `battle` (a `pikapro.Battle`
waiting for `side`'s choice) as Showdown would read it.
Raises `ValueError` if Showdown would reject it.

### `pikapro.compat.poke_env.pikapro_format(format)`

The pikapro format for a pikapro or Showdown format id.

### `pikapro.compat.poke_env.play(p1, p2, format, n_battles=1, *, teams=None, seed=0, team_mode='sets', register=True, max_retries=10, raise_errors=False, python_seed=None, on_battle=None)`

Plays `n_battles` between two poke-env players, `p1` on side 1.

Battle `i` uses pikapro seed `seed + i` and teams from `teams`:

* `None`: each player's teambuilder (`player.next_team`) if it has
  one, else a random legal team, `pikapro.RandomTeams(format,
  team_mode).pair(seed + i)`;
* a pair of team texts (export or packed), or a list of pairs (cycled);
* a `pikapro.RandomTeams`: its `pair(seed + i)`;
* a callable `(i, seed) -> pair`.

`register` adds each battle to both players' `battles` (so their
`n_won_battles` and `win_rate` count it). A rejected choice goes
back to the player as Showdown's `|error|` line; after
`max_retries` rejections in a row the driver chooses `default`
for it (a *fallback*). A poke-env exception ends that battle with an
error in its record, or is raised with `raise_errors`.
`python_seed` seeds Python's `random`, which poke-env's players
draw from. Returns `PlayResults`.

### `pikapro.compat.poke_env.play_parallel(p1, p2, format, n_battles, *, processes=4, seed=0, player_kwargs=(None, None), python_seed=None, **kwargs)`

`play` split over `processes` worker processes (spawned, so
poke-env's event-loop thread isn't forked). `p1` and `p2` are
player *classes*; each worker creates its own pair with
`make_player` (`player_kwargs` per side). Worker `k` plays a
contiguous block of the battles, with the same seeds `play`
would use, and Python's `random` seeded with `python_seed + k`
when `python_seed` is given. `teams` must be picklable (`None`,
team texts); other `kwargs` go to `play`. The records come
back in battle order; `seconds` is the wall time. As with any
spawned `multiprocessing` pool, a script must call it under
`if __name__ == "__main__":`.

### class `pikapro.compat.poke_env.PlayResults`

```python
class PlayResults(format, players, battles, seconds)
```

What `play` returns.

#### `PlayResults.battles_per_second`

*property*

Battles per second of wall time.

#### `PlayResults.errors`

*property*

The battles that a feeder or player exception ended early.

#### `PlayResults.fallbacks`

*property*

Times the driver chose `default` for a player, every battle.

#### `PlayResults.invalid`

*property*

`[Invalid choice]` rejections, both sides, every battle.

#### `PlayResults.n`

*property*

The number of battles played.

#### `PlayResults.quirks`

*property*

poke-env quirks worked around, by name, totalled over the
battles.

#### `PlayResults.stalls`

*property*

Decision rounds where no player answered a request that needed
one.

#### `PlayResults.summary()`

The totals as a plain dict (for printing or logging).

#### `PlayResults.ties`

*property*

Battles that ended without a winner (and without an error).

#### `PlayResults.turns`

*property*

Turns played, every battle.

#### `PlayResults.unavailable`

*property*

`[Unavailable choice]` rejections, both sides, every battle.

#### `PlayResults.wins`

*property*

Wins per side: `[side 1's, side 2's]`.

### `pikapro.compat.poke_env.require_poke_env()`

Imports poke-env, or raises an ImportError that says how to get it.

### `pikapro.compat.poke_env.showdown_format(format)`

The Showdown format id poke-env players use for a pikapro format
(`"championsvgc"` -> `"gen9championsvgc2026regmc"`). A Showdown id
is returned unchanged.

### `pikapro.compat.poke_env.snapshot(battle)`

A poke-env `Battle` or `DoubleBattle`'s tracked state as plain
JSON (see `tools/view-oracle/SCHEMA.md`).

## pikapro.showdown

pikapro.showdown: play on Showdown-protocol servers.

A websocket client for Showdown's server protocol (`PROTOCOL.md`) that
carries battles between a server and a battle handler: log in, search the
ladder, challenge and accept, and play each battle room. With
`SessionHandler`, an agent trained in `pikapro.rl.VecEnv` plays
on a server with the same observations, masks and actions: the room's
lines go through `pikapro.rl.ChannelSession`.

It connects to a local server (Showdown's own or
pikapro-showdown-server), a server you run, and VGC Arena
(`server="arena"`, a bot ladder that starts battles itself:
`ARENA`). It needs the `websockets` package
(`pip install 'pikapro[showdown]'`).

Best-of series (`gen9championsvgc2026regmcbo3`) play through Showdown's
series room: the client follows each game's battle room, answers the
"ready for the next game" prompt, and returns one `SeriesResult`
per series; `SessionHandler` plays each game with the series'
score and the earlier games' records, as `VecEnv`'s series mode does.

Quick start:

```python
import asyncio
from pikapro.showdown import ShowdownClient, SessionHandler

async def main():
    handler = SessionHandler("simple_heuristics")
    async with ShowdownClient("my-bot", server="local", handler=handler) as client:
        results = await client.ladder(5, "gen9championsvgc2026regmc", team=open("team.txt").read())
    print(sum(r.won is True for r in results), "wins")

asyncio.run(main())
```

From the shell: `python -m pikapro.showdown ladder --format ... --team team.txt`
(`python -m pikapro.showdown --help`). The guide is `docs/python/showdown.md`.

### `pikapro.showdown.ARENA`

`ServerConfig`: `<ServerConfig>`

### `pikapro.showdown.LOCAL`

`ServerConfig`: `<ServerConfig>`

### `pikapro.showdown.PIKAPRO`

`ServerConfig`: `<ServerConfig>`

### `pikapro.showdown.PIKAPRO_FORMAT_ALIASES`

`tuple`: `(('gen9customgame', 'gen9singles'), ('gen9doublescustomgame', 'gen9doubles'))`

### class `pikapro.showdown.AcceptPlan`

```python
class AcceptPlan(team=None, open_team_sheets=None, failed=None)
```

How `ShowdownClient.accept_challenges` accepts one challenge,
as its `plan` hook decides it (e.g. a team a user sent for their own
game).

#### `AcceptPlan.failed`

*attribute, default `None`*

#### `AcceptPlan.open_team_sheets`

*attribute, default `None`*

#### `AcceptPlan.team`

*attribute, default `None`*

### class `pikapro.showdown.BattleHandler`

```python
class BattleHandler()
```

Plays a client's battles. Subclass it and override `feed` and
`choose`; the other methods have defaults.

The client calls, for each battle room, `open` once, then
`feed` with every frame's lines, and `choose` (batched
over the rooms waiting) when the room has a request to answer and
`feed` said the handler can act. `error` sees each
`|error|` line, and `close` is called when the room ends or is
left. A handler keeps per-battle state in `BattleRoom.state`,
not by `BattleRoom.id`: Showdown renames a battle room when a
player changes its privacy, and the room (the same object) goes on
under its new id.

A failure stays in its battle: if `open` or `feed` raises,
the client forfeits that battle; if `choose` raises or doesn't
return one entry per room, the client asks again room by room, and
sends `default` for a room whose own call fails. Before starting a
game, the client asks `supports`.

#### `BattleHandler.choose(rooms)`

The choices for `rooms`, one per room, in order: each the
text after `/choose ` (`"move 1 2, switch 3"`, `"team 1234"`;
one line), or `None` to leave that room's request unanswered for
now (it is asked again when the room sends something new).

#### `BattleHandler.close(room)`

The room ended or was left; drop its state.

#### `BattleHandler.error(room, message)`

An `|error|` line (`[Invalid choice] ...`,
`[Unavailable choice] ...`): the client asks again after an
invalid choice, and waits for Showdown's updated request after an
unavailable one.

#### `BattleHandler.feed(room, lines)`

New lines of the room (a whole frame, `|request|` and
`|error|` lines included). Returns whether the handler can
choose now (the client asks only when a request is unanswered).

#### `BattleHandler.max_batch`

*attribute, default `0`*

#### `BattleHandler.open(room)`

A battle room started (or was joined again after a reconnect,
with its log starting over). In a best-of series,
`BattleRoom.series` is the series (a
`pikapro.showdown.SeriesRoom`, whose `state` is the
handler's for the whole series) and `BattleRoom.game` the
game's number.

#### `BattleHandler.series_end(series)`

A best-of series ended (`series`: its
`pikapro.showdown.SeriesRoom`), after its games' rooms
closed; settle what the handler keeps for the series.

#### `BattleHandler.summary(room)`

Extra facts for the battle's `pikapro.showdown.BattleResult`
(`extras`).

#### `BattleHandler.supports(format)`

Whether the handler can play battles in `format` (a Showdown
format id). The client refuses to search or challenge in a format
the handler can't play, and never accepts a challenge in one. The
default: every format.

### class `pikapro.showdown.BattleResult`

```python
class BattleResult(room, format, opponent, won, winner, tie, finished, turns, side, invalid, unavailable, fallbacks, log_path=None, reason=None, extras=<factory>, series=None, game=None, stale=0)
```

How one battle ended, from the client's player's side.

#### `BattleResult.game`

*attribute, default `None`*

#### `BattleResult.log_path`

*attribute, default `None`*

#### `BattleResult.reason`

*attribute, default `None`*

#### `BattleResult.series`

*attribute, default `None`*

#### `BattleResult.stale`

*attribute, default `0`*

#### `BattleResult.to_dict()`

The result as a JSON-ready dict.

### class `pikapro.showdown.BattleRoom`

```python
class BattleRoom(id, username)
```

One battle room the client plays in.

Handlers read it and keep their per-battle state in `state`.
Attributes:

* `id` - the room id; `format` - the format id (from the room id);
  `username` - the player's name when the room opened.
* `lines` - every line the room sent, in order (the saved log; at
  most `MAX_LINES`).
* `side` - `"p1"` / `"p2"` once the room's `|player|` lines
  name the client; `opponent` - the opponent's name (`|player|` or
  `|title|`).
* `request` - the latest request (the parsed `|request|` JSON),
  `None` before one; `rqid` - its `rqid` if the server sends
  them; `requests` - requests received (new ones, or one sent again).
* `turn` - the last `|turn|`.
* `ended`, `winner`, `tie` - set by `|win|` / `|tie|`.
* `invalid`, `unavailable` - the choice errors the room sent;
  `stale` - the choices it rejected as stale (for a request already
  answered or gone; not in `invalid`); `fallbacks` - `default`
  choices the client sent after repeated invalid ones.
* `state` - the handler's.
* `ots_offered` - the room offered Open Team Sheets
  (`|uhtml|otsrequest|`); `ots_foe` / `ots_answer` - the
  opponent's answer and the client's (`True` accepted, `False`
  rejected, `None` none yet; the client's as sent, or as the room
  announced it);
  `ots_open` - the sheets were shown (`|showteam|`);
  `ots_wait_ms` - how long the client held the team-preview decision
  for the opponent's answer (`open_team_sheets="mutual"`), else
  `None`.

#### `BattleRoom.observe(line, user_id)`

Updates the room's own bookkeeping from one line.

#### `BattleRoom.opened`

*property*

When the client first read the room (`time.monotonic()`; kept
when a reconnect sends it again); 0 for a room never read.

#### `BattleRoom.pending`

*property*

A request (or a retry after an invalid choice) is unanswered.

#### `BattleRoom.play_format`

*property*

The format the handler plays: `format`, or what the
server's `ServerConfig.format_aliases` maps it to
(pikapro-showdown-server's custom games are pikapro's
`gen9singles` / `gen9doubles`).

#### `BattleRoom.result(user_id, log_path=None, reason=None)`

The room's `BattleResult`.

### class `pikapro.showdown.ChallengeError`

```python
class ChallengeError(...)  # bases: ShowdownError
```

A challenge was rejected or cancelled, or no battle followed it.

### class `pikapro.showdown.Format`

```python
class Format(id, name, section, random_teams, ladder, challenge, tournament, level_50, best_of)
```

One entry of a server's `|formats|` list.

### `pikapro.showdown.format_key(text)`

A format as the client keys it: the format id, and a custom game's
rules after `@@@` as given (trimmed, comma-separated), which
`/challenge` takes as they are (`"gen9vgc2025regi@@@Best of = 3"`).

### class `pikapro.showdown.LoginError`

```python
class LoginError(...)  # bases: ShowdownError
```

Logging in failed: a token refused, a name in use
(`|nametaken|`).

### `pikapro.showdown.pack_team(team, format_id='')`

A team as `/utm` wants it: packed, or `"null"` for none.

An export-format team (it has a newline) is packed with `pikapro.pack`;
a packed team is kept as it is. `format_id` (a Showdown or pikapro format
id) picks the dex: Champions formats pack with the Champions dex.

### `pikapro.showdown.parse_formats(line)`

The `|formats|` line, by format id.

Showdown's `formatListText` (`server/rooms.ts`): sections are
`|,COLUMN|SECTION`, and each format is `NAME,FLAGS` where `FLAGS`
is a hexadecimal bit set (1 random teams, 2 ladder, 4 challenge, 8
tournaments, 16 level 50, 64 best-of). The legacy suffixes (`,#`
random teams, `,,` search only, `,` challenge only) are read too.

### `pikapro.showdown.pikapro_server(address=None)`

pikapro-showdown-server at `address` (a URL, `host:port` or a
port; `None`: `PIKAPRO`): a local-style server whose custom-game
ids are pikapro's presets.

### `pikapro.showdown.random_choice(request, rng)`

A uniformly random valid choice for a Showdown request (the JSON
of `|request|`), or `None` for a `wait` request.

Team preview picks a random order (`maxChosenTeamSize` of it in
VGC); a forced switch picks random benched Pokémon; a move request
picks, per active Pokémon, among its enabled moves (a random foe
target in doubles) and, unless trapped, its switches. No gimmicks.

### class `pikapro.showdown.RandomRequestHandler`

```python
class RandomRequestHandler(seed=None)  # bases: BattleHandler
```

Answers every request with `random_choice`, reading only the
request JSON. For testing the networking (it needs neither the engine
nor a session), and for formats pikapro doesn't simulate.

#### `RandomRequestHandler.choose(rooms)`

`random_choice` for each room's request.

#### `RandomRequestHandler.feed(room, lines)`

Can act on any request but a `wait`.

### `pikapro.showdown.series_length(format)`

The games in a best-of series of `format` (Showdown's `Best of =
N` rule), `None` for single games: a `…bo3` format
(`gen9championsvgc2026regmcbo3`: Showdown names them "(Bo3)"), or a
challenge's `Best of = N` custom rule.

### class `pikapro.showdown.SeriesResult`

```python
class SeriesResult(room, format, opponent, won, winner, tie, finished, best_of, wins, losses, ties, games, side, invalid, unavailable, fallbacks, turns, log_path=None, reason=None, extras=<factory>, stale=0)
```

How a best-of series ended, from the client's player's side (what
`ShowdownClient.ladder` and the others return in a series
format: one per series, with each game's `BattleResult`).

#### `SeriesResult.log_path`

*attribute, default `None`*

#### `SeriesResult.reason`

*attribute, default `None`*

#### `SeriesResult.stale`

*attribute, default `0`*

#### `SeriesResult.to_dict()`

The result as a JSON-ready dict (the games' too).

### class `pikapro.showdown.SeriesRoom`

```python
class SeriesRoom(id, username)
```

A best-of series the client plays: Showdown's series room
(`game-bestof3-FORMAT-NUM`, a `BestOfGame`), whose games are
battle rooms of their own (`battle-FORMAT-NUM`), one after the other,
between the same two players with the same teams on the same sides.

Handlers see it as `BattleRoom.series` in each game's room, and
keep their per-series state in `state` (`SessionHandler` keeps
the `pikapro.rl.ChannelSeries` there). Attributes:

* `id`, `format` (from the id), `best_of`; `opponent`.
* `games` - the battle room of each game, by number; `results` -
  each finished game's `BattleResult`, by number;
  `game_winners` - each game's winner as the series room announced it
  (`None`: a tie).
* `ended`, `winner`, `tie` - set by the series room's
  `|win|` / `|tie`.
* `lines` - the series room's lines (its log).

#### `SeriesRoom.observe(line, user_id)`

Reads one line of the series room: `("game", n, battle_room)`
when a game starts, `("ready", n)` when Showdown asks whether
the player is ready for game `n`, `("end",)` at the series'
end, `("closed", why)` when the room is gone, else `None`.

#### `SeriesRoom.result(user_id, log_path=None, reason=None)`

The series' `SeriesResult`.

### `pikapro.showdown.server_config(server)`

A `ServerConfig` from a name or an address.

`"arena"` is `ARENA` (VGC Arena; so is a URL on its host),
`"local"` or `None` is `LOCAL`, `"pikapro"` is
`PIKAPRO` (pikapro-showdown-server) and `"pikapro:<address>"` is
`pikapro_server` there; a `ws://`/`wss://` URL, `host:port` or
a port number is a server you run there (no login, no limits).
A URL with another scheme (`http://`) raises `ValueError`, and so
does a host on `psim.us` or `pokemonshowdown.com`.

### class `pikapro.showdown.ServerConfig`

```python
class ServerConfig(websocket_url, throttle=0.0, challenge_interval=0.0, max_games=0, format_aliases=(), series=True, token_login=False, credentials_env=None, commands=None, auto_battles=False, format_suffixes=(), final_close_codes=())
```

Where a client connects, and the limits it keeps there.

#### `ServerConfig.auto_battles`

*attribute, default `False`*

#### `ServerConfig.challenge_interval`

*attribute, default `0.0`*

#### `ServerConfig.commands`

*attribute, default `None`*

#### `ServerConfig.credentials_env`

*attribute, default `None`*

#### `ServerConfig.final_close_codes`

*attribute, default `()`*

#### `ServerConfig.format_aliases`

*attribute, default `()`*

#### `ServerConfig.format_suffixes`

*attribute, default `()`*

#### `ServerConfig.max_games`

*attribute, default `0`*

#### `ServerConfig.play_format(format_id)`

The format a battle in `format_id` is played as on this server
(what handlers and `ChannelSession` see): its alias
(`format_aliases`), else the id without one of
`format_suffixes` when only that is a format the engine
reads, else the id itself.

#### `ServerConfig.series`

*attribute, default `True`*

#### `ServerConfig.throttle`

*attribute, default `0.0`*

#### `ServerConfig.token_login`

*attribute, default `False`*

### class `pikapro.showdown.SessionHandler`

```python
class SessionHandler(policy='random', *, batch=False, max_batch=0, observation='basic', masks='factored', switch_by='row', threaded=None, seed=0, team=None, session_factory=None)  # bases: BattleHandler
```

Plays each battle through a `pikapro.rl.ChannelSession`.

The session reads the room's lines as the player's channel and gives
the environment's observation (`obs`) and mask for each decision;
the policy picks an action and the session turns it into the choice
text. So a policy trained in `pikapro.rl.VecEnv` plays on a
Showdown server.

`policy`:

* a built-in name (`BUILTIN_POLICIES`: `"random"`,
  `"max_base_power"`, `"simple_heuristics"`), played by the
  session (`choose_builtin`);
* a callable `policy(obs, mask) -> action`, called once per decision;
* with `batch=True`, a callable `policy(obs_batch, mask_batch) ->
  actions` called once for all the battles waiting at that moment:
  observations and masks are stacked on a new axis 0 (dicts and tuples
  of arrays item by item), and `actions[i]` is battle `i`'s.

`observation`, `masks` and `switch_by` configure the session as
they configure `VecEnv`. `threaded` runs the policy call in a
worker thread (the default for callables) so the connection keeps
answering pings while a model thinks. `seed` seeds the built-in
policies (decision `k` uses `seed + k`). `team` is the team the
client plays (as it sends it: export or packed format), for the
sessions' own rows (natures, the Pokémon left out at team preview), as
`VecEnv` has them; by default each battle's session gets the team
this client uploaded for it (`BattleRoom.team`), so only a
battle the client didn't start (an unsolicited challenge accepted by
another client) needs it. `session_factory` replaces
`ChannelSession` (tests).

A best-of series (`…bo3`) is played as `pikapro.rl.VecEnv`'s
`series` mode plays one: the handler keeps a
`pikapro.rl.ChannelSeries` per series (in
`SeriesRoom.state`), makes each game's session with it (the
score, the game, the earlier games' public records), and records each
game's session into it when the game ends. A game's result's
`extras` has the session's `series_row` (the environment's
`series` row for this player). A series the server ends early (a
forfeit in the series room, a game lost by disconnection) is settled
as the series room says: the `ChannelSeries` (`series_end`)
and a game's session still open (its `series_row`).

#### `SessionHandler.choose(rooms)`

The policy's actions for the rooms' current decisions (one call
for all of them with `batch=True`), as the sessions' choices.

Each room stands alone: a room with nothing to decide any more (it
ended, or a newer request came while the policy ran) gets `None`,
and a room whose session refuses the action (a masked action, a
wrong length) or whose own policy call fails gets `"default"`. A
batched policy call that fails raises, and the client asks again
room by room.

#### `SessionHandler.close(room)`

Drops the room's session (a series' game first records it in its
series; a room closed before its end, as on a rejoin, records
nothing).

#### `SessionHandler.feed(room, lines)`

Feeds the session; it can act when it `needs_action`.

#### `SessionHandler.open(room)`

Starts the room's `ChannelSession` (in `BattleRoom.state`),
in a series with the series' `pikapro.rl.ChannelSeries`.
Raises `ValueError` for a format the session can't read.

#### `SessionHandler.series_end(series)`

Settles the series' `ChannelSeries` as the series room ended it
(the client calls it when the series ends): its games' results may
not have (a forfeit in the series room, a game lost by
disconnection).

#### `SessionHandler.summary(room)`

`unknown_lines` (how many lines the session kept as unread),
`unknown_sample` (the first five), `session_winner` and, in a
series, `series_row`.

#### `SessionHandler.supports(format)`

Whether a `ChannelSession` reads `format` (the formats in
`docs/python/showdown.md`; any, with a `session_factory`).

### class `pikapro.showdown.ShowdownClient`

```python
class ShowdownClient(username=None, token=None, *, server='local', handler=None, avatar=None, guest=False, max_concurrent=1, log_dir=None, timer=False, forfeit_on_exit=True, reconnect=True, max_reconnects=8, max_retries=3, open_timeout=20.0, login_timeout=60.0, ping_interval=20.0, log=None, open_team_sheets=None, hide_battles=False, ots_wait=20.0, leave_delay=0.0)
```

A player on a Showdown server: it logs in, starts games (ladder,
challenges), and plays each battle room with a `BattleHandler`.

```python
async with ShowdownClient("my-bot", server="local", handler=SessionHandler("simple_heuristics")) as c:
    results = await c.ladder(10, "gen9championsvgc2026regmc", team=open("team.txt").read())
```

A local server takes a name and no login. Without a `username` the
client takes a random `pikapro-…` name (`guest=True` stays a guest
instead, able to read the server but not to battle). VGC Arena logs in
with a bot's name and `token`, by default `ARENA_BOT_NAME` and
`ARENA_BOT_TOKEN`; the token is never logged.

* `server`: a `ServerConfig` or a name for
  `server_config` (`"local"`, `"arena"`, a URL, `host:port`).
* `max_concurrent`: battles at once, all operations together (capped
  at the server's `max_games`).
* `log_dir`: each room's lines are saved there as `ROOM.log`.
* `timer`: turns the battle timer on in each room.
* `forfeit_on_exit`: `close` forfeits unfinished battles.
* `leave_delay`: seconds the client stays in a finished battle room
  (to read a rated game's rating lines).
* `reconnect`: reconnects after a dropped connection (exponential
  backoff, from one second to a minute, which starts over once a
  connection has stayed logged in for 30 seconds; gives up after
  `max_reconnects` failed attempts in a row) and rejoins the battles.
* `login_timeout`: a connection not logged in that many seconds
  after it opened is closed; `connect` raises
  `LoginError`, and a reconnect counts a failed attempt.
* `max_retries`: after that many invalid choices in a row for one
  request the client sends `default` for the handler.
* `open_team_sheets`: the answer to an Open Team Sheets offer at
  team preview. `None` (the default) accepts in a best-of series'
  games and leaves other offers alone, `True` accepts, `False`
  rejects, and `"mutual"` waits up to `ots_wait` seconds for the
  opponent's answer and matches it (rejecting when the wait ends),
  holding the room's team-preview decision meanwhile. The room
  records it (`BattleRoom.ots_*`).

A best-of series (a `…bo3` format, a challenge's `Best of = 3`
rule) is played as one: each game's room goes to the handler of the
operation that started the series (`BattleRoom.series`), and the
operation's result is a `SeriesResult` with every game's.

A subclass reads more of what the server sends, or holds decisions,
through hooks that do nothing by default: `on_battle_lines`,
`on_pm` and `hold_decision` (with `release_decision`);
`send_room` posts in a room.

A handler's failure is its battle's alone: an exception in its
`open` or `feed` forfeits that battle (its result's `reason`
says why), and one in `choose` sends `default` for the battles it
was asked about. If the connection ends for good (the client gave up
reconnecting, or a login failed), every battle ends as disconnected
and the operations waiting to start one raise the error.

#### `ShowdownClient.accept_challenges(from_user=None, n=1, team=None, *, format=None, handler=None, plan=None)`

Accepts `n` challenges (from `from_user` only, if given; in
`format` only, if given, its custom rules included; others are
left standing), at most `max_concurrent` battles at once, and
plays them. A challenge in a format the handler can't play
(`BattleHandler.supports`) is never accepted: it is left
standing. A challenge to a best-of series is played as one (its
result is a `SeriesResult`).

`plan(user_id, format)`, if given, is asked right before each
accept (a battle slot is free by then): an `AcceptPlan`
changes that battle's team and its answer to Open Team Sheets;
`None` accepts as usual (`team` is then asked for its team; a
plan's own team doesn't ask it).

On a server that starts battles itself
(`ServerConfig.auto_battles`, VGC Arena) nothing is
accepted: the team goes up with `/utm` (a rotation's next one
once a battle was played with it; again after each login), and
the next `n` battles the server puts the client in (in
`format`, or any) are played; `from_user` can't be honoured
there, and `plan` isn't asked (no challenge is accepted).

#### `ShowdownClient.auto_team`

*property*

The team (a fixed team or a rotation callable) and format of the
running or last `accept_challenges` on a server that starts
battles itself; `None` before one.

#### `ShowdownClient.cancel_search()`

Asks the server to start no new battle for the client
(`/cancelsearch`), now and again after each login (a login
undoes it). On VGC Arena this is how a bot stops without losing
anything: the battles in progress go on, and so does a return game
the bot owes; once nothing is left to play, the arena says so with
an `|updatesearch|` listing no games (`stopped`). On
another server it cancels the client's ladder searches.

#### `ShowdownClient.challenge(user, format, team=None, *, n=1, handler=None, accept_timeout=300.0)`

Challenges `user` to `n` games in `format`, one challenge
at a time (Showdown allows one outgoing challenge), at most
`max_concurrent` battles at once. `format` may carry a
custom game's rules after `@@@` (`"gen9vgc2025regi@@@Best of =
3"` challenges to a best-of-3 series). Returns the results as
`ladder` does. Raises `ChallengeError` if a challenge
is rejected, cancelled or not accepted within `accept_timeout`
seconds, and `ShowdownError` as `ladder` does.

#### `ShowdownClient.close()`

Leaves cleanly: cancels searches and challenges, forfeits (or
leaves) unfinished battles, waits briefly for them to end, and
closes the connection.

#### `ShowdownClient.close_unlisted(grace=120.0)`

On a server that starts battles itself (VGC Arena): ends each
battle room that its last `|updatesearch|` didn't list, and that
got no frame for `grace` seconds since: the battle ended while
the client couldn't see it (during a reconnect, whose rooms the
arena sends again only while they are in progress). Its result is
unfinished, with that reason (the arena's record of the battle
tells who won). Called every second while connected; `grace=0`
ends at once every room the last list left out (it listed no games
after `cancel_search`, say). Returns the rooms ended.

#### `ShowdownClient.connect()`

Connects and logs in (returns once logged in). Raises
`LoginError` or `ShowdownError` on failure (a
`LoginError` when not logged in within `login_timeout`
seconds). After the connection ended for good, connects again.

#### `ShowdownClient.connected`

*property*

The websocket is open and the client is logged in.

#### `ShowdownClient.hold_decision(room)`

Called when `room` has a request to decide, before the handler
is asked: `True` holds it (nothing is asked or sent) until
`release_decision`. By default, `False`; an exception counts
as `False`. (Open Team Sheets' wait, `open_team_sheets="mutual"`,
holds a decision before this is asked.)

#### `ShowdownClient.ladder(n, format, team=None, *, handler=None, search_timeout=None)`

Plays `n` ladder games in `format` (`/search`), at most
`max_concurrent` at once. `team`: an export or packed team,
`None` for a random-battle format, or `team(format) -> team`
called for each game. Returns the results in start order: a
`BattleResult` per game, or in a best-of format
(`…bo3`) a `SeriesResult` per series (`n` counts series).
Raises `ShowdownError` for a format the server doesn't
ladder or the handler can't play (`BattleHandler.supports`), or
when the connection ends for good.

#### `ShowdownClient.on_battle_lines(room_id, lines)`

Called with each frame of battle room `room_id`'s lines, after
the client read them (`rooms` `[room_id]` has them, while
the battle lasts). It also gets the frames of a room that ended or
that the client doesn't play: a rated battle's rating lines come
after its end, while the client is still in the room
(`leave_delay`). A room that waited to be claimed (a best-of
series' game, before its series room was read) comes again with no
lines when it is. A room Showdown renames (a player changed the
battle's privacy) comes once more under its old id, with the
`|noinit|rename|NEWID|TITLE` line; it is `rooms[NEWID]` from
then on.

#### `ShowdownClient.on_pm(line)`

Called with each `|pm|` line after the client read it: a
challenge's, a user's message, or a message of the client's that the
server refused.

#### `ShowdownClient.release_decision(room_id)`

Asks for the decision `hold_decision` held in `room_id`,
if the room still waits for one (`hold_decision` is asked
again first).

#### `ShowdownClient.send(text, room='')`

Sends one message (`text` to `room`), throttled with the
rest. Raises `ValueError` for a line break or another control
character (Showdown would run each line as its own command).

#### `ShowdownClient.send_room(room_id, text)`

Sends `text` in room `room_id` (battle chat, or a command
there), throttled with the rest, unless the client is closing or
has no connection: whether it was queued. Raises `ValueError` for
a line break or another control character, as `send` does.

#### `ShowdownClient.stopped`

*property*

After `cancel_search`: an `|updatesearch|` read since
`/cancelsearch` went out on this connection listed no games. The
server's list decides: a room of the client's it left out ended
unseen (`close_unlisted` with `grace=0` ends it).

#### `ShowdownClient.upload_team(team=None, format=None)`

Uploads a team now (`/utm`) on a server that starts battles
itself, even if it is the one up: `team` (a rotation callable
gives its next team), or by default the team and format of the
running `accept_challenges` (`auto_team`). The server
plays the team up when it pairs the next battle. With
`rotate_on_wait` `False` this is how a rotation moves on.
Errors as `accept_challenges`' upload (a rotation tries its
next teams).

#### `ShowdownClient.uploaded_team`

*property*

The team uploaded last (packed), on a server that starts battles
itself; `None` before the first upload.

#### `ShowdownClient.uploads`

*property*

The last uploads (at most `UTM_KEPT`), oldest first, each
with when it went out (`time.monotonic()`): with several battles
at once a battle may play any of the last few
(`BattleRoom.opened` says when its room opened).

#### `ShowdownClient.user_id`

*property*

The client's user id (`toID` of the name).

#### `ShowdownClient.wait_for_battles()`

Waits until every battle in progress has ended; returns their
results (battles cut off by the connection ending for good end as
disconnected). Cancelling it (a timeout) leaves the battles, and
the operations playing them, running. A series in progress is
waited for as one (its `SeriesResult`).

### class `pikapro.showdown.ShowdownError`

```python
class ShowdownError(...)  # bases: RuntimeError
```

The server refused something (its `|popup|` text), or the
connection failed.

Also here: `BUILTIN_POLICIES` (see `pikapro.rl`), `to_id` (see `pikapro`).
