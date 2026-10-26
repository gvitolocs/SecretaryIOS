/**
 * The outfit-change effect: a light column rises around Lucina, a holographic ring scans her from
 * the floor up, sparkles gather, a soft flash peaks (the model swaps under it), then the sparkles
 * burst out and drift down. Drawn on a 2D canvas laid over the stage (CSS pixels, pointer-events
 * none), so it works the same whatever the WebGL side is doing. Our own drawing: nothing here
 * comes from any other app.
 *
 * Reduced motion: no particles and no flash, only a short crossfade (`onPeak` runs at once).
 */

export interface SubjectRect {
  /** CSS pixels, relative to the overlay's box. */
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface OutfitEffectOptions {
  /** The box the overlay covers (normally the stage canvas). */
  host: HTMLElement;
  /** The overlay is inserted right after this element (default `host`), under later siblings. */
  after?: Element;
  /** Where she is inside `host` (CSS pixels); defaults to a centred column. */
  subject?: SubjectRect;
  theme?: 'dark' | 'light';
  reducedMotion?: boolean;
  /** Called once at the flash peak: swap the model here. */
  onPeak?: () => void;
  /** Total length in ms (default 1500). The peak is at 36 %. */
  duration?: number;
  /** For tests. */
  random?: () => number;
}

export interface OutfitEffect {
  /** Resolves when the effect has finished and its canvas is gone. */
  readonly done: Promise<void>;
  /** Ends it early (still calls `onPeak` if it has not run yet). */
  cancel(): void;
}

export const EFFECT_DURATION_MS = 1500;
export const EFFECT_PEAK = 0.36;

const PALETTE = {
  dark: { core: [255, 246, 220], a: [120, 232, 255], b: [196, 140, 255], c: [255, 120, 214], gold: [255, 226, 140] },
  light: { core: [255, 255, 255], a: [70, 170, 255], b: [150, 100, 255], c: [255, 90, 190], gold: [255, 190, 70] },
} as const;

type Rgb = readonly [number, number, number];
const rgba = (c: Rgb, a: number): string => `rgba(${c[0]},${c[1]},${c[2]},${Math.max(0, Math.min(1, a)).toFixed(3)})`;
const easeOut = (t: number): number => 1 - Math.pow(1 - t, 3);
const easeInOut = (t: number): number => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const clamp01 = (t: number): number => (t < 0 ? 0 : t > 1 ? 1 : t);

/** Phase values at progress `p` (0..1): exported so the timing can be tested without a canvas. */
export function effectPhases(p: number, peak = EFFECT_PEAK) {
  const rise = clamp01(p / peak);
  const after = clamp01((p - peak) / (1 - peak));
  return {
    /** Light column height, 0..1 of the subject, up to the peak. */
    column: easeOut(rise),
    /** Column and glow opacity: in, then out after the peak. */
    glow: p < peak ? Math.sin((rise * Math.PI) / 2) : 1 - easeInOut(after),
    /** Scanning ring position, 0 = feet, 1 = above the head. */
    ring: easeInOut(clamp01(p / 0.8)),
    /** Flash strength around the peak. */
    flash: Math.exp(-Math.pow((p - peak) / 0.07, 2)),
    /** Sparkles: gathering before the peak (1 → 0 = distance), bursting after (0 → 1). */
    gather: 1 - easeOut(rise),
    burst: easeOut(after),
    /** Overall fade at the end. */
    fade: 1 - clamp01((p - 0.82) / 0.18),
  };
}

interface Spark {
  angle: number;
  radius: number;
  height: number;
  size: number;
  hue: 0 | 1 | 2 | 3;
  spin: number;
  drift: number;
  star: boolean;
}

function now(): number {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
}

export function playOutfitEffect(opts: OutfitEffectOptions): OutfitEffect {
  let peaked = false;
  const peak = (): void => {
    if (peaked) return;
    peaked = true;
    try {
      opts.onPeak?.();
    } catch (e) {
      console.warn('[lucina-stage] outfit swap failed:', e);
    }
  };
  const host = opts.host;
  const parent = host.parentElement;
  const doc = host.ownerDocument;
  const canDraw = !opts.reducedMotion && !!parent && !doc.hidden && typeof requestAnimationFrame === 'function';
  if (!canDraw) {
    peak();
    return { done: Promise.resolve(), cancel: () => {} };
  }

  const duration = opts.duration ?? EFFECT_DURATION_MS;
  const random = opts.random ?? Math.random;
  const pal = PALETTE[opts.theme === 'light' ? 'light' : 'dark'];
  const hues: Rgb[] = [pal.a, pal.b, pal.c, pal.gold];

  const canvas = doc.createElement('canvas');
  canvas.setAttribute('aria-hidden', 'true');
  canvas.dataset.lucinaEffect = 'outfit';
  const w = Math.max(1, host.clientWidth || host.offsetWidth || 1);
  const h = Math.max(1, host.clientHeight || host.offsetHeight || 1);
  const dpr = Math.min(globalThis.devicePixelRatio || 1, 2);
  canvas.width = Math.round(w * dpr);
  canvas.height = Math.round(h * dpr);
  canvas.style.cssText = `position:absolute;left:${host.offsetLeft}px;top:${host.offsetTop}px;width:${w}px;height:${h}px;pointer-events:none;`;
  const anchor = opts.after && opts.after.parentElement === parent ? opts.after : host;
  parent!.insertBefore(canvas, anchor.nextSibling);
  const ctx = canvas.getContext('2d');
  if (!ctx) {
    canvas.remove();
    peak();
    return { done: Promise.resolve(), cancel: () => {} };
  }
  ctx.scale(dpr, dpr);

  const s = opts.subject ?? { x: w * 0.3, y: h * 0.06, width: w * 0.4, height: h * 0.92 };
  const cx = s.x + s.width / 2;
  const floor = s.y + s.height;
  const halfW = Math.max(s.width / 2, s.height * 0.16);
  const sparks: Spark[] = Array.from({ length: Math.round(Math.min(90, 40 + s.height / 12)) }, () => ({
    angle: random() * Math.PI * 2,
    radius: 0.35 + random() * 0.85,
    height: random(),
    size: 1.6 + random() * 3.2,
    hue: Math.floor(random() * 4) as Spark['hue'],
    spin: (random() - 0.5) * 3,
    drift: 0.4 + random() * 0.8,
    star: random() < 0.28,
  }));

  let raf = 0;
  let resolveDone: () => void = () => {};
  const done = new Promise<void>((r) => (resolveDone = r));
  const start = now();
  let finished = false;

  function finish(): void {
    if (finished) return;
    finished = true;
    if (raf) cancelAnimationFrame(raf);
    peak();
    canvas.remove();
    resolveDone();
  }

  function star(x: number, y: number, r: number, color: string): void {
    ctx!.fillStyle = color;
    ctx!.beginPath();
    ctx!.moveTo(x, y - r * 2.4);
    ctx!.quadraticCurveTo(x, y, x + r * 2.4, y);
    ctx!.quadraticCurveTo(x, y, x, y + r * 2.4);
    ctx!.quadraticCurveTo(x, y, x - r * 2.4, y);
    ctx!.quadraticCurveTo(x, y, x, y - r * 2.4);
    ctx!.fill();
  }

  function draw(): void {
    raf = 0;
    const p = clamp01((now() - start) / duration);
    const ph = effectPhases(p);
    if (p >= EFFECT_PEAK) peak();
    const g = ctx!;
    g.clearRect(0, 0, w, h);
    g.globalCompositeOperation = opts.theme === 'light' ? 'source-over' : 'lighter';

    // Light column from the floor up: a soft elongated glow with a brighter beam inside.
    const top = floor - s.height * 1.08 * ph.column;
    const colAlpha = 0.6 * ph.glow * ph.fade;
    const colH = floor - top;
    if (colAlpha > 0.005 && colH > 2) {
      const glow = (radius: number, alpha: number, stops: [number, Rgb, number][]): void => {
        g.save();
        g.translate(cx, floor - colH / 2);
        g.scale(1, colH / 2 / radius);
        const rg = g.createRadialGradient(0, 0, 0, 0, 0, radius);
        for (const [at, c, k] of stops) rg.addColorStop(at, rgba(c, alpha * k));
        rg.addColorStop(1, rgba(pal.b, 0));
        g.fillStyle = rg;
        g.beginPath();
        g.arc(0, 0, radius, 0, Math.PI * 2);
        g.fill();
        g.restore();
      };
      glow(halfW * 1.35, colAlpha, [
        [0, pal.core, 0.55],
        [0.35, pal.a, 0.45],
        [0.7, pal.b, 0.2],
      ]);
      glow(halfW * 0.42, colAlpha, [
        [0, pal.core, 0.9],
        [0.6, pal.c, 0.35],
      ]);
      // The bright tip where the column is rising.
      const tipR = halfW * 0.85;
      g.save();
      g.translate(cx, top);
      g.scale(1, 0.5);
      const tip = g.createRadialGradient(0, 0, 0, 0, 0, tipR);
      tip.addColorStop(0, rgba(pal.core, colAlpha * (1 - ph.flash * 0.5)));
      tip.addColorStop(0.4, rgba(pal.a, colAlpha * 0.4));
      tip.addColorStop(1, rgba(pal.a, 0));
      g.fillStyle = tip;
      g.beginPath();
      g.arc(0, 0, tipR, 0, Math.PI * 2);
      g.fill();
      g.restore();
    }

    // After the flash: two shock rings spread out from her chest.
    if (ph.burst > 0 && ph.burst < 1) {
      const cy = s.y + s.height * 0.42;
      for (const [k, c] of [
        [1, pal.a],
        [0.7, pal.c],
      ] as const) {
        const r = halfW * (0.5 + 1.7 * ph.burst * k);
        g.strokeStyle = rgba(c, 0.7 * (1 - ph.burst) * ph.fade);
        g.lineWidth = 2.5 * (1 - ph.burst) + 0.5;
        g.beginPath();
        g.ellipse(cx, cy, r, r * 0.42, 0, 0, Math.PI * 2);
        g.stroke();
      }
    }

    // Scanning ring (an ellipse seen from slightly above), feet → above the head.
    const ringY = floor - s.height * 1.05 * ph.ring;
    const ringA = Math.sin(Math.PI * clamp01(p / 0.8)) * ph.fade;
    if (ringA > 0.01) {
      for (const [k, c, lw] of [
        [1.15, pal.a, 2.2],
        [0.95, pal.c, 1.2],
      ] as const) {
        g.strokeStyle = rgba(c, 0.85 * ringA);
        g.lineWidth = lw;
        g.shadowColor = rgba(c, ringA);
        g.shadowBlur = 14;
        g.beginPath();
        g.ellipse(cx, ringY, halfW * k, halfW * k * 0.22, 0, 0, Math.PI * 2);
        g.stroke();
      }
      g.shadowBlur = 0;
    }

    // Sparkles: spiral in, then burst out and fall.
    for (const sp of sparks) {
      const a = sp.angle + sp.spin * p * 2.2;
      let r: number;
      let y: number;
      let alpha: number;
      if (p < EFFECT_PEAK) {
        r = halfW * (0.25 + sp.radius * 1.6 * ph.gather);
        y = floor - s.height * (0.08 + 0.85 * sp.height);
        alpha = (1 - ph.gather) * 0.9;
      } else {
        r = halfW * (0.25 + sp.radius * 1.9 * ph.burst);
        y = floor - s.height * (0.08 + 0.85 * sp.height) + s.height * 0.22 * sp.drift * ph.burst * ph.burst;
        alpha = (1 - ph.burst * 0.6) * ph.fade;
      }
      alpha *= 0.65 + 0.35 * Math.sin(p * 38 + sp.angle * 7);
      const x = cx + Math.cos(a) * r;
      const depth = 0.65 + 0.35 * Math.sin(a);
      const size = sp.size * depth * (p < EFFECT_PEAK ? 1 : 1 + 0.9 * (1 - ph.burst));
      if (alpha * depth < 0.02) continue;
      const color = rgba(hues[sp.hue]!, alpha * depth);
      if (sp.star) {
        g.shadowColor = color;
        g.shadowBlur = 8;
        star(x, y, size, color);
        g.shadowBlur = 0;
      }
      else {
        g.fillStyle = color;
        g.beginPath();
        g.arc(x, y, size, 0, Math.PI * 2);
        g.fill();
      }
    }

    // The flash: a soft bloom over her body at the peak.
    const flashA = ph.flash * (opts.theme === 'light' ? 0.75 : 0.9);
    if (flashA > 0.01) {
      const cy = s.y + s.height * 0.45;
      const rad = Math.max(s.width, s.height * 0.55);
      const fg = g.createRadialGradient(cx, cy, 0, cx, cy, rad);
      fg.addColorStop(0, rgba(pal.core, flashA));
      fg.addColorStop(0.35, rgba(pal.gold, flashA * 0.55));
      fg.addColorStop(0.7, rgba(pal.b, flashA * 0.2));
      fg.addColorStop(1, rgba(pal.b, 0));
      g.fillStyle = fg;
      g.fillRect(0, 0, w, h);
    }
    g.globalCompositeOperation = 'source-over';

    if (p >= 1) finish();
    else raf = requestAnimationFrame(draw);
  }
  raf = requestAnimationFrame(draw);
  // Hidden tabs do not run requestAnimationFrame: never hold the swap hostage to it.
  const guard = setTimeout(finish, duration + 1500);
  void done.then(() => clearTimeout(guard));

  return { done, cancel: finish };
}
