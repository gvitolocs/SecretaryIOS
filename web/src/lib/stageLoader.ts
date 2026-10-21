// The Lucina stage (three.js) is large, so it is only fetched when the Chat
// view shows it. Keep every runtime import of the package behind this
// function; type-only imports are fine anywhere.

export type StageModule = typeof import("@secretary/lucina-stage");

export function loadStage(): Promise<StageModule> {
  return import("@secretary/lucina-stage");
}
