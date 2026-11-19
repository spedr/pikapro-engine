#!/bin/sh
# Installs rustup in a manylinux or musllinux image (cibuildwheel's
# before-all, crates/pikapro-py/pyproject.toml). rustup then installs the
# toolchain rust-toolchain.toml pins on first use.
#
# rustup-init is pinned: a release of it for the image's architecture and C
# library (musl where its loader is there), checked against the SHA-256 of
# rustup's `rustup-init.sha256` for that release before it runs. To update
# it, change the version and all four hashes, each from
# https://static.rust-lang.org/rustup/archive/VERSION/TARGET/rustup-init.sha256.
set -eu

version=1.29.3
target=$(uname -m)-unknown-linux-gnu
if [ -e "/lib/ld-musl-$(uname -m).so.1" ]; then
    target=$(uname -m)-unknown-linux-musl
fi
case $target in
    x86_64-unknown-linux-gnu) sha256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55 ;;
    aarch64-unknown-linux-gnu) sha256=9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282 ;;
    x86_64-unknown-linux-musl) sha256=1552169aba2836777136ccff7a381e95c17233b5d8250cd5e10ae3cb971bfcfc ;;
    aarch64-unknown-linux-musl) sha256=956deacdc27ab82ae7ac821d5d3e9e95e4795d476d17efe99743bb8531ca1031 ;;
    *) echo "no pinned rustup-init for $target" >&2; exit 1 ;;
esac

curl --proto =https --tlsv1.2 -sSf -o /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/$version/$target/rustup-init"
echo "$sha256  /tmp/rustup-init" | sha256sum -c -
chmod +x /tmp/rustup-init
/tmp/rustup-init -y --profile minimal --default-toolchain none
rm /tmp/rustup-init
