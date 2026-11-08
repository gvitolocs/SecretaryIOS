/**
 * Photo decorations (Custom Cast's SceneDecoration, our own UI and art): frames, stamps, text
 * (fonts, colour, outline), freehand paint, undo/redo, then save. A self-contained DOM overlay, so
 * the web app and the iOS WKWebView page share it:
 *
 *   const blob = await openPhotoDecorator(document.body, photo, { labels });  // null: cancelled
 *
 * The document is a list of items over the photo; every edit pushes a snapshot for undo. Stamps and
 * text: tap to select, drag to move, two fingers (or the sliders) to scale and turn.
 */
import { STAMP_IDS, FRAMES, drawFrame, stampImage, type FrameId } from './stamps';
import { loadImage } from './filters';

export interface DecoratorLabels {
  title: string;
  frame: string;
  stamp: string;
  text: string;
  paint: string;
  undo: string;
  redo: string;
  save: string;
  cancel: string;
  remove: string;
  mirror: string;
  size: string;
  turn: string;
  opacity: string;
  outline: string;
  font: string;
  add: string;
  placeholder: string;
  brush: string;
  eraser: string;
  clear: string;
  frames: Record<FrameId, string>;
  fonts: Record<FontId, string>;
}

export const FONTS = {
  sans: '"Sora Variable", "Sora", system-ui, -apple-system, sans-serif',
  round: 'ui-rounded, "SF Pro Rounded", "Arial Rounded MT Bold", system-ui, sans-serif',
  serif: 'Georgia, "Times New Roman", serif',
  bold: '"Arial Black", Impact, system-ui, sans-serif',
  hand: '"Bradley Hand", "Segoe Print", "Comic Sans MS", cursive',
  mono: '"JetBrains Mono Variable", ui-monospace, Menlo, monospace',
} as const;
export type FontId = keyof typeof FONTS;

export const DEFAULT_LABELS: DecoratorLabels = {
  title: 'Decorate',
  frame: 'Frame',
  stamp: 'Stamps',
  text: 'Text',
  paint: 'Paint',
  undo: 'Undo',
  redo: 'Redo',
  save: 'Save',
  cancel: 'Cancel',
  remove: 'Remove',
  mirror: 'Mirror',
  size: 'Size',
  turn: 'Turn',
  opacity: 'Opacity',
  outline: 'Outline',
  font: 'Font',
  add: 'Add',
  placeholder: 'Your text',
  brush: 'Brush',
  eraser: 'Eraser',
  clear: 'Clear paint',
  frames: { none: 'None', polaroid: 'Polaroid', film: 'Film', hearts: 'Hearts', stars: 'Stars', neon: 'Neon', lace: 'Lace', ribbon: 'Ribbon' },
  fonts: { sans: 'Sans', round: 'Rounded', serif: 'Serif', bold: 'Bold', hand: 'Hand', mono: 'Mono' },
};

export const PALETTE = ['#ffffff', '#2a2233', '#ff5f8f', '#ffd34d', '#7fd6ff', '#9be0c0', '#b48cff', '#ff8a3d'];

export interface StampItem {
  kind: 'stamp';
  stamp: string;
  x: number;
  y: number;
  /** Width in photo pixels. */
  size: number;
  rot: number;
  alpha: number;
  flip: boolean;
}
export interface TextItem {
  kind: 'text';
  text: string;
  x: number;
  y: number;
  /** Font size in photo pixels. */
  size: number;
  rot: number;
  alpha: number;
  font: FontId;
  color: string;
  outline: string;
  /** Outline width as a fraction of the font size (0 = none). */
  outlineWidth: number;
}
export interface StrokeItem {
  kind: 'stroke';
  color: string;
  width: number;
  erase: boolean;
  points: number[];
}
export type DecoItem = StampItem | TextItem | StrokeItem;

export interface DecoDoc {
  frame: FrameId;
  frameColor: string;
  frameAlpha: number;
  items: DecoItem[];
}

