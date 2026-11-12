import { describe, expect, it } from "vitest";
import { parcel } from "../test/parcelFixtures";
import { stubFetch } from "../test/fetch";
import { ApiError } from "./client";
import { addParcel, deleteParcel, getParcel, listParcels, parseParcel, parseParcelList, refreshParcel, setParcelLabel } from "./parcels";

describe("parcels api", () => {
  it("lists parcels and the digest hour", async () => {
    const calls = stubFetch(() => ({ body: { parcels: [parcel()], digestHour: 9 } }));
    const list = await listParcels();
    expect(calls[0]!).toMatchObject({ method: "GET", url: "/v1/parcels" });
    expect(list.digestHour).toBe(9);
    expect(list.parcels).toHaveLength(1);
    expect(list.parcels[0]!).toMatchObject({ id: "pc-1a2b3c4d", carrier: "postnord", near: true, schedule: "hourly" });
    expect(list.parcels[0]!.events).toHaveLength(3);
  });

  it("adds a parcel with only the fields given", async () => {
    const calls = stubFetch(() => ({ body: { parcel: parcel() } }));
    await addParcel({ number: "0035", label: "Headphones" });
    expect(calls[0]!).toMatchObject({ method: "POST", url: "/v1/parcels", body: { number: "0035", label: "Headphones" }, headers: { "x-secretary-csrf": "1" } });
    expect(calls[0]!.body).not.toHaveProperty("carrier");
  });

  it("gets, refreshes, relabels and deletes one parcel by its encoded id", async () => {
    const calls = stubFetch((c) => (c.method === "DELETE" ? { body: { ok: true } } : { body: { parcel: parcel({ label: null }) } }));
    expect((await getParcel("pc 1")).label).toBeNull();
    await refreshParcel("pc 1");
    await setParcelLabel("pc 1", null);
    expect(await deleteParcel("pc 1")).toEqual({ ok: true });
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET /v1/parcels/pc%201",
      "POST /v1/parcels/pc%201/refresh",
      "PATCH /v1/parcels/pc%201",
      "DELETE /v1/parcels/pc%201",
    ]);
    expect(calls[2]!.body).toEqual({ label: null });
  });

  it("carries the server's code and message on a 400", async () => {
    stubFetch(() => ({ status: 400, body: { error: { code: "bad_request", message: "That tracking number looks invalid." } } }));
    const err = await addParcel({ number: "x" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 400, code: "bad_request", message: "That tracking number looks invalid." });
  });

  it("reports parcels_unavailable on a 503", async () => {
    stubFetch(() => ({ status: 503, body: { error: { code: "parcels_unavailable", message: "Lucina is unreachable." } } }));
    await expect(listParcels()).rejects.toMatchObject({ status: 503, code: "parcels_unavailable" });
  });

  it("tolerates unknown stages and missing optional fields", () => {
    const p = parseParcel({ id: "pc-1", number: "123456", status: { stage: "held_at_depot" }, events: [{ description: "x", stage: "weird" }, null, 5] });
    expect(p).toMatchObject({
      id: "pc-1",
      carrier: null,
      label: null,
      state: "active",
      schedule: "daily",
      near: false,
      lastChecked: null,
      nextCheck: null,
      lastError: null,
    });
    expect(p?.status).toEqual({ stage: "held_at_depot", text: null, lastUpdate: null, expectedDelivery: null, location: null, destinationCountry: null });
    expect(p?.events).toEqual([{ time: null, description: "x", location: null, stage: "weird" }]);
  });

  it("drops entries without an id and survives a malformed list", () => {
    expect(parseParcel(null)).toBeNull();
    expect(parseParcel({ number: "1" })).toBeNull();
    expect(parseParcelList({ parcels: [{ number: "1" }, { id: "a" }] }).parcels.map((p) => p.id)).toEqual(["a"]);
    expect(parseParcelList(undefined)).toEqual({ parcels: [], digestHour: null });
  });

  it("rejects a reply without a parcel", async () => {
    stubFetch(() => ({ body: {} }));
    await expect(getParcel("pc-1")).rejects.toThrow();
  });
});
