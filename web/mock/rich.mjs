// Lucina message format v1 replies for the mock server (docs/lucina-message-format.md).
//
// Chat triggers:
//   "inventario" -> the owner's 2026-10-22 inventory, as DeepSeek formats it now
//                   (mock/rich-inventario.mjs; `before` is the original text wall)
//   "calendario" / "dentista" -> a calendar event waiting for Sì / No / Modifica
//   "widget" / "rich"         -> one of every widget type
// Returns null for anything else (the plain mock replies stay as they were).

import INVENTARIO from "./rich-inventario.mjs";

export const INVENTARIO_BEFORE = INVENTARIO.before;
export const INVENTARIO_QUESTION = INVENTARIO.question;

const CONFIRM = {
  reply: "Confermi? Aggiungo «Dentista» domani alle 9:30 su Google Calendar.\n\n↪︎ rispondi: «sì» · «no» · «Modifica …»",
  format: "v1",
  lang: "it",
  bubbles: ["Confermi? Aggiungo **«Dentista»** domani alle 9:30 su Google Calendar 📅"],
  blocks: [],
  actions: [
    { id: "confirm-yes", label: "Sì", emoji: "✅", kind: "confirm", payload: { answer: "yes", text: "sì" } },
    { id: "confirm-no", label: "No", emoji: "✖️", kind: "confirm", payload: { answer: "no", text: "no" } },
    { id: "confirm-edit", label: "Modifica", emoji: "✏️", kind: "confirm", payload: { answer: "edit", text: "Modifica:" } },
  ],
};

const SHOWCASE = {
  reply: "Ecco la tua settimana 💪 (plain fallback)",
  format: "v1",
  lang: "it",
  bubbles: ["Settimana buona 💪 Tre allenamenti e il peso scende.", "Domani tocca alle gambe: ti ho messo i passi qui sotto."],
  blocks: [
    { type: "stat", items: [{ label: "Allenamenti", value: "3", delta: "+1" }, { label: "Peso", value: "81,6 kg", delta: "-0,4" }, { label: "Passi", value: "9.840" }] },
    { type: "steps", title: "Gambe, domani", items: ["Riscaldamento 10 min", "Squat 4×6 a **90 kg**", "Affondi 3×10", "Stretching"] },
    { type: "table", title: "Ultimi pesi", columns: ["Giorno", "Peso", "Δ"], rows: [["Lun", "82,0", "—"], ["Mer", "81,8", "-0,2"], ["Ven", "81,6", "-0,2"]] },
    { type: "quote", text: "Un giorno storto non decide il futuro.", cite: "Lucina" },
    { type: "link_preview", url: "https://example.com/programma-gambe", title: "Programma gambe (PDF)" },
  ],
  actions: [
    { id: "gym", label: "Apri Palestra", emoji: "🏋️", kind: "route", payload: { path: "/gym" } },
    { id: "move", label: "Sposta a giovedì", emoji: "📅", kind: "reply", payload: { text: "Sposta l'allenamento gambe a giovedì" } },
    { id: "ok", label: "Ok, grazie", kind: "dismiss", payload: {} },
  ],
};

export function richReply(message) {
  const m = String(message || "").toLowerCase();
  if (m.includes("inventario")) return INVENTARIO.after;
  if (/calendari|dentista/.test(m)) return CONFIRM;
  if (/\b(widget|rich)\b/.test(m)) return SHOWCASE;
  if (/^(sì|si)(?=$|[\s.!])/.test(m)) return { reply: "Fatto ✅ «Dentista» è in calendario, domani alle 9:30.", format: "v1", lang: "it", bubbles: ["Fatto ✅ «Dentista» è in calendario, domani alle 9:30."], blocks: [], actions: [] };
  return null;
}

/** Timeline meta for a rich reply (what Lucina stores). */
export function richMeta(r) {
  return { format: "v1", lang: r.lang, bubbles: r.bubbles, blocks: r.blocks, actions: r.actions };
}
