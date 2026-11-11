//! Tests of the vector environment. Seeds from 4,300,000.

use pikapro::testing::sample_team;
use pikapro::{Battle, BattleOptions, BattleRng, MAX_TEAM, PokemonSet, Request, Seed, SlotAction, Team};
use pikapro_data::gen9::{abilities, moves, species};

use pikapro_view::PlayerView;

use super::*;
use crate::vec::{EnvEndReason, RANDOM_ACTION, status};

const SEED: u64 = 4_300_000;

pub(super) fn sample_pair(format: &Format) -> Teams {
    Teams::Pairs(vec![Arc::new([TeamSheet::new(sample_team(format)), TeamSheet::new(sample_team(format))])])
}

/// `n` battles of the sample teams, with the default observation.
pub(super) fn config(format: Format, n: usize) -> EnvConfig {
    let teams = sample_pair(&format);
    EnvConfig { seed: SEED, spin: Duration::from_micros(20), ..EnvConfig::new(format, n, teams) }
}

pub(super) fn env(config: EnvConfig, threads: usize) -> VecEnv {
    VecEnv::new(config, threads).unwrap()
}

/// A random action for each player its mask asks, `-1` for the others,
/// drawn by the test's own RNG from the masks and the pairing rule.
pub(super) fn sample(env: &VecEnv, b: &StepBuffers, rng: &mut BattleRng) -> Vec<i64> {
    let space = env.space();
    let shape = env.shape();
    let per = env.actions_per_player();
    let mut actions = vec![-1i64; shape.n * 2 * per];
    for k in 0..shape.n * 2 {
        if !b.needs_action[k] {
            continue;
        }
        let mask = &b.masks[k * shape.mask..][..shape.mask];
        let acts = &mut actions[k * per..][..per];
        let pick = |rng: &mut BattleRng, xs: &[usize]| xs[rng.random(xs.len() as u32) as usize];
        let set = |m: &[bool]| m.iter().enumerate().filter(|(_, b)| **b).map(|(i, _)| i).collect::<Vec<_>>();
        if env.config().masks == MaskLayout::Joint {
            acts[0] = pick(rng, &set(mask)) as i64;
            continue;
        }
        let slots = space.active * space.slot_size;
        let preview = set(&mask[slots..]);
        if !preview.is_empty() {
            acts[0] = pick(rng, &preview) as i64;
            continue;
        }
        let first = set(&mask[..space.slot_size]);
        assert!(!first.is_empty(), "an asked player has actions");
        loop {
            let a0 = pick(rng, &first);
            acts[0] = a0 as i64;
            if space.active == 1 {
                break;
            }
            let mut pair = vec![false; space.slot_size];
            space.pair_mask(&mask[..slots], b.min_switches[k], a0, &mut pair);
            let second = set(&pair);
            if !second.is_empty() {
                acts[1] = pick(rng, &second) as i64;
                break;
            }
        }
    }
    actions
}

/// Plays `steps` steps from a reset with random actions from the masks;
/// returns every step's outputs.
pub(super) fn play(env: &VecEnv, steps: usize, seed: u64) -> Vec<StepBuffers> {
    let mut rng = BattleRng::new(Seed::fast(seed));
    let mut b = env.buffers();
    env.reset(Some(SEED), &mut b.as_out()).unwrap();
    let mut all = vec![b.clone()];
    for _ in 0..steps {
        let actions = sample(env, &b, &mut rng);
        env.step(&actions, &mut b.as_out()).unwrap();
        all.push(b.clone());
    }
    all
}

#[test]
fn steps_are_deterministic_for_any_thread_count() {
    for format in [Format::champions_vgc(), Format::gen9_singles()] {
        let mut c = config(format.clone(), 7);
        c.opponent = OpponentSpec::One(Opponent::External);
        let one = play(&env(c.clone(), 1), 120, 1);
        for (threads, shards) in [(2, 1), (3, 8), (7, 2)] {
            let c = EnvConfig { shards_per_thread: shards, ..c.clone() };
            assert!(one == play(&env(c, threads), 120, 1), "{} {threads} threads", format.id);
        }
        // Battles ended, restarted, and every submitted action came from a
        // mask: none was rejected.
        assert!(one.iter().any(|b| b.terminated.iter().any(|&t| t)));
        assert!(one.iter().all(|b| !b.status.contains(&status::REJECTED)));
        assert!(one.iter().skip(1).any(|b| b.status.contains(&status::SUBMITTED)));
        for b in &one {
            let shape = b.shape.unwrap();
            for k in 0..shape.n * 2 {
                let mask = &b.masks[k * shape.mask..][..shape.mask];
                assert_eq!(b.needs_action[k], mask.iter().any(|&m| m), "{}", format.id);
            }
        }
    }
}

#[test]
fn built_in_opponents_are_never_asked() {
    let e = env(config(Format::champions_vgc(), 4), 2);
    for b in play(&e, 100, 2) {
        for i in 0..4 {
            assert!(!b.needs_action[2 * i + 1] && b.status[2 * i + 1] == status::IGNORED);
            // Player 0 is asked every step, except when a step ended its
            // episode.
            assert_eq!(b.needs_action[2 * i], b.end_reason[i] == 0);
        }
    }
}

