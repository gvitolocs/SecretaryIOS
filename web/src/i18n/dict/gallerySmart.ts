import type { FeatureDictionary } from "../types";

// Smart search: results ranked by meaning, "Similar photos". Components: components/gallery/smart/*.

const en = {
  similar: "Similar photos",
  resultsFor: "Results for “{query}”",
  similarTo: "Photos like {name}",
  similarToThis: "Photos like this one",
  showAll: "Show everything",
  modeLabel: "Search by",
  modeWords: "Words",
  modeSmart: "Smart",
  hint: { one: "Smart search has looked at {count} photo so far.", other: "Smart search has looked at {count} photos so far." },
  emptySearch: "No photos match that.",
  emptySimilar: "No similar photos found.",
};

export const gallerySmart: FeatureDictionary<typeof en> = {
  en,
  it: {
    similar: "Foto simili",
    resultsFor: "Risultati per “{query}”",
    similarTo: "Foto simili a {name}",
    similarToThis: "Foto simili a questa",
    showAll: "Mostra tutto",
    modeLabel: "Cerca per",
    modeWords: "Parole",
    modeSmart: "Intelligente",
    hint: { one: "La ricerca intelligente ha guardato finora {count} foto.", other: "La ricerca intelligente ha guardato finora {count} foto." },
    emptySearch: "Nessuna foto corrisponde.",
    emptySimilar: "Nessuna foto simile trovata.",
  },
  da: {
    similar: "Lignende billeder",
    resultsFor: "Resultater for “{query}”",
    similarTo: "Billeder ligesom {name}",
    similarToThis: "Billeder ligesom dette",
    showAll: "Vis alt",
    modeLabel: "Søg med",
    modeWords: "Ord",
    modeSmart: "Smart",
    hint: { one: "Smart søgning har indtil videre set på {count} billede.", other: "Smart søgning har indtil videre set på {count} billeder." },
    emptySearch: "Ingen billeder passer.",
    emptySimilar: "Ingen lignende billeder fundet.",
  },
};
