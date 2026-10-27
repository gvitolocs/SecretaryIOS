// "Save to Lucina": a local file goes into the Lucina gallery under the device it came from
// (lucina-media, docs/media-library.md "Upload" and "Devices"). The Mac viewer (Secretary
// Code) calls it with its own device; the phone apps run the same protocol natively.
//
// 1. PUT /v1/media/devices/{id} {name, kind}: registers or renames the device.
// 2. POST /v1/media/uploads {filename, size, meta}: `duplicate` when the server already has it.
// 3. PUT /v1/media/uploads/{id}?offset=N with the bytes, in chunks; a failed chunk resumes from
//    GET /v1/media/uploads/{id}.
// 4. POST /v1/media/uploads/{id}/finish: the item (the server dedupes by content, so saving the
//    same picture twice makes no second item).

import { ApiError, request } from "./client";

export type DeviceKind = "iphone" | "ipad" | "android" | "mac" | "pc" | "backup" | "camera" | "other";

export interface LucinaDevice {
  /** Stable per install, `[A-Za-z0-9._-]{1,64}`, e.g. `mac-<uuid>`. */
  id: string;
  name: string;
  kind: DeviceKind;
}

export interface UploadOptions {
  /** The file name shown in the gallery (a Blob has none). */
  name: string;
  device: LucinaDevice;
  /** Stable per file on the device (a path hash, a Photos id); the content's SHA-256 if absent. */
  assetId?: string;
  /** When the picture was taken (ISO 8601), if the caller knows better than the file's EXIF. */
  takenAt?: string;
  /** `meta.source`; defaults to the device kind ("mac"). */
  source?: string;
  onProgress?: (sent: number, total: number) => void;
  signal?: AbortSignal;
  /** Bytes per PUT. */
  chunkBytes?: number;
}

export interface UploadResult {
  itemId: string;
  duplicate: boolean;
}

interface UploadOut {
  upload_id: string | null;
  offset: number;
  size: number;
  finished: boolean;
  duplicate: boolean;
  item_id: string | null;
}

const DEVICE_ID = /^[A-Za-z0-9._-]{1,64}$/;
const DEFAULT_CHUNK = 8 * 1024 * 1024;
const RETRIES = 3;

export function validDeviceId(id: string): boolean {
  return DEVICE_ID.test(id);
}

export function registerDevice(device: LucinaDevice): Promise<unknown> {
  if (!validDeviceId(device.id)) throw new ApiError(0, "bad_device", "Invalid device id.");
  return request("PUT", `/v1/media/devices/${encodeURIComponent(device.id)}`, {
    body: { name: device.name, kind: device.kind },
  });
}

async function sha256Hex(blob: Blob): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", await blob.arrayBuffer());
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

async function putChunk(id: string, offset: number, chunk: Blob, signal?: AbortSignal): Promise<UploadOut> {
  let res: Response;
  try {
    res = await fetch(`/v1/media/uploads/${encodeURIComponent(id)}?offset=${offset}`, {
      method: "PUT",
      headers: { "Content-Type": "application/octet-stream", "X-Secretary-CSRF": "1", Accept: "application/json" },
      body: chunk,
      credentials: "same-origin",
      cache: "no-store",
      signal,
    });
  } catch (e) {
    if (signal?.aborted) throw e;
    throw new ApiError(0, "network", "Can't reach the server. Check your connection.");
  }
  if (!res.ok) throw new ApiError(res.status, `http_${res.status}`, `Upload failed (${res.status}).`);
  return (await res.json()) as UploadOut;
}

/** Sends `file` to the Lucina gallery as a photo of `options.device`. */
export async function uploadToLucina(file: Blob, options: UploadOptions): Promise<UploadResult> {
  const { device, name, signal } = options;
  const total = file.size;
  if (!name || name.includes("/") || name.length > 255) throw new ApiError(0, "bad_name", "Invalid file name.");
  await registerDevice(device);
  const meta: Record<string, unknown> = {
    device: device.id,
    asset_id: options.assetId ?? (await sha256Hex(file)),
    source: options.source ?? device.kind,
    filename: name,
  };
  if (file.type) meta.mime = file.type;
  if (options.takenAt) meta.taken_at = options.takenAt;
  const created = await request<UploadOut>("POST", "/v1/media/uploads", { body: { filename: name, size: total, meta } });
  if (created.duplicate && created.item_id) {
    options.onProgress?.(total, total);
    return { itemId: created.item_id, duplicate: true };
  }
  const id = created.upload_id;
  if (!id) throw new ApiError(0, "bad_answer", "The server didn't start the upload.");

  const step = Math.max(1, options.chunkBytes ?? DEFAULT_CHUNK);
  let offset = created.offset;
  let failures = 0;
  options.onProgress?.(offset, total);
  while (offset < total) {
    signal?.throwIfAborted();
    try {
      const out = await putChunk(id, offset, file.slice(offset, Math.min(total, offset + step)), signal);
      offset = out.offset;
      failures = 0;
      options.onProgress?.(offset, total);
    } catch (e) {
      if (signal?.aborted || ++failures > RETRIES) throw e;
      // Resume from what the server actually has.
      offset = (await request<UploadOut>("GET", `/v1/media/uploads/${encodeURIComponent(id)}`)).offset;
    }
  }
  const done = await request<{ item_id: string; duplicate: boolean }>(
    "POST",
    `/v1/media/uploads/${encodeURIComponent(id)}/finish`,
    { body: {}, timeoutMs: 120_000 },
  );
  return { itemId: done.item_id, duplicate: done.duplicate };
}
