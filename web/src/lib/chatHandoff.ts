// Something for Lucina's chat to pick up when it opens: pictures to attach and a first line, e.g. the Gallery's
// "Ask Lucina about this photo". Held in memory until the chat takes it; the owner then writes and sends himself.

interface HandOff {
  files: File[];
  text: string;
}

let pending: HandOff | null = null;

export function handOff(files: File[], text = ""): void {
  pending = { files, text };
  if (typeof window !== "undefined") window.dispatchEvent(new CustomEvent("secretary:chat-handoff"));
}

/** What was handed over (once), or null. */
export function takeHandOff(): HandOff | null {
  const taken = pending;
  pending = null;
  return taken;
}
