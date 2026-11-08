import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  buildShareMeta,
  draftAttachment,
  draftFor,
  filesFromClipboard,
  filesFromDataTransfer,
  formatBytes,
  formatCoordinate,
  formatDuration,
  hasFileTransfer,
  isVoiceAutoCaption,
  visibleText,
  locationAttachment,
  prepareDraftFiles,
  releaseDraft,
} from "./attachments";

function transfer(files: File[], types: string[] = ["Files"]): DataTransfer {
  const items = files.map((file) => ({ kind: "file", type: file.type, getAsFile: () => file }));
  return { files, items, types } as unknown as DataTransfer;
}

beforeEach(() => {
  URL.createObjectURL = vi.fn(() => "blob:preview");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("clipboard and drop", () => {
  it("takes files from a paste", () => {
    const file = new File(["hello"], "notes.txt", { type: "text/plain" });
    expect(filesFromClipboard(transfer([file]))).toEqual([file]);
  });

  it("ignores a text-only paste", () => {
    const data = { files: [], items: [], types: ["text/plain"] } as unknown as DataTransfer;
    expect(filesFromClipboard(data)).toEqual([]);
    expect(hasFileTransfer(data)).toBe(false);
  });

  it("falls back to DataTransfer items when files is empty", () => {
    const file = new File(["x"], "photo.png", { type: "image/png" });
    const data = { files: [], items: [{ kind: "file", getAsFile: () => file }], types: ["Files"] } as unknown as DataTransfer;
    expect(filesFromDataTransfer(data)).toEqual([file]);
    expect(filesFromClipboard(data)).toEqual([file]);
    expect(hasFileTransfer(data)).toBe(true);
  });

  it("ignores a drop without files", () => {
    expect(filesFromDataTransfer(null)).toEqual([]);
    expect(hasFileTransfer(null)).toBe(false);
  });
});

describe("draft metadata", () => {
  it("lists every file by index with its dimensions and duration, plus the location", () => {
    const file = new File(["x"], "clip.mp4", { type: "video/mp4" });
    const draft = { ...draftFor(file), width: 1920, height: 1080, durationMs: 12_400.4 };
    const other = { ...draftFor(new File(["y"], "doc.pdf", { type: "application/pdf" })), durationMs: 0 };
    const plain = draftFor(new File(["z"], "a.bin"));
    const meta = JSON.parse(buildShareMeta([draft, other, plain], { lat: 45.4642, lon: 9.19 })!);
    expect(meta).toEqual({
      files: [
        { index: 0, width: 1920, height: 1080, durationMs: 12400 },
        { index: 1, durationMs: 0 },
        { index: 2 },
      ],
      location: { lat: 45.4642, lon: 9.19 },
    });
  });

  it("omits the field only when there is neither a file nor a location", () => {
    expect(buildShareMeta([])).toBeUndefined();
    expect(buildShareMeta([], { lat: 1.5, lon: 2.5 })).toBe('{"location":{"lat":1.5,"lon":2.5}}');
    expect(buildShareMeta([draftFor(new File(["x"], "a.bin"))])).toBe('{"files":[{"index":0}]}');
  });

  it("drops non-finite and negative numbers", () => {
    const draft = { ...draftFor(new File(["x"], "a.png", { type: "image/png" })), width: 0, height: -1, durationMs: Number.NaN };
    expect(buildShareMeta([draft])).toBe('{"files":[{"index":0}]}');
  });
});

describe("optimistic attachments", () => {
  it("mirrors a draft into the timeline attachment", () => {
    const draft = { ...draftFor(new File(["abc"], "photo.png", { type: "image/png" })), width: 10, height: 20, durationMs: 0, mediaId: "m1" };
    expect(draftAttachment(draft)).toMatchObject({
      kind: "image",
      name: "photo.png",
      mime: "image/png",
      bytes: 3,
      previewUrl: "blob:preview",
      width: 10,
      height: 20,
      durationMs: 0,
      mediaId: "m1",
    });
  });

  it("turns a location into a card with a formatted fallback name", () => {
    expect(locationAttachment({ lat: 45.4642, lon: 9.19 })).toEqual({
      kind: "location",
      name: "45.4642, 9.19",
      lat: 45.4642,
      lon: 9.19,
    });
  });

  it("releases a preview URL once", () => {
    const draft = draftFor(new File(["x"], "a.png", { type: "image/png" }));
    releaseDraft(draft);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
  });
});

describe("formatting and preparing", () => {
  it("formats bytes and durations for the tray", () => {
    expect(formatBytes(3)).toBe("3 B");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(12 * 1024)).toBe("12 KB");
    expect(formatBytes(2.5 * 1024 ** 2)).toBe("2.5 MB");
    expect(formatBytes(undefined)).toBe("");
    expect(formatDuration(0)).toBe("0:00");
    expect(formatDuration(65_000)).toBe("1:05");
    expect(formatDuration(3_725_000)).toBe("1:02:05");
    expect(formatDuration(Number.NaN)).toBe("");
  });

  it("formats coordinates without trailing zeroes", () => {
    expect(formatCoordinate(45.4642)).toBe("45.4642");
    expect(formatCoordinate(-9.19)).toBe("-9.19");
    expect(formatCoordinate(12.0)).toBe("12");
  });

  it("prepares files without converting or probing", async () => {
    const files = [new File(["x"], "a.txt", { type: "text/plain" }), new File(["y"], "b.pdf", { type: "application/pdf" })];
    const drafts = await prepareDraftFiles(files, { convert: false, probe: false });
    expect(drafts.map((d) => d.kind)).toEqual(["file", "pdf"]);
    expect(drafts.map((d) => d.name)).toEqual(["a.txt", "b.pdf"]);
  });
});

describe("voice note auto caption", () => {
  const voice = (name: string) => ({ kind: "voice", name });
  it("hides the legacy caption of voice notes", () => {
    expect(visibleText("\u{1F4CE} voice-note.wav", [voice("voice-note.wav")])).toBe("");
    expect(visibleText("  \u{1F4CE} note.m4a ", [voice("note.m4a")])).toBe("");
    expect(visibleText("\u{1F4CE} voice-1.wav, voice-2.wav", [voice("voice-1.wav"), voice("voice-2.wav")])).toBe("");
  });
  it("keeps a real caption", () => {
    expect(visibleText("Listen to this", [voice("voice-note.wav")])).toBe("Listen to this");
    expect(visibleText("\u{1F4CE} voice-note.wav and more", [voice("voice-note.wav")])).toBe("\u{1F4CE} voice-note.wav and more");
  });
  it("keeps names of non-voice attachments", () => {
    expect(visibleText("\u{1F4CE} report.pdf", [{ kind: "pdf", name: "report.pdf" }])).toBe("\u{1F4CE} report.pdf");
    expect(isVoiceAutoCaption("\u{1F4CE} report.pdf, note.m4a", [{ kind: "pdf", name: "report.pdf" }, voice("note.m4a")])).toBe(false);
    expect(isVoiceAutoCaption("\u{1F4CE} x.wav", undefined)).toBe(false);
  });
});