#[test]
fn masks_equal_the_request_s_actions() {
    // Every jointly legal choice the masks allow is one the battle accepts
    // (no hidden trap or Imprison in the sample teams), and every choice
    // the battle lists is in the masks.
    for (format, layout, by) in [
        (Format::champions_vgc(), MaskLayout::Factored, SwitchBy::Row),
        (Format::gen9_vgc(), MaskLayout::Joint, SwitchBy::Position),
        (Format::gen9_doubles(), MaskLayout::Factored, SwitchBy::Position),
        (Format::gen9_singles(), MaskLayout::Factored, SwitchBy::Row),
    ] {
        let c = EnvConfig {
            masks: layout,
            switch_by: by,
            opponent: OpponentSpec::One(Opponent::External),
            ..config(format.clone(), 3)
        };
        let e = env(c, 1);
        let space = e.space().clone();
        let mut rng = BattleRng::new(Seed::fast(3));
        let mut b = e.buffers();
        e.reset(Some(SEED), &mut b.as_out()).unwrap();
        for _ in 0..150 {
            let actions = sample(&e, &b, &mut rng);
            let shape = b.shape.unwrap();
            for i in 0..shape.n {
                e.with_env(i, |env| {
                    for p in 0..2 {
                        let k = 2 * i + p;
                        if !b.needs_action[k] {
                            continue;
                        }
                        let side = env.slot.side_of(p);
                        let legal = *env.battle.legal_actions(side);
                        let own = &env.slot.own[side.index()];
                        let row = |a: SlotAction| match (a, by) {
                            (SlotAction::Switch(pos), SwitchBy::Row) => {
                                space.encode_slot(&SlotAction::Switch(own.mons[pos as usize].id))
                            }
                            _ => space.encode_slot(&a),
                        };
                        let mask = &b.masks[k * shape.mask..][..shape.mask];
                        if !matches!(legal.request(), Request::Move | Request::ForceSwitch { .. }) {
                            continue;
                        }
                        let joint = legal.joint();
                        let listed: Vec<[usize; 2]> = joint
                            .iter()
                            .map(|c| {
                                let Choice::Actions(a) = c else { unreachable!() };
                                [row(a[0]).unwrap(), if space.active > 1 { row(a[1]).unwrap() } else { 0 }]
                            })
                            .collect();
                        let allowed = |a0: usize, a1: usize| match layout {
                            MaskLayout::Factored => space.pair_legal(mask, b.min_switches[k], a0, a1),
                            MaskLayout::Joint => mask[space.joint_index(&[a0, a1][..space.active]).unwrap()],
                        };
                        let a1s = if space.active > 1 { space.slot_size } else { 1 };
                        for a0 in 0..space.slot_size {
                            for a1 in 0..a1s {
                                assert_eq!(
                                    allowed(a0, a1),
                                    listed.contains(&[a0, a1]),
                                    "{} {a0} {a1} {:?}",
                                    format.id,
                                    legal.request()
                                );
                            }
                        }
                    }
                });
            }
            e.step(&actions, &mut b.as_out()).unwrap();
        }
    }
}

#[test]
fn actions_outside_the_mask_are_rejected() {
    let e = env(config(Format::champions_vgc(), 2), 1);
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    let before = b.clone();
    // Out of range, a preview order past the range, and too few actions.
    for bad in [vec![999, 0, 0, 0, 999, 0, 0, 0], vec![-1; 8], vec![360, 0, 0, 0, 360, 0, 0, 0]] {
        e.step(&bad, &mut b.as_out()).unwrap();
        assert_eq!(b.status, [status::REJECTED, 0, status::REJECTED, 0]);
        assert_eq!((&b.obs, &b.masks, &b.needs_action), (&before.obs, &before.masks, &before.needs_action));
    }
    assert!(e.step(&[0; 3], &mut b.as_out()).is_err());
    // A step before the first reset fails.
    let fresh = env(config(Format::champions_vgc(), 2), 1);
    assert!(fresh.step(&[0; 8], &mut b.as_out()).is_err());
}

/// Runs battle 0 with random actions until its episode ends; returns the
/// terminal step's outputs, then the next step's.
fn to_the_end(e: &VecEnv, rng: &mut BattleRng, b: &mut StepBuffers) -> (StepBuffers, StepBuffers) {
    loop {
        let actions = sample(e, b, rng);
        e.step(&actions, &mut b.as_out()).unwrap();
        if b.end_reason[0] != 0 {
            let terminal = b.clone();
            let actions = sample(e, b, rng);
            e.step(&actions, &mut b.as_out()).unwrap();
            return (terminal, b.clone());
        }
    }
}

#[test]
fn next_step_returns_the_terminal_observation_then_resets() {
    let e = env(config(Format::champions_vgc(), 1), 1);
    let mut rng = BattleRng::new(Seed::fast(SEED));
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    assert_eq!(b.episode, [0]);
    for episode in 0..3u64 {
        let (end, next) = to_the_end(&e, &mut rng, &mut b);
        assert_eq!((end.terminated[0], end.truncated[0], end.end_reason[0]), (true, false, 1));
        assert!(end.winner[0] == 0 || end.winner[0] == 1);
        let won = if end.winner[0] == 0 { 1.0 } else { -1.0 };
        assert_eq!(end.reward, [won, -won]);
        assert_eq!(end.episode, [episode]);
        assert!(end.needs_action.iter().all(|&x| !x) && end.masks.iter().all(|&x| !x));
        // The next step starts episode 1 of battle 0 (n = 1), ignoring the
        // actions.
        assert_eq!(next.episode, [episode + 1]);
        assert_eq!((next.status, next.reward, next.end_reason), (vec![0, 0], vec![0.0, 0.0], vec![0]));
        assert!(next.needs_action[0]);
    }
}

