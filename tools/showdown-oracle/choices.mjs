#!/usr/bin/env node
// Probes Pokémon Showdown's choice handling: replays a battle like
// oracle.mjs, and before choice number i tries each probe string for that
// side with `side.choose`, then discards it (`clearChoice`), so
// pikapro-protocol's `parse_choice_for` can be diffed against Showdown's
// verdicts and resulting actions.
//
// Input (JSON, argv[2] or stdin): oracle.mjs's input plus
//   "probes": [[i, "p1", "move Protect"], ...]   (i: index into choices)
//
// Output, one line per probe in input order:
//   probe <n> ok <actions>     e.g. `move 2 -1 terastallize, switch 3`
//   probe <n> err <message>    Showdown's error, without `[Invalid choice]`
// <actions> is `side.getChoice()` with move slots instead of move ids
// (`move locked` / `move struggle` for moves without a slot), targets only
// in doubles, and `team 3,1,2` for team preview.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { Battle, Teams } = require('pokemon-showdown');

const input = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));
const { formatid, seed, teams, choices, probes } = input;

const battle = new Battle({ formatid, seed });
battle.setPlayer('p1', { name: 'p1', team: Teams.pack(Teams.import(teams[0])) });
battle.setPlayer('p2', { name: 'p2', team: Teams.pack(Teams.import(teams[1])) });

function actions(side) {
  const acts = side.choice.actions;
  if (acts.length && acts.every(a => a.choice === 'team')) {
    return 'team ' + acts.map(a => a.pokemon.position + 1).join(',');
  }
  return acts.map(a => {
    switch (a.choice) {
    case 'move': {
      const slot = a.moveSlot === undefined ? (a.moveid === 'struggle' ? 'struggle' : 'locked') : a.moveSlot + 1;
      let out = `move ${slot}`;
      if (a.moveSlot !== undefined && a.targetLoc && battle.activePerHalf > 1) out += ` ${a.targetLoc}`;
      if (a.mega) out += ' mega';
      if (a.terastallize) out += ' terastallize';
      return out;
    }
    case 'switch':
    case 'instaswitch':
    case 'revivalblessing':
      return `switch ${a.target.position + 1}`;
    default:
      return a.choice;
    }
  }).join(', ');
}

// What `side.choose` may change besides `side.choice`: Revival Blessing's
// `switchFlag = false`, the `maybe*` flags and the request an error
// updates. Restored after each probe.
const MUTABLE = ['switchFlag', 'maybeLocked', 'maybeDisabled', 'maybeTrapped', 'trapped'];

function probe(sideid, text) {
  const side = battle.getSide(sideid);
  const saved = side.pokemon.map(p => MUTABLE.map(k => p[k]));
  const request = side.activeRequest && JSON.stringify(side.activeRequest);
  try {
    return probeOnce(side, text);
  } finally {
    side.pokemon.forEach((p, i) => MUTABLE.forEach((k, j) => { p[k] = saved[i][j]; }));
    if (request) side.activeRequest = JSON.parse(request);
  }
}

function probeOnce(side, text) {
  side.clearChoice();
  let result;
  try {
    if (!side.choose(text)) {
      result = `err ${side.choice.error ||
        `Unknown error for choice: ${text}. If you're not using a custom client, please report this as a bug.`}`;
    } else if (!side.isChoiceDone()) {
      result = `err Incomplete choice: ${text} - missing other pokemon`;
    } else {
      result = `ok ${actions(side)}`;
    }
  } catch (e) {
    result = `err EXCEPTION ${e.message}`;
  }
  side.clearChoice();
  return result;
}

const out = [];
const results = new Array(probes.length);
const byIndex = new Map();
probes.forEach(([i, sideid, text], n) => {
  if (!byIndex.has(i)) byIndex.set(i, []);
  byIndex.get(i).push([n, sideid, text]);
});
for (let i = 0; i <= choices.length; i++) {
  for (const [n, sideid, text] of byIndex.get(i) ?? []) results[n] = probe(sideid, text);
  if (i === choices.length) break;
  const [sideid, ...rest] = choices[i].split(' ');
  if (!battle.choose(sideid, rest.join(' '))) {
    out.push(`rejected ${i} ${battle.getSide(sideid).choice.error}`);
    break;
  }
}
results.forEach((r, n) => out.push(`probe ${n} ${r ?? 'err NOT RUN'}`));
process.stdout.write(out.join('\n') + '\n');
