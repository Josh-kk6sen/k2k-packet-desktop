// K2K Packet Desktop — notification sounds.
//
// Synthesized with WebAudio: no audio assets to ship, no licensing, and every
// preset is previewable from Settings → Sounds. The context is created lazily
// and resumed on the first user gesture — autoplay policy leaves a freshly
// created AudioContext suspended, and a suspended context plays silence.

let ctx = null;

/** Create (or reuse) the shared AudioContext. */
function audio() {
  if (!ctx) {
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return null;
    ctx = new AC();
  }
  return ctx;
}

/** Unlock the context on the first gesture (click/keypress anywhere). */
export function initSound() {
  const unlock = () => {
    const c = audio();
    if (c && c.state === "suspended") c.resume().catch(() => {});
  };
  window.addEventListener("pointerdown", unlock, { passive: true });
  window.addEventListener("keydown", unlock, { passive: true });
}

// ── Presets ──────────────────────────────────────────────
// Each preset is a list of {freq, start, dur, type, gain} tone segments.

const PRESETS = {
  none: [],
  ding: [
    { freq: 880, start: 0, dur: 0.75, type: "sine", gain: 1 },
    { freq: 1760, start: 0, dur: 0.35, type: "sine", gain: 0.25 },
  ],
  double: [
    { freq: 880, start: 0, dur: 0.55, type: "sine", gain: 1 },
    { freq: 1760, start: 0, dur: 0.25, type: "sine", gain: 0.22 },
    { freq: 880, start: 0.18, dur: 0.55, type: "sine", gain: 0.9 },
    { freq: 1760, start: 0.18, dur: 0.25, type: "sine", gain: 0.2 },
  ],
  chime: [
    { freq: 659.25, start: 0, dur: 0.5, type: "sine", gain: 0.9 },
    { freq: 880, start: 0.14, dur: 0.7, type: "sine", gain: 0.85 },
  ],
  beep: [{ freq: 1000, start: 0, dur: 0.14, type: "square", gain: 0.5 }],
  chirp: [{ freq: 500, sweepTo: 1250, start: 0, dur: 0.2, type: "sine", gain: 0.8 }],
};

export const SOUND_NAMES = Object.keys(PRESETS);

/**
 * Play a named preset. `volume` is 0–100. Unknown names are silent — an old
 * settings.json must never throw inside an event handler.
 */
export function playSound(name, volume = 70) {
  const preset = PRESETS[name];
  if (!preset || !preset.length) return;
  const c = audio();
  if (!c) return;
  if (c.state === "suspended") c.resume().catch(() => {});

  const vol = Math.max(0, Math.min(100, Number(volume) || 0)) / 100;
  if (vol === 0) return;

  const t0 = c.currentTime + 0.01;
  for (const tone of preset) {
    const osc = c.createOscillator();
    const amp = c.createGain();
    osc.type = tone.type || "sine";
    const at = t0 + tone.start;
    osc.frequency.setValueAtTime(tone.freq, at);
    if (tone.sweepTo) {
      osc.frequency.exponentialRampToValueAtTime(tone.sweepTo, at + tone.dur);
    }
    // Short attack, exponential tail — a square-edged gate clicks.
    amp.gain.setValueAtTime(0.0001, at);
    amp.gain.exponentialRampToValueAtTime(Math.max(0.0002, tone.gain * vol * 0.35), at + 0.012);
    amp.gain.exponentialRampToValueAtTime(0.0001, at + tone.dur);
    osc.connect(amp).connect(c.destination);
    osc.start(at);
    osc.stop(at + tone.dur + 0.02);
  }
}
