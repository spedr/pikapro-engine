// Read the status-order unit test's tab-separated cases from stdin. Inspect
// Showdown's real collector and sorter without replacing any callbacks.
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const { Battle, PRNG } = require('pokemon-showdown');
const covered = new Map();
const statusEvent = /^on(?:Ally|Any|Foe|Source)?(?:SetStatus|TryAddVolatile)$/;

for (const line of readFileSync(0, 'utf8').trim().split('\n')) {
  const [formatid, seedText, room, terrain, abilitiesText, speedsText, targetText, charge, clause] = line.split('\t');
  const abilities = abilitiesText.split(',');
  const speeds = speedsText.split(',').map(Number);
  const seed = Number(seedText);
  const b = new Battle({ formatid, seed: [0, 0, 0, 1] });
  if (!covered.has(b.dex)) covered.set(b.dex, new Set());
  for (const ability of abilities) covered.get(b.dex).add(ability);
  const n = b.activePerHalf;
  const items = ['Lum Berry', 'Light Ball', 'Mental Herb', 'Safety Goggles'];
  for (let side = 0; side < 2; side++) {
    b.setPlayer(`p${side + 1}`, { team: Array.from({ length: n }, (_, slot) => ({
      species: 'Mew', gender: 'N', level: 50, ability: abilities[side * 2 + slot],
      item: items[side * 2 + slot], moves: ['splash'],
    })) });
  }
  if (b.requestState === 'teampreview') {
    const choice = `team ${Array.from({ length: n }, (_, i) => i + 1).join('')}`;
    b.makeChoices(choice, choice);
  }
  const pokemon = b.sides.flatMap(s => s.active);
  const index = p => p.side.n * 2 + p.position;
  const source = b.p2.active[0];
  // Native random formats install this rule at construction. The custom
  // Champions probe can install it too, before move-created field effects.
  if (clause === '1' && !b.field.pseudoWeather.sleepclausemod) b.field.addPseudoWeather('sleepclausemod', source);
  for (const id of ['gravity', 'magicroom', 'wonderroom', 'fairylock', 'echoedvoice']) b.field.addPseudoWeather(id, source);
  if (room === '1') b.field.addPseudoWeather('trickroom', source);
  b.field.setWeather('sunnyday', source);
  if (terrain !== '-') b.field.setTerrain(terrain, source);
  for (const side of b.sides) {
    for (const id of ['reflect', 'mist', 'safeguard', 'lightscreen']) side.addSideCondition(id, side.active[0]);
  }
  for (const p of pokemon) {
    p.status = 'brn';
    p.volatiles = {};
    const ids = index(p) % 2 ? ['uproar', charge] : [charge, 'uproar'];
    for (const id of ids) p.volatiles[id] = { id, target: p };
    p.side.slotConditions[p.position] = { wish: { id: 'wish', target: p.side, isSlotCondition: true } };
    p.storedStats.spe = speeds[index(p)];
    p.boosts.spe = 0;
    p.updateSpeed();
  }
  const targetIndex = Number(targetText);
  const target = b.sides[targetIndex >> 1].active[targetIndex % 2];
  function describe(h, event) {
    const holder = h.effectHolder;
    const where = pokemon.includes(holder) ? `p${index(holder)}` : b.sides.includes(holder) ? `s${holder.n}` : 'f';
    const prefix = ['', 'Ally', 'Any', 'Foe', 'Source'].find(p => h.effect[`on${p}${event}`] === h.callback);
    return `${h.effect.id}:${prefix || 'On'}@${where}/${h.order || 0}/${h.priority || 0}/${h.speed || 0}/${h.subOrder || 0}/${h.effectOrder || 0}`;
  }
  const output = [];
  for (const event of ['SetStatus', 'TryAddVolatile']) {
    const handlers = b.findEventHandlers(target, event, source);
    const before = handlers.map(h => describe(h, event)).join(',');
    b.prng = new PRNG([0, 0, seed >>> 16, seed & 65535]);
    const draws = [];
    const random = b.prng.random.bind(b.prng);
    b.prng.random = (from, to) => {
      const r = random(from, to);
      draws.push(`${to === undefined ? from : to - from}:${to === undefined ? r : r - from}`);
      return r;
    };
    b.speedSort(handlers);
    output.push(before, handlers.map(h => describe(h, event)).join(','), draws.join(','));
  }
  process.stdout.write(output.join('\t') + '\n');
  b.destroy();
}
// Keep the ability inventory complete, and verify that items really have
// no callbacks for these events rather than just omitting them in Rust.
for (const [dex, abilities] of covered) {
  for (const ability of dex.abilities.all()) {
    if (Object.keys(ability).some(k => statusEvent.test(k)) && !abilities.has(ability.id)) {
      throw new Error(`untested status ability: ${ability.id}`);
    }
  }
  for (const item of dex.items.all()) {
    if (Object.keys(item).some(k => statusEvent.test(k))) throw new Error(`untested status item: ${item.id}`);
  }
}
