#!/usr/bin/env bash
# The released package: the extension built as a wheel, as the released
# ones are, and pikapro-py's tests against the installed wheel (the
# editable build is tools/full-gate.sh's); then, in a maintainer's
# checkout, the API reference (docs/python/api.md) checked against it.
#
#   tools/release-config.sh
#   PIKAPRO_UPDATE_API=1 tools/release-config.sh   # rewrite the API snapshot and api.md
#
# Run by tools/full-gate.sh. Needs uv. Honours CARGO_TARGET_DIR.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
venv=$root/crates/pikapro-py/.venv-release

echo "== the extension, as the wheels are built"
if [ ! -x "$venv/bin/python" ]; then
  uv venv -q --python 3.10 "$venv"
fi
uv pip install -q --python "$venv/bin/python" maturin pytest numpy gymnasium pettingzoo websockets
wheels=$CARGO_TARGET_DIR/wheels
rm -rf "$wheels"
(cd crates/pikapro-py && "$venv/bin/maturin" build -q -i "$venv/bin/python" --out "$wheels")
uv pip install -q --python "$venv/bin/python" --reinstall "$wheels"/*.whl
cargo build -p pikapro-cli -p pikapro-showdown-server
# From crates/pikapro-py, which doesn't put python/ on the path: the tests
# import the installed wheel.
(cd crates/pikapro-py \
  && PIKAPRO_CLI=$CARGO_TARGET_DIR/debug/pikapro PIKAPRO_SHOWDOWN_SERVER=$CARGO_TARGET_DIR/debug/pikapro-showdown-server \
     "$venv/bin/python" -c 'import pikapro, sys; assert "site-packages" in pikapro.__file__, pikapro.__file__' \
  && PIKAPRO_CLI=$CARGO_TARGET_DIR/debug/pikapro PIKAPRO_SHOWDOWN_SERVER=$CARGO_TARGET_DIR/debug/pikapro-showdown-server \
     "$venv/bin/python" -m pytest -q -ra -p no:cacheprovider)
echo "== the API reference"
if [ ! -f docs/python/gen_api.py ]; then
  echo "(skipped: docs/python/gen_api.py, the maintainers' generator, isn't in this checkout)"
elif [ -n "${PIKAPRO_UPDATE_API:-}" ]; then
  "$venv/bin/python" docs/python/gen_api.py
else
  "$venv/bin/python" docs/python/gen_api.py --check
fi
echo "== the released package passed"
