#!/usr/bin/env bash
# Build gitdif from source and install the binary.
#
# Usage:
#   ./install.sh                 # install to ~/.local/bin
#   ./install.sh --prefix /usr/local   # install to /usr/local/bin (may need sudo)
#   ./install.sh --uninstall     # remove the installed binary
#
# PREFIX can also be set via the environment: PREFIX=/usr/local ./install.sh

set -euo pipefail

BIN_NAME="gitdif"
PREFIX="${PREFIX:-$HOME/.local}"
UNINSTALL=0

usage() { sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --prefix)   PREFIX="${2:?--prefix needs a value}"; shift 2 ;;
        --prefix=*) PREFIX="${1#*=}"; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help)  usage; exit 0 ;;
        *) echo "error: unknown option '$1'" >&2; usage >&2; exit 1 ;;
    esac
done

BIN_DIR="$PREFIX/bin"
DEST="$BIN_DIR/$BIN_NAME"

# Use sudo only when the target directory isn't writable by us.
SUDO=""
target="$BIN_DIR"
while [[ ! -e "$target" ]]; do target="$(dirname "$target")"; done
if [[ ! -w "$target" ]]; then
    command -v sudo >/dev/null || { echo "error: $BIN_DIR is not writable and sudo is unavailable" >&2; exit 1; }
    SUDO="sudo"
fi

# A copy from `cargo install --path .` lands in ~/.cargo/bin, which usually
# comes earlier in PATH and would keep running instead of this one.
remove_cargo_copy() {
    command -v cargo >/dev/null || return 0
    if cargo install --list 2>/dev/null | grep "^$BIN_NAME " >/dev/null; then
        cargo uninstall "$BIN_NAME"
        echo "Removed the cargo-installed copy of $BIN_NAME"
        REMOVED_CARGO_COPY=1
    fi
}
REMOVED_CARGO_COPY=0

if [[ $UNINSTALL -eq 1 ]]; then
    remove_cargo_copy
    if [[ -e "$DEST" ]]; then
        $SUDO rm -f "$DEST"
        echo "Removed $DEST"
    else
        echo "Nothing to remove: $DEST does not exist"
    fi
    exit 0
fi

command -v cargo >/dev/null || {
    echo "error: cargo not found. Install Rust from https://rustup.rs and retry." >&2
    exit 1
}
command -v git >/dev/null || echo "warning: git not found in PATH; gitdif needs it at runtime." >&2

cd "$(dirname "$(readlink -f "$0")")"

echo "Building $BIN_NAME (release)..."
cargo build --release --locked

remove_cargo_copy
$SUDO install -d "$BIN_DIR"
$SUDO install -m 755 "target/release/$BIN_NAME" "$DEST"
echo "Installed $DEST"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: $BIN_DIR is not in your PATH. Add it, e.g.:"
       echo "    export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

found="$(command -v "$BIN_NAME" || true)"
if [[ -n "$found" && "$found" != "$DEST" ]]; then
    echo "warning: '$BIN_NAME' in your PATH resolves to $found, not $DEST" >&2
elif [[ $REMOVED_CARGO_COPY -eq 1 ]]; then
    echo "note: run 'hash -r' (or open a new shell) if your shell still finds the old copy"
fi