#[test]
fn same_step_autoreset_matches_next_step() {
    let format = Format::gen9_singles();
    let next = env(config(format.clone(), 1), 1);
    let same = env(EnvConfig { autoreset: Autoreset::SameStep, ..config(format, 1) }, 1);
    let (mut bn, mut bs) = (next.buffers(), same.buffers());
    next.reset(Some(SEED), &mut bn.as_out()).unwrap();
    same.reset(Some(SEED), &mut bs.as_out()).unwrap();
    let mut rng = BattleRng::new(Seed::fast(SEED + 1));
    let mut ended = 0;
    while ended < 3 {
        let actions = sample(&next, &bn, &mut rng);
        next.step(&actions, &mut bn.as_out()).unwrap();
        same.step(&actions, &mut bs.as_out()).unwrap();
        assert_eq!((&bn.reward, &bn.end_reason, &bn.winner), (&bs.reward, &bs.end_reason, &bs.winner));
        if bn.end_reason[0] == 0 {
            assert_eq!((&bn.obs, &bn.masks, &bn.episode), (&bs.obs, &bs.masks, &bs.episode));
            continue;
        }
        ended += 1;
        // Same-step: the terminal observation in the final buffers, the
        // next episode's first in the main ones.
        assert_eq!(bs.final_obs, bn.obs);
        assert_eq!(
            (bs.final_episode[0], bs.final_turns[0]),
            (bn.episode[0], next.with_env(0, |e| e.battle.turn()).unwrap())
        );
        next.step(&actions, &mut bn.as_out()).unwrap();
        assert_eq!((&bn.obs, &bn.masks, &bn.episode), (&bs.obs, &bs.masks, &bs.episode));
    }
}

#[test]
fn rewards_are_zero_sum_per_episode() {
    for reward in [RewardPreset::Terminal, RewardPreset::KO] {
        let c =
            EnvConfig { reward, opponent: OpponentSpec::One(Opponent::External), ..config(Format::champions_vgc(), 3) };
        let e = env(c, 2);
        let mut sums = [[0f32; 2]; 3];
        let mut shaped = false;
        for b in play(&e, 300, 4) {
            for i in 0..3 {
                for (sum, &r) in sums[i].iter_mut().zip(&b.reward[2 * i..][..2]) {
                    *sum += r;
                    // A reward between decisions is shaping.
                    shaped |= b.end_reason[i] == 0 && r != 0.0;
                }
                if b.end_reason[i] != 0 {
                    assert!((sums[i][0] + sums[i][1]).abs() < 1e-5, "{reward:?} {sums:?}");
                    let terminal = if b.winner[i] == 2 { 0.0 } else { 1.0 };
                    let expect = if b.winner[i] == 0 { terminal } else { -terminal };
                    // With KO shaping the winner took every foe down: +0.4
                    // at most, and the loser's KOs take some back.
                    assert!((sums[i][0] - expect).abs() <= 0.4 + 1e-5, "{reward:?} {sums:?}");
                    if reward == RewardPreset::Terminal {
                        assert_eq!(sums[i][0], expect);
                    }
                    sums[i] = [0.0; 2];
                }
            }
        }
        assert_eq!(shaped, reward != RewardPreset::Terminal);
    }
}

#[test]
fn episodes_replay_through_a_plain_battle() {
    let c = EnvConfig {
        record: true,
        randomize_sides: true,
        teams: Teams::Generator(
            crate::teams::generator(&Format::gen9_vgc(), pikapro_teams::Mode::Sets, Default::default()).unwrap(),
        ),
        ..config(Format::gen9_vgc(), 3)
    };
    let e = env(c, 2);
    let mut rng = BattleRng::new(Seed::fast(SEED + 2));
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    let mut replayed = 0;
    for _ in 0..400 {
        let actions = sample(&e, &b, &mut rng);
        e.step(&actions, &mut b.as_out()).unwrap();
        for i in 0..3 {
            if b.end_reason[i] == 0 {
                continue;
            }
            let record = e.record(i, false).unwrap();
            let (result, turn) = e.with_env(i, |env| (env.battle.result(), env.battle.turn())).unwrap();
            assert_eq!(record.seed, episode_seed(SEED, record.episode));
            assert_eq!(record.agent_side as u8, b.agent_side[i]);
            let teams = [record.sheets[0].team.clone(), record.sheets[1].team.clone()];
            let mut battle =
                Battle::new(Format::gen9_vgc(), teams, BattleOptions::new(Seed::fast(record.seed))).unwrap();
            for &(side, choice) in &record.choices {
                battle.submit(side, choice).unwrap();
            }
            assert_eq!((battle.result(), battle.turn()), (result, turn));
            replayed += 1;
        }
    }
    assert!(replayed > 3);
    assert!(e.record(0, true).is_some());
}

