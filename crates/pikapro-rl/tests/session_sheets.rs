//! Open Team Sheets through a channel session: a sheet fills a team
//! preview wildcard with the forme the details will show, crowns Zacian
//! and Zamazenta only at the start, and a sheet fed late doesn't give back
//! an item a Pokémon may have lost.

use pikapro::{PokemonSet, SideId, Team};
use pikapro_data::gen9::{abilities, items, moves, species};
use pikapro_rl::session::{ChannelSession, SessionOptions};
use pikapro_view::{ItemState, MonFlags};

fn session(format: &pikapro::Format) -> ChannelSession {
    let mut s = ChannelSession::for_format(format.clone(), true, SessionOptions::default()).unwrap();
    s.set_side(SideId::P2);
    s
}

fn sheet_line(team: Team, format: &pikapro::Format) -> String {
    let packed = pikapro_protocol::pack_team(&pikapro_protocol::TeamSheet::new(team), format);
    format!("|showteam|p1|{packed}")
}

fn set(species: pikapro_data::SpeciesId) -> PokemonSet {
    PokemonSet::new(species).with_level(50).with_moves(&[moves::SURF])
}

/// Greninja-Bond's details show Greninja: the sheet fills the wildcard
/// row with that, and the switch-in doesn't change it.
#[test]
fn a_sheet_fills_the_forme_the_details_show() {
    let format = pikapro::Format::gen9_vgc();
    let team = Team::new(vec![
        set(species::GRENINJA_BOND).with_ability(abilities::BATTLE_BOND),
        set(species::PIKACHU),
        set(species::RAICHU),
        set(species::PICHU),
    ]);
    let mut s = session(&format);
    for l in [
        "|player|p1|a|",
        "|player|p2|b|",
        "|poke|p1|Greninja-*, L50, M|",
        "|poke|p1|Pikachu, L50, M|",
        "|poke|p1|Raichu, L50, M|",
        "|poke|p1|Pichu, L50, M|",
        "|teampreview|4",
    ] {
        s.feed(l);
    }
    s.feed(&sheet_line(team, &format));
    s.flush();
    let row = s.tracker().side(SideId::P1).mons[0];
    assert_eq!((row.species, row.details_species), (species::GRENINJA, species::GRENINJA));
    assert!(!row.flags.has(MonFlags::FORME_HIDDEN));
    assert_eq!(row.base_ability, pikapro_view::AbilityState::Known(abilities::BATTLE_BOND));
    s.feed("|start");
    s.feed("|switch|p1a: Greninja|Greninja, L50, M|100/100");
    assert_eq!(s.tracker().side(SideId::P1).mons[0].species, species::GRENINJA);
}

/// A Zacian holding a Rusted Sword is Zacian at team preview and crowned
/// from the start: the sheet (which shows it crowned) fills it so.
#[test]
fn a_crowned_sheet_crowns_at_the_start() {
    let format = pikapro::Format::gen9_vgc();
    let zacian = PokemonSet::new(species::ZACIAN)
        .with_level(50)
        .with_item(items::RUSTED_SWORD)
        .with_moves(&[moves::IRON_HEAD, moves::PROTECT]);
    let team = Team::new(vec![zacian, set(species::PIKACHU), set(species::RAICHU), set(species::PICHU)]);
    let mut s = session(&format);
    for l in [
        "|player|p1|a|",
        "|player|p2|b|",
        "|poke|p1|Zacian-*, L50|",
        "|poke|p1|Pikachu, L50, M|",
        "|poke|p1|Raichu, L50, M|",
        "|poke|p1|Pichu, L50, M|",
        "|teampreview|4",
    ] {
        s.feed(l);
    }
    s.feed(&sheet_line(team, &format));
    s.flush();
    assert_eq!(s.tracker().side(SideId::P1).mons[0].species, species::ZACIAN, "before the start");
    s.feed("|start");
    let row = s.tracker().side(SideId::P1).mons[0];
    assert_eq!((row.species, row.details_species), (species::ZACIAN_CROWNED, species::ZACIAN_CROWNED));
}

/// A sheet fed after the start fills what can't have changed, but not the
/// item of a Pokémon that has been on the field.
#[test]
fn a_late_sheet_leaves_the_items_of_pokemon_seen() {
    let format = pikapro::Format::gen9_vgc();
    let team = Team::new(vec![
        set(species::PIKACHU).with_item(items::STICKY_BARB),
        set(species::RAICHU).with_item(items::LEFTOVERS),
        set(species::PICHU),
        set(species::SNORLAX),
    ]);
    let mut s = session(&format);
    for l in [
        "|player|p1|a|",
        "|player|p2|b|",
        "|poke|p1|Pikachu, L50, M|",
        "|poke|p1|Raichu, L50, M|",
        "|poke|p1|Pichu, L50, M|",
        "|poke|p1|Snorlax, L50, M|",
        "|teampreview|4",
        "|start",
        "|switch|p1a: Pikachu|Pikachu, L50, M|100/100",
    ] {
        s.feed(l);
    }
    s.flush();
    s.feed(&sheet_line(team, &format));
    let side = s.tracker().side(SideId::P1);
    assert_eq!(side.mons[0].item, ItemState::Unknown, "Pikachu was seen: its barb may have moved");
    assert_eq!(side.mons[1].item, ItemState::Known(items::LEFTOVERS), "Raichu wasn't");
    assert!(side.mons[0].revealed(moves::SURF).is_some(), "its moves are its own");
}
