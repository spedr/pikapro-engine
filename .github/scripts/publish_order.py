#!/usr/bin/env python3
"""Prints the workspace's publishable crates in dependency order, one
`name version` per line, from `cargo metadata` (so a renamed crate, or a
new one, needs no change here).

    python3 .github/scripts/publish_order.py [--version X.Y.Z]

A crate is publishable unless its manifest says `publish = false` (or
names only other registries). A crate comes after every workspace crate it
depends on, dev-dependencies included when they carry a version (cargo
keeps those in the published manifest; a path-only dev-dependency is
dropped). Ties keep the workspace's member order.

Fails if a publishable crate depends on an unpublishable one, if the
dependencies form a cycle, or (with --version) if a publishable crate's
version isn't X.Y.Z.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", help="the version every publishable crate must have")
    args = parser.parse_args()

    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    members = {p["id"]: p for p in meta["packages"] if p["id"] in set(meta["workspace_members"])}
    order = [members[i] for i in meta["workspace_members"]]
    by_name = {p["name"]: p for p in order}

    def publishable(p: dict) -> bool:
        # null: anywhere; []: publish = false; a list: those registries.
        return p["publish"] is None or "crates-io" in p["publish"]

    errors = []
    deps: dict[str, set[str]] = {}
    for p in order:
        if not publishable(p):
            continue
        deps[p["name"]] = set()
        for d in p["dependencies"]:
            if d["name"] not in by_name or "path" not in d:
                continue
            if d["kind"] == "dev" and d["req"] == "*":
                continue  # path-only dev-dependency: dropped from the package
            if not publishable(by_name[d["name"]]):
                errors.append(f"{p['name']} depends on {d['name']}, which has publish = false")
                continue
            deps[p["name"]].add(d["name"])
        if args.version and p["version"] != args.version:
            errors.append(f"{p['name']} is version {p['version']}, not {args.version}")

    done: list[str] = []
    pending = [p["name"] for p in order if p["name"] in deps]
    while pending:
        ready = [n for n in pending if deps[n] <= set(done)]
        if not ready:
            errors.append(f"dependency cycle among {', '.join(pending)}")
            break
        done.append(ready[0])
        pending.remove(ready[0])

    if errors:
        for e in errors:
            print(f"::error::{e}", file=sys.stderr)
        return 1
    if not done:
        print("::error::no publishable crate in the workspace", file=sys.stderr)
        return 1
    for name in done:
        print(name, by_name[name]["version"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
