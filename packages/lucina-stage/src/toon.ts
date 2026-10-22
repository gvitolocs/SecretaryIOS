/**
 * Fire Emblem Engage toon look (port of the game shader as rebuilt in Blender): lambert -> game
 * colour ramp (AO from the Multi map), metal ramp, cyan Emblem rim, hair angel ring, inverted-hull
 * outlines. Opt-in per model: manifest `"material": "engage-toon"` + a `toon` block keyed by
 * material name (ramps and texture overrides are private assets next to the manifest).
 * Swapped-in outfits without their own block inherit it without the per-file texture overrides.
 */
import {
  ACESFilmicToneMapping,
  BackSide,
  ClampToEdgeWrapping,
  Color,
  DataTexture,
  Float32BufferAttribute,
  LinearFilter,
  Matrix3,
  Mesh,
  NoColorSpace,
  ShaderMaterial,
  SkinnedMesh,
  SRGBColorSpace,
  Texture,
  Vector2,
  Vector3,
  type BufferGeometry,
  type Material,
  type MeshStandardMaterial,
  type Object3D,
  type Scene,
  type WebGLRenderer,
} from 'three';
import { fetchOk, type LoadOptions } from './loader';
import type { NormalizedManifest } from './manifest';
import { toonFragment, toonVertex } from './toon-shaders';

type RGB = [number, number, number];
const RAMP_ROWS = { dress: 0, skin: 1, metal: 2, hair: 3, dress_plain: 4, metal_plain: 5 };
type Ramp = keyof typeof RAMP_ROWS;
export interface ToonBloom {
  threshold: number;
  strength: number;
  radius: number;
}
export interface ToonMaterialConfig {
  preset: 'lit' | 'flat' | 'hide';
  ramp: Ramp;
  map?: string;
  multi?: string;
  metalRamp?: 'metal' | 'metal_plain';
  normalScale: number;
  color?: RGB;
  solidColor: boolean;
  saturation: number;
  detail: number;
  detailMean: number;
  emission: RGB;
  emissionAmount: number;
  rim: number;
  angelRing?: RGB;
  alpha: 'map' | 'none';
  outline?: { color: RGB; albedoMix: number; scale: number; weight?: 'shell' };
}
export interface ToonConfig {
  ramps?: string;
  light: RGB;
  ambient: number;
  bloom: ToonBloom;
  default?: ToonMaterialConfig;
  hairParts?: ToonMaterialConfig;
  materials: Record<string, ToonMaterialConfig>;
}
const object = (v: unknown): Record<string, unknown> =>
  v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
const number = (v: unknown, fallback: number, min = 0, max = 1): number =>
  typeof v === 'number' && Number.isFinite(v) && v >= min && v <= max ? v : fallback;
const rgb = (v: unknown, min = 0, max = 1): RGB | undefined =>
  Array.isArray(v) && v.length === 3 && v.every((n) => typeof n === 'number' && Number.isFinite(n) && n >= min && n <= max)
    ? (v as RGB)
    : undefined;
const file = (v: unknown): string | undefined => (typeof v === 'string' && /^[a-z0-9][a-z0-9._-]*$/.test(v) ? v : undefined);
const ramp = (v: unknown): Ramp => (typeof v === 'string' && Object.hasOwn(RAMP_ROWS, v) ? (v as Ramp) : 'dress');

