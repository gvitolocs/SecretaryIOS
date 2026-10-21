// Web Storage can be missing or throw (private mode, blocked site data,
// quota). Every access goes through these helpers, which never throw.

type Area = "local" | "session";

function storage(area: Area): Storage | null {
  try {
    return area === "local" ? window.localStorage : window.sessionStorage;
  } catch {
    return null;
  }
}

export function readString(area: Area, key: string): string | null {
  try {
    return storage(area)?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

export function writeString(area: Area, key: string, value: string): void {
  try {
    storage(area)?.setItem(key, value);
  } catch {
    // Ignore: persistence is a convenience, never a requirement.
  }
}

export function removeKey(area: Area, key: string): void {
  try {
    storage(area)?.removeItem(key);
  } catch {
    // Ignore.
  }
}

export function readJSON(area: Area, key: string): unknown {
  const raw = readString(area, key);
  if (raw === null) return null;
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    return null;
  }
}

export function writeJSON(area: Area, key: string, value: unknown): void {
  try {
    writeString(area, key, JSON.stringify(value));
  } catch {
    // Ignore (e.g. circular data).
  }
}
