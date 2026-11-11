#!/usr/bin/env node
// Prints allowlist.py's COSMETIC_GROUPS from the pinned Showdown
// (tools/showdown-oracle, `npm ci` first):
//
//   node tools/view-oracle/diff/cosmetic.mjs
//
// A group is the formes of one species that differ only in looks: the same
// base species, base stats, types and abilities, in the gen9 or the
// champions dex. Formes a battle changes into (`battleOnly`, a required
// move), Totem, Gigantamax and custom formes are left out, so a group never
// hides a forme change the view must follow (Cherrim-Sunshine,
// Mimikyu-Busted, Minior-Meteor) or a Mega Evolution.
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, '../../showdown-oracle/package.json'));
const { Dex } = require('pokemon-showdown/dist/sim');

const groups = new Map();
for (const mod of ['gen9', 'champions']) {
  const dex = Dex.mod(mod);
  for (const s of dex.species.all()) {
    if (!s.exists || s.battleOnly || s.requiredMove || s.isTotem) continue;
    if (['Gigantamax', 'Custom', 'CAP'].includes(s.isNonstandard) || /gmax$|totem$/.test(s.id)) continue;
    const key = [s.baseSpecies, JSON.stringify(s.baseStats), s.types.join('/'), JSON.stringify(s.abilities)].join('|');
    if (!groups.has(key)) groups.set(key, new Set());
    groups.get(key).add(s.id);
    for (const c of s.cosmeticFormes || []) groups.get(key).add(dex.species.get(c).id);
  }
}
const out = [...groups.values()].filter((g) => g.size > 1).map((g) => [...g].sort());
out.sort((a, b) => (a[0] < b[0] ? -1 : 1));
console.log('COSMETIC_GROUPS: Tuple[FrozenSet[str], ...] = (');
for (const g of out) console.log(`    frozenset({${g.map((id) => `"${id}"`).join(', ')}}),`);
console.log(')');
