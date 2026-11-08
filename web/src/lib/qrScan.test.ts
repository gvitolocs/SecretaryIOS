import { afterEach, describe, expect, it, vi } from "vitest";
import { approveCodeFrom, createQrReader } from "./qrScan";

describe("approveCodeFrom", () => {
  it.each([
    ["https://getsecretary.tech/approve?c=w0CwKBb_4q6MacWLPEja7w", "w0CwKBb_4q6MacWLPEja7w"],
    ["  https://getsecretary.tech/approve?c=abcdef123  ", "abcdef123"],
    ["http://127.0.0.1:18899/approve?c=localcode1", "localcode1"],
    ["482913", "482913"],
    ["482 913", "482913"],
    ["482-913", "482913"],
  ])("reads %s", (text, code) => {
    expect(approveCodeFrom(text, "127.0.0.1:18899")).toBe(code);
  });

  it.each([
    "https://evil.example/approve?c=abcdef123",
    "http://getsecretary.tech/approve?c=abcdef123",
    "https://getsecretary.tech/other?c=abcdef123",
    "https://getsecretary.tech/approve",
    "https://getsecretary.tech/approve?c=bad%20code",
    "12345",
    "1234567",
    "hello",
    "",
  ])("ignores %s", (text) => {
    expect(approveCodeFrom(text, "app.example")).toBeNull();
  });
});

describe("createQrReader", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("uses the browser's BarcodeDetector when it reads QR codes", async () => {
    const detect = vi.fn(async () => [{ rawValue: "https://getsecretary.tech/approve?c=abcdef123" }]);
    vi.stubGlobal(
      "BarcodeDetector",
      class {
        static getSupportedFormats = async () => ["qr_code", "ean_13"];
        detect = detect;
      },
    );
    const reader = await createQrReader();
    expect(await reader.read(document.createElement("video"))).toEqual(["https://getsecretary.tech/approve?c=abcdef123"]);
    expect(detect).toHaveBeenCalled();
  });

  it("falls back to jsQR, which waits for a frame", async () => {
    vi.stubGlobal("BarcodeDetector", undefined);
    const reader = await createQrReader();
    expect(await reader.read(document.createElement("video"))).toEqual([]);
  });
});
