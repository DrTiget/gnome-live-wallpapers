// Wallpaper Engine web API shim, injected at document start into every web wallpaper.
// The daemon calls window.__wpe.* via WebView.evaluate_javascript().
(function () {
  "use strict";
  const audioListeners = [];
  const pausedListeners = [];
  let pending = null; // properties received before the page registered its listener

  // Real pause: defer requestAnimationFrame callbacks and pause media while
  // the wallpaper is covered (WE stops rendering in that case as well).
  let paused = false;
  const nativeRAF = window.requestAnimationFrame.bind(window);
  const nativeCAF = window.cancelAnimationFrame.bind(window);
  const deferred = new Map();
  let fakeId = 1 << 30;
  window.requestAnimationFrame = function (cb) {
    if (!paused) return nativeRAF(cb);
    const id = ++fakeId;
    deferred.set(id, cb);
    return id;
  };
  window.cancelAnimationFrame = function (id) {
    if (!deferred.delete(id)) nativeCAF(id);
  };
  function setMediaPaused(p) {
    for (const m of document.querySelectorAll("video, audio")) {
      if (p && !m.paused) { m.dataset.wpePaused = "1"; m.pause(); }
      else if (!p && m.dataset.wpePaused) { delete m.dataset.wpePaused; m.play().catch(() => {}); }
    }
  }

  window.wallpaperRegisterAudioListener = function (cb) { audioListeners.push(cb); };
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
      if (!p) {
        const cbs = [...deferred.values()];
        deferred.clear();
        cbs.forEach((cb) => nativeRAF(cb));
      }
      if (listener && listener.setPaused) listener.setPaused(p);
      pausedListeners.forEach((cb) => cb({ paused: p }));
    },
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
