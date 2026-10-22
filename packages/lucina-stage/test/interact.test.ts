import { describe, expect, it } from 'vitest';
import {
  composeCamera,
  DoubleTap,
  FEEL,
  gazeFollowWeight,
  GestureTracker,
  Orbit,
  PRESET_ZOOM,
  ReactionPlanner,
  regionOfBone,
  regionOfPoint,
  shotAt,
  wrapAngle,
  type BodyMetrics,
  type Shot,
  type ViewPreset,
} from '../src/interact';

const DEG = Math.PI / 180;

// A 1.6 m figure facing +Z; the camera in front, as the stage frames her.
const shots: Record<ViewPreset, Shot> = {
  full: { target: { x: 0, y: 0.8, z: 0 }, pos: { x: 0, y: 0.85, z: 3.4 }, fov: 30 },
  bust: { target: { x: 0, y: 1.35, z: 0.05 }, pos: { x: 0, y: 1.45, z: 1.3 }, fov: 28 },
  face: { target: { x: 0, y: 1.47, z: 0.08 }, pos: { x: 0, y: 1.48, z: 0.75 }, fov: 26 },
};

function run(o: Orbit, seconds: number, dt = 1 / 60): void {
  for (let t = 0; t < seconds; t += dt) o.update(dt);
}