/** Invalid optional values fall back independently; unknown fields never reach shader uniforms. */
export function normalizeToon(raw: unknown): ToonConfig {
  const o = object(raw),
    b = object(o.bloom);
  const light = rgb(o.light, -Infinity, Infinity);
  const out: ToonConfig = {
    ramps: file(o.ramps),
    light: light && light.some((n) => n !== 0) ? light : [-0.33, 0.62, 0.71],
    ambient: number(o.ambient, 0.03),
    bloom: { threshold: number(b.threshold, 0.72, 0, 10), strength: number(b.strength, 0.55, 0, 10), radius: number(b.radius, 0.55) },
    materials: Object.create(null) as Record<string, ToonMaterialConfig>,
  };
  const entries = { ...object(o.materials) };
  const defaults = Symbol('default'),
    hairParts = Symbol('hairParts');
  const all: [string | symbol, unknown][] = [...Object.entries(entries), [defaults, o.default], [hairParts, o.hairParts]];
  for (const [name, value] of all) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) continue;
    const m = object(value),
      ol = object(m.outline),
      oc = rgb(ol.color);
    const entry: ToonMaterialConfig = {
      preset: m.preset === 'flat' || m.preset === 'hide' ? m.preset : 'lit',
      ramp: ramp(m.ramp),
      map: file(m.map),
      multi: m.multi === 'orm' ? 'orm' : file(m.multi),
      metalRamp: m.metalRamp === 'metal' || m.metalRamp === 'metal_plain' ? m.metalRamp : undefined,
      normalScale: number(m.normalScale, 0, 0, 4),
      color: rgb(m.color),
      solidColor: m.solidColor === true,
      saturation: number(m.saturation, 1),
      detail: number(m.detail, 0),
      detailMean: number(m.detailMean, 0.5, 0.000001, 1),
      emission: rgb(m.emission) ?? [0, 0, 0],
      emissionAmount: number(m.emissionAmount, 0, 0, 10),
      rim: number(m.rim, 0, 0, 10),
      angelRing: rgb(m.angelRing),
      alpha: m.alpha === 'none' ? 'none' : 'map',
      outline: oc
        ? {
            color: oc,
            albedoMix: number(ol.albedoMix, 0),
            scale: number(ol.scale, 3.5, 0, 20),
            ...(ol.weight === 'shell' ? { weight: 'shell' as const } : {}),
          }
        : undefined,
    };
    for (const key of Object.keys(entry)) {
      if (entry[key as keyof ToonMaterialConfig] === undefined) delete (entry as unknown as Record<string, unknown>)[key];
    }
    if (name === defaults) out.default = entry;
    else if (name === hairParts) out.hairParts = entry;
    else out.materials[name as string] = entry;
  }
  return out;
}
const color = (v: RGB): Color => new Color().setRGB(...v, SRGBColorSpace);

const ACES_IN = [0.59719, 0.35458, 0.04823, 0.076, 0.90834, 0.01566, 0.0284, 0.13383, 0.83777] as const;
const ACES_OUT = [1.60475, -0.53108, -0.07367, -0.10208, 1.10813, -0.00605, -0.00327, -0.07276, 1.07602] as const;
const acesRRT = (v: number): number => (v * (v + 0.0245786) - 0.000090537) / (v * (0.983729 * v + 0.432951) + 0.238081);

/**
 * Scene-space (pre-tone-map) grey value whose ACES display value is `display`.
 *
 * UnrealBloomPass thresholds scene space, but toon bloom thresholds are authored in display
 * space (Blender's glare threshold on the Standard-view image). With the inverse-tone toon
 * material active, convert back through three r186's forward ACES: scale by exposure / 0.6,
 * the input matrix, the RRT/ODT fit and the output matrix. Grey stays grey (both matrices have
 * rows summing to 1), so a scalar bisection on [0, 20] is enough. 40 steps, current exposure.
 */
export function acesSceneThreshold(display: number, exposure: number): number {
  const target = Math.min(1, Math.max(0, display));
  const e = exposure > 0 ? exposure : 1e-4;
  const forward = (x: number): number => {
    const v = (x * e) / 0.6;
    const fit = [0, 1, 2].map((row) => {
      const r = ACES_IN[row * 3]! * v + ACES_IN[row * 3 + 1]! * v + ACES_IN[row * 3 + 2]! * v;
      return Math.min(1, Math.max(0, acesRRT(r)));
    });
    // Grey: the first output channel is the display value of the (near) equal RGB triple.
    const out = ACES_OUT[0]! * fit[0]! + ACES_OUT[1]! * fit[1]! + ACES_OUT[2]! * fit[2]!;
    return Math.min(1, Math.max(0, out));
  };
  let lo = 0,
    hi = 20;
  if (forward(hi) < target) return hi;
  for (let i = 0; i < 40; i++) {
    const mid = (lo + hi) / 2;
    if (forward(mid) < target) lo = mid;
    else hi = mid;
  }
  return (lo + hi) / 2;
}

