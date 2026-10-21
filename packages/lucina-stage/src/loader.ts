/**
 * Loads the (private, auth-gated) GLB with fetch() so `fetchInit` (cookies / Authorization)
 * applies, then parses it with GLTFLoader + meshopt.
 */
import {
  DoubleSide,
  Mesh,
  type Material,
  type MeshStandardMaterial,
  type Object3D,
  type Texture,
} from 'three';
import { GLTFLoader, type GLTF } from 'three/addons/loaders/GLTFLoader.js';
import { MeshoptDecoder } from 'three/addons/libs/meshopt_decoder.module.js';
import { normalizeManifest, type NormalizedManifest } from './manifest';

export interface LoadOptions {
  fetchInit?: RequestInit;
  signal?: AbortSignal;
}

export async function fetchOk(url: string, init: RequestInit | undefined, signal?: AbortSignal): Promise<Response> {
  const res = await fetch(url, { ...init, ...(signal ? { signal } : {}) });
  if (!res.ok) throw new Error(`GET ${url} failed: HTTP ${res.status}`);
  return res;
}

export async function loadManifest(url: string | undefined, opts: LoadOptions = {}): Promise<NormalizedManifest> {
  if (!url) return normalizeManifest(null);
  const res = await fetchOk(url, opts.fetchInit, opts.signal);
  return normalizeManifest(await res.json());
}

/** Reads `extensionsUsed` from a GLB (or glTF JSON) without parsing the whole asset. */
export function gltfExtensions(buf: ArrayBuffer): string[] {
  try {
    const dv = new DataView(buf);
    let json: unknown;
    if (buf.byteLength >= 20 && dv.getUint32(0, true) === 0x46546c67) {
      const len = dv.getUint32(12, true);
      const type = dv.getUint32(16, true);
      if (type !== 0x4e4f534a) return [];
      json = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 20, len)));
    } else {
      json = JSON.parse(new TextDecoder().decode(new Uint8Array(buf)));
    }
    const used = (json as { extensionsUsed?: unknown }).extensionsUsed;
    return Array.isArray(used) ? used.filter((e): e is string => typeof e === 'string') : [];
  } catch {
    return [];
  }
}

function headersObject(init?: RequestInit): Record<string, string> {
  const out: Record<string, string> = {};
  if (!init?.headers) return out;
  new Headers(init.headers).forEach((v, k) => {
    out[k] = v;
  });
  return out;
}

export interface LoadedModel {
  gltf: GLTF;
  extensions: string[];
}

export async function loadModel(url: string, opts: LoadOptions = {}): Promise<LoadedModel> {
  const res = await fetchOk(url, opts.fetchInit, opts.signal);
  const buf = await res.arrayBuffer();
  const extensions = gltfExtensions(buf);
  const loader = new GLTFLoader();
  loader.setMeshoptDecoder(MeshoptDecoder);
  // External resources (only for .gltf with separate files) get the same auth.
  loader.setRequestHeader(headersObject(opts.fetchInit));
  loader.setWithCredentials(opts.fetchInit?.credentials === 'include');
  // Draco is deliberately unsupported: three's DRACOLoader pulls ~1.8 MB of decoder into any bundle
  // that imports it. The Lucina GLB uses meshopt (EXT_meshopt_compression) instead.
  if (extensions.includes('KHR_draco_mesh_compression')) {
    throw new Error('This GLB uses Draco compression; re-export it with meshopt (gltf-transform meshopt).');
  }
  const base = url.slice(0, url.lastIndexOf('/') + 1);
  const gltf = await loader.parseAsync(buf, base);
  return { gltf, extensions };
}

const ALPHA_CUTOUT = /hair|lash|brow|matsuge|mayu|kami|髪|まつ|睫|眉|bang|fringe|ahoge|tail/i;
const OVERLAY = /tear|blush|shadow|cheek|涙|頬|照れ|exp|effect|highlight|hl/i;

function isStandard(m: Material): m is MeshStandardMaterial {
  return (m as MeshStandardMaterial).isMeshStandardMaterial === true;
}

/**
 * Makes GLB materials behave for an anime model: double-sided, cut-out alpha for hair/lashes
 * (no sorting artifacts), non-metallic (metal without a reflection probe renders black),
 * and drops all-black vertex colours that would black out faces.
 */
export function fixMaterials(root: Object3D): void {
  const done = new Set<Material>();
  root.traverse((o) => {
    const mesh = o as Mesh;
    if (!mesh.isMesh) return;
    const mats = Array.isArray(mesh.material) ? mesh.material : [mesh.material];
    const colors = mesh.geometry.getAttribute('color');
    for (const m of mats) {
      if (!m || done.has(m)) continue;
      done.add(m);
      const label = `${m.name} ${mesh.name}`;
      m.side = DoubleSide;
      if (isStandard(m)) {
        if (!m.metalnessMap) m.metalness = Math.min(m.metalness, 0.05);
        if (!m.roughnessMap) m.roughness = Math.max(m.roughness, 0.55);
        if (m.vertexColors && colors && averageColor(colors) < 0.02) m.vertexColors = false;
      }
      const map = (m as { map?: Texture | null }).map;
      const blended = m.transparent;
      if (blended && ALPHA_CUTOUT.test(label) && !OVERLAY.test(label)) {
        // Blended hair/lashes sort badly; a cut-out with alpha-to-coverage is stable.
        m.transparent = false;
        m.alphaTest = Math.max(m.alphaTest, 0.5);
        m.depthWrite = true;
        m.alphaToCoverage = true;
      } else if (blended) {
        m.depthWrite = false;
      } else if (map && m.alphaTest === 0) {
        // MMD always honours texture alpha, but conversions often export alphaMode OPAQUE:
        // without a cut-out, hair tips, lashes and eye overlays render as solid squares.
        m.alphaTest = 0.5;
        m.alphaToCoverage = true;
      }
      if (map) map.anisotropy = 4;
      m.needsUpdate = true;
    }
    mesh.frustumCulled = false; // skinned bounds are bind-pose; avoid popping
  });
}

function averageColor(attr: { count: number; getX(i: number): number; getY(i: number): number; getZ(i: number): number }): number {
  const n = Math.min(attr.count, 2000);
  if (!n) return 1;
  const step = Math.max(1, Math.floor(attr.count / n));
  let s = 0;
  let c = 0;
  for (let i = 0; i < attr.count; i += step) {
    s += attr.getX(i) + attr.getY(i) + attr.getZ(i);
    c += 3;
  }
  return s / c;
}

/** Hides nodes listed by name (exact or sanitised). */
export function hideNodes(root: Object3D, names: readonly string[]): string[] {
  if (!names.length) return [];
  const want = new Set(names.flatMap((n) => [n, n.replace(/\s/g, '_').replace(/[[\]\.:\/]/g, '')]));
  const hidden: string[] = [];
  root.traverse((o) => {
    if (o.name && want.has(o.name)) {
      o.visible = false;
      hidden.push(o.name);
    }
  });
  return hidden;
}
