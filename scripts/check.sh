#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="${HOME:?}/.cargo/bin:$PATH"
export CARGO_HOME="${CARGO_HOME:-$ROOT/.cache/cargo}"
# Respect Cargo's target directory and isolate the actual executable location.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
cd -- "$ROOT"
if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--ui" ) ]]; then
    printf 'Usage: bash scripts/check.sh [--ui]\n' >&2
    exit 2
fi
TOOLCHAIN="$(rustc --version)"
case "$TOOLCHAIN" in
    'rustc 1.99.1 '*) ;;
    *) printf 'Expected rustc 1.99.1, got %s\n' "$TOOLCHAIN" >&2; exit 1 ;;
esac
export MTP_BUILD_ID="${MTP_BUILD_ID:-$(git describe --always --dirty)}"
printf 'Toolchain: %s\n' "$TOOLCHAIN"
printf 'Cargo: '; cargo --version
cargo fmt --all -- --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --locked -p mtp-worker -p mtp-probe
"${CARGO_TARGET_DIR:?}/debug/mtp-probe" fake "${CARGO_TARGET_DIR:?}/debug/mtp-worker"
# GPUI is a separate gate; a successful core check never claims UI acceptance.
if [[ "${1:-}" == "--ui" ]]; then
    if [[ "$(uname -s)" == "Darwin" ]]; then
        xcrun --find metal
        xcrun metal --version
    fi
    cargo check --locked -p app-gpui
    cargo test --locked -p app-gpui
    cargo clippy --locked -p app-gpui --all-targets -- -D warnings
    cargo build --locked -p app-gpui
fi
