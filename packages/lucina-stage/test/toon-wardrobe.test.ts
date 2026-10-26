// @vitest-environment jsdom
import { BoxGeometry, Group, Mesh, MeshStandardMaterial, PerspectiveCamera, ShaderMaterial, Texture } from 'three';
import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from 'vitest';
import { normalizeManifest } from '../src/manifest';
import { applyManifestToon, type ToonHandle } from '../src/toon';
import { createWardrobeStage, type StageFactory, type WardrobeStageOptions } from '../src/wardrobe';
import type { LucinaStage, LucinaStageOptions } from '../src/stage';

const manifests: Record<string, unknown> = {
  '/original/manifest.json': {
    material: 'engage-toon',
    toon: {
      ramps: 'ramps.png',
      default: { multi: 'orm' },
      materials: {
        Clothes: { map: 'body.webp', multi: 'body-multi.png', ramp: 'dress', outline: { color: [0.3, 0.4, 0.5] } },
        Trim: { map: 'trim.webp', multi: 'trim-multi.png', ramp: 'metal_plain' },
      },
    },
  },
  '/outfit/manifest.json': { camera: {} },
  '/outfit/own.json': { material: 'engage-toon', toon: { ramps: 'own-ramps.png', default: { ramp: 'metal' } } },
  '/outfit/standard.json': { material: 'standard' },
};

interface Made {
  root: Group;
  clothes: Mesh;
  trim: Mesh;
  original: MeshStandardMaterial;
  handle: ToonHandle | null;
  opts: LucinaStageOptions;
}

let made: Made[] = [];
let fetch: MockInstance<typeof globalThis.fetch>;
let host: HTMLDivElement;
let canvas: HTMLCanvasElement;
let blobs: Map<string, Blob>;
let blobSeq = 0;

/** Mirrors createLucinaStage: the manifest is loaded from `manifestUrl` and drives the toon setup. */
const factory: StageFactory = async (_canvas, opts) => {
  const root = new Group();
  // Clothes has a metalnessMap (file multi -> orm when inherited); Trim does not (file multi dropped).
  const original = new MeshStandardMaterial({ name: 'Clothes', map: new Texture(), metalnessMap: new Texture() });
  const clothes = new Mesh(new BoxGeometry(), original);
  const trim = new Mesh(new BoxGeometry(), new MeshStandardMaterial({ name: 'Trim', map: new Texture() }));
  root.add(clothes, trim);
  const url = opts.manifestUrl ?? '';
  const raw = url.startsWith('blob:') ? JSON.parse(await (await globalThis.fetch(url)).text()) : manifests[url];
  const handle = await applyManifestToon(root, normalizeManifest(raw), {
    ...opts,
    baseUrl: opts.manifestUrl?.startsWith('blob:') ? opts.modelUrl : opts.manifestUrl,
  });
  made.push({ root, clothes, trim, original, handle, opts });
  return {
    state: 'idle',
    speaking: false,
    isMuted: () => true,
    setMuted() {},
    setState() {},
    onStateChange: () => () => {},
    onTap: () => () => {},
    lookAtPointer() {},
    setExpression() {},
    gesture: () => true,
    dispose: () => handle?.dispose(),
    info: { placeholder: false, rig: { bones: {} } },
    three: { model: root, camera: new PerspectiveCamera() },
  } as unknown as LucinaStage;
};

const stageWith = (extra: Partial<WardrobeStageOptions> = {}) =>
  createWardrobeStage(
    canvas,
    {
      modelUrl: '/original/model.glb',
      manifestUrl: '/original/manifest.json',
      wardrobe: [
        { id: 'next', glb: 'next.glb', manifest: 'manifest.json' },
        { id: 'own', glb: 'own.glb', manifest: 'own.json' },
      ],
      assetBase: '/outfit/',
      changeEffect: false,
      ...extra,
    },
    factory,
  );

const uniforms = (m: Mesh) => (m.material as ShaderMaterial).uniforms;
const defines = (m: Mesh) => (m.material as ShaderMaterial).defines;
const fetched = () => fetch.mock.calls.map((c) => String(c[0]));

beforeEach(() => {
  made = [];
  blobs = new Map();
  blobSeq = 0;
  host = document.createElement('div');
  canvas = document.createElement('canvas');
  host.append(canvas);
  document.body.append(host);
  fetch = vi.spyOn(globalThis, 'fetch').mockImplementation(async (u) => {
    const url = String(u);
    const blob = blobs.get(url);
    return blob ? new Response(await blob.text()) : new Response('image');
  });
  vi.spyOn(URL, 'createObjectURL').mockImplementation((obj) => {
    const url = `blob:test/${++blobSeq}`;
    if (obj instanceof Blob) blobs.set(url, obj);
    return url;
  });
  vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
  vi.stubGlobal('createImageBitmap', async () => ({ width: 1, height: 1, close() {} }));
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  host.remove();
});