/**
 * Per-vertex hull weight for `outline.weight: "shell"`.
 *
 * The face material also covers lashes and mouth/eye pieces, which must not get an inverted
 * hull. Vertices are welded by position (so UV seams do not split islands) and split into
 * connected components; every component except the largest (the skin shell) gets 0. Border
 * vertices of the shell (welded edges used by one triangle) get 0, their one-ring gets 0.5,
 * and the interior gets 1, so the jaw/cheek silhouette extrudes but eye/mouth openings do not.
 */
export function computeOutlineWeights(geometry: BufferGeometry): Float32Array {
  const position = geometry.getAttribute('position');
  const count = position?.count ?? 0;
  const weights = new Float32Array(count);
  if (!position || count === 0) return weights;
  // Weld positions: 1e-5 local units, enough to join duplicated seam vertices without merging detail.
  const weldIds = new Int32Array(count);
  const welded = new Map<string, number>();
  for (let i = 0; i < count; i++) {
    const key = `${Math.round(position.getX(i) * 1e5)},${Math.round(position.getY(i) * 1e5)},${Math.round(position.getZ(i) * 1e5)}`;
    let id = welded.get(key);
    if (id === undefined) {
      id = welded.size;
      welded.set(key, id);
    }
    weldIds[i] = id;
  }
  const weldedCount = welded.size;
  const index = geometry.getIndex();
  const triangleCount = Math.floor((index?.count ?? count) / 3);
  const triangles: number[] = [];
  for (let t = 0; t < triangleCount; t++) {
    const a = weldIds[index ? index.getX(t * 3) : t * 3]!;
    const b = weldIds[index ? index.getX(t * 3 + 1) : t * 3 + 1]!;
    const c = weldIds[index ? index.getX(t * 3 + 2) : t * 3 + 2]!;
    if (a === b || b === c || a === c) continue;
    triangles.push(a, b, c);
  }
  const parent = new Int32Array(weldedCount);
  for (let i = 0; i < weldedCount; i++) parent[i] = i;
  const find = (v: number): number => {
    let root = v;
    while (parent[root] !== root) root = parent[root]!;
    for (let n = v; parent[n] !== root; ) {
      const next = parent[n]!;
      parent[n] = root;
      n = next;
    }
    return root;
  };
  for (let t = 0; t < triangles.length; t += 3) {
    const a = find(triangles[t]!),
      b = find(triangles[t + 1]!),
      c = find(triangles[t + 2]!);
    if (b !== a) parent[b] = a;
    if (c !== a) parent[c] = a;
  }
  const triangleCounts = new Int32Array(weldedCount);
  for (let t = 0; t < triangles.length; t += 3) triangleCounts[find(triangles[t]!)]!++;
  let largest = -1,
    largestCount = 0;
  for (let i = 0; i < weldedCount; i++) {
    if (find(i) === i && triangleCounts[i]! > largestCount) {
      largest = i;
      largestCount = triangleCounts[i]!;
    }
  }
  if (largest < 0) return weights;
  const inLargest = new Uint8Array(weldedCount);
  for (let i = 0; i < weldedCount; i++) inLargest[i] = find(i) === largest ? 1 : 0;
  // Welded edges of the shell; used exactly once means an open border (eye/mouth/lash rim).
  const edges = new Map<number, number>();
  const edge = (a: number, b: number): number => (a < b ? a * weldedCount + b : b * weldedCount + a);
  const largestTriangles: number[] = [];
  for (let t = 0; t < triangles.length; t += 3) {
    const a = triangles[t]!,
      b = triangles[t + 1]!,
      c = triangles[t + 2]!;
    if (!inLargest[a]) continue;
    largestTriangles.push(a, b, c);
    for (const [x, y] of [
      [a, b],
      [b, c],
      [c, a],
    ] as const) {
      const key = edge(x, y);
      edges.set(key, (edges.get(key) ?? 0) + 1);
    }
  }
  const border = new Uint8Array(weldedCount);
  for (const [key, uses] of edges) {
    if (uses !== 1) continue;
    border[Math.floor(key / weldedCount)] = 1;
    border[key % weldedCount] = 1;
  }
  const ring = new Uint8Array(weldedCount);
  for (let t = 0; t < largestTriangles.length; t += 3) {
    const a = largestTriangles[t]!,
      b = largestTriangles[t + 1]!,
      c = largestTriangles[t + 2]!;
    if (border[a]) ring[b] = ring[c] = 1;
    if (border[b]) ring[a] = ring[c] = 1;
    if (border[c]) ring[a] = ring[b] = 1;
  }
  for (let i = 0; i < count; i++) {
    const id = weldIds[i]!;
    weights[i] = !inLargest[id] || border[id] ? 0 : ring[id] ? 0.5 : 1;
  }
  return weights;
}

