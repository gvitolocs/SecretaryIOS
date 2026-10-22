/**
 * Optional bloom: RenderPass -> UnrealBloomPass -> OutputPass (tone mapping + sRGB).
 * Only built on the "high" tier; everything else renders straight to the canvas, where the
 * additive floor/particle shaders already read as glowing.
 */
import { ACESFilmicToneMapping, HalfFloatType, Vector2, type Camera, type Scene, type WebGLRenderer } from 'three';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';
import { acesSceneThreshold } from '../toon';

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
    this.bloom.threshold = inverse ? acesSceneThreshold(display, exposure) : display;
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
