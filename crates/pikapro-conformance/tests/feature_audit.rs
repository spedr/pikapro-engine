//! The feature audit (`view::features`): every derived value of a
//! player's view against what a perfect observer of the public log
//! computes under its documented meaning, over battles of four presets
//! (all but championsbss; legal sets, the fuzzer's teams, the targeted
//! generators). A rule that breaks fails the test;
//! `fixed_findings_stay_fixed` keeps the repro of each finding the audit
//! made.
//!
//! Scale: `PIKAPRO_FEATURE_AUDIT_BATTLES` battles per format and source
//! (default 12); `cargo run --release -p pikapro-conformance --bin
//! feature_audit` runs thousands, and a teams file.

use pikapro::Format;
use pikapro_conformance::view::features::{FeatureAudit, audit_battle};
use pikapro_conformance::view::{Driver, Generator, Setup, Source, Target};

fn formats() -> [Format; 7] {
    [
        Format::champions_vgc(),
        Format::gen9_vgc(),
        Format::gen9_doubles(),
        Format::gen9_singles(),
        Format::gen9_ou(),
        Format::gen9_random_battle(),
        Format::gen9_random_doubles_battle(),
    ]
}

fn audit(format: &Format, source: Source, seeds: impl Iterator<Item = u64>) -> FeatureAudit {
    let g = Generator::new(format, source).unwrap_or_else(|e| panic!("{e}"));
    let mut audit = FeatureAudit::new(3);
    for seed in seeds {
        let setup = g.setup(seed);
        audit_battle(&setup, Driver::random(seed), &mut audit).unwrap_or_else(|f| panic!("{} {seed}: {f}", format.id));
    }
    audit
}

#[test]
fn feature_audit_matches_the_reference() {
    let n: u64 = std::env::var("PIKAPRO_FEATURE_AUDIT_BATTLES").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
    let mut total = FeatureAudit::new(3);
    for format in formats() {
        let targets: Vec<Target> = Target::ALL.into_iter().filter(|t| t.applies(&format)).collect();
        for (k, seed) in (9_300_000..9_300_000 + n).enumerate() {
            let target = targets[k % targets.len()];
            for source in [Source::Sets, Source::Fuzz, Source::Target(target)] {
                total.merge(audit(&format, source, std::iter::once(seed)));
            }
        }
    }
    let broken = total.broken();
    assert!(broken.is_empty(), "rules broken: {broken:?}\n{}", total.report(3));
    // The audit compared something of every kind.
    for rule in [
        "turns_active",
        "status_turns toxic",
        "status_turns sleep",
        "sleep_from_rest",
        "confusion_checks",
        "weather_turns drift",
        "pp_spent",
        "last_move",
        "mega_used",
    ] {
        assert!(total.rules.get(rule).is_some_and(|t| t.checked > 0), "{rule} never checked");
    }
}

/// A source's generator for battle `seed`, as the `feature_audit` binary
/// picks it for `--source targets` and `--source combos`.
fn cycled(list: &[Target], format: &Format, seed: u64) -> Source {
    let list: Vec<Target> = list.iter().copied().filter(|t| t.applies(format)).collect();
    Source::Target(list[(seed % list.len() as u64) as usize])
}

