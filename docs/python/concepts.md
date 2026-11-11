# Concepts

How `pikapro.rl` models a battle for a learning agent. The
[RL guide](../RL_GUIDE.md) lists every option.

## Players, steps and episodes

A `VecEnv` holds `n` battles. Each has two **players**: player 0 is the
agent, whose actions always come from you, and player 1 is its
opponent, a built-in policy or you again (`opponent="external"`). Every
per-player array has the battle on axis 0 and the player on axis 1.

A **step** is one request round of every battle. You submit the actions
of the players that were asked; the environment resolves the turn and
stops when a caller must act again. `out.needs_action[i, p]` says
whether player `p` of battle `i` must act next step. Actions of players
that aren't asked are ignored. `out.status[i, p]` says what became of an
action: `0` ignored, `1` submitted, `-1` rejected (outside the mask:
nothing happened, choose again), `2` unavailable (below).

An **episode** is one battle from team preview to the end. Episode `e`
is played by battle `e % n`, with a battle seed hashed from `(seed, e)`.
So the episodes don't depend on the number of threads, and base seeds
`s` and `s + 1` play unrelated episodes. `out.episode[i]` names the
episode `out.obs[i]` belongs to, and `env.record(i)["seed"]` gives its
battle seed.

## The player view

Observations come from each player's **view** of the battle: the
protocol log of its channel and its own requests, never the battle's
state, so they can't leak what the player doesn't know. So:

- the foe's HP is the percentage the client shows (`hp="exact"` opts into
  exact foe HP, which the custom-game formats show);
- a foe's item, ability, moves and Tera type are unknown until revealed
  (or shown by an open team sheet);
- Illusion fools the view as it fools a real client, until it breaks;
- own facts come from the request (exact HP, stats, PP, the moves the
  request disables, whether the Pokémon is trapped) and from the team
  the player built. A `ChannelSession` gets the team from its `team=`;
- the view is the battle as it stands; earlier turns are left to the
  model, as frame stacking.

A built-in opponent's observation rows are zeros unless you pass
`observe_builtin=True`.

## Honest masks

`out.masks[i, p]` marks the actions player `p` may choose. The mask is
built from what the player's request shows, not from the engine's
hidden state, which matters in two cases:

- A foe's unrevealed Shadow Tag, Arena Trap or Magnet Pull traps the
  active Pokémon, but the request only says `maybe_trapped`. Switches
  stay in the mask.
- A move disabled only by a foe's Imprison stays in the mask.

If the player picks such an action, the battle refuses it the way
Showdown does (`[Unavailable choice]`): the player gets status `2`, keeps
`needs_action`, its view and mask learn the trap or the disabled move,
and it chooses again next step while the other side's choice waits.

## Actions and rows

`masks=` picks the action layout:

- **`"factored"`** (the default of `VecEnv`): one action per active slot,
  `(n, 2, active)`. Each slot has `slot_size` actions: 15 in singles, 47
  in doubles. At team preview slot 0 holds the index of a team order.
  The mask is each slot's allowed actions followed by the team orders.
  It doesn't encode the pairing rules between the two slots: no two
  switches to one Pokémon, at most one gimmick, a forced switch that
  must use its replacement. `env.pair_mask(masks, min_switches, a0)`
  gives slot 1's actions that complete slot 0's `a0`. An impossible pair
  is rejected with status `-1`.
- **`"joint"`**: one choice index per player, `(n, 2)`, over every joint
  choice: 2,569 in VGC (2,209 move choices and 360 team orders), 15 in
  `gen9singles` and `gen9randombattle`, 21 in `gen9ou` (six leads),
  and 135 in `championsbss` (120 team orders). The mask is exact. The
  Gymnasium, PettingZoo and SB3 adapters
  use this layout.

