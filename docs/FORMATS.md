# Formats

A `Format` is plain data (`crates/pikapro/src/format.rs`). Mechanics code
branches on its fields (`mechanics`, `game_type`, `stat_system`, `gimmicks`,
...), never on its `id`. A new format is a new value, not new code.

## Presets

| id | game type | preview | level | stat system | gimmicks | clauses |
|---|---|---|---|---|---|---|
| `gen9singles` | singles | none | set level | EVs/IVs | Tera | none |
| `gen9doubles` | doubles | none | set level | EVs/IVs | Tera | none |
| `gen9vgc` | doubles | bring 4–6 pick 4 | adjusted down to 50 | EVs/IVs | Tera | species, item |
| `gen9ou` | singles | bring 1–6, all of them | set level (100 by default) | EVs/IVs | Tera | species; Endless Battle Clause |
| `gen9randombattle` | singles | none | set level (per species) | EVs/IVs | Tera | species; Sleep Clause Mod, Illusion Level Mod |
| `gen9randomdoublesbattle` | doubles | none | set level (per species) | EVs/IVs | Tera | species; Sleep Clause Mod, Illusion Level Mod |
| `championsvgc` | doubles | bring exactly 6 pick 4 | exactly 50 | Stat Points | Mega Evolution | species, item |
| `championsbss` | singles | bring exactly 6 pick 3 | exactly 50 | Stat Points | Mega Evolution | species, item |

All presets end in a tie at turn 1000.

