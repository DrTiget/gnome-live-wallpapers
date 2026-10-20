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
let clock = 0;
const timers = [];
const window = {
  __wpeFps: 30,
  performance: { now: () => clock },
  requestAnimationFrame: (cb) => { rafQueue.push(cb); return rafQueue.length; },
  cancelAnimationFrame: () => {},
};
const setTimeout = (fn, ms) => { timers.push([clock + (ms || 0), fn]); return timers.length; };
// Advance the fake clock, firing due timers.
function advance(ms) {
  clock += ms;
  for (const t of timers.splice(0).filter((t) => { if (t[0] <= clock) return true; timers.push(t); return false; })) t[1]();
}
const frame = () => { const q = rafQueue.splice(0); q.forEach((cb) => cb(clock)); return q.length; };
class MouseEvent { constructor(type, init) { this.type = type; Object.assign(this, init); } }
const ctx = vm.createContext({
  window, MouseEvent, PointerEvent: MouseEvent, setTimeout,
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
frame();
assert.equal(ran, 1);

// rAF is limited to __wpeFps (30): a loop driven by a 144 Hz display runs ~30 times a second
let calls = 0;
const loop = () => { calls++; window.requestAnimationFrame(loop); };
window.requestAnimationFrame(loop);
for (let t = 0; t < 1000; t += 1000 / 144) { advance(1000 / 144); frame(); }
assert.ok(calls >= 27 && calls <= 31, `30 fps cap, got ${calls}`);
// several callbacks share one native frame; cancel works
let a = 0, b = 0;
advance(100);
window.requestAnimationFrame(() => a++);
const idb = window.requestAnimationFrame(() => b++);
window.cancelAnimationFrame(idb);
advance(100); frame();
assert.equal(a, 1); assert.equal(b, 0);
wpe.setFps(60);
calls = 0;
for (let t = 0; t < 1000; t += 1000 / 144) { advance(1000 / 144); frame(); }
assert.ok(calls >= 55 && calls <= 61, `60 fps cap, got ${calls}`);
console.log("shim.js: all tests passed");