export function emptyDoc(): DecoDoc {
  return { frame: 'none', frameColor: '#ffffff', frameAlpha: 1, items: [] };
}

/** Undo/redo over document snapshots. */
export class History<T> {
  private past: string[] = [];
  private future: string[] = [];
  constructor(
    private present: T,
    private readonly limit = 60,
  ) {}
  get value(): T {
    return this.present;
  }
  /** Records the current state, then replaces it. */
  push(next: T): void {
    this.past.push(JSON.stringify(this.present));
    if (this.past.length > this.limit) this.past.shift();
    this.future = [];
    this.present = next;
  }
  /** Replace without a new undo step (e.g. while dragging). */
  replace(next: T): void {
    this.present = next;
  }
  /** Marks the current state as an undo point without changing it. */
  checkpoint(): void {
    this.past.push(JSON.stringify(this.present));
    if (this.past.length > this.limit) this.past.shift();
    this.future = [];
  }
  get canUndo(): boolean {
    return this.past.length > 0;
  }
  get canRedo(): boolean {
    return this.future.length > 0;
  }
  undo(): T {
    const p = this.past.pop();
    if (p !== undefined) {
      this.future.push(JSON.stringify(this.present));
      this.present = JSON.parse(p) as T;
    }
    return this.present;
  }
  redo(): T {
    const f = this.future.pop();
    if (f !== undefined) {
      this.past.push(JSON.stringify(this.present));
      this.present = JSON.parse(f) as T;
    }
    return this.present;
  }
}

/** Text item bounds (unrotated), for hit tests and the selection box. */
function textBox(g: CanvasRenderingContext2D, t: TextItem): { w: number; h: number } {
  g.font = `700 ${t.size}px ${FONTS[t.font]}`;
  const lines = t.text.split('\n');
  const w = Math.max(...lines.map((l) => g.measureText(l).width), t.size * 0.5);
  return { w: w + t.size * t.outlineWidth * 2, h: lines.length * t.size * 1.2 };
}

function itemBox(g: CanvasRenderingContext2D, it: StampItem | TextItem): { w: number; h: number } {
  return it.kind === 'stamp' ? { w: it.size, h: it.size } : textBox(g, it);
}

/** Is (px, py) inside the item (in its rotated frame)? */
export function hitItem(g: CanvasRenderingContext2D, it: StampItem | TextItem, px: number, py: number, pad = 0): boolean {
  const { w, h } = itemBox(g, it);
  const c = Math.cos(-it.rot);
  const s = Math.sin(-it.rot);
  const dx = px - it.x;
  const dy = py - it.y;
  const lx = dx * c - dy * s;
  const ly = dx * s + dy * c;
  return Math.abs(lx) <= w / 2 + pad && Math.abs(ly) <= h / 2 + pad;
}

