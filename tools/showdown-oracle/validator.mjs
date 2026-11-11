#!/usr/bin/env node
// Runs Pokémon Showdown's team validator (`sim/team-validator.ts`) on teams
// and prints the problems it finds. This is the reference ("oracle") that
// pikapro-validate is diffed against (see docs/CONFORMANCE.md).
//
// Input (JSON, from the file given as the first argument or stdin):
//   {
//     "formatid": "gen9championsvgc2026regmc",   // any Showdown format id
//     "teams": [
//       [{"species": "Garchomp", "ability": "Rough Skin", "moves": ["Earthquake"], ...}, ...],
//       "<Showdown export text>",
//       ...
//     ]
//   }
// A team is either a list of Showdown `PokemonSet` objects (the fields
// `TeamValidator` reads: name, species, item, ability, moves, nature, evs,
// ivs, level, gender, shiny, teraType, ...) or a team in export format.
//
// Output: one line per team, in order: a JSON array of Showdown's problem
// strings (`[]` when the team is valid).
//
// With `"played": true` in the input, each team's line is followed by a
// second one: the team as the validator left it, which Showdown's server
// then plays (`validateSet` mutates the sets: `validateForme` turns a
// battle-only forme into the forme it changes from, the Mega ability fix,
// Behemoth Blade to Iron Head, an event's gender, ...). A JSON array with
// one string per set, `species|item|ability|move,move,...|gender|level|tera`
// in the format's dex's names; the gender is the one the battle starts with
// (the set's, else the species' fixed one; empty: the battle draws it), the
// Tera type empty when the set has none.
//
//   node validator.mjs --rules <formatid>   prints the format's resolved rule table
//
// The validator is deterministic for teams (no PRNG is involved), so no seed
// is needed.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { Dex, Teams, TeamValidator } = require('pokemon-showdown');

if (process.argv[2] === '--rules') {
  const format = Dex.formats.get(process.argv[3], true);
  const table = Dex.formats.getRuleTable(format);
  const out = {
    name: format.name,
    mod: format.mod,
    rules: [...table.entries()],
    tagRules: table.tagRules,
    valueRules: [...table.valueRules.entries()],
    minTeamSize: table.minTeamSize,
    maxTeamSize: table.maxTeamSize,
    pickedTeamSize: table.pickedTeamSize,
    minLevel: table.minLevel,
    maxLevel: table.maxLevel,
    defaultLevel: table.defaultLevel,
    adjustLevel: table.adjustLevel,
    adjustLevelDown: table.adjustLevelDown,
    evLimit: table.evLimit,
    minSourceGen: table.minSourceGen,
    maxMoveCount: table.maxMoveCount,
    complexBans: table.complexBans,
    complexTeamBans: table.complexTeamBans,
  };
  console.log(JSON.stringify(out, null, 1));
  process.exit(0);
}

const input = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));
const { formatid, teams, played } = input;
if (!formatid || !Array.isArray(teams)) {
  console.error('validator: expected {formatid, teams: [...]}');
  process.exit(64);
}

const validator = new TeamValidator(formatid);
const lines = [];
for (const team of teams) {
  const sets = typeof team === 'string' ? Teams.import(team) : structuredClone(team);
  let problems;
  try {
    problems = validator.validateTeam(sets) ?? [];
  } catch (e) {
    // A crash is a verdict too: the Rust side reports it as a mismatch.
    problems = [`!crash: ${e?.message ?? e}`];
  }
  lines.push(JSON.stringify(problems));
  if (played) {
    const dex = validator.dex;
    const name = (table, value) => (value ? table.get(value).name || String(value) : '');
    lines.push(JSON.stringify(sets.map(set => [
      name(dex.species, set.species),
      name(dex.items, set.item),
      name(dex.abilities, set.ability),
      (set.moves ?? []).filter(m => m).map(m => name(dex.moves, m)).join(','),
      set.gender || dex.species.get(set.species).gender || '',
      String(set.level ?? ''),
      set.teraType ? dex.types.get(set.teraType).name : '',
    ].join('|'))));
  }
}
process.stdout.write(lines.join('\n') + (lines.length ? '\n' : ''));