#[test]
fn max_turns_truncates() {
    let c = EnvConfig { max_turns: Some(2), ..config(Format::champions_vgc(), 2) };
    let e = env(c, 1);
    let mut cut = 0;
    for b in play(&e, 40, 5) {
        for i in 0..2 {
            if b.end_reason[i] != 0 {
                assert_eq!(b.end_reason[i], EnvEndReason::MaxTurns as u8);
                assert!(b.truncated[i] && !b.terminated[i]);
                assert_eq!((b.winner[i], b.reward[2 * i], b.reward[2 * i + 1]), (-1, 0.0, 0.0));
                cut += 1;
            }
        }
    }
    assert!(cut >= 4);
    // Same-step: a cut episode lasted `max_turns` turns.
    let c = EnvConfig { max_turns: Some(2), autoreset: Autoreset::SameStep, ..config(Format::champions_vgc(), 2) };
    let e = env(c, 1);
    let mut cut = 0;
    for b in play(&e, 40, 5) {
        for i in 0..2 {
            if b.end_reason[i] == EnvEndReason::MaxTurns as u8 {
                assert_eq!(b.final_turns[i], 2);
                cut += 1;
            }
        }
    }
    assert!(cut >= 4);
}

#[test]
fn preview_modes() {
    for preview in [PreviewMode::Learned, PreviewMode::Random, PreviewMode::Fixed] {
        let e = env(EnvConfig { preview, ..config(Format::champions_vgc(), 2) }, 1);
        let mut b = e.buffers();
        e.reset(Some(SEED), &mut b.as_out()).unwrap();
        let space = e.space();
        let slots = space.active * space.slot_size;
        let mask = &b.masks[..b.shape.unwrap().mask];
        let orders = mask[slots..].iter().filter(|&&m| m).count();
        let request = e.with_env(0, |env| env.battle.request(env.slot.side_of(0))).unwrap();
        match preview {
            PreviewMode::Learned => {
                assert_eq!((orders, matches!(request, Request::TeamPreview { .. })), (360, true));
            }
            _ => {
                assert_eq!((orders, request), (0, Request::Move));
                assert!(mask[..slots].iter().any(|&m| m));
            }
        }
        if preview == PreviewMode::Fixed {
            // The first four, in order: team positions 0..4 lead.
            let order: Vec<u8> = e
                .with_env(0, |env| env.battle.state().side(env.slot.side_of(0)).team.iter().map(|m| m.id).collect())
                .unwrap();
            assert_eq!(order, [0, 1, 2, 3]);
        }
    }
}

#[test]
fn opponents_change_from_the_next_episode() {
    let c = EnvConfig { max_turns: Some(1), ..config(Format::gen9_singles(), 4) };
    let e = env(c, 2);
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    e.set_opponent(Some(&[1, 3]), Opponent::External).unwrap();
    assert!(e.set_opponent(Some(&[4]), Opponent::External).is_err());
    let mut rng = BattleRng::new(Seed::fast(SEED + 3));
    // The episodes under way keep their built-in opponent.
    let actions = sample(&e, &b, &mut rng);
    e.step(&actions, &mut b.as_out()).unwrap();
    assert!((0..4).all(|i| !b.needs_action[2 * i + 1]));
    for _ in 0..6 {
        let actions = sample(&e, &b, &mut rng);
        e.step(&actions, &mut b.as_out()).unwrap();
    }
    let external: Vec<bool> = (0..4).map(|i| e.with_env(i, |env| env.slot.opponent.is_external()).unwrap()).collect();
    assert_eq!(external, [false, true, false, true]);
    // A mixture: both kinds come up, from the episodes' seeds.
    let mix = OpponentSpec::Mix(Arc::from(vec![(Opponent::random(), 1.0), (Opponent::External, 1.0)]));
    let c = EnvConfig { opponent: mix, ..config(Format::gen9_singles(), 16) };
    let e = env(c, 1);
    e.reset(Some(SEED), &mut e.buffers().as_out()).unwrap();
    let kinds: Vec<bool> = (0..16).map(|i| e.with_env(i, |env| env.slot.opponent.is_external()).unwrap()).collect();
    assert!(kinds.contains(&true) && kinds.contains(&false));
    let bad = OpponentSpec::Mix(Arc::from(vec![(Opponent::random(), 0.0)]));
    assert!(e.set_opponent(None, bad).is_err());
}

#[test]
fn sides_can_be_randomised() {
    let c = EnvConfig { randomize_sides: true, ..config(Format::gen9_singles(), 16) };
    let e = env(c, 1);
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    assert!(b.agent_side.contains(&0) && b.agent_side.contains(&1));
    // Player 0 is asked, whichever side it plays.
    assert!((0..16).all(|i| b.needs_action[2 * i] && !b.needs_action[2 * i + 1]));
    for i in 0..16 {
        let side = e.with_env(i, |env| env.slot.side_of(0) as u8).unwrap();
        assert_eq!(side, b.agent_side[i]);
    }
}

