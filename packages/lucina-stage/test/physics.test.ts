import { Bone, Group, Quaternion, Vector3 } from 'three';
import { describe, expect, it } from 'vitest';
import { matchChain, parsePhysics, resolveVariant } from '../src/physics/config';
import { PhysicsRig, physicsTier } from '../src/physics/rig';
import { SpringSolver } from '../src/physics/solver';
import { normalizeManifest } from '../src/manifest';
import type { ChainSpec } from '../src/physics/config';

/** body (at 1.5 m) with a 4-bone strand hanging from it, 0.1 m per segment, plus a second strand. */
function rig(): { root: Group; body: Bone; strand: Bone[]; strand2: Bone[] } {
  const root = new Group();
  const body = new Bone();
  body.name = 'body';
  body.position.set(0, 1.5, 0);
  root.add(body);
  const mk = (prefix: string, x: number): Bone[] => {
    const out: Bone[] = [];
    let parent: Bone = body;
    for (let i = 0; i < 4; i++) {
      const b = new Bone();
      b.name = `${prefix}${i}`;
      b.position.set(i === 0 ? x : 0, i === 0 ? 0 : -0.1, i === 0 ? -0.1 : 0);
      parent.add(b);
      out.push(b);
      parent = b;
    }
    return out;
  };
  const strand = mk('hair', 0);
  const strand2 = mk('side', 0.1);
  root.updateMatrixWorld(true);
  return { root, body, strand, strand2 };
}

function chain(over: Partial<ChainSpec> = {}): ChainSpec {
  return {
    name: 'hair',
    bones: ['hair0', 'hair1', 'hair2', 'hair3'],
    stiffness: 1,
    gravityPower: 0,
    gravityDir: [0, -1, 0],
    dragForce: 0.4,
    hitRadius: 0.02,
    maxAngle: 0,
    inertia: 1,
    colliders: [],
    lod: 'medium',
    end: 0.1,
    ...over,
  };
}

const wp = (b: Bone): Vector3 => b.getWorldPosition(new Vector3());

/** Runs `n` frames of `dt`, calling `move(t)` before each. */
function run(s: SpringSolver, root: Group, n: number, dt: number, move?: (t: number) => void): void {
  for (let i = 0; i < n; i++) {
    move?.(i * dt);
    s.update(dt);
    root.updateMatrixWorld(true);
  }
}

describe('physics config', () => {
  it('validates an untrusted block', () => {
    const cfg = parsePhysics({
      rate: 1000,
      colliders: [
        { name: 'head', bone: 'head', shape: 'sphere', radius: 0.1, at: [0, 1.5, 0] },
        { name: 'bad', bone: 'head', radius: -1 },
        { name: 'arm', bone: 'arm', shape: 'capsule', radius: 0.05, offset: [0, 0, 0], tail: [0, 0.2, 0] },
      ],
      chains: [
        { name: 'hair', bones: ['a', 'b'], stiffness: [2, 1], dragForce: 3, colliders: ['head', '@body'], end: 'tip' },
        { name: 'hair', bones: ['c'] },
        { name: 'x', bones: [] },
        { name: 'evil<script>', bones: ['d'] },
      ],
      cloth: [{ name: 'cape', chains: ['hair', 'missing'] }],
      outfits: { gym: { disable: ['hair*'] } },
    })!;
    expect(cfg.rate).toBe(240);
    expect(cfg.colliders.map((c) => c.name)).toEqual(['head', 'arm']);
    expect(cfg.chains).toHaveLength(1);
    expect(cfg.chains[0]).toMatchObject({ stiffness: [2, 1], dragForce: 1, end: 'tip', lod: 'medium' });
    expect(cfg.cloth).toHaveLength(0);
    expect(parsePhysics({ chains: [] })).toBeNull();
    expect(parsePhysics({ enabled: false, chains: [{ name: 'a', bones: ['a'] }] })).toBeNull();
    expect(parsePhysics('nope')).toBeNull();
  });

  it('keeps the raw block through the manifest normaliser', () => {
    expect(normalizeManifest({ physics: { chains: [] } }).physics).toEqual({ chains: [] });
  });

  it('resolves outfit and hairstyle variants', () => {
    const cfg = parsePhysics({
      chains: [
        { name: 'hair_l', group: 'hair', bones: ['a'] },
        { name: 'hair_r', group: 'hair', bones: ['b'] },
        { name: 'cape', group: 'cape', bones: ['c'], stiffness: 1 },
      ],
      outfits: { gym: { disable: ['@cape'], chains: { 'hair_*': { stiffness: 3 } } } },
      hairstyles: { ponytail: { disable: ['@hair'] }, bob: { disable: ['@hair'], enable: ['hair_l'] } },
    })!;
    expect(resolveVariant(cfg, 'gym', null).map((c) => [c.name, c.stiffness])).toEqual([
      ['hair_l', 3],
      ['hair_r', 3],
    ]);
    expect(resolveVariant(cfg, null, 'ponytail').map((c) => c.name)).toEqual(['cape']);
    expect(resolveVariant(cfg, 'gym', 'bob').map((c) => c.name)).toEqual(['hair_l']);
    expect(cfg.chains[0]!.stiffness).toBe(1); // untouched
    expect(matchChain('@hair', { name: 'x', group: 'hair' })).toBe(true);
    expect(matchChain('h*_l', { name: 'hair_l' })).toBe(true);
    expect(matchChain('h*_l', { name: 'hair_r' })).toBe(false);
  });

  it('maps quality to tiers', () => {
    expect(physicsTier('low', false)).toBe('off');
    expect(physicsTier('auto', true)).toBe('medium');
    expect(physicsTier('high', false)).toBe('high');
    expect(physicsTier(undefined, false)).toBe('high');
    expect(physicsTier('high', true)).toBe('medium');
  });
});

