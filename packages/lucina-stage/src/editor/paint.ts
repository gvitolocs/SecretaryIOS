/**
 * Colours of the editor on the toon materials: iris colour, eye highlight, skin tone, makeup and
 * the hair highlight (angel ring). Hair colour itself goes through the wardrobe's hair tint
 * (hair.ts), so outfit swaps and hair parts keep it.
 *
 * Toon materials (toon.ts) are ShaderMaterials made per model, so the editor patches their shader
 * text at two anchors (declarations before `void main()`, colour work right after the saturation
 * line) instead of changing toon-shaders.ts. Non-toon materials fall back to a colour multiply
 * where that makes sense, and are otherwise left alone.
 *
 * Makeup is four procedural masks drawn into one RGBA texture in the face's UV space (R lipstick,
 * G eyeliner, B eyeshadow, A blush). The masks are rasterised once per model from the face
 * landmarks (face.ts): each face triangle is drawn into its UV footprint, and every texel takes the
 * mask value at its interpolated rest-pose position.
 */
import { Color, DataTexture, LinearFilter, RGBAFormat, ShaderMaterial, Vector3, Vector4, type Material, type Mesh, type Object3D } from 'three';
import type { FaceLandmarks, FacePart, V3 } from './face';
import { hexToHsv, MAKEUP_LAYERS, type Look } from './look';

const ANCHOR_DECL = 'void main() {';
const ANCHOR_COLOR = 'albedo = mix(vec3(lum(albedo)), albedo, saturation);';

/** Materials whose toon tint the skin tone multiplies. */
export const SKIN_MATERIALS = ['FaceSkin', 'BodySkin', 'FaceColor', 'Mouth', 'SkinTinted'];
/** Base iris hue when the texture cannot be read (Lucina's blue). */
export const DEFAULT_IRIS_HSV: [number, number, number] = [218, 0.55, 0.75];
/** The skin tone the base textures are painted in; tones are applied relative to it. */
export const NATURAL_SKIN = '#f6dccb';

function isToon(m: Material | undefined): m is ShaderMaterial {
  return !!m && (m as ShaderMaterial).isShaderMaterial === true && m.userData.engageToon === true;
}

/** Inserts `decl` and `color` code into a toon fragment shader once per key. Returns success. */
export function patchToon(m: ShaderMaterial, key: string, decl: string, color: string, uniforms: Record<string, { value: unknown }>): boolean {
  const done = (m.userData.editorPatches ??= []) as string[];
  if (done.includes(key)) return true;
  const src = m.fragmentShader;
  if (!src.includes(ANCHOR_DECL) || !src.includes(ANCHOR_COLOR)) return false;
  m.fragmentShader = src.replace(ANCHOR_DECL, `${decl}\n${ANCHOR_DECL}`).replace(ANCHOR_COLOR, `${ANCHOR_COLOR}\n${color}`);
  Object.assign(m.uniforms, uniforms);
  done.push(key);
  m.needsUpdate = true;
  return true;
}

const HSV_GLSL = `
vec3 edRgb2hsv(vec3 c) {
  vec4 K = vec4(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
  vec4 p = mix(vec4(c.bg, K.wz), vec4(c.gb, K.xy), step(c.b, c.g));
  vec4 q = mix(vec4(p.xyw, c.r), vec4(c.r, p.yzx), step(p.x, c.r));
  float d = q.x - min(q.w, q.y);
  return vec3(abs(q.z + (q.w - q.y) / (6.0 * d + 1e-10)), d / (q.x + 1e-10), q.x);
}
vec3 edHsv2rgb(vec3 c) {
  vec3 p = abs(fract(c.xxx + vec3(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
  return c.z * mix(vec3(1.0), clamp(p - 1.0, 0.0, 1.0), c.y);
}`;

/** Hue shift (turns), saturation and value multipliers; only coloured texels change (the whites stay). */
const IRIS_DECL = `uniform vec3 edIris;\n${HSV_GLSL}`;
const IRIS_COLOR = `  {
    vec3 hsv = edRgb2hsv(albedo);
    float k = smoothstep(0.08, 0.25, hsv.y);
    vec3 shifted = edHsv2rgb(vec3(fract(hsv.x + edIris.x), clamp(hsv.y * edIris.y, 0.0, 1.0), clamp(hsv.z * edIris.z, 0.0, 1.0)));
    albedo = mix(albedo, shifted, k);
  }`;

