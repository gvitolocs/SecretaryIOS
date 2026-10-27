import type { FeatureDictionary } from "../types";

// Films: the Film and Dansk film folders on the server (owner, 2026-10-26).

const en = {
  title: "Films",
  continue: "Continue watching",
  search: "Search films",
  empty: "No films yet",
  emptyDetail: "Films in the Film and Dansk film folders on the server show up here.",
  fromStart: "Play from the beginning",
  left: "{time} left",
};

export const films: FeatureDictionary<typeof en> = {
  en,
  it: {
    title: "Film",
    continue: "Continua a guardare",
    search: "Cerca film",
    empty: "Ancora nessun film",
    emptyDetail: "Qui compaiono i film delle cartelle Film e Dansk film sul server.",
    fromStart: "Riproduci dall'inizio",
    left: "Mancano {time}",
  },
  da: {
    title: "Film",
    continue: "Fortsæt med at se",
    search: "Søg i film",
    empty: "Ingen film endnu",
    emptyDetail: "Film i mapperne Film og Dansk film på serveren vises her.",
    fromStart: "Afspil fra begyndelsen",
    left: "{time} tilbage",
  },
};
