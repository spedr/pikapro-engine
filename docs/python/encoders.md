# Observations

An observation is one float vector per player, written by an encoder
from the player's view. `"basic"` is the one that ships. For features of
your own, write a function over `BattleView` in Python, or an encoder in
Rust.

```python
import pikapro.rl as rl

env = rl.VecEnv("championsvgc", n=64, masks="joint")   # observation="basic"
out = env.reset(seed=0)
out.obs.shape                     # (64, 2, 1688) float32: 2 is the player
env.observation["columns"][:4]    # ['own0.present', 'own0.active', 'own0.fainted', 'own0.hp']
```

Each player's observation is ego-centric: its own Pokémon first
([rows](concepts.md#actions-and-rows)). A built-in opponent's row is
zeros unless the environment is made with `observe_builtin=True`.

## The `basic` vector

1,688 values per player, the same in every format. Flags and numbers
near 0 to 1, with no ids, for a plain network.
`env.observation["columns"]` names every value: `own0.hp`,
`foe3.move1_base_power`, `own_side.spikes`, `field.weather_rain`.

| block | values |
|---|---|
| 12 Pokémon rows (own, then foe; 136 each) | `present`, `active`, `fainted`, `hp` (fraction), `status_*` (6), `boost_*` (7, stage / 6), `type_*` (18), `gimmick` (Mega Evolved or Terastallized), then 4 moves |
| each move (25) | `known`, `base_power` (/ 200), `type_*` (18), `physical`, `special`, `status`, `pp` (fraction left), `disabled` |
| 2 sides (18 each) | `team_size` (/ 6), `fainted` (over the team size), `gimmick_used`, one value per side condition (layers over its maximum) |
| field (20) | `weather_*` (7), `terrain_*` (4), `trick_room`, `gravity`, `magic_room`, `wonder_room`, `turn` (min(turn, 100) / 100), `request_*` (4: wait, team preview, move, forced switch) |

Unknown is 0: a foe's unrevealed moves have `known` 0. A foe's `hp` is
the shown percentage, and 1 before any HP was shown. In a best-of series
it encodes each game alone; the score is in `Step.series`.

## Your own features in Python

`env.view(i, player)` is the player's view as a `BattleView`: what it has
seen, with poke-env's names
([API reference](api.md#class-pikaprorlbattleview)). `rl.embed_all(env,
fn)` calls `fn(BattleView)` for every battle and stacks the results:

```python
import numpy as np

def embed(view):
    """The HP fraction of the six own and six foe rows."""
    mons = list(view.team.values()) + list(view.opponent_team.values())
    hp = [m.current_hp_fraction for m in mons]
    return np.array(hp + [0.0] * (12 - len(hp)), dtype=np.float32)

x = rl.embed_all(env, embed)      # (64, 12)
```

It costs Python per battle per step.
[`examples/custom_encoder.py`](../../crates/pikapro-py/examples/custom_encoder.py)
is a fuller one. On a server, `ChannelSession.view` is the same
`BattleView`.

## Your own encoder in Rust

An encoder runs on the environment's worker threads, with no Python per
step. Implement `pikapro_rl::encode::Encoder`:

```rust
pub trait Encoder: Send + Sync {
    fn size(&self) -> usize;
    fn encode(&self, view: &PlayerView<'_>, tables: &Tables, out: &mut [f32]);
}
```

It reads a view, never a battle, so it can't leak hidden information.
`ViewCtx::new(view, tables)` resolves the view into rows, and
`ctx.mon(side, row)` gives a `MonRef` with the facts as the player knows
them: `species`, `hp_frac`, `status`, `types`, `moves` (slots with PP,
uses, disabled), `item`, `ability`, `nature`, `exact_stats` and `boosts`.
`ctx.active_in(side, slot)` gives the Pokémon in a slot. `Tables` has the
format's dex and `move_facts`, `species_types` and `effect16` (type
effectiveness). `PlayerView::series` has the score and the earlier
games' records in a best-of series.

[`encode/basic.rs`](../../crates/pikapro-rl/src/encode/basic.rs) is a
complete encoder to copy. Pass yours as
`EnvConfig::observation` (`Observation::new(MyEncoder)`). To use it from
Python, add its name to `observation_arg` in
`crates/pikapro-py/src/rl.rs` and build the package from source
([install](install.md)).

## Layout versions

A trained model depends on which integer means which action.
`pikapro.ACTION_SPACE_VERSION` versions the action indexing
([actions](concepts.md#actions-and-rows)); a bump is a breaking change.
Store it with a checkpoint, along with the format and the observation's
`columns`, and compare them when you load it:

```python
import pikapro

LAYOUT = {"actions": pikapro.ACTION_SPACE_VERSION, "columns": env.observation["columns"]}
```
