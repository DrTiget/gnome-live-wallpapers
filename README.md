# WPE — живые обои для GNOME (Wayland)

Аналог Wallpaper Engine для Ubuntu 26.04 / GNOME Shell 50: видео, web (HTML с интерактивом),
сцены (через [linux-wallpaperengine](https://github.com/Almamu/linux-wallpaperengine)),
аудиореактивность. Обои импортируются из Steam Workshop Wallpaper Engine (appid 431960).

Архитектура и план: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Лицензия: GPL-3.0-or-later.

## Состояние

| часть | статус |
|---|---|
| `we-core` | готово, тесты зелёные |
| `wpe-daemon` (headless: D-Bus, каталог, state) | собирается, тесты |
| `wpe-daemon --features gtk` (рендерер Video/Web/Scene) | каркас, **не собирался** на dev-сервере (нет GTK dev-библиотек) — проверить на десктопе |
| `wpe-gallery` | CLI (`list/scan/set/props/prop`); GUI — этап 3 |
| `gnome-extension` | metadata + каркас `extension.js` |

## Зависимости (Ubuntu 26.04)

```sh
sudo apt install build-essential pkg-config git curl \
  libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav gstreamer1.0-gl gstreamer1.0-gtk4 \
  libpipewire-0.3-dev libspa-0.2-dev clang libclang-dev
# сцены (этап 2), для сборки linux-wallpaperengine — сверить с его README:
sudo apt install cmake libglfw3-dev libglew-dev libglm-dev libsdl2-dev liblz4-dev \
  libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libmpv-dev libpulse-dev \
  libfftw3-dev libxrandr-dev libxinerama-dev libxcursor-dev libxi-dev libxxf86vm-dev \
  libwayland-dev wayland-protocols libxkbcommon-dev
```

Rust — через rustup (без sudo): `curl https://sh.rustup.rs -sSf | sh`.

## Сборка

```sh
cargo test --workspace                       # we-core + headless daemon
cargo build --release -p wpe-daemon --features gtk
cargo build --release -p wpe-gallery

./target/release/wpe-gallery scan ~/.local/share/Steam/steamapps/workshop/content/431960
./target/release/wpe-daemon &                # D-Bus io.github.wpe.Daemon
./target/release/wpe-gallery list
./target/release/wpe-gallery set '*' <workshop-id>
./target/release/wpe-gallery prop <id> schemecolor '"1 0 0"'
```

Расширение:

```sh
cp -r gnome-extension/wpe@wpe.github.io ~/.local/share/gnome-shell/extensions/
gnome-extensions enable wpe@wpe.github.io   # после перелогина (Wayland)
```
