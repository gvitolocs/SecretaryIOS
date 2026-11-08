import { afterEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import { deleteEdits, getEdits, parseItem, putEdits } from "./gallery";
import { emptyEdit, setAdjust } from "../lib/editList";

afterEach(() => vi.unstubAllGlobals());

const itemJson = (over: Record<string, unknown> = {}) => ({
  id: "itm_1",
  kind: "photo",
  title: "a.jpg",
  mime: "image/jpeg",
  size: 10,
  source: "phone-ios",
  edited: true,
  width: 60,
  height: 80,
  urls: {
    original: "/v1/media/items/itm_1/original",
    download: "/v1/media/items/itm_1/original?download=1",
    grid: "/v1/media/items/itm_1/r/grid?v=1e99",
    edited: "/v1/media/items/itm_1/r/edited?v=1e99",
    download_edited: "/v1/media/items/itm_1/edits/download",
  },
  ...over,
});

describe("the editor's calls", () => {
  it("reads the edited flag and the derivative's URLs, which stay apart from the original's", () => {
    const item = parseItem(itemJson())!;
    expect(item.edited).toBe(true);
    expect(item.urls.edited).toBe("/v1/media/items/itm_1/r/edited?v=1e99");
    expect(item.urls.downloadEdited).toBe("/v1/media/items/itm_1/edits/download");
    expect(item.urls.original).toBe("/v1/media/items/itm_1/original");
    const plain = parseItem(itemJson({ edited: undefined, urls: { original: "/v1/media/items/itm_1/original" } }))!;
    expect(plain.edited).toBe(false);
    expect(plain.urls.edited).toBeUndefined();
    // A foreign URL is dropped like every other.
    expect(parseItem(itemJson({ urls: { edited: "https://evil.example/x.jpg" } }))!.urls.edited).toBeUndefined();
  });

  it("gets the list and the working copy's URL", async () => {
    const calls = stubFetch(() => ({
      body: {
        edited: true,
        edits: { v: 1, adjust: { exposure: 0.5 }, filter: { name: "bw", strength: 0.5 } },
        updated_at: "2026-10-28T10:00:00.000Z",
        base_url: "/v1/media/items/itm_1/edits/base",
        item: itemJson(),
      },
    }));
    const r = await getEdits("itm_1");
    expect(calls[0]).toMatchObject({ method: "GET", url: "/v1/media/items/itm_1/edits" });
    expect(r.edited).toBe(true);
    expect(r.edits?.adjust.exposure).toBe(0.5);
    expect(r.edits?.filter).toEqual({ name: "bw", strength: 0.5 });
    expect(r.baseUrl).toBe("/v1/media/items/itm_1/edits/base");
    expect(r.item?.id).toBe("itm_1");
  });

  it("saves a list with PUT and reads the item back", async () => {
    const calls = stubFetch(() => ({ body: { edited: true, edits: { v: 1 }, base_url: "/v1/media/items/itm_1/edits/base", item: itemJson() } }));
    const list = setAdjust(emptyEdit(), "exposure", 1);
    const r = await putEdits("itm_1", list);
    expect(calls[0]).toMatchObject({ method: "PUT", url: "/v1/media/items/itm_1/edits", body: list });
    expect(calls[0]!.headers["x-secretary-csrf"]).toBe("1");
    expect(r.item?.edited).toBe(true);
  });

  it("reverts with DELETE", async () => {
    const calls = stubFetch(() => ({ body: { edited: false, edits: null, base_url: "/v1/media/items/itm_1/edits/base", item: itemJson({ edited: false, urls: { original: "/v1/media/items/itm_1/original" } }) } }));
    const r = await deleteEdits("itm_1");
    expect(calls[0]).toMatchObject({ method: "DELETE", url: "/v1/media/items/itm_1/edits" });
    expect(r.edited).toBe(false);
    expect(r.edits).toBeNull();
    expect(r.item?.edited).toBe(false);
  });

  it("escapes the id and passes server refusals on", async () => {
    stubFetch(() => ({ status: 422, body: { error: "this picture can't be decoded for editing" } }));
    await expect(putEdits("itm/../x", emptyEdit())).rejects.toMatchObject({ status: 422 });
  });
});