const MAKEUP_DECL = `uniform sampler2D edMakeup;
uniform vec4 edMakeupAmount;
uniform vec3 edLip;
uniform vec3 edLiner;
uniform vec3 edShadow;
uniform vec3 edBlush;`;
const MAKEUP_COLOR = `  {
    vec4 mk = texture2D(edMakeup, tUv) * edMakeupAmount;
    float l = lum(albedo);
    albedo = mix(albedo, albedo * edShadow * 1.15, mk.b * 0.85);
    albedo = mix(albedo, edBlush * (0.75 + 0.3 * l), mk.a * 0.55);
    albedo = mix(albedo, edLip * (0.55 + 0.5 * l), mk.r * 0.9);
    albedo = mix(albedo, edLiner, mk.g);
  }`;

function linear(hex: string): Color {
  return new Color(hex); // three converts sRGB hex to the working (linear) colour space
}

// ---------------------------------------------------------------------------------------------
// Makeup masks

export interface MaskOptions {
  size?: number;
}

const smoothstep = (e0: number, e1: number, x: number): number => {
  const t = Math.min(1, Math.max(0, (x - e0) / (e1 - e0)));
  return t * t * (3 - 2 * t);
};

/** The four mask values (0..1) at a rest-pose face position. Pure (unit-tested). */
export function makeupAt(p: V3, lm: FaceLandmarks): [number, number, number, number] {
  const u = lm.u;
  const ell = (c: V3, rx: number, ry: number, rz: number) => Math.hypot((p[0] - c[0]) / rx, (p[1] - c[1]) / ry, (p[2] - c[2]) / rz);
  const front = p[2] > lm.centre[2];
  if (!front) return [0, 0, 0, 0];
  // Lipstick: an ellipse on the lips.
  const dl = ell([lm.mouth[0], lm.mouth[1] - 0.02 * u, lm.mouth[2]], lm.mouthHalfWidth * 0.95, 0.15 * u, 0.6 * u);
  const lip = 1 - smoothstep(0.7, 1.0, dl);
  let liner = 0, shadow = 0, blush = 0;
  const [hx, hy] = lm.eyeHalf;
  for (const [c, side] of [
    [lm.eyeL, Math.sign(lm.eyeL[0] - lm.eyeR[0]) || 1],
    [lm.eyeR, -(Math.sign(lm.eyeL[0] - lm.eyeR[0]) || 1)],
  ] as [V3, number][]) {
    const de = ell(c, hx * 1.12, hy * 1.08, 0.8 * u);
    // Eyeliner: a band along the upper lid, with a small wing past the outer corner.
    const upper = smoothstep(c[1] - hy * 0.15, c[1] + hy * 0.15, p[1]);
    const band = smoothstep(0.92, 1.0, de) * (1 - smoothstep(1.12, 1.25, de)) * upper;
    const wx = (p[0] - c[0]) * side;
    const wing = wx > hx * 0.9 && wx < hx * 1.45 && Math.abs(p[1] - (c[1] + hy * 0.15 + (wx - hx) * 0.45)) < hy * 0.09 ? 1 : 0;
    liner = Math.max(liner, band, wing);
    // Eyeshadow: soft, above the eye, under the brow.
    const ds = ell([c[0] + side * 0.05 * u, c[1] + hy * 0.75, c[2]], hx * 1.45, hy * 0.95, 0.8 * u);
    shadow = Math.max(shadow, (1 - smoothstep(0.35, 1.0, ds)) * smoothstep(0.95, 1.1, de));
    // Blush: on the cheek, below and a little outside the eye.
    const db = ell([c[0] + side * 0.2 * u, c[1] - hy - 0.42 * u, c[2]], 0.45 * u, 0.28 * u, 0.9 * u);
    blush = Math.max(blush, 1 - smoothstep(0.15, 1.0, db));
  }
  return [lip, liner, shadow, blush];
}