/** Renders the document over the photo. */
export function renderDoc(
  g: CanvasRenderingContext2D,
  photo: CanvasImageSource,
  w: number,
  h: number,
  doc: DecoDoc,
  stamps: Map<string, HTMLImageElement>,
  paint: HTMLCanvasElement | null,
  selected = -1,
): void {
  g.clearRect(0, 0, w, h);
  g.drawImage(photo, 0, 0, w, h);
  drawFrame(g, doc.frame, w, h, doc.frameColor, doc.frameAlpha);
  // Paint on its own layer (the eraser only erases paint).
  if (paint) {
    const p = paint.getContext('2d')!;
    p.clearRect(0, 0, w, h);
    for (const it of doc.items) if (it.kind === 'stroke') drawStroke(p, it);
    g.drawImage(paint, 0, 0);
  }
  doc.items.forEach((it, i) => {
    if (it.kind === 'stroke') return;
    g.save();
    g.globalAlpha = it.alpha;
    g.translate(it.x, it.y);
    g.rotate(it.rot);
    if (it.kind === 'stamp') {
      const img = stamps.get(it.stamp);
      if (img) {
        if (it.flip) g.scale(-1, 1);
        g.drawImage(img, -it.size / 2, -it.size / 2, it.size, it.size);
      }
    } else {
      g.font = `700 ${it.size}px ${FONTS[it.font]}`;
      g.textAlign = 'center';
      g.textBaseline = 'middle';
      g.lineJoin = 'round';
      const lines = it.text.split('\n');
      lines.forEach((line, k) => {
        const y = (k - (lines.length - 1) / 2) * it.size * 1.2;
        if (it.outlineWidth > 0) {
          g.strokeStyle = it.outline;
          g.lineWidth = it.size * it.outlineWidth * 2;
          g.strokeText(line, 0, y);
        }
        g.fillStyle = it.color;
        g.fillText(line, 0, y);
      });
    }
    g.restore();
    if (i === selected) {
      const { w: bw, h: bh } = itemBox(g, it);
      g.save();
      g.translate(it.x, it.y);
      g.rotate(it.rot);
      g.setLineDash([Math.max(4, w / 120), Math.max(3, w / 160)]);
      g.lineWidth = Math.max(2, w / 400);
      g.strokeStyle = '#ffffff';
      g.strokeRect(-bw / 2 - 6, -bh / 2 - 6, bw + 12, bh + 12);
      g.strokeStyle = '#ff5f8f';
      g.lineDashOffset = Math.max(4, w / 120);
      g.strokeRect(-bw / 2 - 6, -bh / 2 - 6, bw + 12, bh + 12);
      g.restore();
    }
  });
}

function drawStroke(g: CanvasRenderingContext2D, s: StrokeItem): void {
  const p = s.points;
  if (p.length < 2) return;
  g.save();
  g.globalCompositeOperation = s.erase ? 'destination-out' : 'source-over';
  g.strokeStyle = s.color;
  g.fillStyle = s.color;
  g.lineWidth = s.width;
  g.lineCap = 'round';
  g.lineJoin = 'round';
  if (p.length === 2) {
    g.beginPath();
    g.arc(p[0]!, p[1]!, s.width / 2, 0, Math.PI * 2);
    g.fill();
  } else {
    g.beginPath();
    g.moveTo(p[0]!, p[1]!);
    for (let i = 2; i < p.length - 2; i += 2) {
      const mx = (p[i]! + p[i + 2]!) / 2;
      const my = (p[i + 1]! + p[i + 3]!) / 2;
      g.quadraticCurveTo(p[i]!, p[i + 1]!, mx, my);
    }
    g.lineTo(p[p.length - 2]!, p[p.length - 1]!);
    g.stroke();
  }
  g.restore();
}

