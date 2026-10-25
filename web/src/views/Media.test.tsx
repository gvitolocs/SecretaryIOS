import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { clientProfile, type SubtitleTrack } from "../api/player";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import MediaView, { trackDetail, trackName } from "./Media";

// The real engine from the committed module (the page fetches it; jsdom cannot).
vi.mock("../lib/playerCore", async (original) => {
  const mod = await original<typeof import("../lib/playerCore")>();
  const { readFileSync } = await import("node:fs");
  const { join } = await import("node:path");
  const core = mod.coreFromBytes(readFileSync(join(process.cwd(), "src/lib/player-core.wasm")));
  return { ...mod, playerCore: () => core };
});

const ITEMS = [
  { id: "itm_a", kind: "video", title: "The Sword in the Stone", album: "Film", duration_ms: 4_577_960, poster_url: "/v1/media/s/t/itm_a/poster.jpg" },
  { id: "itm_b", kind: "video", title: "Up", album: "Dansk film", duration_ms: 5_937_736 },
];

describe("Media", () => {
  afterEach(() => vi.restoreAllMocks());

  it("lists the library by album with durations", async () => {
    const calls = stubFetch((call) => {
      if (call.url.startsWith("/v1/media/items")) return { body: { ok: true, items: ITEMS, next_cursor: null } };
      if (call.url.startsWith("/v1/media/progress")) return { body: { ok: true, entries: [], acks: [] } };
      return { status: 404, body: {} };
    });
    renderAt("/media", () => <Route path="/media" component={MediaView} />);
    expect(await screen.findByRole("button", { name: /The Sword in the Stone/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Film" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Dansk film" })).toBeInTheDocument();
    expect(screen.getByText("1:16:17")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/media/items?kind=video&limit=200")).toBe(true);
    fireEvent.click(screen.getByRole("tab", { name: "Audio" }));
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/media/items?kind=audio&limit=200")).toBe(true));
  });

  it("names subtitle tracks by language and source", () => {
    const track = (over: Partial<SubtitleTrack>): SubtitleTrack => ({
      id: "x0", lang: "da", label: "whisper-da", forced: false, kind: "text", format: "srt", source: "file", url: "/s", ...over,
    });
    expect(trackName(track({}), "en")).toBe("Danish · whisper-da");
    expect(trackName(track({ lang: "dan", label: "Dansk" }), "da")).toBe("Dansk");
    expect(trackName(track({ lang: null, label: "Subs/English" }), "en")).toBe("Subs/English");
    expect(trackDetail(track({ kind: "image", source: "embedded", forced: true }))).toBe("in the video · picture, read by OCR · forced");
  });

  it("tells the server which browser plays", () => {
    expect(clientProfile("Mozilla/5.0 (Macintosh) AppleWebKit/605 Version/19.0 Safari/605.1.15")).toBe("web_safari");
    expect(clientProfile("Mozilla/5.0 (X11) AppleWebKit/537.36 Chrome/150.0 Safari/537.36")).toBe("web_chrome");
    expect(clientProfile("Mozilla/5.0 (X11; rv:150.0) Gecko/20100101 Firefox/150.0")).toBe("web_firefox");
  });
});
