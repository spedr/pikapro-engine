//! Mega Stones (the items `items::owner` assigns to
//! [`Owner::Mega`](super::Owner::Mega)).
//!
//! A Mega Stone's only callback is `onTakeItem`: it can't be taken from, or
//! given to, a Pokémon it works for. Showdown has three versions, all
//! reading the Pokémon's `baseSpecies` (the event's target: the holder
//! when an item is taken, the receiver when Trick or Thief gives it):
//!
//! - most stones: `!item.megaStone?.[source.baseSpecies.baseSpecies]`, the
//!   base species' name (Charizard, and so Charizard-Mega-X);
//! - Floettite, Meowsticite, Magearnite and Tatsugirinite, whose keys are
//!   formes: `!item.megaStone[source.baseSpecies.name] &&
//!   !Object.values(item.megaStone).includes(source.baseSpecies.name)`;
//! - Zygardite: `source.baseSpecies.baseSpecies !== 'Zygarde'`.
//!
//! Z-Crystals are Past in Gen 9 and Champions; their callbacks (`onTakeItem:
//! false`, ...) are not modelled.
//!
//! Mega Evolution itself is `resolve/mega.rs`.

use pikapro_data::gen9::{items as i, species};
use pikapro_data::{Event, EventPrefix, ItemId};

use crate::battle::Battle;
use crate::resolve::events::{EventCtx, Relay};
use crate::state::Position;

/// Runs `item`'s callback for `ev.event` / `ev.prefix` on its holder.
pub(super) fn run(battle: &mut Battle, ev: &mut EventCtx<'_>, holder: Position, item: ItemId) -> Option<Relay> {
    let dex = battle.format.dex;
    let stone = dex.item(item);
    match (ev.event, ev.prefix) {
        (Event::TakeItem, EventPrefix::On) if !stone.mega.is_empty() => {
            let base = battle.state.side(holder.side).team.get(holder.slot as usize)?.base_species;
            let base_of_base = dex.species(base).base_species;
            let stuck = match item {
                i::ZYGARDITE => base_of_base == species::ZYGARDE,
                i::FLOETTITE | i::MEOWSTICITE | i::MAGEARNITE | i::TATSUGIRINITE => {
                    stone.mega.iter().any(|&(from, to)| from == base || to == base)
                }
                _ => stone.mega.iter().any(|&(from, _)| from == base_of_base),
            };
            Some(Relay::from_bool(!stuck))
        }
        _ => None,
    }
}