`gen9singles` and `gen9doubles` are custom games: any team of one to six
Pokémon the engine can build, no team preview (the first Pokémon leads),
no bans and no clauses. They are for experiments and for testing
mechanics, not a format people play. `gen9ou` is the tier format:
Smogon's Scarlet/Violet OverUsed as Showdown plays it (`[Gen 9] OU`),
with team preview, the tier's bans and clauses (in the team validator)
and the Endless Battle Clause. `gen9randombattle` and
`gen9randomdoublesbattle` are Showdown's Gen 9 random battles
([below](#gen-9-random-battles)). The other Smogon tiers aren't presets.

`Format::team_preview` says how a battle starts (`TeamPreview`): `None`
(no preview), `Pick(n)` (preview, each player brings `n`: Showdown's
`Picked Team Size`), or `All` (preview without a picked size, as in
`gen9ou`: every Pokémon is brought and the choice sets their order.
Showdown writes `|teampreview` without a count and a request without
`maxChosenTeamSize`, and fills a short choice up in team order: `team 3`
leads with the third Pokémon).

Each preset is checked against a Showdown format, and its teams are
validated as one (`pikapro-validate`):

| id | played as on Showdown | validated as |
|---|---|---|
| `gen9singles` | `gen9customgame@@@!Team Preview` | `gen9anythinggoes@@@Species Clause` |
| `gen9doubles` | `gen9doublescustomgame@@@!Team Preview` | `gen9anythinggoes@@@Species Clause` |
| `gen9vgc` | `gen9doublescustomgame@@@Picked Team Size = 4,Adjust Level Down = 50` | `gen9vgc2025regi` ([Gen 9] VGC 2025 Reg I) |
| `gen9ou` | `gen9ou` ([Gen 9] OU) | `gen9ou` |
| `gen9randombattle` | `gen9randombattle` | `gen9anythinggoes@@@Species Clause` (supplied teams; Showdown validates no generated one) |
| `gen9randomdoublesbattle` | `gen9randomdoublesbattle` | `gen9anythinggoes@@@Species Clause` (supplied teams) |
| `championsvgc` | `gen9championsvgc2026regmc` | `gen9championsvgc2026regmc` ([Gen 9 Champions] VGC 2026 Reg M-C) |
| `championsbss` | `gen9championsbssregmc` | `gen9championsbssregmc` ([Gen 9 Champions] BSS Reg M-C) |

`championsbss` is `championsvgc`'s rule set in singles, without Open
Team Sheets.

Showdown's custom games show exact HP to everyone, so the logs of the
presets checked against one (`gen9singles`, `gen9doubles`, `gen9vgc`) do
too. The logs of `gen9ou`, the random battles, `championsvgc` and
`championsbss` show a foe's HP as a percentage. Those also have the
cartridge's integer overflows, which Showdown's custom games turn off
(`Format::cartridge_overflow`), and mark a Pokémon `maybeTrapped` by a
trapping ability a foe's species could have
(`Format::obtainable_abilities`; in `gen9ou` not by Arena Trap or Shadow
Tag, which the format bans: `Format::banned_trapping_abilities`). The player view
(`pikapro-view`, `pikapro.rl`) reports percentages in every format
unless asked for exact HP.

Level rules follow Showdown's names (`LevelRule`). `AdjustDown(n)` is
`Adjust Level Down = n`, as in Scarlet/Violet's Flat Rules: only higher
levels change. `Adjust(n)` is `Adjust Level = n`, as in Champions' Flat
Rules: every Pokémon battles at exactly `n`.

## Gen 9 OU

`gen9ou` is Showdown's `[Gen 9] OU` at the pinned commit: the `Standard`
ruleset with Evasion Abilities Clause and Sleep Moves Clause, without
Sleep Clause Mod.

| rule | where |
|---|---|
| Team Preview (no picked team size), HP Percentage Mod, Cancel Mod | the battle (`Format::gen9_ou`) and its log |
| Endless Battle Clause | the battle ([below](#endless-battle-clause)) |
| the eight `\|rule\|` lines of the header | `BattleInfo::for_format` |
| Obtainable, EV limit 510, teams of 1 to 6, levels 1 to 100 (100 by default), Min Source Gen 9 | the validator (`ValidationFormat::gen9_ou`) |
| Species Clause, Nickname Clause | the validator; Species Clause also in `Team::validate` |
| OHKO Clause, Sleep Moves Clause (a move with `ohko`, or whose status is sleep; Yawn) | the validator |
| Evasion Items, Moves and Abilities Clauses (Bright Powder, Lax Incense; Acupressure, Minimize, Double Team; Sand Veil, Snow Cloak) | the validator |
| the bans: the Uber and AG tiers, Arena Trap, Moody, Shadow Tag, King's Rock, Razor Fang, Baton Pass, Last Respects, Shed Tail, Tera Blast | the validator; the tiers are in `pikapro-data`'s legality tables (`legality::Tier`) |

A battle doesn't check a team's legality: `Battle::new` only wants one
to six Pokémon, no two of a species (Showdown's simulator checks
nothing, its server validates first). Validate teams with
`pikapro-validate` (`validate_preset`), or draw them with
`pikapro-teams`.

### Endless Battle Clause

Showdown ends a battle that could go on for ever
(`maybeTriggerEndlessBattleClause` in `sim/battle.js`), and `gen9ou` has
the rule (`Format::endless_battle_clause`). A Pokémon is *stale* when it
restored PP or HP in a way that can repeat:

- it ate a restorative berry (Leppa, Sitrus, Oran, Figy, Wiki, Mago,
  Aguav, Iapapa, Enigma) that it had been given back (Recycle, Harvest,
  Pickup, Trick, ...): stale by its own side's doing;
- a foe healed it (Heal Pulse, Floral Healing), ate its Leppa Berry (Bug
  Bite, Pluck) or flung one at it, or gave it an ability (Skill Swap,
  Entrainment, Wandering Spirit; this one ends when it leaves the
  field): stale by a foe's doing.

At the end of each turn past turn 100, if every side's active Pokémon are
stale, at least one by a foe's doing, and a side is trapped or has no
Pokémon left that isn't stale, the battle ends. A side whose team
started with a restorative berry and a way to get one back (Harvest,
Pickup or Recycle) loses; if both or neither did, it is a tie
(`EndReason::EndlessBattle`). Showdown's code is ported as written,
including what looks unintended in it: an item received through Trick
counts as the side's own doing (`setItem` is called without the
Pokémon that caused it), and whether a team "started with a berry" is
read from the last Pokémon looked at, in the team's current order.

## Gen 9 random battles

`gen9randombattle` and `gen9randomdoublesbattle` are Showdown's `[Gen 9]
Random Battle` and `[Gen 9] Random Doubles Battle` at the pinned commit:
Obtainable, Species Clause, HP Percentage Mod, Cancel Mod, Sleep Clause
Mod and Illusion Level Mod, no team preview, six Pokémon at their levels.
Showdown's teams come from its random-battle generator, on a PRNG of
each player's own (never the battle's), and are checked against nothing.
pikapro-teams ports that generator exactly
([`random_battle`](../crates/pikapro-teams/src/random_battle/mod.rs)), so
a seed gives Showdown's team, and a battle between teams, generated or
supplied, is Showdown's.

| rule | where |
|---|---|
| no team preview, six Pokémon, Species Clause, foes' HP in percent, the overflows, `maybeTrapped` by foes' species' abilities | the battle (`Format::gen9_random_battle`) and its log |
| Sleep Clause Mod (`Format::sleep_clause_mod`) | the battle ([below](#sleep-clause-mod)) |
| Illusion Level Mod (`Format::illusion_level_mod`) | the battle: a disguise's details show the disguise's level |
| Obtainable, PotD | nothing: Showdown doesn't validate generated teams, and PotD needs the server's `Config.potd`, which the simulator doesn't have |

### Generated teams

Showdown seeds each player's team on its own (`Battle.getTeam`): with
the `seed` of its `>player`, or a fresh `sodium` seed it then writes into
those options, so the input log has it. The same seed gives the same
team in pikapro:

- `pikapro-server`'s stream and `pikapro-simulate` generate the team of a
  `>player` that has none, from its seed or a fresh one. A Showdown
  battle's input log (`>requestlog`, the `end` message's `inputLog`, or a
  server's `/exportinputlog`) replays it, teams included.
- `pikapro_teams::random_battle::generate` takes the PRNG of a seed
  (`Seed::Sodium`, `Seed::ShowdownGen5`).
- `TeamGenerator`'s `Mode::Sets` (the default team source of
  `pikapro-rl` and Python) gives for seed `n` Showdown's team for
  `gen5,<n in 16 hex digits>`.

The generator can draw eight cosmetic formes Showdown makes up without a
Pokédex entry (Sawsbuck-Summer, Florges-Blue, Alcremie-Salted-Cream, ...);
they are in the ID space after every other species, and only the
generator draws them. `pikapro-validate` isn't applied to generated
teams (Showdown validates none).

### Sleep Clause Mod

A sleep from a foe fails while a Pokémon of the target's side is asleep,
unless that sleep came from its own side (Rest, Yawn or a move from an
ally). It is the last handler of `SetStatus` on the field (Showdown's
`onSetStatus` on a `Rule` pseudo-weather, sub-order 5: after a terrain's
and Safeguard's). `Sleep Clause Mod activated.` and a hint follow.

## Reading Showdown's formats

A `ChannelSession` reads these format ids (`pikapro_protocol::channel::showdown_format`):

| Showdown id | preset | foe HP shown | series, open sheets |
|---|---|---|---|
| `gen9championsvgc2026regmc`, `…regmb` | `championsvgc` | percent | offered |
| `gen9championsvgc2026regmcbo3`, `…regmbbo3` | `championsvgc` | percent | best of 3, forced |
| `gen9championsbssregmc`, `…regmb` | `championsbss` | percent | |
| `gen9vgc2025regi` and every `gen9vgc20…` regulation | `gen9vgc` | percent | offered |
| a `gen9vgc20…bo3` id | `gen9vgc` | percent | best of 3, offered |
| `gen9ou` | `gen9ou` | percent | |
| `gen9customgame@@@!Team Preview` | `gen9singles` | exact | |
| `gen9doublescustomgame@@@!Team Preview` | `gen9doubles` | exact | |
| `gen9doublescustomgame@@@Picked Team Size = 4,…` | `gen9vgc` | exact | |
| `gen9randombattle`, `gen9unratedrandombattle`, `gen9randombattleblitz` | `gen9randombattle` | percent | |
| `gen9randomdoublesbattle` | `gen9randomdoublesbattle` | percent | |
| a preset id | that preset | as its Showdown format | |

A `Best of = N` custom rule (`…@@@Best of = 3`) makes any of them a
series. Reg M-B plays on M-C's mechanics; only Wish's and Strength Sap's
PP differ.

Plain `gen9customgame` and `gen9doublescustomgame` aren't read: on
Showdown they have team preview, which the `gen9singles` and
`gen9doubles` presets don't. Challenge with `@@@!Team Preview`.
pikapro-showdown-server's games under those ids have no team preview:
connect with `server="pikapro"`, whose aliases read them as the presets.

## Pokémon Champions

Pokémon Champions is the official VGC game from the 2026 season on. It
runs a Gen 9 battle engine with its own rules. pikapro models it as
`Mechanics::Champions` plus `StatSystem::StatPoints`, with its own `Dex`
(`pikapro_data::champions`). Showdown's `champions` mod at the pinned
commit `a5df827` (Reg M-C) is the reference.

| area | Scarlet/Violet | Champions |
|---|---|---|
| Stat investment | EVs ≤252/stat, ≤510 total; IVs 0–31 | Stat Points ≤32/stat, ≤66 total; IVs fixed at 31 |
| Stat formula | standard | Level 50: HP `base + SP + 75`, others `base + SP + 20`, then nature. Equals the standard formula with EVs `= 4 + 8(SP − 1)` |
| PP | `pp × 8/5` | base PP capped at 20, then `(pp/5 + 1) × 4`; some base PP changed (Protect 10 → 5) |
| Trick Room speed | `(10000 − spe) mod 8192` (fast Pokémon wrap around) | `−spe`, no truncation |
| Paralysis | 25% full paralysis | 12.5% (1/8) |
| Sleep | 1–3 turns | 2–3 turns (`sample([2, 3, 3])`) |
| Freeze | 20% thaw per turn | thaws after ≤3 turns, 25% per turn |
| Encore | the target's queued move this turn is kept | a different queued move is replaced by the encored one at once |
| Mega Evolution | not available | once per battle; Megas don't revert on faint |
| Terastallization | once per battle | not in the current regulations |
| Moves / items / abilities | – | 256 / 253 / 15 overrides in the mod |
| Level | Adjust Level Down = 50 | Adjust Level = 50 (every Pokémon at 50) |
| Team structure | bring 4–6 | doubles, bring exactly 6 pick 4, open team sheets; singles (BSS), bring exactly 6 pick 3 |
| Foe HP display | `ceil(100·hp/max)`, 99 while hurt | `floor(100·hp/max)`, at least 1, colour hint at 20% / 50% |

Only the fixed-level-50 path is modelled, not the mod's `Level Clause
Mod`.
