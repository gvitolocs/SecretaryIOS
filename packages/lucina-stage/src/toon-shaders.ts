/**
 * Inverse of the two ACES matrices in three r186's `tonemapping_pars_fragment` (row-major).
 * The bloom pipeline's OutputPass tone-maps the whole frame, including `toneMapped = false`
 * toon pixels, so those pixels ship the inverse and OutputPass maps them back to Standard.
 */
export const ACES_INVERSE_IN = [1.764740972, -0.6757776782, -0.0889632938, -0.147027852, 1.1602515117, -0.0132236597, -0.0363368301, -0.1624364369, 1.198773267];
export const ACES_INVERSE_OUT = [0.6430382486, 0.3111867518, 0.0457754574, 0.0592686904, 0.9314364869, 0.0092949157, 0.0059619013, 0.0639290157, 0.9301183842];

/** GLSL column-major mat3 from a row-major array. */
const mat3 = (m: number[]): string =>
  `mat3(vec3(${m[0]}, ${m[3]}, ${m[6]}), vec3(${m[1]}, ${m[4]}, ${m[7]}), vec3(${m[2]}, ${m[5]}, ${m[8]}))`;

export const toonVertex = /* glsl */ `
#include <common>
#include <morphtarget_pars_vertex>
#include <skinning_pars_vertex>
varying vec2 tUv;
varying vec3 tNormal;
varying vec3 tView;
uniform float outlineWidth;
uniform float outlineMax;
#ifdef OUTLINE_COLOR
attribute vec3 color;
#endif
#ifdef OUTLINE_WEIGHT
attribute float outlineWeight;
#endif
void main() {
  tUv = uv;
  #include <beginnormal_vertex>
  #include <morphinstance_vertex>
  #include <morphnormal_vertex>
  #include <skinbase_vertex>
  #include <skinnormal_vertex>
  #include <defaultnormal_vertex>
  tNormal = normalize(transformedNormal);
  #include <begin_vertex>
  #include <morphtarget_vertex>
  #include <skinning_vertex>
  float k = 1.0;
  #ifdef OUTLINE_COLOR
  k = clamp(color.r / outlineMax, 0.0, 1.0);
  #endif
  // "shell" weight: CPU-classified per-vertex extrusion, with 0 meaning no hull at all.
  #ifdef OUTLINE_WEIGHT
  k = outlineWeight;
  #endif
  // Extrude in world space, then return to local space (including nonuniform scale).
  vec3 wn = inverseTransformDirection(normalize(normalMatrix * objectNormal), viewMatrix);
  mat3 m = mat3(modelMatrix);
  vec3 localPush = vec3(dot(cross(m[1], m[2]), wn), dot(cross(m[2], m[0]), wn), dot(cross(m[0], m[1]), wn)) / dot(m[0], cross(m[1], m[2]));
  #ifdef OUTLINE_WEIGHT
  transformed += localPush * outlineWidth * k;
  #else
  transformed += localPush * outlineWidth * (0.15 + 0.85 * k);
  #endif
  #include <project_vertex>
  tView = -mvPosition.xyz;
}
`;

