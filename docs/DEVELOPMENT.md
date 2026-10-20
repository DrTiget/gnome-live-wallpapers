# Разработка

## Структура репозитория

| Путь | Что |
|---|---|
| `crates/we-core` | парсер `project.json`, свойства, каталог, сканер Steam; без GTK, с тестами (`tests/`, фикстуры в `tests/fixtures`) |
| `crates/daemon` | `wpe-daemon`: `core.rs` (каталог + состояние), `dbus.rs` (D-Bus), `gtk_renderer.rs`, `scene.rs`, `audio.rs`, `webpath.rs`, `shim.js`, `state.rs` |
| `crates/gallery` | `wpe-gallery`: `main.rs` (CLI), `gui.rs` (libadwaita, фича `gtk`) |
| `gnome-extension/wpe@wpe.github.io` | расширение GNOME Shell 50 |
| `third_party/linux-wallpaperengine` | патчи к апстриму и README к ним |
| `scripts/` | установка, сборка рендерера сцен, nested-тест и его помощники |
| `data/` | systemd unit, `.desktop`, иконка |
| `examples/` | тестовые обои |
| `docs/` | архитектура, скриншоты |

Архитектура: [ARCHITECTURE.md](ARCHITECTURE.md).

## Сборка

Зависимости — в [README](../README.md#установка). Без GTK-фич собираются только we-core, headless-демон и CLI:

```sh
cargo build --workspace
cargo build --release -p wpe-daemon --features gtk     # настоящий рендерер
cargo build --release -p wpe-gallery --features gtk    # GUI + CLI
```

Headless-демон (без `gtk`) только логирует команды рендеру — удобно для разработки галереи и расширения.

## Тесты

```sh
cargo test --workspace          # we-core + headless-демон, GTK не нужен
node scripts/test-shim.mjs      # JS-шим API Wallpaper Engine
```

## Интеграционный тест: `scripts/nested-test.sh`

Запускает отдельную headless GNOME Shell (`gnome-shell --headless --virtual-monitor …`) в своём
`dbus-run-session`, GSettings в keyfile, свои `XDG_*` — реальная сессия и dconf не затрагиваются.
Нужны собранные в `target/release` `wpe-daemon` и `wpe-gallery` с `--features gtk` и установленное
расширение (`scripts/install.sh`). Результаты — в `/tmp/wpe-nested/` (`*.png`, `shell.log`, `daemon.log`).

```sh
scripts/nested-test.sh
WPE_SHELL_MODE=ubuntu WPE_MONITORS="1280x720 1024x768" scripts/nested-test.sh   # с dock и DING, 2 монитора
WPE_SCENES="beach 3582604159" scripts/nested-test.sh        # сцены: скриншоты, FPS, CPU/GPU, курсор, пауза
WPE_CPU="web-interactive video-test" scripts/nested-test.sh # CPU по процессам
WPE_PAUSE=1 WPE_MONITORS="1920x1080 1280x720" scripts/nested-test.sh  # решения о паузе
WPE_AUDIO=1 WPE_AUDIO_SCENES=3479671230 scripts/nested-test.sh        # спектр: свой pipewire, тон 220 Гц L / 3 кГц R
WPE_GUI=1 scripts/nested-test.sh                            # GUI галереи, скриншоты gui-*.png
```

Переменные: `WPE_TEST_DIR` (каталог тестовых обоев, по умолчанию `~/wpe-test`; можно `examples/` после
`examples/make-test-video.sh`), `WPE_GUI_ACTIONS` (действия `rd-probe.py`: `click:x,y`, `sleep:сек`, `shot:имя`),
`OUT` и `WPE_NESTED_DISPLAY` (разводят параллельные запуски), `WPE_DEBUG`, `WPE_EXT_DEBUG`, `WPE_EXT_SRC`.
`scripts/rd-probe.py` делает скриншоты и синтетический ввод через Mutter RemoteDesktop/ScreenCast.

## Расширение GNOME Shell

После правки `extension.js` расширение нужно переустановить (`scripts/install.sh`) и перелогиниться; быстрее
проверять в nested-тесте (`WPE_EXT_SRC=gnome-extension/wpe@wpe.github.io`).

## Патчи linux-wallpaperengine

Апстрим закреплён коммитом `UPSTREAM_COMMIT` в `scripts/build-scene-renderer.sh`; наши изменения лежат как
патчи `third_party/linux-wallpaperengine/NNNN-*.patch`. Скрипт применяет те, чьей темы (subject) ещё нет в ветке `wpe`.

Добавить патч:

```sh
cd ~/src/linux-wallpaperengine && git checkout wpe
# правки, затем:
git commit -am "Тема патча"
git format-patch -1 -o /путь/к/репо/third_party/linux-wallpaperengine --start-number 4
```

Назовите файл `000N-имя.patch`, опишите его в `third_party/linux-wallpaperengine/README.md`. Для смены версии
апстрима обновите `UPSTREAM_COMMIT` и убедитесь, что патчи применяются.

## Стиль

Rust-код и комментарии на английском, пользовательские строки интерфейса — на русском. Формат: `cargo fmt`
не обязателен, но следуйте окружающему коду.
