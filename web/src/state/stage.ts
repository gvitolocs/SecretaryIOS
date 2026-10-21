import { createSignal } from "solid-js";
import { readString, writeString } from "../lib/storage";

// Whether the Lucina stage is shown. Hiding it disposes the renderer (frees
// the GPU); the choice is remembered per browser.

const HIDDEN_KEY = "secretary.stage.hidden";

const [stageHidden, setHidden] = createSignal(readString("local", HIDDEN_KEY) === "1");

export { stageHidden };

export function setStageHidden(hidden: boolean): void {
  setHidden(hidden);
  writeString("local", HIDDEN_KEY, hidden ? "1" : "0");
}

/** Test helper: re-read the stored preference. */
export function reloadStageFromStorage(): void {
  setHidden(readString("local", HIDDEN_KEY) === "1");
}

/** What Lucina's stage is doing now (idle, listening, thinking, speaking), for the shell's UI. */
export type LucinaActivity = "idle" | "listening" | "thinking" | "speaking";
const [activity, setActivity] = createSignal<LucinaActivity>("idle");
export { activity as lucinaActivity, setActivity as setLucinaActivity };