export interface ToonHandle {
  config: ToonConfig;
  update(): void;
  dispose(): void;
}
export interface ToonOptions extends LoadOptions {
  baseUrl?: string;
  modelUrl?: string;
  manifestUrl?: string;
  toonFallback?: ToonSource;
}

/** Prepare every texture/material before mutating the scene, so a failed pack keeps PBR intact. */
export async function applyToon(model: Object3D, raw: unknown, opts: ToonOptions = {}): Promise<ToonHandle | null> {
  const config = normalizeToon(raw);
  const owned = new Set<Texture>();
  const created = new Set<Material>();
  const outlines: { source: Mesh; hull: Mesh }[] = [];
  const children = new Set<ToonHandle>();
  const originals = new Map<Mesh, Material | Material[]>();
  const cache = new Map<string, Promise<Texture>>();
  const restorers: (() => void)[] = [];
  let toneRenderer: WebGLRenderer | null = null;
  let toneScene: Scene | null = null;
  // The bloom pipeline's OutputPass tone-maps the whole frame; invert ACES only for toon pixels.
  // AgX/Neutral have no closed-form inverse, so they keep the toneMapped=false output.
  const syncTone = (): void => {
    if (!toneRenderer || !toneScene) return;
    const inverse =
      toneScene.userData.toonInverseTone === true && toneRenderer.toneMapping === ACESFilmicToneMapping ? 1 : 0;
    const exposure = toneRenderer.toneMappingExposure;
    for (const m of created) {
      const uniforms = (m as ShaderMaterial).uniforms;
      if (!m.userData.engageToon || !uniforms?.inverseTone || !uniforms.toneExposure) continue;
      uniforms.inverseTone.value = inverse;
      uniforms.toneExposure.value = exposure;
    }
  };
  // Capture the renderer/scene without touching stage.ts: every toon draw refreshes them first.
  const hookTone = (target: Mesh): void => {
    const previous = target.onBeforeRender;
    target.onBeforeRender = (renderer, scene, camera, geometry, material, group) => {
      toneRenderer = renderer;
      toneScene = scene;
      syncTone();
      previous.call(target, renderer, scene, camera, geometry, material, group);
    };
    restorers.push(() => {
      target.onBeforeRender = previous;
    });
  };
  let resolvedBase = opts.baseUrl;
  const load = (name: string, data = false): Promise<Texture> => {
    const key = `${data}:${name}`;
    if (!cache.has(key))
      cache.set(
        key,
        (async () => {
          const base = new URL(resolvedBase!);
          const res = await fetchOk(new URL(name, base).href, opts.fetchInit, opts.signal);
          const bitmap = await createImageBitmap(await res.blob(), {
            imageOrientation: 'none',
            premultiplyAlpha: 'none',
            colorSpaceConversion: 'none',
          });
          const t = new Texture(bitmap);
          t.flipY = false;
          t.colorSpace = data ? NoColorSpace : SRGBColorSpace;
          t.minFilter = t.magFilter = LinearFilter;
          t.wrapS = t.wrapT = ClampToEdgeWrapping;
          t.generateMipmaps = false;
          t.needsUpdate = true;
          owned.add(t);
          return t;
        })(),
      );
    return cache.get(key)!;
  };
  try {
    resolvedBase = new URL(opts.baseUrl ?? './', globalThis.location?.href ?? 'http://localhost/').href;
    const names = [...Object.values(config.materials), config.default, config.hairParts].filter((m): m is ToonMaterialConfig => !!m);
    await Promise.all([
      ...(config.ramps ? [load(config.ramps)] : []),
      ...names.flatMap((m) => [m.map ? load(m.map) : null, m.multi && m.multi !== 'orm' ? load(m.multi, true) : null]),
    ]);
    const white = new DataTexture(new Uint8Array([255, 255, 255, 255]), 1, 1);
    white.needsUpdate = true;
    owned.add(white);
    const ramps = config.ramps ? await load(config.ramps) : white;
    const meshes: Mesh[] = [];
    model.traverse((o) => {
      if ((o as Mesh).isMesh && !o.userData.toonOutline) meshes.push(o as Mesh);
    });
    const material = async (src: Material, cfg: ToonMaterialConfig, mesh: Mesh, outline = false): Promise<ShaderMaterial> => {
      const original = src as MeshStandardMaterial;
      const map = cfg.map ? await load(cfg.map) : original.map;
      const multi = cfg.multi === 'orm' ? original.metalnessMap : cfg.multi ? await load(cfg.multi, true) : null;
      const normal = cfg.normalScale > 0 ? original.normalMap : null;
      map?.updateMatrix();
      multi?.updateMatrix();
      normal?.updateMatrix();
      const defines: Record<string, string | number> = {};
      if (map) defines.HAS_MAP = '';
      if (cfg.alpha === 'map') defines.MAP_ALPHA = '';
      if (normal) defines.HAS_NORMAL = '';
      if (multi) defines.HAS_MULTI = '';
      if (multi && cfg.multi === 'orm') defines.ORM_MULTI = '';
      if (cfg.metalRamp && multi) defines.METAL_RAMP = '';
      if (cfg.preset === 'flat') defines.FLAT_TOON = '';
      if (cfg.solidColor) defines.SOLID_COLOR = '';
      if (cfg.angelRing) defines.ANGEL_RING = '';
      if (outline) defines.OUTLINE = '';
      const attr = mesh.geometry.getAttribute('color');
      let max = 0;
      if (outline && attr) {
        for (let i = 0; i < attr.count; i++) max = Math.max(max, attr.getX(i));
        if (max > 0) defines.OUTLINE_COLOR = '';
      }
      if (outline && cfg.outline?.weight === 'shell') {
        // Shared with the hull: add once, even across wardrobe re-applies.
        if (!mesh.geometry.getAttribute('outlineWeight')) {
          mesh.geometry.setAttribute('outlineWeight', new Float32BufferAttribute(computeOutlineWeights(mesh.geometry), 1));
        }
        defines.OUTLINE_WEIGHT = '';
      }
      const m = new ShaderMaterial({
        name: src.name,
        defines,
        vertexShader: toonVertex,
        fragmentShader: toonFragment,
        uniforms: {
          albedoMap: { value: map ?? white },
          multiMap: { value: multi ?? white },
          ramps: { value: ramps },
          normalMap: { value: normal ?? white },
          multiTransform: { value: multi?.matrix.clone() ?? new Matrix3() },
          mapTransform: { value: map?.matrix.clone() ?? new Matrix3() },
          normalTransform: { value: normal?.matrix.clone() ?? new Matrix3() },
          normalScale: { value: (original.normalScale?.clone() ?? new Vector2(1, 1)).multiplyScalar(cfg.normalScale) },
          tint: { value: cfg.color ? color(cfg.color) : (original.color?.clone() ?? new Color(1, 1, 1)) },
          lightDirection: { value: new Vector3(...config.light).normalize() },
          ambient: { value: config.ambient },
          rowV: { value: (RAMP_ROWS[cfg.ramp] + 0.5) / 8 },
          metalRowV: { value: (RAMP_ROWS[cfg.metalRamp ?? 'metal'] + 0.5) / 8 },
          saturation: { value: cfg.saturation },
          detail: { value: cfg.detail },
          detailMean: { value: cfg.detailMean },
          emission: { value: color(cfg.emission) },
          emissionAmount: { value: cfg.emissionAmount },
          ringColor: { value: color(cfg.angelRing ?? [0, 0, 0]) },
          rim: { value: cfg.rim },
          rimColor: { value: color([0.212, 0.878, 1]) },
          opacity: { value: src.opacity },
          alphaTest: { value: src.alphaTest },
          outlineColor: { value: color(cfg.outline?.color ?? [0, 0, 0]) },
          albedoMix: { value: cfg.outline?.albedoMix ?? 0 },
          outlineWidth: { value: outline ? (0.0009 * (cfg.outline?.scale ?? 3.5)) / 3.5 : 0 },
          outlineMax: { value: max || 1 },
          inverseTone: { value: 0 },
          toneExposure: { value: 1 },
        },
        side: outline ? BackSide : src.side,
        transparent: src.transparent,
        depthWrite: src.depthWrite,
        depthTest: src.depthTest,
        blending: src.blending,
        blendSrc: src.blendSrc,
        blendDst: src.blendDst,
        blendEquation: src.blendEquation,
        blendSrcAlpha: src.blendSrcAlpha,
        blendDstAlpha: src.blendDstAlpha,
        blendEquationAlpha: src.blendEquationAlpha,
        blendColor: src.blendColor.clone(),
        blendAlpha: src.blendAlpha,
        alphaTest: src.alphaTest,
        alphaToCoverage: src.alphaToCoverage,
        premultipliedAlpha: src.premultipliedAlpha,
        toneMapped: false,
      });
      m.userData.engageToon = true;
      m.visible = src.visible && cfg.preset !== 'hide';
      created.add(m);
      return m;
    };
    const entryFor = (m: Material): ToonMaterialConfig | undefined => config.materials[m.name] ?? (m.visible ? config.default : undefined);
    const replacements: { mesh: Mesh; mats: Material[]; hull?: Mesh }[] = [];
    for (const mesh of meshes) {
      const src = Array.isArray(mesh.material) ? mesh.material : [mesh.material];
      if (!mesh.visible || !src.some(entryFor)) continue;
      const mats = await Promise.all(src.map((m) => (entryFor(m) ? material(m, entryFor(m)!, mesh) : m)));
      let hull: Mesh | undefined;
      if (src.some((m) => entryFor(m)?.outline)) {
        const hullMats = await Promise.all(
          src.map(async (m) => {
            const cfg = entryFor(m);
            if (cfg?.outline && cfg.preset !== 'hide') return material(m, cfg, mesh, true);
            const invisible = new ShaderMaterial({ visible: false });
            created.add(invisible);
            return invisible;
          }),
        );
        const hullMaterial = Array.isArray(mesh.material) ? hullMats : hullMats[0]!;
        hull = (mesh as SkinnedMesh).isSkinnedMesh ? new SkinnedMesh(mesh.geometry, hullMaterial) : new Mesh(mesh.geometry, hullMaterial);
        if ((hull as SkinnedMesh).isSkinnedMesh) {
          const s = mesh as SkinnedMesh,
            h = hull as SkinnedMesh;
          h.bindMode = s.bindMode;
          h.bind(s.skeleton, s.bindMatrix);
          h.bindMatrixInverse.copy(s.bindMatrixInverse);
        }
        hull.userData.toonOutline = true;
        hull.name = `${mesh.name}_toon_outline`;
        hull.morphTargetInfluences = mesh.morphTargetInfluences;
        hull.morphTargetDictionary = mesh.morphTargetDictionary;
        hull.frustumCulled = false;
        outlines.push({ source: mesh, hull });
      }
      replacements.push({ mesh, mats, hull });
    }
    for (const { mesh, mats, hull } of replacements) {
      originals.set(mesh, mesh.material);
      mesh.material = Array.isArray(mesh.material) ? mats : mats[0]!;
      hookTone(mesh);
      if (hull) {
        hookTone(hull);
        mesh.parent?.add(hull);
      }
    }
    const update = (): void => {
      syncTone();
      for (const child of children) child.update();
      for (const { source, hull } of outlines) {
        if (hull.parent !== source.parent) source.parent?.add(hull);
        if ((source as SkinnedMesh).isSkinnedMesh && (hull as SkinnedMesh).skeleton !== (source as SkinnedMesh).skeleton) {
          (hull as SkinnedMesh).bind((source as SkinnedMesh).skeleton, (source as SkinnedMesh).bindMatrix);
        }
        hull.visible = source.visible;
        hull.position.copy(source.position);
        hull.quaternion.copy(source.quaternion);
        hull.scale.copy(source.scale);
        hull.matrixAutoUpdate = source.matrixAutoUpdate;
        hull.matrix.copy(source.matrix);
        hull.renderOrder = source.renderOrder;
      }
    };
    update();
    const handle: ToonHandle = {
      config,
      update,
      dispose() {
        bindings.delete(model);
        for (const child of children) child.dispose();
        children.clear();
        for (const restore of restorers) restore();
        for (const [mesh, mat] of originals) mesh.material = mat;
        for (const { hull } of outlines) hull.removeFromParent();
        for (const m of created) m.dispose();
        for (const t of owned) {
          (t.image as ImageBitmap)?.close?.();
          t.dispose();
        }
      },
    };
    bindings.set(model, { handle, source: { config, baseUrl: resolvedBase, modelUrl: absoluteUrl(opts.modelUrl), manifestUrl: absoluteUrl(opts.manifestUrl) }, options: { ...opts, baseUrl: resolvedBase }, children });
    return handle;
  } catch (e) {
    for (const restore of restorers) restore();
    for (const [mesh, mat] of originals) mesh.material = mat;
    for (const { hull } of outlines) hull.removeFromParent();
    // Wait for pending loads before releasing their bitmaps as well.
    await Promise.allSettled(cache.values());
    for (const m of created) m.dispose();
    for (const t of owned) {
      (t.image as ImageBitmap)?.close?.();
      t.dispose();
    }
    console.warn('[lucina-stage] toon setup failed, keeping PBR materials:', e);
    return null;
  }
}

