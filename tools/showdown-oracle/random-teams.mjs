#!/usr/bin/env node
// Showdown's random-battle team generator for a list of seeds, as a battle
// calls it (`Battle.getTeam`: `Teams.getGenerator(format, seed)` for the
// first player, `setSeed(seed)` for the next, then `getTeam()`), so
// pikapro-teams' port (`random_battle`) can be diffed against it.
//
// Input (JSON, from the file given as argv[2] or stdin):
//   {
//     "formatid": "gen9randombattle",           // or gen9randomdoublesbattle
//     "seeds": ["gen5,0001000200030004", "sodium,..."],   // PRNG seeds
//     "trace": false                             // record every PRNG draw
//   }
//
// Output: one line per seed, in input order, its fields separated by tabs:
//   team <seed> <sets> <Teams.pack(team)>
//   error <seed> <message>                     (the generator threw)
// where <sets> is each set's fields joined by `|` (species, the species
// its sets are from, name, gender, S for shiny, level, moves, ability,
// item, EVs, IVs, Tera type, role; moves, EVs and IVs joined by `,`;
// species, moves, ability and item by name) and the sets joined by `]`.
// With "trace", a line `trace <seed> <draws>` follows, the draws separated
// by spaces as `bound:result:site`: `random(n)` as n and its result,
// `random(from, to)` as `to - from` and the result minus `from`, the site
// the generator's function that drew.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { Dex, Teams } = require('pokemon-showdown');

const input = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));
const { formatid, seeds, trace } = input;
if (!formatid || !Array.isArray(seeds)) {
  console.error('random-teams: expected {formatid, seeds: [...], trace?}');
  process.exit(64);
}
const dex = Dex.forFormat(formatid);

// The PRNG's callers that only pass a draw on.
const WRAPPERS = new Set(['random', 'randomChance', 'sample', 'sampleIfArray', 'sampleNoReplace', 'shuffle']);

function site() {
  for (const frame of new Error().stack.split('\n').slice(2)) {
    const m = /at (?:RandomTeams\.)?(\w+) .*random-battles/.exec(frame);
    if (m && !WRAPPERS.has(m[1])) return m[1];
  }
  return '?';
}

function line(set) {
  return [
    set.species,
    dex.species.get(set.speciesId).name,
    set.name,
    set.gender,
    set.shiny ? 'S' : '',
    set.level,
    set.moves.map((m) => dex.moves.get(m).name).join(','),
    set.ability,
    set.item ? dex.items.get(set.item).name : '',
    ['hp', 'atk', 'def', 'spa', 'spd', 'spe'].map((s) => set.evs[s]).join(','),
    ['hp', 'atk', 'def', 'spa', 'spd', 'spe'].map((s) => set.ivs[s]).join(','),
    set.teraType,
    set.role,
  ].join('|');
}

let generator = null;
const out = [];
for (const seed of seeds) {
  if (!generator) generator = Teams.getGenerator(formatid, seed);
  else generator.setSeed(seed);
  const draws = [];
  if (trace) {
    const prng = generator.prng;
    const random = prng.random.bind(prng);
    prng.random = (from, to) => {
      const result = random(from, to);
      draws.push(to === undefined ? [from, result, site()] : [to - from, result - from, site()]);
      return result;
    };
  }
  try {
    const team = generator.getTeam();
    out.push(['team', seed, team.map(line).join(']'), Teams.pack(team)].join('\t'));
  } catch (e) {
    out.push(['error', seed, e.message].join('\t'));
  }
  if (trace) out.push(['trace', seed, draws.map((d) => d.join(':')).join(' ')].join('\t'));
}
process.stdout.write(out.join('\n') + '\n');
