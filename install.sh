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

if [[ $UNINSTALL -eq 1 ]]; then
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

$SUDO install -d "$BIN_DIR"
$SUDO install -m 755 "target/release/$BIN_NAME" "$DEST"
echo "Installed $DEST"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: $BIN_DIR is not in your PATH. Add it, e.g.:"
       echo "    export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
