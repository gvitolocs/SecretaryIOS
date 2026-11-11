import { describe, expect, it } from "vitest";
import { setLocale } from "../i18n";
import { daily, delivered, parcel } from "../test/parcelFixtures";
import { carrierName, dateOnly, lastChange, parcelStage, parcelTitle, sortParcels, splitParcels, stageName, stageTone } from "./parcels";

describe("carrierName", () => {
  it("names the common carriers", () => {
    const cases: Record<string, string> = {
      postnord: "PostNord",
      gls: "GLS",
      "gls-dk": "GLS",
      dao: "DAO",
      "bring-posten": "Bring",
      dhl: "DHL",
      "dhl-express": "DHL",
      ups: "UPS",
      "poste-italiane": "Poste Italiane",
      dpd: "DPD",
      "dpd-de": "DPD",
      budbee: "Budbee",
      instabox: "Instabox",
    };
    for (const [id, name] of Object.entries(cases)) expect(carrierName(id)).toBe(name);
  });

  it("title-cases an unknown id with spaces for dashes", () => {
    expect(carrierName("some-new-carrier")).toBe("Some New Carrier");
    expect(carrierName("glsx")).toBe("Glsx");
  });

  it("is empty without a carrier", () => {
    expect(carrierName(null)).toBe("");
    expect(carrierName(undefined)).toBe("");
    expect(carrierName("  ")).toBe("");
  });
});

describe("stages", () => {
  it("names every known stage in the app's language", () => {
    expect(stageName("in_transit")).toBe("In transit");
    expect(stageName("out_for_delivery")).toBe("Out for delivery");
    expect(stageName("ready_for_pickup")).toBe("Ready for pickup");
    setLocale("da");
    expect(stageName("delivered")).toBe("Leveret");
  });

  it("makes a readable name out of an unknown stage", () => {
    expect(stageName("held_at_depot")).toBe("Held at depot");
    expect(stageName("")).toBe("Unknown");
    expect(stageName(null)).toBe("Unknown");
  });

  it("colours stages by tone", () => {
    expect(stageTone("delivered")).toBe("ok");
    expect(stageTone("in_transit")).toBe("info");
    expect(stageTone("ready_for_pickup")).toBe("warn");
    expect(stageTone("failed_attempt")).toBe("bad");
    expect(stageTone("pending")).toBe("muted");
    expect(stageTone("never_heard_of_it")).toBe("muted");
  });

  it("takes a parcel's stage from its status, else its newest event", () => {
    expect(parcelStage(parcel())).toBe("out_for_delivery");
    expect(parcelStage(parcel({ status: null }))).toBe("out_for_delivery");
    expect(parcelStage(daily())).toBe("unknown");
  });
});

describe("titles and order", () => {
  it("titles by label, else carrier and number, else the number", () => {
    expect(parcelTitle(parcel())).toBe("Headphones");
    expect(parcelTitle(delivered())).toBe("DHL JD014600003SE");
    expect(parcelTitle(daily())).toBe("RR123456789DK");
  });

  it("dates a change by the status, the newest event, the last check, then the day added", () => {
    expect(lastChange(parcel())).toBe(Date.parse("2026-09-28T05:33:46Z"));
    expect(lastChange(parcel({ status: null }))).toBe(Date.parse("2026-09-28T05:33:46Z"));
    expect(lastChange(parcel({ status: null, events: [] }))).toBe(Date.parse("2026-11-11T16:00:00Z"));
    expect(lastChange(daily())).toBe(Date.parse("2026-11-11T15:58:00Z"));
  });

  it("puts active parcels first, newest change first, then delivered and stopped", () => {
    const older = parcel({ id: "old", status: { stage: "in_transit", text: null, lastUpdate: "2026-09-01T00:00:00Z", expectedDelivery: null, location: null, destinationCountry: null }, events: [] });
    const stopped = parcel({ id: "stopped", state: "stopped", schedule: "stopped" });
    const list = [delivered(), older, stopped, parcel()];
    const { active, done } = splitParcels(list);
    expect(active.map((p) => p.id)).toEqual(["pc-1a2b3c4d", "old"]);
    expect(done.map((p) => p.id)).toEqual(["stopped", "pc-99999999"]);
    expect(sortParcels(list).map((p) => p.id)).toEqual(["pc-1a2b3c4d", "old", "stopped", "pc-99999999"]);
  });
});

describe("dateOnly", () => {
  it("reads a date as a local day", () => {
    const d = dateOnly("2026-09-28");
    expect([d?.getFullYear(), d?.getMonth(), d?.getDate()]).toEqual([2026, 8, 28]);
    expect(dateOnly(null)).toBeNull();
    expect(dateOnly("soon")).toBeNull();
  });
});
