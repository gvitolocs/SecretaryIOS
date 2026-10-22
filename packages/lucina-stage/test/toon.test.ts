import { describe, expect, it, vi } from 'vitest';
import {
  ACESFilmicToneMapping,
  AgXToneMapping,
  Bone,
  Color,
  ShaderMaterial,
  Texture,
  BoxGeometry,
  BufferGeometry,
  Float32BufferAttribute,
  Group,
  MeshStandardMaterial,
  Skeleton,
  SkinnedMesh,
  Uint16BufferAttribute,
} from 'three';
import {
  acesSceneThreshold,
  applyManifestToon,
  applyToon,
  applyToonHairPart,
  computeOutlineWeights,
  normalizeToon,
} from '../src/toon';
import { ACES_INVERSE_IN, ACES_INVERSE_OUT, toonFragment } from '../src/toon-shaders';
import { HairRig, tintMaterial } from '../src/hair';
import { normalizeManifest } from '../src/manifest';

function model() {
  const geometry = new BoxGeometry();
  const count = geometry.attributes.position!.count;
  geometry.setAttribute('skinIndex', new Uint16BufferAttribute(new Uint16Array(count * 4), 4));
  const weights = new Float32Array(count * 4);
  for (let i = 0; i < count; i++) weights[i * 4] = 1;
  geometry.setAttribute('skinWeight', new Float32BufferAttribute(weights, 4));
  geometry.morphAttributes.position = [geometry.attributes.position!.clone()];
  geometry.morphAttributes.normal = [geometry.attributes.normal!.clone()];
  const material = new MeshStandardMaterial({ name: 'Skin' });
  const mesh = new SkinnedMesh(geometry, material);
  const bone = new Bone();
  mesh.add(bone);
  mesh.bind(new Skeleton([bone]));
  mesh.updateMorphTargets();
  mesh.morphTargetDictionary = { smile: 0 };
  const root = new Group();
  root.add(mesh);
  return { root, mesh, material };
}

describe('toon config', () => {
  it('normalizes defaults and ignores unknown fields', () => {
    const cfg = normalizeToon({ unknown: 1, materials: { Skin: { unknown: true } } });
    expect(cfg.light).toEqual([-0.33, 0.62, 0.71]);
    expect(cfg.ambient).toBe(0.03);
    expect(cfg.bloom).toEqual({ threshold: 0.72, strength: 0.55, radius: 0.55 });
    expect(cfg.materials.Skin).toMatchObject({ preset: 'lit', ramp: 'dress', detail: 0, normalScale: 0, saturation: 1 });
    expect(cfg).not.toHaveProperty('unknown');
    expect(cfg.materials.Skin).not.toHaveProperty('unknown');
  });
  it('drops unsafe paths and invalid numbers/colours/enums independently', () => {
    const cfg = normalizeToon({
      ramps: '../bad.png',
      light: [0, 0, 0],
      ambient: NaN,
      bloom: { radius: -2 },
      materials: {
        Skin: {
          map: 'UPPER.png',
          multi: '/private.png',
          color: [2, 0, 0],
          ramp: '__proto__',
          preset: 'oops',
          detailMean: 0,
          normalScale: Infinity,
          outline: { color: [0, NaN, 0] },
        },
        invalid: 3,
      },
    });
    expect(cfg.ramps).toBeUndefined();
    expect(cfg.light).toEqual([-0.33, 0.62, 0.71]);
    expect(cfg.materials.Skin).toMatchObject({ ramp: 'dress', preset: 'lit', detailMean: 0.5, normalScale: 0 });
    expect(cfg.materials.Skin?.color).toBeUndefined();
    expect(cfg.materials.Skin?.map).toBeUndefined();
    expect(cfg.materials.Skin?.multi).toBeUndefined();
    expect(cfg.materials.Skin?.outline).toBeUndefined();
    expect(cfg.materials.invalid).toBeUndefined();
  });
});

