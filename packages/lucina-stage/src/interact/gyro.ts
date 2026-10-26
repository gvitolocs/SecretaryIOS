/**
 * Gyro parallax (Custom Cast's `GyroCamMove`): tilting the phone turns the camera a little around
 * her. Low-passed, with a dead zone; at most ±8° yaw and ±5° pitch. The resting angle is
 * whatever the phone was at when it was turned on (and drifts slowly toward the current hold).
 */

export interface GyroOffset {
  yaw: number;
  pitch: number;
}

const DEG = Math.PI / 180;
export const GYRO_LIMITS = { yaw: 8 * DEG, pitch: 5 * DEG, deadZone: 0.6 * DEG };

/** Pure: offset from the device angles (degrees: beta front/back, gamma left/right) and a base. */
export function gyroOffset(beta: number, gamma: number, base: { beta: number; gamma: number }): GyroOffset {
  const dz = (v: number) => (Math.abs(v) < GYRO_LIMITS.deadZone ? 0 : v - Math.sign(v) * GYRO_LIMITS.deadZone);
  const db = dz((beta - base.beta) * DEG);
  const dg = dz((gamma - base.gamma) * DEG);
  const clampTo = (v: number, m: number) => Math.max(-m, Math.min(m, v));
  return { yaw: clampTo(-dg * 0.5, GYRO_LIMITS.yaw) + 0, pitch: clampTo(db * 0.35, GYRO_LIMITS.pitch) + 0 };
}

/** Asks for motion permission (iOS Safari/WKWebView); must run inside a user gesture. */
export async function requestMotionPermission(): Promise<boolean> {
  const D = (globalThis as { DeviceOrientationEvent?: { requestPermission?: () => Promise<string> } }).DeviceOrientationEvent;
  if (!D) return false;
  if (typeof D.requestPermission !== 'function') return true;
  try {
    return (await D.requestPermission()) === 'granted';
  } catch {
    return false;
  }
}

export class GyroParallax {
  readonly offset: GyroOffset = { yaw: 0, pitch: 0 };
  private target: GyroOffset = { yaw: 0, pitch: 0 };
  private base: { beta: number; gamma: number } | null = null;
  private on = false;
  private readonly handler = (e: DeviceOrientationEvent): void => {
    if (e.beta === null || e.gamma === null) return;
    if (!this.base) this.base = { beta: e.beta, gamma: e.gamma };
    // The rest angle follows the hold slowly (about 8 s), so a new grip re-centres.
    this.base.beta += (e.beta - this.base.beta) * 0.002;
    this.base.gamma += (e.gamma - this.base.gamma) * 0.002;
    this.target = gyroOffset(e.beta, e.gamma, this.base);
  };

  get enabled(): boolean {
    return this.on;
  }

  enable(on: boolean): void {
    if (on === this.on || typeof window === 'undefined') return;
    this.on = on;
    this.base = null;
    if (on) window.addEventListener('deviceorientation', this.handler);
    else {
      window.removeEventListener('deviceorientation', this.handler);
      this.target = { yaw: 0, pitch: 0 };
    }
  }

  update(dt: number): void {
    const k = 1 - Math.exp(-dt / 0.18);
    this.offset.yaw += (this.target.yaw - this.offset.yaw) * k;
    this.offset.pitch += (this.target.pitch - this.offset.pitch) * k;
  }

  dispose(): void {
    this.enable(false);
  }
}
