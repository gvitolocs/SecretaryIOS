/**
 * The few hooks the studio extras need inside a stage's frame (see stage.ts). One hooks object is
 * shared by every stage a wardrobe creates, so poses, props and filters survive outfit swaps.
 */
import type { Object3D, PerspectiveCamera, Scene, WebGLRenderer } from 'three';
import type { LogicalBone } from '../types';

export interface FrameContext {
  dt: number;
  scene: Scene;
  camera: PerspectiveCamera;
  /** The loaded model (the GLB scene, or the placeholder). */
  model: Object3D;
  renderer: WebGLRenderer;
  canvas: HTMLCanvasElement;
  /** Logical bone → node name, as the rig bound them. */
  bones: Partial<Record<LogicalBone, string>>;
  /** Her height and the floor (world space). */
  height: number;
  floorY: number;
  reducedMotion: boolean;
  dancing: boolean;
  placeholder: boolean;
}

export interface StageHooks {
  /**
   * Called every frame right after the body is posed by the clips and the procedural layer, before
   * spring-bone physics: pose overrides (held poses, dance moves, hands, face morphs).
   */
  readonly pose: Set<(ctx: FrameContext) => void>;
  /**
   * Called every frame after physics and the camera are placed, right before the render: camera
   * roll, filters, scene extras.
   */
  readonly frame: Set<(ctx: FrameContext) => void>;
  /** Gaze override in character space (radians: yaw > 0 toward +X, her left; pitch > 0 down), or null: the camera. */
  gaze: { yaw: number; pitch: number } | null;
}

export function createStageHooks(): StageHooks {
  return { pose: new Set(), frame: new Set(), gaze: null };
}
