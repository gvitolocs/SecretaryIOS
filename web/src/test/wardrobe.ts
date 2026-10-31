// Wardrobe fixtures for tests (same shapes as GET /v1/avatar/wardrobe and GET /v1/avatar).

export const OUTFITS = [
  { id: "uniform", name: { it: "Divisa", en: "Uniform", da: "Uniform" }, glb: "lucina.glb", thumbnail: "outfit-uniform.webp", tags: ["uniform"] },
  { id: "gym", name: { it: "Palestra", en: "Gym" }, glb: "lucina-gym.glb", tags: ["sport"] },
  { id: "chef", name: { en: "Chef" }, glb: "lucina-chef.glb", tags: ["work"] },
  { id: "maid", name: "Maid", glb: "lucina-maid.glb", tags: ["maid"] },
];

export const HAIRSTYLES = [
  { id: "natural", name: { en: "Natural", it: "Naturali" } },
  { id: "ponytail", name: { en: "Ponytail", it: "Coda" }, glb: "lucina-hair-ponytail.glb", thumbnail: "hair-ponytail.webp", hideTiara: true },
  { id: "bun", name: { en: "Bun", it: "Chignon" }, glb: "lucina-hair-bun.glb" },
];

export const COLORS = [
  { id: "pink", name: { en: "Pink", it: "Rosa" }, hex: "#ff8ac8" },
  { id: "silver", name: { en: "Silver" }, hex: "#c8cedc" },
];

export const WARDROBE = { outfits: OUTFITS, hairstyles: HAIRSTYLES, colors: COLORS };

export function settings(extra: Record<string, unknown> = {}) {
  return {
    outfit: null,
    hair: null,
    hairColor: null,
    auto: true,
    rules: {
      gym: { outfit: "gym", hair: "ponytail" },
      shopping: { outfit: "chef", hair: "natural" },
      default: { outfit: "uniform", hair: "natural" },
    },
    updatedMs: 0,
    updatedBy: "",
    ...extra,
  };
}

/** A PUT /v1/avatar merge like the server's. */
export function merge(current: ReturnType<typeof settings>, patch: Record<string, unknown>) {
  const rules = { ...current.rules } as Record<string, { outfit: string; hair: string }>;
  for (const [k, v] of Object.entries((patch.rules as Record<string, object>) ?? {})) rules[k] = { ...rules[k]!, ...v };
  return { ...current, ...patch, rules } as ReturnType<typeof settings>;
}
