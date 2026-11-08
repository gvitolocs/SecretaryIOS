import { afterEach, describe, expect, it, vi } from "vitest";
import { stubFetch } from "../test/fetch";
import {
  assignFace,
  deleteFaceData,
  faceThumbUrl,
  galleryQueryString,
  getDetails,
  getFaceSettings,
  getPerson,
  listItemFaces,
  listPeople,
  listPersonItems,
  listUnassignedFaces,
  mergePeople,
  parseFaceBox,
  parseFaceSettings,
  parseItemFace,
  parsePerson,
  parsePersonRefs,
  patchPerson,
  setFacesEnabled,
} from "./gallery";

afterEach(() => vi.unstubAllGlobals());

const rawPerson = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  name: null,
  hidden: false,
  birth_date: null,
  face_count: 4,
  item_count: 3,
  cover_face_id: `${id}_f`,
  thumb_url: `/v1/media/people/${id}/thumb?v=1`,
  ...extra,
});
const rawItem = (id: string) => ({ id, kind: "photo", title: `${id}.jpg`, mime: "image/jpeg", size: 1, created_at: "2026-10-01T10:00:00Z", urls: {} });

describe("parsing", () => {
  it("reads the settings, counting from zero when fields are missing", () => {
    expect(parseFaceSettings({ enabled: true, model: "m", scanned: 10, pending: 5, faces: 20, people: 3, running: true, paused_hot: true })).toEqual({
      enabled: true,
      model: "m",
      scanned: 10,
      pending: 5,
      faces: 20,
      people: 3,
      running: true,
      pausedHot: true,
      pausedBusy: false,
    });
    expect(parseFaceSettings(null)).toMatchObject({ enabled: false, scanned: 0, pending: 0, model: null });
    expect(parseFaceSettings({ enabled: "yes", scanned: -4 })).toMatchObject({ enabled: false, scanned: 0 });
  });

  it("reads a person and drops one without an id; a bad date or a foreign thumbnail is ignored", () => {
    expect(parsePerson(rawPerson("p1", { name: "Alma", birth_date: "1990-04-12" }))).toEqual({
      id: "p1",
      name: "Alma",
      hidden: false,
      birthDate: "1990-04-12",
      faceCount: 4,
      itemCount: 3,
      coverFaceId: "p1_f",
      thumbUrl: "/v1/media/people/p1/thumb?v=1",
    });
    expect(parsePerson({ name: "x" })).toBeNull();
    expect(parsePerson(rawPerson("p2", { birth_date: "12/04/1990", thumb_url: "https://elsewhere.test/x.jpg" }))).toMatchObject({ birthDate: null, thumbUrl: null });
  });

  it("keeps a face box inside the picture and drops one with no size", () => {
    expect(parseFaceBox({ x: 0.2, y: 0.3, w: 0.1, h: 0.2 })).toEqual({ x: 0.2, y: 0.3, w: 0.1, h: 0.2 });
    expect(parseFaceBox({ x: -0.1, y: 0.9, w: 0.5, h: 0.5 })).toEqual({ x: 0, y: 0.9, w: 0.5, h: expect.closeTo(0.1, 6) });
    expect(parseFaceBox({ x: 0.2, y: 0.2, w: 0, h: 0.2 })).toBeNull();
    expect(parseFaceBox({ x: "0.2", y: 0.2, w: 0.1, h: 0.2 })).toBeNull();
    expect(parseFaceBox(undefined)).toBeNull();
  });

  it("reads an item's face with its person, as manual or machine-made", () => {
    const face = parseItemFace({ id: "f1", box: { x: 0.1, y: 0.1, w: 0.2, h: 0.2 }, score: 0.97, source: "manual", person: { id: "p1", name: "Alma" } });
    expect(face).toEqual({ id: "f1", box: { x: 0.1, y: 0.1, w: 0.2, h: 0.2 }, score: 0.97, source: "manual", person: { id: "p1", name: "Alma" } });
    expect(parseItemFace({ id: "f2", box: { x: 0, y: 0, w: 0.1, h: 0.1 }, person: null })).toMatchObject({ source: "ml", person: null, score: 0 });
    expect(parseItemFace({ id: "f3", box: {} })).toBeNull();
  });

  it("reads the people of a photo from objects, and from plain names of an older server", () => {
    expect(parsePersonRefs([{ id: "p1", name: "Alma" }, { id: "p2", name: null }, { name: "no id" }, "Bob", ""])).toEqual([
      { id: "p1", name: "Alma" },
      { id: "p2", name: null },
      { id: null, name: "Bob" },
    ]);
    expect(parsePersonRefs(undefined)).toEqual([]);
  });

  it("puts the details' people in ItemDetails", async () => {
    stubFetch(() => ({ body: { item: rawItem("a"), exif: null, people: [{ id: "p1", name: "Alma" }] } }));
    expect((await getDetails("a"))?.people).toEqual([{ id: "p1", name: "Alma" }]);
  });
});

