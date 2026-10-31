// ThumbHash decoder (https://evanw.github.io/thumbhash/), vendored so the
// chat bubble can paint an instant placeholder without a new dependency.
//
// The hash is a tiny DCT: one average colour, a few chroma terms and a
// luminance gradient. Decoding it produces a 32 px image that is stretched to
// the attachment frame until the real image has loaded.

export interface ThumbHashImage {
  w: number;
  h: number;
  rgba: Uint8Array;
}

/** Standard base64 (also tolerates a data URL or base64url) to bytes. */
export function base64ToBytes(value: string): Uint8Array {
  const clean = value.replace(/^data:[^,]*,/, "").replace(/[\s=]+/g, "").replace(/-/g, "+").replace(/_/g, "/");
  if (!clean) return new Uint8Array();
  try {
    if (typeof atob === "function") {
      const binary = atob(clean);
      const out = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
      return out;
    }
  } catch {
    return new Uint8Array();
  }
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const out: number[] = [];
  let buffer = 0;
  let bits = 0;
  for (const ch of clean) {
    const v = alphabet.indexOf(ch);
    if (v < 0) continue;
    buffer = (buffer << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((buffer >> bits) & 0xff);
    }
  }
  return new Uint8Array(out);
}

export function thumbHashToApproximateAspectRatio(hash: Uint8Array): number {
  const header = hash[3] ?? 0;
  const hasAlpha = ((hash[2] ?? 0) & 0x80) !== 0;
  const isLandscape = ((hash[4] ?? 0) & 0x80) !== 0;
  const lx = isLandscape ? (hasAlpha ? 5 : 7) : header & 7;
  const ly = isLandscape ? header & 7 : hasAlpha ? 5 : 7;
  return lx / ly;
}