/** Rasterises the masks of `parts` into a UV-space RGBA texture. */
export function makeupMask(parts: FacePart[], lm: FaceLandmarks, opts: MaskOptions = {}): DataTexture {
  const size = opts.size ?? 512;
  const data = new Uint8Array(size * size * 4);
  const covered = new Uint8Array(size * size);
  for (const part of parts) {
    if (part.material !== 'FaceSkin' && part.material !== 'FaceColor') continue;
    const g = part.mesh.geometry;
    const pos = g.attributes.position;
    const uv = g.attributes.uv;
    if (!pos || !uv) continue;
    const idx = g.index;
    const tri = idx ? idx.count / 3 : pos.count / 3;
    const at = (k: number) => (idx ? idx.getX(k) : k);
    for (let t = 0; t < tri; t++) {
      const i0 = at(t * 3), i1 = at(t * 3 + 1), i2 = at(t * 3 + 2);
      const ux = [uv.getX(i0) * size, uv.getX(i1) * size, uv.getX(i2) * size];
      const uy = [uv.getY(i0) * size, uv.getY(i1) * size, uv.getY(i2) * size];
      const P: V3[] = [i0, i1, i2].map((i) => [pos.getX(i), pos.getY(i), pos.getZ(i)] as V3);
      const x0 = Math.max(0, Math.floor(Math.min(...ux))), x1 = Math.min(size - 1, Math.ceil(Math.max(...ux)));
      const y0 = Math.max(0, Math.floor(Math.min(...uy))), y1 = Math.min(size - 1, Math.ceil(Math.max(...uy)));
      const den = (uy[1]! - uy[2]!) * (ux[0]! - ux[2]!) + (ux[2]! - ux[1]!) * (uy[0]! - uy[2]!);
      if (Math.abs(den) < 1e-12) continue;
      for (let y = y0; y <= y1; y++) {
        for (let x = x0; x <= x1; x++) {
          const px = x + 0.5, py = y + 0.5;
          const a = ((uy[1]! - uy[2]!) * (px - ux[2]!) + (ux[2]! - ux[1]!) * (py - uy[2]!)) / den;
          const b = ((uy[2]! - uy[0]!) * (px - ux[2]!) + (ux[0]! - ux[2]!) * (py - uy[2]!)) / den;
          const c = 1 - a - b;
          const e = -0.02;
          if (a < e || b < e || c < e) continue;
          const p: V3 = [0, 1, 2].map((k) => a * P[0]![k]! + b * P[1]![k]! + c * P[2]![k]!) as V3;
          const m = makeupAt(p, lm);
          const o = (y * size + x) * 4;
          for (let k = 0; k < 4; k++) data[o + k] = Math.max(data[o + k]!, Math.round(m[k]! * 255));
          covered[y * size + x] = 1;
        }
      }
    }
  }
  // Two texels of padding past the UV islands, so filtering at the seams never darkens.
  for (let pass = 0; pass < 2; pass++) {
    const next = covered.slice();
    for (let y = 0; y < size; y++) {
      for (let x = 0; x < size; x++) {
        if (covered[y * size + x]) continue;
        for (const [dx, dy] of [[1, 0], [-1, 0], [0, 1], [0, -1]]) {
          const nx = x + dx!, ny = y + dy!;
          if (nx < 0 || ny < 0 || nx >= size || ny >= size || !covered[ny * size + nx]) continue;
          const o = (y * size + x) * 4, s = (ny * size + nx) * 4;
          for (let k = 0; k < 4; k++) data[o + k] = Math.max(data[o + k]!, data[s + k]!);
          next[y * size + x] = 1;
        }
      }
    }
    covered.set(next);
  }
  const tex = new DataTexture(data, size, size, RGBAFormat);
  tex.flipY = false;
  tex.minFilter = tex.magFilter = LinearFilter;
  tex.generateMipmaps = false;
  tex.needsUpdate = true;
  tex.name = 'editor-makeup-mask';
  return tex;
}

// ---------------------------------------------------------------------------------------------

export interface PaintTargets {
  iris: ShaderMaterial[];
  highlight: Mesh[];
  skin: { material: Material; base: Color }[];
  makeup: ShaderMaterial[];
  ring: { material: ShaderMaterial; base: Vector3 | Color }[];
}

/** Paint state of one model: patched materials, the mask texture, base colours. */
export class Painter {
  private mask: DataTexture | null = null;
  private irisBase: [number, number, number] = DEFAULT_IRIS_HSV;
  private readonly uniforms = {
    edIris: { value: new Vector3(0, 1, 1) },
    edMakeup: { value: null as DataTexture | null },
    edMakeupAmount: { value: new Vector4(0, 0, 0, 0) },
    edLip: { value: new Color() },
    edLiner: { value: new Color() },
    edShadow: { value: new Color() },
    edBlush: { value: new Color() },
  };
  private readonly skinBase = new Map<Material, Color>();
  private readonly ringBase = new Map<ShaderMaterial, Color>();

  constructor(
    readonly model: Object3D,
    parts: FacePart[],
    lm: FaceLandmarks | null,
  ) {
    if (lm && parts.some((p) => p.material === 'FaceSkin' || p.material === 'FaceColor')) {
      try {
        this.mask = makeupMask(parts, lm);
        this.uniforms.edMakeup.value = this.mask;
      } catch (e) {
        console.warn('[lucina-stage] makeup masks not built:', e);
      }
    }
    this.patchAll();
  }

