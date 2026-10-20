# linux-wallpaperengine (scene renderer)

Upstream: https://github.com/Almamu/linux-wallpaperengine (GPL-3.0).
Pinned commit: `b016d7d` (master, 2026-06-09) — `UPSTREAM_COMMIT` in
`scripts/build-scene-renderer.sh`.

`0001-control-stdin.patch` (candidate for upstream):

* `--control-stdin` — the host forwards input and pause state, one command per line:
  `m <x> <y>` pointer in window coordinates (top-left origin), `b <l|r> <0|1>` button,
  `p <0|1>` pause/resume rendering (the render loop sleeps, like its own
  fullscreen pause), `q` quit; EOF on stdin quits too, so a crashed daemon never
  leaves orphaned renderers. Needed because the window is cloned into the GNOME
  desktop background and never receives real pointer events.
* `find_package(GLUT)` is no longer `REQUIRED`: GLUT is linked but never used.

`0002-audio-capture-and-project-type.patch`:

* audio processing (pulse API, monitor of the default sink; works with `--silent`,
  which only mutes the wallpaper's own sounds): upstream re-created the record
  stream on every sink/source event without disconnecting the old one, so several
  streams kept recording; now the stream is re-taken only when the default sink
  changes. While rendering is paused the stream is corked (`PlaybackRecorder::setPaused`).
* projects without `"type"` (official `audiophile`, `techno`) no longer fail with
  "Project type missing": the type is inferred from the main file. Note these two
  are 3D perspective scenes (no `orthogonalprojection`), which linux-wallpaperengine
  does not render yet, so they still fail one step later.

`0003-exit-on-fatal-error.patch`: on a fatal error (bad `scene.json`, unsupported
scene) print it and `_Exit(1)` instead of returning from `main`, whose static
destructors (CEF, audio threads) could hang and leave a process without a window.
wpe-daemon reads its output and marks such wallpapers `broken` with that line as reason.
