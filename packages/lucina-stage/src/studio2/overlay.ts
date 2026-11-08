/**
 * Screen overlays over the stage:
 *
 * - **Live stamps** (Custom Cast's LiveStampData, StampExpand/StampShrink 0.42 s): a stamp pops up,
 *   holds, and shrinks away. During dance mode they pop on their own every couple of seconds.
 * - **Flick ring** (Custom Cast's FlickShortcut + CircleCommandUI, clockwise from the top): a
 *   ring of eight shortcuts around the finger; flicking toward one fires it (a face, a motion or a
 *   stamp).
 *
 * Both are plain DOM (fixed position, no pointer events of their own), so they work over any host.
 */
import { stampDataUrl } from './stamps';

export const STAMP_EXPAND_S = 0.42;

export type FlickAction = { kind: 'face'; id: string } | { kind: 'motion'; id: string } | { kind: 'stamp'; id: string };
export interface FlickSlot {
  action: FlickAction;
  /** Short label shown for faces and motions. */
  label?: string;
}

export const DEFAULT_FLICK_SLOTS: FlickSlot[] = [
  { action: { kind: 'face', id: 'smile' }, label: 'Smile' },
  { action: { kind: 'stamp', id: 'heart' } },
  { action: { kind: 'motion', id: 'greet' }, label: 'Wave' },
  { action: { kind: 'stamp', id: 'star' } },
  { action: { kind: 'face', id: 'surprised' }, label: 'Wow' },
  { action: { kind: 'stamp', id: 'sparkle' } },
  { action: { kind: 'motion', id: 'happy' }, label: 'Happy' },
  { action: { kind: 'stamp', id: 'note' } },
];

/** Which of `n` slots (clockwise from the top) a flick by (dx, dy) screen pixels points at; -1 inside the dead zone. */
export function flickSlot(dx: number, dy: number, n: number, deadZone = 36): number {
  if (Math.hypot(dx, dy) < deadZone || n <= 0) return -1;
  // 0 at the top, increasing clockwise (screen y grows downward).
  const a = Math.atan2(dx, -dy);
  const step = (Math.PI * 2) / n;
  return ((Math.round(a / step) % n) + n) % n;
}

let layer: HTMLDivElement | null = null;
function overlayLayer(): HTMLDivElement {
  if (layer && layer.isConnected) return layer;
  layer = document.createElement('div');
  layer.className = 'lucina-studio-overlay';
  Object.assign(layer.style, { position: 'fixed', inset: '0', pointerEvents: 'none', zIndex: '2147482000', overflow: 'hidden' });
  document.body.append(layer);
  return layer;
}

/** Pops a stamp centred at client (x, y). `size` in CSS pixels. Resolves when it is gone. */
export function popStamp(id: string, x: number, y: number, opts: { size?: number; reduced?: boolean; hold?: number } = {}): Promise<void> {
  if (typeof document === 'undefined') return Promise.resolve();
  const size = opts.size ?? 96;
  const img = document.createElement('img');
  img.alt = '';
  img.src = stampDataUrl(id);
  Object.assign(img.style, {
    position: 'absolute',
    left: `${x - size / 2}px`,
    top: `${y - size / 2}px`,
    width: `${size}px`,
    height: `${size}px`,
    filter: 'drop-shadow(0 4px 10px rgba(0,0,0,.25))',
    willChange: 'transform, opacity',
  });
  overlayLayer().append(img);
  const hold = (opts.hold ?? 1.1) * 1000;
  const t = STAMP_EXPAND_S * 1000;
  const rot = (Math.random() - 0.5) * 24;
  if (typeof img.animate !== 'function') {
    return new Promise((r) => setTimeout(() => (img.remove(), r()), t * 2 + hold));
  }
  const frames: Keyframe[] = opts.reduced
    ? [{ opacity: 0 }, { opacity: 1, offset: 0.15 }, { opacity: 1, offset: 0.85 }, { opacity: 0 }]
    : [
        { transform: `scale(0) rotate(${rot}deg)`, opacity: 0 },
        { transform: `scale(1.18) rotate(${rot}deg)`, opacity: 1, offset: (t * 0.7) / (t * 2 + hold) },
        { transform: `scale(1) rotate(${rot}deg)`, opacity: 1, offset: t / (t * 2 + hold) },
        { transform: `scale(1) rotate(${rot}deg)`, opacity: 1, offset: (t + hold) / (t * 2 + hold) },
        { transform: `scale(0) rotate(${rot * 2}deg)`, opacity: 0 },
      ];
  const anim = img.animate(frames, { duration: t * 2 + hold, easing: 'ease-out' });
  return anim.finished.then(
    () => img.remove(),
    () => img.remove(),
  );
}