#[test]
fn team_pools_and_open_sheets() {
    let format = Format::champions_vgc();
    let g = crate::teams::generator(&format, pikapro_teams::Mode::Sets, Default::default()).unwrap();
    let pool: Arc<[TeamSheet]> = (0..5).flat_map(|s| crate::teams::pair(&g, s).unwrap()).collect::<Vec<_>>().into();
    let c = EnvConfig { teams: Teams::PerSide([pool.clone(), pool.clone()]), ots: 1.0, ..config(format, 4) };
    let e = env(c, 1);
    e.reset(Some(SEED), &mut e.buffers().as_out()).unwrap();
    for i in 0..4 {
        e.with_env(i, |env| {
            assert!(env.sheets.iter().all(|s| pool.contains(s)));
            assert!(env.slot.tracker.ots.iter().all(|o| o.is_some_and(|o| o.mons.len() == 6)));
        });
    }
    assert!(VecEnv::new(EnvConfig { ots: 1.5, ..config(Format::gen9_singles(), 1) }, 1).is_err());
}

/// Player row `k`'s observation and what it is asked.
fn player_rows(b: &StepBuffers, k: usize) -> (Vec<u32>, Vec<bool>, u8, bool) {
    let shape = b.shape.unwrap();
    let obs = b.obs[k * shape.obs..][..shape.obs].iter().map(|x| x.to_bits()).collect();
    (obs, b.masks[k * shape.mask..][..shape.mask].to_vec(), b.min_switches[k], b.needs_action[k])
}

#[test]
fn one_sided_open_sheets() {
    // The same battles, the same actions, in three environments: closed
    // sheets, open sheets, and open to player 0 alone. Player 1's rows are
    // those of the game with closed sheets; player 0's those of the game
    // with open sheets.
    let format = Format::champions_vgc();
    let g = crate::teams::generator(&format, pikapro_teams::Mode::Sets, Default::default()).unwrap();
    let pool: Arc<[TeamSheet]> = (0..8).flat_map(|s| crate::teams::pair(&g, s).unwrap()).collect::<Vec<_>>().into();
    let n = 8;
    let base = EnvConfig {
        teams: Teams::PerSide([pool.clone(), pool]),
        opponent: OpponentSpec::One(Opponent::External),
        randomize_sides: true,
        ..config(format, n)
    };
    let open = EnvConfig { ots: 1.0, ..base.clone() };
    let mine = EnvConfig { ots_view: OtsView::Player0, ..open.clone() };
    let envs = [env(base.clone(), 1), env(open, 1), env(mine, 2)];
    let mut bufs: Vec<StepBuffers> = envs.iter().map(|e| e.buffers()).collect();
    for (e, b) in envs.iter().zip(&mut bufs) {
        e.reset(Some(SEED), &mut b.as_out()).unwrap();
    }
    let mut rng = BattleRng::new(Seed::fast(SEED));
    let (mut differs, mut ended) = ([false; 2], 0);
    for _ in 0..300 {
        let [closed, open, mine] = &bufs[..] else { unreachable!() };
        for i in 0..n {
            let (p0, p1) = (2 * i, 2 * i + 1);
            assert_eq!(player_rows(mine, p1), player_rows(closed, p1), "battle {i}: player 1 sees closed sheets");
            assert_eq!(player_rows(mine, p0), player_rows(open, p0), "battle {i}: player 0 sees open sheets");
            differs[0] |= player_rows(mine, p0) != player_rows(closed, p0);
            differs[1] |= player_rows(open, p1) != player_rows(closed, p1);
        }
        ended += closed.terminated.iter().filter(|&&t| t).count();
        let actions = sample(&envs[2], mine, &mut rng);
        for (e, b) in envs.iter().zip(&mut bufs) {
            e.step(&actions, &mut b.as_out()).unwrap();
        }
    }
    assert_eq!(differs, [true; 2], "the sheets show in the views they reach");
    assert!(ended > n, "episodes ended and restarted: {ended}");
    let series = EnvConfig { ots_view: OtsView::Player0, series: Some(3), ..base };
    assert!(VecEnv::new(series, 1).is_err());
}

#[test]
fn switches_name_observation_rows() {
    // After switches, a switch action's index is the Pokémon's identity,
    // not its position; `SwitchBy::Position` keeps Showdown's.
    let format = Format::gen9_singles();
    for by in [SwitchBy::Row, SwitchBy::Position] {
        let e = env(EnvConfig { switch_by: by, ..config(format.clone(), 1) }, 1);
        let space = e.space().clone();
        let mut b = e.buffers();
        e.reset(Some(SEED), &mut b.as_out()).unwrap();
        let mut checked = 0;
        for _ in 0..60 {
            let (ids, active_id) = e
                .with_env(0, |env| {
                    let own = &env.slot.own[env.slot.side_of(0).index()];
                    (own.mons.iter().map(|m| m.id).collect::<Vec<_>>(), own.mons[0].id)
                })
                .unwrap();
            if b.end_reason[0] == 0 && b.needs_action[0] {
                // Switch to the first benched Pokémon that can come in.
                let mask = &b.masks[..space.slot_size];
                let target = (1..ids.len()).find(|&pos| {
                    let k = if by == SwitchBy::Row { ids[pos] as usize } else { pos };
                    mask[space.switch_base() + k]
                });
                if let Some(pos) = target {
                    let k = if by == SwitchBy::Row { ids[pos] as usize } else { pos };
                    e.step(&[(space.switch_base() + k) as i64, 0], &mut b.as_out()).unwrap();
                    assert_eq!(b.status[0], status::SUBMITTED);
                    let now = e.with_env(0, |env| env.slot.own[env.slot.side_of(0).index()].mons[0].id).unwrap();
                    // The chosen Pokémon is in (unless it fainted at once
                    // and was replaced by a forced switch: rare).
                    if b.end_reason[0] == 0 && ids[pos] != active_id && now == ids[pos] {
                        checked += 1;
                    }
                    continue;
                }
            }
            e.step(&[RANDOM_ACTION, 0], &mut b.as_out()).unwrap();
        }
        assert!(checked > 5, "{by:?} {checked}");
    }
}