/** Manifest opt-in, with an explicit standard-material escape hatch. */
export function applyManifestToon(
  model: Object3D,
  manifest: Pick<NormalizedManifest, 'material' | 'toon'>,
  opts: ToonOptions & { material?: 'engage-toon' | 'standard' } = {},
): Promise<ToonHandle | null> {
  if (opts.material === 'standard') return Promise.resolve(null);
  const fallback = opts.toonFallback;
  const url = absoluteUrl(opts.manifestUrl);
  // The starting manifest (or the wardrobe's rig manifest) reused for this model is not its own toon source.
  const reused = !!fallback && (!url || url === fallback.manifestUrl || url === fallback.rigManifestUrl);
  if (!reused) {
    if (manifest.material && manifest.material !== 'engage-toon') return Promise.resolve(null);
    if (manifest.toon !== undefined) {
      return manifest.material === 'engage-toon' || fallback ? applyToon(model, manifest.toon, opts) : Promise.resolve(null);
    }
  }
  if (!fallback) return Promise.resolve(null);
  // File overrides only fit the GLB they were authored for, loaded with the manifest that named them.
  const isBaseModel =
    !!fallback.modelUrl && absoluteUrl(opts.modelUrl) === fallback.modelUrl && (!url || url === fallback.manifestUrl);
  const config = isBaseModel ? fallback.config : inheritedToon(model, fallback.config);
  return applyToon(model, config, { ...opts, baseUrl: fallback.baseUrl });
}

