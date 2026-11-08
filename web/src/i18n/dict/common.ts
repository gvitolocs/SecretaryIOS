import type { FeatureDictionary } from "../types";

const en = {
  close: "Close",
  dismiss: "Dismiss",
  tryAgain: "Try again",
  cancel: "Cancel",
  save: "Save",
  edit: "Edit",
  delete: "Delete",
  confirmDelete: "Confirm delete",
  saving: "Saving…",
  loading: "Loading",
  updating: "Updating…",
};

export const common: FeatureDictionary<typeof en> = {
  en,
  it: {
    close: "Chiudi",
    dismiss: "Chiudi",
    tryAgain: "Riprova",
    cancel: "Annulla",
    save: "Salva",
    edit: "Modifica",
    delete: "Elimina",
    confirmDelete: "Conferma eliminazione",
    saving: "Salvataggio…",
    loading: "Caricamento",
    updating: "Aggiornamento…",
  },
  da: {
    close: "Luk",
    dismiss: "Luk",
    tryAgain: "Prøv igen",
    cancel: "Annuller",
    save: "Gem",
    edit: "Rediger",
    delete: "Slet",
    confirmDelete: "Bekræft sletning",
    saving: "Gemmer…",
    loading: "Indlæser",
    updating: "Opdaterer…",
  },
};