/// A singles battle where p2's Wobbuffet traps p1 with Shadow Tag, which
/// p1's request doesn't show.
fn trapped_env(opponent: Opponent) -> VecEnv {
    let format = Format::gen9_singles();
    let p1 = Team::new(vec![
        PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE]),
        PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE]),
    ]);
    let p2 = Team::new(vec![
        PokemonSet::new(species::WOBBUFFET).with_ability(abilities::SHADOW_TAG).with_moves(&[moves::SPLASH]),
    ]);
    let teams = Teams::Pairs(vec![Arc::new([TeamSheet::new(p1), TeamSheet::new(p2)])]);
    let c = EnvConfig { opponent: OpponentSpec::One(opponent), ..EnvConfig::new(format, 1, teams) };
    env(c, 1)
}

#[test]
fn a_hidden_trap_makes_a_switch_unavailable() {
    {
        let e = trapped_env(Opponent::External);
        let space = e.space().clone();
        let switch = (space.switch_base() + 1) as i64;
        let mut b = e.buffers();
        e.reset(Some(SEED), &mut b.as_out()).unwrap();
        // Turn 1: not trapped yet. Both use their move.
        assert!(b.masks[switch as usize]);
        e.step(&[0, 0], &mut b.as_out()).unwrap();
        assert_eq!(b.status, [status::SUBMITTED; 2]);
        // Turn 2: the request says `maybeTrapped`, and the mask keeps the
        // switch (the engine's legal actions don't have it).
        let trapped = |e: &VecEnv| {
            e.with_env(0, |env| {
                let a = env.slot.own[0].active[0].unwrap();
                (a.maybe_trapped, a.trapped)
            })
            .unwrap()
        };
        assert_eq!(trapped(&e), (true, false));
        assert!(b.masks[switch as usize]);
        // The switch is unavailable: p1 must choose again, its view says
        // trapped and its mask lost the switch; p2's choice waits.
        e.step(&[switch, 0], &mut b.as_out()).unwrap();
        assert_eq!(b.status, [status::UNAVAILABLE, status::SUBMITTED]);
        assert_eq!(b.needs_action, [true, false]);
        assert_eq!(trapped(&e), (false, true));
        assert!(!b.masks[switch as usize] && b.masks[0]);
        // Choosing again resolves the turn.
        e.step(&[0, 0], &mut b.as_out()).unwrap();
        assert_eq!(b.status, [status::SUBMITTED, status::IGNORED]);
        assert_eq!(b.needs_action, [true, true]);
        assert_eq!(trapped(&e), (true, false), "a new request");
    }
}

#[test]
fn built_in_players_choose_again_after_an_unavailable_choice() {
    // Player 0 always switches while its mask allows it: with a random
    // opponent, the battle goes on (the opponent is never stuck either).
    let e = trapped_env(Opponent::random());
    let space = e.space().clone();
    let switch = (space.switch_base() + 1) as i64;
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    let mut unavailable = 0;
    for _ in 0..20 {
        let a = if b.masks[switch as usize] { switch } else { 0 };
        e.step(&[a, 0], &mut b.as_out()).unwrap();
        unavailable += usize::from(b.status[0] == status::UNAVAILABLE);
        assert_ne!(b.status[0], status::REJECTED);
    }
    assert!(unavailable >= 1);
}

#[test]
fn imprison_makes_a_move_unavailable() {
    let format = Format::gen9_doubles();
    let p1 = Team::new(vec![
        PokemonSet::new(species::PIKACHU).with_moves(&[moves::TACKLE, moves::SPLASH]),
        PokemonSet::new(species::SNORLAX).with_moves(&[moves::TACKLE, moves::SPLASH]),
    ]);
    let p2 = Team::new(vec![
        PokemonSet::new(species::SNORLAX).with_moves(&[moves::IMPRISON, moves::TACKLE]),
        PokemonSet::new(species::SNORLAX).with_moves(&[moves::SPLASH]),
    ]);
    let teams = Teams::Pairs(vec![Arc::new([TeamSheet::new(p1), TeamSheet::new(p2)])]);
    // Two Pokémon bring both, in order (the preview space has orders of
    // four only).
    let c = EnvConfig {
        opponent: OpponentSpec::One(Opponent::External),
        preview: PreviewMode::Fixed,
        ..EnvConfig::new(format, 1, teams)
    };
    let e = env(c, 1);
    let space = e.space().clone();
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    let splash = space.encode_slot(&SlotAction::Move { index: 1, target: None, gimmick: None }).unwrap() as i64;
    let imprison = space.encode_slot(&SlotAction::Move { index: 0, target: None, gimmick: None }).unwrap() as i64;
    e.step(&[splash, splash, imprison, imprison], &mut b.as_out()).unwrap();
    assert_eq!(b.status, [status::SUBMITTED; 2]);
    let tackle = space
        .encode_slot(&SlotAction::Move { index: 0, target: Some(pikapro::Target::Foe(0)), gimmick: None })
        .unwrap();
    let slot1 = |b: &StepBuffers, a: usize| b.masks[space.slot_size + a];
    // The last active Pokémon's request shows Tackle usable; the first's
    // doesn't.
    assert!(!b.masks[tackle] && slot1(&b, tackle));
    e.step(&[splash, tackle as i64, imprison, 0], &mut b.as_out()).unwrap();
    assert_eq!(b.status, [status::UNAVAILABLE, status::SUBMITTED]);
    assert!(!slot1(&b, tackle) && slot1(&b, splash as usize));
    let own = e.with_env(0, |env| env.slot.own[0].active[1].unwrap()).unwrap();
    assert!(!own.maybe_disabled && !own.maybe_locked, "doubles: updateDisabledRequest clears them");
    e.step(&[splash, splash, 0, 0], &mut b.as_out()).unwrap();
    assert_eq!(b.status, [status::SUBMITTED, status::IGNORED]);
}

