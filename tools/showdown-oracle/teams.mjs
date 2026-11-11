#!/usr/bin/env node
// Round-trips teams through Pokémon Showdown's team formats, so
// pikapro-protocol's import, export, pack and unpack can be diffed against
// `Teams.import`, `Teams.export`, `Teams.pack` and `Teams.unpack`.
//
// Input (JSON, from the file given as argv[2] or stdin): an array of teams,
// each in export or packed format (anything `Teams.import` reads).
//
// Output, per team:
//   @@@ export      Teams.export(Teams.import(team)), trailing spaces removed
//   @@@ pack        Teams.pack(Teams.import(team))
//   @@@ repack      Teams.pack(Teams.unpack(Teams.pack(Teams.import(team))))
//   @@@ end
// with `null` in all three when `Teams.import` returns null.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const { Teams } = require('pokemon-showdown');

const teams = JSON.parse(readFileSync(process.argv[2] ?? 0, 'utf8'));
const out = [];
for (const text of teams) {
  const sets = Teams.import(text);
  if (!sets) {
    out.push('@@@ export', 'null', '@@@ pack', 'null', '@@@ repack', 'null', '@@@ end');
    continue;
  }
  const exported = Teams.export(sets).split('\n').map(l => l.trimEnd()).join('\n').trimEnd();
  const packed = Teams.pack(sets);
  out.push('@@@ export', exported, '@@@ pack', packed, '@@@ repack', Teams.pack(Teams.unpack(packed)), '@@@ end');
}
process.stdout.write(out.join('\n') + '\n');