const CSS = `
.lsd{position:fixed;inset:0;z-index:2147483000;display:flex;flex-direction:column;background:rgba(14,12,20,.96);color:#f5f0fa;font:14px/1.3 system-ui,-apple-system,sans-serif;-webkit-user-select:none;user-select:none;padding:env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left)}
.lsd *{box-sizing:border-box}
.lsd-bar{display:flex;align-items:center;gap:8px;padding:8px 12px}
.lsd-bar h2{flex:1;margin:0;font-size:16px;font-weight:600}
.lsd button{font:inherit;color:inherit;background:rgba(255,255,255,.1);border:1px solid rgba(255,255,255,.14);border-radius:999px;padding:7px 13px;min-height:36px;cursor:pointer}
.lsd button[aria-pressed=true]{background:#ff5f8f;border-color:#ff5f8f;color:#fff}
.lsd button:disabled{opacity:.4}
.lsd .lsd-primary{background:#ff5f8f;border-color:#ff5f8f;font-weight:600}
.lsd-view{flex:1;min-height:0;display:flex;align-items:center;justify-content:center;padding:4px 12px;touch-action:none}
.lsd-view canvas{max-width:100%;max-height:100%;border-radius:10px;box-shadow:0 6px 30px rgba(0,0,0,.5);touch-action:none}
.lsd-tabs{display:flex;gap:6px;padding:6px 12px;overflow-x:auto}
.lsd-panel{display:flex;flex-wrap:wrap;gap:8px;align-items:center;padding:6px 12px 12px;max-height:34vh;overflow-y:auto}
.lsd-stamp{width:48px;height:48px;padding:4px!important;border-radius:12px!important}
.lsd-stamp img{width:100%;height:100%}
.lsd-sw{width:30px;height:30px;min-height:30px!important;padding:0!important;border-radius:50%!important;border:2px solid rgba(255,255,255,.4)!important}
.lsd-sw[aria-pressed=true]{outline:3px solid #ff5f8f;outline-offset:2px}
.lsd label{display:flex;align-items:center;gap:6px;font-size:13px;opacity:.9}
.lsd input[type=range]{width:120px;accent-color:#ff5f8f}
.lsd input[type=text]{font:inherit;color:#fff;background:rgba(255,255,255,.08);border:1px solid rgba(255,255,255,.2);border-radius:10px;padding:8px 10px;min-width:0;flex:1 1 160px}
.lsd select{font:inherit;color:#fff;background:#2a2433;border:1px solid rgba(255,255,255,.2);border-radius:10px;padding:7px}
.lsd-row{display:flex;flex-wrap:wrap;gap:8px;align-items:center;width:100%}
`;

type Tab = 'frame' | 'stamp' | 'text' | 'paint';

export interface DecoratorOptions {
  labels?: Partial<DecoratorLabels>;
  /** Largest side of the working image (default 2048; the photo is scaled down to it). */
  maxSize?: number;
  /** Starting tab. */
  tab?: Tab;
}

export interface DecoratorHandle {
  /** Resolves with the decorated PNG, or null when cancelled. */
  readonly result: Promise<Blob | null>;
  readonly root: HTMLElement;
  close(): void;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, props: Partial<HTMLElementTagNameMap[K]> & Record<string, unknown> = {}, ...kids: (Node | string)[]): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === 'dataset') Object.assign(e.dataset, v);
    else if (k.startsWith('aria') || k === 'role') e.setAttribute(k.replace(/^aria([A-Z])/, (_m, c: string) => `aria-${c.toLowerCase()}`), String(v));
    else (e as unknown as Record<string, unknown>)[k] = v;
  }
  e.append(...kids);
  return e;
}

function slider(label: string, min: number, max: number, step: number, value: number, on: (v: number) => void, done: () => void): HTMLLabelElement {
  const input = el('input', { type: 'range', min: String(min), max: String(max), step: String(step), value: String(value) });
  input.addEventListener('pointerdown', () => done());
  input.addEventListener('input', () => on(Number(input.value)));
  return el('label', {}, label, input);
}

