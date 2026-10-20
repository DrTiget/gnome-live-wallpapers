// Unit test for crates/daemon/src/shim.js in plain Node (fake window/document).
// usage: node scripts/test-shim.mjs
import fs from "node:fs";
import assert from "node:assert/strict";
import vm from "node:vm";

const src = fs.readFileSync(new URL("../crates/daemon/src/shim.js", import.meta.url), "utf8");
let rafQueue = [];
const events = [];
const media = [{ paused: false, dataset: {}, pause() { this.paused = true; }, play() { this.paused = false; return Promise.resolve(); } }];
const target = { dispatchEvent: (e) => events.push(e.type) };
const window = {
  requestAnimationFrame: (cb) => { rafQueue.push(cb); return rafQueue.length; },
  cancelAnimationFrame: () => {},
};
class MouseEvent { constructor(type, init) { this.type = type; Object.assign(this, init); } }
const ctx = vm.createContext({
  window, MouseEvent, PointerEvent: MouseEvent,
  document: { elementFromPoint: () => target, body: target, querySelectorAll: () => media },
});
vm.runInContext(src, ctx);
const wpe = window.__wpe;

// properties delivered before the listener is registered are replayed
wpe.applyUserProperties({ a: { type: "bool", value: true } });
let got = null;
window.wallpaperPropertyListener = { applyUserProperties: (p) => { got = p; } };
assert.equal(JSON.stringify(got), JSON.stringify({ a: { type: "bool", value: true } }));
wpe.applyUserProperties({ b: { type: "slider", value: 2 } });
assert.equal(JSON.stringify(got), JSON.stringify({ b: { type: "slider", value: 2 } }));

// audio listener
let audio = null;
window.wallpaperRegisterAudioListener((a) => { audio = a; });
wpe.audio(new Array(128).fill(0.5));
assert.equal(audio.length, 128);

// pointer events
wpe.pointer("motion", 10, 20, 0);
wpe.pointer("press", 10, 20, 1);
wpe.pointer("release", 10, 20, 1);
assert.equal(JSON.stringify(events), JSON.stringify(["mousemove", "pointermove", "pointerdown", "mousedown", "pointerup", "mouseup", "click"]));

// pause defers rAF and pauses media; resume flushes and resumes
let pausedSeen = null;
window.wallpaperRegisterPausedListener((e) => { pausedSeen = e.paused; });
wpe.setPaused(true);
assert.equal(pausedSeen, true);
assert.equal(media[0].paused, true);
let ran = 0;
window.requestAnimationFrame(() => ran++);
assert.equal(rafQueue.length, 0, "rAF must be deferred while paused");
wpe.setPaused(false);
assert.equal(media[0].paused, false);
assert.equal(rafQueue.length, 1);
rafQueue[0]();
assert.equal(ran, 1);
console.log("shim.js: all tests passed");
