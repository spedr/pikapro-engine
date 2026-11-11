"""The crates that take no external dependencies (docs/ARCHITECTURE.md,
"Rules") depend only on the workspace's own crates: a dependency with a
source (crates.io, git) fails. Reads `cargo metadata --no-deps` on stdin.

    cargo metadata --no-deps --format-version 1 | python3 .github/scripts/no_external_deps.py
"""

import json
import sys

CRATES = {
    "pikapro-data",
    "pikapro",
    "pikapro-protocol",
    "pikapro-validate",
    "pikapro-teams",
    "pikapro-server",
    "pikapro-view",
    "pikapro-rl",
}

meta = json.load(sys.stdin)
seen = set()
bad = []
for pkg in meta["packages"]:
    if pkg["name"] not in CRATES:
        continue
    seen.add(pkg["name"])
    for dep in pkg["dependencies"]:
        if dep.get("source") is not None:
            bad.append(f"{pkg['name']}: {dep['name']} ({dep['source']})")
missing = CRATES - seen
if missing:
    bad.append(f"not in the workspace: {', '.join(sorted(missing))}")
if bad:
    print("external dependencies where none are allowed:\n  " + "\n  ".join(bad))
    sys.exit(1)
print(f"{len(seen)} crates, no external dependencies")