describe('toon scene', () => {
  const raw = { materials: { Skin: { preset: 'lit', outline: { color: [0.6, 0.5, 0.8], scale: 3.5 } } } };
  it('shares geometry, skeleton and morph arrays with a sibling hull and restores on disposal', async () => {
    const { root, mesh, material } = model();
    const handle = await applyToon(root, raw);
    expect(handle).not.toBeNull();
    const hull = root.children[1] as SkinnedMesh;
    expect(hull.parent).toBe(mesh.parent);
    expect(Array.isArray(hull.material)).toBe(false);
    expect(hull.geometry).toBe(mesh.geometry);
    expect(hull.skeleton).toBe(mesh.skeleton);
    expect(hull.bindMatrix.equals(mesh.bindMatrix)).toBe(true);
    expect(hull.morphTargetInfluences).toBe(mesh.morphTargetInfluences);
    expect(hull.morphTargetDictionary).toBe(mesh.morphTargetDictionary);
    expect(hull.frustumCulled).toBe(false);
    mesh.visible = false;
    mesh.position.x = 2;
    handle!.update();
    expect(hull.visible).toBe(false);
    expect(hull.position.x).toBe(2);
    handle!.dispose();
    expect(mesh.material).toBe(material);
    expect(root.children).toEqual([mesh]);
  });
  it('does not change materials when the manifest mode is absent or standard overrides it', async () => {
    const { root, mesh, material } = model();
    expect(await applyManifestToon(root, normalizeManifest({ toon: raw }))).toBeNull();
    expect(await applyManifestToon(root, normalizeManifest({ material: 'engage-toon', toon: raw }), { material: 'standard' })).toBeNull();
    expect(mesh.material).toBe(material);
    expect(root.children).toEqual([mesh]);
  });
  it('keeps original materials when an authenticated texture request fails', async () => {
    const { root, mesh, material } = model();
    const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('', { status: 403 }));
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const signal = new AbortController().signal;
    try {
      expect(
        await applyToon(
          root,
          { ...raw, ramps: 'ramps.png' },
          { baseUrl: 'https://example.test/private/manifest.json', fetchInit: { credentials: 'include' }, signal },
        ),
      ).toBeNull();
      expect(fetch).toHaveBeenCalledWith('https://example.test/private/ramps.png', { credentials: 'include', signal });
      expect(mesh.material).toBe(material);
      expect(root.children).toEqual([mesh]);
    } finally {
      fetch.mockRestore();
      warn.mockRestore();
    }
  });
});

