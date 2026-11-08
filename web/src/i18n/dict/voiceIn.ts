import type { FeatureDictionary } from "../types";

// Streaming voice input: the chat composer's voice-note button (components/VoiceNoteButton.tsx) and
// the stage's live reply (components/LucinaPanel.tsx). Protocol: lib/voiceSocket.ts.

const en = {
  record: "Voice note: hold to record, or click to start and stop",
  recording: "Recording",
  release: "Release to send",
  cancel: "Cancel the voice note",
  transcribing: "Transcribing…",
  offline: "Voice isn't connected yet. Try again in a moment.",
  busy: "The last voice message is still going. Try again in a moment.",
  oldServer: "Voice needs a server update.",
  micBlocked: "The microphone is blocked.",
  failed: "The voice note didn't go through.",
  tooShort: "That was too short to send.",
  capped: "That note reached the 10 minute limit and was sent.",
  noSpeech: "I didn't catch that.",
};

export const voiceIn: FeatureDictionary<typeof en> = {
  en,
  it: {
    record: "Nota vocale: tieni premuto per registrare, oppure clicca per iniziare e fermare",
    recording: "Registrazione",
    release: "Rilascia per inviare",
    cancel: "Annulla la nota vocale",
    transcribing: "Trascrizione…",
    offline: "La voce non è ancora connessa. Riprova tra un attimo.",
    busy: "Il messaggio vocale precedente è ancora in corso. Riprova tra un attimo.",
    oldServer: "La voce richiede un aggiornamento del server.",
    micBlocked: "Il microfono è bloccato.",
    failed: "La nota vocale non è partita.",
    tooShort: "Troppo breve per essere inviata.",
    capped: "La nota ha raggiunto il limite di 10 minuti ed è stata inviata.",
    noSpeech: "Non ho capito.",
  },
  da: {
    record: "Talebesked: hold nede for at optage, eller klik for at starte og stoppe",
    recording: "Optager",
    release: "Slip for at sende",
    cancel: "Annuller talebeskeden",
    transcribing: "Transskriberer…",
    offline: "Stemmen er ikke forbundet endnu. Prøv igen om et øjeblik.",
    busy: "Den forrige talebesked er stadig i gang. Prøv igen om et øjeblik.",
    oldServer: "Stemmen kræver en serveropdatering.",
    micBlocked: "Mikrofonen er blokeret.",
    failed: "Talebeskeden gik ikke igennem.",
    tooShort: "Det var for kort til at blive sendt.",
    capped: "Beskeden nåede grænsen på 10 minutter og blev sendt.",
    noSpeech: "Jeg fangede det ikke.",
  },
};
