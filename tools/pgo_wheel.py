#!/usr/bin/env python3
"""The profile for the wheels' profile-guided optimisation (PGO).

cibuildwheel runs this before it builds a wheel (`before-build` in
.github/workflows/wheels.yml), with RUSTFLAGS already set to the flags
the wheel is then built with:

    RUSTFLAGS="$(python3 tools/pgo_wheel.py --print-rustflags)" python3 tools/pgo_wheel.py

It needs rustup, and pip in the Python it runs under. In order:

1. Builds the training binaries with `-Cprofile-generate`, in
   `target/pgo/target`: the `pikapro` CLI and pikapro-bench's
   `pikapro-insns`, with the wheel's cargo profile (`release-py`) and,
   for the crates they share with the extension (the engine, the view, the
   RL environment, the protocol, teams and the validator), the extension's
   cargo features. A function's profile is found by its symbol name, which
   depends on its crate's features, profile and target, so a mismatch
   would leave the extension unoptimised.
2. Runs the workload, one thread, a fixed amount of work (so the profile is
   the same on every run): `pikapro bench` in five formats, and every
   `pikapro-insns` workload (battles, legal actions, requests, log
   rendering, the player view, the `basic` encoder, the vector
   environment, building battles, team import). The small ones repeat (the
   environment 10 times, the per-call ones 20), so that they count
   alongside the battles.
3. Merges the profile into `target/pgo/merged.profdata` with llvm-profdata
   (rustup's `llvm-tools`), and fails if it is empty.
4. Builds the extension with maturin and RUSTFLAGS (the profile, and
   `-pgo-warn-missing-function`), and fails if LLVM reports a profile out
   of date, or if a shared crate's functions lack profile data because the
   crate was built differently (its codegen units' hash, which follows its
   symbol names, differs from the instrumented build's). Functions the
   training binaries don't call may lack data: a linker may drop them
   (macOS's does), with their profile records. cibuildwheel's own build
   then reuses those crates. The extension's own crate (the PyO3 layer)
   and pikapro-server have no profile: no training binary links them.

Rust-only on purpose: the profile doesn't need the wheel, numpy or a
Python test run, and the training binaries are the same code (step 1).
"""

from __future__ import annotations

import argparse
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PGO = ROOT / "target" / "pgo"
PROFILE = PGO / "merged.profdata"
MISSING_FLAG = "-Cllvm-args=-pgo-warn-missing-function"
# The crates both the training binaries and the extension link: every
# function of theirs must find its profile.
SHARED = (
    "pikapro",
    "pikapro_data",
    "pikapro_protocol",
    "pikapro_rl",
    "pikapro_teams",
    "pikapro_validate",
    "pikapro_view",
)
FORMATS = ("gen9vgc", "championsvgc", "gen9singles", "gen9doubles", "championsbss")
# pikapro-insns workloads: how many times each runs, by name prefix; the
# rest, the small per-call ones, run PER_CALL_REPEATS times.
REPEATS = {"battle": 1, "vec-env": 10}
PER_CALL_REPEATS = 20


def rustflags() -> str:
    return f"-Cprofile-use={PROFILE} {MISSING_FLAG}"


def log(msg: str) -> None:
    print(f"pgo_wheel: {msg}", flush=True)


def fail(msg: str) -> None:
    print(f"pgo_wheel: error: {msg}", file=sys.stderr, flush=True)
    sys.exit(1)


def run(cmd: list, env: dict[str, str] | None = None, quiet: bool = False, cwd: Path = ROOT) -> str:
    """Runs a command (from the root by default); returns its output if `quiet`."""
    if not quiet:
        log("$ " + " ".join(shlex.quote(str(c)) for c in cmd))
    out = subprocess.run(
        [str(c) for c in cmd],
        check=False,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE if quiet else None,
        stderr=subprocess.STDOUT if quiet else None,
        # Tools print UTF-8 (maturin's emoji), not Windows' ANSI code page.
        encoding="utf-8",
        errors="replace",
    )
    if out.returncode != 0:
        if quiet:
            print(out.stdout, end="")
        fail(f"{cmd[0]} exited with {out.returncode}")
    return out.stdout or ""


def check_rustflags() -> None:
    """RUSTFLAGS must be what the wheel is built with: the profile below."""
    flags = shlex.split(os.environ.get("RUSTFLAGS", ""), posix=os.name != "nt")
    use = [f[len("-Cprofile-use=") :] for f in flags if f.startswith("-Cprofile-use=")]

    def norm(p: str) -> str:
        return os.path.normcase(os.path.normpath(p))

    if len(use) != 1 or norm(use[0]) != norm(str(PROFILE)) or MISSING_FLAG not in flags:
        fail(
            f"RUSTFLAGS is {os.environ.get('RUSTFLAGS', '')!r}; the wheel's build must have "
            f"{rustflags()!r} (wheels.yml sets it)"
        )


