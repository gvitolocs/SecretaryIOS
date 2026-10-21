/**
 * Critically damped spring (exact integration, stable for any dt).
 * `halflife` is the time in seconds for the distance to the target to halve.
 */
export class Spring {
  x: number;
  v = 0;

  constructor(initial = 0) {
    this.x = initial;
  }

  step(target: number, halflife: number, dt: number): number {
    const omega = (2 * Math.LN2) / Math.max(halflife, 1e-4);
    const j0 = this.x - target;
    const j1 = this.v + j0 * omega;
    const e = Math.exp(-omega * dt);
    this.x = e * (j0 + j1 * dt) + target;
    this.v = e * (this.v - j1 * omega * dt);
    return this.x;
  }

  /** Adds velocity, e.g. for a nod impulse. */
  kick(velocity: number): void {
    this.v += velocity;
  }

  reset(x = 0): void {
    this.x = x;
    this.v = 0;
  }
}

/** Exponential approach with separate rise (attack) and fall (release) time constants. */
export function attackRelease(current: number, target: number, attack: number, release: number, dt: number): number {
  const tau = target > current ? attack : release;
  const a = 1 - Math.exp(-dt / Math.max(tau, 1e-4));
  return current + (target - current) * a;
}

/** Frame-rate independent exponential smoothing toward `target` with time constant `tau`. */
export function damp(current: number, target: number, tau: number, dt: number): number {
  return current + (target - current) * (1 - Math.exp(-dt / Math.max(tau, 1e-4)));
}
