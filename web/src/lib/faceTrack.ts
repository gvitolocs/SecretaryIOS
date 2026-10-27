// Face tracking on the web (Custom Cast F1): the front camera → MediaPipe Face Landmarker (WASM,
// served from our own origin: /mediapipe/) → blendshapes and head pose → Lucina's stage.
//
// Privacy: the frames stay in this tab. They go from the camera into the WASM model and nowhere
// else: nothing is uploaded, recorded or stored. The camera starts only from a tap (the browser asks
// for permission then) and stops on stop(), when the page is hidden, or when the owner closes Lucina.
// (The MediaPipe runtime also tries to POST anonymous usage counters, no image data, to
// odml.pa.googleapis.com; the app's CSP, connect-src 'self', blocks that request.)

// Type-only: the stage package (three.js) stays behind lib/stageLoader; the solver comes from it.
import type { Blendshapes, FaceDrive } from "@secretary/lucina-stage";

export const FACE_WASM_BASE = "/mediapipe/wasm";
export const FACE_MODEL_URL = "/mediapipe/face_landmarker.task";

export type FaceTrackError = "unsupported" | "denied" | "no-camera" | "failed";

export interface FaceTracker {
  stop(): void;
  /** Takes the current head pose as straight ahead. */
  recenter(): void;
  readonly running: boolean;
}

/** The stage package's `FaceSolver` (blendshapes + head transform → drive, with calibration). */
export interface FaceSolverLike {
  solve(shapes: Blendshapes, matrix?: ArrayLike<number> | null): FaceDrive;
  recenter(): void;
}

export interface FaceTrackOptions {
  solver: FaceSolverLike;
  onDrive: (drive: FaceDrive | null) => void;
  /** Tracking stopped by itself (camera ended, page hidden, error). */
  onStop?: (reason: "ended" | "hidden" | FaceTrackError) => void;
  /** Frames per second to analyse (default 30). */
  fps?: number;
}

export function isFaceTrackingSupported(): boolean {
  return typeof navigator !== "undefined" && !!navigator.mediaDevices?.getUserMedia && typeof WebAssembly === "object";
}

function errorOf(e: unknown): FaceTrackError {
  const name = (e as { name?: string } | null)?.name ?? "";
  if (name === "NotAllowedError" || name === "SecurityError") return "denied";
  if (name === "NotFoundError" || name === "OverconstrainedError" || name === "NotReadableError") return "no-camera";
  return "failed";
}

/**
 * Starts the camera and the landmarker. Call it from a tap: the permission prompt needs the gesture.
 * Rejects with a `FaceTrackError` string.
 */
export async function startFaceTracking(opts: FaceTrackOptions): Promise<FaceTracker> {
  if (!isFaceTrackingSupported()) throw "unsupported" satisfies FaceTrackError;
  // The camera first, while the tap still counts as a user gesture.
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      video: { facingMode: "user", width: { ideal: 640 }, height: { ideal: 480 }, frameRate: { ideal: 30 } },
      audio: false,
    });
  } catch (e) {
    throw errorOf(e);
  }
  const stopStream = () => stream.getTracks().forEach((t) => t.stop());

  let landmarker: import("@mediapipe/tasks-vision").FaceLandmarker;
  try {
    const { FaceLandmarker, FilesetResolver } = await import("@mediapipe/tasks-vision");
    const fileset = await FilesetResolver.forVisionTasks(FACE_WASM_BASE);
    const options = (delegate: "GPU" | "CPU") => ({
      baseOptions: { modelAssetPath: FACE_MODEL_URL, delegate },
      runningMode: "VIDEO" as const,
      numFaces: 1,
      outputFaceBlendshapes: true,
      outputFacialTransformationMatrixes: true,
    });
    landmarker = await FaceLandmarker.createFromOptions(fileset, options("GPU")).catch(() =>
      FaceLandmarker.createFromOptions(fileset, options("CPU")),
    );
  } catch {
    stopStream();
    throw "failed" satisfies FaceTrackError;
  }

  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.setAttribute("playsinline", "");
  video.srcObject = stream;
  try {
    await video.play();
  } catch (e) {
    stopStream();
    landmarker.close();
    throw errorOf(e);
  }

  const solver = opts.solver;
  const minGap = 1000 / (opts.fps ?? 30);
  let running = true;
  let raf = 0;
  let lastTs = -1;
  let lastRun = 0;

  const finish = (reason?: "ended" | "hidden" | FaceTrackError) => {
    if (!running) return;
    running = false;
    cancelAnimationFrame(raf);
    document.removeEventListener("visibilitychange", onVisibility);
    stopStream();
    video.srcObject = null;
    try {
      landmarker.close();
    } catch {
      // already closed
    }
    opts.onDrive(null);
    if (reason) opts.onStop?.(reason);
  };
  const onVisibility = () => {
    if (document.hidden) finish("hidden");
  };
  document.addEventListener("visibilitychange", onVisibility);
  stream.getVideoTracks()[0]?.addEventListener("ended", () => finish("ended"));

  const tick = (now: number) => {
    if (!running) return;
    raf = requestAnimationFrame(tick);
    if (now - lastRun < minGap || video.readyState < 2) return;
    lastRun = now;
    // detectForVideo needs strictly increasing timestamps.
    const ts = Math.max(Math.round(now), lastTs + 1);
    lastTs = ts;
    let result: import("@mediapipe/tasks-vision").FaceLandmarkerResult;
    try {
      result = landmarker.detectForVideo(video, ts);
    } catch {
      finish("failed");
      return;
    }
    const cats = result.faceBlendshapes?.[0]?.categories;
    if (!cats) return; // no face: the stage eases back after its timeout
    const shapes: Record<string, number> = {};
    for (const c of cats) shapes[c.categoryName] = c.score;
    const m = result.facialTransformationMatrixes?.[0]?.data;
    opts.onDrive(solver.solve(shapes, m ?? null));
  };
  raf = requestAnimationFrame(tick);

  return {
    stop: () => finish(),
    recenter: () => solver.recenter(),
    get running() {
      return running;
    },
  };
}
