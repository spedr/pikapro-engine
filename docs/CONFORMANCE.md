# Conformance with Pokémon Showdown

Pokémon has too many edge cases to get right from documentation, so
pikapro treats Pokémon Showdown as its specification. The same inputs go
through both simulators and the output has to match, down to each random
draw.

Showdown is pinned to master commit
`a5df8274e85b0889bf2a9b3422a08b39732374fc`, since Champions
Reg M-C exists only on master. [SHOWDOWN_REPIN.md](SHOWDOWN_REPIN.md) says
how the pin is moved.

## How it works

1. **Same random numbers.** `Seed::ShowdownGen5` makes `BattleRng` produce
   Showdown's random stream ([DETERMINISM.md](DETERMINISM.md)).
2. **Same order of draws.** `resolve/` follows Showdown's control flow, so
   draws happen in the same order.
3. **Same inputs.** `pikapro-conformance` plays a random battle and records
   the teams and every choice. `tools/showdown-oracle` replays them on the
   real simulator.
4. **Same output.** The logs are compared line by line. With `--trace`,
   every random draw is compared too, so the first hidden divergence shows
   up, not just its first visible symptom.

## Status

No divergence from Showdown is known in the eight formats. In two cases
Showdown itself throws or corrupts a team ([below](#where-showdown-throws)).

- Broad differential coverage comprises about 128,000 battles with mirror
  teams, forced mechanics and debug assertions.
- `gen9ou` coverage comprises about 70,000 battles: 51,500 between teams
  Showdown's validator accepts in the tier, the rest with the fuzzer's
  own teams (level 100, one to six Pokémon), mirrors and forced
  mechanics, including 1,800 through the Endless Battle Clause.
- `gen9randombattle` and `gen9randomdoublesbattle` coverage comprises about
  29,000 battles on Showdown's own formats with supplied teams (the
  battle's PRNG sees none of the generator): 5,000 per format with
  sets harvested from the generator, 2,000 per format with the fuzzer's
  teams at levels 62 to 100, mirrors, every applicable forced mechanic,
  and 2,000 per format forced through Sleep Clause Mod; and 10,000
  battles (5,000 per format) between generated teams (`--legal sets`),
  where Showdown draws its own teams from the players' seeds, with 1,200
  battle-stream scripts whose players bring seeds instead of teams
  ([below](#random-battle-teams)). The generator itself matches
  Showdown's on 470,000 seeds per format, Gen 5 and sodium.
- The fuzzer covers 685 moves in the Gen 9 formats and 515 in Champions,
  316 abilities and 334 items, with Tera and Mega Evolution.
- `cargo test` replays over 1,300 battle fixtures, over 140 view fixtures,
  47 channel fixtures and over 90 battle-stream transcripts, without
  needing Node.

## Running it

Regenerate the offline handler-order fixture with `PIKAPRO_REGENERATE_STATUS_HANDLERS=1 cargo test -p pikapro --lib status_handler_lists_and_tie_draws_match_showdown` (uses `tools/showdown-oracle/status-handlers.mjs`).

```sh
# once: install and build the pinned Showdown (Node 22.18+)
(cd tools/showdown-oracle && npm ci)

# random battles, reporting the first divergence of each failure
cargo conformance --format gen9singles --battles 200 --trace

# force a mechanic into the teams (the names are in `view::Target`)
cargo conformance --format championsvgc --target curse --battles 300 --trace

# mirror teams, so speed ties come up far more often
cargo conformance --format gen9doubles --mirror --battles 200 --trace

# choice strings: 10 probes of `side.choose` before every choice
cargo conformance --format gen9vgc --battles 200 --choices 10

# legal teams (pikapro-teams' generators) instead of the fuzzer's own
cargo conformance --format gen9ou --legal sets --battles 200 --trace

# gen9ou's Endless Battle Clause, which other battles rarely reach
cargo conformance --format gen9ou --target endless --battles 300 --trace

# the random battles, between the teams Showdown's generator draws for seeds
cargo conformance --format gen9randombattle --legal sets --battles 200 --trace
cargo conformance --format gen9randomdoublesbattle --target sleepclause --battles 300 --trace

# record passing battles as fixtures, or re-check every fixture on Showdown
cargo conformance --format gen9singles --battles 200 --record crates/pikapro-conformance/fixtures
cargo conformance --rerecord crates/pikapro-conformance/fixtures

# the regression test (no Node)
cargo test -p pikapro-conformance --test golden
```

## Workflow for fixing a divergence

1. `cargo conformance --format F --trace --show 1` and read the first
   divergence.
2. Replay the saved file with Showdown's random call sites:
   `PIKAPRO_ORACLE_TRACE=1 node tools/showdown-oracle/oracle.mjs target/conformance/<format>-<seed>.json`
3. Read that code in
   `tools/showdown-oracle/node_modules/pokemon-showdown/dist/sim/` and port
   it, **including when it draws**.
4. Rerun the fuzzer, then `--record` new fixtures so `cargo test` keeps the
   fix.

Never edit a fixture by hand or delete one to make a test pass. Each
fixture directory has a `MANIFEST`, so a missing fixture fails the test.

## What is compared

- **Every line Showdown writes**, `|split|` blocks included, then each of
  the four channels: omniscient, spectator, `p1` and `p2`. Only the time
  in `|t:|` lines and `|debug|` lines are left out
  (`NOT_COMPARED` in `crates/pikapro-conformance/src/lib.rs`).
- **Every `|request|`**, byte for byte, at the same decision point.
- **Choices**, with `--choices`: pikapro and `side.choose` must accept and
  reject the same strings.
- **Log on and log off.** Each battle is replayed with events on and off,
  and must reach the same state after every choice.

## Format mapping

[FORMATS.md](FORMATS.md#presets) lists the Showdown format of each
preset. `gen9singles` and `gen9doubles` run on debug formats: exact HP
for everyone, `debug` lines. `gen9ou` runs on Showdown's own `gen9ou`;
the fuzzer's teams for it are level 100 (50 elsewhere), and with
`--legal` they are teams Showdown's validator accepts in the tier. The random
battles run on Showdown's own `gen9randombattle` and `gen9randomdoublesbattle`,
with teams given to it (`getTeam` then skips the generator), or with
`--legal sets` the players' seeds, from which Showdown's generator draws
the teams pikapro-teams draws for them. Champions teams send Stat Points in the
`EVs:` field, as the `champions` mod reads them.

Showdown's custom games ignore the bit count of `trunc(num, bits)`, so
damage doesn't wrap at 16 bits there. `Format::cartridge_overflow` says
which formats wrap: championsvgc, championsbss, gen9ou and the random
battles do.

## Scripted battles

A branch random battles never reach gets a scripted fixture: a file with
the format, a seed, both teams and every choice (the fixture format,
without `expect` lines), put in a directory of its own with a `MANIFEST`
that lists it. `cargo conformance --rerecord DIR` plays it on both
simulators and, if they agree, writes Showdown's log into it; it then
goes into `crates/pikapro-conformance/fixtures` (and its `MANIFEST`)
like any other. This is the recording path for the `*-scripted-*` fixtures.

### Endless Battle Clause

`gen9ou`'s Endless Battle Clause only acts after turn 100, on battles
that could last for ever. The fuzzer's random battles rarely get there
(about one in a thousand of its own teams' battles, none of the legal
teams'), so the clause has two checks of its own.

`--target endless` gives every Pokémon a move that heals its foe and a
weak attack, some of them restorative berries and others Recycle, Bug
Bite, Pluck, Fling, Skill Swap, Entrainment, Ingrain or Block: most of
these battles go stale and end by the clause (37 of the 40 in
`the_endless_target_reaches_the_clause`: 8 lost by one side, 28 tied
with the message, 1 tied without it, as the order of each team makes
it).

And 18 scripted battles of 100 turns or more
(`gen9ou-scripted-endless-*`) take one branch of
`maybeTriggerEndlessBattleClause`, or one place Showdown writes a
staleness, each. `tests/golden.rs`
(`endless_battle_scenarios_end_as_recorded`) says what each ends with:

| branch | scenario |
|---|---|
| one side's team could cycle a berry: it loses | `pickup-side-loses`, `harvest-side-loses` |
| both could: a tie, with the message | `leppa-recycle-loop-both-cycle` (a Leppa Berry eaten and got back with Recycle three times) |
| neither could: a tie, no message | `neither-cycles-tie` |
| one side isn't stale: no end | `one-side-stale-goes-on` |
| both stale by their own doing only: no end | `internal-only-goes-on` (an Oran Berry got back with Recycle and eaten) |
| own doing on one side, a foe's on the other: ends | `internal-and-skill-swap` |
| both sides can switch to a Pokémon that isn't stale: no end | `both-can-switch-goes-on` |
| a side that could is trapped: ends | `trapped-side-cannot-switch` (Ingrain) |
| a benched Pokémon's set counts for its team | `benched-cycler-loses`, `trapped-side-cannot-switch` |
| the team's order decides whether it "started with a berry" | `bench-order-berry-last-loses`, `bench-order-recycle-last-tie` (one team, two team-preview orders) |
| a staleness that ends on leaving the field | `switching-ends-volatile-staleness` (Skill Swap) |
| staleness by a foe: Heal Pulse, Floral Healing | most of them; `entrainment-and-floral-healing` |
| a Leppa Berry eaten by Bug Bite or Pluck, or flung | `bug-bite-leppa-berry`, `pluck-leppa-berry`, `fling-leppa-berry` |
| an ability given: Skill Swap, Entrainment, Wandering Spirit | `internal-and-skill-swap`, `entrainment-and-floral-healing`, `wandering-spirit` |

Not covered by a battle against Showdown: the clause in doubles (no
doubles preset has it), Harvest or Pickup actually bringing a berry back
(they count for a team by its sets alone; a berry they bring back goes
through the same `setItem` as Recycle's), and a held item lost between
getting it back and eating it (`takeItem` drops `pendingStaleness`; unit
tests in `resolve/endless.rs` cover the bookkeeping).

## Random-battle teams

`pikapro_teams::random_battle` is Showdown's Gen 9 random-battle team
generator ported exactly, and `random_teams` diffs it against Showdown's
(`tools/showdown-oracle/random-teams.mjs`): the same seeds through both,
every field of every set compared, and Showdown's packed team read back
by pikapro-protocol. A team that differs is shown with both generators'
draws side by side, up to the first that differs.

```sh
cargo run --release -p pikapro-conformance --bin random_teams -- --format gen9randombattle --seeds 100000
cargo run --release -p pikapro-conformance --bin random_teams -- --format gen9randomdoublesbattle --seeds 20000 --sodium
cargo run --release -p pikapro-conformance --bin random_teams -- --format gen9randombattle --trace 17
```

`cargo conformance --legal sets` in a random battle gives each player a
seed: pikapro plays the generator's teams for them and Showdown its own
(`teamSeeds` in the oracle's input). Their fixtures
(`*-generated-*`) record the seeds, and `cargo test` checks that the
generator still gives the recorded teams. `pikapro-teams`'
`tests/random_battle.txt` holds Showdown's teams for a few seeds per
format and a digest of many more (`random_teams --record`).

## Team validation

`pikapro-validate` is checked against Showdown's team validator: the same
teams go through both, and every message must match, word for word and in
order.

```sh
cargo run --release -p pikapro-conformance --bin validate_fuzz -- --teams 2000 --show 5
cargo run --release -p pikapro-conformance --bin validate_fuzz -- --check crates/pikapro-validate/tests/deep.txt
```

Each preset's validation format is in [FORMATS.md](FORMATS.md#presets).

## Legal random teams

Generated teams pass Showdown's validator, `pikapro-validate`,
`Team::validate` and `Battle::new`, except for `Mode::Sets` in
`gen9randombattle` and `gen9randomdoublesbattle`: Showdown's generator,
whose teams bypass validation as in Showdown and may fail supplied-team
validation (for example, a random-battle level below an evolution
level); they remain playable by `Battle::new`.

```sh
cargo run --release -p pikapro-conformance --bin teams_fuzz -- --teams 10000 --jobs 2
```

## Battle stream

`pikapro-server`'s `BattleStream` runs the same command scripts as
Showdown's. Every `update`, `sideupdate`, `end` and `requesteddata` message
is compared, then each player's channel.

```sh
cargo run --release -p pikapro-server --example stream_fuzz -- --format championsvgc --battles 200
```

## Player view

`pikapro-view` must hold exactly what the player's channel shows. Three
checks run at every decision point:

1. **Soundness.** Everything the view claims is true of the real battle
   state.
2. **No leaks.** An event a player can't see leaves their view unchanged,
   and changing the opponent's hidden information (a move, item, spread,
   ability) doesn't change what the view shows.
3. **Completeness.** The view knows what poke-env's tracker knows. The
   accepted differences are poke-env's own tracking choices, listed in
   `tools/view-oracle/diff/allowlist.py`.

```sh
cargo view-conformance --format championsvgc --battles 1000 --jobs 4
cargo view-conformance --format championsvgc --battles 300 --check poke-env --python .venv/bin/python
```

Known gaps:

- The player's own exact HP, status, ability and forme come from requests,
  so they're stale after the last request of a battle.
- A Pokémon that transforms before its first request (an Imposter lead) has
  no moves of its own in the view until a later request.
- A Sticky Barb that moves to an itemless attacker is invisible until it
  does damage.
- Volatiles aren't projected onto the poke-env schema's `effects`.

### The channel round trip

The view also has to come from the text alone. `cargo channel-conformance`
feeds each player's channel to a `ChannelSession` and compares its view,
observation, mask and `/choose` text with the environment's at every step.
`--showdown` does the same with Showdown's own streams, and `--reconnect`
also checks a session that joins mid-battle.

```sh
cargo channel-conformance --format championsvgc --battles 500
```

## Feature audit

This checks that the numbers a view derives mean what their
documentation says. Counters, durations, PP and event flags are compared at
every decision point with a reference computed from the engine, never from
the view.

```sh
cargo run --release -p pikapro-conformance --bin feature_audit -- --format championsvgc --battles 5000 --source sets
```

## Where Showdown draws random numbers

A mechanic that touches any of these must keep Showdown's order.

| Source | Showdown | pikapro |
|---|---|---|
| Queue sort ties | `speedSort(queue)` in `commitChoices` and the Gen 8+ re-sort before each move | `queue::sort` |
| Insertion ties | `insertChoice` places tied actions with `random(first, last + 1)` (e.g. `runSwitch`) | `queue::insert_choice` |
| Target draws | `getRandomTarget` → `randomFoe()` → `sample()` from `resolveAction`, `getActionSpeed` → `getTarget`, `runMove` → `getTarget`, and `getMoveTargets` | `resolve/targeting.rs` |
| Active-Pokémon sorts | `eachEvent` (`Update` after most actions and hits, `BeforeTurn`), `runSwitch` | `each_event`, `run_switch` |
| Handler sorts | every `runEvent` with two or more handlers (`speedSort`: order, priority, speed, sub-order; abilities 7, items 8), `fieldEvent('Residual')` and `fieldEvent('SwitchIn')`; ties are shuffled from the collection order (for `runEvent`: the target's status, volatiles in the order they were added, ability, item and slot conditions, the allies' and foes' prefixed handlers, the source's, the sides', the field's; for `fieldEvent`: field, then per side its conditions and each active Pokémon's handlers, then side and field handlers acting on it). A pseudo-weather's sub-order is 2 until one of its handlers has run through `runEvent`, then 5. `TryHit`, `DamagingHit` and `Invulnerability` sort left to right without draws; `priorityEvent` (`RedirectTarget`, `WeatherModifyDamage`) without draws | `resolve/events.rs`, `handlers.rs` |
| Drag targets | `dragIn` → `getRandomSwitchable` → `sample()` (draws even with one candidate), after the Roar / Dragon Tail action | `switching::run_drags` |
| Move rolls | accuracy, crit, damage roll, Protect's `stall` chance | `resolve/moves.rs` |
| Called moves | Metronome's `sample()` over its list (standard or `Unobtainable` moves with the `metronome` flag, sorted by `num`; one draw even from one move), Sleep Talk's over the user's moves; then `getRandomTarget` for the called move | `order::metronome_move` (a static table per dex), `order::call_move` |
| Effect rolls | `secondaries`: `random(100)` per secondary per hit target (fainted ones too); `selfDrops`: `random(100)` once per move for `self.boosts` (per target per hit for multi-hit), even without a chance | `resolve/secondaries.rs` |

Two details matter everywhere:

- **Stored speed.** Sorts use Showdown's stored `pokemon.speed`, refreshed
  only by `updateSpeed`. pikapro keeps it in `PokemonState::speed`.
- **"Fainted" is a flag, not HP 0.** A Pokémon knocked out mid-move still
  counts as active until `faintMessages` reports it. pikapro models this as
  `SideState::active`, which `faint_messages` clears.

## Where Showdown throws

One rare case in doubles. A Pokémon comes in early through an Eject Pack
switch and faints on entry, and the switch its partner queued at the start
of the turn then brings it in again. Showdown leaves it in two slots and
throws on the forced switches at the end of the turn.

pikapro matches Showdown through that turn's `|upkeep`. After that it goes
on by its own rules, so the battle can finish. The oracle exits with status
4 on a throw, and `cargo conformance` reports it as a failure.

The other is a choice no client sends: two `team` pieces at team preview in
doubles (`default, team 1`, or ` team 12, team 34` with a leading space).
Showdown's `chooseTeam` has no check for a team already chosen and queues
both pieces' actions, eight for a team of four. The battle then starts with
a team that lists Pokémon twice, or throws. pikapro rejects the second
piece with `Can't choose for Team Preview: You must choose exactly 4
Pokémon`, and the first piece's team stays chosen.
