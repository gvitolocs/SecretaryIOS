/** Mute preference persisted in localStorage. Storage can be missing or throw (private mode, WKWebView). */

export const MUTE_STORAGE_KEY = 'lucina-stage:muted';

type MinimalStorage = Pick<Storage, 'getItem' | 'setItem'>;

function defaultStorage(): MinimalStorage | null {
  try {
    return typeof globalThis.localStorage === 'undefined' ? null : globalThis.localStorage;
  } catch {
    return null;
  }
}

/** The stored preference, or null when nothing is stored or storage is unavailable. */
export function readStoredMuted(key = MUTE_STORAGE_KEY, storage: MinimalStorage | null = defaultStorage()): boolean | null {
  if (!storage) return null;
  try {
    const v = storage.getItem(key);
    return v === '1' ? true : v === '0' ? false : null;
  } catch {
    return null;
  }
}

export function readMuted(key = MUTE_STORAGE_KEY, storage: MinimalStorage | null = defaultStorage()): boolean {
  return readStoredMuted(key, storage) ?? false;
}

/** Returns false when the value could not be stored. */
export function writeMuted(muted: boolean, key = MUTE_STORAGE_KEY, storage: MinimalStorage | null = defaultStorage()): boolean {
  if (!storage) return false;
  try {
    storage.setItem(key, muted ? '1' : '0');
    return true;
  } catch {
    return false;
  }
}
