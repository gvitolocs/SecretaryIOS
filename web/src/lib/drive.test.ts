import { describe, expect, it } from "vitest";
import {
  breadcrumbs,
  browseUrl,
  cleanPath,
  defaultOrder,
  downloadUrl,
  encodeSegment,
  fileHref,
  filterLabelKey,
  filtersSearch,
  folderHref,
  formatBytes,
  guessType,
  imageUrl,
  itemLocation,
  itemPreview,
  itemsUrl,
  itemType,
  NO_FILTERS,
  openParam,
  parseArchive,
  parseBrowse,
  parseDriveItem,
  parseDriveRoute,
  parseFilters,
  parseItemsPage,
  parseRoots,
  parseTextPreview,
  previewLines,
  thumbnailUrl,
  tokenClass,
  typeBadge,
  typeLabelKey,
} from "./drive";

const PDF = {
  id: "itm_pdf",
  kind: "file",
  title: "L05 slides.pdf",
  mime: "application/pdf",
  size: 1234567,
  created_at: "2026-03-04T10:00:00Z",
  thumb_state: "ready",
  thumb_url: "/v1/media/s/tok/itm_pdf/thumb.jpg",
  urls: {
    original: "/v1/media/items/itm_pdf/original",
    download: "/v1/media/items/itm_pdf/original?download=1",
    thumb: "/v1/media/items/itm_pdf/thumb?size=256&v=1",
    grid: "/v1/media/s/tok/itm_pdf/r/grid?v=1",
    pdf: "/v1/media/items/itm_pdf/pdf",
    text: "/v1/media/items/itm_pdf/text",
    archive: null,
    preview: "https://evil.example/x.jpg",
    large: "//evil.example/y.jpg",
  },
  drive: {
    folder: "Lezioni",
    path: "Digital Forensics/L05 slides.pdf",
    dir: "Digital Forensics",
    name: "L05 slides.pdf",
    ext: "pdf",
    type: "pdf",
    preview: "pdf",
    language: null,
    modified: "2026-03-04T10:00:00.000Z",
    text: "ready",
  },
  snippet: "the <b>NOT</b> escaped text",
  unknown_field: 42,
};

describe("parseDriveItem", () => {
  it("keeps the Drive fields and only same-origin URLs", () => {
    const item = parseDriveItem(PDF)!;
    expect(item.id).toBe("itm_pdf");
    expect(item.size).toBe(1234567);
    expect(item.urls.pdf).toBe("/v1/media/items/itm_pdf/pdf");
    expect(item.urls.download).toBe("/v1/media/items/itm_pdf/original?download=1");
    expect(item.urls.preview).toBeUndefined();
    expect(item.urls.large).toBeUndefined();
    expect(item.urls.archive).toBeUndefined();
    expect(item.drive).toMatchObject({ folder: "Lezioni", dir: "Digital Forensics", type: "pdf", preview: "pdf", ext: "pdf" });
    expect(item.snippet).toBe("the <b>NOT</b> escaped text");
    expect(itemLocation(item)).toBe("Lezioni / Digital Forensics");
  });

  it("drops items without an id and survives junk", () => {
    expect(parseDriveItem(null)).toBeNull();
    expect(parseDriveItem({ title: "x" })).toBeNull();
    const item = parseDriveItem({ id: "a", urls: "nope", drive: { folder: 3 }, size: -1, thumb_url: "javascript:alert(1)" })!;
    expect(item.drive).toBeNull();
    expect(item.size).toBeNull();
    expect(item.thumbUrl).toBeNull();
    expect(item.urls).toEqual({});
  });

  it("guesses the type on an older server (no drive field)", () => {
    expect(itemType(parseDriveItem({ id: "p", kind: "photo", title: "IMG_1.HEIC" })!)).toBe("image");
    expect(itemType(parseDriveItem({ id: "v", kind: "video", title: "a.mov" })!)).toBe("video");
    expect(itemType(parseDriveItem({ id: "z", kind: "file", title: "src.zip" })!)).toBe("archive");
    expect(itemPreview(parseDriveItem({ id: "d", kind: "file", title: "notes.docx" })!)).toBe("office");
    expect(guessType("file", "text/plain", "")).toBe("text");
    expect(guessType("file", "", "rs")).toBe("code");
    expect(guessType("file", "", "bin")).toBe("other");
  });

  it("cleans unsafe Drive paths", () => {
    expect(cleanPath("/a//b/../c/./d")).toBe("a/b/c/d");
    expect(cleanPath("a\\b/c")).toBe("c");
    const item = parseDriveItem({ id: "x", drive: { folder: "Tracer", path: "../../etc/passwd", type: "weird" } })!;
    expect(item.drive?.path).toBe("etc/passwd");
    expect(item.drive?.type).toBe("other");
  });
});

