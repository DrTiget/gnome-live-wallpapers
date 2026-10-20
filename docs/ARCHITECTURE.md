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
  * **Scene**: подпроцесс linux-wallpaperengine (`scene.rs` — аргументы и протокол, `gtk_renderer.rs` — жизненный цикл), своего окна демон не создаёт. См. «Интеграция linux-wallpaperengine».
  * **Audio** (этап 3, `crates/daemon/src/audio.rs`): подпроцесс `pw-record` (`stream.capture.sink=true` — монитор default sink, f32 стерео 24 кГц; без сборки биндингов libpipewire), окно Ханна 1024, свой radix-2 FFT, 64 логарифмические полосы на канал (40 Гц–11 кГц, −60…0 дБ → 0…1), быстрая атака / медленный спад, 30 Гц → `window.__wpe.audio([...])`; тишина после первого нулевого кадра не отправляется. Захват идёт, только пока есть видимая (не на паузе) web-страница, вызвавшая `wallpaperRegisterAudioListener` (шим сообщает об этом через `webkit.messageHandlers.wpe`); иначе `pw-record` завершён. Сцены захватывают звук сами (linux-wallpaperengine, pulse API, при `supportsaudioprocessing`), независимо от `--silent`.

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
| `SetPaused(s monitor, b paused)` | монитор закрыт (развёрнутое/полноэкранное окно или окно >90% монитора) или экран заблокирован; решение о паузе принимает демон по настройкам | расширение |
| `SetFullscreenActive(b active)` | на каком-то мониторе полноэкранное приложение (fullscreen или фокусное окно на весь монитор вместе с панелью) | расширение |
| `SetPauseOnMaximized(b)` / `SetPauseOnFullscreenAny(b)` | настройки паузы (в `state.json`, по умолчанию вкл.; в `GetSettings`) | галерея |
| `SetMonitors(as connectors)` | раскладка мониторов изменилась | расширение |
| `SetVolume(d volume, b muted)` | | галерея |
| `AddDirectory(s path)` | добавить каталог с обоями (сохраняется в `extra_dirs`), пересканировать | галерея |
| `GetRendererPids() → a{su}` | монитор → PID процесса сцены, чтобы расширение нашло его окно; изменения — сигнал `RendererPidsChanged()` | расширение |
| `SetFps(u fps)` / `GetFps() → u` | лимит FPS (1..240, по умолчанию 30, в `state.json`): сцены (перезапуск) и `requestAnimationFrame` web-обоев | галерея |

Свойство `Version (s)`. Сигналы: `CatalogChanged()`, `AssignmentsChanged()`, `PropertiesChanged(s id)`, `RendererPidsChanged()`.

Демон стартует как systemd user unit (`wpe-daemon.service`, `PartOf=graphical-session.target`) и/или D-Bus activation.

### Расширение GNOME Shell 50

`gnome-extension/wpe@wpe.github.io/extension.js` — подход Hanabi (jeffshee/gnome-ext-hanabi), адаптированный и проверенный на Shell 50.1 (headless-сессия, см. «Тестирование»):

