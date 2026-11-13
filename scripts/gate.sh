#!/usr/bin/env bash
# The local gate, as CI runs it: the format, clippy for macOS, Linux and
# Windows, and the tests on this host. Run it before each commit.
#
# Usage: scripts/gate.sh
#
# The check for Linux leaves out burnout-gui, because it links fontconfig,
# and a cross build has no fontconfig of the target. CI builds it on Linux.
# The targets aarch64-unknown-linux-musl and aarch64-pc-windows-msvc must be
# installed with rustup.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy -q --workspace --all-targets -- -D warnings
cargo clippy -q --workspace --exclude burnout-gui --all-targets --target aarch64-unknown-linux-musl -- -D warnings
cargo clippy -q --workspace --all-targets --target aarch64-pc-windows-msvc -- -D warnings
cargo test -q --workspace 2>&1 | grep -E '^test result|FAILED|panicked|error' | awk '{p+=$4; f+=$6} /FAILED|panicked|error/ {print} END {print "passed", p, "failed", f; exit (f > 0)}'
