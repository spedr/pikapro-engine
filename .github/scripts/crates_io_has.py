#!/usr/bin/env python3
"""Exits 0 if crates.io has version VERSION of crate NAME (yanked or not),
1 if it doesn't (or the crate doesn't exist), 2 if the index can't be read.

    python3 .github/scripts/crates_io_has.py NAME VERSION

Reads the sparse index (https://index.crates.io), which is what cargo
itself reads, so it needs no token and no API user agent policy.
"""

from __future__ import annotations

import json
import sys
import urllib.error
import urllib.request


def index_path(name: str) -> str:
    name = name.lower()
    if len(name) <= 2:
        return f"{len(name)}/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def main() -> int:
    name, version = sys.argv[1], sys.argv[2]
    url = f"https://index.crates.io/{index_path(name)}"
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            body = response.read().decode()
    except urllib.error.HTTPError as e:
        if e.code in (403, 404):
            return 1
        print(f"{url}: {e}", file=sys.stderr)
        return 2
    except OSError as e:
        print(f"{url}: {e}", file=sys.stderr)
        return 2
    versions = {json.loads(line)["vers"] for line in body.splitlines() if line.strip()}
    return 0 if version in versions else 1


if __name__ == "__main__":
    sys.exit(main())