def llvm_profdata() -> Path:
    run(["rustup", "component", "add", "llvm-tools"])
    sysroot = run(["rustc", "--print", "sysroot"], quiet=True).strip()
    host = re.search(r"^host: (\S+)$", run(["rustc", "-vV"], quiet=True), re.MULTILINE)
    if not host:
        fail("no host in `rustc -vV`")
    exe = ".exe" if os.name == "nt" else ""
    path = Path(sysroot) / "lib" / "rustlib" / host.group(1) / "bin" / f"llvm-profdata{exe}"
    if not path.is_file():
        fail(f"{path} not found after `rustup component add llvm-tools`")
    return path


def build_instrumented(raw: Path) -> Path:
    """The training binaries with -Cprofile-generate; returns their directory."""
    env = dict(os.environ)
    env.pop("CARGO_ENCODED_RUSTFLAGS", None)
    env.pop("LLVM_PROFILE_FILE", None)
    env["RUSTFLAGS"] = f"-Cprofile-generate={raw}"
    env["CARGO_TARGET_DIR"] = str(PGO / "target")
    run(
        [
            "cargo",
            "build",
            "--profile",
            "release-py",
            "-p",
            "pikapro-bench",
            "--bin",
            "pikapro-insns",
        ],
        env,
    )
    run(["cargo", "build", "--profile", "release-py", "-p", "pikapro-cli"], env)
    target = os.environ.get("CARGO_BUILD_TARGET", "")
    return PGO / "target" / target / "release-py"


def workload(bin_dir: Path) -> None:
    exe = ".exe" if os.name == "nt" else ""
    cli, insns = bin_dir / f"pikapro{exe}", bin_dir / f"pikapro-insns{exe}"
    start = time.monotonic()
    for f in FORMATS:
        run([cli, "bench", "--format", f, "--battles", "2000", "--threads", "1"], quiet=True)
    run([cli, "bench", "--format", "championsvgc", "--battles", "1000", "--threads", "1", "--events"], quiet=True)
    names = run([insns, "list"], quiet=True).split()
    if len(names) < 20:
        fail(f"pikapro-insns lists {len(names)} workloads")
    for name in names:
        prefix = name.split("/")[0]
        repeats = next((n for p, n in REPEATS.items() if prefix.startswith(p)), PER_CALL_REPEATS)
        for _ in range(repeats):
            run([insns, "run", name], quiet=True)
    log(
        f"workload: {len(FORMATS) + 1} CLI benches and {len(names)} pikapro-insns workloads in {time.monotonic() - start:.0f}s"
    )


def merge(profdata: Path, raw: Path) -> None:
    files = sorted(raw.glob("*.profraw"))
    if not files:
        fail(f"the workload wrote no .profraw file in {raw}")
    run([profdata, "merge", "-o", PROFILE, raw], quiet=True)
    show = run([profdata, "show", PROFILE], quiet=True)
    print(show, end="")

    def stat(name: str) -> int:
        m = re.search(rf"^{name}: (\d+)", show, re.MULTILINE)
        return int(m.group(1)) if m else 0

    functions, total = stat("Total functions"), stat("Total count")
    if functions < 1000 or total < 100_000_000:
        fail(f"the profile is empty: {functions} functions, total count {total}")
    log(f"profile: {PROFILE} from {len(files)} .profraw files, {functions} functions")


def cgu_hashes(bin_dir: Path, crate: str) -> set[str]:
    """The codegen-unit hashes in the instrumented build's newest rlib of
    `crate`: they derive from the crate's symbol hash, so a build with the
    same hash has the same symbol names."""
    rlibs = sorted((bin_dir / "deps").glob(f"lib{crate}-*.rlib"), key=lambda p: p.stat().st_mtime)
    if not rlibs:
        return set()
    data = rlibs[-1].read_bytes()
    return {h.decode() for h in re.findall(rb"\b" + crate.encode() + rb"\.([0-9a-f]{8,})-cgu\.", data)}


# A warning LLVM gives while compiling a codegen unit: `warning:
# <crate>.<hash>-cgu.<n>: <message>`.
CGU_WARNING = re.compile(r"warning: (\w+)\.([0-9a-f]+)-cgu\.\d+: (.*)")
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def parse_warnings(out: str) -> tuple[dict[str, int], dict[str, set[str]], list[str]]:
    """The build output's codegen-unit warnings: functions without profile
    data by crate, the codegen-unit hashes of those crates, and every other
    such warning (a profile out of date, malformed, ...)."""
    missing: dict[str, int] = {}
    cgus: dict[str, set[str]] = {}
    others = []
    for line in out.splitlines():
        line = ANSI.sub("", line)
        m = CGU_WARNING.match(line)
        if not m:
            continue
        if m.group(3).startswith("no profile data available for function"):
            missing[m.group(1)] = missing.get(m.group(1), 0) + 1
            cgus.setdefault(m.group(1), set()).add(m.group(2))
        else:
            others.append(line)
    return missing, cgus, others