describe('outfit and part toon materials', () => {
  it('uses the default for unlisted visible materials, preserving hidden ones', async () => {
    const { root, mesh, material } = model();
    const hidden = new SkinnedMesh(mesh.geometry, material.clone());
    hidden.visible = false;
    root.add(hidden);
    const handle = await applyToon(root, { default: { ramp: 'metal', outline: { color: [0, 0, 1] } } });
    expect(mesh.material).not.toBe(material);
    expect((mesh.material as unknown as ShaderMaterial).uniforms.rowV?.value).toBe(2.5 / 8);
    expect(hidden.material).not.toHaveProperty('isShaderMaterial');
    expect(root.children.filter((o) => o.userData.toonOutline)).toHaveLength(1);
    handle!.dispose();
  });

  it('uses the GLB ORM directly with B as metal and no file fetch', async () => {
    const { root, mesh, material } = model();
    material.map = new Texture();
    material.metalnessMap = new Texture();
    const fetch = vi.spyOn(globalThis, 'fetch');
    try {
      const handle = await applyToon(root, { default: { multi: 'orm', metalRamp: 'metal' } });
      const mat = mesh.material as unknown as ShaderMaterial;
      expect(mat.uniforms.albedoMap?.value).toBe(material.map);
      expect(mat.uniforms.multiMap?.value).toBe(material.metalnessMap);
      expect(mat.defines).toHaveProperty('ORM_MULTI');
      expect(mat.fragmentShader).toContain('vec4(1.0, multi.b, 0.0, 1.0)');
      expect(fetch).not.toHaveBeenCalled();
      handle!.dispose();
    } finally {
      fetch.mockRestore();
    }
  });

  it('tints toon hair through its linear tint uniform and restores its natural colour', async () => {
    const { root, mesh } = model();
    const handle = await applyToon(root, { default: { color: [0.2, 0.3, 0.5], solidColor: true } });
    const m = mesh.material as unknown as ShaderMaterial,
      natural = (m.uniforms.tint!.value as Color).clone();
    tintMaterial(m, '#ff0080');
    expect((m.uniforms.tint!.value as Color).equals(new Color('#ff0080'))).toBe(true);
    expect(m.userData.hairTint).toBeUndefined();
    tintMaterial(m, null);
    expect((m.uniforms.tint!.value as Color).equals(natural)).toBe(true);
    handle!.dispose();
  });

  it('prepares part materials and keeps hull skeletons shared after HairRig rebinds them', async () => {
    const { root } = model();
    const bone = new Bone();
    bone.name = 'head';
    root.add(bone);
    const main = await applyToon(root, { hairParts: { color: [0.1, 0.2, 0.4], outline: { color: [0.5, 0.5, 0.8] } }, materials: {} });
    const part = model();
    part.mesh.skeleton.bones[0]!.name = 'head';
    const prepared = await applyToonHairPart(root, part.root);
    const rig = new HairRig(root, 'head');
    rig.attach(part.root, { id: 'bun', glb: 'bun.glb' });
    main!.update();
    const hull = part.mesh.parent!.children.find((o) => o.userData.toonOutline) as SkinnedMesh;
    expect(prepared).not.toBeNull();
    expect(part.mesh.skeleton.bones[0]).toBe(bone);
    expect(hull.skeleton).toBe(part.mesh.skeleton);
    expect(hull.parent).toBe(part.mesh.parent);
    rig.setTint('#5566ff');
    expect((part.mesh.material as unknown as ShaderMaterial).uniforms.tint!.value).toEqual(new Color('#5566ff'));
    rig.detach();
    expect(hull.parent).toBeNull();
    expect(part.mesh.material).toBe(part.material);
    main!.dispose();
  });
});

describe('albedo saturation', () => {
  it('accepts 0..1 and defaults invalid values to 1 for all entry types', () => {
    const cfg = normalizeToon({
      default: { saturation: 0 },
      hairParts: { saturation: 0.4 },
      materials: {
        A: { saturation: 1 },
        B: { saturation: -0.1 },
        C: { saturation: 1.1 },
        D: { saturation: NaN },
        E: { saturation: '0.4' },
      },
    });
    expect(cfg.default?.saturation).toBe(0);
    expect(cfg.hairParts?.saturation).toBe(0.4);
    for (const name of ['A', 'B', 'C', 'D', 'E']) expect(cfg.materials[name]?.saturation).toBe(1);
  });
  it('supplies saturation to the shader before ramp shading', async () => {
    const { root, mesh } = model();
    const handle = await applyToon(root, { default: { saturation: 0.35 } });
    const mat = mesh.material as unknown as ShaderMaterial;
    expect(mat.uniforms.saturation?.value).toBe(0.35);
    expect(mat.fragmentShader).toContain('albedo = mix(vec3(lum(albedo)), albedo, saturation)');
    expect(mat.fragmentShader.indexOf('albedo = mix')).toBeLessThan(mat.fragmentShader.indexOf('col = albedo * ramp'));
    handle!.dispose();
  });
});

const ACES_IN = [0.59719, 0.35458, 0.04823, 0.076, 0.90834, 0.01566, 0.0284, 0.13383, 0.83777];
const ACES_OUT = [1.60475, -0.53108, -0.07367, -0.10208, 1.10813, -0.00605, -0.00327, -0.07276, 1.07602];
const mul3 = (m: number[], v: number[]): number[] => [
  m[0]! * v[0]! + m[1]! * v[1]! + m[2]! * v[2]!,
  m[3]! * v[0]! + m[4]! * v[1]! + m[5]! * v[2]!,
  m[6]! * v[0]! + m[7]! * v[1]! + m[8]! * v[2]!,
];
const rrt = (v: number): number => (v * (v + 0.0245786) - 0.000090537) / (v * (0.983729 * v + 0.432951) + 0.238081);
const forwardACES = (c: number[], exposure: number): number[] =>
  mul3(ACES_OUT, mul3(ACES_IN, c.map((x) => (x * exposure) / 0.6)).map(rrt)).map((x) => Math.min(1, Math.max(0, x)));
