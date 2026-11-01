import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setImageCoreForTests } from "../lib/imageCore";
import { installDesktopViewer, lucinaViewer } from "../lib/desktopViewer";
import { DesktopViewerHost } from "./DesktopViewer";

// What Secretary Code does when Finder opens a picture: window.lucinaViewer.open(folder, i).
const MAC = { id: "mac-0b1c", name: "MacBook Air", kind: "mac" as const };

beforeEach(() => {
  setImageCoreForTests(null);
  let n = 0;
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: vi.fn(() => `blob:local/${++n}`), revokeObjectURL: vi.fn() }));
});
afterEach(() => {
  lucinaViewer.close();
  lucinaViewer.setDevice(null);
  vi.unstubAllGlobals();
});

describe("the Mac viewer entry point", () => {
  it("opens a folder at the clicked file, pages through it and tells the shell when it closes", async () => {
    const ready = vi.fn();
    window.addEventListener("lucinaviewer:ready", ready);
    installDesktopViewer();
    expect(window.lucinaViewer).toBe(lucinaViewer);
    expect(ready).toHaveBeenCalledOnce();

    render(() => <DesktopViewerHost />);
    const onClose = vi.fn();
    const folder = [
      { name: "beach.jpg", url: "secretary-file:///Users/me/Pictures/beach.jpg" },
      { name: "bad.jpg", url: "javascript:alert(1)" },
      { name: "desk.png", blob: new Blob(["png"], { type: "image/png" }) },
    ];
    expect(window.lucinaViewer!.open(folder, 2, { onClose })).toBe(true);
    const dialog = await screen.findByRole("dialog", { name: "Photo 2 of 2" });
    expect(dialog.querySelector("img")).toHaveAttribute("src", "blob:local/1");
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    expect(screen.getByRole("dialog", { name: "Photo 1 of 2" }).querySelector("img")).toHaveAttribute(
      "src",
      "secretary-file:///Users/me/Pictures/beach.jpg",
    );
    // Without a device there's nothing to save to.
    expect(screen.queryByRole("button", { name: "Save to Lucina" })).toBeNull();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(onClose).toHaveBeenCalledOnce();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:local/1");
  });

  it("saves a local file as this Mac", async () => {
    const calls: { method: string; url: string; body?: unknown }[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
      const url = String(input);
      const method = init.method ?? "GET";
      calls.push({ method, url, body: typeof init.body === "string" ? JSON.parse(init.body) : init.body });
      const json = (b: unknown) => new Response(JSON.stringify(b), { status: 200 });
      if (url === "/v1/media/uploads") return json({ upload_id: "upl-9", offset: 0, size: 3, finished: false, duplicate: false, item_id: null });
      if (url.startsWith("/v1/media/uploads/upl-9?offset=")) return json({ upload_id: "upl-9", offset: 3, size: 3, finished: false, duplicate: false, item_id: null });
      if (url.endsWith("/finish")) return json({ item_id: "itm-1", duplicate: false, version: 1 });
      return json({});
    }));
    render(() => <DesktopViewerHost />);
    lucinaViewer.open([{ name: "desk.png", blob: new Blob(["png"], { type: "image/png" }), assetId: "p1", takenAt: "2026-10-27T09:00:00Z" }], 0, { device: MAC });
    fireEvent.click(await screen.findByRole("button", { name: "Save to Lucina" }));
    expect(await screen.findByRole("button", { name: "Saved to Lucina" })).toBeInTheDocument();
    expect(calls[0]).toMatchObject({ method: "PUT", url: "/v1/media/devices/mac-0b1c" });
    expect(calls[1]!.body).toMatchObject({ filename: "desk.png", meta: { device: "mac-0b1c", asset_id: "p1", source: "mac", taken_at: "2026-10-27T09:00:00Z" } });
    await waitFor(() => expect(calls.some((c) => c.url.endsWith("/finish"))).toBe(true));
  });

  it("refuses to open nothing and refuses a bad device id", () => {
    expect(lucinaViewer.open([{ name: "x", url: "javascript:void 0" }])).toBe(false);
    expect(lucinaViewer.isOpen()).toBe(false);
    expect(() => lucinaViewer.setDevice({ ...MAC, id: "has space" })).toThrow();
  });
});
