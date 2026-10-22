// Lucina's outfit (GET/PUT /v1/avatar) and the wardrobe (GET /v1/avatar/wardrobe).

import { parseSettings, parseWardrobe, type AvatarPatch, type AvatarSettings, type Outfit } from "../lib/wardrobe";
import { request } from "./client";

const CLIENT = { "X-Secretary-Client": "web" };

export const avatarApi = {
  get: async (): Promise<AvatarSettings> => parseSettings(await request<unknown>("GET", "/v1/avatar")),
  put: async (patch: AvatarPatch): Promise<AvatarSettings> =>
    parseSettings(await request<unknown>("PUT", "/v1/avatar", { body: patch, headers: CLIENT })),
  wardrobe: async (): Promise<Outfit[]> => parseWardrobe(await request<unknown>("GET", "/v1/avatar/wardrobe")),
};
