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

`0004-image-size-from-model.patch`: an image's quad size comes from the model's
`width`/`height`, the first texture's size is only a fallback. The first texture
can be a mask: `deep_space` Background (flowimage, 480x270 flowmask over
1920x1080 layers) was drawn as a small rotated rectangle in the middle.

`0005-hide-non-solid-solid-layers.patch`: a solid layer with `"solid": false` is an
invisible container and is not drawn. With size 0x0 it used to be stretched over
the scene and filled white (music player "Player Options" in workshop 3481803447).

`0006-text-utf8-font-fallback.patch`: text is decoded as UTF-8 (was byte by byte,
so `「 TUESDAY 」` came out as `ã�� …`); characters missing in the wallpaper's font
are taken from a fontconfig match (needs `libfontconfig-dev` to build).

`0007-text-script-import-init.patch`: text layer scripts with `import * as WEMath
from 'WEMath'` no longer fail with a syntax error (the placeholder text, e.g.
"trackname", stayed on screen); `init(value)` result replaces the text as in WE.
