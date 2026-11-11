//! pikapro-protocol's team formats against Pokémon Showdown's
//! (`tools/showdown-oracle/teams.mjs`): random teams with genders,
//! nicknames and the other extras go through our export, and Showdown's
//! `Teams.import` / `export` / `pack` / `unpack` must agree with ours.
//! Skipped (with a message) when Node or the oracle's dependencies are
//! missing.

use std::io::Write as _;
use std::process::{Command, Stdio};

use pikapro::{BattleRng, Format, Seed};
use pikapro_conformance::{oracle_available, oracle_dir, random_team};
use pikapro_data::Type;
use pikapro_data::gen9::moves;
use pikapro_protocol::{SetExtras, TeamSheet, export_sheet, import_sheet, json_string, pack_team, unpack_team};

const NICKNAMES: &[&str] = &["Chompy", "Mr. Nice Guy", "O'Brien", "Zap 2", "Sir-Hops-A-Lot", "Élan", "Pikachu", "x"];

fn random_sheet(format: &Format, rng: &mut BattleRng) -> TeamSheet {
    let mut team = random_team(format, rng);
    let balls: Vec<&str> = format.dex.items.iter().map(|i| i.name).filter(|n| n.ends_with(" Ball")).collect();
    let mut extras = Vec::new();
    for set in &mut team.sets {
        set.level = 1 + rng.random(100) as u8;
        if rng.random(4) == 0 && !set.moves.is_full() {
            set.moves
                .push([moves::HIDDEN_POWER_FIRE, moves::HIDDEN_POWER_ICE, moves::HIDDEN_POWER][rng.random(3) as usize]);
        }
        let chance = |rng: &mut BattleRng| rng.random(3) == 0;
        extras.push(SetExtras {
            name: if chance(rng) {
                NICKNAMES[rng.random(NICKNAMES.len() as u32) as usize].to_string()
            } else {
                String::new()
            },
            shiny: chance(rng),
            happiness: chance(rng).then(|| rng.random(255) as u8),
            pokeball: if chance(rng) {
                balls[rng.random(balls.len() as u32) as usize].to_string()
            } else {
                String::new()
            },
            hp_type: chance(rng).then(|| Type::ALL[rng.random(Type::ALL.len() as u32) as usize]),
            gigantamax: chance(rng),
            dynamax_level: None,
        });
    }
    TeamSheet { team, extras }
}

