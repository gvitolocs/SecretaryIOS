// Lucina's look (GET/PUT /v1/avatar) and the wardrobe (GET /v1/avatar/wardrobe).

import { parseSettings, parseWardrobeFile, type AvatarPatch, type AvatarSettings, type WardrobeFile } from "../lib/wardrobe";
import { request } from "./client";

const CLIENT = { "X-Secretary-Client": "web" };

export const avatarApi = {
  get: async (): Promise<AvatarSettings> => parseSettings(await request<unknown>("GET", "/v1/avatar")),
  put: async (patch: AvatarPatch): Promise<AvatarSettings> =>
    parseSettings(await request<unknown>("PUT", "/v1/avatar", { body: patch, headers: CLIENT })),
  wardrobe: async (): Promise<WardrobeFile> => parseWardrobeFile(await request<unknown>("GET", "/v1/avatar/wardrobe")),
};