describe('wardrobe toon', () => {
  it('base model gets the full config with its file overrides', async () => {
    const stage = await stageWith();
    const base = made[0]!;
    expect(base.handle?.config.materials.Clothes).toMatchObject({ map: 'body.webp', multi: 'body-multi.png' });
    expect(uniforms(base.clothes).albedoMap!.value).not.toBe(base.original.map);
    expect(defines(base.clothes)).not.toHaveProperty('ORM_MULTI');
    expect(fetched().some((u) => u.endsWith('/original/body.webp'))).toBe(true);
    stage.dispose();
  });

  it('(a) a swap without a manifest keeps the base manifestUrl and inherits the look without file overrides', async () => {
    const stage = await stageWith();
    const oldHull = made[0]!.root.children.find((o) => o.userData.toonOutline)!;
    expect(oldHull).toBeDefined();
    await stage.setModel('/no-manifest.glb');
    const next = made[1]!;
    expect(next.opts.manifestUrl).toBe('/original/manifest.json');
    // Old handle disposed: hull gone, PBR material back.
    expect(oldHull.parent).toBeNull();
    expect(made[0]!.clothes.material).toBe(made[0]!.original);

    const cfg = next.handle!.config;
    expect(cfg.materials.Clothes).not.toHaveProperty('map');
    expect(cfg.materials.Clothes!.multi).toBe('orm'); // has a metalnessMap
    expect(cfg.materials.Trim).not.toHaveProperty('map');
    expect(cfg.materials.Trim).not.toHaveProperty('multi'); // no metalnessMap
    expect(cfg.materials.Trim!.ramp).toBe('metal_plain');
    expect(cfg.default).toMatchObject({ multi: 'orm' });
    expect(uniforms(next.clothes).albedoMap!.value).toBe(next.original.map);
    expect(uniforms(next.clothes).multiMap!.value).toBe(next.original.metalnessMap);
    expect(defines(next.clothes)).toHaveProperty('ORM_MULTI');
    expect(defines(next.trim)).not.toHaveProperty('HAS_MULTI');
    // Outlines survive inheritance; ramps still come from the base manifest's folder.
    expect(next.root.children.some((o) => o.userData.toonOutline)).toBe(true);
    const ramps = fetched().filter((u) => u.endsWith('/ramps.png'));
    expect(ramps).toHaveLength(2);
    expect(ramps.every((u) => u.endsWith('/original/ramps.png'))).toBe(true);
    stage.dispose();
  });

  it('the base manifest URL passed for another model URL also inherits', async () => {
    const stage = await stageWith();
    await stage.setModel('/explicit.glb', '/original/manifest.json');
    const next = made[1]!;
    expect(next.opts.manifestUrl).toBe('/original/manifest.json');
    expect(next.handle!.config.materials.Clothes).not.toHaveProperty('map');
    expect(next.handle!.config.materials.Clothes!.multi).toBe('orm');
    expect(uniforms(next.clothes).albedoMap!.value).toBe(next.original.map);
    stage.dispose();
  });

  it('an outfit whose own manifest has no toon block inherits too', async () => {
    const stage = await stageWith();
    await stage.setOutfit('next');
    const next = made[1]!;
    expect(next.opts.manifestUrl).toBe('/outfit/manifest.json');
    expect(next.handle!.config.materials.Clothes).not.toHaveProperty('map');
    expect(next.handle!.config.materials.Clothes!.multi).toBe('orm');
    expect(fetched().filter((u) => u.endsWith('/ramps.png')).every((u) => u.endsWith('/original/ramps.png'))).toBe(true);
    stage.dispose();
  });

  it('(b) swapping back to the base GLB restores the file overrides', async () => {
    const stage = await stageWith();
    await stage.setModel('/no-manifest.glb');
    await stage.setModel('/original/model.glb');
    const back = made[2]!;
    expect(back.opts.manifestUrl).toBe('/original/manifest.json');
    expect(back.handle!.config.materials.Clothes).toMatchObject({ map: 'body.webp', multi: 'body-multi.png' });
    expect(uniforms(back.clothes).albedoMap!.value).not.toBe(back.original.map);
    expect(defines(back.clothes)).not.toHaveProperty('ORM_MULTI');
    stage.dispose();
  });

  it('(c) an outfit with its own toon block uses it (URL or manifest object)', async () => {
    const stage = await stageWith();
    await stage.setOutfit('own');
    const own = made[1]!;
    expect(own.opts.manifestUrl).toBe('/outfit/own.json');
    expect(own.handle!.config.ramps).toBe('own-ramps.png');
    expect(own.handle!.config.materials).toEqual({});
    expect(uniforms(own.clothes).rowV!.value).toBe(2.5 / 8);
    expect(fetched().some((u) => u.endsWith('/outfit/own-ramps.png'))).toBe(true);

    await stage.setModel('/object.glb', { material: 'engage-toon', toon: { default: { ramp: 'hair' } } });
    const obj = made[2]!;
    expect(obj.opts.manifestUrl?.startsWith('blob:')).toBe(true);
    expect(obj.handle!.config.materials).toEqual({});
    expect(uniforms(obj.clothes).rowV!.value).toBe(3.5 / 8);
    stage.dispose();
  });

  it('(d) material: standard disables toon everywhere', async () => {
    const stage = await stageWith({ material: 'standard' });
    expect(made[0]!.handle).toBeNull();
    await stage.setModel('/no-manifest.glb');
    expect(made[1]!.handle).toBeNull();
    await stage.setOutfit('own');
    expect(made[2]!.handle).toBeNull();
    expect(made.every((m) => m.clothes.material === m.original)).toBe(true);
    expect(fetch).not.toHaveBeenCalled();
    stage.dispose();
  });

  it('a manifest asking for standard materials keeps PBR', async () => {
    const stage = await stageWith();
    await stage.setModel('/standard.glb', '/outfit/standard.json');
    expect(made[1]!.handle).toBeNull();
    stage.dispose();
  });
});
