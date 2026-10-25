import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { IDBFactory } from "fake-indexeddb";
import { ShareComposer, ShareSheet } from "./ShareComposer";
import { fileShare, listShares, saveShare, sendShare } from "../lib/share";
import { Attachments } from "../views/Chat";
import { parseItem } from "../api/timeline";
vi.mock("heic2any", () => ({ default: vi.fn(async () => new Blob(["jpeg"], { type: "image/jpeg" })) }));

const nativeClone = structuredClone;
// jsdom Files are not supported by Node's structuredClone. Match the browser's
// immutable File clone behavior while leaving other IndexedDB values native.
function cloneWithFiles(value: unknown): unknown {
  if (value instanceof File) return new File([value], value.name, { type: value.type, lastModified: value.lastModified });
  if (Array.isArray(value)) return value.map(cloneWithFiles);
  if (value && Object.getPrototypeOf(value) === Object.prototype) {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, cloneWithFiles(item)]));
  }
  return nativeClone(value);
}

beforeEach(() => {
  vi.stubGlobal("structuredClone", cloneWithFiles);
  vi.stubGlobal("indexedDB", new IDBFactory());
  vi.stubGlobal("fetch", vi.fn(async () => new Response('{}', { status: 202 })));
  URL.createObjectURL = vi.fn(() => "blob:preview");
  URL.revokeObjectURL = vi.fn();
  window.history.replaceState({}, "", "/");
});
const entry = () => ({ ...fileShare([new File(["image"], "photo.png", { type: "image/png" }), new File(["pdf"], "doc.pdf", { type: "application/pdf" })]), title: "A page", text: ["Shared words"], url: ["https://example.com/page"] });

describe("share sheet", () => {
  it("renders previews and releases image URLs on close", () => {
    const { unmount } = render(() => <ShareSheet entry={entry()} onSent={() => {}} onClose={() => {}} />);
    expect(screen.getByRole("dialog", { name: "Share with Lucina" })).toBeInTheDocument();
    expect(screen.getByAltText("photo.png")).toHaveAttribute("src", "blob:preview");
    expect(screen.getByText(/doc.pdf/)).toHaveTextContent("KB");
    expect(screen.getByRole("link", { name: /example.com/ })).toHaveAttribute("href", "https://example.com/page");
    expect(screen.getByText("Shared words")).toBeInTheDocument();
    unmount();
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:preview");
  });

  it("posts multipart fields and CSRF, clears the inbox and query on 202", async () => {
    const share = entry(); await saveShare(share);
    window.history.replaceState({}, "", `/?keep=1&share=${share.id}#chat`);
    const sent = vi.fn();
    render(() => <ShareSheet entry={share} onSent={sent} onClose={() => {}} />);
    fireEvent.input(screen.getByLabelText("Add a note"), { target: { value: "Tomorrow" } });
    fireEvent.click(screen.getByRole("button", { name: "Remind me" }));
    fireEvent.click(screen.getByRole("button", { name: "Send to Lucina" }));
    await waitFor(() => expect(sent).toHaveBeenCalledOnce());
    const [path, init] = vi.mocked(fetch).mock.calls[0]!;
    expect(path).toBe("/v1/share");
    expect(init).toMatchObject({ method: "POST", credentials: "include", headers: { "X-Secretary-CSRF": "1" } });
    const data = init!.body as FormData;
    expect(Object.fromEntries([...data.entries()].filter(([k]) => k !== "file"))).toEqual({ shareId: share.id, intent: "remind", note: "Tomorrow", lang: "en", channel: "web", source: "web-share", title: "A page", text: "Shared words", url: "https://example.com/page" });
    expect(data.getAll("file").map((f) => (f as File).name)).toEqual(["photo.png", "doc.pdf"]);
    expect(await listShares()).toEqual([]);
    expect(window.location.search + window.location.hash).toBe("?keep=1#chat");
  });

  it("keeps the entry on 500 and retries with the same shareId", async () => {
    vi.mocked(fetch).mockResolvedValue(new Response('{"error":"Unavailable"}', { status: 500 }));
    const share = entry(); await saveShare(share);
    const sent = vi.fn();
    render(() => <ShareSheet entry={share} onSent={sent} onClose={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Send to Lucina" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Unavailable");
    expect(await listShares()).toHaveLength(1);
    expect(sent).not.toHaveBeenCalled();
    vi.mocked(fetch).mockResolvedValue(new Response('{}', { status: 202 }));
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(sent).toHaveBeenCalledOnce());
    expect((vi.mocked(fetch).mock.calls[1]![1]!.body as FormData).get("shareId")).toBe(share.id);
  });

  it("recovers pending shares without a query and opens picker files in the same sheet", async () => {
    await saveShare(entry());
    render(() => <ShareComposer />);
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(screen.getByAltText("photo.png")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    fireEvent.change(screen.getByLabelText("Attach files", { selector: "input" }), { target: { files: [new File(["text"], "new.txt")] } });
    await waitFor(async () => expect(await listShares()).toHaveLength(2));
    expect(await screen.findByRole("dialog")).toHaveTextContent("new.txt");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("converts HEIC to JPEG and gives unknown files an octet-stream type", async () => {
    await sendShare(fileShare([new File(["heic"], "photo.HEIC"), new File(["data"], "unknown")]), "ask", "");
    const files = (vi.mocked(fetch).mock.calls[0]![1]!.body as FormData).getAll("file") as File[];
    expect(files.map((f) => [f.name, f.type])).toEqual([["photo.jpg", "image/jpeg"], ["unknown", "application/octet-stream"]]);
  });

  it("rejects empty shares and excessive multipart parts before sending", async () => {
    await expect(sendShare(fileShare([]), "ask", "")).rejects.toThrow("empty");
    await expect(sendShare(fileShare(Array.from({ length: 26 }, () => new File(["x"], "x.txt"))), "ask", "")).rejects.toThrow("limits");
    expect(fetch).not.toHaveBeenCalled();
  });
});

it("parses extended attachments, renders thumbnails, safe links and kind chips", () => {
  const item = parseItem({ id: "1", ts: new Date().toISOString(), role: "user", channel: "web", text: "Shared", attachments: [
    { kind: "image", name: "photo", mediaId: "itm_photo", mime: "image/jpeg", bytes: 123 },
    { kind: "video", name: "clip", mediaId: "itm_video" },
    { kind: "link", url: "https://example.com/" },
    { kind: "link", name: "unsafe", url: "javascript:alert(1)" },
    ...["audio", "pdf", "document", "file", "text", "contact", "location", "voice"].map((kind) => ({ kind })),
    { kind: "invalid" },
  ] })!;
  expect(item.attachments![0]).toMatchObject({ mime: "image/jpeg", bytes: 123 });
  render(() => <Attachments items={item.attachments!} />);
  expect(screen.getByAltText("photo")).toHaveAttribute("src", "/v1/media/items/itm_photo/thumb?size=256");
  expect(screen.getByAltText("clip")).toHaveAttribute("src", "/v1/media/items/itm_video/thumb?size=256");
  expect(screen.getByRole("link", { name: /example.com/ })).toHaveAttribute("href", "https://example.com/");
  expect(screen.queryByRole("link", { name: /unsafe/ })).toBeNull();
  expect(screen.getAllByRole("listitem")).toHaveLength(12);
});
