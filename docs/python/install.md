# Install

## From PyPI

```sh
pip install pikapro                  # the simulator and pikapro.rl; needs only numpy
pip install 'pikapro[gym]'           # + Gymnasium adapters (pikapro.rl.gym)
pip install 'pikapro[all]'           # every optional integration
```

Wheels are built for CPython 3.10 and later, one per platform. They use
the stable ABI, so a new CPython needs no new wheel.

| platform | wheel |
|---|---|
| Linux x86_64, glibc 2.28+ (RHEL 8, Debian 10, Ubuntu 18.10 and later) | `manylinux_2_28_x86_64` |
| Linux aarch64, glibc 2.28+ | `manylinux_2_28_aarch64` |
| Linux x86_64 and aarch64, musl (Alpine) | `musllinux_1_2_*` |
| macOS 10.13+ on Intel | `macosx_10_13_x86_64` |
| macOS 11+ on Apple silicon | `macosx_11_0_arm64` |
| Windows x86_64 | `win_amd64` |

Anywhere else pip builds from the sdist, which needs Rust 1.88 or later
([rustup](https://rustup.rs) installs the pinned toolchain). A build
from the sdist or a checkout gets no profile-guided optimisation, so it
runs slower than the wheels
([performance](performance.md#how-these-were-measured)).

## Extras

The core needs only numpy 1.21 or later. Each integration is an extra
that only its own module imports.

| extra | installs | for |
|---|---|---|
| `gym` | `gymnasium>=1.0` | `pikapro.rl.gym`: `GymEnv`, `GymVectorEnv` |
| `pettingzoo` | `pettingzoo>=1.24`, `gymnasium` | `pikapro.rl.pettingzoo`: `ParallelEnv` |
| `sb3` | `stable-baselines3>=2.3`, `sb3-contrib>=2.3`, `gymnasium` | `pikapro.rl.sb3`: `SB3VecEnv` for MaskablePPO |
| `poke-env` | `poke-env>=0.16,<0.17` | `pikapro.compat.poke_env` |
| `showdown` | `websockets>=13` | `pikapro.showdown`: the client for local servers and VGC Arena |
| `all` | all of the above | |
| `test` | `pytest` | running the test suite |
| `dev` | `pytest`, `maturin` | building from a checkout (`maturin develop --extras dev`) |

`sb3` pulls in PyTorch. To choose its build, CPU only or a CUDA
version, install torch first:

```sh
pip install torch --index-url https://download.pytorch.org/whl/cpu
pip install 'pikapro[sb3]'
```

The poke-env bridge drives poke-env's own parser and players, so it
follows one poke-env minor version (0.16).

## Check the install

```sh
python -c "import pikapro, pikapro.rl; print(pikapro.__version__, pikapro.formats())"
```

## From a checkout

You need [Rust](https://rustup.rs) and, for the commands below,
[uv](https://docs.astral.sh/uv/). `rust-toolchain.toml` pins the Rust
version, and cargo installs it on first use.

```sh
git clone https://github.com/spedr/pikapro-engine && cd pikapro-engine/crates/pikapro-py
uv venv && source .venv/bin/activate
uv pip install maturin
maturin develop --uv --extras test      # builds and installs pikapro, editable
pytest                                  # the test suite
```

Don't pass `--release`: it selects the `release` profile, which aborts
the interpreter on a Rust panic. `maturin develop` uses `release-py`,
which turns a panic into a Python exception. Changes to `python/pikapro/**`
apply without a rebuild; changes to the Rust code need `maturin develop`
again.

To build a wheel instead, run `maturin build --out dist`.

Without a checkout, pip can build from git directly:

```sh
pip install "git+https://github.com/spedr/pikapro-engine#subdirectory=crates/pikapro-py"
```
