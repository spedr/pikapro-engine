//! Hostile inputs for everything that reads text from a player: team import
//! (export and packed formats), `unpack_sets` and `choose`. None may panic,
//! and the work must stay proportional to the input (a 4 MB species field
//! takes seconds if the dex lookups re-normalise it for every row).

use std::time::{Duration, Instant};

use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, Format, Seed, SideId};
use pikapro_protocol::{TeamSheet, choose, export_sheet, import_sheet, pack_team, unpack_sets, unpack_team};

const MB: usize = 1 << 20;

/// Generous for a debug build: the linear work these do takes milliseconds
/// in a release build.
const LIMIT: Duration = Duration::from_secs(2);

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    assert!(took < LIMIT, "{what}: {took:?}");
    out
}

/// Every reader on `text`, in an EV and a Stat Point format; what imports
/// must export, pack and unpack again.
fn read_team(what: &str, text: &str) {
    for format in [Format::gen9_singles(), Format::champions_vgc()] {
        timed(what, || {
            let _ = unpack_sets(text);
            let _ = unpack_team(text, &format);
            if let Ok(sheet) = import_sheet(text, &format) {
                let _ = export_sheet(&sheet, &format);
                let packed = pack_team(&sheet, &format);
                let _ = unpack_team(&packed, &format);
                let _ = sheet.nicknames(&format);
            }
        });
    }
}

#[test]
fn huge_fields() {
    // A 4 MB species field, packed and exported.
    let species = "a".repeat(4 * MB);
    read_team("4 MB species", &format!("Pikachu|{species}|||Protect|||||||"));
    read_team("4 MB species", &format!("{species}\n- Protect\n"));
    let long = |c: &str| c.repeat(MB / c.len());
    for filler in ["a", " ", "|", "]", ",", "é", "\u{1f600}", "0", "\u{300}"] {
        let big = long(filler);
        read_team("species", &format!("Pikachu|{big}|||Protect|||||||"));
        read_team("name", &format!("{big}|Pikachu|||Protect|||||||"));
        read_team("misc", &format!("Pikachu||||Protect|||||||{big}"));
        read_team("moves", &format!("Pikachu||||{big}|||||||"));
        read_team("evs", &format!("Pikachu||||Protect||{big}|||||"));
        read_team("level", &format!("Pikachu||||Protect||||||{big}|"));
        read_team("export species", &format!("{big}\n- Protect\n"));
        read_team("export nickname", &format!("{big} (Pikachu)\n- Protect\n"));
        read_team("export move", &format!("Pikachu\n- {big}\n"));
        read_team("export evs", &format!("Pikachu\nEVs: {big}\n- Protect\n"));
        read_team("export level", &format!("Pikachu\nLevel: {big}\n- Protect\n"));
        read_team("export item", &format!("Pikachu @ {big}\n- Protect\n"));
        read_team("text", &big);
    }
}

#[test]
fn many_sets_and_lines() {
    let set = "Pikachu||||Protect|||||||";
    let packed = vec![set; 50_000].join("]");
    let format = Format::gen9_singles();
    let sheet = timed("50k packed sets", || unpack_team(&packed, &format).unwrap());
    assert_eq!(sheet.team.sets.len(), 24);
    read_team("50k packed sets", &packed);
    read_team("50k export sets", &vec!["Pikachu\n- Protect\n"; 50_000].join("\n"));
    read_team("100k moves", &format!("Pikachu\n{}", "- Protect\n".repeat(100_000)));
    read_team("1M empty lines", &"\n".repeat(MB));
    read_team("500k separators", &"---\n===\n".repeat(MB / 4));
}