describe('orbit', () => {
  it('composes the framing exactly at the presets', () => {
    for (const p of ['full', 'bust', 'face'] as const) {
      const c = composeCamera(shots, { yaw: 0, pitch: 0, zoom: PRESET_ZOOM[p], panX: 0, panY: 0 });
      expect(c.pos.x).toBeCloseTo(shots[p].pos.x, 5);
      expect(c.pos.y).toBeCloseTo(shots[p].pos.y, 5);
      expect(c.pos.z).toBeCloseTo(shots[p].pos.z, 5);
      expect(c.fov).toBeCloseTo(shots[p].fov, 5);
    }
  });

  it('a drag right turns her right (camera goes to -X), with a short glide', () => {
    const o = new Orbit(shots);
    o.rotate(40, 0, 400);
    o.update(1 / 60);
    const afterOne = o.view.yaw;
    expect(afterOne).toBeLessThan(0);
    run(o, 1);
    // The glide adds up to about degreesPerWidth * dx / width.
    expect(o.view.yaw / DEG).toBeCloseTo(-(FEEL.degreesPerWidth * 40) / 400, 0);
    expect(o.moving).toBe(false);
    const c = composeCamera(o.shots, o.view);
    expect(c.pos.x).toBeLessThan(0);
  });

  it('the glide decays like Custom Cast (×0.833 per 60 Hz frame), independent of the frame rate', () => {
    const a = new Orbit(shots);
    const b = new Orbit(shots);
    a.rotate(30, 0, 400);
    b.rotate(30, 0, 400);
    run(a, 0.5, 1 / 60);
    run(b, 0.5, 1 / 30);
    expect(a.view.yaw).toBeCloseTo(b.view.yaw, 2);
  });

  it('yaw is unlimited and wraps; pitch is clamped to -30..+45 degrees of elevation', () => {
    const o = new Orbit(shots);
    for (let i = 0; i < 40; i++) {
      o.rotate(-400, 0, 400);
      run(o, 0.05);
    }
    expect(Math.abs(o.view.yaw)).toBeLessThanOrEqual(Math.PI);
    for (let i = 0; i < 40; i++) {
      o.rotate(0, 400, 400);
      run(o, 0.05);
    }
    run(o, 1);
    const top = composeCamera(o.shots, o.view);
    const el = Math.asin((top.pos.y - top.target.y) / Math.hypot(top.pos.x - top.target.x, top.pos.y - top.target.y, top.pos.z - top.target.z));
    expect(el / DEG).toBeLessThanOrEqual(45.01);
    expect(el / DEG).toBeGreaterThan(40);
    for (let i = 0; i < 60; i++) {
      o.rotate(0, -400, 400);
      run(o, 0.05);
    }
    run(o, 1);
    const low = composeCamera(o.shots, o.view);
    const el2 = Math.asin((low.pos.y - low.target.y) / Math.hypot(low.pos.x - low.target.x, low.pos.y - low.target.y, low.pos.z - low.target.z));
    expect(el2 / DEG).toBeGreaterThanOrEqual(-30.01);
  });

  it('pinch zooms along full → bust → face and springs back from the overshoot', () => {
    const o = new Orbit(shots, { zoom: 0.5 });
    o.pinch(1.5);
    run(o, 1);
    expect(o.view.zoom).toBeGreaterThan(0.5);
    for (let i = 0; i < 20; i++) o.pinch(1.5);
    run(o, 1);
    expect(o.view.zoom).toBeGreaterThan(1);
    expect(o.view.zoom).toBeLessThanOrEqual(1.06 + 1e-9);
    o.release();
    run(o, 1);
    expect(o.view.zoom).toBeCloseTo(1, 3);
    // Pinching closed by the whole span goes back to full body.
    const span = Math.log(3.4 / 0.67);
    o.pinch(Math.exp(-span * 2));
    o.release();
    run(o, 2);
    expect(o.view.zoom).toBeCloseTo(0, 3);
  });

  it('zoom distance is geometric between the keyframes', () => {
    const mid = shotAt(shots, 0.25);
    const full = Math.hypot(0, 0.05, 3.4);
    const bust = Math.hypot(0, 0.1, 1.25);
    expect(mid.dist).toBeCloseTo(Math.sqrt(full * bust), 4);
  });

  it('pan follows the fingers and stays on her', () => {
    const o = new Orbit(shots, { zoom: 0 });
    o.bounds = { min: { x: -0.3, y: 0, z: -0.2 }, max: { x: 0.3, y: 1.6, z: 0.2 }, height: 1.6 };
    o.pan(-50, 0, 800); // fingers move left: the target goes right
    run(o, 1);
    expect(o.view.panX).toBeGreaterThan(0);
    for (let i = 0; i < 50; i++) o.pan(0, -400, 800);
    run(o, 1);
    const c = composeCamera(o.shots, o.view);
    expect(c.target.y).toBeLessThanOrEqual(1.6 + 1e-6);
    for (let i = 0; i < 50; i++) o.pan(-400, 0, 800);
    run(o, 1);
    expect(o.view.panX).toBeLessThanOrEqual(0.3 + 1e-6);
  });

  it('reset flies home the short way round; presets keep the angle', () => {
    const o = new Orbit(shots, { zoom: 0.5 });
    o.flyTo({ yaw: 170 * DEG, zoom: 1, panX: 0.1 }, false);
    o.preset('full');
    run(o, 0.6);
    expect(o.view.zoom).toBeCloseTo(0, 5);
    expect(o.view.yaw).toBeCloseTo(170 * DEG, 5);
    expect(o.view.panX).toBeCloseTo(0, 5);
    // 170° → −170° goes 20° through the back, not 340° round the front.
    o.setShots(shots);
    o.flyTo({ yaw: -170 * DEG });
    o.update(0.2);
    expect(Math.abs(o.view.yaw)).toBeGreaterThan(170 * DEG - 1e-6);
    run(o, 0.6);
    expect(o.view.yaw).toBeCloseTo(-170 * DEG, 5);
    o.reset();
    run(o, 0.6);
    expect(o.view.yaw).toBeCloseTo(0, 5);
    expect(o.view.zoom).toBeCloseTo(0.5, 5);
  });

  it('reduced motion: no glide, no fly-to', () => {
    const o = new Orbit(shots);
    o.reduced = true;
    o.rotate(40, 0, 400);
    const y = o.view.yaw;
    expect(y / DEG).toBeCloseTo(-(FEEL.degreesPerWidth * 40) / 400, 3);
    run(o, 1);
    expect(o.view.yaw).toBe(y);
    o.preset('face');
    expect(o.view.zoom).toBe(1);
  });

  it('wrapAngle and the gaze follow weight', () => {
    expect(wrapAngle(3 * Math.PI)).toBeCloseTo(Math.PI, 9);
    expect(wrapAngle(-3 * Math.PI / 2)).toBeCloseTo(Math.PI / 2, 9);
    expect(gazeFollowWeight(0)).toBe(1);
    expect(gazeFollowWeight(90 * DEG)).toBe(1);
    expect(gazeFollowWeight(125 * DEG)).toBeGreaterThan(0.2);
    expect(gazeFollowWeight(125 * DEG)).toBeLessThan(0.8);
    expect(gazeFollowWeight(170 * DEG)).toBe(0);
    expect(gazeFollowWeight(-170 * DEG)).toBe(0);
  });
});

