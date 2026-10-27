import type { FeatureDictionary } from "../types";

// The Gallery grid (views/Gallery.tsx): sources, filters, sections, selection and the trash.

const en = {
  title: "Gallery",
  loading: "Loading…",
  empty: "No photos here yet.",
  loadError: "Couldn't load the gallery.",
  retry: "Try again",
};

export const gallery: FeatureDictionary<typeof en> = {
  en,
  it: { title: "Galleria", loading: "Caricamento…", empty: "Ancora nessuna foto qui.", loadError: "Impossibile caricare la galleria.", retry: "Riprova" },
  da: { title: "Galleri", loading: "Indlæser…", empty: "Ingen billeder her endnu.", loadError: "Kunne ikke indlæse galleriet.", retry: "Prøv igen" },
};