export function thumbHashToRGBA(hash: Uint8Array): ThumbHashImage {
  const { PI, min, max, cos, round } = Math;
  const at = (i: number): number => hash[i] ?? 0;

  let header24 = at(0) | (at(1) << 8) | (at(2) << 16);
  let header16 = at(3) | (at(4) << 8);
  let l_dc = (header24 & 63) / 63;
  let p_dc = ((header24 >> 6) & 63) / 31.5 - 1;
  let q_dc = ((header24 >> 12) & 63) / 31.5 - 1;
  let l_scale = ((header24 >> 18) & 31) / 31;
  let hasAlpha = header24 >> 23;
  let p_scale = ((header16 >> 3) & 63) / 63;
  let q_scale = ((header16 >> 9) & 63) / 63;
  let isLandscape = header16 >> 15;
  let lx = max(3, isLandscape ? (hasAlpha ? 5 : 7) : header16 & 7);
  let ly = max(3, isLandscape ? header16 & 7 : hasAlpha ? 5 : 7);
  let a_dc = hasAlpha ? (at(5) & 15) / 15 : 1;
  let a_scale = (at(5) >> 4) / 15;

  const ac_start = hasAlpha ? 6 : 5;
  let ac_index = 0;
  const decodeChannel = (nx: number, ny: number, scale: number): number[] => {
    const ac: number[] = [];
    for (let cy = 0; cy < ny; cy++) {
      for (let cx = cy ? 0 : 1; cx * ny < nx * (ny - cy); cx++) {
        const byte = at(ac_start + (ac_index >> 1));
        ac.push((((byte >> ((ac_index++ & 1) << 2)) & 15) / 7.5 - 1) * scale);
      }
    }
    return ac;
  };
  const l_ac = decodeChannel(lx, ly, l_scale);
  const p_ac = decodeChannel(3, 3, p_scale * 1.25);
  const q_ac = decodeChannel(3, 3, q_scale * 1.25);
  const a_ac = hasAlpha ? decodeChannel(5, 5, a_scale) : [];

  const ratio = thumbHashToApproximateAspectRatio(hash);
  const w = round(ratio > 1 ? 32 : 32 * ratio);
  const h = round(ratio > 1 ? 32 / ratio : 32);
  const rgba = new Uint8Array(w * h * 4);
  const fx: number[] = [];
  const fy: number[] = [];

  for (let y = 0, i = 0; y < h; y++) {
    for (let x = 0; x < w; x++, i += 4) {
      let l = l_dc;
      let p = p_dc;
      let q = q_dc;
      let a = a_dc;

      for (let cx = 0, n = max(lx, hasAlpha ? 5 : 3); cx < n; cx++) fx[cx] = cos((PI / w) * (x + 0.5) * cx);
      for (let cy = 0, n = max(ly, hasAlpha ? 5 : 3); cy < n; cy++) fy[cy] = cos((PI / h) * (y + 0.5) * cy);

      for (let cy = 0, j = 0; cy < ly; cy++) {
        for (let cx = cy ? 0 : 1, fy2 = (fy[cy] ?? 0) * 2; cx * ly < lx * (ly - cy); cx++, j++) {
          l += (l_ac[j] ?? 0) * (fx[cx] ?? 0) * fy2;
        }
      }

      for (let cy = 0, j = 0; cy < 3; cy++) {
        for (let cx = cy ? 0 : 1, fy2 = (fy[cy] ?? 0) * 2; cx < 3 - cy; cx++, j++) {
          const f = (fx[cx] ?? 0) * fy2;
          p += (p_ac[j] ?? 0) * f;
          q += (q_ac[j] ?? 0) * f;
        }
      }

      if (hasAlpha) {
        for (let cy = 0, j = 0; cy < 5; cy++) {
          for (let cx = cy ? 0 : 1, fy2 = (fy[cy] ?? 0) * 2; cx < 5 - cy; cx++, j++) {
            a += (a_ac[j] ?? 0) * (fx[cx] ?? 0) * fy2;
          }
        }
      }

      const b = l - (2 / 3) * p;
      const r = (3 * l - b + q) / 2;
      const g = r - q;
      rgba[i] = max(0, 255 * min(1, r));
      rgba[i + 1] = max(0, 255 * min(1, g));
      rgba[i + 2] = max(0, 255 * min(1, b));
      rgba[i + 3] = max(0, 255 * min(1, a));
    }
  }
  return { w, h, rgba };
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(bytes: number[]): number {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = (CRC_TABLE[(crc ^ byte) & 0xff] ?? 0) ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function u32(value: number): number[] {
  return [(value >>> 24) & 255, (value >>> 16) & 255, (value >>> 8) & 255, value & 255];
}

function chunk(type: string, data: number[]): number[] {
  const body = [...type].map((c) => c.charCodeAt(0)).concat(data);
  return [...u32(data.length), ...body, ...u32(crc32(body))];
}

/** A small PNG data URL for a decoded hash (used as the bubble placeholder). */
export function rgbaToDataURL(w: number, h: number, rgba: Uint8Array): string {
  const raw: number[] = [];
  for (let y = 0, i = 0; y < h; y++) {
    raw.push(0);
    for (let x = 0; x < w; x++, i += 4) raw.push(rgba[i] ?? 0, rgba[i + 1] ?? 0, rgba[i + 2] ?? 0, rgba[i + 3] ?? 0);
  }
  const idat: number[] = [];
  for (let offset = 0; offset < raw.length; offset += 65535) {
    const block = raw.slice(offset, offset + 65535);
    const last = offset + 65535 >= raw.length;
    idat.push(last ? 1 : 0, block.length & 255, (block.length >> 8) & 255, ~block.length & 255, ((~block.length) >> 8) & 255);
    idat.push(...block);
  }
  let a = 1;
  let b = 0;
  for (const byte of raw) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  idat.push((b >> 8) & 255, b & 255, (a >> 8) & 255, a & 255);
  const header = [137, 80, 78, 71, 13, 10, 26, 10];
  const ihdr = [...u32(w), ...u32(h), 8, 6, 0, 0, 0];
  const bytes = [...header, ...chunk("IHDR", ihdr), ...chunk("IDAT", [0x78, 0x01, ...idat]), ...chunk("IEND", [])];
  let binary = "";
  for (let i = 0; i < bytes.length; i += 8192) {
    binary += String.fromCharCode(...bytes.slice(i, i + 8192));
  }
  return `data:image/png;base64,${btoa(binary)}`;
}

/** Decode a base64 ThumbHash to a placeholder PNG data URL and its size. */
export function thumbHashToImage(value: string | undefined): { url: string; width: number; height: number } | null {
  if (!value) return null;
  try {
    const bytes = base64ToBytes(value);
    if (bytes.length < 5) return null;
    const { w, h, rgba } = thumbHashToRGBA(bytes);
    if (!w || !h) return null;
    return { url: rgbaToDataURL(w, h, rgba), width: w, height: h };
  } catch {
    return null;
  }
}