export const toonFragment = /* glsl */ `
#include <common>
varying vec2 tUv;
varying vec3 tNormal;
varying vec3 tView;
uniform sampler2D albedoMap;
uniform sampler2D multiMap;
uniform sampler2D ramps;
uniform sampler2D normalMap;
uniform mat3 mapTransform;
uniform mat3 multiTransform;
uniform mat3 normalTransform;
uniform vec2 normalScale;
uniform vec3 tint;
uniform vec3 lightDirection;
uniform vec3 emission;
uniform vec3 ringColor;
uniform vec3 rimColor;
uniform vec3 outlineColor;
uniform float ambient;
uniform float rowV;
uniform float metalRowV;
uniform float saturation;
uniform float detail;
uniform float detailMean;
uniform float emissionAmount;
uniform float rim;
uniform float opacity;
uniform float alphaTest;
uniform float albedoMix;
uniform float inverseTone;
uniform float toneExposure;
float lum(vec3 c) { return dot(c, vec3(0.2126, 0.7152, 0.0722)); }

// Inverse of three r186 ACESFilmicToneMapping (forward: c *= exposure / 0.6, ACESInputMat,
// RRTAndODTFit per channel, ACESOutputMat, saturate). Only ACES is inverted here; AgX/Neutral
// keep inverseTone = 0 because their forward transform has no closed-form inverse in the shader.
vec3 acesInverse(vec3 c) {
  const mat3 invOut = ${mat3(ACES_INVERSE_OUT)};
  const mat3 invIn = ${mat3(ACES_INVERSE_IN)};
  vec3 w = clamp(invOut * clamp(c, 0.0, 1.0), 0.0, 0.999);
  vec3 a = vec3(1.0) - 0.983729 * w;
  vec3 b = vec3(0.0245786) - 0.4329510 * w;
  vec3 k = -(vec3(0.000090537) + 0.238081 * w);
  vec3 v = (-b + sqrt(max(b * b - 4.0 * a * k, vec3(0.0)))) / (2.0 * a);
  return max(invIn * v * (0.6 / max(toneExposure, 0.0001)), vec3(0.0));
}

void main() {
  vec2 uv = (mapTransform * vec3(tUv, 1.0)).xy;
  vec4 texel = vec4(1.0);
  #ifdef HAS_MAP
  texel = texture2D(albedoMap, uv);
  #endif
  float alpha = opacity;
  #ifdef MAP_ALPHA
  alpha *= texel.a;
  #endif
  #ifdef ALPHA_TO_COVERAGE
  alpha = smoothstep(alphaTest, alphaTest + max(fwidth(alpha), 0.0001), alpha);
  if (alpha == 0.0) discard;
  #else
  if (alpha < alphaTest) discard;
  #endif
  vec3 albedo = texel.rgb * tint;
  #ifdef SOLID_COLOR
  albedo = tint * mix(1.0, lum(texel.rgb) / detailMean, detail);
  #endif
  albedo = mix(vec3(lum(albedo)), albedo, saturation);
  vec3 col;
  #ifdef OUTLINE
  col = mix(outlineColor, albedo, albedoMix);
  #else
  float face = gl_FrontFacing ? 1.0 : -1.0;
  vec3 N = normalize(tNormal) * face;
  vec3 V = normalize(tView);
  #ifdef HAS_NORMAL
  vec2 nuv = (normalTransform * vec3(tUv, 1.0)).xy;
  vec3 q0 = dFdx(-tView), q1 = dFdy(-tView);
  vec2 st0 = dFdx(nuv), st1 = dFdy(nuv);
  vec3 T = cross(q1, N) * st0.x + cross(N, q0) * st1.x;
  vec3 B = cross(q1, N) * st0.y + cross(N, q0) * st1.y;
  float det = max(dot(T,T), dot(B,B));
  float inv = det > 0.0 ? inversesqrt(det) : 0.0;
  vec3 mapN = texture2D(normalMap, nuv).xyz * 2.0 - 1.0;
  mapN.xy *= normalScale;
  N = normalize(mat3(T * inv * face, B * inv * face, N) * mapN);
  #endif
  vec3 L = normalize(lightDirection);
  float nl = saturate(dot(N,L));
  float U = nl + ambient;
  vec4 multi = vec4(1.0, 0.0, 0.0, 1.0);
  #ifdef HAS_MULTI
  multi = texture2D(multiMap, (multiTransform * vec3(tUv, 1.0)).xy);
  #ifdef ORM_MULTI
  multi = vec4(1.0, multi.b, 0.0, 1.0);
  #endif
  U *= mix(0.45, 1.0, multi.r);
  #endif
  #ifdef FLAT_TOON
  U = 0.7 + 0.3 * saturate(U);
  #endif
  vec3 ramp = texture2D(ramps, vec2(saturate(U), rowV)).rgb;
  ramp = mix(ramp, vec3(lum(ramp)), 0.25);
  ramp = mix(ramp, vec3(1.0), 0.30);
  col = albedo * ramp;
  #ifdef METAL_RAMP
  float a = 0.35 * 0.35;
  vec3 H = normalize(L + V);
  float nh = saturate(dot(N,H)), nv = max(dot(N,V), 0.001);
  float d = a*a / pow(nh*nh*(a*a - 1.0) + 1.0, 2.0);
  float s = saturate(d * nl / (4.0 * nv) + 0.04);
  col = mix(col, col * texture2D(ramps, vec2(s, metalRowV)).rgb, multi.g);
  #endif
  #ifdef ANGEL_RING
  float ny = inverseTransformDirection(N, viewMatrix).y;
  float band = smoothstep(0.30,0.55,ny) * (1.0-smoothstep(0.72,0.92,ny)) * 0.6;
  col = mix(col, 1.0-(1.0-col)*(1.0-ringColor), band);
  #endif
  col += emission * emissionAmount;
  float f = 1.0 - pow(abs(dot(N,V)), 0.6);
  col += rimColor * smoothstep(0.45,0.85,f) * rim;
  #endif
  if (inverseTone > 0.5) col = acesInverse(col);
  gl_FragColor = vec4(col, alpha);
  #include <colorspace_fragment>
  #include <premultiplied_alpha_fragment>
}
`;
