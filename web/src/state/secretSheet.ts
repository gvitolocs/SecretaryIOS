// The "Add secret" sheet's open state. The value lives in this signal only
// while the sheet is open: never in storage, the chat cache or a URL.

import { createSignal } from "solid-js";
import { readJSON, writeJSON } from "../lib/storage";

export interface SecretSheetInput {
  mode?: "save" | "project";
  name?: string;
  value?: string;
  /** A value Lucina holds for the owner (from a chat card). */
  held?: string;
  /** `/secret` target (alias or project/env/path). */
  target?: string;
  project?: string;
  environment?: string;
  path?: string;
  projectName?: string;
}

const [sheet, setSheet] = createSignal<SecretSheetInput | null>(null);

export { sheet as secretSheet };

export function openSecretSheet(input: SecretSheetInput = {}): void {
  setSheet({ mode: "save", ...input });
}

export function closeSecretSheet(): void {
  setSheet(null);
}

export interface LastPlace {
  project: string;
  environment: string;
  path: string;
}

const LAST_KEY = "secretary.secret.last.v1";

/** The last project / environment / folder used (names only). */
export function lastPlace(): LastPlace | null {
  const v = readJSON("local", LAST_KEY) as Partial<LastPlace> | null;
  if (!v || typeof v.project !== "string" || typeof v.environment !== "string" || typeof v.path !== "string") return null;
  return { project: v.project, environment: v.environment, path: v.path };
}

export function rememberPlace(p: LastPlace): void {
  writeJSON("local", LAST_KEY, p);
}