#[test]
fn own_rows_are_team_sheet_order() {
    // The row a switch names is the Pokémon's identity: its index on the
    // team sheet, whatever the team order.
    let e = env(config(Format::champions_vgc(), 1), 1);
    e.reset(Some(SEED), &mut e.buffers().as_out()).unwrap();
    e.with_env(0, |env| {
        let own = &env.slot.own[0];
        let mut ids: Vec<u8> = own.mons.iter().map(|m| m.id).collect();
        ids.sort();
        assert_eq!(ids, (0..MAX_TEAM as u8).collect::<Vec<_>>());
    });
}

/// With open sheets, each sheet entry is its own Pokémon's: foe row `k`
/// (team preview's order) reads entry `k`, whatever order the foe's
/// Pokémon come in.
#[test]
fn open_sheets_land_on_their_pokemon() {
    use crate::encode::ViewCtx;
    let format = Format::champions_vgc();
    let tables = crate::tables::Tables::for_format(&format);
    let dex = format.dex;
    let g = crate::teams::generator(&format, pikapro_teams::Mode::Sets, Default::default()).unwrap();
    let c = EnvConfig {
        teams: Teams::Generator(g),
        ots: 1.0,
        opponent: OpponentSpec::One(Opponent::External),
        ..config(format.clone(), 8)
    };
    let e = env(c, 1);
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    let mut rng = BattleRng::new(Seed::fast(SEED + 9));
    let (mut revealed, mut out_of_order) = (0, 0);
    for _ in 0..40 {
        let actions = sample(&e, &b, &mut rng);
        e.step(&actions, &mut b.as_out()).unwrap();
        for i in 0..8 {
            e.with_env(i, |env| {
                for p in 0..2 {
                    let side = env.slot.side_of(p);
                    let sets = &env.sheets[side.foe().index()].team.sets;
                    let view = env.slot.view(side);
                    let ctx = ViewCtx::new(&view, &tables);
                    let order = &view.foe_side().reveal_order;
                    out_of_order += usize::from(order.windows(2).any(|w| w[0] > w[1]));
                    for (row, set) in sets.iter().enumerate() {
                        let m = ctx.mon(1, row);
                        let sheet = m.sheet.expect("an entry per previewed Pokémon");
                        assert_eq!((&sheet.moves[..], sheet.item), (&set.moves[..], set.item), "battle {i} row {row}");
                        let base = |s| dex.species(s).base_species;
                        assert_eq!(base(sheet.species), base(set.species), "battle {i} row {row}");
                        // The Pokémon the log shows in the row is the entry's.
                        let shown = m.public.expect("previewed").details_species;
                        assert_eq!(base(shown), base(set.species), "battle {i} row {row}");
                        if m.is_revealed() {
                            revealed += 1;
                            let moves = m.moves().map(|mv| mv.slot.id());
                            let want: Vec<_> = set.moves.iter().map(|&id| Some(id)).collect();
                            assert_eq!(&moves[..want.len()], &want[..], "battle {i} row {row}");
                        }
                    }
                }
            });
        }
    }
    assert!(revealed > 500 && out_of_order > 100, "{revealed} revealed, {out_of_order} views out of order");
}

#[test]
fn best_of_three_series() {
    use crate::series::SeriesResult;
    let format = Format::champions_bss();
    let g = crate::teams::generator(&format, pikapro_teams::Mode::Sets, Default::default()).unwrap();
    for autoreset in [Autoreset::NextStep, Autoreset::SameStep] {
        let c = EnvConfig {
            series: Some(3),
            autoreset,
            teams: Teams::Generator(g.clone()),
            max_turns: Some(8),
            ..config(format.clone(), 2)
        };
        let e = env(c, 1);
        let mut b = e.buffers();
        assert!(e.step(&[0; 4], &mut StepBuffers::new(e.shape()).as_out()).is_err());
        e.reset(Some(SEED), &mut b.as_out()).unwrap();
        assert_eq!(b.series, [1, 0, 0, 0, 1, 0, 0, 0]);
        let mut rng = BattleRng::new(Seed::fast(SEED + 4));
        let mut series_seen = 0;
        let mut sheets = e.with_env(0, |env| env.sheets.clone()).unwrap();
        for _ in 0..600 {
            let actions = sample(&e, &b, &mut rng);
            e.step(&actions, &mut b.as_out()).unwrap();
            // Every game of a series plays the same teams with open sheets.
            let (now, ots, game) = e
                .with_env(0, |env| {
                    (env.sheets.clone(), env.slot.tracker.ots[0].is_some(), env.slot.series.as_ref().unwrap().game)
                })
                .unwrap();
            assert!(ots);
            if game > 1 {
                assert_eq!(now, sheets);
            }
            sheets = now;
            if b.end_reason[0] == 0 {
                continue;
            }
            let row = match autoreset {
                Autoreset::NextStep => &b.series[..4],
                Autoreset::SameStep => &b.final_series[..4],
            };
            let (played, wins, result) = (row[0], [row[1], row[2]], row[3]);
            let ties = played - wins[0] - wins[1];
            let threshold = (3 - ties) / 2 + 1;
            let expect = if wins[0] >= threshold {
                SeriesResult::Player0
            } else if wins[1] >= threshold {
                SeriesResult::Player1
            } else if played == 3 {
                SeriesResult::Tie
            } else {
                SeriesResult::Ongoing
            };
            assert_eq!(result, expect as u8, "{row:?}");
            if result != 0 {
                series_seen += 1;
            }
        }
        assert!(series_seen >= 3, "{autoreset:?} {series_seen}");
    }
}

