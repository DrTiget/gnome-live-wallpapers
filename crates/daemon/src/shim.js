// Wallpaper Engine web API shim, injected at document start into every web wallpaper.
// The daemon calls window.__wpe.* via WebView.evaluate_javascript().
(function () {
  "use strict";
  const audioListeners = [];
  const pausedListeners = [];
  let pending = null; // properties received before the page registered its listener

  // requestAnimationFrame runs at most `fps` times a second (the daemon's
  // frame rate limit; a background must not render at 144 Hz) and not at all
  // while paused (covered wallpaper; WE stops rendering then as well).
  // Callbacks are queued and run together in one native frame.
  let paused = false;
  let interval = 1000 / (window.__wpeFps || 30);
  const nativeRAF = window.requestAnimationFrame.bind(window);
  const queued = new Map(); // id -> callback
  let nextId = 1;
  let timer = null;
  let nextFrame = 0; // performance.now() of the earliest next frame
  const now = () => (window.performance ? window.performance.now() : Date.now());
  function runFrame(ts) {
    timer = null;
    if (paused) return;
    // Keep a steady cadence (vsync rounding must not lower the rate); after
    // a pause or an idle page start over from now.
    const t = now();
    nextFrame = t - nextFrame > interval ? t + interval : nextFrame + interval;
    const cbs = [...queued.values()];
    queued.clear();
    for (const cb of cbs) {
      try { cb(ts); } catch (e) { setTimeout(() => { throw e; }); }
    }
  }
  function schedule() {
    if (timer !== null || paused || queued.size === 0) return;
    const wait = nextFrame - now();
    timer = wait > 0 ? setTimeout(() => { timer = nativeRAF(runFrame); }, wait) : nativeRAF(runFrame);
  }
  window.requestAnimationFrame = function (cb) {
    const id = nextId++;
    queued.set(id, cb);
    schedule();
    return id;
  };
  window.cancelAnimationFrame = function (id) { queued.delete(id); };
  function setMediaPaused(p) {
    for (const m of document.querySelectorAll("video, audio")) {
      if (p && !m.paused) { m.dataset.wpePaused = "1"; m.pause(); }
      else if (!p && m.dataset.wpePaused) { delete m.dataset.wpePaused; m.play().catch(() => {}); }
    }
  }

  window.wallpaperRegisterAudioListener = function (cb) {
    audioListeners.push(cb);
    // Tell the daemon to start capturing (it only does so for pages that listen).
    try { window.webkit.messageHandlers.wpe.postMessage("audio"); } catch (_) { /* not in WebKitGTK */ }
  };
  window.wallpaperRegisterPausedListener = function (cb) { pausedListeners.push(cb); };
  window.wallpaperRequestRandomFileForProperty = function (_key, cb) { /* stage 3 */ };

  let listener = window.wallpaperPropertyListener;
  Object.defineProperty(window, "wallpaperPropertyListener", {
    configurable: true,
    get() { return listener; },
    set(v) {
      listener = v;
      if (pending && v && v.applyUserProperties) { v.applyUserProperties(pending); }
    },
  });

  window.__wpe = {
    applyUserProperties(props) {
      pending = Object.assign(pending || {}, props);
      if (listener && listener.applyUserProperties) listener.applyUserProperties(props);
    },
    applyGeneralProperties(props) {
      if (listener && listener.applyGeneralProperties) listener.applyGeneralProperties(props);
    },
    setPaused(p) {
      if (p === paused) return;
      paused = p;
      setMediaPaused(p);
      if (!p) schedule();
      if (listener && listener.setPaused) listener.setPaused(p);
      pausedListeners.forEach((cb) => cb({ paused: p }));
    },
    setFps(fps) { interval = 1000 / Math.max(1, fps); },
    // 128 floats: 64 left + 64 right channel, 0..~1.
    audio(samples) { for (const cb of audioListeners) cb(samples); },
    // Synthesised pointer events from the GNOME extension (desktop is below all windows).
    pointer(kind, x, y, button) {
      const target = document.elementFromPoint(x, y) || document.body;
      const init = { clientX: x, clientY: y, screenX: x, screenY: y, button: Math.max(0, button - 1),
                     buttons: kind === "release" ? 0 : (button ? 1 << (button - 1) : 0), bubbles: true, cancelable: true, view: window };
      if (kind === "motion") { target.dispatchEvent(new MouseEvent("mousemove", init)); target.dispatchEvent(new PointerEvent("pointermove", init)); }
      else if (kind === "press") { target.dispatchEvent(new PointerEvent("pointerdown", init)); target.dispatchEvent(new MouseEvent("mousedown", init)); }
      else if (kind === "release") {
        target.dispatchEvent(new PointerEvent("pointerup", init));
        target.dispatchEvent(new MouseEvent("mouseup", init));
        if (button === 1) target.dispatchEvent(new MouseEvent("click", init));
      }
    },
  };
})();