/** Opens the decorator over `host` (usually document.body). */
export function openPhotoDecorator(host: HTMLElement, photo: Blob, opts: DecoratorOptions = {}): DecoratorHandle {
  const L: DecoratorLabels = { ...DEFAULT_LABELS, ...opts.labels, frames: { ...DEFAULT_LABELS.frames, ...opts.labels?.frames }, fonts: { ...DEFAULT_LABELS.fonts, ...opts.labels?.fonts } };
  let resolveResult!: (b: Blob | null) => void;
  const result = new Promise<Blob | null>((r) => (resolveResult = r));

  if (!document.getElementById('lsd-style')) document.head.append(el('style', { id: 'lsd-style', textContent: CSS }));
  const root = el('div', { className: 'lsd', role: 'dialog', ariaLabel: L.title });
  const canvas = el('canvas', { ariaLabel: L.title });
  const view = el('div', { className: 'lsd-view' }, canvas);
  const undoBtn = el('button', { type: 'button', textContent: L.undo, disabled: true });
  const redoBtn = el('button', { type: 'button', textContent: L.redo, disabled: true });
  const cancelBtn = el('button', { type: 'button', textContent: L.cancel });
  const saveBtn = el('button', { type: 'button', className: 'lsd-primary', textContent: L.save });
  const bar = el('div', { className: 'lsd-bar' }, cancelBtn, el('h2', { textContent: L.title }), undoBtn, redoBtn, saveBtn);
  const tabs = el('div', { className: 'lsd-tabs', role: 'tablist' });
  const panel = el('div', { className: 'lsd-panel' });
  root.append(bar, view, tabs, panel);
  host.append(root);

  const g = canvas.getContext('2d')!;
  const paint = document.createElement('canvas');
  const stamps = new Map<string, HTMLImageElement>();
  let photoImg: HTMLImageElement | HTMLCanvasElement | null = null;
  let W = 1;
  let H = 1;
  const hist = new History<DecoDoc>(emptyDoc());
  let tab: Tab = opts.tab ?? 'stamp';
  let selected = -1;
  // Text and paint settings carry over between items.
  const textStyle = { font: 'round' as FontId, color: '#ffffff', outline: '#ff5f8f', outlineWidth: 0.08 };
  const brush = { color: '#ff5f8f', width: 0.012, erase: false };
  let closed = false;

  const doc = () => hist.value;
  const clone = (): DecoDoc => JSON.parse(JSON.stringify(hist.value)) as DecoDoc;
  const sel = (): StampItem | TextItem | null => {
    const it = doc().items[selected];
    return it && it.kind !== 'stroke' ? it : null;
  };

  function draw(): void {
    if (!photoImg) return;
    renderDoc(g, photoImg, W, H, doc(), stamps, paint, selected);
    undoBtn.disabled = !hist.canUndo;
    redoBtn.disabled = !hist.canRedo;
  }

  function edit(fn: (d: DecoDoc) => void, live = false): void {
    const d = clone();
    fn(d);
    if (live) hist.replace(d);
    else hist.push(d);
    draw();
  }

  // ---------- Panels ----------
  function swatches(current: string, pick: (c: string) => void): HTMLElement {
    const row = el('div', { className: 'lsd-row' });
    for (const c of PALETTE) {
      const b = el('button', { type: 'button', className: 'lsd-sw', ariaLabel: c, ariaPressed: String(c === current) });
      b.style.background = c;
      b.addEventListener('click', () => {
        pick(c);
        row.querySelectorAll('button').forEach((x) => x.setAttribute('aria-pressed', String(x === b)));
      });
      row.append(b);
    }
    return row;
  }

  function itemControls(): HTMLElement | null {
    const it = sel();
    if (!it) return null;
    const row = el('div', { className: 'lsd-row' });
    const cp = () => hist.checkpoint();
    const max = it.kind === 'stamp' ? Math.max(W, H) * 0.9 : Math.max(W, H) * 0.25;
    row.append(
      slider(L.size, Math.round(max * 0.04), Math.round(max), 1, it.size, (v) => edit((d) => ((d.items[selected] as StampItem).size = v), true), cp),
      slider(L.turn, -180, 180, 1, Math.round((it.rot * 180) / Math.PI), (v) => edit((d) => ((d.items[selected] as StampItem).rot = (v * Math.PI) / 180), true), cp),
      slider(L.opacity, 0.1, 1, 0.05, it.alpha, (v) => edit((d) => ((d.items[selected] as StampItem).alpha = v), true), cp),
    );
    if (it.kind === 'stamp') {
      const m = el('button', { type: 'button', textContent: L.mirror });
      m.addEventListener('click', () => edit((d) => ((d.items[selected] as StampItem).flip = !(d.items[selected] as StampItem).flip)));
      row.append(m);
    }
    const rm = el('button', { type: 'button', textContent: L.remove });
    rm.addEventListener('click', () => {
      edit((d) => d.items.splice(selected, 1));
      selected = -1;
      renderPanel();
      draw();
    });
    row.append(rm);
    return row;
  }

  function renderPanel(): void {
    panel.replaceChildren();
    if (tab === 'frame') {
      const row = el('div', { className: 'lsd-row' });
      for (const f of FRAMES) {
        const b = el('button', { type: 'button', textContent: L.frames[f], ariaPressed: String(doc().frame === f) });
        b.addEventListener('click', () => {
          edit((d) => (d.frame = f));
          renderPanel();
        });
        row.append(b);
      }
      panel.append(row, swatches(doc().frameColor, (c) => edit((d) => (d.frameColor = c))), slider(L.opacity, 0.2, 1, 0.05, doc().frameAlpha, (v) => edit((d) => (d.frameAlpha = v), true), () => hist.checkpoint()));
    } else if (tab === 'stamp') {
      const row = el('div', { className: 'lsd-row' });
      for (const id of STAMP_IDS) {
        const b = el('button', { type: 'button', className: 'lsd-stamp', ariaLabel: id }, el('img', { alt: '', src: stamps.get(id)?.src ?? '' }));
        b.addEventListener('click', () => {
          edit((d) => d.items.push({ kind: 'stamp', stamp: id, x: W / 2 + (Math.random() - 0.5) * W * 0.2, y: H / 2 + (Math.random() - 0.5) * H * 0.2, size: Math.min(W, H) * 0.24, rot: (Math.random() - 0.5) * 0.4, alpha: 1, flip: false }));
          selected = doc().items.length - 1;
          renderPanel();
          draw();
        });
        row.append(b);
      }
      panel.append(row);
      const c = itemControls();
      if (c && sel()?.kind === 'stamp') panel.append(c);
    } else if (tab === 'text') {
      const cur = sel()?.kind === 'text' ? (sel() as TextItem) : null;
      const input = el('input', { type: 'text', placeholder: L.placeholder, value: cur?.text ?? '', maxLength: 80 });
      const add = el('button', { type: 'button', className: 'lsd-primary', textContent: L.add });
      const font = el('select', { ariaLabel: L.font });
      for (const id of Object.keys(FONTS) as FontId[]) {
        const o = el('option', { value: id, textContent: L.fonts[id] });
        o.style.fontFamily = FONTS[id];
        font.append(o);
      }
      font.value = cur?.font ?? textStyle.font;
      const applyStyle = (patch: Partial<TextItem>) => {
        Object.assign(textStyle, patch);
        if (sel()?.kind === 'text') edit((d) => Object.assign(d.items[selected]!, patch));
      };
      input.addEventListener('focus', () => {
        if (sel()?.kind === 'text') hist.checkpoint();
      });
      input.addEventListener('input', () => {
        if (sel()?.kind === 'text') edit((d) => ((d.items[selected] as TextItem).text = input.value || ' '), true);
      });
      add.addEventListener('click', () => {
        const text = input.value.trim() || L.placeholder;
        edit((d) => d.items.push({ kind: 'text', text, x: W / 2, y: H * 0.8, size: Math.min(W, H) * 0.09, rot: 0, alpha: 1, ...textStyle }));
        selected = doc().items.length - 1;
        renderPanel();
        draw();
      });
      font.addEventListener('change', () => applyStyle({ font: font.value as FontId }));
      panel.append(el('div', { className: 'lsd-row' }, input, add, font), swatches(cur?.color ?? textStyle.color, (c) => applyStyle({ color: c })));
      const ol = el('div', { className: 'lsd-row' }, el('span', { textContent: L.outline }));
      ol.append(swatches(cur?.outline ?? textStyle.outline, (c) => applyStyle({ outline: c })));
      ol.append(slider(L.outline, 0, 0.2, 0.01, cur?.outlineWidth ?? textStyle.outlineWidth, (v) => applyStyle({ outlineWidth: v }), () => {}));
      panel.append(ol);
      const c = itemControls();
      if (c && cur) panel.append(c);
    } else {
      const er = el('button', { type: 'button', textContent: L.eraser, ariaPressed: String(brush.erase) });
      er.addEventListener('click', () => {
        brush.erase = !brush.erase;
        er.setAttribute('aria-pressed', String(brush.erase));
      });
      const clear = el('button', { type: 'button', textContent: L.clear });
      clear.addEventListener('click', () => edit((d) => (d.items = d.items.filter((i) => i.kind !== 'stroke'))));
      panel.append(
        swatches(brush.color, (c) => {
          brush.color = c;
          brush.erase = false;
          er.setAttribute('aria-pressed', 'false');
        }),
        el('div', { className: 'lsd-row' }, slider(L.brush, 0.003, 0.05, 0.001, brush.width, (v) => (brush.width = v), () => {}), er, clear),
      );
    }
  }

  function renderTabs(): void {
    tabs.replaceChildren();
    for (const [id, label] of [
      ['frame', L.frame],
      ['stamp', L.stamp],
      ['text', L.text],
      ['paint', L.paint],
    ] as [Tab, string][]) {
      const b = el('button', { type: 'button', role: 'tab', textContent: label, ariaPressed: String(tab === id) });
      b.addEventListener('click', () => {
        tab = id;
        if (tab === 'paint' || tab === 'frame') selected = -1;
        renderTabs();
        renderPanel();
        draw();
      });
      tabs.append(b);
    }
  }

  // ---------- Pointer: select, drag, pinch/turn, paint ----------
  const pts = new Map<number, { x: number; y: number }>();
  let drag: { start: { x: number; y: number }; item: { x: number; y: number }; moved: boolean } | null = null;
  let pinch: { d0: number; a0: number; size: number; rot: number } | null = null;
  let stroke: StrokeItem | null = null;

  const toDoc = (e: PointerEvent) => {
    const r = canvas.getBoundingClientRect();
    return { x: ((e.clientX - r.left) / Math.max(r.width, 1)) * W, y: ((e.clientY - r.top) / Math.max(r.height, 1)) * H };
  };
  const twoFinger = () => {
    const [a, b] = [...pts.values()];
    return { d: Math.hypot(b!.x - a!.x, b!.y - a!.y), a: Math.atan2(b!.y - a!.y, b!.x - a!.x) };
  };

  canvas.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    try {
      canvas.setPointerCapture(e.pointerId);
    } catch {
      /* synthetic */
    }
    const p = toDoc(e);
    pts.set(e.pointerId, p);
    if (tab === 'paint') {
      if (pts.size > 1) return;
      stroke = { kind: 'stroke', color: brush.color, width: Math.max(2, brush.width * Math.max(W, H)), erase: brush.erase, points: [p.x, p.y] };
      edit((d) => d.items.push(stroke!));
      return;
    }
    if (pts.size === 2 && sel()) {
      const t = twoFinger();
      const it = sel()!;
      hist.checkpoint();
      pinch = { d0: t.d, a0: t.a, size: it.size, rot: it.rot };
      drag = null;
      return;
    }
    if (pts.size > 1) return;
    // Topmost item under the finger (with a little slack for small stamps).
    const pad = Math.max(W, H) * 0.01;
    let hit = -1;
    doc().items.forEach((it, i) => {
      if (it.kind !== 'stroke' && hitItem(g, it, p.x, p.y, pad)) hit = i;
    });
    if (hit >= 0) {
      const it = doc().items[hit] as StampItem | TextItem;
      if (it.kind === 'text' && tab !== 'text') tab = 'text';
      if (it.kind === 'stamp' && tab !== 'stamp') tab = 'stamp';
      selected = hit;
      hist.checkpoint();
      drag = { start: p, item: { x: it.x, y: it.y }, moved: false };
      renderTabs();
      renderPanel();
    } else {
      selected = -1;
      renderPanel();
    }
    draw();
  });
  canvas.addEventListener('pointermove', (e) => {
    if (!pts.has(e.pointerId)) return;
    const p = toDoc(e);
    pts.set(e.pointerId, p);
    if (stroke) {
      stroke.points.push(p.x, p.y);
      edit((d) => ((d.items[d.items.length - 1] as StrokeItem).points = stroke!.points.slice()), true);
      return;
    }
    if (pinch && pts.size >= 2 && selected >= 0) {
      const t = twoFinger();
      const k = t.d / Math.max(1, pinch.d0);
      const pz = pinch;
      edit((d) => {
        const it = d.items[selected] as StampItem;
        it.size = Math.max(8, Math.min(Math.max(W, H) * 1.2, pz.size * k));
        it.rot = pz.rot + (t.a - pz.a0);
      }, true);
      return;
    }
    if (drag && selected >= 0) {
      const dx = p.x - drag.start.x;
      const dy = p.y - drag.start.y;
      if (Math.hypot(dx, dy) > 2) drag.moved = true;
      const base = drag.item;
      edit((d) => {
        const it = d.items[selected] as StampItem;
        it.x = Math.max(0, Math.min(W, base.x + dx));
        it.y = Math.max(0, Math.min(H, base.y + dy));
      }, true);
    }
  });
  const up = (e: PointerEvent) => {
    pts.delete(e.pointerId);
    if (stroke && !pts.size) stroke = null;
    if (pts.size < 2) pinch = null;
    if (!pts.size) {
      drag = null;
      renderPanel();
      draw();
    }
  };
  canvas.addEventListener('pointerup', up);
  canvas.addEventListener('pointercancel', up);

  undoBtn.addEventListener('click', () => {
    hist.undo();
    selected = -1;
    renderPanel();
    draw();
  });
  redoBtn.addEventListener('click', () => {
    hist.redo();
    selected = -1;
    renderPanel();
    draw();
  });
  const onKey = (e: KeyboardEvent) => {
    if ((e.target as HTMLElement | null)?.tagName === 'INPUT') return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'z') {
      e.preventDefault();
      if (e.shiftKey) hist.redo();
      else hist.undo();
      selected = -1;
      renderPanel();
      draw();
    } else if (e.key === 'Escape') close(null);
    else if ((e.key === 'Delete' || e.key === 'Backspace') && sel()) {
      edit((d) => d.items.splice(selected, 1));
      selected = -1;
      renderPanel();
      draw();
    }
  };
  window.addEventListener('keydown', onKey);

  function close(blob: Blob | null): void {
    if (closed) return;
    closed = true;
    window.removeEventListener('keydown', onKey);
    root.remove();
    resolveResult(blob);
  }
  cancelBtn.addEventListener('click', () => close(null));
  saveBtn.addEventListener('click', () => {
    if (!photoImg) return close(null);
    // Without the selection box.
    renderDoc(g, photoImg, W, H, doc(), stamps, paint, -1);
    canvas.toBlob((b) => close(b), 'image/png');
  });

  // ---------- Load ----------
  void (async () => {
    try {
      const [img] = await Promise.all([loadImage(photo), ...STAMP_IDS.map((id) => stampImage(id).then((im) => stamps.set(id, im)).catch(() => {}))]);
      const max = opts.maxSize ?? 2048;
      const k = Math.min(1, max / Math.max(img.width, img.height));
      W = Math.max(1, Math.round(img.width * k));
      H = Math.max(1, Math.round(img.height * k));
      if (k < 1) {
        const c = document.createElement('canvas');
        c.width = W;
        c.height = H;
        c.getContext('2d')!.drawImage(img, 0, 0, W, H);
        photoImg = c;
      } else photoImg = img;
      canvas.width = paint.width = W;
      canvas.height = paint.height = H;
      renderTabs();
      renderPanel();
      draw();
    } catch {
      close(null);
    }
  })();

  return { result, root, close: () => close(null) };
}
