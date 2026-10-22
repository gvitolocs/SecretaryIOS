/**
 * Dance music, only ever started by an explicit tap (the host calls `stage.setDance(true,
 * { music })` from a click). Either a track the owner provides (decoded and looped), or a short
 * beat synthesised here at 120 BPM (kick, hat, a bass line, a pad): our own, royalty-free.
 * Plays through the stage's audio graph, so the mute switch silences it.
 */

export interface MusicHandle {
  stop(): void;
}

/** Loops a decoded track into `dest`. */
export function playTrack(ctx: AudioContext, dest: AudioNode, buffer: AudioBuffer): MusicHandle {
  const src = ctx.createBufferSource();
  src.buffer = buffer;
  src.loop = true;
  const g = ctx.createGain();
  g.gain.value = 0.8;
  src.connect(g).connect(dest);
  src.start();
  return {
    stop() {
      g.gain.setTargetAtTime(0, ctx.currentTime, 0.08);
      try {
        src.stop(ctx.currentTime + 0.4);
      } catch {
        /* already stopped */
      }
    },
  };
}

const BPM = 120;
const BASS = [41, 41, 44, 39, 41, 41, 46, 44]; // MIDI notes, one per beat (F minor-ish)

function hz(midi: number): number {
  return 440 * Math.pow(2, (midi - 69) / 12);
}

/** A synthesised 120 BPM loop, scheduled a little ahead with a timer. */
export function playBeat(ctx: AudioContext, dest: AudioNode): MusicHandle {
  const out = ctx.createGain();
  out.gain.value = 0.55;
  out.connect(dest);
  const beat = 60 / BPM;
  let next = ctx.currentTime + 0.05;
  let n = 0;
  let noise: AudioBuffer | null = null;
  try {
    noise = ctx.createBuffer(1, Math.floor(ctx.sampleRate * 0.05), ctx.sampleRate);
    const d = noise.getChannelData(0);
    for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1;
  } catch {
    noise = null;
  }

  const kick = (t: number): void => {
    const o = ctx.createOscillator();
    const g = ctx.createGain();
    o.frequency.setValueAtTime(140, t);
    o.frequency.exponentialRampToValueAtTime(45, t + 0.12);
    g.gain.setValueAtTime(0.9, t);
    g.gain.exponentialRampToValueAtTime(0.001, t + 0.25);
    o.connect(g).connect(out);
    o.start(t);
    o.stop(t + 0.3);
  };
  const hat = (t: number): void => {
    if (!noise) return;
    const s = ctx.createBufferSource();
    s.buffer = noise;
    const f = ctx.createBiquadFilter();
    f.type = 'highpass';
    f.frequency.value = 7000;
    const g = ctx.createGain();
    g.gain.setValueAtTime(0.18, t);
    g.gain.exponentialRampToValueAtTime(0.001, t + 0.05);
    s.connect(f).connect(g).connect(out);
    s.start(t);
  };
  const bass = (t: number, midi: number): void => {
    const o = ctx.createOscillator();
    o.type = 'sawtooth';
    o.frequency.value = hz(midi);
    const f = ctx.createBiquadFilter();
    f.type = 'lowpass';
    f.frequency.setValueAtTime(900, t);
    f.frequency.exponentialRampToValueAtTime(200, t + beat * 0.45);
    const g = ctx.createGain();
    g.gain.setValueAtTime(0.0001, t);
    g.gain.exponentialRampToValueAtTime(0.25, t + 0.01);
    g.gain.exponentialRampToValueAtTime(0.001, t + beat * 0.48);
    o.connect(f).connect(g).connect(out);
    o.start(t);
    o.stop(t + beat * 0.5);
  };
  const pad = (t: number, midi: number): void => {
    for (const k of [0, 3, 7]) {
      const o = ctx.createOscillator();
      o.type = 'triangle';
      o.frequency.value = hz(midi + 24 + k);
      const g = ctx.createGain();
      g.gain.setValueAtTime(0.0001, t);
      g.gain.linearRampToValueAtTime(0.035, t + 0.3);
      g.gain.linearRampToValueAtTime(0.0001, t + beat * 4);
      o.connect(g).connect(out);
      o.start(t);
      o.stop(t + beat * 4 + 0.05);
    }
  };

  const schedule = (): void => {
    while (next < ctx.currentTime + 0.3) {
      const step = n % 16; // eighth notes
      const t = next;
      if (step % 2 === 0) kick(t);
      hat(t + beat / 4);
      if (step % 2 === 1) bass(t, BASS[(n >> 1) % BASS.length]!);
      if (step % 8 === 0) pad(t, BASS[(n >> 1) % BASS.length]!);
      next += beat / 2;
      n++;
    }
  };
  schedule();
  const timer = setInterval(schedule, 100);
  return {
    stop() {
      clearInterval(timer);
      out.gain.setTargetAtTime(0, ctx.currentTime, 0.08);
      setTimeout(() => out.disconnect(), 600);
    },
  };
}
