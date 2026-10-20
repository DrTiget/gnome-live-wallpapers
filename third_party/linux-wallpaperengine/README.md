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