describe("pictures", () => {
  it("prefers the tokenised thumbnail, then the JXL grid where it decodes", () => {
    const item = parseDriveItem(PDF)!;
    expect(thumbnailUrl(item, false)).toBe("/v1/media/s/tok/itm_pdf/thumb.jpg");
    const noToken = { ...item, thumbUrl: null };
    expect(thumbnailUrl(noToken, true)).toBe("/v1/media/s/tok/itm_pdf/r/grid?v=1");
    expect(thumbnailUrl(noToken, false)).toBe("/v1/media/items/itm_pdf/thumb?size=256&v=1");
    expect(thumbnailUrl({ ...item, thumbState: "pending" }, false)).toBeNull();
  });

  it("opens images at the tokenised original, else the original", () => {
    expect(imageUrl(parseDriveItem(PDF)!)).toBe("/v1/media/s/tok/itm_pdf/file");
    expect(imageUrl(parseDriveItem({ id: "i", urls: { original: "/v1/media/items/i/original" } })!)).toBe("/v1/media/items/i/original");
    expect(downloadUrl(parseDriveItem({ id: "a b" })!)).toBe("/v1/media/items/a%20b/original?download=1");
  });
});

describe("listings", () => {
  it("parses the roots and ignores unknown shapes", () => {
    const roots = parseRoots({
      folders: [{ folder: "Lezioni", files: 87, bytes: 11811160064, modified: "2026-10-20T00:00:00Z" }, { folder: "a/b" }, { nope: 1 }],
      albums: [{ id: "sys-voice-memos", title: "Voice memos", count: 12 }, { title: "no id" }],
      converters: { pdf_thumbs: true, office_pdf: false, junk: "x" },
    });
    expect(roots.folders.map((f) => f.folder)).toEqual(["Lezioni"]);
    expect(roots.albums).toEqual([{ id: "sys-voice-memos", title: "Voice memos", count: 12 }]);
    expect(roots.converters).toEqual({ pdf_thumbs: true, office_pdf: false });
    expect(parseRoots("garbage")).toEqual({ folders: [], albums: [], converters: {} });
  });

  it("parses a browse page and builds crumbs when the server sends none", () => {
    const page = parseBrowse(
      {
        folder: "Lezioni",
        path: "Digital Forensics",
        folders: [{ name: "Lab", path: "Digital Forensics/Lab", files: 12, bytes: 3456 }, { name: "bad", path: "" }],
        files: [PDF, { no: "id" }],
        total: 87,
        next_offset: 200,
      },
      "Lezioni",
      "Digital Forensics",
    );
    expect(page.breadcrumbs).toEqual([
      { name: "Lezioni", path: "" },
      { name: "Digital Forensics", path: "Digital Forensics" },
    ]);
    expect(page.folders.map((f) => f.path)).toEqual(["Digital Forensics/Lab"]);
    expect(page.files.map((f) => f.id)).toEqual(["itm_pdf"]);
    expect(page.nextOffset).toBe(200);
    expect(parseBrowse({ next_offset: null }, "X", "").nextOffset).toBeNull();
  });

  it("parses an items page", () => {
    expect(parseItemsPage({ items: [PDF], next_cursor: "c2", total: 3 })).toMatchObject({ nextCursor: "c2", total: 3 });
    expect(parseItemsPage({ items: [], next_cursor: null }).nextCursor).toBeNull();
  });

  it("builds crumbs", () => {
    expect(breadcrumbs("Tracer", "src/a")).toEqual([
      { name: "Tracer", path: "" },
      { name: "src", path: "src" },
      { name: "a", path: "src/a" },
    ]);
  });
});

describe("text and archives", () => {
  it("reads pending and highlighted text", () => {
    expect(parseTextPreview({ state: "pending" })).toEqual({ state: "pending" });
    const p = parseTextPreview({
      language: "rust",
      truncated: true,
      bytes: 30,
      text: "fn main() {}\n// two\nthree\n",
      lines: [[["kw", "fn"], ["", " "], ["fn", "main"], ["punct", "() {}"]], [["com", "// two"]]],
    });
    if (p.state !== "ready") throw new Error("not ready");
    expect(p.truncated).toBe(true);
    const lines = previewLines(p);
    expect(lines).toHaveLength(3);
    expect(lines[0]![2]).toEqual(["fn", "main"]);
    expect(lines[2]).toEqual([["", "three"]]);
  });

  it("falls back to plain lines without highlighting", () => {
    const p = parseTextPreview({ text: "a\r\nb", lines: "junk" });
    if (p.state !== "ready") throw new Error("not ready");
    expect(previewLines(p)).toEqual([[["", "a"]], [["", "b"]]]);
  });

  it("maps token classes to CSS classes, and nothing else", () => {
    expect(tokenClass("kw")).toBe("tok-kw");
    expect(tokenClass("head")).toBe("tok-head");
    expect(tokenClass("")).toBe("");
    expect(tokenClass('x" onclick="')).toBe("");
  });

  it("parses an archive listing", () => {
    const a = parseArchive({ format: "zip", total: 3, truncated: false, entries: [{ path: "src/", dir: true }, { path: "src/main.rs", size: 1234 }, { size: 1 }] });
    expect(a.entries).toEqual([
      { path: "src/", size: null, dir: true, modified: null },
      { path: "src/main.rs", size: 1234, dir: false, modified: null },
    ]);
  });
});

