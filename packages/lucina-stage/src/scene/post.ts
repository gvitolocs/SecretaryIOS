/**
 * Optional bloom: RenderPass -> UnrealBloomPass -> OutputPass (tone mapping + sRGB).
 * Only built on the "high" tier; everything else renders straight to the canvas, where the
 * additive floor/particle shaders already read as glowing.
 */
import { HalfFloatType, Vector2, type Camera, type Scene, type WebGLRenderer } from 'three';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';

export class BloomPipeline {
  private readonly composer: EffectComposer;
  private readonly bloom: UnrealBloomPass;

  constructor(renderer: WebGLRenderer, scene: Scene, camera: Camera) {
    const size = renderer.getSize(new Vector2());
    this.composer = new EffectComposer(renderer);
    this.composer.renderTarget1.texture.type = HalfFloatType;
    this.composer.renderTarget2.texture.type = HalfFloatType;
    this.composer.addPass(new RenderPass(scene, camera));
    this.bloom = new UnrealBloomPass(new Vector2(size.x, size.y), 0.5, 0.55, 0.82);
    this.composer.addPass(this.bloom);
    this.composer.addPass(new OutputPass());
  }

  setSize(w: number, h: number, dpr: number): void {
    this.composer.setPixelRatio(dpr);
    this.composer.setSize(w, h);
  }

  /** 0..1 extra glow (e.g. while she speaks). */
  setBoost(k: number): void {
    this.bloom.strength = 0.45 + 0.35 * k;
  }

  setTheme(light: boolean): void {
    // Bright backgrounds would bloom everywhere: raise the threshold on the light theme.
    this.bloom.threshold = light ? 0.97 : 0.82;
    this.bloom.radius = light ? 0.35 : 0.55;
  }

  render(dt: number): void {
    this.composer.render(dt);
  }

  dispose(): void {
    this.bloom.dispose();
    this.composer.dispose();
  }
}