describe('spring solver', () => {
  it('stays finite and keeps bone lengths under violent motion at variable frame rates', () => {
    const { root, body, strand } = rig();
    const { solver } = SpringSolver.bind(root, [chain({ gravityPower: 0.5, stiffness: 0.5, dragForce: 0.1 })], []);
    let seed = 7;
    const rnd = (): number => ((seed = (seed * 16807) % 2147483647) / 2147483647) - 0.5;
    for (let i = 0; i < 600; i++) {
      body.position.set(rnd() * 0.4, 1.5 + rnd() * 0.4, rnd() * 0.4);
      body.quaternion.setFromAxisAngle(new Vector3(rnd(), rnd(), rnd()).normalize(), rnd() * 2);
      solver.update(0.005 + Math.abs(rnd()) * 0.19);
      root.updateMatrixWorld(true);
      for (let j = 0; j < 4; j++) {
        const t = solver.tail(j);
        expect(Number.isFinite(t.x + t.y + t.z)).toBe(true);
        const head = wp(strand[j]!);
        expect(t.distanceTo(head)).toBeCloseTo(0.1, 3);
      }
    }
    for (const b of strand) expect(Math.abs(b.quaternion.length() - 1)).toBeLessThan(1e-5);
  });

  it('swings behind a moving body, then settles back to rest', () => {
    const { root, body } = rig();
    const { solver } = SpringSolver.bind(root, [chain()], []);
    run(solver, root, 2, 1 / 60);
    const rest = solver.tail(3).clone();
    run(solver, root, 20, 1 / 60, (t) => body.position.setX(t * 2)); // 2 m/s to +X for 1/3 s
    const lag = solver.tail(3).x - wp(body).x;
    expect(lag).toBeLessThan(-0.02); // the tip trails
    run(solver, root, 600, 1 / 60);
    expect(solver.tail(3).x - wp(body).x).toBeCloseTo(rest.x, 2);
  });

  it('moves the same at 30, 60 and 120 Hz', () => {
    const tips: Vector3[] = [];
    for (const hz of [30, 60, 120]) {
      const { root, body } = rig();
      const { solver } = SpringSolver.bind(root, [chain({ gravityPower: 0.3 })], []);
      run(solver, root, Math.round(hz * 0.25), 1 / hz, (t) => body.position.setZ(Math.sin(t * 12) * 0.2));
      run(solver, root, Math.round(hz * 0.5), 1 / hz);
      tips.push(solver.tail(3).clone());
    }
    expect(tips[0]!.distanceTo(tips[1]!)).toBeLessThan(0.02);
    expect(tips[2]!.distanceTo(tips[1]!)).toBeLessThan(0.02);
  });

  it('pushes tails out of colliders', () => {
    const { root, body } = rig();
    // A ball right under the strand's middle; gravity drags the strand onto it sideways.
    const { solver } = SpringSolver.bind(
      root,
      [chain({ stiffness: 0, gravityPower: 2, gravityDir: [1, 0, 0], colliders: ['ball'] })],
      [{ name: 'ball', bone: 'body', shape: 'sphere', radius: 0.08, at: [0.12, 1.3, -0.1] }],
    );
    run(solver, root, 240, 1 / 60);
    const ball = new Vector3(0.12, 1.3, -0.1).add(wp(body)).sub(new Vector3(0, 1.5, 0));
    for (let j = 0; j < 4; j++) expect(solver.tail(j).distanceTo(ball)).toBeGreaterThan(0.08 + 0.02 - 1e-3);
    // And it did swing toward +X (gravity is sideways), so the ball was in the way.
    expect(solver.tail(3).x).toBeGreaterThan(0.05);
  });

  it('pushes out of capsules and holds a bind-pose overlap instead of shoving it', () => {
    const { root } = rig();
    const { solver } = SpringSolver.bind(
      root,
      [chain({ colliders: ['leg', 'overlap'] })],
      [
        { name: 'leg', bone: 'body', shape: 'capsule', radius: 0.05, at: [-0.2, 1.25, 0.05], to: [0.2, 1.25, 0.05] },
        // The strand's 2nd tail (y 1.3) sits 1 cm from this core: an authoring overlap.
        { name: 'overlap', bone: 'body', shape: 'sphere', radius: 0.05, at: [0, 1.3, -0.11] },
      ],
    );
    expect(solver.stats.restOverlaps).toBeGreaterThan(0);
    run(solver, root, 2, 1 / 60);
    const before = solver.tail(1).clone();
    run(solver, root, 120, 1 / 60);
    expect(solver.tail(1).distanceTo(before)).toBeLessThan(1e-3);
    // Now kick the strand forward (+Z) through the capsule: it must stay outside.
    for (let j = 0; j < 4; j++) {
      const t = solver.tail(j);
      solver.poke(t, new Vector3(0, 0, 1), 0.6, 0.01);
    }
    for (let i = 0; i < 60; i++) {
      solver.update(1 / 60);
      root.updateMatrixWorld(true);
      for (let j = 0; j < 4; j++) {
        const t = solver.tail(j);
        const dy = t.y - 1.25;
        const dz = t.z - 0.05;
        if (Math.abs(t.x) <= 0.2) expect(Math.hypot(dy, dz)).toBeGreaterThan(0.05 + 0.02 - 5e-3); // a few mm where the length and the capsule disagree
      }
    }
  });

  it('keeps a cone limit', () => {
    const { root } = rig();
    const { solver } = SpringSolver.bind(root, [chain({ stiffness: 0, gravityPower: 3, gravityDir: [0, 0, 1], maxAngle: 20 })], []);
    run(solver, root, 300, 1 / 60);
    // Root joint: rest direction is straight down (-Y).
    const d = solver.tail(0).sub(new Vector3(0, 1.5, -0.1)).normalize();
    expect((Math.acos(-d.y) * 180) / Math.PI).toBeLessThan(20.5);
  });

  it('resets on a teleport instead of swinging', () => {
    const { root, strand } = rig();
    const { solver } = SpringSolver.bind(root, [chain()], []);
    run(solver, root, 10, 1 / 60);
    root.position.set(3, 0, 0);
    root.updateMatrixWorld(true);
    solver.update(1 / 60);
    root.updateMatrixWorld(true);
    // Straight down from the new spot at once, no swing.
    expect(solver.tail(3).x).toBeCloseTo(3, 4);
    expect(strand[0]!.quaternion.angleTo(new Quaternion())).toBeLessThan(1e-4);
    // An explicit reset after the strand was disturbed snaps it back too.
    solver.poke(solver.tail(3), new Vector3(1, 0, 0), 0.1, 0.05);
    run(solver, root, 3, 1 / 60);
    solver.reset();
    run(solver, root, 1, 1 / 60);
    expect(solver.tail(3).x).toBeCloseTo(3, 4);
  });

  it('swings around the animated pose (a clip driving the bone)', () => {
    const { root, strand } = rig();
    const { solver } = SpringSolver.bind(root, [chain({ stiffness: 4 })], []);
    const tilt = new Quaternion().setFromAxisAngle(new Vector3(1, 0, 0), 0.6);
    run(solver, root, 300, 1 / 60, () => strand[0]!.quaternion.copy(tilt)); // the "clip" writes every frame
    const d = solver.tail(0).sub(wp(strand[0]!)).normalize();
    const want = new Vector3(0, -1, 0).applyQuaternion(tilt);
    expect(d.angleTo(want)).toBeLessThan(0.05);
    // When the clip stops writing, the pose it left is kept as the base (three's mixer writes only changes).
    run(solver, root, 300, 1 / 60);
    expect(solver.tail(0).sub(wp(strand[0]!)).normalize().angleTo(want)).toBeLessThan(0.05);
  });

  it('links cloth rows so neighbours follow', () => {
    const linked = (stiffness: number): number => {
      const { root } = rig();
      const side: ChainSpec = chain({ name: 'side', bones: ['side0', 'side1', 'side2', 'side3'] });
      const { solver } = SpringSolver.bind(root, [chain(), side], [], {}, stiffness ? [{ chains: ['hair', 'side'], stiffness, loop: false }] : []);
      run(solver, root, 2, 1 / 60);
      const before = solver.tail(solver.chains[solver.chainIndex('side')]!.start + 3).clone();
      solver.poke(solver.tail(3), new Vector3(0, 0, -1), 0.05, 0.01);
      run(solver, root, 6, 1 / 60);
      return solver.tail(solver.chains[solver.chainIndex('side')]!.start + 3).distanceTo(before);
    };
    expect(linked(0)).toBeLessThan(1e-6);
    expect(linked(0.8)).toBeGreaterThan(0.002);
  });
});