/** The flick ring at client (x, y). `highlight(i)` marks a slot; `close()` removes it. */
export class FlickRing {
  readonly el: HTMLDivElement;
  private readonly items: HTMLDivElement[] = [];
  private current = -1;

  constructor(
    readonly x: number,
    readonly y: number,
    readonly slots: FlickSlot[],
    radius = 92,
  ) {
    const el = document.createElement('div');
    Object.assign(el.style, { position: 'absolute', left: `${x}px`, top: `${y}px`, width: '0', height: '0' });
    const disc = document.createElement('div');
    Object.assign(disc.style, {
      position: 'absolute',
      left: `${-radius - 34}px`,
      top: `${-radius - 34}px`,
      width: `${2 * radius + 68}px`,
      height: `${2 * radius + 68}px`,
      borderRadius: '50%',
      background: 'radial-gradient(circle, rgba(20,14,30,.15) 0 30%, rgba(20,14,30,.45) 31% 100%)',
      border: '2px solid rgba(255,255,255,.35)',
      backdropFilter: 'blur(2px)',
    });
    el.append(disc);
    const n = slots.length;
    slots.forEach((s, i) => {
      const a = (i / n) * Math.PI * 2;
      const it = document.createElement('div');
      Object.assign(it.style, {
        position: 'absolute',
        left: `${Math.sin(a) * radius - 28}px`,
        top: `${-Math.cos(a) * radius - 28}px`,
        width: '56px',
        height: '56px',
        borderRadius: '50%',
        background: 'rgba(255,255,255,.92)',
        color: '#2a2233',
        font: '600 12px/1.1 system-ui, -apple-system, sans-serif',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        textAlign: 'center',
        boxShadow: '0 2px 8px rgba(0,0,0,.25)',
        transition: 'transform .12s ease, background .12s ease',
      });
      if (s.action.kind === 'stamp') {
        const img = document.createElement('img');
        img.src = stampDataUrl(s.action.id);
        img.alt = s.action.id;
        Object.assign(img.style, { width: '40px', height: '40px' });
        it.append(img);
      } else it.textContent = s.label ?? s.action.id;
      it.dataset.kind = s.action.kind;
      el.append(it);
      this.items.push(it);
    });
    overlayLayer().append(el);
    this.el = el;
    if (typeof el.animate === 'function') el.animate([{ transform: 'scale(.6)', opacity: 0 }, { transform: 'scale(1)', opacity: 1 }], { duration: 140, easing: 'ease-out' });
  }

  /** Points the ring at the finger's offset; returns the slot (or -1). */
  track(dx: number, dy: number): number {
    const i = flickSlot(dx, dy, this.slots.length);
    if (i !== this.current) {
      this.items[this.current]?.style.setProperty('transform', '');
      this.items[this.current]?.style.setProperty('background', 'rgba(255,255,255,.92)');
      this.current = i;
      this.items[i]?.style.setProperty('transform', 'scale(1.22)');
      this.items[i]?.style.setProperty('background', '#ffd6e5');
    }
    return i;
  }

  get selected(): number {
    return this.current;
  }

  close(): void {
    this.el.remove();
  }
}
