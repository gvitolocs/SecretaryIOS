import type { FeatureDictionary } from "../types";
const en = {
  title: "Share with Lucina", attach: "Attach files", note: "Add a note", ask: "Send to Lucina",
  library: "Save to library", shopping: "Add to shopping", saveNote: "Save as note", remind: "Remind me",
  sending: "Sending…", failed: "Couldn't send this share. Try again.", storageFailed: "Couldn't open the share inbox. Try again.",
  limits: "Share up to 2 GB and 32 form parts at a time.", empty: "Add a file, link or text to share.",
  video: "Video", audio: "Audio", pdf: "PDF", document: "Document", link: "Link", text: "Text", contact: "Contact", location: "Location",
};
export const share: FeatureDictionary<typeof en> = {
  en,
  it: {
    title: "Condividi con Lucina", attach: "Allega file", note: "Aggiungi una nota", ask: "Invia a Lucina",
    library: "Salva nella libreria", shopping: "Aggiungi alla spesa", saveNote: "Salva come nota", remind: "Ricordamelo",
    sending: "Invio…", failed: "Invio non riuscito. Riprova.", storageFailed: "Impossibile aprire le condivisioni. Riprova.",
    limits: "Condividi fino a 2 GB e 32 parti del modulo alla volta.", empty: "Aggiungi un file, un link o del testo da condividere.",
    video: "Video", audio: "Audio", pdf: "PDF", document: "Documento", link: "Link", text: "Testo", contact: "Contatto", location: "Posizione",
  },
  da: {
    title: "Del med Lucina", attach: "Vedhæft filer", note: "Tilføj en note", ask: "Send til Lucina",
    library: "Gem i bibliotek", shopping: "Føj til indkøb", saveNote: "Gem som note", remind: "Mind mig om det",
    sending: "Sender…", failed: "Kunne ikke sende delingen. Prøv igen.", storageFailed: "Kunne ikke åbne delinger. Prøv igen.",
    limits: "Del op til 2 GB og 32 formulardele ad gangen.", empty: "Tilføj en fil, et link eller tekst at dele.",
    video: "Video", audio: "Lyd", pdf: "PDF", document: "Dokument", link: "Link", text: "Tekst", contact: "Kontakt", location: "Placering",
  },
};
