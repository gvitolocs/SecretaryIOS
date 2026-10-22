import { describe, expect, it, vi } from 'vitest';
import {
  Bone,
  Color,
  ShaderMaterial,
  Texture,
  BoxGeometry,
  Float32BufferAttribute,
  Group,
  MeshStandardMaterial,
  Skeleton,
  SkinnedMesh,
  Uint16BufferAttribute,
} from 'three';
import { applyManifestToon, applyToon, applyToonHairPart, normalizeToon } from '../src/toon';
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
