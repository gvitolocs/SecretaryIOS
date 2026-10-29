// Secretary Code's focus mode: inside the Mac app a page that wants "full screen" asks the shell to
// give it nearly the whole window (the rail and Lucina step aside) instead of calling the Fullscreen
// API, which would put the entire Mac window into macOS full screen.
//
// Contract (SecretaryCode lucina-shell 9f762f1): `window.secretaryShell.setFocusMode(on)`, and
// `onFocusModeChange(listener)` → unsubscribe, which also reports the shell leaving focus mode by
// itself (Esc, its "Exit focus" button, another view). Outside the shell both are no-ops.

interface FocusShell {
  setFocusMode(on: boolean): void;
  onFocusModeChange?(listener: (on: boolean) => void): () => void;
}

function shell(): FocusShell | undefined {
  if (typeof window === "undefined") return undefined;
  const s = (window as { secretaryShell?: Partial<FocusShell> }).secretaryShell;
  return s && typeof s.setFocusMode === "function" ? (s as FocusShell) : undefined;
}

/** The page runs inside Secretary Code, whose focus mode replaces full screen. */
export function hasFocusMode(): boolean {
  return shell() !== undefined;
}

/** Ask the shell for focus mode (or to leave it). Returns false outside the shell. */
export function setFocusMode(on: boolean): boolean {
  const s = shell();
  if (!s) return false;
  try {
    s.setFocusMode(on);
    return true;
  } catch {
    return false;
  }
}

/** Follow the shell's focus mode, including when it leaves by itself. Returns the unsubscribe. */
export function onFocusModeChange(listener: (on: boolean) => void): () => void {
  const s = shell();
  if (!s || typeof s.onFocusModeChange !== "function") return () => undefined;
  try {
    return s.onFocusModeChange((on) => listener(on === true));
  } catch {
    return () => undefined;
  }
}