def self_test() -> None:
    """parse_warnings on plain and coloured (CARGO_TERM_COLOR=always) lines."""
    plain = (
        "warning: pikapro_py.746730ccc95f218-cgu.0: no profile data available for function "
        "_ZN10pikapro_py2rl3run17h0123456789abcdefE Hash = 297402247331721286 up to 0 count discarded"
    )
    coloured = (
        "\x1b[0m\x1b[1m\x1b[33mwarning\x1b[0m\x1b[0m\x1b[1m: pikapro.66baec20914e3048-cgu.0: no profile data "
        "available for function _ZN7pikapro6battle3foo17h0123456789abcdefE Hash = 1 up to 0 count discarded\x1b[0m"
    )
    stale = (
        "\x1b[1m\x1b[33mwarning\x1b[0m\x1b[1m: pikapro.66baec20914e3048-cgu.0: function control flow change "
        "detected (hash mismatch) _ZN7pikapro7resolve5queue10speed_sort17hc948d3671b545dc2E Hash = 1\x1b[0m"
    )
    unrelated = "\x1b[1m\x1b[33mwarning\x1b[0m: `pikapro-py` (lib) generated 1272 warnings"
    missing, cgus, others = parse_warnings(f"{plain}\n{coloured}\n{stale}\n{unrelated}\nCompiling pikapro")
    assert missing == {"pikapro_py": 1, "pikapro": 1}, missing
    assert cgus == {"pikapro_py": {"746730ccc95f218"}, "pikapro": {"66baec20914e3048"}}, cgus
    assert len(others) == 1 and "hash mismatch" in others[0] and "\x1b" not in others[0], others
    log("self-test passed")


def verify(bin_dir: Path) -> None:
    """Builds the extension as cibuildwheel will and checks LLVM's PGO warnings."""
    run([sys.executable, "-m", "pip", "install", "-q", "maturin>=1.9.4,<2"])
    args = shlex.split(os.environ.get("MATURIN_PEP517_ARGS", ""))
    # Plain text to parse (the workflow sets CARGO_TERM_COLOR=always).
    env = dict(os.environ, CARGO_TERM_COLOR="never")
    out = run(
        [
            sys.executable,
            "-m",
            "maturin",
            "build",
            *args,
            "-i",
            sys.executable,
            "--auditwheel",
            "skip",
            "--out",
            PGO / "verify-wheel",
        ],
        env,
        quiet=True,
        cwd=ROOT / "crates" / "pikapro-py",
    )
    for line in out.splitlines():
        if not CGU_WARNING.match(ANSI.sub("", line)) and (
            "warning" in line or "error" in line or "Built wheel" in line
        ):
            print(line)
    missing, cgus, stale = parse_warnings(out)
    log(
        "functions without profile data, by crate: "
        + (", ".join(f"{k} {v}" for k, v in sorted(missing.items())) or "none")
    )
    if stale:
        print("\n".join(stale[:20]), flush=True)
        fail(f"LLVM reports {len(stale)} profile problems (above): the profile doesn't match the build")
    # The extension's own crate is in no training binary, so some of its
    # functions always lack profile data: none found means the warnings
    # weren't parsed (or -pgo-warn-missing-function didn't reach LLVM).
    if not missing.get("pikapro_py"):
        fail("no function of pikapro_py is reported without profile data: the build's warnings weren't parsed")
    # A shared crate's function can lack profile data only because the
    # training binaries don't call it: a linker may drop such functions,
    # profile records included (macOS's does, Linux's doesn't). Then the
    # crate itself is the instrumented one, which its codegen units' hash
    # shows; a crate built differently (cargo profile, features or target)
    # has other symbol names, and none of its functions find their profile.
    for crate in sorted(set(missing) & set(SHARED)):
        built = cgu_hashes(bin_dir, crate)
        if not cgus[crate] <= built:
            fail(
                f"{crate} was built differently for the extension (codegen units {sorted(cgus[crate])}) and "
                f"for the training binaries ({sorted(built)}): different cargo profile, features or target, so "
                "its functions find no profile"
            )
    log("the shared crates match the profile (the functions without data are ones the training binaries don't call)")


def main() -> None:
    # Windows' console code page can't print every character the tools do.
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(errors="backslashreplace")
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument(
        "--print-rustflags", action="store_true", help="print the RUSTFLAGS the wheel is built with, and exit"
    )
    parser.add_argument("--self-test", action="store_true", help="test the warning parser, and exit")
    args = parser.parse_args()
    if args.print_rustflags:
        print(rustflags())
        return
    self_test()
    if args.self_test:
        return
    check_rustflags()
    profdata = llvm_profdata()
    # A new profile every run; the instrumented build stays (cargo rebuilds what changed).
    raw = PGO / "raw"
    shutil.rmtree(raw, ignore_errors=True)
    shutil.rmtree(PGO / "verify-wheel", ignore_errors=True)
    PROFILE.unlink(missing_ok=True)
    raw.mkdir(parents=True)
    bin_dir = build_instrumented(raw)
    workload(bin_dir)
    merge(profdata, raw)
    verify(bin_dir)


if __name__ == "__main__":
    main()
