import { afterEach, describe, expect, it, vi } from "vitest";
import { isFaceTrackingSupported, startFaceTracking } from "./faceTrack";

const solver = { solve: () => ({}), recenter: () => {} };

describe("startFaceTracking", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("is unsupported without a camera API", async () => {
    vi.stubGlobal("navigator", {});
    expect(isFaceTrackingSupported()).toBe(false);
    await expect(startFaceTracking({ solver, onDrive: () => {} })).rejects.toBe("unsupported");
  });

  it("maps a refused permission to 'denied' and never loads the model", async () => {
    const getUserMedia = vi.fn().mockRejectedValue(Object.assign(new Error("no"), { name: "NotAllowedError" }));
    vi.stubGlobal("navigator", { mediaDevices: { getUserMedia } });
    await expect(startFaceTracking({ solver, onDrive: () => {} })).rejects.toBe("denied");
    expect(getUserMedia).toHaveBeenCalledWith(expect.objectContaining({ audio: false, video: expect.objectContaining({ facingMode: "user" }) }));
  });

  it("maps a missing camera to 'no-camera'", async () => {
    const getUserMedia = vi.fn().mockRejectedValue(Object.assign(new Error("none"), { name: "NotFoundError" }));
    vi.stubGlobal("navigator", { mediaDevices: { getUserMedia } });
    await expect(startFaceTracking({ solver, onDrive: () => {} })).rejects.toBe("no-camera");
  });
});