/// The findings of the feature audit, each with the repro that broke its
/// rule: the rule is checked there, and holds.
#[test]
fn fixed_findings_stay_fixed() {
    let champions = Format::champions_vgc();
    let doubles = Format::gen9_doubles();
    let vgc = Format::gen9_vgc();
    let singles = Format::gen9_singles();
    let one = |format: &Format, source: Source, seed: u64| audit(format, source, seed..seed + 1);
    let fixed: [(&str, FeatureAudit); 20] = [
        // Turn 8: confused after it moved, 1 turn and 0 checks; turn 9, 2
        // turns and 1 check.
        ("confusion_checks", one(&champions, Source::Sets, 9_100_032)),
        // Turn 4: frozen, 1 attempt made (Champions thaws at the third).
        ("status_turns freeze", one(&champions, Source::Sets, 9_100_004)),
        // Turn 13: a Sleep Talk turn (`cant|slp`, then the move) counted
        // two attempts.
        ("status_turns sleep", one(&champions, Source::Target(Target::SleepTalk), 9_100_000)),
        // Turn 5: Expanding Force in Psychic Terrain (`[spread]`) hit the
        // Pressure foe it didn't name.
        ("pp_spent (foe has Pressure)", one(&champions, Source::Sets, 9_100_284)),
        // Dragon Darts at a foe hits its ally too (`smartTarget`).
        ("pp_spent (foe has Pressure)", one(&doubles, cycled(&Target::ALL, &doubles, 9_101_187), 9_101_187)),
        // Turn 3: a Sky Attack charged at a Pressure foe that switched out
        // before the release; Pressure took its PP at the charge.
        ("pp_spent (foe has Pressure)", one(&champions, cycled(&Target::COMBOS, &champions, 9_101_938), 9_101_938)),
        // Turn 12: a Leppa Berry gave Darkest Lariat 10 PP back.
        ("pp_spent", one(&champions, Source::Target(Target::RevivalBlessing), 9_100_154)),
        // Turn 9: Oricorio's Dancer copy of Teeter Dance hit itself in
        // confusion; its own Beak Blast after was taken for the copy.
        ("pp_spent", one(&doubles, cycled(&Target::COMBOS, &doubles, 9_100_124), 9_100_124)),
        ("last_move", one(&doubles, cycled(&Target::COMBOS, &doubles, 9_100_124), 9_100_124)),
        // A replacement after the residuals: the volatiles' durations had
        // counted `|turn|` lines, a turn behind the field's.
        ("durations after the residuals", one(&champions, Source::Sets, 9_100_052)),
        // Rest's sleep (3 turns; 2 or 3 otherwise in Champions) wasn't
        // encoded.
        ("sleep_from_rest", one(&champions, Source::Target(Target::SleepTalk), 9_100_003)),
        // A charge's Pressure when the release may name another foe: Follow
        // Me drew the charge (turn 4), Storm Drain the release (turn 8),
        // Ally Switch turned a charge at the ally into one at its user
        // (turn 9).
        ("pp_spent (foe has Pressure)", one(&doubles, Source::Fuzz, 9_101_750)),
        ("pp_spent (foe has Pressure)", one(&doubles, Source::Uniform, 9_108_912)),
        ("pp_spent (foe has Pressure)", one(&champions, Source::Uniform, 9_101_676)),
        // Turn 8: a Choice lock (into Metronome) failed the release of the
        // Meteor Beam it called: no `lastMove`.
        ("last_move", one(&vgc, cycled(&Target::COMBOS, &vgc, 9_102_259), 9_102_259)),
        // Turn 8: Slow Start's count stops under Neutralizing Gas.
        ("slow_start_turns drift", one(&vgc, Source::Uniform, 9_102_616)),
        // Turn 9: Spite on the move Sketch copied.
        ("pp_spent", one(&vgc, Source::Fuzz, 9_104_428)),
        // Turn 13: a transformed Neutralizing Gas holder suppresses nothing
        // (`ignoringAbility`), Pressure included.
        ("pp_spent (foe has Pressure)", one(&doubles, cycled(&Target::COMBOS, &doubles, 9_207_418), 9_207_418)),
        // Tera Blast (`mustpressure`) with no target
        // (`[notarget]`): Pressure takes nothing.
        ("pp_spent (foe has Pressure)", one(&doubles, cycled(&Target::ALL, &doubles, 9_302_624), 9_302_624)),
        // A Raging Fury Copycat called ended silently (its user was
        // confused already); the next one, naming its target, is a use.
        ("pp_spent", one(&singles, Source::Fuzz, 9_206_869)),
    ];
    for (rule, audit) in &fixed {
        let t = audit.rules.get(rule).unwrap_or_else(|| panic!("{rule} never checked"));
        assert!(t.checked > 0 && t.bad == 0, "{rule} broke again\n{}", audit.report(3));
        assert!(audit.broken().is_empty(), "{rule}: {:?}\n{}", audit.broken(), audit.report(3));
    }
    // The `sleep_from_rest` repro sees a Rest sleep.
    let rest = &fixed[10].1.histograms["asleep from Rest at a decision"];
    assert!(rest.get(&1).is_some_and(|&n| n > 0), "{rest:?}");
}

/// A team whose set names a Mega forme (`Charizard-Mega-Y @
/// Charizardite Y`, as tournament pastes do) goes through the training
/// importer (`pikapro_rl::teams::import_pool`) as Showdown's validator
/// leaves it (`validateForme`: `set.species = species.battleOnly`), so the
/// battle starts in the base forme with the Mega Evolution unused, and the
/// audit finds nothing.
#[test]
fn a_listed_mega_forme_is_imported_as_its_base_forme() {
    let format = Format::champions_vgc();
    let g = Generator::new(&format, Source::Sets).expect("a generator");
    let dex = format.dex;
    let mut checked = 0;
    for seed in 9_100_000..9_100_200 {
        let mut setup: Setup = g.setup(seed);
        // Its first set holding a Mega Stone, named as its Mega forme.
        let Some(i) = setup.sheets[0]
            .team
            .sets
            .iter()
            .position(|s| s.item.is_some_and(|i| dex.item(i).mega.iter().any(|&(base, _)| base == s.species)))
        else {
            continue;
        };
        let set = &mut setup.sheets[0].team.sets[i];
        let base = set.species;
        let item = set.item.expect("a Mega Stone");
        let &(_, mega) = dex.item(item).mega.iter().find(|&&(b, _)| b == base).expect("its Mega forme");
        set.species = mega;
        let text = pikapro_protocol::export_sheet(&setup.sheets[0], &format);
        let (pool, rejected) = pikapro_rl::teams::import_pool(&[text], &format);
        assert!(rejected.is_empty(), "seed {seed}: {rejected:?}");
        assert_eq!(pool[0].team.sets[i].species, base, "seed {seed}: {} kept its Mega forme", dex.species(mega).name);
        setup.sheets[0] = pool[0].clone();
        let mut audit = FeatureAudit::new(3);
        audit_battle(&setup, Driver::random(seed), &mut audit).expect("a battle");
        let broken = audit.broken();
        assert!(broken.is_empty(), "seed {seed}: {broken:?}\n{}", audit.report(3));
        checked += 1;
        if checked == 10 {
            return;
        }
    }
    panic!("only {checked} teams with a Mega Stone");
}