1. **Поиск окон.** `window_manager::map`, `display::window-created` (+ `notify::title`): окна с заголовком `@wpe!<connector>` и окна процессов из `GetRendererPids` (`Meta.Window.get_pid()`; список обновляется по `RendererPidsChanged`, после чего уже открытые окна пересматриваются). Монитор запоминается в окне при захвате: старое окно перезапускаемой сцены остаётся скрытым до закрытия, а клон берёт самое новое окно монитора. Индекс монитора — `global.backend.get_monitor_manager().get_monitor_for_connector()`.
2. **Окно в фон.** В `BackgroundManager._createBackgroundActor` (InjectionManager) в каждый фон монитора добавляется `St.Widget` с `Clutter.Clone` актёра окна рендерера. Фон есть и на рабочем столе, и в миниатюрах/рабочих столах обзора, поэтому обои видны везде. Экран блокировки не трогаем.
3. **Как прячем настоящее окно.** *Не сворачиваем* (в отличие от Hanabi): Mutter 50 даёт свёрнутому окну xdg-toplevel `suspended`, GTK/WebKit перестают рисовать, клон замирает (проверено). Вместо этого окно остаётся отображённым: `stick()`, `lower()`, не `above`, а его `MetaWindowActor` скрыт (`hide()` + повторное скрытие на `notify::visible`). Клон рисует скрытый источник, Mutter продолжает слать frame callbacks — 60 FPS.
4. **Геометрия.** Mutter автоматически подгоняет окно размером с монитор под рабочую область (док/панель: 1533×868 вместо 1600×900). Расширение делает `unmaximize()` + `move_resize_frame(user_op=true, геометрия монитора)` на `position-changed`/`size-changed`/`monitors-changed`, иначе клон растягивается и координаты курсора съезжают.
5. **Скрытие из списков** (как Hanabi): `Shell.Global.get_window_actors`, `Workspace/WorkspaceThumbnail._isOverviewWindow`, `Meta.Display.get_tab_list` (alt-tab), `Shell.WindowTracker.get_window_app`, `Shell.App.get_windows/get_n_windows`, `Shell.AppSystem.get_running` (док). Уведомление «… готово» (`WindowAttentionHandler`, подключён bound-методом) переподключается через фильтр. Если фокус попал в окно рендерера — он передаётся верхнему обычному окну.
6. **Ввод.** `global.stage::captured-event` (motion/press/release). Событие считается «на рабочем столе», если под курсором `global.stage`, актёр из `Main.layoutManager._backgroundGroup` (фон реактивен из-за контекстного меню) или прозрачное окно Desktop Icons NG (Ubuntu, `com.rastersoft.ding` / заголовок `@!…`/`Desktop Icons N`). Не в обзоре, без модальных диалогов, не на экране блокировки. Release доставляется, если press был на рабочем столе. Motion ограничен ~60 Гц с доставкой последней позиции. → `PointerEvent(connector, x, y локальные, kind, button)` → демон → `window.__wpe.pointer()` → DOM `mousemove/pointer*/mousedown/mouseup/click`.
7. **Пауза.** Расширение сообщает факты, демон решает (`Core::update_pause`). Монитор «закрыт», если на активном рабочем столе есть видимое (`showing_on_its_workspace`) окно `fullscreen || is_maximized()` или окно, покрывающее больше 90% монитора (без рендереров, DING, skip-taskbar), или экран заблокирован (`session-modes: user, unlock-dialog`) → `SetPaused`. «Полноэкранное приложение» — окно `fullscreen` или фокусное окно на весь монитор вместе с верхней панелью (развёрнутое окно до панели не достаёт) → `SetFullscreenActive`. Пауза монитора = (закрыт и `pause_on_maximized`) или (полноэкранное приложение где угодно и `pause_on_fullscreen_any`). Пересчёт по сигналам окон (в т.ч. размер/позиция), фокуса и рабочих столов, отправляются только изменения; при (пере)запуске демона состояние шлётся заново (`bus_watch_name`). Демон пишет каждое изменение в журнал (`[pause] DP-1: paused`).
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

