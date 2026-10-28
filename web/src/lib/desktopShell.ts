// Secretary Code (the Mac app) embeds this web app as its main window and exposes `window.secretaryShell`, a small
// bridge to the desktop (contract: lucina-shell-build.md in the sessions' notes). The desktop has its own rail
// (Lucina, Code, Computer, Gallery, Films, All chats, Secrets, Server, Settings), so the web app drops its top bar
// and tab bar there; the shell moves this app between pages with history.pushState + popstate.
// In a browser or on the phone it is absent, and everything that uses it stays hidden.

/** A view of the desktop rail. */
export type ShellView =
  | "lucina"
  | "code"
  | "computer"
  | "gallery"
  | "films"
  | "chats"
  | "secrets"
  | "health"
  | "shopping"
  | "training";

export interface SecretaryShell {
  version?: number;
  /** The desktop app hosts this page. */
  isDesktop?: boolean;
  /** Show the coding view, optionally on a project folder. */
  showCode(projectPath?: string): void;
  hideCode?(): void;
  /** Switch the desktop rail to a view. */
  show?(view: ShellView): void;
  /** Make the Mac forget its device token (the web app signs out too). */
  signOut?(): void | Promise<void>;
  /** "Show adult plugins" changed on this device: the rail shows or hides NClient at once. */
  setAdultPlugins?(on: boolean): void;
}

declare global {
  interface Window {
    secretaryShell?: SecretaryShell;
  }
}

const EMBEDDED_KEY = "secretary.shell";

/**
 * Inside Secretary Code: its bridge is there, or the shell opened this page with `?shell=1` (remembered for the
 * browser session, since the app's own navigation drops the query).
 */
export function embedded(): boolean {
  if (desktopShell()) return true;
  if (typeof window === "undefined") return false;
  try {
    if (new URLSearchParams(window.location.search).get("shell") === "1") {
      sessionStorage.setItem(EMBEDDED_KEY, "1");
      return true;
    }
    return sessionStorage.getItem(EMBEDDED_KEY) === "1";
  } catch {
    return false;
  }
}

/** The web app's own top bar and tab bar: shown everywhere except inside Secretary Code, whose rail replaces them. */
export function showWebChrome(): boolean {
  return !embedded();
}

/** The desktop bridge, when this page runs inside Secretary Code. */
export function desktopShell(): SecretaryShell | undefined {
  if (typeof window === "undefined") return undefined;
  const shell = window.secretaryShell;
  return shell && typeof shell.showCode === "function" ? shell : undefined;
}