/// Random text built from the characters the formats give meaning to.
#[test]
fn random_garbage() {
    const PIECES: &[&str] = &[
        "|",
        "]",
        ",",
        "\n",
        " ",
        "(",
        ")",
        " @ ",
        " (M)",
        "Pikachu",
        "Garchomp",
        "Protect",
        "- ",
        "EVs: ",
        "IVs: ",
        "Level: ",
        "Happiness: ",
        "Ability: ",
        "Hidden Power [",
        "Nature",
        "252",
        "0x",
        "-",
        "e9",
        "S",
        "H",
        "é",
        "\u{1f600}",
        "\u{feff}",
        "\u{85}",
        "\u{202e}",
        "\u{300}",
        "\r",
        "\t",
        "NaN",
        "Infinity",
        "===",
        "---",
    ];
    let mut rng = BattleRng::new(Seed::fast(0x686f_7374));
    for _ in 0..3000 {
        let len = rng.random(60) as usize;
        let text: String = (0..len).map(|_| PIECES[rng.random(PIECES.len() as u32) as usize]).collect();
        read_team("garbage", &text);
    }
}

fn battle(format: Format) -> Battle {
    let teams = [sample_team(&format), sample_team(&format)];
    Battle::new(format, teams, BattleOptions::new(Seed::fast(1))).unwrap()
}

#[test]
fn hostile_choices() {
    let big = |s: &str| s.repeat(MB / s.len());
    let mut inputs: Vec<String> = [
        "",
        " ",
        ",",
        "team",
        "team ",
        "move",
        "switch",
        "move 0",
        "move -1",
        "switch 0",
        "switch -0",
        "switch 0x",
        "switch 0x3",
        "switch 1e3",
        "team 0x2, 1",
        "move 1 +3",
        "move 1 -3",
        "move 1\u{3000}1",
        "move 1 mega mega",
        "switch \u{85}3",
        "team \u{1f600}",
        "move 99999999999999999999999999",
        "switch 99999999999999999999999",
        "team 999999999999999999999999, 1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for filler in ["9", ",", " ", "a", "move 1,", "\u{1f600}", "0x", "-"] {
        inputs.push(big(filler));
        inputs.push(format!("move {}", big(filler)));
        inputs.push(format!("switch {}", big(filler)));
        inputs.push(format!("team {}", big(filler)));
        inputs.push(format!("move 1 {}", big(filler)));
    }
    let names = [String::new(), "A".repeat(MB)];
    for format in [Format::gen9_singles(), Format::gen9_doubles(), Format::gen9_vgc()] {
        let b = battle(format);
        for input in &inputs {
            timed("choose", || choose(&b, SideId::P1, input, &names, None));
        }
    }
    let format = Format::gen9_singles();
    let sheet = TeamSheet::new(sample_team(&format));
    let _ = sheet.nicknames(&format);
}

/// Numbers no server sends are clamped, not wrapped: a team size or PP
/// past 255.
#[test]
fn request_numbers_clamp() {
    use pikapro::request::{MoveChoices, RequestKind};
    use pikapro_protocol::{Perspective, Renderer, channel::parse_request};
    let request = |format: &Format| {
        let renderer = Renderer::new(format, Perspective::Omniscient);
        renderer.request_json(&battle(format.clone()).request_data(SideId::P1))
    };
    let format = Format::champions_vgc();
    let json = request(&format).replacen("\"maxChosenTeamSize\":4", "\"maxChosenTeamSize\":257", 1);
    let r = parse_request(&json, &format).unwrap();
    assert_eq!(r.kind, RequestKind::TeamPreview { max_chosen_team_size: Some(255) });

    let format = Format::gen9_singles();
    let json = request(&format);
    let at = json.find("\"pp\":").unwrap() + "\"pp\":".len();
    let end = at + json[at..].find(',').unwrap();
    let json = format!("{}300{}", &json[..at], &json[end..]);
    let r = parse_request(&json, &format).unwrap();
    let RequestKind::Move { active } = r.kind else { panic!("{:?}", r.kind) };
    let Some(Some(a)) = active.first() else { panic!("{active:?}") };
    let MoveChoices::Moves(moves) = a.moves else { panic!("{:?}", a.moves) };
    assert_eq!(moves[0].pp, 255);
}