/// Runs `teams.mjs`; per team, its `(export, pack, repack)`.
fn showdown(teams: &[String]) -> Vec<[String; 3]> {
    let json = format!("[{}]", teams.iter().map(|t| json_string(t)).collect::<Vec<_>>().join(","));
    let mut child = Command::new("node")
        .arg(oracle_dir().join("teams.mjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("node runs");
    child.stdin.take().unwrap().write_all(json.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "teams.mjs failed");
    let text = String::from_utf8(out.stdout).unwrap();
    let mut results = Vec::new();
    let mut sections: Vec<Vec<&str>> = Vec::new();
    for line in text.lines() {
        match line.strip_prefix("@@@ ") {
            Some("end") => {
                let [a, b, c] = <[Vec<&str>; 3]>::try_from(std::mem::take(&mut sections)).expect("three sections");
                results.push([a.join("\n"), b.join("\n"), c.join("\n")]);
            }
            Some(_) => sections.push(Vec::new()),
            None => sections.last_mut().expect("inside a section").push(line),
        }
    }
    results
}

#[test]
fn team_formats_match_showdown() {
    if !oracle_available() {
        eprintln!("skipped: the Showdown oracle is not installed");
        return;
    }
    let mut failures = Vec::new();
    for format in Format::presets() {
        let mut rng = BattleRng::new(Seed::fast(1_700_000));
        let sheets: Vec<TeamSheet> = (0..40).map(|_| random_sheet(format, &mut rng)).collect();
        let texts: Vec<String> = sheets.iter().map(|s| export_sheet(s, format)).collect();
        for (i, ((sheet, ours), [export, pack, repack])) in sheets.iter().zip(&texts).zip(showdown(&texts)).enumerate()
        {
            let tag = format!("{} team {i}", format.id);
            if export != ours.trim_end() {
                failures.push(format!("{tag}: export\n--- ours\n{ours}\n--- showdown\n{export}"));
            }
            let our_pack = pack_team(sheet, format);
            if pack != our_pack {
                failures.push(format!("{tag}: pack\n  ours     {our_pack}\n  showdown {pack}"));
            }
            if repack != pack {
                failures.push(format!("{tag}: Showdown's unpack changed the team\n  {pack}\n  {repack}"));
            }
            match unpack_team(&pack, format) {
                Ok(s) if &s == sheet => {}
                other => failures.push(format!("{tag}: unpack of Showdown's pack\n  {other:?}")),
            }
            match import_sheet(&export, format) {
                Ok(s) if &s == sheet => {}
                other => failures.push(format!("{tag}: import of Showdown's export\n  {other:?}")),
            }
        }
        // The packed format's Dynamax level, which the export format's
        // import ignores.
        let mut sheet = sheets[0].clone();
        sheet.extras[0].dynamax_level = Some(3);
        let ours = pack_team(&sheet, format);
        let [_, pack, _] = &showdown(std::slice::from_ref(&ours))[0];
        if pack != &ours {
            failures.push(format!("{}: dynamax level\n  ours     {ours}\n  showdown {pack}", format.id));
        }
        assert_eq!(unpack_team(&ours, format).unwrap(), sheet);
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n\n"));
}

/// Hand-written pastes in the spellings Showdown's import accepts: ours
/// must read them as Showdown does (compared through the export). Two
/// differences are by design, so the pastes avoid them: our sets always
/// have an ability and a nature (written even when the paste had none),
/// and names are written in their canonical spelling (`adamant nature`
/// comes back as `Adamant Nature`; Showdown keeps what was typed).
#[test]
fn showdown_spellings_import_alike() {
    if !oracle_available() {
        eprintln!("skipped: the Showdown oracle is not installed");
        return;
    }
    let pastes = [
        "=== [gen9] Folder/Team ===\n\nChompy (Garchomp) (F) @ No Item\nTrait: Rough Skin\nLevel: 50\nEVs: 4 hp / 252 Attack / 252 Spd\nAdamant Nature\nIVs: 0 spa\n~Earthquake\n-Frustration\n- Hidden Power [Fire]\n---\nGarchomp (Garchomp) (M)\nAbility: Sand Veil\nHappiness: 3\nDynamax Level: 4\nHardy Nature\n- Frustration\n",
        "Snorlax\nAbility: Thick Fat\nShiny: Yes\nPokeball: Great Ball\nHidden Power: Fire\nGigantamax: Yes\nTera Type: Stellar\nCalm Nature\n- Return\n",
        "Rotom-Wash (M)\nAbility: Levitate\nBold Nature\n- Protect\n\nRotom (Rotom-Wash)\nAbility: Levitate\nBold Nature\n- Protect\n",
        "Garchomp||ChoiceScarf|H|Earthquake,Protect|Jolly|,252,,,4,252|F|,,,0,,|S|50|120,Ice,GreatBall,G,3,Ground]Pikachu|||Static|Thunderbolt|Hardy||||||",
    ];
    let format = Format::gen9_singles();
    let texts: Vec<String> = pastes.iter().map(|p| p.to_string()).collect();
    let mut failures = Vec::new();
    for (paste, [export, pack, _]) in pastes.iter().zip(showdown(&texts)) {
        let sheet = import_sheet(paste, &format).unwrap_or_else(|e| panic!("{e}\n{paste}"));
        let ours = export_sheet(&sheet, &format);
        if ours.trim_end() != export {
            failures.push(format!("export\n--- ours\n{ours}\n--- showdown\n{export}"));
        }
        if pack_team(&sheet, &format) != pack {
            failures.push(format!("pack\n  ours     {}\n  showdown {pack}", pack_team(&sheet, &format)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Odd and hostile texts, hand-written and random: where our import reads
/// one, Showdown's must read the same team (compared through the export
/// and the packed form); where Showdown's `Teams.import` returns `null`,
/// ours must fail. Ours may refuse what Showdown keeps (unknown names,
/// numbers a set can't hold), which Showdown's validator then rejects.
#[test]
fn hostile_texts_import_alike() {
    if !oracle_available() {
        eprintln!("skipped: the Showdown oracle is not installed");
        return;
    }
    let mut texts: Vec<String> = [
        "Pikachu\nAbility: Static\nEVs: 252  Atk / 4 HP / 0x10 Def / 12abc Spe / 4 Atack / 8\nIVs: x Atk / 7 SpA / 0x1f Spe\nHardy Nature\n- Protect\n",
        "Pikachu\nAbility: Static\nLevel: 0\nHardy Nature\n- Protect\n",
        "Pikachu\nAbility: Static\nLevel: 0x32\nHappiness: 0x10\nHardy Nature\n- Protect\n",
        "Pikachu\nAbility: Static\nLevel: 5e1\nHardy Nature\n- Protect\n",
        "Pikachu\nAbility: Static\nLevel:  7 \nHardy Nature\n- Protect\n",
        "Pikachu\nAbility: Static\nHardy Nature\n- Hidden Power [\n",
        "Pikachu\nAbility: Static\nHardy Nature\nundefined Nature\n- Protect\n",
        "Pikachu\nAbility: Static\nEVs: 4 HP\nEVs: 8 Atk\nHardy Nature\n- Protect\n",
        "AB (Pikachu) (F) @ Light Ball @ Choice Scarf\nAbility: Static\nHardy Nature\n- Protect\n",
        "  Zap [x], (Pikachu)  \nAbility: Static\nHardy Nature\n- Protect\n",
        "\u{feff}Pikachu\u{feff}\nAbility: Static\nHardy Nature\n- Protect\n",
        "\n\nPikachu||LightBall|Static|Protect|Hardy||||||\n\n",
        " \nPikachu||LightBall|Static|Protect|Hardy||||||",
        "A]B|Pikachu||Static|Protect|Hardy|||0,0|||",
        "Pikachu|||Static|Protect|Hardy|,1|M|,5||0x32|3,Fire,FooBall2,G,4,Water,extra,more",
        "Pikachu|||Static|Protect|Hardy|||||abc|",
        "Pikachu|||Static|Protect|Hardy|||||0|",
        "Greninja|||S|WaterShuriken|Hardy||||||",
        "Pikachu|||Static|Protect|Hardy||||||]",
        "Pikachu|||Static|Protect",
        "Pikachu|||Static|Protect|Hardy||||||]Garchomp|||RoughSkin|Protect|Hardy||||||",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Many sets: Showdown's unpack reads 24.
    texts.push(vec!["Pikachu|||Static|Protect|Hardy||||||"; 30].join("]"));
    // Random exports with one line garbled.
    const NOISE: &[&str] = &["  ", " ", "0x", "e1", "abc", "]", "|", ",", "[", "\u{3000}", "\u{85}", "-", "00"];
    let format = Format::gen9_singles();
    let mut rng = BattleRng::new(Seed::fast(1_710_000));
    for _ in 0..300 {
        let text = export_sheet(&random_sheet(&format, &mut rng), &format);
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let k = rng.random(lines.len() as u32) as usize;
        let line = &mut lines[k];
        let mut at = rng.random(line.len() as u32 + 1) as usize;
        while !line.is_char_boundary(at) {
            at -= 1;
        }
        line.insert_str(at, NOISE[rng.random(NOISE.len() as u32) as usize]);
        texts.push(lines.join("\n"));
    }
    let mut failures = Vec::new();
    let mut compared = 0;
    // Showdown keeps names as typed (`Mud[dy Water`) where ours are
    // canonical, so the teams are compared through Showdown's packed form,
    // read back by ours: the same team if both read the text alike (Poke
    // Balls, which both keep as written, by their packed names). A `|` in a
    // nickname can't be packed, so no text has one.
    let balls = |mut sheet: TeamSheet| {
        for e in &mut sheet.extras {
            e.pokeball = pikapro_protocol::team::pack_name(&e.pokeball);
        }
        sheet
    };
    for (text, [_, pack, _]) in texts.iter().zip(showdown(&texts)) {
        // Nor can a `]` in a Tera or Hidden Power type, which Showdown keeps
        // as typed and packs raw: its packed team splits there.
        let raw_type = |l: &str| (l.starts_with("Tera Type:") || l.starts_with("Hidden Power:")) && l.contains(']');
        if text.lines().any(raw_type) {
            continue;
        }
        match import_sheet(text, &format) {
            Ok(_) if pack == "null" => failures.push(format!("{text:?}: Showdown's import is null, ours isn't")),
            Ok(sheet) => {
                compared += 1;
                match unpack_team(&pack, &format).map(balls) {
                    Ok(theirs) if theirs == balls(sheet.clone()) => {}
                    theirs => failures.push(format!(
                        "{text:?}\n  showdown {pack}\n  ours     {}\n  {theirs:?}",
                        pack_team(&sheet, &format)
                    )),
                }
            }
            Err(_) => {}
        }
    }
    assert!(compared > 100, "only {compared} texts imported");
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n\n"));
}