describe('physics rig', () => {
  const block = {
    colliders: [{ name: 'core', bone: 'body', shape: 'sphere', radius: 0.05, at: [0, 1.5, 0.2] }],
    chains: [
      { name: 'hair', group: 'hair', bones: ['hair0', 'hair1', 'hair2', 'hair3'], end: 0.1, colliders: ['core'] },
      { name: 'side', group: 'side', bones: ['side0', 'side1', 'side2', 'side3'], lod: 'high' },
      { name: 'ghost', bones: ['nope'] },
    ],
    outfits: { gym: { disable: ['@hair'] } },
  };

  it('binds, reports and follows quality, variants and reduced motion', () => {
    const { root, body, strand } = rig();
    const p = PhysicsRig.create(root, block, { phone: false })!;
    const s = p.stats();
    expect([s.joints, s.chains, s.colliders, s.skipped]).toEqual([8, 2, 1, ['ghost']]);
    expect(p.tier).toBe('high');
    expect(p.chains).toEqual(['hair', 'side']);
    p.update(1 / 60);
    body.position.x = 0.3;
    root.updateMatrixWorld(true);
    p.update(1 / 60);
    expect(strand[0]!.quaternion.angleTo(new Quaternion())).toBeGreaterThan(1e-3);
    expect(p.stats().ms).toBeGreaterThanOrEqual(0);
    // Low: off, bones back to their pose.
    p.setQuality('low');
    expect(p.tier).toBe('off');
    expect(strand[0]!.quaternion.angleTo(new Quaternion())).toBeLessThan(1e-6);
    p.update(1 / 60);
    p.setQuality('medium');
    expect(p.tier).toBe('medium');
    p.setVariant({ outfit: 'gym' });
    expect(p.chains).toEqual(['side']);
    p.setVariant({ outfit: null });
    expect(p.chains).toEqual(['hair', 'side']);
    p.setReducedMotion(true);
    expect(p.raw.amplitude).toBeCloseTo(0.35);
  });

  it('skips high-only chains on the medium tier', () => {
    const { root, body, strand2 } = rig();
    const p = PhysicsRig.create(root, block, { phone: true })!;
    expect(p.tier).toBe('medium');
    for (let i = 0; i < 10; i++) {
      body.position.x = i * 0.05;
      root.updateMatrixWorld(true);
      p.update(1 / 60);
    }
    expect(strand2[0]!.quaternion.angleTo(new Quaternion())).toBeLessThan(1e-6);
  });

  it('orbit inertia and pokes move tails; nothing without a physics block', () => {
    const { root } = rig();
    const p = PhysicsRig.create(root, block, { phone: false })!;
    p.update(1 / 60);
    const before = p.raw.tail(3).clone();
    p.orbit(0.2, { x: 0, y: 1.5, z: 0 });
    expect(p.raw.tail(3).distanceTo(before)).toBeGreaterThan(0.005);
    expect(p.poke({ x: before.x, y: before.y, z: before.z }, { x: 1, y: 0, z: 0 })).toBeGreaterThan(0);
    expect(PhysicsRig.create(root, undefined)).toBeNull();
    expect(PhysicsRig.create(root, { chains: [{ name: 'x', bones: ['missing'] }] })).toBeNull();
  });
});
