#!/usr/bin/env bash
# The full local gate: rustfmt, clippy, the dependency rule, the tests, the
# docs, the Python leg and the released package (the wheel). Run it before
# handing work back:
#
#   tools/full-gate.sh            # everything (about 15 minutes warm)
#   tools/full-gate.sh --no-insns # without the instruction counts
#
# Needs uv (the Python leg makes crates/pikapro-py/.venv on first use) and
# valgrind (`cargo bench-insns`; skipped with a warning when missing). The
# counts match the baseline only on Ubuntu 22.04 (glibc 2.35, valgrind
# 3.18), as the top of benches/src/bin/pikapro-insns.rs explains. Honours
# CARGO_TARGET_DIR.
set -euo pipefail

insns=1
for arg in "$@"; do
  case "$arg" in
    --no-insns) insns=0 ;;
    *) echo "usage: $0 [--no-insns]" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target=${CARGO_TARGET_DIR:-$root/target}
venv=$root/crates/pikapro-py/.venv
started=$(date +%s)
step() { printf '\n== %s (%ds)\n' "$*" "$(( $(date +%s) - started ))"; }

step "rustfmt"
cargo fmt --all --check
step "clippy"
cargo ci-check
step "no external dependencies in the core crates"
cargo metadata --no-deps --format-version 1 | python3 .github/scripts/no_external_deps.py
step "tests"
cargo test --workspace
step "docs"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

step "Python virtualenv"
if [ ! -x "$venv/bin/python" ]; then
  uv venv --python 3.10 "$venv"
fi
uv pip install --python "$venv/bin/python" -q maturin pytest numpy gymnasium pettingzoo websockets
step "clippy with the PyO3 layer"
PYO3_PYTHON=$venv/bin/python cargo clippy -p pikapro-py --features python,pyo3/abi3-py310 --all-targets -- -D warnings
step "the binaries pytest drives"
cargo build -p pikapro-cli -p pikapro-showdown-server
step "maturin develop"
(cd crates/pikapro-py && source .venv/bin/activate && maturin develop --uv)
step "pytest"
(cd crates/pikapro-py \
  && PIKAPRO_CLI=$target/debug/pikapro PIKAPRO_SHOWDOWN_SERVER=$target/debug/pikapro-showdown-server \
     .venv/bin/python -m pytest -q -ra)
# The API itself is pinned by pytest (tests/test_api_snapshot.py); the
# reference's generator is the maintainers' and isn't in a public checkout.
if [ -f docs/python/gen_api.py ]; then
  step "the API reference is up to date"
  "$venv/bin/python" docs/python/gen_api.py --check
else
  step "the API reference: skipped (docs/python/gen_api.py isn't in this checkout)"
fi
step "LICENSE, THIRD_PARTY_NOTICES and the sdist's toolchain file"
for f in crates/*/LICENSE; do
  cmp LICENSE "$f"
  cmp THIRD_PARTY_NOTICES "$(dirname "$f")/THIRD_PARTY_NOTICES"
done
cmp rust-toolchain.toml crates/pikapro-py/rust-toolchain.toml

step "the released package: the wheel and pytest against it (tools/release-config.sh)"
tools/release-config.sh

if [ "$insns" = 1 ]; then
  if command -v valgrind > /dev/null; then
    step "instruction counts (cargo bench-insns)"
    cargo bench-insns check
  else
    echo "warning: valgrind not found, cargo bench-insns skipped" >&2
  fi
fi
step "all passed"
