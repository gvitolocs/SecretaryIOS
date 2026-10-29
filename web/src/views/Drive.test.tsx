import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { type Call, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import DriveView from "./Drive";

const ROOTS = {
  folders: [
    { folder: "Lezioni", files: 87, bytes: 11811160064, modified: "2026-10-20T10:00:00Z" },
    { folder: "Tracer", files: 4898, bytes: 123, modified: "2026-10-21T10:00:00Z" },
  ],
  albums: [{ id: "sys-voice-memos", title: "Voice memos", count: 12 }],
  converters: { pdf_thumbs: true, office_pdf: false, pdf_text: true },
};

function file(id: string, path: string, type: string, preview: string, over: Record<string, unknown> = {}) {
  const name = path.split("/").pop()!;
  const dir = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
  return {
    id,
    kind: "file",
    title: name,
    mime: "application/octet-stream",
    size: 2048,
    created_at: "2026-03-04T10:00:00Z",
    thumb_state: "none",
    urls: {
      original: `/v1/media/items/${id}/original`,
      download: `/v1/media/items/${id}/original?download=1`,
      pdf: type === "pdf" ? `/v1/media/items/${id}/pdf` : null,
      text: ["text", "code", "pdf"].includes(type) ? `/v1/media/items/${id}/text` : null,
      archive: type === "archive" ? `/v1/media/items/${id}/archive` : null,
    },
    drive: { folder: over.folder ?? "Lezioni", path, dir, name, ext: name.split(".").pop(), type, preview, language: null, modified: "2026-03-04T10:00:00.000Z", text: "ready" },
    snippet: null,
    ...over,
  };
}

const SLIDES = file("itm_pdf", "Digital Forensics/L05 slides.pdf", "pdf", "pdf");
const MAIN = file("itm_rs", "src/main.rs", "code", "code", { folder: "Tracer" });
const ZIP = file("itm_zip", "Digital Forensics/lab.rar", "archive", "archive");

function browse(call: Call) {
  const url = new URL(call.url, "http://x");
  const folder = url.searchParams.get("folder");
  const path = url.searchParams.get("path") ?? "";
  if (folder === "Lezioni" && path === "") {
    return {
      body: {
        folder,
        path,
        breadcrumbs: [{ name: "Lezioni", path: "" }],
        folders: [{ name: "Digital Forensics", path: "Digital Forensics", files: 12, bytes: 3456, modified: null }],
        files: [],
        total: 0,
        next_offset: null,
      },
    };
  }
  if (folder === "Lezioni" && path === "Digital Forensics") {
    const offset = Number(url.searchParams.get("offset") ?? 0);
    return {
      body: {
        folder,
        path,
        breadcrumbs: [
          { name: "Lezioni", path: "" },
          { name: "Digital Forensics", path: "Digital Forensics" },
        ],
        folders: [],
        files: offset === 0 ? [SLIDES] : [ZIP],
        total: 2,
        next_offset: offset === 0 ? 1 : null,
      },
    };
  }
  if (folder === "Tracer") {
    return { body: { folder, path, breadcrumbs: [{ name: "Tracer", path: "" }], folders: [], files: [MAIN], total: 1, next_offset: null } };
  }
  return { status: 404, body: { error: "unknown folder" } };
}

function routes() {
  return <Route path="/drive/*rest" component={DriveView} />;
}

describe("Drive", () => {
  afterEach(() => vi.restoreAllMocks());

  it("lists the roots, browses folders with breadcrumbs and loads more", async () => {
    const calls = stubFetch((call) => {
      if (call.url === "/v1/media/drive") return { body: ROOTS };
      if (call.url.startsWith("/v1/media/drive/browse")) return browse(call);
      return { status: 404, body: {} };
    });
    renderAt("/drive", routes);
    fireEvent.click(await screen.findByRole("button", { name: /Lezioni/ }));
    expect(screen.getByRole("button", { name: /Voice memos/ })).toBeInTheDocument();

    fireEvent.click(await screen.findByRole("button", { name: /Digital Forensics/ }));
    expect(await screen.findByRole("button", { name: /L05 slides\.pdf/ })).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/media/drive/browse?folder=Lezioni&path=Digital+Forensics&limit=200")).toBe(true);
    expect(calls.every((c) => c.credentials === "same-origin")).toBe(true);

    const crumbs = screen.getByRole("navigation", { name: "Location" });
    expect(within(crumbs).getByRole("link", { name: "Lezioni" }).getAttribute("href")).toBe("/drive/folder/Lezioni");
    expect(within(crumbs).getByRole("link", { name: "Digital Forensics" })).toHaveAttribute("aria-current", "page");

    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    expect(await screen.findByRole("button", { name: /lab\.rar/ })).toBeInTheDocument();
    expect(calls.some((c) => c.url.includes("path=Digital+Forensics&offset=1&limit=200"))).toBe(true);
    expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
  });

  it("filters by type and sorts through the browse query", async () => {
    const calls = stubFetch((call) => (call.url.startsWith("/v1/media/drive/browse") ? browse(call) : { status: 404, body: {} }));
    renderAt("/drive/folder/Lezioni/Digital%20Forensics", routes);
    await screen.findByRole("button", { name: /L05 slides\.pdf/ });
    fireEvent.click(screen.getByRole("button", { name: "PDF" }));
    await waitFor(() => expect(calls.some((c) => c.url.includes("&type=pdf"))).toBe(true));
    expect(screen.getByRole("button", { name: "PDF" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.change(screen.getByRole("combobox", { name: "Sort by" }), { target: { value: "size" } });
    await waitFor(() => expect(calls.some((c) => c.url.includes("sort=size&order=desc&type=pdf"))).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Descending" }));
    await waitFor(() => expect(calls.some((c) => c.url.includes("sort=size&order=asc&type=pdf"))).toBe(true));
  });

  it("searches everywhere from the root and shows snippets as plain text", async () => {
    const hit = { ...SLIDES, snippet: "the <b>chain</b> of custody" };
    const calls = stubFetch((call) => {
      if (call.url === "/v1/media/drive") return { body: ROOTS };
      if (call.url.startsWith("/v1/media/items?")) return { body: { ok: true, items: [hit], next_cursor: null, total: 1 } };
      return { status: 404, body: {} };
    });
    const { container } = renderAt("/drive", routes);
    await screen.findByRole("button", { name: /Lezioni/ });
    fireEvent.input(screen.getByRole("searchbox", { name: "Search the Drive" }), { target: { value: "chain of custody" } });
    expect(await screen.findByText("the <b>chain</b> of custody")).toBeInTheDocument();
    expect(container.querySelector(".drive-row__snippet b")).toBeNull();
    expect(screen.getByText(/Lezioni \/ Digital Forensics/)).toBeInTheDocument();
    expect(screen.getByText("1 result")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/media/items?scope=drive&q=chain+of+custody&limit=200")).toBe(true);
  });

  it("previews a code file with highlighted tokens, line numbers and a copy button", async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    stubFetch((call) => {
      if (call.url.startsWith("/v1/media/drive/browse")) return browse(call);
      if (call.url === "/v1/media/items/itm_rs/text?highlight=1") {
        return {
          body: {
            language: "rust",
            source: "file",
            truncated: true,
            bytes: 40,
            text: "fn main() {\n    // <script>alert(1)</script>\n}\n",
            lines: [[["kw", "fn"], ["", " "], ["fn", "main"], ["punct", "() {"]], [["", "    "], ["com", "// <script>alert(1)</script>"]], [["punct", "}"]]],
          },
        };
      }
      return { status: 404, body: {} };
    });
    const { container } = renderAt("/drive/folder/Tracer", routes);
    fireEvent.click(await screen.findByRole("button", { name: /main\.rs/ }));
    const dialog = await screen.findByRole("dialog", { name: "main.rs" });
    expect(await within(dialog).findByText("fn")).toHaveClass("tok-kw");
    expect(within(dialog).getByText("main")).toHaveClass("tok-fn");
    expect(within(dialog).getByText("// <script>alert(1)</script>")).toHaveClass("tok-com");
    expect(container.querySelector("script")).toBeNull();
    expect(within(dialog).getByText(/Only the beginning/)).toBeInTheDocument();
    expect(container.querySelectorAll(".drive-code__no")).toHaveLength(3);
    expect(within(dialog).getByRole("link", { name: /Download/ }).getAttribute("href")).toBe("/v1/media/items/itm_rs/original?download=1");
    fireEvent.click(within(dialog).getByRole("button", { name: /Copy/ }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("fn main() {\n    // <script>alert(1)</script>\n}\n"));

    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("button", { name: /main\.rs/ })).toBeInTheDocument();
  });

  it("waits while the server is still reading a file", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let asks = 0;
    stubFetch((call) => {
      if (call.url.startsWith("/v1/media/drive/browse")) return browse(call);
      if (call.url === "/v1/media/items/itm_rs") return { body: MAIN };
      if (call.url.startsWith("/v1/media/items/itm_rs/text")) {
        asks++;
        return asks === 1 ? { status: 202, body: { state: "pending" } } : { body: { text: "hello\n", lines: [[["", "hello"]]] } };
      }
      return { status: 404, body: {} };
    });
    renderAt("/drive/folder/Tracer?open=itm_rs", routes);
    expect(await screen.findByText("Still reading this file…")).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(3100);
    expect(await screen.findByText("hello")).toBeInTheDocument();
    expect(asks).toBe(2);
  });

  it("opens /drive/file/<id> links over the file's folder, with the PDF inline", async () => {
    const calls = stubFetch((call) => {
      if (call.url === "/v1/media/items/itm_pdf") return { body: SLIDES };
      if (call.url.startsWith("/v1/media/drive/browse")) return browse(call);
      return { status: 404, body: {} };
    });
    const { container } = renderAt("/drive/file/itm_pdf", routes);
    const dialog = await screen.findByRole("dialog", { name: "L05 slides.pdf" });
    await waitFor(() => expect(container.querySelector("object.drive-pdf")?.getAttribute("data")).toBe("/v1/media/items/itm_pdf/pdf"));
    expect(within(dialog).getAllByRole("link", { name: /Open in new tab/ })[0]!.getAttribute("href")).toBe("/v1/media/items/itm_pdf/pdf");
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/media/drive/browse?folder=Lezioni&path=Digital+Forensics&limit=200")).toBe(true));
    fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
    expect(await screen.findByRole("button", { name: /L05 slides\.pdf/ })).toBeInTheDocument();
  });

  it("explains archives the server cannot list", async () => {
    stubFetch((call) => {
      if (call.url.startsWith("/v1/media/drive/browse")) return browse(call);
      if (call.url === "/v1/media/items/itm_zip") return { body: ZIP };
      if (call.url === "/v1/media/items/itm_zip/archive") return { status: 415, body: { error: "rar is not supported" } };
      return { status: 404, body: {} };
    });
    renderAt("/drive/file/itm_zip", routes);
    expect(await screen.findByText(/This archive format can't be listed/)).toBeInTheDocument();
  });

  it("lists archive entries", async () => {
    const tgz = file("itm_tgz", "Digital Forensics/src.tar.gz", "archive", "archive");
    stubFetch((call) => {
      if (call.url.startsWith("/v1/media/drive/browse")) return browse(call);
      if (call.url === "/v1/media/items/itm_tgz") return { body: tgz };
      if (call.url === "/v1/media/items/itm_tgz/archive") {
        return { body: { format: "tar.gz", total: 2, truncated: false, entries: [{ path: "src/", dir: true }, { path: "src/main.rs", size: 1234 }] } };
      }
      return { status: 404, body: {} };
    });
    renderAt("/drive/file/itm_tgz", routes);
    expect(await screen.findByText("src/main.rs")).toBeInTheDocument();
    expect(screen.getByText("1.2 KB")).toBeInTheDocument();
    expect(screen.getByText("tar.gz · 2 entries")).toBeInTheDocument();
  });

  it("falls back to the Drive albums on an older server", async () => {
    const calls = stubFetch((call) => {
      if (call.url === "/v1/media/drive") return { status: 404, body: { error: "not found" } };
      if (call.url === "/v1/media/albums?scope=drive") return { body: { albums: [{ id: "alb_1", title: "Nextcloud", count: 3 }] } };
      if (call.url.startsWith("/v1/media/items?")) {
        return { body: { ok: true, items: [{ id: "itm_old", kind: "photo", title: "IMG_1.JPG", album: "Nextcloud" }], next_cursor: null } };
      }
      return { status: 404, body: {} };
    });
    renderAt("/drive", routes);
    fireEvent.click(await screen.findByRole("button", { name: /Nextcloud/ }));
    expect(await screen.findByRole("button", { name: /IMG_1\.JPG/ })).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/media/items?scope=drive&album=alb_1&limit=200")).toBe(true);
    expect(screen.getByText("Image")).toBeInTheDocument();
  });

  it("speaks Italian and Danish", async () => {
    stubFetch((call) => (call.url === "/v1/media/drive" ? { body: ROOTS } : { status: 404, body: {} }));
    setLocale("it");
    renderAt("/drive", routes);
    expect(await screen.findByRole("heading", { name: "Cartelle" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Fogli di calcolo" })).toBeInTheDocument();
    setLocale("da");
    expect(await screen.findByRole("heading", { name: "Mapper" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Regneark" })).toBeInTheDocument();
  });
});

describe("Drive desktop", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("shows the page header, type chips and list rows", async () => {
    stubFetch((call) => {
      if (call.url === "/v1/media/drive") return { body: ROOTS };
      if (call.url.startsWith("/v1/media/drive/browse")) return browse(call);
      return { status: 404, body: {} };
    });
    renderAt("/drive/folder/Lezioni/Digital%20Forensics", routes);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent("Drive");
    expect(screen.getByText("Lezioni › Digital Forensics")).toBeInTheDocument();
    expect(document.querySelector(".drive-crumbs")).toBeNull();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(screen.getByRole("searchbox")).toHaveClass("pg-input");
    expect(screen.getByRole("button", { name: "All" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /L05 slides\.pdf/ })).toBeInTheDocument();
  });
});
