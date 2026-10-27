// Secretary Code (the Mac app) embeds this web app as its main window and exposes `window.secretaryShell`, a small
// bridge to the desktop: showing its coding view ("Code" plugin) on top of the web app and hiding it again.
// In a browser or on the phone it is absent, and everything that uses it stays hidden.

export interface SecretaryShell {
  /** The desktop app hosts this page. */
  isDesktop?: boolean;
  /** Show the coding view, optionally on a project folder. */
  showCode(projectPath?: string): void;
  hideCode?(): void;
}

declare global {
  interface Window {
    secretaryShell?: SecretaryShell;
  }
}

/** The desktop bridge, when this page runs inside Secretary Code. */
export function desktopShell(): SecretaryShell | undefined {
  if (typeof window === "undefined") return undefined;
  const shell = window.secretaryShell;
  return shell && typeof shell.showCode === "function" ? shell : undefined;
}
