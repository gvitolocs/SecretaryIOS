/**
 * Holographic platform under Lucina's feet: concentric rings, rotating dashed arcs, tick marks,
 * a faint grid, a core glow that follows her voice, and ripples that roll outward while she
 * listens. One transparent disc, additive on the dark theme, alpha-blended on the light one.
 */
import { AdditiveBlending, CircleGeometry, Color, Mesh, NormalBlending, ShaderMaterial } from 'three';

const vertex = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
}
`;

const fragment = /* glsl */ `
#define TAU 6.28318530718
uniform float uTime;
uniform float uListen;
uniform float uThink;
uniform float uLevel;
uniform float uTap;
uniform float uBoost;
uniform vec3 uA;
uniform vec3 uB;
uniform vec3 uC;
varying vec2 vUv;

float ring(float r, float c, float w) { return smoothstep(w, 0.0, abs(r - c)); }

void main() {
  vec2 p = vUv * 2.0 - 1.0;
  float r = length(p);
  if (r > 1.0) discard;
  float a = atan(p.y, p.x);
  float t = uTime;
  float spin = t * (0.12 + 0.8 * uThink);
  float fade = smoothstep(1.0, 0.5, r);

  float c = 0.0;
  c += ring(r, 0.30, 0.010) * 0.9;
  c += ring(r, 0.60, 0.007) * 0.65;
  c += ring(r, 0.88, 0.005) * 0.45;
  float s1 = fract((a + spin) / TAU * 28.0);
  c += ring(r, 0.45, 0.018) * step(0.45, s1) * 0.5;
  float s2 = fract((a - spin * 1.7) / TAU * 3.0);
  c += ring(r, 0.74, 0.016) * smoothstep(0.0, 0.02, s2) * step(s2, 0.3) * 0.85;
  float s3 = fract((a + spin * 0.6 + 1.0) / TAU * 2.0);
  c += ring(r, 0.34, 0.022) * smoothstep(0.0, 0.03, s3) * step(s3, 0.18) * 0.7;
  float tick = step(0.9, fract(a / TAU * 90.0));
  c += tick * step(0.8, r) * step(r, 0.85) * 0.45;
  vec2 g = abs(fract(p * 7.0) - 0.5);
  c += smoothstep(0.47, 0.5, max(g.x, g.y)) * 0.1 * smoothstep(0.95, 0.25, r);

  float rip = 0.0;
  for (int i = 0; i < 3; i++) {
    float ph = fract(t * 0.5 + float(i) / 3.0);
    rip += ring(r, 0.18 + ph * 0.8, 0.03) * (1.0 - ph);
  }
  c += rip * uListen * 1.3;
  // Tap: one fast shock ring.
  c += ring(r, 0.2 + uTap * 0.85, 0.04) * (1.0 - uTap) * step(0.001, uTap) * 1.5;

  float core = exp(-r * r * 12.0) * (0.5 + 1.1 * uLevel + 0.25 * uListen);
  vec3 col = mix(uA, uB, 0.5 + 0.5 * sin(a * 2.0 + t * 0.35));
  col = mix(col, uC, uThink * 0.75);
  float inten = c * fade + core;
  vec3 rgb = (col * c * fade + mix(uA, uB, 0.3) * core) / max(inten, 1e-3);
  gl_FragColor = vec4(rgb * uBoost, clamp(inten, 0.0, 1.0));
  #include <colorspace_fragment>
}
`;

export class HoloFloor {
  readonly mesh: Mesh;
  readonly material: ShaderMaterial;

  constructor(radius: number) {
    this.material = new ShaderMaterial({
      vertexShader: vertex,
      fragmentShader: fragment,
      transparent: true,
      depthWrite: false,
      blending: AdditiveBlending,
      toneMapped: false,
      uniforms: {
        uTime: { value: 0 },
        uListen: { value: 0 },
        uThink: { value: 0 },
        uLevel: { value: 0 },
        uTap: { value: 0 },
        uBoost: { value: 1.4 },
        uA: { value: new Color() },
        uB: { value: new Color() },
        uC: { value: new Color() },
      },
    });
    this.mesh = new Mesh(new CircleGeometry(radius, 96), this.material);
    this.mesh.name = 'StageFloor';
    this.mesh.rotation.x = -Math.PI / 2;
    this.mesh.renderOrder = -2;
    this.mesh.raycast = () => {};
  }

  setColors(a: Color, b: Color, c: Color, light: boolean): void {
    const u = this.material.uniforms;
    (u.uA!.value as Color).copy(a);
    (u.uB!.value as Color).copy(b);
    (u.uC!.value as Color).copy(c);
    u.uBoost!.value = light ? 1 : 1.5;
    this.material.blending = light ? NormalBlending : AdditiveBlending;
    this.material.needsUpdate = true;
  }

  dispose(): void {
    this.mesh.geometry.dispose();
    this.material.dispose();
  }
}
