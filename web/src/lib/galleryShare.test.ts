import { describe, expect, it, vi } from "vitest";
import { galleryItem } from "../test/galleryFixtures";
import { galleryShare } from "../test/galleryParityFixtures";
import {
  canShareFiles,
  canShareLinks,
  downloadEntries,
  downloadSequentially,
  expiryDays,
  expiryState,
  fileNameOf,
  fitsAsFiles,
  orderShares,
  PUBLIC_HOST,
  shareable,
  shareUrl,
} from "./galleryShare";

const loc = (url: string) => {
  const u = new URL(url);
  return { protocol: u.protocol, host: u.host, origin: u.origin };
};

describe("expiry", () => {
  it("maps the choices to days, never to no expiry", () => {
    expect(["1", "7", "30"].map((c) => expiryDays(c as "1"))).toEqual([1, 7, 30]);
    expect(expiryDays("never")).toBeNull();
  });

  it("says what a link's row says about its life", () => {
    const now = Date.parse("2026-10-28T12:00:00Z");
    expect(expiryState({ revokedAt: "2026-10-01T00:00:00Z", expiresAt: null }, now)).toEqual({ kind: "off" });
    expect(expiryState({ revokedAt: null, expiresAt: null }, now)).toEqual({ kind: "never" });
    expect(expiryState({ revokedAt: null, expiresAt: "2026-10-28T12:00:00Z" }, now)).toEqual({ kind: "expired" });
    expect(expiryState({ revokedAt: null, expiresAt: "garbage" }, now)).toEqual({ kind: "expired" });
    // The nearest day, never below 1, and a clock a little off doesn't change a fresh link's count; turned off wins over expired.
    expect(expiryState({ revokedAt: null, expiresAt: "2026-10-28T13:00:00Z" }, now)).toEqual({ kind: "days", days: 1 });
    expect(expiryState({ revokedAt: null, expiresAt: "2026-11-04T12:00:00Z" }, now)).toEqual({ kind: "days", days: 7 });
    expect(expiryState({ revokedAt: null, expiresAt: "2026-11-04T12:00:05Z" }, now)).toEqual({ kind: "days", days: 7 });
    expect(expiryState({ revokedAt: null, expiresAt: "2026-11-04T11:59:55Z" }, now)).toEqual({ kind: "days", days: 7 });
    expect(expiryState({ revokedAt: null, expiresAt: "2026-11-27T12:00:00Z" }, now)).toEqual({ kind: "days", days: 30 });
    expect(expiryState({ revokedAt: "2026-10-01T00:00:00Z", expiresAt: "2020-01-01T00:00:00Z" }, now)).toEqual({ kind: "off" });
  });
});

describe("shareUrl", () => {
  it("uses this page's own origin only when it is a public https host", () => {
    const s = galleryShare("TOKEN");
    expect(shareUrl(s, loc("https://getsecretary.tech/gallery"))).toBe("https://getsecretary.tech/a/TOKEN");
    expect(shareUrl(s, loc("https://example.org:8443/x"))).toBe("https://example.org:8443/a/TOKEN");
  });

  it("hands out the public host from a dev server, an http page or a local name", () => {
    const s = galleryShare("TOKEN");
    for (const url of ["http://localhost:5173/", "https://localhost:8443/", "http://192.168.1.4/", "http://getsecretary.tech/", "https://secretary.localhost/", "https://app.test/", "https://127.0.0.1/"]) {
      expect(shareUrl(s, loc(url)), url).toBe(`${PUBLIC_HOST}/a/TOKEN`);
    }
  });
});

describe("the list order", () => {
  it("puts live links first, newest first within each group", () => {
    const list = [
      galleryShare("old-live", { createdAt: "2026-10-01T00:00:00Z" }),
      galleryShare("off", { live: false, createdAt: "2026-10-27T00:00:00Z" }),
      galleryShare("new-live", { createdAt: "2026-10-20T00:00:00Z" }),
    ];
    expect(orderShares(list).map((s) => s.token)).toEqual(["new-live", "old-live", "off"]);
    expect(list.map((s) => s.token)).toEqual(["old-live", "off", "new-live"]);
  });
});

describe("files", () => {
  it("names a file by its original name, else its title, else its id", () => {
    expect(fileNameOf(galleryItem("a"))).toBe("IMG_a.JPG");
    expect(fileNameOf(galleryItem("a", { origin: { filename: null, folder: null, importedFrom: null } }))).toBe("a.jpg");
    expect(fileNameOf(galleryItem("a", { title: "", origin: { filename: null, folder: null, importedFrom: null }, mime: "image/jpeg" }))).toBe("a.jpg");
    expect(fileNameOf(galleryItem("a", { title: "", origin: { filename: null, folder: null, importedFrom: null }, mime: "" }))).toBe("a");
  });

  it("only hands over a few small files; the rest go as the link", () => {
    const small = Array.from({ length: 10 }, (_, i) => galleryItem(String(i), { size: 1_000_000 }));
    expect(fitsAsFiles(small)).toBe(true);
    expect(fitsAsFiles([...small, galleryItem("x")])).toBe(false);
    expect(fitsAsFiles([galleryItem("big", { size: 200 * 1024 * 1024 })])).toBe(false);
    expect(fitsAsFiles([])).toBe(false);
  });

  it("asks the browser whether it can share files and links", () => {
    const files = [new File(["x"], "a.jpg", { type: "image/jpeg" })];
    const can = { share: vi.fn(), canShare: vi.fn(() => true) };
    expect(canShareFiles(files, can)).toBe(true);
    expect(canShareFiles([], can)).toBe(false);
    expect(canShareFiles(files, { share: vi.fn(), canShare: () => false })).toBe(false);
    expect(canShareFiles(files, {} as Navigator)).toBe(false);
    expect(canShareFiles(files, undefined)).toBe(false);
    expect(canShareLinks({ share: vi.fn() })).toBe(true);
    expect(canShareLinks({} as Navigator)).toBe(false);
  });

  it("keeps photos and videos only", () => {
    const list = [galleryItem("p"), galleryItem("v", { kind: "video" }), { ...galleryItem("f"), kind: "file" as never }];
    expect(shareable(list).map((i) => i.id)).toEqual(["p", "v"]);
  });
});

describe("downloads", () => {
  it("lists the download URL of each item (else its original) under its file name", () => {
    const a = galleryItem("a");
    const b = galleryItem("b", { urls: { original: "/o/b" } });
    const c = galleryItem("c", { urls: {} });
    expect(downloadEntries([a, b, c])).toEqual([
      { href: "/v1/media/items/a/download", name: "IMG_a.JPG" },
      { href: "/o/b", name: "IMG_b.JPG" },
    ]);
  });

  it("starts them one after another with a pause between, and stops when cancelled", async () => {
    const log: string[] = [];
    const entries = ["1", "2", "3"].map((n) => ({ href: `/h/${n}`, name: n }));
    const started = await downloadSequentially(
      entries,
      (e) => log.push(`start ${e.name}`),
      async (ms) => void log.push(`wait ${ms}`),
      600,
    );
    expect(started).toBe(3);
    expect(log).toEqual(["start 1", "wait 600", "start 2", "wait 600", "start 3"]);

    const second: string[] = [];
    let stop = false;
    const n = await downloadSequentially(
      entries,
      (e) => {
        second.push(e.name);
        if (e.name === "2") stop = true;
      },
      async () => {},
      0,
      () => stop,
    );
    expect(n).toBe(2);
    expect(second).toEqual(["1", "2"]);
  });
});