const inverseACES = (c: number[], exposure: number): number[] => {
  const w = mul3(ACES_INVERSE_OUT, c.map((x) => Math.min(1, Math.max(0, x)))).map((x) => Math.min(0.999, Math.max(0, x)));
  const v = w.map((wi) => {
    const a = 1 - 0.983729 * wi,
      b = 0.0245786 - 0.432951 * wi,
      k = -(0.000090537 + 0.238081 * wi);
    return (-b + Math.sqrt(Math.max(b * b - 4 * a * k, 0))) / (2 * a);
  });
  return mul3(ACES_INVERSE_IN, v).map((x) => Math.max(0, (x * 0.6) / exposure));
};

describe('bloom threshold in scene space', () => {
  it('bisects the display-space 0.72 threshold through ACES', () => {
    for (const [exposure, expected] of [
      [0.9, 0.936],
      [1, 0.842],
    ] as const) {
      const scene = acesSceneThreshold(0.72, exposure);
      expect(Math.abs(scene - expected)).toBeLessThan(0.005);
      expect(Math.abs(forwardACES([scene, scene, scene], exposure)[0]! - 0.72)).toBeLessThan(1e-6);
    }
  });

  it('converts the light-theme 0.97 threshold the same way', () => {
    const scene = acesSceneThreshold(0.97, 0.92);
    expect(Math.abs(scene - 5.9905)).toBeLessThan(0.005);
    expect(Math.abs(forwardACES([scene, scene, scene], 0.92)[0]! - 0.97)).toBeLessThan(1e-6);
  });
});

describe('ACES inverse for bloom', () => {
  it('round-trips linear toon colours that stay in gamut', () => {
    let checked = 0;
    for (const exposure of [0.9, 1, 1.2]) {
      for (let r = 0; r <= 10; r++) {
        for (let g = 0; g <= 10; g++) {
          for (let b = 0; b <= 10; b++) {
            const c = [r / 10, g / 10, b / 10];
            // Only colours the forward transform actually maps inside (0,1)^3 have an inverse.
            const raw = mul3(ACES_OUT, mul3(ACES_IN, c.map((x) => (x * exposure) / 0.6)).map(rrt));
            if (raw.some((x) => x <= 1e-4 || x >= 1 - 1e-4)) continue;
            const display = forwardACES(c, exposure);
            const back = inverseACES(display, exposure);
            expect(Math.max(...back.map((x, i) => Math.abs(x - c[i]!)))).toBeLessThan(1e-3);
            const again = forwardACES(back, exposure);
            expect(Math.max(...again.map((x, i) => Math.abs(x - display[i]!)))).toBeLessThan(1e-3);
            checked++;
          }
        }
      }
    }
    expect(checked).toBeGreaterThan(1000);
  });

  it('embeds the precomputed inverse matrices in the toon fragment', () => {
    for (const value of [...ACES_INVERSE_IN, ...ACES_INVERSE_OUT]) expect(toonFragment).toContain(String(value));
    expect(toonFragment).toContain('if (inverseTone > 0.5) col = acesInverse(col);');
  });
});