export interface ToonSource {
  /** Original model and manifest identities, independent of the texture asset URL. */
  modelUrl?: string;
  manifestUrl?: string;
  /** A rig-only manifest that swaps reuse without it naming their toon look (wardrobe `manifestUrl`). */
  rigManifestUrl?: string;
  config: ToonConfig;
  baseUrl?: string;
}
interface ToonBinding {
  handle: ToonHandle;
  source: ToonSource;
  options: ToonOptions;
  children: Set<ToonHandle>;
}
const bindings = new WeakMap<Object3D, ToonBinding>();

function absoluteUrl(url: string | undefined): string | undefined {
  return url ? new URL(url, globalThis.location?.href ?? 'http://localhost/').href : undefined;
}

/** Only named material overrides belong to the base GLB; shared style entries stay intact. */
export function inheritedToon(model: Object3D, config: ToonConfig): ToonConfig {
  const orm = new Set<string>();
  model.traverse(o => {
    const mesh = o as Mesh;
    if (!mesh.isMesh || o.userData.toonOutline) return;
    for (const mat of Array.isArray(mesh.material) ? mesh.material : [mesh.material]) {
      if ((mat as MeshStandardMaterial).metalnessMap) orm.add(mat.name);
    }
  });
  const materials = Object.fromEntries(Object.entries(config.materials).map(([name, material]) => {
    const { map: _map, multi, ...entry } = material;
    return [name, { ...entry, ...(multi === 'orm' || (multi && orm.has(name)) ? { multi: 'orm' } : {}) }];
  }));
  return { ...config, materials };
}

