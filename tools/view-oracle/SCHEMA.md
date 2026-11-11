# View oracle: record schemas

Two JSON Lines formats. **Decision records** are what a player's client
receives, one record per request sent to a side. **State records** are
what poke-env 0.16.1 tracks after each decision record, normalized. The
pikapro player view (`pikapro-view`) is projected onto the state schema
and diffed against poke-env's, field by field.

Conventions for both:

- One JSON object per line, UTF-8, no trailing commas.
- **No floats.** Every number is an integer, so two implementations can
  produce identical bytes.
- An *ID* is Showdown's `toID`: lowercase ASCII letters and digits
  (`stealthrock`, `charizardmegax`, `leftovers`). poke-env enum members
  become IDs by lowercasing and dropping underscores (`STEALTH_ROCK` →
  `stealthrock`, `ELECTRIC_TERRAIN` → `electricterrain`).
- `side` is `"p1"` or `"p2"`.

## Input: decision records

Written by `gen_decisions.py` and by pikapro-conformance's `view_dump`,
which must produce the same records.

| field | type | meaning |
|---|---|---|
| `format` | string | pikapro format: `championsvgc`, `gen9vgc`, `gen9singles`, `gen9doubles` |
| `seed` | int | the battle's seed (`pikapro.Battle(format, teams, seed)`) |
| `side` | string | whose client this is |
| `index` | int | the record's position in its stream, from 0 |
| `turn` | int | `Battle.turn` when the record was sent |
| `lines` | string[] | the side's channel lines since this stream's previous record, in order (`Battle.log(side, start)`); the first record starts with `Battle.header()` |
| `request` | string or null | the `\|request\|` JSON text as sent (`Battle.request_json(side)`), without the `\|request\|` prefix; `null` in the stream's last record, sent when the battle ended |
| `teams` | string[2] | optional, first record only: both teams (export or packed), to replay the battle |

A *stream* is the records of one `(format, seed, side)`, in `index`
order. Streams may interleave in a file.

A record is one update the battle room sends: the new lines, then the
request. That is Showdown 0.11.11's order (`battle.sendUpdates` sends the
`update`, then each side's request; see `pikapro-server`'s `stream.rs`).
After each update *both* sides get a record, because Showdown sends both
sides a request after every update: a side with nothing to do gets a
`{"wait":true,...}` request. So a stream has a record for every request its
side receives, including waits, plus the final one with `request: null`,
whose `lines` end with `|win|...` or `|tie`.

A consumer feeds `lines` in order, then `request` (as a
`|request|<json>` line). The poke-env consumer feeds lines exactly as
poke-env's `Player._handle_battle_message` dispatches a room message
(`pikapro.compat.poke_env.Feeder`).

**Generator recipe** (`gen_decisions.py`), so another implementation can
play the same battles: battle `k` of a run with `--seed S` has seed
`s = S + k`, teams `pikapro.RandomTeams(format, "sets").pair(s)` (`--teams
uniform` for uniform mode) and choices from one
`pikapro.RandomPolicy(s ^ 0x5EED)`: after each update, `choose(battle, 0)`
is submitted (if the side has legal actions), then `choose(battle, 1)`.

## Output: state records

Written by `pokeenv_view.py`, one per decision record.

| field | type | meaning |
|---|---|---|
| `format`, `seed`, `side`, `index`, `turn` | | copied from the decision record |
| `state` | object | the normalized state (below), after feeding the record |
| `warnings` | string[] | optional: what poke-env logged at WARNING or above while parsing this record |
| `error` | string | instead of `state`: poke-env raised (`"<Exception>: <message>"`); the stream has no further records |
| `line` | string or null | with `error`: the line poke-env raised on |

### `state`

The state of the poke-env `Battle` (singles) or `DoubleBattle` (doubles)
that tracks `side`. `pikapro.compat.poke_env.snapshot(battle)` computes it.

| field | type | poke-env source | meaning |
|---|---|---|---|
| `role` | string or null | `player_role` | the tracked side |
| `turn` | int | `turn` | the last `\|turn\|` number, 0 before it |
| `finished` | bool | `finished` | after `\|win\|` or `\|tie` |
| `won` | bool or null | `won` | null until finished and for a tie |
| `teampreview` | bool | `teampreview` | the last request was a team preview |
| `wait` | bool | `_wait` | the last request was a wait |
| `force_switch` | bool[] | `force_switch` | per active slot (one entry in singles) |
| `used_tera`, `opponent_used_tera` | bool | `used_tera`, `opponent_used_tera` | a `-terastallize` was seen for that side |
| `used_mega`, `opponent_used_mega` | bool | `used_mega_evolve`, `opponent_used_mega_evolve` | a `-mega` was seen for that side |
| `weather` | object or null | `weather` | `{"name": ID, "since": int}`: the weather and the turn of the last `-weather` line naming it (poke-env rewrites it on every `[upkeep]` line) |
| `fields` | {ID: int} | `fields` | terrains and pseudo-weathers (`trickroom`, `gravity`, `electricterrain`, `neutralizinggas`) → the turn of their last `-fieldstart` |
| `side_conditions`, `opponent_side_conditions` | {ID: int} | `side_conditions`, `opponent_side_conditions` | `spikes`, `toxicspikes` → layers; any other condition → the turn of its first `-sidestart` |
| `active` | (string or null)[] | `active_pokemon` | per slot, the key (in `team`) of the tracked side's active Pokémon; null for an empty slot or a fainted Pokémon |
| `opponent_active` | (string or null)[] | `opponent_active_pokemon` | the same for the foe, keys in `opponent_team` |
| `team` | {key: Pokémon} | `team` | the tracked side's Pokémon, in poke-env's order (the order of the first request) |
| `opponent_team` | {key: Pokémon} | `_opponent_team` | the foe's Pokémon that have appeared (switched in, or were named), in order of appearance |
| `opponent_preview` | ID[] | `teampreview_opponent_team` | the species of the foe's `\|poke\|` lines, in order; empty without team preview |

