// Wardrobe thumbnails (dev only, `npm run dev` then open /thumbs.html): every outfit in
// LUCINA_ASSETS_DIR/wardrobe.json is drawn by the stage itself, so the tiles get the same toon
// materials, face shading and lights as the live view. Bust framing from the front, transparent
// background (the tile paints its category gradient), eyes open, 480×640 like the 3:4 tiles, with
// headroom for the name ribbon. Each frame goes through /dev-capture as outfit-<id>.png into
// LUCINA_CAPTURE_DIR. `?only=id,id` renders a subset; `?zoom=`, `?panY=`… adjust the view.

import type { Mesh, Object3D } from 'three';
import { createWardrobeStage, parseWardrobeFile } from '../src/index';

const WIDTH = 480;
const HEIGHT = 640;
/** Head and shoulders, the head under the tile's name ribbon. */
const VIEW = { yaw: 0, pitch: 0, zoom: 0.42, panX: -0.02, panY: 0.06 };
/** Every tile in the same idle pose: this clip, restarted, at the same time into it. */
const POSE_CLIP = 'idle_breathe';
const SETTLE_MS = 2500;

const logEl = document.querySelector<HTMLDivElement>('#log')!;
const log = (msg: string): void => {
  logEl.textContent += `${msg}\n`;
};
const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/** Blinks are procedural; zero them right before the frame is drawn so no tile has closed eyes. */
function openEyes(root: Object3D): void {
  root.traverse((o) => {
    const m = o as Mesh;
    const dict = m.morphTargetDictionary;
    const inf = m.morphTargetInfluences;
    if (!dict || !inf) return;
    for (const [name, i] of Object.entries(dict)) if (/blink|まばたき|ウィンク/i.test(name)) inf[i] = 0;
  });
}

function toDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(r.error);
    r.readAsDataURL(blob);
  });
}

async function main(): Promise<void> {
  const q = new URLSearchParams(location.search);
  const only = q.get('only')?.split(',').filter(Boolean);
  const file = parseWardrobeFile(await fetch('/dev-assets/wardrobe.json').then((r) => r.json()));
  const outfits = file.outfits.filter((o) => !only || only.includes(o.id));
  const canvas = document.querySelector<HTMLCanvasElement>('#stage')!;
  canvas.width = WIDTH;
  canvas.height = HEIGHT;
  const view = { ...VIEW };
  for (const k of Object.keys(view) as (keyof typeof view)[]) if (q.has(k)) view[k] = Number(q.get(k));
  const stage = await createWardrobeStage(canvas, {
    modelUrl: '/dev-assets/lucina.glb',
    manifestUrl: '/dev-assets/manifest.json',
    clipsUrl: '/dev-assets/lucina-anims.glb',
    clipsMetaUrl: '/dev-assets/clips.json',
    wardrobe: file.outfits,
    assetBase: '/dev-assets/',
    framing: 'bust',
    reducedMotion: true,
    changeEffect: false,
    muted: true,
    muteStorageKey: null,
    maxPixelRatio: 1,
    // setView needs the orbit controller; nobody touches this page.
    interactive: true,
    onError: (e) => log(`error: ${String(e)}`),
  });
  (window as unknown as { stage: typeof stage }).stage = stage;
  stage.setBackground('transparent');
  const clip = q.get('clip') ?? POSE_CLIP;
  stage.setStateClips({ idle: [clip] });
  let done = 0;
  for (const o of outfits) {
    await stage.setOutfit(o.id, { effect: false });
    stage.setView(view, false);
    // As a one-shot it starts at 0 (loops start at a random phase); 8 s, longer than the settle.
    void stage.play(clip, { loop: false, fade: 0 }).catch(() => {});
    await sleep(SETTLE_MS);
    openEyes(stage.three.model);
    const url = await toDataUrl(await stage.snapshot());
    const res = await fetch(`/dev-capture?name=${encodeURIComponent(`outfit-${o.id}`)}`, { method: 'POST', body: url });
    log(`${o.id} -> ${await res.text()}`);
    done++;
  }
  log(`done ${done}`);
  document.title = `done ${done}`;
}

void main().catch((e) => log(`failed: ${String(e)}`));