`env.decode(i, player, actions)` returns the `pikapro.Choice` an action makes
and `env.describe(...)` its Showdown string (`move 1 2, switch 3`). The
indexing is `pikapro.ActionSpace` ([simulator API](simulator.md#action-space)),
and `pikapro.ACTION_SPACE_VERSION` versions it.

`rl.action_table(format)` describes every factored action index with
static integer columns (`kind`, `move_slot`, `target`, `gimmick`,
`switch`, `preview`, and the orders in `preview_orders`). With joint
masks, choice `j < joint_size` is `a0 * slot_size + a1`
(`ActionSpace.split`).

A doubles target code is 0 (none, or Showdown's random target), 1 and 2
the foes in slots 0 and 1, 3 and 4 the player's own slots 0 and 1.
Codes 3 and 4 are absolute, so one of them is the user itself;
`rl.relative_target(slot, code)` gives 3 the user, 4 its ally. A
team-preview choice is an ordered selection of 4 of 6 (360), because
the order sets the positions `switch i` refers to. In gen9ou, where the
whole team is brought, a team-preview choice names the lead (6 choices):
the others keep their team order.

**Rows.** After the player axis, each view is ego-centric: side 0 is the
player's own, side 1 the foe's. Own row `k` is the Pokémon registered
`k`-th in the team, for the whole battle. Foe rows follow the order of
team preview, or of first appearance without one. With `switch_by="row"`
(the default), switch action `k` sends in own row `k`.
`switch_by="position"` uses Showdown's current team position.

## Rewards and termination

Two presets, both zero-sum:

- `reward="terminal"` (the default): +1 win, −1 loss, 0 tie, when the
  episode ends.
- `reward="ko"`: the terminal reward plus `ko_weight` (0.1) per foe
  Pokémon knocked out and minus `ko_weight` per own one, computed from
  the Pokémon still standing. A bigger win is worth more, so it changes
  the objective; it isn't potential shaping.

`out.reward[i, p]` is what player `p` earned since its previous
decision, paid when the player must act again or when the episode ends.
Any other reward is yours to compute from the view.

`out.end_reason` says how an episode ended (`pikapro.rl.END_REASONS`
names the codes):

| `end_reason` | meaning | flag |
|---|---|---|
| 0 | not over | |
| 1 | a side has no Pokémon left | `terminated` |
| 2 | the format's turn limit (Showdown's tie at turn 1000) | `truncated` |
| 3 | forfeit | `terminated` |
| 4 | the environment's `max_turns` | `truncated` |
| 5 | an error: the battle failed | `truncated` |
| 6 | the Endless Battle Clause ended the battle (gen9ou: a win or a tie, [formats](../FORMATS.md#endless-battle-clause)) | `terminated` |

`out.winner` is `-1` for no result, `0` or `1` the winning player, `2` a
tie. A `max_turns` cut or an error has no result.

An error is a panic in the engine or in a Rust policy or encoder, or an
episode that can't start. Only that battle ends its episode, with reward
0 and a zeroed observation; the call doesn't raise, and
`env.take_errors()` returns `(battle, episode, message)`. A Rust program
built with `panic = "abort"` aborts instead.

## Autoreset

`autoreset=` sets what the next step does after a battle ends.

- **`"next_step"`** (the default; Gymnasium 1.x's `NEXT_STEP`): the step
  that ends an episode returns its terminal observation with
  `terminated` or `truncated`. The next step starts the next episode in
  that battle, ignores the battle's actions and pays reward 0.
- **`"same_step"`** (what Stable-Baselines3 expects): the next episode
  starts at once. `obs`, `masks`, `needs_action`, `agent_side` and
  `episode` describe the new episode; `reward`, `terminated`,
  `truncated`, `end_reason` and `winner` the one that ended, whose last
  observation is in `final_obs` (with `final_episode` and `final_turns`).

`SB3VecEnv` uses `same_step`; the Gymnasium adapters use `next_step`.

## Opponents and self-play

`opponent=` is one of:

- a built-in policy: `"random"`, `"max_base_power"` or
  `"simple_heuristics"`, poke-env's three baseline players ported to
  Rust, playing on their own view;
- `"external"`: you play player 1 too;
- a mixture drawn per episode: `{"random": 1, "simple_heuristics": 3}`.

`env.set_opponent(...)` changes it between calls, for every battle or
for `which=[...]`, from each battle's next episode on. A curriculum:

```python
import copy
import numpy as np
import pikapro.rl as rl

env = rl.VecEnv("championsvgc", n=64, opponent="simple_heuristics", observation="basic", masks="joint")
agent, frozen = Agent(), None           # Agent: yours; act(obs, masks) -> (n,) actions
out = env.reset(seed=0)
actions = np.full((env.n, 2), -1, dtype=np.int64)
for update in range(400):
    if update == 200:
        env.set_opponent("external")    # self-play from each battle's next episode
        frozen = copy.deepcopy(agent)
    actions[:, 0] = agent.act(out.obs[:, 0], out.masks[:, 0])
    if frozen is not None:              # its own ego-centric view
        actions[:, 1] = frozen.act(out.obs[:, 1], out.masks[:, 1])
    out = env.step(actions)
```

The Gymnasium and Stable-Baselines3 adapters step player 0 only, so they
refuse `"external"`; for self-play through an adapter, use the
PettingZoo `ParallelEnv`.

Both players' views are ego-centric, so the same network plays either
seat. `side_randomization=True` makes player 0 play p1 or p2 at random
(`out.agent_side`).

[`examples/cleanrl_ppo_selfplay.py`](../../crates/pikapro-py/examples/cleanrl_ppo_selfplay.py)
is a full self-play loop. `pikapro.rl.eval` measures a policy against
built-in ones or a frozen checkpoint.

## Team pools

`teams=` is where each episode's teams come from:

- `None` or `"sets"` (the default): random teams from Showdown's
  random-battle sets, fresh every episode; `"uniform"` draws every legal
  choice uniformly; a `pikapro.RandomTeams` gives more options (level, team
  size, restricted species, moves, items). Teams pass validation, except
  in the two random-battle formats, where `"sets"` is Showdown's
  random-battle team generator, ported exactly, which nothing validates.
- A pair of team texts (Showdown's export or packed format): fixed teams.
  A list of pairs: episode `e` plays pair `e % len`.
- A `TeamPool`: both players draw from it. Two pools: one per player.

A pool can come from a list of pokepaste links, one per line:

```python
pool = rl.teams.from_pokepaste_list("teams.txt", "~/.cache/pikapro-pastes", "championsvgc")
print(len(pool), "teams;", len(pool.rejected), "rejected")
for index, url, problems in pool.rejected:      # in Showdown's words
    print(url, problems)
env = rl.VecEnv("championsvgc", n=1024, teams=pool)
```

Each paste is fetched once into the cache directory you give, and later
loads read the cache (`fetch=False` forbids the network). Every team is
checked by the format's validator, and only valid teams are kept, in the
form the validator leaves them, which is what a Showdown server plays.
`rl.teams.from_export_texts` and `rl.teams.from_dir` build pools from
local texts.

## Open team sheets and best-of-3

Open sheets need a format with team preview, where Showdown shows
them: `ots` and `series` raise `ValueError` in `gen9singles` and
`gen9doubles`.

`ots=True` shows both players' sheets at team preview: species, items,
abilities, moves, levels, genders and Tera types (and natures in
Champions), but not EVs or IVs, as in VGC's open team sheets.
`ots=0.3` opens them in 30% of episodes. `ots_view="player0"` shows the
sheets to player 0 alone, to measure what a sheet is worth to a policy.

`series="bo3"` plays Showdown's best-of-3, and `"bo5"`, `"bo7"` and
`"bo9"` its longer best-ofs. Every game of a series has the same teams,
sides and opponent, with open sheets. `out.series[i]` is
`(game, player 0's wins, player 1's wins, result)`, in player 0's frame;
`BattleView.series` gives it from each player's side. `basic` doesn't
include the score. On a server, `SessionHandler`
carries a series from game to game
([showdown.md](showdown.md#best-of-series)).

## Determinism and replay

The same format, teams, seed and choices give the same battle, bit for
bit, on every platform and for any number of threads.

With `record=True`, `env.record(i)` returns battle `i`'s current episode:
its seed, both teams, `agent_side` and every choice in order.
`rl.replay(format, record)` plays it again through `pikapro.Battle` and
reaches the same battle, whose `log()` is Showdown's protocol for any
perspective:

```python
import numpy as np
import pikapro.rl as rl

env = rl.VecEnv("gen9singles", n=4, record=True, seed=7)
out = env.reset(seed=7)
rng = np.random.default_rng(0)
while not out.terminated[0] and not out.truncated[0]:
    out = env.step(env.sample_actions(rng))
battle = rl.replay("gen9singles", env.record(0))
print(battle.winner, len(battle.log("p1")), "log lines")
```

The replayed battle's `view(side)` is the view the
environment gave that side's player, with foe HP as percentages (the
default), without open team sheets or a series, and unless a choice was
refused as unavailable in the episode.

`env.battle(i)` copies battle `i` as a `pikapro.Battle`, and
`Battle.fork(seed)` copies it with a new random stream.
