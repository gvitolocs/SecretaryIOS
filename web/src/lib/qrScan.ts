// Reading QR sign-in codes in the signed-in app (views/ScanQr.tsx): what a scanned or typed
// value means, and the camera decoder (BarcodeDetector where the browser has it, jsQR otherwise;
// iOS Safari has no BarcodeDetector).

/** Hosts whose /approve links this app answers: its own, and the production one. */
const APPROVE_HOSTS = ["getsecretary.tech"];
const CODE = /^[A-Za-z0-9_-]{6,64}$/;

/**
 * The sign-in code in a scanned QR (`https://getsecretary.tech/approve?c=…`) or a typed
 * 6-digit code ("482 913" works too); null for anything else.
 */
export function approveCodeFrom(text: string, ownHost = typeof location === "undefined" ? "" : location.host): string | null {
  const raw = text.trim();
  const digits = raw.replace(/[\s-]/g, "");
  if (/^\d{6}$/.test(digits)) return digits;
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  // Production links are https; this app's own host may be http (a local dev server).
  const own = url.host === ownHost;
  if (!own && !(APPROVE_HOSTS.includes(url.host) && url.protocol === "https:")) return null;
  if (url.pathname !== "/approve") return null;
  const c = url.searchParams.get("c");
  return c && CODE.test(c) ? c : null;
}

/** Finds QR codes in video frames. */
export interface QrReader {
  read(video: HTMLVideoElement): Promise<string[]>;
}

type Detector = { detect(source: CanvasImageSource): Promise<{ rawValue: string }[]> };
type DetectorClass = {
  new (opts: { formats: string[] }): Detector;
  getSupportedFormats?: () => Promise<string[]>;
};

/** The browser's BarcodeDetector when it reads QR codes, else jsQR (loaded on demand). */
export async function createQrReader(): Promise<QrReader> {
  const Native = (globalThis as { BarcodeDetector?: DetectorClass }).BarcodeDetector;
  if (Native) {
    try {
      const formats = (await Native.getSupportedFormats?.()) ?? ["qr_code"];
      if (formats.includes("qr_code")) {
        const detector = new Native({ formats: ["qr_code"] });
        return {
          async read(video) {
            try {
              return (await detector.detect(video)).map((r) => r.rawValue);
            } catch {
              return []; // no frame yet
            }
          },
        };
      }
    } catch {
      // fall through to jsQR
    }
  }
  const { default: jsQR } = await import("jsqr");
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  return {
    async read(video) {
      if (!ctx || video.readyState < 2 || !video.videoWidth) return [];
      // Scale down: a 640 px frame is plenty for a QR code on a screen, and much faster.
      const scale = Math.min(1, 640 / Math.max(video.videoWidth, video.videoHeight));
      canvas.width = Math.round(video.videoWidth * scale);
      canvas.height = Math.round(video.videoHeight * scale);
      ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
      const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
      const found = jsQR(img.data, img.width, img.height, { inversionAttempts: "attemptBoth" });
      return found ? [found.data] : [];
    },
  };
}