#[test]
fn actions_from_the_masks_are_never_rejected_with_random_sets() {
    // Random sets in every preset and both mask layouts: whatever comes up
    // (Revival Blessing, Commander, trappers, Imprison, Illusion), an action
    // the masks allow is submitted, or unavailable for what the request
    // hid, never rejected.
    for (k, format) in Format::presets().iter().enumerate() {
        for mode in [pikapro_teams::Mode::Sets, pikapro_teams::Mode::Uniform] {
            let g = crate::teams::generator(format, mode, Default::default()).unwrap();
            let layout = if k % 2 == 0 { MaskLayout::Factored } else { MaskLayout::Joint };
            let c = EnvConfig {
                teams: Teams::Generator(g),
                masks: layout,
                opponent: OpponentSpec::One(Opponent::External),
                randomize_sides: true,
                ..config(format.clone(), 8)
            };
            let e = env(c, 2);
            for b in play(&e, 250, SEED + k as u64) {
                assert!(!b.status.contains(&status::REJECTED), "{} {mode:?} {:?}", format.id, b.status);
            }
        }
    }
}

#[test]
fn observations_are_each_player_s_view() {
    use crate::encode::{Basic, Encoder};
    let format = Format::champions_vgc();
    for opponent in [Opponent::External, Opponent::random()] {
        let c = EnvConfig {
            opponent: OpponentSpec::One(opponent.clone()),
            randomize_sides: true,
            ..config(format.clone(), 4)
        };
        let one = env(c.clone(), 1);
        assert_eq!(one.shape().obs, Basic::SIZE);
        let history = play(&one, 60, 7);
        // Deterministic for any thread count.
        assert!(history == play(&env(c, 3), 60, 7));
        let b = history.last().unwrap();
        let tables = crate::tables::Tables::for_format(&format);
        for i in 0..4 {
            one.with_env(i, |env| {
                for p in 0..2 {
                    let k = 2 * i + p;
                    let side = env.slot.side_of(p);
                    let view = PlayerView::new(side, &env.slot.tracker, &env.slot.own[side.index()])
                        .with_team(&env.slot.team[side.index()]);
                    // A built-in opponent's rows are zeros.
                    let mut want = vec![0.0; Basic::SIZE];
                    if p == 0 || opponent.is_external() {
                        Basic.encode(&view, &tables, &mut want);
                    }
                    assert!(b.obs[k * Basic::SIZE..][..Basic::SIZE] == want, "{i} {p}");
                }
            });
        }
    }
}

#[test]
fn baseline_opponents_play_on_the_workers() {
    for format in [Format::champions_vgc(), Format::gen9_singles()] {
        for opponent in [Opponent::max_base_power(), Opponent::simple_heuristics()] {
            let c = EnvConfig {
                opponent: OpponentSpec::One(opponent.clone()),
                randomize_sides: true,
                ..config(format.clone(), 4)
            };
            let history = play(&env(c.clone(), 2), 200, 9);
            assert!(history.iter().any(|b| b.end_reason.iter().any(|&r| r != 0)), "{} {opponent:?}", format.id);
            assert!(history.iter().all(|b| !b.status.contains(&status::REJECTED)));
            assert!(history == play(&env(c, 1), 200, 9), "deterministic");
        }
    }
    // A trapped player facing a heuristic opponent: both keep playing.
    let e = trapped_env(Opponent::simple_heuristics());
    let mut b = e.buffers();
    e.reset(Some(SEED), &mut b.as_out()).unwrap();
    for _ in 0..20 {
        e.step(&[RANDOM_ACTION, 0], &mut b.as_out()).unwrap();
        assert_ne!(b.status[0], status::REJECTED);
    }
}

#[test]
fn neighbouring_base_seeds_play_unrelated_episodes() {
    // Vector envs seed sub-env `i` with `seed + i`: base `s + 1` must not
    // replay base `s`'s episodes one later (as `seed + e` did).
    let mut seen = std::collections::BTreeSet::new();
    for base in SEED..SEED + 64 {
        for e in 0..1024 {
            assert!(seen.insert(episode_seed(base, e)), "base {base} episode {e}");
        }
    }
}
