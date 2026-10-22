/**
 * Original stamps (simple SVG, drawn for Lucina; nothing from Custom Cast) and photo frames
 * (drawn on a canvas). Stamps are used by the photo decorator (P4), the live stamps (D7) and the
 * flick ring (D6).
 */

const svg = (body: string): string => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">${body}</svg>`;
const O = 'stroke="#2a2233" stroke-width="4" stroke-linejoin="round" stroke-linecap="round"';
const O3 = 'stroke="#2a2233" stroke-width="3" stroke-linejoin="round" stroke-linecap="round"';

export const STAMPS: Record<string, string> = {
  heart: svg(`<path ${O} fill="#ff6f9f" d="M50 86 C20 64 8 48 12 32 C16 16 38 12 50 30 C62 12 84 16 88 32 C92 48 80 64 50 86 Z"/><path fill="#fff" opacity=".7" d="M26 30 c4-6 12-7 15-3 c-6 1-11 4-13 9z"/>`),
  star: svg(`<path ${O} fill="#ffd34d" d="M50 8 L61 37 L92 38 L67 57 L76 88 L50 70 L24 88 L33 57 L8 38 L39 37 Z"/><circle cx="42" cy="50" r="3.5" fill="#2a2233"/><circle cx="58" cy="50" r="3.5" fill="#2a2233"/><path d="M44 59 q6 5 12 0" ${O3} fill="none"/>`),
  sparkle: svg(`<path ${O} fill="#9fe3ff" d="M50 6 C54 36 64 46 94 50 C64 54 54 64 50 94 C46 64 36 54 6 50 C36 46 46 36 50 6 Z"/><path ${O3} fill="#fff6b0" d="M82 10 C83 18 86 21 94 22 C86 23 83 26 82 34 C81 26 78 23 70 22 C78 21 81 18 82 10 Z"/>`),
  note: svg(`<path ${O} fill="#b48cff" d="M36 74 a12 10 -20 1 1 -2 -2 V22 L80 12 V62 a12 10 -20 1 1 -2 -2 V30 L40 38 V74 Z"/>`),
  flower: svg(`<g ${O} fill="#ffb3d1"><circle cx="50" cy="26" r="16"/><circle cx="74" cy="44" r="16"/><circle cx="65" cy="72" r="16"/><circle cx="35" cy="72" r="16"/><circle cx="26" cy="44" r="16"/></g><circle ${O} cx="50" cy="50" r="13" fill="#ffd34d"/>`),
  crown: svg(`<path ${O} fill="#ffd34d" d="M14 74 L10 30 L32 50 L50 20 L68 50 L90 30 L86 74 Z"/><rect ${O} x="14" y="74" width="72" height="12" rx="4" fill="#ffc21a"/><circle cx="50" cy="56" r="6" fill="#ff6f9f"/><circle cx="30" cy="62" r="4" fill="#7fd6ff"/><circle cx="70" cy="62" r="4" fill="#7fd6ff"/>`),
  bubble: svg(`<path ${O} fill="#ffffff" d="M14 22 h72 a8 8 0 0 1 8 8 v34 a8 8 0 0 1 -8 8 H44 L26 88 L30 72 H14 a8 8 0 0 1 -8 -8 V30 a8 8 0 0 1 8 -8 Z"/><circle cx="32" cy="47" r="6" fill="#ff6f9f"/><circle cx="50" cy="47" r="6" fill="#ffd34d"/><circle cx="68" cy="47" r="6" fill="#7fd6ff"/>`),
  cat: svg(`<path ${O} fill="#fff4e6" d="M18 40 L22 10 L42 28 C47 27 53 27 58 28 L78 10 L82 40 C90 52 88 72 74 82 C60 92 40 92 26 82 C12 72 10 52 18 40 Z"/><circle cx="37" cy="54" r="5" fill="#2a2233"/><circle cx="63" cy="54" r="5" fill="#2a2233"/><path fill="#ff8fb0" d="M46 63 h8 l-4 5 z"/><path d="M50 68 q-6 6 -11 2 M50 68 q6 6 11 2" ${O3} fill="none"/><circle cx="28" cy="66" r="5" fill="#ffb3c9" opacity=".8"/><circle cx="72" cy="66" r="5" fill="#ffb3c9" opacity=".8"/>`),
  ribbon: svg(`<path ${O} fill="#ff5f8f" d="M50 46 L14 24 C8 36 8 56 14 68 Z"/><path ${O} fill="#ff5f8f" d="M50 46 L86 24 C92 36 92 56 86 68 Z"/><path ${O} fill="#e8456f" d="M44 50 L30 90 L40 86 L46 94 L52 54 Z M56 50 L70 90 L60 86 L54 94 L48 54 Z"/><circle ${O} cx="50" cy="46" r="9" fill="#ff8fb0"/>`),
  sun: svg(`<g ${O} fill="#ffb84d"><path d="M50 4 l6 14 h-12 z"/><path d="M50 96 l6 -14 h-12 z"/><path d="M4 50 l14 6 v-12 z"/><path d="M96 50 l-14 6 v-12 z"/><path d="M17 17 l14 6 l-8 8 z"/><path d="M83 83 l-14 -6 l8 -8 z"/><path d="M83 17 l-6 14 l-8 -8 z"/><path d="M17 83 l6 -14 l8 8 z"/></g><circle ${O} cx="50" cy="50" r="24" fill="#ffd34d"/><path d="M41 52 q9 8 18 0" ${O3} fill="none"/>`),
  moon: svg(`<path ${O} fill="#fff1a8" d="M64 10 C38 12 20 34 22 58 C24 80 46 94 68 90 C80 88 88 80 92 72 C70 80 46 66 44 44 C42 28 50 16 64 10 Z"/><circle cx="74" cy="30" r="3" fill="#fff"/><circle cx="84" cy="46" r="2" fill="#fff"/>`),
  rainbow: svg(`<g fill="none" stroke-linecap="round"><path d="M10 78 A40 40 0 0 1 90 78" stroke="#2a2233" stroke-width="30"/><path d="M10 78 A40 40 0 0 1 90 78" stroke="#ff6f9f" stroke-width="22"/><path d="M18 78 A32 32 0 0 1 82 78" stroke="#ffd34d" stroke-width="8"/><path d="M26 78 A24 24 0 0 1 74 78" stroke="#7fd6ff" stroke-width="8"/></g><g ${O} fill="#fff"><circle cx="16" cy="80" r="9"/><circle cx="84" cy="80" r="9"/></g>`),
  paw: svg(`<g ${O} fill="#ffb3c9"><ellipse cx="50" cy="66" rx="20" ry="17"/><ellipse cx="24" cy="44" rx="9" ry="11"/><ellipse cx="40" cy="28" rx="9" ry="11"/><ellipse cx="60" cy="28" rx="9" ry="11"/><ellipse cx="76" cy="44" rx="9" ry="11"/></g>`),
  wow: svg(`<path ${O} fill="#ffe14d" d="M50 4 L58 24 L80 12 L74 34 L96 38 L78 52 L94 70 L70 70 L72 94 L54 80 L40 96 L36 74 L12 82 L22 62 L4 50 L24 40 L14 20 L36 26 Z"/><text x="50" y="58" font-family="Arial Black, Arial, sans-serif" font-weight="900" font-size="22" text-anchor="middle" fill="#ff3d7f" stroke="#2a2233" stroke-width="1.5">WOW</text>`),
};

