/**
 * GPU particles. `Motes` are ambient light specks rising around Lucina; `swirl` (thinking)
 * pulls them into a faster, tighter vortex. `Burst` is a one-shot sparkle spray (taps).
 * All motion is computed in the vertex shader from per-particle seeds: no CPU work per frame.
 */
import {
  AdditiveBlending,
  BufferAttribute,
  BufferGeometry,
  Color,
  NormalBlending,
  Points,
  ShaderMaterial,
  Vector3,
} from 'three';

const common = /* glsl */ `
uniform float uTime;
uniform float uPixel;
uniform float uSize;
uniform vec3 uA;
uniform vec3 uB;
uniform vec3 uC;
attribute vec4 seed;
varying float vAlpha;
varying float vSpark;
varying vec3 vCol;
vec3 pick(vec4 s) { return s.y < 0.45 ? uA : (s.w < 0.7 ? uB : uC); }
`;

const motesVertex = /* glsl */ `
${common}
uniform float uSwirl;
uniform float uRadius;
uniform float uHeight;
uniform vec3 uOrigin;
void main() {
  float t = uTime;
  float speed = (0.025 + 0.035 * seed.w) * (1.0 + uSwirl * 1.6);
  float life = fract(seed.z + t * speed);
  float rad = uRadius * mix(0.3, 1.0, seed.y) * mix(1.0, 0.6, uSwirl) * (1.0 + 0.15 * sin(life * 6.2831 + seed.x * 9.0));
  float ang = seed.x * 6.2831 + t * (0.06 + 0.1 * seed.w + uSwirl * (1.4 - seed.y * 0.6));
  vec3 p = uOrigin + vec3(cos(ang) * rad, life * uHeight, sin(ang) * rad);
  p.x += sin(t * 0.7 + seed.w * 20.0) * 0.025 * uRadius;
  vec4 mv = modelViewMatrix * vec4(p, 1.0);
  gl_Position = projectionMatrix * mv;
  gl_PointSize = uSize * (0.35 + seed.w * 1.0) * uPixel / max(-mv.z, 0.05);
  vAlpha = smoothstep(0.0, 0.12, life) * smoothstep(1.0, 0.55, life) * (0.25 + 0.6 * seed.y);
  vSpark = step(0.93, seed.w);
  vCol = pick(seed);
}
`;

const burstVertex = /* glsl */ `
${common}
uniform float uProgress;
uniform vec3 uOrigin;
uniform float uSpread;
void main() {
  float k = 1.0 - pow(1.0 - uProgress, 3.0);
  float th = seed.x * 6.2831;
  float ph = acos(1.0 - 2.0 * seed.y);
  vec3 dir = vec3(sin(ph) * cos(th), cos(ph) * 0.8 + 0.25, sin(ph) * sin(th));
  vec3 p = uOrigin + dir * uSpread * k * (0.5 + seed.z) + vec3(0.0, -0.3 * uSpread * uProgress * uProgress, 0.0);
  vec4 mv = modelViewMatrix * vec4(p, 1.0);
  gl_Position = projectionMatrix * mv;
  gl_PointSize = uSize * (0.6 + seed.w) * uPixel / max(-mv.z, 0.05) * (1.0 - uProgress * 0.6);
  vAlpha = (1.0 - uProgress) * step(0.0001, uProgress) * step(uProgress, 0.999);
  vSpark = step(0.6, seed.w);
  vCol = pick(seed);
}
`;

const fragment = /* glsl */ `
uniform float uBoost;
varying float vAlpha;
varying float vSpark;
varying vec3 vCol;
void main() {
  vec2 q = gl_PointCoord - 0.5;
  float d = length(q);
  float a = exp(-d * d * 30.0);
  // Four-point sparkle on a few of them.
  float cross = (smoothstep(0.06, 0.0, abs(q.x)) + smoothstep(0.06, 0.0, abs(q.y))) * smoothstep(0.5, 0.0, d);
  a = max(a, cross * vSpark * 0.9);
  a *= vAlpha;
  if (a < 0.003) discard;
  gl_FragColor = vec4(vCol * uBoost, a);
  #include <colorspace_fragment>
}
`;

