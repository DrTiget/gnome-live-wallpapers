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
| `crates/we-core` | парсинг `project.json` (все типы: video, web, scene, preset, application; без `type` — по расширению `file`, в т.ч. `.exe` → application), свойства `general.properties`, локализация, валидация, сканер Steam-библиотек (`libraryfolders.vdf`, native/Flatpak/Snap Steam), пресеты. Без зависимостей от GTK, полностью покрыт тестами. |
| `crates/daemon` (`wpe-daemon`) | D-Bus сервис + рендерер. Без фичи `gtk` — headless (логирует команды рендеру; удобно для отладки GUI/расширения). С `--features gtk` — настоящий рендерер. |
| `crates/gallery` (`wpe-gallery`) | этап 1: CLI поверх D-Bus; этап 3: libadwaita-галерея. |
| `gnome-extension/wpe@wpe.github.io` | расширение Shell 50. |

### Демон

* `core.rs` — логика без GTK: `Catalog` + `State` (`$XDG_CONFIG_HOME/wpe/state.json`: назначения монитор→обои, `"*"` — все мониторы; оверрайды свойств; громкость). Пресеты разрешаются в базовые обои + значения пресета + пользовательские оверрайды. Наружу отдаёт команды `RenderCmd` трейту `Renderer`.
* `gtk_renderer.rs` — GTK-поток. Команды из потоков zbus передаются через `glib::MainContext::invoke`. На каждый монитор — окно без рамки размером с монитор и заголовком `@wpe!<connector>` (например `@wpe!DP-1`) — по этому маркеру расширение его находит (не fullscreen: иначе Shell считал бы монитор занятым полноэкранным окном и прятал верхнюю панель). Горячее подключение мониторов — `gdk::Display::monitors()::items-changed`.
  * **Video**: `gtk::MediaFile` (бэкенд GStreamer в GTK4) → `gtk::Picture` с `ContentFit::Cover`, loop, без звука по умолчанию (`State.muted = true`, `SetVolume`). NVIDIA 580: аппаратное декодирование через `nvcodec` (gst-plugins-bad), при проблемах — запасной вариант `playbin3` + `gtk4paintablesink` (gstreamer1.0-gtk4).
  * **Web**: `webkit6::WebView`, страница грузится через собственную схему `wpe://<хеш каталога>/<file>` (`webpath.rs`: защита от выхода за каталог, MIME, HTTP Range для `<video>`), схема зарегистрирована как secure + CORS. Не `file://`: SPA-фреймворки (официальные обои CORSAIR Collection на Angular) вызывают `history.replaceState()` с URL каталога, что WebKit запрещает для `file:` — белый экран. Вызовы шима из демона безопасны до загрузки страницы; свойства и состояние паузы досылаются после каждой (пере)загрузки. `WPE_DEBUG=1` выводит консоль JS в лог демона. В начало документа инжектится `shim.js`, эмулирующий API WE:
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
| `Rescan()` | пересканировать Workshop + `~/.local/share/wpe/wallpapers` + `extra_dirs` | галерея |
| `SetWallpaper(s monitor, s id)` | `monitor` = connector или `"*"`; пустой `id` снимает | галерея |
| `GetAssignments() → a{ss}` | | галерея |
| `GetProperties(s id) → s` | JSON `we_core::Properties` с применёнными оверрайдами | галерея |
| `SetProperty(s id, s key, s value_json)` | валидирует/клампит, сохраняет, отправляет в WebView | галерея |
| `ResetProperties(s id)` | | галерея |
| `PointerEvent(s monitor, d x, d y, s kind, u button)` | kind: `motion\|press\|release`, координаты локальные для монитора | расширение |
| `SetPaused(s monitor, b paused)` | | расширение |
| `SetMonitors(as connectors)` | раскладка мониторов изменилась | расширение |
| `SetVolume(d volume, b muted)` | | галерея |
| `AddDirectory(s path)` | добавить каталог с обоями (сохраняется в `extra_dirs`), пересканировать | галерея |
| *(этап 2)* `GetRendererPids() → a{su}` | PID процессов сцен, чтобы расширение нашло их окна | расширение |

Свойство `Version (s)`. Сигналы: `CatalogChanged()`, `AssignmentsChanged()`, `PropertiesChanged(s id)`.

Демон стартует как systemd user unit (`wpe-daemon.service`, `PartOf=graphical-session.target`) и/или D-Bus activation.

### Расширение GNOME Shell 50

`gnome-extension/wpe@wpe.github.io/extension.js` — подход Hanabi (jeffshee/gnome-ext-hanabi), адаптированный и проверенный на Shell 50.1 (headless-сессия, см. «Тестирование»):

