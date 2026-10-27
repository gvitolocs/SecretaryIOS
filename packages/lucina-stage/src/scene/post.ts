/**
 * Optional bloom: RenderPass -> UnrealBloomPass -> OutputPass (tone mapping + sRGB).
 * Only built on the "high" tier; everything else renders straight to the canvas, where the
 * additive floor/particle shaders already read as glowing.
 */
import { ACESFilmicToneMapping, HalfFloatType, Vector2, type Camera, type IUniform, type Scene, type WebGLRenderer } from 'three';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';

/**
 * Lets UnrealBloomPass's luminosity high pass work on ACES display values (displayInverse = 1) and pass only
 * the part above the threshold, like Blender's glare on the Standard-view image, instead of the whole
 * (unbounded) scene value that the toon ACES inverse produces for colours near white.
 */
function displaySpaceHighPass(pass: UnrealBloomPass): void {
  const m = pass.materialHighPassFilter;
  const u = m.uniforms as Record<string, IUniform>; // the same object as pass.highPassUniforms
  u.displayInverse = { value: 0 };
  u.displayExposure = { value: 1 };
  m.fragmentShader = m.fragmentShader
    .replace(
      'void main() {',
      `uniform float displayInverse;
uniform float displayExposure;
vec3 bloomRRT(vec3 v) { return (v * (v + 0.0245786) - 0.000090537) / (v * (0.983729 * v + 0.4329510) + 0.238081); }
vec3 bloomAces(vec3 c) {
  const mat3 inM = mat3(vec3(0.59719, 0.07600, 0.02840), vec3(0.35458, 0.90834, 0.13383), vec3(0.04823, 0.01566, 0.83777));
  const mat3 outM = mat3(vec3(1.60475, -0.10208, -0.00327), vec3(-0.53108, 1.10813, -0.07276), vec3(-0.07367, -0.00605, 1.07602));
  return clamp(outM * bloomRRT(inM * (c * displayExposure / 0.6)), 0.0, 1.0);
}
void main() {`,
    )
    .replace(
      'vec4 texel = texture2D( tDiffuse, vUv );',
      'vec4 texel = texture2D( tDiffuse, vUv );\n\t\t\tif ( displayInverse > 0.5 ) texel.rgb = bloomAces( texel.rgb );',
    )
    // Blender's glare passes only the part above the threshold; UnrealBloomPass passes the whole colour.
    .replace(
      'gl_FragColor = mix( outputColor, texel, alpha );',
      'if ( displayInverse > 0.5 ) texel.rgb = max( texel.rgb - vec3( luminosityThreshold ), vec3( 0.0 ) );\n\t\t\tgl_FragColor = mix( outputColor, texel, alpha );',
    );
  m.needsUpdate = true;
}

export class BloomPipeline {
  private readonly composer: EffectComposer;
  private readonly bloom: UnrealBloomPass;
  private readonly toon: { threshold: number; strength: number; radius: number } | undefined;
  private light = false;
  // Scene-space threshold cache: bisection only when the exposure or theme (or pipeline mode) changes.
  private cache = { exposure: NaN, light: false, display: NaN, inverse: false };

  constructor(
    private readonly renderer: WebGLRenderer,
    private readonly scene: Scene,
    camera: Camera,
  ) {
    this.toon = scene.userData.toonBloom;
    // OutputPass tone-maps the whole image, so toon materials output the ACES inverse.
    scene.userData.toonInverseTone = true;
    const size = renderer.getSize(new Vector2());
    this.composer = new EffectComposer(renderer);
    this.composer.renderTarget1.texture.type = HalfFloatType;
    this.composer.renderTarget2.texture.type = HalfFloatType;
    this.composer.addPass(new RenderPass(scene, camera));
    this.bloom = new UnrealBloomPass(new Vector2(size.x, size.y), 0.5, 0.55, 0.82);
    displaySpaceHighPass(this.bloom);
    this.composer.addPass(this.bloom);
    // OutputPass tone-maps the complete bloom image, including toneMapped=false toon pixels.
    this.composer.addPass(new OutputPass());
    this.syncThreshold();
  }

  setSize(w: number, h: number, dpr: number): void {
    this.composer.setPixelRatio(dpr);
    this.composer.setSize(w, h);
  }

  /** 0..1 extra glow (e.g. while she speaks). */
  setBoost(k: number): void {
    this.bloom.strength = (this.toon?.strength ?? 0.45) + 0.35 * k;
  }

  setTheme(light: boolean): void {
    this.light = light;
    // Bright backgrounds would bloom everywhere: raise the threshold on the light theme.
    this.bloom.radius = light ? 0.35 : (this.toon?.radius ?? 0.55);
    this.syncThreshold();
  }

  /**
   * Toon bloom thresholds are display-space values. While OutputPass tone-maps the frame (and
   * toon pixels carry the ACES inverse), UnrealBloomPass sees scene space, so convert the
   * threshold back through exponentiated ACES; otherwise use the display value unchanged.
   */
  private syncThreshold(): void {
    const display = this.light ? 0.97 : (this.toon?.threshold ?? 0.82);
    const exposure = this.renderer.toneMappingExposure;
    const inverse =
      this.toon !== undefined &&
      this.scene.userData.toonInverseTone === true &&
      this.renderer.toneMapping === ACESFilmicToneMapping;
    const c = this.cache;
    if (c.exposure === exposure && c.light === this.light && c.display === display && c.inverse === inverse) return;
    // With the inverse-tone toon pixels the high pass measures and passes display-space colour (see
    // displaySpaceHighPass): near-white toon albedo (a game face, hands, cuffs) inverts to scene values in the
    // tens, which a scene-space high pass would hand to the blur unclamped and the face would blow out.
    const u = this.bloom.materialHighPassFilter.uniforms;
    u.displayInverse!.value = inverse ? 1 : 0;
    u.displayExposure!.value = exposure;
    this.bloom.threshold = display;
    this.cache = { exposure, light: this.light, display, inverse };
  }

  render(dt: number): void {
    this.syncThreshold();
    this.composer.render(dt);
  }

  dispose(): void {
    this.scene.userData.toonInverseTone = false;
    this.bloom.dispose();
    this.composer.dispose();
  }
}