function seeds(n: number, random: () => number): BufferGeometry {
  const g = new BufferGeometry();
  g.setAttribute('position', new BufferAttribute(new Float32Array(n * 3), 3));
  const s = new Float32Array(n * 4);
  for (let i = 0; i < s.length; i++) s[i] = random();
  g.setAttribute('seed', new BufferAttribute(s, 4));
  return g;
}

function material(vertexShader: string, extra: Record<string, { value: unknown }>): ShaderMaterial {
  return new ShaderMaterial({
    vertexShader,
    fragmentShader: fragment,
    transparent: true,
    depthWrite: false,
    blending: AdditiveBlending,
    toneMapped: false,
    uniforms: {
      uTime: { value: 0 },
      uPixel: { value: 1 },
      uSize: { value: 0.04 },
      uBoost: { value: 1.6 },
      uA: { value: new Color() },
      uB: { value: new Color() },
      uC: { value: new Color() },
      ...extra,
    },
  });
}

function tint(m: ShaderMaterial, a: Color, b: Color, c: Color, light: boolean): void {
  const u = m.uniforms;
  (u.uA!.value as Color).copy(a);
  (u.uB!.value as Color).copy(b);
  (u.uC!.value as Color).copy(c);
  u.uBoost!.value = light ? 1 : 1.6;
  m.blending = light ? NormalBlending : AdditiveBlending;
  m.needsUpdate = true;
}

export class Motes {
  readonly points: Points;
  readonly material: ShaderMaterial;
  private count: number;

  constructor(
    max: number,
    opts: { origin: Vector3; radius: number; height: number; size: number },
    random: () => number = Math.random,
  ) {
    this.count = max;
    this.material = material(motesVertex, {
      uSwirl: { value: 0 },
      uRadius: { value: opts.radius },
      uHeight: { value: opts.height },
      uOrigin: { value: opts.origin.clone() },
    });
    this.material.uniforms.uSize!.value = opts.size;
    this.points = new Points(seeds(max, random), this.material);
    this.points.name = 'StageMotes';
    this.points.frustumCulled = false;
    this.points.renderOrder = 2;
    this.points.raycast = () => {};
  }

  setCount(n: number): void {
    this.count = Math.max(0, Math.min(n, this.points.geometry.getAttribute('position').count));
    this.points.geometry.setDrawRange(0, this.count);
    this.points.visible = this.count > 0;
  }

  setColors(a: Color, b: Color, c: Color, light: boolean): void {
    tint(this.material, a, b, c, light);
  }

  dispose(): void {
    this.points.geometry.dispose();
    this.material.dispose();
  }
}

export class Burst {
  readonly points: Points;
  readonly material: ShaderMaterial;
  private t = -1;
  private readonly duration = 1.1;

  constructor(n: number, spread: number, size: number, random: () => number = Math.random) {
    this.material = material(burstVertex, {
      uProgress: { value: 0 },
      uOrigin: { value: new Vector3() },
      uSpread: { value: spread },
    });
    this.material.uniforms.uSize!.value = size;
    this.points = new Points(seeds(n, random), this.material);
    this.points.name = 'StageBurst';
    this.points.frustumCulled = false;
    this.points.visible = false;
    this.points.renderOrder = 3;
    this.points.raycast = () => {};
  }

  fire(at: Vector3): void {
    (this.material.uniforms.uOrigin!.value as Vector3).copy(at);
    this.t = 0;
    this.points.visible = true;
  }

  update(dt: number): void {
    if (this.t < 0) return;
    this.t += dt;
    const p = this.t / this.duration;
    if (p >= 1) {
      this.t = -1;
      this.points.visible = false;
      this.material.uniforms.uProgress!.value = 0;
      return;
    }
    this.material.uniforms.uProgress!.value = p;
  }

  setColors(a: Color, b: Color, c: Color, light: boolean): void {
    tint(this.material, a, b, c, light);
  }

  dispose(): void {
    this.points.geometry.dispose();
    this.material.dispose();
  }
}
