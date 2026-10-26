import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { clearMediaItems } from "../api/media";
import type { Attachment } from "../api/timeline";
import { Attachments } from "../components/ChatAttachments";
import { resetJxlSupport } from "../lib/jxl";
import { reloadChatFromStorage, stopSharePolling } from "../state/chat";
import { stubFetch } from "../test/fetch";
import ChatView from "./Chat";

class FakeXHR {
  static instances: FakeXHR[] = [];
  upload: { onprogress?: (event: ProgressEvent) => void } = {};
  status = 0;
  responseText = "";
  withCredentials = false;
  timeout = 0;
  body: FormData | null = null;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  onabort: (() => void) | null = null;
  open = vi.fn();
  setRequestHeader = vi.fn();
  send = vi.fn((form: FormData) => {
    this.body = form;
  });
  constructor() {
    FakeXHR.instances.push(this);
  }
  progress(loaded: number, total: number) {
    this.upload.onprogress?.({ loaded, total } as ProgressEvent);
  }
  finish(status: number, body: unknown) {
    this.status = status;
    this.responseText = JSON.stringify(body);
    this.onload?.();
  }
}

/** An Image whose JPEG XL probe fails at once (jsdom never loads images). */
class NoJxlImage {
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  set src(_value: string) {
    queueMicrotask(() => this.onerror?.());
  }
}

function transfer(files: File[]): DataTransfer {
  return {
    files,
    items: files.map((file) => ({ kind: "file", getAsFile: () => file })),
    types: ["Files"],
  } as unknown as DataTransfer;
}

beforeEach(() => {
  reloadChatFromStorage();
  stubFetch((call) =>
    call.url.startsWith("/v1/timeline") ? { body: { items: [], nextBefore: null } } : { body: {} },
  );
  FakeXHR.instances = [];
  vi.stubGlobal("XMLHttpRequest", FakeXHR as unknown as typeof XMLHttpRequest);
});

afterEach(() => {
  stopSharePolling();
  vi.unstubAllGlobals();
});

