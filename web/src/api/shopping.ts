// Shopping v2 endpoints (docs/shopping-v2.md). The legacy `/v1/shopping` calls
// stay in api/secretary.ts for older clients; this view no longer uses them.

import { parseCatalog, type Catalog, type Lang } from "../lib/shoppingCatalog";
import {
  parseCustomObjects,
  parseList,
  parseLists,
  parseListState,
  parseOpsResponse,
  type ListState,
  type CustomObject,
  type ListTheme,
  type Op,
  type OpsResponse,
  type ShopList,
} from "../lib/shoppingOps";
import { parseDealsMap, type DealsMapData } from "../lib/dealsMap";
import { ApiError, request } from "./client";

const CLIENT = { "X-Secretary-Client": "web" };
const enc = encodeURIComponent;

function mustList(raw: unknown): ShopList {
  const list = parseList(raw);
  if (!list) throw new ApiError(500, "bad_response", "The server sent an unexpected list.");
  return list;
}

export const shoppingApi = {
  catalog: async (lang: Lang): Promise<Catalog> =>
    parseCatalog(await request<unknown>("GET", `/v1/shopping/catalog?lang=${enc(lang)}`)),
  lists: async (): Promise<ShopList[]> => parseLists(await request<unknown>("GET", "/v1/shopping/lists")),
  createList: async (name: string, theme: ListTheme): Promise<ShopList> =>
    mustList(await request<unknown>("POST", "/v1/shopping/lists", { body: { name, theme }, headers: CLIENT })),
  patchList: async (id: string, patch: { name?: string; theme?: ListTheme }): Promise<ShopList> =>
    mustList(await request<unknown>("PATCH", `/v1/shopping/lists/${enc(id)}`, { body: patch, headers: CLIENT })),
  deleteList: (id: string): Promise<unknown> =>
    request<unknown>("DELETE", `/v1/shopping/lists/${enc(id)}`, { headers: CLIENT }),
  /** `lang`: item names come back in the UI language. */
  listState: async (id: string, lang?: Lang): Promise<ListState> =>
    parseListState(await request<unknown>("GET", `/v1/shopping/lists/${enc(id)}${lang ? `?lang=${enc(lang)}` : ""}`), id),
  postOps: async (id: string, expectedRevision: number, ops: Op[], lang?: Lang): Promise<OpsResponse> =>
    parseOpsResponse(
      await request<unknown>("POST", `/v1/shopping/lists/${enc(id)}/ops`, {
        body: { expectedRevision, ...(lang ? { lang } : {}), ops },
        headers: CLIENT,
      }),
    ),
  /** The owner's custom item objects. */
  customItems: async (): Promise<CustomObject[]> => parseCustomObjects(await request<unknown>("GET", "/v1/shopping/items")),
  /** Deals map around home (Lucina's deal engine), radius 500..=15000 m. */
  dealsMap: async (radiusM?: number): Promise<DealsMapData> =>
    parseDealsMap(
      await request<unknown>(
        "GET",
        `/v1/grocery/deals/map${radiusM !== undefined ? `?radiusM=${enc(String(radiusM))}` : ""}`,
      ),
    ),
};