  /** (Re)patches the model's materials; call again after parts or hair were added. */
  patchAll(): void {
    const seen = new Set<Material>();
    this.model.traverse((o) => {
      const mesh = o as Mesh;
      if (!mesh.isMesh) return;
      for (const m of Array.isArray(mesh.material) ? mesh.material : [mesh.material]) {
        if (!m || seen.has(m)) continue;
        seen.add(m);
        const name = m.name;
        if (isToon(m) && (name === 'EyeL' || name === 'EyeR') && !mesh.userData.toonOutline) {
          patchToon(m, 'iris', IRIS_DECL, IRIS_COLOR, { edIris: this.uniforms.edIris });
        }
        if (isToon(m) && this.mask && (name === 'FaceSkin' || name === 'FaceColor') && !mesh.userData.toonOutline) {
          const { edIris: _i, ...mk } = this.uniforms;
          patchToon(m, 'makeup', MAKEUP_DECL, MAKEUP_COLOR, mk);
        }
        if (SKIN_MATERIALS.includes(name) && !mesh.userData.toonOutline && !this.skinBase.has(m)) {
          const c = isToon(m) ? (m.uniforms.tint?.value as Color | undefined) : (m as Material & { color?: Color }).color;
          if (c instanceof Color) this.skinBase.set(m, c.clone());
        }
        if (isToon(m) && m.defines?.ANGEL_RING !== undefined && m.uniforms.ringColor?.value instanceof Color && !this.ringBase.has(m)) {
          this.ringBase.set(m, (m.uniforms.ringColor.value as Color).clone());
        }
      }
    });
  }

  /** The iris texture's own colour (hue in degrees, s, v), measured by the caller if it can. */
  setIrisBase(hsv: [number, number, number]): void {
    this.irisBase = hsv;
  }

  apply(look: Pick<Look, 'eyes' | 'skin' | 'makeup' | 'hair'>): void {
    // Iris: rotate the hue to the target, scale saturation/value toward it.
    const iris = this.uniforms.edIris.value;
    if (look.eyes.iris) {
      const [h, s, v] = hexToHsv(look.eyes.iris);
      const [bh, bs, bv] = this.irisBase;
      iris.set((((h - bh) % 360) + 360) % 360 / 360, Math.min(2.5, Math.max(0.2, s / Math.max(bs, 0.05))), Math.min(1.8, Math.max(0.3, v / Math.max(bv, 0.05))));
    } else iris.set(0, 1, 1);
    // Highlight on/off.
    this.model.traverse((o) => {
      const mesh = o as Mesh;
      if (!mesh.isMesh) return;
      const mats = Array.isArray(mesh.material) ? mesh.material : [mesh.material];
      if (mats.some((m) => m?.name === 'EyeHL')) {
        mesh.userData.editorHighlightVisible ??= mesh.visible;
        mesh.visible = look.eyes.highlight ? (mesh.userData.editorHighlightVisible as boolean) : false;
      }
    });
    // Skin tone: relative to the natural tone the textures are painted in.
    const ratio = look.skin.tone ? linear(look.skin.tone).clone() : null;
    if (ratio) {
      const nat = linear(NATURAL_SKIN);
      ratio.setRGB(clampR(ratio.r / nat.r), clampR(ratio.g / nat.g), clampR(ratio.b / nat.b));
    }
    for (const [m, base] of this.skinBase) {
      const c = isToon(m) ? (m.uniforms.tint?.value as Color) : (m as Material & { color?: Color }).color;
      if (!c) continue;
      c.copy(base);
      if (ratio) c.multiply(ratio);
    }
    // Makeup.
    const amt = this.uniforms.edMakeupAmount.value;
    const mk = look.makeup;
    amt.set(mk.lipstick.amount / 100, mk.eyeliner.amount / 100, mk.eyeshadow.amount / 100, mk.blush.amount / 100);
    this.uniforms.edLip.value.set(mk.lipstick.color);
    this.uniforms.edLiner.value.set(mk.eyeliner.color);
    this.uniforms.edShadow.value.set(mk.eyeshadow.color);
    this.uniforms.edBlush.value.set(mk.blush.color);
    // Hair highlight (angel ring).
    for (const [m, base] of this.ringBase) (m.uniforms.ringColor!.value as Color).copy(look.hair.highlight ? linear(look.hair.highlight) : base);
  }

  dispose(): void {
    this.mask?.dispose();
    this.mask = null;
  }
}

const clampR = (x: number) => Math.min(1.35, Math.max(0.2, x));

export { MAKEUP_LAYERS };