describe('gestures', () => {
  function tracker() {
    const log: string[] = [];
    const t = new GestureTracker({
      rotate: (dx, dy) => log.push(`rotate ${dx},${dy}`),
      pan: (dx, dy) => log.push(`pan ${dx},${dy}`),
      pinch: (r) => log.push(`pinch ${r.toFixed(2)}`),
      tap: (x, y) => log.push(`tap ${x},${y}`),
      release: (g) => log.push(`release ${g}`),
    });
    return { t, log };
  }
  const P = (id: number, x: number, y: number, t: number, type = 'touch', button = 0) => ({ id, x, y, t, type, button });

  it('a short still press is a tap; a drag past the slop rotates and is not a tap', () => {
    const { t, log } = tracker();
    t.down(P(1, 100, 100, 0));
    t.move(P(1, 103, 101, 50));
    t.up(P(1, 103, 101, 120));
    expect(log).toEqual(['tap 103,101', 'release false']);
    log.length = 0;
    t.down(P(1, 100, 100, 0));
    t.move(P(1, 110, 100, 16));
    t.move(P(1, 130, 105, 32));
    t.up(P(1, 130, 105, 48));
    expect(log).toEqual(['rotate 10,0', 'rotate 20,5', 'release true']);
  });

  it('a long press is not a tap', () => {
    const { t, log } = tracker();
    t.down(P(1, 100, 100, 0));
    t.up(P(1, 100, 100, 900));
    expect(log).toEqual(['release false']);
  });

  it('two fingers pinch; moving together they pan; lifting one never rotates', () => {
    const { t, log } = tracker();
    t.down(P(1, 100, 100, 0));
    t.down(P(2, 200, 100, 0));
    t.move(P(2, 300, 100, 16));
    expect(log).toEqual(['pinch 2.00']);
    log.length = 0;
    t.move(P(1, 100, 120, 32));
    t.move(P(2, 300, 120, 40));
    expect(log.filter((l) => l.startsWith('pan'))).toEqual(['pan 0,10']);
    log.length = 0;
    t.up(P(2, 300, 120, 60));
    t.move(P(1, 50, 160, 80));
    t.up(P(1, 50, 160, 100));
    expect(log).toEqual(['release true']);
  });

  it('right mouse button pans', () => {
    const { t, log } = tracker();
    t.down(P(1, 10, 10, 0, 'mouse', 2));
    t.move(P(1, 30, 10, 16, 'mouse', 2));
    t.up(P(1, 30, 10, 32, 'mouse', 2));
    expect(log[0]).toBe('pan 20,0');
  });

  it('double-tap needs two taps close in time and space', () => {
    const d = new DoubleTap(300, 30);
    expect(d.tap(10, 10, 0)).toBe(false);
    expect(d.tap(15, 12, 200)).toBe(true);
    expect(d.tap(15, 12, 300)).toBe(false);
    expect(d.tap(15, 12, 800)).toBe(false);
    expect(d.tap(100, 12, 900)).toBe(false);
  });
});