/** Preserve the full starting look so swaps back to its GLB recover its file overrides. */
export function toonFallbackFor(model: Object3D, rigManifestUrl?: string): ToonSource | undefined {
  const source = bindings.get(model)?.source;
  return source && rigManifestUrl ? { ...source, rigManifestUrl: absoluteUrl(rigManifestUrl) } : source;
}

/** Prepare before HairRig re-parents the part; its hulls follow the rebound source meshes. */
export async function applyToonHairPart(model: Object3D, part: Object3D): Promise<ToonHandle | null> {
  const parent = bindings.get(model);
  if (!parent) return null;
  const cfg = parent.source.config;
  const fallback = cfg.hairParts ?? cfg.materials['Hair.001'] ?? cfg.materials.Hair;
  const materials: Record<string, ToonMaterialConfig> = Object.create(null);
  part.traverse((o) => {
    const mesh = o as Mesh;
    if (!mesh.isMesh) return;
    for (const mat of Array.isArray(mesh.material) ? mesh.material : [mesh.material]) {
      if (cfg.materials[mat.name]) materials[mat.name] = cfg.materials[mat.name]!;
    }
  });
  const handle = await applyToon(part, { ...cfg, materials, default: fallback, hairParts: undefined }, parent.options);
  if (handle) {
    parent.children.add(handle);
    const dispose = handle.dispose.bind(handle);
    handle.dispose = () => {
      parent.children.delete(handle);
      dispose();
    };
  }
  return handle;
}

export function toonHandleFor(model: Object3D): ToonHandle | undefined {
  return bindings.get(model)?.handle;
}

/** Hair's natural tint is restored on null; toon shaders need no standard-map injection. */
export function tintToonMaterial(material: Material, hex: string | null): boolean {
  const m = material as ShaderMaterial;
  const tint = m.userData.engageToon && m.uniforms.tint?.value;
  if (!(tint instanceof Color)) return false;
  const base = (m.userData.toonBaseTint ??= tint.clone()) as Color;
  if (hex) tint.set(hex);
  else tint.copy(base);
  return true;
}