/** Grid patch (n x n vertices, z = 0) plus a separate island triangle. */
function patch(n: number): { geometry: BufferGeometry; island: number[] } {
  const geometry = new BufferGeometry();
  const position: number[] = [];
  for (let y = 0; y < n; y++) for (let x = 0; x < n; x++) position.push(x, y, 0);
  const index: number[] = [];
  for (let y = 0; y < n - 1; y++) {
    for (let x = 0; x < n - 1; x++) {
      const a = y * n + x,
        b = a + 1,
        c = a + n,
        d = c + 1;
      index.push(a, c, b, b, c, d);
    }
  }
  const island = [position.length / 3, position.length / 3 + 1, position.length / 3 + 2];
  position.push(-10, -10, 0, -9, -10, 0, -9.5, -9, 0);
  index.push(island[0]!, island[1]!, island[2]!);
  geometry.setAttribute('position', new Float32BufferAttribute(position, 3));
  geometry.setIndex(index);
  return { geometry, island };
}

describe('shell outline weights', () => {
  it('keeps the largest island, drops other pieces and fades the open border', () => {
    const n = 5;
    const { geometry, island } = patch(n);
    const k = computeOutlineWeights(geometry);
    for (let y = 0; y < n; y++) {
      for (let x = 0; x < n; x++) {
        const i = y * n + x;
        const border = x === 0 || y === 0 || x === n - 1 || y === n - 1;
        const ring = !border && (x === 1 || y === 1 || x === n - 2 || y === n - 2);
        expect(k[i]).toBe(border ? 0 : ring ? 0.5 : 1);
      }
    }
    for (const i of island) expect(k[i]).toBe(0);
  });

  it('welds UV seams before finding islands', () => {
    const geometry = new BufferGeometry();
    // Closed tetrahedron with every face's vertices duplicated (as a UV atlas would split them).
    const a = [0, 0, 0],
      b = [1, 0, 0],
      c = [0, 1, 0],
      d = [0, 0, 1];
    geometry.setAttribute(
      'position',
      new Float32BufferAttribute([...a, ...b, ...c, ...a, ...b, ...d, ...a, ...c, ...d, ...b, ...c, ...d], 3),
    );
    const k = computeOutlineWeights(geometry);
    // Welded, the four faces are one closed island with no boundary: full hull everywhere.
    for (const value of k) expect(value).toBe(1);
  });

  it('adds the shared attribute once and drives the hull shader', async () => {
    const { root, mesh } = model();
    const raw = { materials: { Skin: { outline: { color: [0.6, 0.5, 0.8], scale: 2.6, weight: 'shell' } } } };
    const first = await applyToon(root, raw);
    const attribute = mesh.geometry.getAttribute('outlineWeight');
    expect(attribute.count).toBe(mesh.geometry.getAttribute('position').count);
    const hull = root.children[1] as SkinnedMesh;
    expect(hull.geometry).toBe(mesh.geometry);
    expect((hull.material as ShaderMaterial).defines).toHaveProperty('OUTLINE_WEIGHT');
    first!.dispose();
    const second = await applyToon(root, raw);
    expect(mesh.geometry.getAttribute('outlineWeight')).toBe(attribute);
    expect(normalizeToon(raw).materials.Skin!.outline!.weight).toBe('shell');
    second!.dispose();
  });
});

describe('bloom inverse tone uniforms', () => {
  it('inverts only for ACES when the bloom pipeline asks for it', async () => {
    const { root, mesh } = model();
    const handle = await applyToon(root, { materials: { Skin: { preset: 'lit' } } });
    const material = mesh.material as unknown as ShaderMaterial;
    const render = (toneMapping: number, exposure: number, inverse: boolean): void => {
      (mesh.onBeforeRender as unknown as (r: unknown, s: unknown) => void)(
        { toneMapping, toneMappingExposure: exposure },
        { userData: { toonInverseTone: inverse } },
      );
    };
    render(ACESFilmicToneMapping, 0.9, true);
    handle!.update();
    expect(material.uniforms.inverseTone!.value).toBe(1);
    expect(material.uniforms.toneExposure!.value).toBe(0.9);
    render(ACESFilmicToneMapping, 0.9, false);
    handle!.update();
    expect(material.uniforms.inverseTone!.value).toBe(0);
    render(AgXToneMapping, 0.9, true);
    handle!.update();
    expect(material.uniforms.inverseTone!.value).toBe(0);
    handle!.dispose();
  });
});
