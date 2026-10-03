#!/bin/sh
set -eu
cd "$(dirname "$0")"
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
cargo build --release
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"
cp -f target/release/hack-shell "$HOME/.local/bin/hack-shell"
cp -f data/hack-shell.desktop "$HOME/.local/share/applications/hack-shell.desktop"
chmod +x "$HOME/.local/bin/hack-shell"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$HOME/.local/share/applications" || true
fi
echo "Installed ~/.local/bin/hack-shell"
echo "Launch it from the app menu, or run: hack-shell"
echo
echo "Make it Plasma's default terminal with:"
echo "  kwriteconfig6 --file kdeglobals --group General --key TerminalApplication hack-shell"
echo "  kwriteconfig6 --file kdeglobals --group General --key TerminalService hack-shell.desktop"
