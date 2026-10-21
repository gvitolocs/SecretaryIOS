/** Frame-time statistics exported as `stage.stats`. */

export interface FrameSample {
  /** Time since the previous rendered frame (ms). */
  frameMs: number;
  /** CPU time spent in update + render submission this frame (ms). */
  cpuMs: number;
}

export type FrameListener = (sample: FrameSample) => void;

export class FrameStats {
  /** Exponential moving averages. */
  frameMs = 0;
  cpuMs = 0;
  /** Worst frame in the last ~second. */
  maxFrameMs = 0;
  frames = 0;
  private windowMax = 0;
  private windowStart = 0;
  private readonly listeners = new Set<FrameListener>();

  get fps(): number {
    return this.frameMs > 0 ? 1000 / this.frameMs : 0;
  }

  /** Called by the stage each frame. */
  record(frameMs: number, cpuMs: number, now: number): void {
    this.frames++;
    const a = this.frames < 10 ? 0.5 : 0.05;
    this.frameMs = this.frameMs === 0 ? frameMs : this.frameMs + (frameMs - this.frameMs) * a;
    this.cpuMs = this.cpuMs === 0 ? cpuMs : this.cpuMs + (cpuMs - this.cpuMs) * a;
    this.windowMax = Math.max(this.windowMax, frameMs);
    if (now - this.windowStart > 1000) {
      this.maxFrameMs = this.windowMax;
      this.windowMax = 0;
      this.windowStart = now;
    }
    if (this.listeners.size) {
      const s = { frameMs, cpuMs };
      for (const l of this.listeners) l(s);
    }
  }

  /** Subscribes to every frame; returns an unsubscribe function. */
  onFrame(cb: FrameListener): () => void {
    this.listeners.add(cb);
    return () => this.listeners.delete(cb);
  }

  snapshot(): { fps: number; frameMs: number; cpuMs: number; maxFrameMs: number; frames: number } {
    return { fps: this.fps, frameMs: this.frameMs, cpuMs: this.cpuMs, maxFrameMs: this.maxFrameMs, frames: this.frames };
  }

  clear(): void {
    this.listeners.clear();
  }
}