Как сделано (этап 2):
* Сборка — `scripts/build-scene-renderer.sh`: апстрим, пиннинг коммита, наш патч `third_party/linux-wallpaperengine/0001-control-stdin.patch`, установка в `~/.local/lib/linux-wallpaperengine` (RPATH `$ORIGIN`, рядом рантайм CEF) + симлинк `~/.local/bin/linux-wallpaperengine`. Демон ищет бинарник в `$WPE_SCENE_BIN`, `~/.local/bin`, затем в `PATH` (у systemd user unit `~/.local/bin` в PATH обычно нет).
* Запуск: `linux-wallpaperengine --control-stdin --no-fullscreen-pause --window 0x0x<W>x<H> --scaling fill --fps <N> --silent|--volume <0..100> --assets-dir <Steam>/common/wallpaper_engine/assets [--set-property key=value…] <каталог>`; `LC_NUMERIC=C` (значения парсятся `std::stof`). Свойства: bool → `1/0`, цвет `"r g b"`, числа и строки как есть, пустые файлы пропускаются. На Mutter нет wlr-layer-shell, поэтому `--screen-root` неприменим — оконный режим (GLFW, Wayland). Позиция окна игнорируется Wayland — её выставляет расширение.
* Окно находит расширение по PID и ставит в фон так же, как наши окна (не сворачивается, полный размер монитора, скрытый актёр + клон).
* Управляющий канал (патч `--control-stdin`, строки в stdin): `m x y` — курсор (координаты окна, от левого верхнего угла), `b l|r 0|1` — кнопки, `p 0|1` — пауза (цикл рендера спит, как при собственной fullscreen-паузе апстрима), `q` — выход; EOF тоже завершает процесс, так что падение демона не оставляет сирот. Почему не SIGSTOP: остановленный Wayland-клиент не отвечает на ping композитора («приложение не отвечает»), а при паузе процесс жив и отвечает. Курсор от расширения (`PointerEvent`) уходит в `GLFWMouseInput` вместо `glfwGetCursorPos`, поэтому параллакс и курсорные эффекты работают, хотя настоящее окно ввода не получает.
* Свойства, громкость и FPS передаются только командной строкой → при изменении процесс перезапускается (старый получает `q`, через 3 с — SIGKILL, если не вышел). Упавший сам процесс снимается с монитора (лог `[scene] … exited`), на его месте — обычный фон GNOME.
* Каталог: кроме Workshop сканируются встроенные проекты `<Steam>/common/wallpaper_engine/projects/defaultprojects` (`we_core::wallpaper_engine_dirs`).

8. **Нагрузка.** Web: на проприетарном NVIDIA WebKitGTK сам отключает DMA-BUF и передаёт каждый кадр через разделяемую память, которую UI-процесс копирует на CPU; Vulkan-рендерер GTK к тому же медленно импортирует DMA-BUF. Поэтому демон на NVIDIA (если не задано в окружении) ставит `WEBKIT_FORCE_DMABUF_RENDERER=1` и `GSK_RENDERER=ngl`; шим ограничивает `requestAnimationFrame` лимитом FPS (равномерная сетка кадров, на паузе не вызывается); при закрытии web-обоев их web-процесс завершается. Видео: `playbin` → `glsinkbin` → `gtk4paintablesink` (аппаратные декодеры по рангу, кадры остаются GL-текстурами); `GtkMediaFile` — запасной путь. Замеры (nested, GTX 1660 SUPER, 1 монитор 1920×1080, 30 FPS): анимированные web-обои 12–15% CPU суммарно (было ~85%), статичная страница 0,4%, видео 1080p30 H.264 4:2:0 14%.

Дальше (не сделано): офскрин-вывод в DMA-BUF и показ в окне демона (`GdkDmabufTextureBuilder`) — убрал бы отдельные окна и перезапуски; изменение свойств без перезапуска через тот же канал.

## Этапы

1. **MVP: Video + Web с интерактивом на фоне** — готово и проверено в headless GNOME Shell 50.1 на NVIDIA 580 (1 и 2 монитора, режимы `user` и `ubuntu` с DING): we-core, D-Bus API, GTK-рендерер Video/Web + шим, расширение (фон, скрытие окон, курсор/клики, пауза), CLI `wpe-gallery`, systemd user unit, `scripts/install.sh`.
2. **Сцены** — готово (см. выше): сборка с патчем, подпроцесс в окне по PID, пауза/курсор через `--control-stdin`, свойства/FPS/громкость с перезапуском. Проверено в headless GNOME Shell на реальных сценах (`WPE_SCENES=… scripts/nested-test.sh`).
3. **Аудио + галерея**: PipeWire → FFT → `wallpaperRegisterAudioListener`; libadwaita-галерея: сетка превью (GIF/JPG), фильтры по типу/тегам/поиск (`Catalog::search`), выбор монитора, редактор свойств, генерируемый по `PropertyKind` (color → `GtkColorDialogButton`, slider → `GtkScale`, combo → `AdwComboRow`, bool → `AdwSwitchRow`, text → `AdwEntryRow`, file/directory → `GtkFileDialog`), с учётом `condition` (простое вычисление `key.value == X`), локализация через `Project::localize`.
4. Дальше: плейлисты, импорт `.pkg`/распаковка, автозапуск, энергосбережение (пауза на батарее), web-свойства `directory` (`wallpaperRequestRandomFileForProperty`).
