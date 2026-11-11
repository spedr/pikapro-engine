"""poke-env's Gymnasium environments (SinglesEnv, DoublesEnv) against a
server: a few episodes with random legal actions (from the action mask)
against a RandomPlayer.

    python env_smoke.py --server ws://localhost:8000/showdown/websocket \
        --format gen9customgame --episodes 5
"""

from __future__ import annotations

import argparse
import json
import pathlib
import time

import numpy as np
from gymnasium.spaces import Box
from poke_env import AccountConfiguration, ServerConfiguration
from poke_env.environment import DoublesEnv, SingleAgentWrapper, SinglesEnv
from poke_env.player import RandomPlayer

TEAMS = pathlib.Path(__file__).parent / "teams"


def make(base):
    class Env(base):
        def __init__(self, **kwargs):
            super().__init__(**kwargs)
            self.observation_spaces = {a: Box(0.0, 1.0, (1,), dtype=np.float32) for a in self.possible_agents}

        def calc_reward(self, battle):
            return self.reward_computing_helper(battle, fainted_value=1.0, victory_value=10.0)

        def embed_battle(self, battle):
            return np.array([battle.turn / 100], dtype=np.float32)

    return Env


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", default="ws://localhost:8000/showdown/websocket")
    parser.add_argument("--format", default="gen9customgame")
    parser.add_argument("--episodes", type=int, default=5)
    parser.add_argument("--name", default="env")
    args = parser.parse_args()
    team_file = "gen9championsvgc2026regmc.txt" if "champions" in args.format else "gen9vgc2025regi.txt"
    team = (TEAMS / team_file).read_text()
    server = ServerConfiguration(args.server, "http://localhost:8000/action.php?")
    doubles = "vgc" in args.format or "doubles" in args.format
    env = make(DoublesEnv if doubles else SinglesEnv)(
        account_configuration1=AccountConfiguration(f"{args.name} 1", None),
        account_configuration2=AccountConfiguration(f"{args.name} 2", None),
        battle_format=args.format,
        server_configuration=server,
        team=team,
        strict=False,
        choose_on_teampreview=False,
    )
    opponent = RandomPlayer(
        AccountConfiguration(f"{args.name} o", None),
        battle_format=args.format,
        server_configuration=server,
        team=team,
        start_listening=False,
    )
    wrapped = SingleAgentWrapper(env, opponent)
    rng = np.random.default_rng(2_200_000)
    start = time.perf_counter()
    steps, results = 0, []
    for _ in range(args.episodes):
        obs, _ = wrapped.reset()
        done = False
        while not done:
            mask = np.asarray(obs["action_mask"])
            if doubles:
                # DoublesEnv's mask covers both slots' actions, one after the other.
                half = len(mask) // 2
                action = np.array(
                    [rng.choice(np.flatnonzero(mask[:half]) if mask[:half].any() else [0]),
                     rng.choice(np.flatnonzero(mask[half:]) if mask[half:].any() else [0])]
                )
            else:
                legal = np.flatnonzero(mask)
                action = np.int64(rng.choice(legal) if len(legal) else 0)
            obs, reward, terminated, truncated, _ = wrapped.step(action)
            steps += 1
            done = terminated or truncated
        battle = env.battle1
        results.append("won" if battle.won else "lost" if battle.lost else "tied")
    elapsed = time.perf_counter() - start
    print(json.dumps({"format": args.format, "episodes": len(results), "results": results, "steps": steps,
                      "seconds": round(elapsed, 2)}))
    wrapped.close()


if __name__ == "__main__":
    main()
