// @vitest-environment jsdom
import { Bone, BoxGeometry, Group, Matrix4, Mesh, MeshStandardMaterial, PerspectiveCamera, Skeleton, SkinnedMesh, type Object3D } from 'three';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { HairRig, NATURAL_HAIR, parseHairColors, parseHairstyles, tintMaterial } from '../src/hair';
import type { LucinaStage, LucinaStageOptions } from '../src/stage';
import { createWardrobeStage, parseWardrobeFile, type StageFactory } from '../src/wardrobe';

const mat = (name: string) => new MeshStandardMaterial({ name });

/** A tiny skinned "model": a head bone, a body skinned to it, base hair and a tiara. */
function model(): { root: Group; head: Bone; hair: Mesh; highlight: Mesh; tiara: Mesh; body: SkinnedMesh } {
  const root = new Group();
  const hips = new Bone();
  hips.name = 'Hips';
  const head = new Bone();
  head.name = 'Head';
  head.position.set(0, 1.5, 0);
  hips.add(head);
  root.add(hips);
  root.updateMatrixWorld(true);
  const body = new SkinnedMesh(new BoxGeometry(), mat('Body'));
  body.bind(new Skeleton([hips, head]), new Matrix4());
  root.add(body);
  const hair = new Mesh(new BoxGeometry(), mat('Hair'));
  const highlight = new Mesh(new BoxGeometry(), mat('HairHighlight'));
  const tiara = new Mesh(new BoxGeometry(), mat('Tiara'));
  root.add(hair, highlight, tiara);
  return { root, head, hair, highlight, tiara, body };
}

function part(): Object3D {
  const g = new Group();
  g.add(new Mesh(new BoxGeometry(), mat('PonytailHair')));
  return g;
}

describe('hairstyle and colour lists', () => {
  it('keeps valid entries', () => {
    const styles = parseHairstyles([
      { id: 'natural', name: { it: 'Naturale' } },
      { id: 'ponytail', name: { en: 'Ponytail' }, glb: 'lucina-hair-ponytail.glb', thumbnail: 'hair-ponytail.webp', hideTiara: true },
      { id: 'bun', glb: 'lucina-hair-bun.glb', hideMaterials: ['Hair'] },
      { id: 'nope' },
      { id: 'evil', glb: '../x.glb' },
      { id: 'ponytail', glb: 'dup.glb' },
    ]);
    expect(styles.map((s) => s.id)).toEqual(['natural', 'ponytail', 'bun']);
    expect(styles[1]).toMatchObject({ hideTiara: true, thumbnail: 'hair-ponytail.webp' });
    expect(styles[0]!.glb).toBeUndefined();
    expect(parseHairColors([{ id: 'pink', hex: '#FF88CC' }, { id: 'bad', hex: 'red' }, { id: 'x' }])).toEqual([{ id: 'pink', hex: '#ff88cc' }]);
  });

  it('reads the whole wardrobe file, and the old bare array', () => {
    const file = parseWardrobeFile({
      outfits: [{ id: 'gym', glb: 'lucina-gym.glb' }],
      hairstyles: [{ id: 'ponytail', glb: 'lucina-hair-ponytail.glb' }],
      colors: [{ id: 'silver', hex: '#c0c4d0' }],
    });
    expect([file.outfits.length, file.hairstyles.length, file.colors.length]).toEqual([1, 1, 1]);
    expect(parseWardrobeFile([{ id: 'gym', glb: 'lucina-gym.glb' }]).outfits).toHaveLength(1);
  });
});

