import { readString, writeString } from "./storage";

export function uuid(): string {
  try {
    if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
      return crypto.randomUUID();
    }
  } catch {
    // Fall through to the manual version.
  }
  const b = new Uint8Array(16);
  if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {
    crypto.getRandomValues(b);
  } else {
    for (let i = 0; i < b.length; i++) b[i] = Math.floor(Math.random() * 256);
  }
  b[6] = ((b[6] ?? 0) & 0x0f) | 0x40;
  b[8] = ((b[8] ?? 0) & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

const SESSION_ID_KEY = "secretary.sessionId";
let memorySessionId: string | null = null;

/** Stable chat session id for this browser (localStorage, memory fallback). */
export function chatSessionId(): string {
  if (memorySessionId) return memorySessionId;
  const stored = readString("local", SESSION_ID_KEY);
  if (stored) {
    memorySessionId = stored;
    return stored;
  }
  const id = uuid();
  memorySessionId = id;
  writeString("local", SESSION_ID_KEY, id);
  return id;
}
