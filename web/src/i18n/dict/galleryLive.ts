import type { FeatureDictionary } from "../types";

// Live Photos: the badge on a tile, the LIVE button in the viewer. Components: components/gallery/live/*.

const en = {
  photo: "Live Photo",
  label: "LIVE",
  play: "Play Live Photo",
  playHint: "Play the Live Photo (or hold the photo, or hold L)",
};

export const galleryLive: FeatureDictionary<typeof en> = {
  en,
  it: {
    photo: "Live Photo",
    label: "LIVE",
    play: "Riproduci la Live Photo",
    playHint: "Riproduci la Live Photo (o tieni premuta la foto, o il tasto L)",
  },
  da: {
    photo: "Live Photo",
    label: "LIVE",
    play: "Afspil Live Photo",
    playHint: "Afspil Live Photo (eller hold fast på billedet, eller hold L nede)",
  },
};