1. **Поиск окон.** `window_manager::map`, `display::window-created` (+ `notify::title`): окна с заголовком `@wpe!<connector>`. Индекс монитора — `global.backend.get_monitor_manager().get_monitor_for_connector()`.
2. **Окно в фон.** В `BackgroundManager._createBackgroundActor` (InjectionManager) в каждый фон монитора добавляется `St.Widget` с `Clutter.Clone` актёра окна рендерера. Фон есть и на рабочем столе, и в миниатюрах/рабочих столах обзора, поэтому обои видны везде. Экран блокировки не трогаем.
3. **Как прячем настоящее окно.** *Не сворачиваем* (в отличие от Hanabi): Mutter 50 даёт свёрнутому окну xdg-toplevel `suspended`, GTK/WebKit перестают рисовать, клон замирает (проверено). Вместо этого окно остаётся отображённым: `stick()`, `lower()`, не `above`, а его `MetaWindowActor` скрыт (`hide()` + повторное скрытие на `notify::visible`). Клон рисует скрытый источник, Mutter продолжает слать frame callbacks — 60 FPS.
4. **Геометрия.** Mutter автоматически подгоняет окно размером с монитор под рабочую область (док/панель: 1533×868 вместо 1600×900). Расширение делает `unmaximize()` + `move_resize_frame(user_op=true, геометрия монитора)` на `position-changed`/`size-changed`/`monitors-changed`, иначе клон растягивается и координаты курсора съезжают.
5. **Скрытие из списков** (как Hanabi): `Shell.Global.get_window_actors`, `Workspace/WorkspaceThumbnail._isOverviewWindow`, `Meta.Display.get_tab_list` (alt-tab), `Shell.WindowTracker.get_window_app`, `Shell.App.get_windows/get_n_windows`, `Shell.AppSystem.get_running` (док). Уведомление «… готово» (`WindowAttentionHandler`, подключён bound-методом) переподключается через фильтр. Если фокус попал в окно рендерера — он передаётся верхнему обычному окну.
6. **Ввод.** `global.stage::captured-event` (motion/press/release). Событие считается «на рабочем столе», если под курсором `global.stage`, актёр из `Main.layoutManager._backgroundGroup` (фон реактивен из-за контекстного меню) или прозрачное окно Desktop Icons NG (Ubuntu, `com.rastersoft.ding` / заголовок `@!…`/`Desktop Icons N`). Не в обзоре, без модальных диалогов, не на экране блокировки. Release доставляется, если press был на рабочем столе. Motion ограничен ~60 Гц с доставкой последней позиции. → `PointerEvent(connector, x, y локальные, kind, button)` → демон → `window.__wpe.pointer()` → DOM `mousemove/pointer*/mousedown/mouseup/click`.
7. **Пауза.** Монитор на паузе, если на активном рабочем столе есть видимое (`showing_on_its_workspace`) окно `fullscreen || is_maximized()` на этом мониторе (без рендереров, DING, skip-taskbar), или экран заблокирован (`session-modes: user, unlock-dialog` — расширение не выгружается при блокировке). Пересчёт по сигналам окон/рабочих столов, отправляются только изменения `SetPaused`; при (пере)запуске демона состояние шлётся заново (`bus_watch_name`).
   В демоне: видео — `MediaFile.set_playing(false)`; web — шим откладывает колбэки `requestAnimationFrame`, ставит на паузу `<video>/<audio>` и вызывает `setPaused`/`wallpaperRegisterPausedListener`.

Защита в демоне: окна создаются, только пока расширение `ACTIVE` (`org.gnome.Shell.Extensions.GetExtensionInfo`, опрос раз в 2 с); если расширение выключено — окна закрываются. Иначе без расширения на экране оказалось бы обычное окно поверх рабочего стола. `WPE_NOHIDE=1` — отладочный режим: обычные окна с рамкой, без проверки.

Wayland-ограничение: клиент не может сам стать фоном (в Mutter нет wlr-layer-shell), поэтому без расширения обои не показываются.

### Тестирование без перелогина

`scripts/nested-test.sh` запускает **отдельную headless GNOME Shell** (`gnome-shell --headless --virtual-monitor …`) в собственном `dbus-run-session`, с GSettings в keyfile и своими `XDG_*` каталогами — реальная сессия и её dconf не затрагиваются. Внутри: демон, CLI, `scripts/rd-probe.py` (Mutter RemoteDesktop + ScreenCast: скриншоты и синтетические движения/клики), `scripts/maximized-window.py` (проверка паузы). Переменные: `WPE_SHELL_MODE=ubuntu` (с ubuntu-dock и DING как в реальной сессии), `WPE_MONITORS="1280x720 1024x768"` (несколько мониторов), `WPE_DEBUG=1` (лог команд рендеру), `WPE_EXT_DEBUG=1` (лог расширения). Результаты — в `/tmp/wpe-nested/*.png`, `shell.log`, `daemon.log`.

`node scripts/test-shim.mjs` — юнит-тест JS-шима.

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

1. **MVP: Video + Web с интерактивом на фоне** — готово и проверено в headless GNOME Shell 50.1 на NVIDIA 580 (1 и 2 монитора, режимы `user` и `ubuntu` с DING): we-core, D-Bus API, GTK-рендерер Video/Web + шим, расширение (фон, скрытие окон, курсор/клики, пауза), CLI `wpe-gallery`, systemd user unit, `scripts/install.sh`.
2. **Сцены**: сборка linux-wallpaperengine (подмодуль + скрипт), 2a, затем 2b.
3. **Аудио + галерея**: PipeWire → FFT → `wallpaperRegisterAudioListener`; libadwaita-галерея: сетка превью (GIF/JPG), фильтры по типу/тегам/поиск (`Catalog::search`), выбор монитора, редактор свойств, генерируемый по `PropertyKind` (color → `GtkColorDialogButton`, slider → `GtkScale`, combo → `AdwComboRow`, bool → `AdwSwitchRow`, text → `AdwEntryRow`, file/directory → `GtkFileDialog`), с учётом `condition` (простое вычисление `key.value == X`), локализация через `Project::localize`.
4. Дальше: плейлисты, импорт `.pkg`/распаковка, автозапуск, энергосбережение (пауза на батарее), web-свойства `directory` (`wallpaperRequestRandomFileForProperty`).