describe("requests", () => {
  it("reads and changes the settings, and deletes the face data", async () => {
    const calls = stubFetch((call) => {
      if (call.method === "DELETE") return { body: { ok: true } };
      return { body: { enabled: call.method === "PUT", scanned: 1, pending: 2 } };
    });
    expect((await getFaceSettings()).enabled).toBe(false);
    expect((await setFacesEnabled(true)).enabled).toBe(true);
    await deleteFaceData();
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual(["GET /v1/media/faces/settings", "PUT /v1/media/faces/settings", "DELETE /v1/media/faces"]);
    expect(calls[1]!.body).toEqual({ enabled: true });
  });

  it("lists people with the server's own defaults unless asked otherwise", async () => {
    const calls = stubFetch(() => ({ body: { people: [rawPerson("p1"), { name: "no id" }, rawPerson("p2")] } }));
    expect((await listPeople()).map((p) => p.id)).toEqual(["p1", "p2"]);
    await listPeople({ includeHidden: true, minFaces: 1 });
    expect(calls.map((c) => c.url)).toEqual(["/v1/media/people", "/v1/media/people?min_faces=1&include_hidden=true"]);
  });

  it("gets one person, and edits a name, hidden, a birth date and the cover", async () => {
    const calls = stubFetch((call) => ({ body: rawPerson("p1", call.method === "PATCH" ? { name: "Alma" } : {}) }));
    expect((await getPerson("p1"))?.id).toBe("p1");
    await patchPerson("p1", { name: "  Alma  ", hidden: true, birthDate: null, coverFaceId: "f9" });
    await patchPerson("p1", { name: "   " });
    await patchPerson("p1", { name: null });
    expect(calls[1]).toMatchObject({ method: "PATCH", url: "/v1/media/people/p1" });
    expect(calls[1]!.body).toEqual({ name: "Alma", hidden: true, birth_date: null, cover_face_id: "f9" });
    expect(calls[2]!.body).toEqual({ name: null });
    expect(calls[3]!.body).toEqual({ name: null });
  });

  it("cuts a name at the server's limit", async () => {
    const calls = stubFetch(() => ({ body: rawPerson("p1") }));
    await patchPerson("p1", { name: "x".repeat(120) });
    expect((calls[0]!.body as { name: string }).name).toHaveLength(80);
  });

  it("merges people into the one to keep", async () => {
    const calls = stubFetch(() => ({ body: rawPerson("keep", { face_count: 9 }) }));
    const person = await mergePeople("keep", ["a", "b"]);
    expect(person.faceCount).toBe(9);
    expect(calls[0]).toMatchObject({ method: "POST", url: "/v1/media/people/keep/merge" });
    expect(calls[0]!.body).toEqual({ from: ["a", "b"] });
  });

  it("pages a person's photos through their own route with the listing's filters, without a person parameter", async () => {
    const calls = stubFetch(() => ({ body: { items: [rawItem("i1"), { id: "" }], next_cursor: "c2", total: 40 } }));
    const page = await listPersonItems("p 1", { origin: "all", kind: "photo", q: "beach", from: "2026-01-01", cursor: "c1", limit: 50, person: "p 1" });
    expect(page).toMatchObject({ nextCursor: "c2" });
    expect(page.items.map((i) => i.id)).toEqual(["i1"]);
    const url = new URL(calls[0]!.url, "http://x");
    expect(url.pathname).toBe("/v1/media/people/p%201/items");
    expect(Object.fromEntries(url.searchParams)).toEqual({ scope: "gallery", origin: "all", kind: "photo", q: "beach", from: "2026-01-01", cursor: "c1", limit: "50" });
  });

  it("sends person= on the listing's query (smart search restricts to that person)", () => {
    const params = new URLSearchParams(galleryQueryString({ q: "dog", smart: true, person: "p1" }));
    expect(params.get("person")).toBe("p1");
    expect(new URLSearchParams(galleryQueryString({})).has("person")).toBe(false);
  });

  it("reads an item's faces and drops the ones without a usable box", async () => {
    const calls = stubFetch(() => ({
      body: { faces: [{ id: "f1", box: { x: 0.1, y: 0.1, w: 0.2, h: 0.2 }, score: 0.9, source: "ml", person: null }, { id: "f2", box: { w: 0 } }] },
    }));
    expect((await listItemFaces("it 1")).map((f) => f.id)).toEqual(["f1"]);
    expect(calls[0]!.url).toBe("/v1/media/items/it%201/faces");
  });

  it("assigns a face to a person, to nobody, or to a new person", async () => {
    const calls = stubFetch(() => ({ body: { id: "f1", box: { x: 0, y: 0, w: 0.1, h: 0.1 }, source: "manual", person: { id: "p1", name: "Alma" } } }));
    const face = await assignFace("f1", { personId: "p1" });
    expect(face?.person?.id).toBe("p1");
    await assignFace("f1", { personId: null });
    await assignFace("f1", { newPerson: "  Chiara " });
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual(Array(3).fill("PUT /v1/media/faces/f1"));
    expect(calls.map((c) => c.body)).toEqual([{ person_id: "p1" }, { person_id: null }, { new_person: "Chiara" }]);
  });

  it("returns null when the server's answer to an assignment has no usable face", async () => {
    stubFetch(() => ({ body: { ok: true } }));
    expect(await assignFace("f1", { personId: "p1" })).toBeNull();
  });

  it("lists the faces without a person with their cursor, falling back to the face thumbnail", async () => {
    const calls = stubFetch(() => ({
      body: { faces: [{ id: "f1", item_id: "i1", thumb_url: "/v1/media/faces/f1/thumb", score: 0.9 }, { id: "f2" }, { score: 1 }], next_cursor: "n" },
    }));
    const page = await listUnassignedFaces({ cursor: "c0" });
    expect(page.nextCursor).toBe("n");
    expect(page.faces).toEqual([
      { id: "f1", itemId: "i1", thumbUrl: "/v1/media/faces/f1/thumb", score: 0.9 },
      { id: "f2", itemId: null, thumbUrl: faceThumbUrl("f2"), score: 0 },
    ]);
    expect(calls[0]!.url).toBe("/v1/media/faces/unassigned?limit=60&cursor=c0");
  });
});
