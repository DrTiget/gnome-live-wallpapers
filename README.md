# WPE — живые обои для GNOME (Wayland)

Аналог Wallpaper Engine для Ubuntu 26.04 / GNOME Shell 50: видео, web (HTML с интерактивом),
сцены (через [linux-wallpaperengine](https://github.com/Almamu/linux-wallpaperengine)),
аудиореактивность. Обои импортируются из Steam Workshop Wallpaper Engine (appid 431960).

Архитектура и план: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Лицензия: GPL-3.0-or-later.

## Состояние

| часть | статус |
|---|---|
| `we-core` | готово, тесты зелёные |
| `wpe-daemon --features gtk` | этап 1 готов: видео (loop, без звука, cover) и web (WebKitGTK 6 + шим API WE: свойства, курсор/клики, пауза, аудио-API), окно на монитор, hotplug |
| `wpe-daemon` без `gtk` | headless: D-Bus, каталог, state — для разработки GUI/расширения |
| `wpe-gallery` | CLI (`list/scan/add-dir/set/props/prop/volume`); GUI — этап 3 |
| `gnome-extension` | готово: окно рендерера → фон под всеми окнами (и в обзоре), скрыто из обзора/alt-tab/дока, курсор/клики с пустого рабочего стола (и поверх DING), пауза при maximized/fullscreen и блокировке |
| сцены, аудиоспектр | этапы 2–3 |

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

## Сборка и тесты

```sh
cargo test --workspace                        # we-core + headless daemon
node scripts/test-shim.mjs                    # JS-шим API Wallpaper Engine
cargo build --release -p wpe-daemon --features gtk
cargo build --release -p wpe-gallery
scripts/nested-test.sh                        # интеграционный тест в отдельной headless GNOME Shell
WPE_SHELL_MODE=ubuntu WPE_MONITORS="1280x720 1024x768" scripts/nested-test.sh
```

`nested-test.sh` не трогает текущую сессию (свой D-Bus, GSettings в keyfile, свои `XDG_*`),
скриншоты и логи — в `/tmp/wpe-nested/`. Тестовые обои: `examples/` (видео генерируется
`examples/make-test-video.sh`), путь задаётся `WPE_TEST_DIR` (по умолчанию `~/wpe-test`; можно указать `examples/` после `make-test-video.sh`).

## Установка (без sudo)

```sh
scripts/install.sh       # ~/.local/bin/wpe-{daemon,gallery}, systemd --user unit (enable),
                         # расширение в ~/.local/share/gnome-shell/extensions + включение
# выйти из сеанса и войти снова (Wayland подхватывает новые расширения только так)
wpe-gallery add-dir ~/wpe-test        # или обои из Steam Workshop находятся сами
wpe-gallery list
wpe-gallery set '*' web-interactive   # '*' — все мониторы, или DP-1 / HDMI-1 …
wpe-gallery prop web-interactive schemecolor '"1 0.3 0.1"'
wpe-gallery volume 0.5 false          # включить звук (по умолчанию выключен)
scripts/install.sh --uninstall
```

Состояние: `~/.config/wpe/state.json`. Свои обои без Steam: `~/.local/share/wpe/wallpapers/<имя>/project.json`.

Диагностика:

```sh
gnome-extensions info wpe@wpe.github.io     # State: ACTIVE
systemctl --user status wpe-daemon
journalctl --user -u wpe-daemon -f          # демон
journalctl -f /usr/bin/gnome-shell | grep wpe   # расширение
systemctl --user stop wpe-daemon            # убрать обои немедленно
```

Отладка: `WPE_DEBUG=1` (демон логирует команды рендеру), `WPE_NOHIDE=1` (обычные окна с рамкой,
без расширения), `WPE_EXT_DEBUG=1` в окружении gnome-shell (лог ввода/геометрии расширения).
