/**
 * Animated nebula sky drawn as a full-screen quad behind everything: a vertical gradient,
 * domain-warped fbm clouds, twinkling stars (dark theme), a halo behind Lucina that brightens
 * with her voice, and a soft vignette. Cheap: one pass, 2-5 noise octaves.
 */
import { Color, Mesh, PlaneGeometry, ShaderMaterial, Vector2 } from 'three';

const vertex = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = vec4(position.xy, 1.0, 1.0);
}
`;

const fragment = /* glsl */ `
uniform float uTime;
uniform float uAspect;
uniform float uDetail;
uniform float uLight;
uniform float uListen;
uniform float uThink;
uniform float uLevel;
uniform vec2 uCenter;
uniform vec2 uParallax;
uniform vec3 uTop;
uniform vec3 uMid;
uniform vec3 uBottom;
uniform vec3 uNebA;
uniform vec3 uNebB;
uniform vec3 uHalo;
varying vec2 vUv;

float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}
float noise(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  vec2 u = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) {
  float v = 0.0;
  float a = 0.5;
  for (int i = 0; i < 5; i++) {
    if (float(i) >= uDetail) break;
    v += a * noise(p);
    p = p * 2.03 + vec2(1.7, 9.2);
    a *= 0.5;
  }
  return v;
}

void main() {
  vec2 uv = vUv;
  vec2 p = vec2((uv.x - 0.5) * uAspect, uv.y - 0.5) + uParallax;
  float t = uTime;

  vec3 col = mix(uBottom, uMid, smoothstep(0.0, 0.5, uv.y));
  col = mix(col, uTop, smoothstep(0.5, 1.05, uv.y));

  if (uDetail > 0.5) {
    vec2 q = p * 1.5 + vec2(t * 0.011, -t * 0.007);
    vec2 w = vec2(fbm(q + vec2(0.0, t * 0.018)), fbm(q + vec2(5.2, 1.3) - t * 0.014));
    float n = fbm(q + 1.9 * w + uThink * vec2(sin(t * 0.6), cos(t * 0.5)) * 0.15);
    float neb = smoothstep(0.3, 0.9, n);
    vec3 nebCol = mix(uNebA, uNebB, clamp(w.x * 1.5 - 0.25, 0.0, 1.0));
    col += nebCol * neb * (0.95 + 0.4 * uThink);
    // Thin bright filaments where the warp folds.
    float fil = smoothstep(0.015, 0.0, abs(n - 0.62)) * 0.25;
    col += nebCol * fil * (1.0 - uLight * 0.5);
  }

  // Stars, only on the dark sky and mostly in the upper part.
  vec2 sg = (uv * vec2(uAspect, 1.0) + uParallax * 0.5) * 80.0;
  vec2 id = floor(sg);
  float h = hash(id);
  vec2 f = fract(sg) - 0.5 - (vec2(hash(id + 3.1), hash(id + 7.7)) - 0.5) * 0.6;
  float star = step(0.982, h) * smoothstep(0.09, 0.0, length(f));
  star *= 0.55 + 0.45 * sin(t * (1.2 + h * 3.0) + h * 40.0);
  col += vec3(0.85, 0.92, 1.0) * star * (1.0 - uLight) * smoothstep(0.2, 0.65, uv.y);

  // Halo behind her: breathes slowly, swells with her voice and while listening.
  vec2 d = vec2((uv.x - uCenter.x) * uAspect, (uv.y - uCenter.y) * 0.75);
  float halo = exp(-dot(d, d) * 16.0);
  float pulse = 0.5 + 0.5 * sin(t * 0.9);
  col += uHalo * halo * (0.11 + 0.03 * pulse + 0.32 * uLevel + 0.08 * uListen);

  // Horizon haze: a faint band where the floor meets the sky.
  col += uHalo * 0.12 * exp(-pow((uv.y - uCenter.y + 0.42) * 7.0, 2.0)) * (1.0 - uLight * 0.4);

  float vig = smoothstep(1.3, 0.3, length(p * vec2(0.85, 1.15)));
  col *= mix(1.0, vig, 0.75 - uLight * 0.55);

  gl_FragColor = vec4(col, 1.0);
  #include <colorspace_fragment>
}
`;

export interface SkyPalette {
  top: Color;
  mid: Color;
  bottom: Color;
  nebA: Color;
  nebB: Color;
  halo: Color;
  light: number;
}

export class Sky {
  readonly mesh: Mesh;
  readonly material: ShaderMaterial;

  constructor() {
    this.material = new ShaderMaterial({
      vertexShader: vertex,
      fragmentShader: fragment,
      depthTest: false,
      depthWrite: false,
      toneMapped: false,
      uniforms: {
        uTime: { value: 0 },
        uAspect: { value: 1 },
        uDetail: { value: 4 },
        uLight: { value: 0 },
        uListen: { value: 0 },
        uThink: { value: 0 },
        uLevel: { value: 0 },
        uCenter: { value: new Vector2(0.5, 0.6) },
        uParallax: { value: new Vector2() },
        uTop: { value: new Color() },
        uMid: { value: new Color() },
        uBottom: { value: new Color() },
        uNebA: { value: new Color() },
        uNebB: { value: new Color() },
        uHalo: { value: new Color() },
      },
    });
    this.mesh = new Mesh(new PlaneGeometry(2, 2), this.material);
    this.mesh.name = 'StageSky';
    this.mesh.frustumCulled = false;
    this.mesh.renderOrder = -100;
    this.mesh.raycast = () => {};
  }

  setPalette(p: SkyPalette): void {
    const u = this.material.uniforms;
    (u.uTop!.value as Color).copy(p.top);
    (u.uMid!.value as Color).copy(p.mid);
    (u.uBottom!.value as Color).copy(p.bottom);
    (u.uNebA!.value as Color).copy(p.nebA);
    (u.uNebB!.value as Color).copy(p.nebB);
    (u.uHalo!.value as Color).copy(p.halo);
    u.uLight!.value = p.light;
  }

  dispose(): void {
    this.mesh.geometry.dispose();
    this.material.dispose();
  }
}
