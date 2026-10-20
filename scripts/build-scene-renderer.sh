#!/bin/bash
# Build linux-wallpaperengine (scene renderer) with our patch and install it
# per-user, no sudo:
#   ~/.local/lib/linux-wallpaperengine/   binary + CEF runtime (RPATH=$ORIGIN)
#   ~/.local/bin/linux-wallpaperengine    symlink (what wpe-daemon runs)
# usage: scripts/build-scene-renderer.sh [src-dir]   (default ~/src/linux-wallpaperengine)
# Build deps: see README ("сцены").
set -euo pipefail
UPSTREAM=https://github.com/Almamu/linux-wallpaperengine.git
UPSTREAM_COMMIT=b016d7d
REPO=$(cd "$(dirname "$0")/.." && pwd)
PATCHES=$REPO/third_party/linux-wallpaperengine
SRC=${1:-$HOME/src/linux-wallpaperengine}
PREFIX=$HOME/.local/lib/linux-wallpaperengine

if [ ! -d "$SRC/.git" ]; then
    git clone "$UPSTREAM" "$SRC"
fi
cd "$SRC"
if ! git rev-parse -q --verify wpe >/dev/null; then
    git fetch -q origin
    git checkout -q -b wpe "$UPSTREAM_COMMIT"
else
    git checkout -q wpe
fi
# Apply our patches that are not on the branch yet (matched by subject).
for p in "$PATCHES"/0*.patch; do
    subject=$(sed -n 's/^Subject: \[PATCH[^]]*\] //p' "$p" | head -1)
    if ! git log --format=%s "$UPSTREAM_COMMIT"..wpe | grep -qF "$subject"; then
        git -c user.name=WPE -c user.email=wpe@localhost am -q "$p"
    fi
done
git submodule update --init --recursive -q

cmake -S . -B build -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_RPATH='$ORIGIN:$ORIGIN/lib'
cmake --build build -j"$(nproc)"
cmake --install build
mkdir -p "$HOME/.local/bin"
ln -sf "$PREFIX/linux-wallpaperengine" "$HOME/.local/bin/linux-wallpaperengine"
"$PREFIX/linux-wallpaperengine" --help | grep -q -- --control-stdin
echo "installed: $HOME/.local/bin/linux-wallpaperengine -> $PREFIX"
