#!/bin/bash
# Per-user install (no sudo): binaries, systemd --user unit, GNOME extension.
#   scripts/install.sh            build + install + enable for next login
#   scripts/install.sh --uninstall
# Nothing is started in the running session: on Wayland GNOME Shell only
# discovers new extensions after re-login, and the daemon would otherwise
# wait for the extension anyway.
set -euo pipefail
cd "$(dirname "$0")/.."
UUID=wpe@wpe.github.io
BIN=$HOME/.local/bin
EXT=${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/$UUID
APPS=${XDG_DATA_HOME:-$HOME/.local/share}/applications
ICONS=${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps
UNIT=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/wpe-daemon.service

if [ "${1:-}" = --uninstall ]; then
    systemctl --user disable --now wpe-daemon.service 2>/dev/null || true
    rm -f "$UNIT" "$BIN/wpe-daemon" "$BIN/wpe-gallery" "$APPS/io.github.wpe.Gallery.desktop" "$ICONS/io.github.wpe.Gallery.svg"
    gnome-extensions disable "$UUID" 2>/dev/null || true
    rm -rf "$EXT"
    systemctl --user daemon-reload
    echo "uninstalled (state kept in ~/.config/wpe)"
    exit 0
fi

CARGO=${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}
"$CARGO" build --release -p wpe-daemon --features gtk
"$CARGO" build --release -p wpe-gallery --features gtk

install -Dm755 target/release/wpe-daemon "$BIN/wpe-daemon"
install -Dm755 target/release/wpe-gallery "$BIN/wpe-gallery"
# ~/.local/bin is often not in the GNOME session PATH: use an absolute Exec
mkdir -p "$APPS"
sed "s|^Exec=wpe-gallery|Exec=$BIN/wpe-gallery|" data/io.github.wpe.Gallery.desktop > "$APPS/io.github.wpe.Gallery.desktop"
install -Dm644 data/io.github.wpe.Gallery.svg "$ICONS/io.github.wpe.Gallery.svg"
update-desktop-database "$APPS" 2>/dev/null || true
gtk-update-icon-cache -q -t "${ICONS%/scalable/apps}" 2>/dev/null || true
install -Dm644 data/wpe-daemon.service "$UNIT"
rm -rf "$EXT"
mkdir -p "$(dirname "$EXT")"
cp -r "gnome-extension/$UUID" "$EXT"

systemctl --user daemon-reload
systemctl --user enable wpe-daemon.service

# Enable the extension for the next login (same as `gnome-extensions enable`,
# which only works after the shell has discovered the extension).
current=$(gsettings get org.gnome.shell enabled-extensions)
if [[ $current != *"'$UUID'"* ]]; then
    if [[ $current == "@as []" || $current == "[]" ]]; then
        gsettings set org.gnome.shell enabled-extensions "['$UUID']"
    else
        gsettings set org.gnome.shell enabled-extensions "${current%]}, '$UUID']"
    fi
fi
echo "installed. Log out and back in, then: wpe-gallery list / wpe-gallery set '*' <id>"