describe('HairRig', () => {
  it('attaches a part to the head bone at its bind pose, hides the base hair and the tiara', () => {
    const m = model();
    const rig = new HairRig(m.root, 'Head');
    expect(rig.hasBaseHair).toBe(true);
    const p = part();
    rig.attach(p, { id: 'ponytail', glb: 'x.glb', hideTiara: true });
    expect(rig.hairstyle).toBe('ponytail');
    expect([m.hair.visible, m.highlight.visible, m.tiara.visible, m.body.visible]).toEqual([false, false, false, true]);
    const holder = p.parent!;
    expect(holder.parent).toBe(m.head);
    // Bind-pose local transform: the inverse of the head's bind position.
    expect(holder.matrix.elements[13]).toBeCloseTo(-1.5);

    rig.attach(null, null);
    expect(rig.hairstyle).toBe(NATURAL_HAIR);
    expect([m.hair.visible, m.highlight.visible, m.tiara.visible]).toEqual([true, true, true]);
    expect(holder.parent).toBeNull();
  });

  it('re-binds a skinned part to the model bones by name', () => {
    const m = model();
    const rig = new HairRig(m.root, 'Head');
    const g = new Group();
    const head = new Bone();
    head.name = 'Head';
    g.add(head);
    const hair = new SkinnedMesh(new BoxGeometry(), mat('Hair2'));
    hair.bind(new Skeleton([head]), new Matrix4());
    g.add(hair);
    rig.attach(g, { id: 'bun', glb: 'x.glb' });
    expect(hair.skeleton.bones[0]).toBe(m.head);
    expect(hair.parent?.name).toBe('Hair:bun');
  });

  it('re-binds to a grafted node that no skin of the model uses (loaded as a plain Object3D)', () => {
    const m = model();
    // The outfit file carries the hairstyle's own bone, but none of its skins: GLTFLoader makes it an Object3D.
    const grafted = new Group();
    grafted.name = 'ptail_0';
    grafted.position.set(0, 0.05, -0.15);
    m.head.add(grafted);
    const rig = new HairRig(m.root, 'Head');
    const g = new Group();
    const head = new Bone();
    head.name = 'Head';
    const tail = new Bone();
    tail.name = 'ptail_0';
    head.add(tail);
    g.add(head);
    const hair = new SkinnedMesh(new BoxGeometry(), mat('Ponytail'));
    hair.bind(new Skeleton([head, tail]), new Matrix4());
    g.add(hair);
    rig.attach(g, { id: 'ponytail', glb: 'p.glb' });
    expect(hair.skeleton.bones).toEqual([m.head, grafted]);
    expect(hair.parent?.name).toBe('Hair:ponytail');
    expect(m.head.children.some((c) => c.name.startsWith('HairRigid:'))).toBe(false);
  });

  it('tints through the shader, and falls back to the colour', () => {
    const m = model();
    const rig = new HairRig(m.root, 'Head');
    rig.setTint('#ff0000');
    const ud = (m.hair.material as MeshStandardMaterial).userData as { hairTint: { uHairTint: { value: { r: number } }; uHairTintAmount: { value: number } } };
    expect(ud.hairTint.uHairTintAmount.value).toBe(1);
    expect(ud.hairTint.uHairTint.value.r).toBeCloseTo(1);
    const shader = { uniforms: {}, fragmentShader: 'void main() {\n#include <map_fragment>\n}', vertexShader: '' };
    (m.hair.material as MeshStandardMaterial).onBeforeCompile(shader as never, null as never);
    expect(shader.fragmentShader).toContain('uHairTint');
    rig.setTint(null);
    expect(ud.hairTint.uHairTintAmount.value).toBe(0);

    const plain = mat('Custom');
    tintMaterial(plain, '#00ff00');
    plain.onBeforeCompile({ uniforms: {}, fragmentShader: 'void main() {}', vertexShader: '' } as never, null as never);
    tintMaterial(plain, '#00ff00');
    expect(plain.color.r).toBeCloseTo(0);
    tintMaterial(plain, null);
    expect(plain.color.r).toBeCloseTo(1);
  });
});

describe('createWardrobeStage hair', () => {
  let host: HTMLDivElement;
  let canvas: HTMLCanvasElement;
  beforeEach(() => {
    host = document.createElement('div');
    canvas = document.createElement('canvas');
    host.appendChild(canvas);
    document.body.appendChild(host);
  });
  afterEach(() => host.remove());

  const models: ReturnType<typeof model>[] = [];
  const factory: StageFactory = async (_c: HTMLCanvasElement, opts: LucinaStageOptions) => {
    const m = model();
    models.push(m);
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
      dispose() {},
      info: { placeholder: false, clips: [], extensions: [], rig: { bones: { head: 'Head' }, missingBones: [], morphs: {}, missingMorphs: [] }, hidden: [] },
      three: { model: m.root, camera: new PerspectiveCamera() },
      opts,
    } as unknown as LucinaStage;
  };

  it('starts with a hairstyle, changes it, tints it, and carries it to a new outfit', async () => {
    models.length = 0;
    const loaded: string[] = [];
    const s = await createWardrobeStage(
      canvas,
      {
        changeEffect: false,
        wardrobe: [
          { id: 'uniform', glb: 'lucina.glb' },
          { id: 'gym', glb: 'lucina-gym.glb' },
        ],
        outfit: 'uniform',
        hairstyles: [
          { id: 'ponytail', glb: 'lucina-hair-ponytail.glb', hideTiara: true },
          { id: 'bun', glb: 'lucina-hair-bun.glb' },
        ],
        hairColors: [{ id: 'pink', hex: '#ff88cc' }],
        hair: 'ponytail',
        hairLoader: async (url) => (loaded.push(url), part()),
      },
      factory,
    );
    expect(loaded).toEqual(['/v1/assets/lucina-hair-ponytail.glb']);
    expect(s.hair).toBe('ponytail');
    expect(models[0]!.hair.visible).toBe(false);
    expect(models[0]!.tiara.visible).toBe(false);

    const changes: [string, string | null][] = [];
    s.onHairChange((h, c) => changes.push([h, c]));
    await s.setHair('bun');
    expect(models[0]!.tiara.visible).toBe(true); // the bun keeps the tiara
    await s.setHairColor('pink');
    await expect(s.setHair('mohawk')).rejects.toThrow(/Unknown hairstyle/);
    await expect(s.setHairColor('green')).rejects.toThrow(/Unknown hair colour/);
    expect(changes).toEqual([
      ['bun', null],
      ['bun', 'pink'],
    ]);

    await s.setOutfit('gym');
    expect(models).toHaveLength(2);
    expect(models[1]!.hair.visible).toBe(false); // the new model wears the bun too
    const tint = (models[1]!.hair.material as MeshStandardMaterial).userData.hairTint as { uHairTintAmount: { value: number } };
    expect(tint.uHairTintAmount.value).toBe(1);

    await s.setHair(null);
    expect(models[1]!.hair.visible).toBe(true);
    expect(s.hair).toBe(NATURAL_HAIR);
    s.dispose();
  });
});
