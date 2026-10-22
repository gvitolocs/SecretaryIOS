// Wardrobe fixtures for tests (same shape as GET /v1/avatar/wardrobe).

export const OUTFITS = [
  { id: "uniform", name: { it: "Divisa", en: "Uniform", da: "Uniform" }, glb: "lucina.glb", thumbnail: "outfit-uniform.webp", tags: ["uniform"] },
  { id: "gym", name: { it: "Palestra", en: "Gym" }, glb: "lucina-gym.glb", tags: ["sport"] },
  { id: "chef", name: { en: "Chef" }, glb: "lucina-chef.glb", tags: ["work"] },
  { id: "maid", name: "Maid", glb: "lucina-maid.glb", tags: ["maid"] },
];

export function settings(extra: Record<string, unknown> = {}) {
  return {
    outfit: null,
    auto: true,
    rules: { gym: "gym", shopping: "chef", default: "uniform" },
    updatedMs: 0,
    updatedBy: "",
    ...extra,
  };
}