describe("queries", () => {
  it("builds browse queries", () => {
    expect(browseUrl("Lezioni", "", NO_FILTERS)).toBe("/v1/media/drive/browse?folder=Lezioni&limit=200");
    expect(browseUrl("Lezioni", "Digital Forensics", { q: " slides ", type: "pdf", sort: "size", order: "asc" }, 200)).toBe(
      "/v1/media/drive/browse?folder=Lezioni&path=Digital+Forensics&sort=size&order=asc&type=pdf&q=slides&offset=200&limit=200",
    );
  });

  it("builds item queries", () => {
    expect(itemsUrl({ q: "digital forensics", type: "presentation" })).toBe("/v1/media/items?scope=drive&q=digital+forensics&type=presentation&limit=200");
    expect(itemsUrl({ album: "sys-voice-memos", cursor: "c2" })).toBe("/v1/media/items?scope=drive&album=sys-voice-memos&cursor=c2&limit=200");
  });

  it("knows the server's default orders", () => {
    expect(defaultOrder("name")).toBe("asc");
    expect(defaultOrder("type")).toBe("asc");
    expect(defaultOrder("modified")).toBe("desc");
    expect(defaultOrder("size")).toBe("desc");
  });
});

describe("routes", () => {
  it("parses Drive routes", () => {
    expect(parseDriveRoute("/drive")).toEqual({ view: "roots" });
    expect(parseDriveRoute("/drive/")).toEqual({ view: "roots" });
    expect(parseDriveRoute("/drive/folder/Lezioni")).toEqual({ view: "folder", folder: "Lezioni", path: "" });
    expect(parseDriveRoute("/drive/folder/Lezioni/Digital%20Forensics/Lab")).toEqual({ view: "folder", folder: "Lezioni", path: "Digital Forensics/Lab" });
    expect(parseDriveRoute("/drive/file/itm_1")).toEqual({ view: "file", id: "itm_1" });
    expect(parseDriveRoute("/drive/album/sys-voice-memos")).toEqual({ view: "album", id: "sys-voice-memos" });
    expect(parseDriveRoute("/drive/folder/%E0%A4%A")).toEqual({ view: "roots" });
    expect(parseDriveRoute("/drive/folder/..")).toEqual({ view: "roots" });
    expect(parseDriveRoute("/drive/folder/Tracer/a/../../b")).toEqual({ view: "folder", folder: "Tracer", path: "a/b" });
    expect(parseDriveRoute("/drive/nope")).toEqual({ view: "roots" });
  });

  it("builds hrefs that round-trip and suit chat route buttons", () => {
    const href = folderHref("Lezioni", "Digital Forensics/L(05) it's #1?");
    expect(href).toBe("/drive/folder/Lezioni/Digital%20Forensics/L%2805%29%20it%27s%20%231%3F");
    expect(/^\/[A-Za-z0-9/_\-.%]{0,120}$/.test(href)).toBe(true);
    expect(parseDriveRoute(href)).toEqual({ view: "folder", folder: "Lezioni", path: "Digital Forensics/L(05) it's #1?" });
    expect(fileHref("itm_a/b")).toBe("/drive/file/itm_a%2Fb");
    expect(encodeSegment("~!*")).toBe("%7E%21%2A");
  });

  it("reads and writes filters in the search string", () => {
    expect(parseFilters("?q=forensics&type=pdf&sort=size&order=asc")).toEqual({ q: "forensics", type: "pdf", sort: "size", order: "asc" });
    expect(parseFilters("?type=nope&sort=bogus&order=asc")).toEqual(NO_FILTERS);
    expect(filtersSearch({ q: " a b ", type: "code", sort: null, order: null }, "itm_1")).toBe("?q=a+b&type=code&open=itm_1");
    expect(filtersSearch(NO_FILTERS)).toBe("");
    expect(openParam("?open=itm_9")).toBe("itm_9");
    expect(openParam("?q=x")).toBeNull();
  });
});

describe("labels", () => {
  it("names types and filters", () => {
    expect(typeLabelKey("presentation")).toBe("typePresentation");
    expect(filterLabelKey("pdf")).toBe("filterPdf");
    expect(typeBadge("pdf")).toEqual({ badge: "PDF", tone: "red" });
    expect(typeBadge("code", "rs").badge).toBe("RS");
    expect(typeBadge("code", "swiftx").badge).toBe("</>");
    expect(typeBadge("other").badge).toBe("");
  });

  it("formats sizes for the language", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(999)).toBe("999 B");
    expect(formatBytes(1_234_567)).toBe("1.2 MB");
    expect(formatBytes(1_234_567, "da")).toBe("1,2 MB");
    expect(formatBytes(11_811_160_064)).toBe("11.8 GB");
    expect(formatBytes(null)).toBe("");
  });
});