A **key** is poke-env's: `"p1: <name>"`, the Pokémon's ident without the
slot letter (the nickname if it has one).

### Pokémon

| field | type | poke-env source | meaning |
|---|---|---|---|
| `species` | ID | `species` | the species poke-env stored: from `details` (`\|switch\|`, `\|poke\|`, the request). A forme change or Mega Evolution seen only in the log doesn't change it; a request's `details` does, so the own side's follows the request |
| `forme` | ID | the Pokédex entry of `base_stats` | the forme poke-env's stats and types come from: follows `-formechange`, `detailschange` and `-mega` |
| `level` | int | `level` | 100 when `details` has no level |
| `gender` | `"M"`, `"F"`, `"N"` or null | `gender` | `"N"` when `details` has none; null before any `details` |
| `hp`, `max_hp` | int | `current_hp`, `max_hp` | as shown: exact for the own side (from the request); for the foe, what its HP lines say (`x/100` in percentage formats, exact HP in the custom-game presets). 0/0 if never shown (or after poke-env's `was_illusioned`) |
| `status` | ID or null | `status` | `brn`, `frz`, `par`, `psn`, `slp`, `tox`, `fnt` |
| `boosts` | {stat: int} | `boosts` | all seven keys: `accuracy`, `atk`, `def`, `evasion`, `spa`, `spd`, `spe` |
| `item` | ID, null or `"?"` | `item` | the item; null: known to hold none (the request's `""`, or removed: `-enditem`, Knock Off); `"?"`: unknown (poke-env's `unknown_item`) |
| `ability` | ID or `"?"` | `ability` | the effective ability (a temporary one, e.g. from Skill Swap or a Mega's, wins over the base one); `"?"` unknown |
| `moves` | ID[] | `_moves._base_moves` | the Pokémon's own known moves in the order poke-env learned them: all four from the request for the own side; for the foe, the ones it was seen using (moves called by others, e.g. Metronome, and moves used while transformed are not in it) |
| `types` | ID[] | `types` | current types: the Tera type once terastallized, else a type change (`typechange`, Transform), else the forme's |
| `tera_type` | ID or `"?"` | `tera_type` | set by `-terastallize` or a `tera:` detail; `"?"` otherwise |
| `terastallized` | bool | `is_terastallized` | |
| `mega` | bool | from `forme` | the forme is a Mega (its Pokédex `forme` starts with `Mega`) |
| `fainted` | bool | `fainted` | |
| `active` | bool | `active` | poke-env's flag: switch-ins set it, switch-outs clear it, and each request overwrites the own side's from its `active` field (a fainted Pokémon waiting to be replaced stays active) |
| `revealed` | bool | `revealed` | it has switched in (poke-env's `switch_in`) |
| `effects` | {ID: int} | `effects` | volatiles poke-env tracks (`substitute`, `confusion`, `leechseed`, `taunt`, ...) with poke-env's counter (turns or uses, per effect) |

### How poke-env represents unknowns

- `"?"` (`pikapro.compat.poke_env.UNKNOWN`) marks a fact poke-env doesn't
  know: an unrevealed item or ability, a Tera type never shown.
- null means known to be absent: no item, no weather, an empty slot.
- A foe Pokémon that never appeared is not in `opponent_team`; with team
  preview its species is in `opponent_preview`.
- Unrevealed moves are simply missing from `moves`.
- A foe species with a single possible ability has that `ability` from
  the start (poke-env's `_update_from_pokedex`), before any reveal.

### Known differences to expect from poke-env

These are poke-env's tracking choices, not protocol facts; the diff's
allowlist is `diff/allowlist.py`:

- The own side's `tera_type` is always `"?"`: poke-env doesn't read a
  request's `teraType`.
- `weather.since` moves to the current turn on every `[upkeep]` line.
- `species` of the own side follows the request's `details`, the
  foe's doesn't (compare on `forme`).
- A fainted own Pokémon keeps `active: true` until the request that
  replaces it.
- poke-env guesses Regenerator healing on switch-out
  (`Pokemon.switch_out`) for any Pokémon it believes has Regenerator.
