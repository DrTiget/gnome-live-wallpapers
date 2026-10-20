# WPE — архитектура

Аналог Wallpaper Engine для Ubuntu 26.04 / GNOME Shell 50 (только Wayland).
Лицензия GPL-3.0-or-later (из-за linux-wallpaperengine).

## Компоненты

```
┌──────────────────────┐   D-Bus (session)    ┌─────────────────────────────┐
│ wpe-gallery (GTK4/   │ ───────────────────▶ │ wpe-daemon (Rust, GTK4)     │
│ libadwaita)          │ ◀── signals ──────── │  core: каталог + state.json │
└──────────────────────┘                      │  renderer: окно на монитор   │
┌──────────────────────┐   D-Bus              │   ├ Video  — GTK MediaFile/  │
│ GNOME Shell ext.     │ ───────────────────▶ │   │          GStreamer       │
│ (ESM JS)             │  PointerEvent,       │   ├ Web    — WebKitGTK 6 +   │
│  - окна @wpe!<conn>  │  SetPaused,          │   │          JS-шим WE API   │
│    → фон рабочего    │  SetMonitors         │   ├ Scene  — подпроцесс      │
│    стола (Hanabi)    │                      │   │   linux-wallpaperengine  │
│  - курсор/клики      │                      │   └ Audio  — PipeWire monitor│
│  - пауза при fullscr │                      │             → FFT 128 полос │
└──────────────────────┘                      └──────────────┬──────────────┘
                                                             │ we-core
                                         Steam: steamapps/workshop/content/431960/<id>/project.json
```

| crate / каталог | роль |
|---|---|
| `crates/we-core` | парсинг `project.json` (все типы: video, web, scene, preset, application), свойства `general.properties`, локализация, валидация, сканер Steam-библиотек (`libraryfolders.vdf`, native/Flatpak/Snap Steam), пресеты. Без зависимостей от GTK, полностью покрыт тестами. |
| `crates/daemon` (`wpe-daemon`) | D-Bus сервис + рендерер. Без фичи `gtk` — headless (логирует команды рендеру; удобно для отладки GUI/расширения). С `--features gtk` — настоящий рендерер. |
| `crates/gallery` (`wpe-gallery`) | этап 1: CLI поверх D-Bus; этап 3: libadwaita-галерея. |
| `gnome-extension/wpe@wpe.github.io` | расширение Shell 50. |

### Демон

* `core.rs` — логика без GTK: `Catalog` + `State` (`$XDG_CONFIG_HOME/wpe/state.json`: назначения монитор→обои, `"*"` — все мониторы; оверрайды свойств; громкость). Пресеты разрешаются в базовые обои + значения пресета + пользовательские оверрайды. Наружу отдаёт команды `RenderCmd` трейту `Renderer`.
* `gtk_renderer.rs` — GTK-поток. Команды из потоков zbus передаются через `glib::MainContext::invoke`. На каждый монитор — undecorated fullscreen-окно с заголовком `@wpe!<connector>` (например `@wpe!DP-1`) — по этому маркеру расширение его находит.
  * **Video**: `gtk::MediaFile` (бэкенд GStreamer в GTK4) → `gtk::Picture` с `ContentFit::Cover`, loop, mute по умолчанию. NVIDIA 580: аппаратное декодирование через `nvcodec` (gst-plugins-bad), при проблемах — запасной вариант `playbin3` + `gtk4paintablesink` (gstreamer1.0-gtk4).
  * **Web**: `webkit6::WebView`, `file://` к `index.html`, разрешён доступ file→file. В начало документа инжектится `shim.js`, эмулирующий API WE:
    `wallpaperPropertyListener.applyUserProperties({key:{type,value}})` (в т.ч. если листенер назначен после первой отправки),
    `applyGeneralProperties`, `setPaused`, `wallpaperRegisterAudioListener(cb)` (массив 128 float: 64 L + 64 R),
    `wallpaperRegisterPausedListener`, синтетические `mousemove/pointer*/mousedown/mouseup/click` по координатам от расширения.
  * **Scene**: подпроцесс linux-wallpaperengine (см. ниже).
  * **Audio** (этап 3): `pipewire-rs`, захват монитора default sink (`stream.capture.sink=true`), окно 1024 сэмплов, FFT (`rustfft`), 64 логарифмические полосы на канал, нормализация + сглаживание как в WE, 30 Гц → `window.__wpe.audio([...])`. Только для обоев с `supportsaudioprocessing` и только если не на паузе.

### D-Bus API

Имя `io.github.wpe.Daemon`, объект `/io/github/wpe/Daemon`, интерфейс `io.github.wpe.Daemon1`:

| Метод | Сигнатура | Кто вызывает |
|---|---|---|
| `ListWallpapers() → s` | JSON `[{id,title,type,supported,preview,root,tags,audio,rating}]` | галерея |
| `Rescan()` | пересканировать Workshop + `extra_dirs` | галерея |
| `SetWallpaper(s monitor, s id)` | `monitor` = connector или `"*"`; пустой `id` снимает | галерея |
| `GetAssignments() → a{ss}` | | галерея |
| `GetProperties(s id) → s` | JSON `we_core::Properties` с применёнными оверрайдами | галерея |
| `SetProperty(s id, s key, s value_json)` | валидирует/клампит, сохраняет, отправляет в WebView | галерея |
| `ResetProperties(s id)` | | галерея |
| `PointerEvent(s monitor, d x, d y, s kind, u button)` | kind: `motion\|press\|release`, координаты локальные для монитора | расширение |
| `SetPaused(s monitor, b paused)` | | расширение |
| `SetMonitors(as connectors)` | раскладка мониторов изменилась | расширение |
| `SetVolume(d volume, b muted)` | | галерея |
| *(этап 2)* `GetRendererPids() → a{su}` | PID процессов сцен, чтобы расширение нашло их окна | расширение |

Свойство `Version (s)`. Сигналы: `CatalogChanged()`, `AssignmentsChanged()`, `PropertiesChanged(s id)`.

Демон стартует как systemd user unit (`wpe-daemon.service`, `PartOf=graphical-session.target`) и/или D-Bus activation.

### Расширение GNOME Shell 50

Подход Hanabi (jeffshee/gnome-ext-hanabi):
1. `global.display` `window-created` → окна с заголовком `@wpe!<conn>` (и окна с PID сцен).
2. `WindowActor` переносится в `Main.layoutManager._backgroundGroup` поверх стандартного фона нужного монитора; окно скрывается из alt-tab/обзора/дока (override `Workspace`/`AltTab` фильтров, как в Hanabi), делается sticky.
3. В обзоре (Activities) — клон актёра в фоне рабочих столов.
4. Ввод: окна в background group не получают событий, поэтому расширение слушает `captured-event` на stage; если под курсором фон (нет окна, не панель) — `PointerEvent` (motion с троттлингом ~60 Гц).
5. Пауза: для каждого монитора, если на активном рабочем столе есть fullscreen или maximized окно (`window.is_fullscreen()`, `get_maximized()`), не минимизированное, — `SetPaused(conn, true)`. Также пауза при блокировке экрана.

Wayland-ограничение: клиент не может сам стать фоном (в Mutter нет wlr-layer-shell), поэтому без расширения обои работают просто как обычные окна.

## Интеграция linux-wallpaperengine (сцены)

**Решение: подпроцесс, не FFI.**

* Лицензия не мешает ни одному варианту (весь проект GPL-3).
* linux-wallpaperengine — приложение, а не библиотека: собственный главный цикл, GLFW/SDL, глобальное состояние, CEF для web-элементов, FFmpeg/mpv. Встраивание в процесс GTK потребовало бы большого рефакторинга (два GL-контекста в одном потоке, конфликт главных циклов, CEF в одном процессе с WebKit).
* Падение рендерера сцены (частое для экзотических шейдеров) не убивает демон и остальные мониторы.
* Обновление апстрима — просто пересборка бинарника (подмодуль `third_party/linux-wallpaperengine`, пиннинг коммита).

Этапы интеграции:
* **2a**: запуск в оконном режиме (`--window <геометрия> --silent --fps N <путь>`; на Mutter его вывод `--screen-root` (wlr-layer-shell) недоступен). Окно находит расширение по PID и ставит в фон так же, как наши окна. Пауза — `SIGSTOP/SIGCONT`, громкость — `--volume`/`--silent`, свойства — `--set-property key=value` с перезапуском.
* **2b** (если нужен ввод мыши и бесшовность): небольшой патч в форке — управляющий канал (stdin/unix-socket: координаты мыши, пауза, свойства) и, опционально, офскрин-вывод в DMA-BUF, который демон показывает через `GdkDmabufTextureBuilder` в своём окне (тогда отдельные окна сцен не нужны). Патч предлагать в апстрим.

## Этапы

1. **MVP: Video + Web с интерактивом на фоне.** we-core (готово), D-Bus API (готово, headless), GTK-рендерер Video/Web + шим, расширение: фон + курсор/клики + пауза, CLI `wpe-gallery`, systemd user unit.
2. **Сцены**: сборка linux-wallpaperengine (подмодуль + скрипт), 2a, затем 2b.
3. **Аудио + галерея**: PipeWire → FFT → `wallpaperRegisterAudioListener`; libadwaita-галерея: сетка превью (GIF/JPG), фильтры по типу/тегам/поиск (`Catalog::search`), выбор монитора, редактор свойств, генерируемый по `PropertyKind` (color → `GtkColorDialogButton`, slider → `GtkScale`, combo → `AdwComboRow`, bool → `AdwSwitchRow`, text → `AdwEntryRow`, file/directory → `GtkFileDialog`), с учётом `condition` (простое вычисление `key.value == X`), локализация через `Project::localize`.
4. Дальше: плейлисты, импорт `.pkg`/распаковка, автозапуск, энергосбережение (пауза на батарее), web-свойства `directory` (`wallpaperRequestRandomFileForProperty`).