export const STAMP_IDS = Object.keys(STAMPS);

export function stampDataUrl(id: string): string {
  const s = STAMPS[id] ?? STAMPS.heart!;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(s)}`;
}

const cache = new Map<string, Promise<HTMLImageElement>>();
/** The stamp as a loaded image (cached). */
export function stampImage(id: string): Promise<HTMLImageElement> {
  let p = cache.get(id);
  if (!p) {
    p = new Promise((resolve, reject) => {
      const img = new Image();
      img.onload = () => resolve(img);
      img.onerror = () => reject(new Error(`stamp ${id}`));
      img.src = stampDataUrl(id);
    });
    p.catch(() => cache.delete(id));
    cache.set(id, p);
  }
  return p;
}

// ---------- Frames ----------

export const FRAMES = ['none', 'polaroid', 'film', 'hearts', 'stars', 'neon', 'lace', 'ribbon'] as const;
export type FrameId = (typeof FRAMES)[number];

function heartPath(g: CanvasRenderingContext2D, x: number, y: number, s: number): void {
  g.beginPath();
  g.moveTo(x, y + s * 0.35);
  g.bezierCurveTo(x - s * 0.6, y - s * 0.1, x - s * 0.3, y - s * 0.6, x, y - s * 0.25);
  g.bezierCurveTo(x + s * 0.3, y - s * 0.6, x + s * 0.6, y - s * 0.1, x, y + s * 0.35);
  g.closePath();
}

function starPath(g: CanvasRenderingContext2D, x: number, y: number, r: number): void {
  g.beginPath();
  for (let i = 0; i < 10; i++) {
    const a = -Math.PI / 2 + (i * Math.PI) / 5;
    const rr = i % 2 ? r * 0.45 : r;
    g.lineTo(x + Math.cos(a) * rr, y + Math.sin(a) * rr);
  }
  g.closePath();
}

/** Points evenly spaced along the border, `inset` px inside the edge. */
function borderPoints(w: number, h: number, inset: number, step: number): [number, number][] {
  const pts: [number, number][] = [];
  const nx = Math.max(2, Math.round((w - 2 * inset) / step));
  const ny = Math.max(2, Math.round((h - 2 * inset) / step));
  for (let i = 0; i <= nx; i++) {
    const x = inset + ((w - 2 * inset) * i) / nx;
    pts.push([x, inset], [x, h - inset]);
  }
  for (let j = 1; j < ny; j++) {
    const y = inset + ((h - 2 * inset) * j) / ny;
    pts.push([inset, y], [w - inset, y]);
  }
  return pts;
}

/** Draws a frame over the whole canvas (w × h). `color`: the frame's main colour where it has one. */
export function drawFrame(g: CanvasRenderingContext2D, id: FrameId, w: number, h: number, color = '#ffffff', alpha = 1): void {
  if (id === 'none') return;
  const u = Math.min(w, h) / 100;
  g.save();
  g.globalAlpha = alpha;
  switch (id) {
    case 'polaroid': {
      const side = 5 * u;
      const bottom = 16 * u;
      g.fillStyle = color;
      g.beginPath();
      g.rect(0, 0, w, h);
      g.rect(side, side, w - 2 * side, h - side - bottom);
      g.fill('evenodd');
      g.strokeStyle = 'rgba(0,0,0,0.12)';
      g.lineWidth = 0.4 * u;
      g.strokeRect(side, side, w - 2 * side, h - side - bottom);
      break;
    }
    case 'film': {
      const band = 9 * u;
      g.fillStyle = '#16131c';
      g.fillRect(0, 0, w, band);
      g.fillRect(0, h - band, w, band);
      g.fillStyle = '#f4efe6';
      const hole = 3 * u;
      for (let x = 2 * u; x < w - hole; x += 6 * u) {
        g.fillRect(x, band / 2 - hole / 2, hole * 1.3, hole);
        g.fillRect(x, h - band / 2 - hole / 2, hole * 1.3, hole);
      }
      break;
    }
    case 'hearts': {
      g.fillStyle = color === '#ffffff' ? '#ff6f9f' : color;
      g.strokeStyle = '#ffffff';
      g.lineWidth = 0.8 * u;
      for (const [x, y] of borderPoints(w, h, 5 * u, 11 * u)) {
        heartPath(g, x, y, 7 * u);
        g.fill();
        g.stroke();
      }
      break;
    }
    case 'stars': {
      const cols = ['#ffd34d', '#7fd6ff', '#ff8fb0', '#b48cff'];
      let i = 0;
      for (const [x, y] of borderPoints(w, h, 5 * u, 10 * u)) {
        g.fillStyle = cols[i++ % cols.length]!;
        starPath(g, x, y, (i % 3 ? 3.5 : 5) * u);
        g.fill();
      }
      break;
    }
    case 'neon': {
      const c = color === '#ffffff' ? '#ff3dbb' : color;
      g.strokeStyle = c;
      g.shadowColor = c;
      g.shadowBlur = 4 * u;
      g.lineWidth = 1.6 * u;
      const r = 6 * u;
      const m = 4 * u;
      g.beginPath();
      g.roundRect(m, m, w - 2 * m, h - 2 * m, r);
      g.stroke();
      g.lineWidth = 0.6 * u;
      g.strokeStyle = '#ffffff';
      g.stroke();
      break;
    }
    case 'lace': {
      const band = 4 * u;
      g.fillStyle = color;
      g.beginPath();
      g.rect(0, 0, w, h);
      g.rect(band, band, w - 2 * band, h - 2 * band);
      g.fill('evenodd');
      const r = 2.4 * u;
      for (const [x, y] of borderPoints(w, h, band, r * 2)) {
        g.beginPath();
        g.arc(x, y, r, 0, Math.PI * 2);
        g.fill();
      }
      g.fillStyle = 'rgba(255,255,255,0.0)';
      break;
    }
    case 'ribbon': {
      const c = color === '#ffffff' ? '#ff5f8f' : color;
      g.fillStyle = c;
      const band = 3.2 * u;
      g.fillRect(0, h * 0.16, w, band);
      g.fillRect(w * 0.82, 0, band, h);
      // A bow at the crossing.
      const x = w * 0.82 + band / 2;
      const y = h * 0.16 + band / 2;
      g.strokeStyle = '#2a2233';
      g.lineWidth = 0.6 * u;
      for (const s of [-1, 1]) {
        g.beginPath();
        g.moveTo(x, y);
        g.quadraticCurveTo(x + s * 10 * u, y - 9 * u, x + s * 11 * u, y);
        g.quadraticCurveTo(x + s * 10 * u, y + 9 * u, x, y);
        g.fill();
        g.stroke();
      }
      g.beginPath();
      g.arc(x, y, 2.4 * u, 0, Math.PI * 2);
      g.fill();
      g.stroke();
      break;
    }
  }
  g.restore();
}