describe("chat attachments composer", () => {
  it("attaches files pasted into the composer but leaves plain text alone", async () => {
    render(() => <ChatView />);
    const box = screen.getByLabelText("Message Lucina");
    fireEvent.paste(box, {
      clipboardData: {
        files: [],
        items: [],
        types: ["text/plain"],
        getData: () => "hello",
      },
    });
    expect(document.querySelector(".attachment-tray")).toBeNull();

    fireEvent.paste(box, {
      clipboardData: transfer([new File(["notes"], "notes.txt", { type: "text/plain" })]),
    });
    expect(await screen.findByText(/notes\.txt/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove notes.txt" })).toBeInTheDocument();
  });

  it("shows a drop overlay and attaches dropped files", async () => {
    render(() => <ChatView />);
    const chat = document.querySelector(".chat")!;
    const data = transfer([new File(["pdf"], "dropped.pdf", { type: "application/pdf" })]);

    fireEvent.dragEnter(chat, { dataTransfer: data });
    expect(screen.getByText("Drop files to send to Lucina")).toBeInTheDocument();

    fireEvent.drop(chat, { dataTransfer: data });
    expect(await screen.findByText(/dropped\.pdf/)).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByText("Drop files to send to Lucina")).toBeNull());
  });

  it("opens the attach menu with photos, files, camera and location", () => {
    render(() => <ChatView />);
    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    expect(screen.getByRole("menuitem", { name: /Photos & videos/ })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: /Files/ })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: /Camera/ })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: /Location/ })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("shares the current location once and sends it as meta.location", async () => {
    const getCurrentPosition = vi.fn((ok: PositionCallback) =>
      ok({ coords: { latitude: 45.4642, longitude: 9.19 } } as GeolocationPosition),
    );
    vi.stubGlobal("navigator", Object.assign(Object.create(navigator), { geolocation: { getCurrentPosition } }));
    render(() => <ChatView />);
    fireEvent.click(screen.getByRole("button", { name: "Add attachment" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /Location/ }));
    expect(getCurrentPosition).toHaveBeenCalledOnce();
    expect(screen.getByText(/Current location: 45.4642, 9.19/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(FakeXHR.instances).toHaveLength(1));
    expect(JSON.parse(String(FakeXHR.instances[0]!.body!.get("meta")))).toEqual({ location: { lat: 45.4642, lon: 9.19 } });
    expect(FakeXHR.instances[0]!.body!.getAll("file")).toEqual([]);
  });

  it("sends the caption with the files, rings the upload progress and retries with the same shareId", async () => {
    render(() => <ChatView />);
    const box = screen.getByLabelText("Message Lucina");
    fireEvent.paste(box, { clipboardData: transfer([new File(["notes"], "notes.txt", { type: "text/plain" })]) });
    await screen.findByRole("button", { name: "Remove notes.txt" });
    fireEvent.input(box, { target: { value: "Read this" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(FakeXHR.instances).toHaveLength(1));
    const first = FakeXHR.instances[0]!;
    expect(first.body!.get("note")).toBe("Read this");
    expect(first.body!.get("intent")).toBe("ask");
    expect(document.querySelector(".attachment-tray")).toBeNull();

    first.progress(50, 100);
    expect(await screen.findByText("Sending… 50%")).toBeInTheDocument();
    expect(document.querySelector<HTMLElement>(".share-progress-ring")?.style.getPropertyValue("--share-progress")).toBe("50%");

    first.finish(500, { error: "Unavailable" });
    expect(await screen.findByText("Not sent — Retry")).toBeInTheDocument();
    expect(document.querySelector(".share-progress-ring")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(FakeXHR.instances).toHaveLength(2));
    expect(FakeXHR.instances[1]!.body!.get("shareId")).toBe(first.body!.get("shareId"));
    FakeXHR.instances[1]!.finish(202, { stage: "reading", userItemId: "u1" });
    expect(await screen.findByText("Lucina is reading…")).toBeInTheDocument();
    expect(screen.queryByText("Not sent — Retry")).toBeNull();
  });
});

describe("attachment bubbles", () => {
  beforeEach(() => {
    vi.stubGlobal("Image", NoJxlImage);
    resetJxlSupport();
    clearMediaItems();
  });

  const images: Attachment[] = ["a", "b", "c"].map((id) => ({ kind: "image", name: `${id}.jpg`, mediaId: id, width: 400, height: 300 }));

  it("lays several images out as an album and pages through them in the lightbox", async () => {
    const calls = stubFetch((call) =>
      call.url === "/v1/media/items/a"
        ? { body: { id: "a", thumbhash: "nQcKNZiAh3dwiIh4iHiIiICACPh4", urls: { preview: "/v1/media/items/a/r/preview", grid: "https://evil.example/x" } } }
        : { body: {} },
    );
    render(() => <Attachments items={images} />);
    expect(document.querySelector(".attachments--album.attachments--n3")).not.toBeNull();
    expect(await screen.findByAltText("a.jpg")).toHaveAttribute("src", "/v1/media/items/a/thumb?size=256");

    // The first image (index 0) opens too.
    fireEvent.click(screen.getByRole("button", { name: "Open image a.jpg" }));
    const dialog = screen.getByRole("dialog", { name: "Attachment 1 of 3" });
    await waitFor(() => expect(dialog.querySelector("img")).toHaveAttribute("src", "/v1/media/items/a/r/preview"));
    expect(calls.some((c) => c.url === "/v1/media/items/a")).toBe(true);

    fireEvent.keyDown(window, { key: "ArrowRight" });
    expect(screen.getByRole("dialog", { name: "Attachment 2 of 3" }).querySelector("img")).toHaveAttribute(
      "src",
      "/v1/media/items/b/thumb?size=1024",
    );
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    fireEvent.keyDown(window, { key: "ArrowLeft" });
    expect(screen.getByRole("dialog", { name: "Attachment 3 of 3" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("paints the ThumbHash placeholder and keeps a single image's aspect ratio", async () => {
    stubFetch(() => ({ body: {} }));
    render(() => <Attachments items={[{ kind: "image", name: "one.jpg", mediaId: "one", width: 300, height: 600, thumbhash: "nQcKNZiAh3dwiIh4iHiIiICACPh4" }]} />);
    expect(document.querySelector(".attachments--n1")).not.toBeNull();
    const frame = document.querySelector<HTMLElement>(".media-frame")!;
    expect(frame.style.aspectRatio).toBe("300 / 600");
    expect(frame.style.getPropertyValue("--media-ar")).toBe("0.5");
    expect(document.querySelector(".media-thumbhash")?.getAttribute("src")).toMatch(/^data:image\/png;base64,/);
  });

  it("renders file, link, location, audio and video cards", async () => {
    stubFetch(() => ({ body: {} }));
    render(() => (
      <Attachments
        items={[
          { kind: "pdf", name: "report.pdf", mediaId: "p1", bytes: 2048 },
          { kind: "link", name: "Example page", url: "https://example.com/page" },
          { kind: "location", lat: 45.4642, lon: 9.19 },
          { kind: "audio", name: "memo.m4a", mediaId: "a1", durationMs: 65_000 },
          { kind: "video", name: "clip.mp4", mediaId: "v1", durationMs: 12_000 },
        ]}
      />
    ));
    expect(screen.getByRole("link", { name: "Download report.pdf" })).toHaveAttribute("href", "/v1/media/items/p1/original");
    expect(screen.getByText("2.0 KB", { exact: false })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Example page/ })).toHaveTextContent("example.com");
    expect(screen.getByRole("link", { name: "Open 45.4642, 9.19 in Google Maps" })).toHaveAttribute(
      "href",
      "https://www.google.com/maps/search/?api=1&query=45.4642%2C9.19",
    );
    expect(screen.getByRole("button", { name: "Play" })).toBeInTheDocument();
    expect(screen.getByText("1:05")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Play video clip.mp4" })).toHaveTextContent("0:12");
  });

  it("plays a video inline from the tokenised play URL", async () => {
    const calls = stubFetch((call) =>
      call.url === "/v1/media/items/v1/play" ? { body: { url: "/v1/media/s/tok/v1/file" } } : { body: {} },
    );
    render(() => <Attachments items={[{ kind: "video", name: "clip.mp4", mediaId: "v1" }]} />);
    fireEvent.click(screen.getByRole("button", { name: "Play video clip.mp4" }));
    await waitFor(() => expect(document.querySelector("video")).toHaveAttribute("src", "/v1/media/s/tok/v1/file"));
    const play = calls.find((c) => c.url === "/v1/media/items/v1/play")!;
    expect(play.method).toBe("POST");
    expect(play.headers["x-secretary-csrf"]).toBe("1");
  });
});
