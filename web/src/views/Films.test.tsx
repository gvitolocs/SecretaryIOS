import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { playQueue } from "../state/media";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import FilmsView, { splitTitle } from "./Films";

// The real engine from the committed module (the page fetches it; jsdom cannot).
vi.mock("../lib/playerCore", async (original) => {
  const mod = await original<typeof import("../lib/playerCore")>();
  const { readFileSync } = await import("node:fs");
  const { join } = await import("node:path");
  const core = mod.coreFromBytes(readFileSync(join(process.cwd(), "src/lib/player-core.wasm")));
  return { ...mod, playerCore: () => core };
});

// Playback itself is the player's business (state/media.ts); here only what the page asks for.
vi.mock("../state/media", async (original) => {
  const mod = await original<typeof import("../state/media")>();
  return { ...mod, playQueue: vi.fn(async () => undefined) };
});

const ALBUMS = [
  { id: "alb_film", title: "Film", system_key: null, cover_item: null, count: 1 },
  { id: "alb_dansk", title: "Dansk film", system_key: null, cover_item: "itm_up", count: 2 },
  { id: "alb_recent", title: "Recents", system_key: "recent", cover_item: null, count: 40 },
  { id: "alb_empty", title: "Empty", system_key: null, cover_item: null, count: 0 },
];

const FILMS: Record<string, unknown[]> = {
  alb_film: [{ id: "itm_sword", kind: "video", title: "The Sword in the Stone", duration_ms: 4_577_960, poster_url: "/v1/media/s/t/itm_sword/poster.jpg" }],
  alb_dansk: [
    { id: "itm_up", kind: "video", title: "Up - Dansk dub + Dansk subs", duration_ms: 5_400_000 },
    { id: "itm_cars", kind: "video", title: "Biler - Dansk dub", duration_ms: 7_020_000 },
  ],
};

function serve(progress: unknown[] = []) {
  return stubFetch((call) => {
    if (call.url === "/v1/media/albums?scope=films") return { body: { ok: true, albums: ALBUMS } };
    if (call.url.startsWith("/v1/media/items")) {
      const album = new URLSearchParams(call.url.split("?")[1]).get("album") ?? "";
      return { body: { ok: true, items: FILMS[album] ?? [], next_cursor: null } };
    }
    if (call.url.startsWith("/v1/media/progress")) return { body: { ok: true, entries: progress, acks: [] } };
    return { status: 404, body: {} };
  });
}

function renderFilms() {
  renderAt("/films", () => <Route path="/films" component={FilmsView} />);
}

describe("Films", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.mocked(playQueue).mockClear();
  });

  it("shows the film folders, biggest first, with their films", async () => {
    const calls = serve();
    renderFilms();
    expect(await screen.findByRole("button", { name: /^The Sword in the Stone/ })).toBeInTheDocument();
    const headings = screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent);
    expect(headings).toEqual(["Dansk film2", "Film1"]);
    const dansk = screen.getByRole("region", { name: /^Dansk film/ });
    expect(within(dansk).getAllByRole("button").map((b) => b.textContent)).toEqual([
      "Biler1:57:00 · Dansk dub",
      "Up1:30:00 · Dansk dub + Dansk subs",
    ]);
    expect(calls.some((c) => c.url === "/v1/media/items?scope=films&kind=video&album=alb_dansk&limit=200")).toBe(true);
    expect(calls.some((c) => c.url.includes("album=alb_recent") || c.url.includes("album=alb_empty"))).toBe(false);
    expect(screen.queryByRole("heading", { name: "Continue watching" })).toBeNull();
  });

  it("puts a started film in Continue watching with the time left", async () => {
    serve([{ id: "itm_up", position_ms: 1_800_000, duration_ms: 5_400_000, finished: false, updated_at: 1_000 }]);
    renderFilms();
    const row = await screen.findByRole("region", { name: "Continue watching" });
    expect(within(row).getByRole("button", { name: /^Up1:00:00 left · Dansk dub \+ Dansk subs/ })).toBeInTheDocument();
    expect(within(row).getByRole("button", { name: "Play from the beginning: Up - Dansk dub + Dansk subs" })).toBeInTheDocument();
  });

  it("filters by title", async () => {
    serve();
    renderFilms();
    await screen.findByRole("region", { name: /^Dansk film/ });
    fireEvent.input(screen.getByRole("searchbox", { name: "Search films" }), { target: { value: "sword" } });
    await waitFor(() => expect(screen.queryByRole("region", { name: /^Dansk film/ })).toBeNull());
    expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual(["The Sword in the Stone1:16:17"]);
  });

  it("plays only the clicked film, or from the beginning", async () => {
    serve([{ id: "itm_up", position_ms: 1_800_000, duration_ms: 5_400_000, finished: false, updated_at: 1_000 }]);
    renderFilms();
    const dansk = await screen.findByRole("region", { name: /^Dansk film/ });
    fireEvent.click(within(dansk).getByRole("button", { name: /^Biler/ }));
    expect(playQueue).toHaveBeenLastCalledWith([expect.objectContaining({ id: "itm_cars" })], 0, { fromStart: false });
    expect(vi.mocked(playQueue).mock.calls[0]![0]).toHaveLength(1);
    fireEvent.click(await within(dansk).findByRole("button", { name: /^Play from the beginning: Up/ }));
    expect(playQueue).toHaveBeenLastCalledWith([expect.objectContaining({ id: "itm_up" })], 0, { fromStart: true });
  });

  it("shows an empty state when no folder has films", async () => {
    stubFetch((call) => {
      if (call.url.startsWith("/v1/media/albums")) return { body: { ok: true, albums: [] } };
      if (call.url.startsWith("/v1/media/progress")) return { body: { ok: true, entries: [], acks: [] } };
      return { status: 404, body: {} };
    });
    renderFilms();
    expect(await screen.findByText("No films yet")).toBeInTheDocument();
  });

  it("splits the edition off the title", () => {
    expect(splitTitle("Up - Dansk dub + Dansk subs")).toEqual({ name: "Up", edition: "Dansk dub + Dansk subs" });
    expect(splitTitle("Film")).toEqual({ name: "Film", edition: null });
  });
});