describe('regions', () => {
  it('maps bone names in several conventions', () => {
    const cases: [string, string, string | null][] = [
      ['head neck upper', 'head', null],
      ['Hair05', 'head', null],
      ['eye_L', 'face', 'left'],
      ['toplip1_R', 'face', 'right'],
      ['arm left finger index a', 'hand', 'left'],
      ['arm right wrist', 'hand', 'right'],
      ['arm left elbow', 'arm', 'left'],
      ['leg right knee', 'leg', 'right'],
      ['breast left', 'chest', 'left'],
      ['spine upper', 'chest', null],
      ['pelvis', 'hips', null],
      ['LeftHand', 'hand', 'left'],
      ['RightUpperLeg', 'leg', 'right'],
      ['左腕', 'arm', 'left'],
      ['右ひざ', 'leg', 'right'],
      ['頭', 'head', null],
      ['上半身', 'chest', null],
      ['下半身', 'hips', null],
      ['スカート_前_1', 'skirt', null],
      ['J_Bip_L_Hand', 'hand', 'left'],
    ];
    for (const [name, region, side] of cases) {
      const r = regionOfBone(name);
      expect(r?.region, name).toBe(region);
      expect(r?.side, name).toBe(side);
    }
    expect(regionOfBone('root ground')).toBeNull();
    expect(regionOfBone('jnt_058')).toBeNull();
  });

  it('falls back to the hit height', () => {
    const m: BodyMetrics = { floorY: 0, height: 1.6, headY: 1.42, eyeY: 1.5, centerX: 0, centerZ: 0, headZ: 0, shoulderHalf: 0.18 };
    expect(regionOfPoint({ x: 0, y: 1.58, z: 0 }, m).region).toBe('head');
    expect(regionOfPoint({ x: 0, y: 1.45, z: 0.08 }, m).region).toBe('face');
    expect(regionOfPoint({ x: 0, y: 1.2, z: 0.1 }, m).region).toBe('chest');
    expect(regionOfPoint({ x: 0.25, y: 1.1, z: 0 }, m)).toMatchObject({ region: 'arm', side: 'left' });
    expect(regionOfPoint({ x: -0.25, y: 0.85, z: 0 }, m)).toMatchObject({ region: 'hand', side: 'right' });
    expect(regionOfPoint({ x: 0, y: 0.8, z: 0.1 }, m).region).toBe('hips');
    expect(regionOfPoint({ x: 0.05, y: 0.7, z: 0.1 }, m).region).toBe('skirt');
    expect(regionOfPoint({ x: 0.08, y: 0.3, z: 0 }, m)).toMatchObject({ region: 'leg', side: 'left' });
  });
});

describe('reactions', () => {
  it('a second head tap is a pat, and pats escalate', () => {
    const p = new ReactionPlanner(() => 0);
    const hit = { region: 'head' as const, side: null, bone: null };
    expect(p.plan(hit, 0).pat).toBe(0);
    expect(p.plan(hit, 0.8)).toMatchObject({ pat: 1, line: 'pat' });
    expect(p.plan(hit, 1.6)).toMatchObject({ pat: 2 });
    expect(p.plan(hit, 2.4)).toMatchObject({ pat: 3 });
    expect(p.plan(hit, 3.0)).toMatchObject({ pat: 3 });
    // A pause ends the patting.
    expect(p.plan(hit, 10).pat).toBe(0);
  });

  it('rotates reactions per region and gets fed up after four pokes', () => {
    const p = new ReactionPlanner(() => 0);
    const chest = { region: 'chest' as const, side: null, bone: null };
    const a = p.plan(chest, 0).name;
    const b = p.plan(chest, 0.5).name;
    expect(a).not.toBe(b);
    p.plan(chest, 1);
    expect(p.plan(chest, 1.5).name).toBe('enough');
    expect(p.plan(chest, 2).name).not.toBe('enough');
  });

  it('never uses the clips that are broken on the live model', async () => {
    const { REACTIONS, PATS, ENOUGH, BLOCKED_CLIPS } = await import('../src/interact');
    for (const spec of [...Object.values(REACTIONS).flat(), ...PATS, ENOUGH]) {
      for (const c of spec.clips) expect(BLOCKED_CLIPS.has(c), `${spec.name} uses ${c}`).toBe(false);
    }
  });
});
