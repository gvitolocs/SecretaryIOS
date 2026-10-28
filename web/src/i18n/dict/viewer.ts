import type { FeatureDictionary } from "../types";

// The photo viewer (components/PhotoViewer.tsx) and the Mac viewer window (/viewer).

const en = {
  position: "Photo {index} of {count}",
  previous: "Previous",
  next: "Next",
  share: "Share",
  more: "More",
  save: "Save to Lucina",
  saving: "Saving… {percent}%",
  saved: "Saved to Lucina",
  already: "Already in Lucina",
  saveFailed: "Not saved · Retry",
  empty: "Open a picture with Secretary Code to see it here.",
};

export const viewer: FeatureDictionary<typeof en> = {
  en,
  it: {
    position: "Foto {index} di {count}",
    previous: "Precedente",
    next: "Successiva",
    share: "Condividi",
    more: "Altro",
    save: "Salva su Lucina",
    saving: "Salvataggio… {percent}%",
    saved: "Salvata su Lucina",
    already: "Già su Lucina",
    saveFailed: "Non salvata · Riprova",
    empty: "Apri un'immagine con Secretary Code per vederla qui.",
  },
  da: {
    position: "Foto {index} af {count}",
    previous: "Forrige",
    next: "Næste",
    share: "Del",
    more: "Mere",
    save: "Gem i Lucina",
    saving: "Gemmer… {percent}%",
    saved: "Gemt i Lucina",
    already: "Allerede i Lucina",
    saveFailed: "Ikke gemt · Prøv igen",
    empty: "Åbn et billede med Secretary Code for at se det her.",
  },
};
