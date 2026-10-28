import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { type Call, stubFetch } from "../test/fetch";
import { rawItem } from "../test/galleryGrid";
import { renderAt } from "../test/router";
import GalleryDuplicatesView from "./GalleryDuplicates";

vi.mock("../lib/jxl", () => ({ supportsJxl: () => Promise.resolve(false) }));
// The viewer has its own tests; here it is a stand-in that shows what the page opens it with.
vi.mock("../components/gallery/GalleryViewer", () => ({
  GalleryViewer: (p: { items: { id: string }[]; index: number; onClose: () => void }) => (
    <div data-testid="viewer" data-index={p.index}>
      {p.items.map((i) => i.id).join(",")}
      <button type="button" onClick={p.onClose}>
        close viewer
      </button>
    </div>
  ),
}));

const member = (id: string, size: number, metadata: number, keep = false) => ({ ...rawItem(id, { size, title: `${id}.jpg` }), review: { metadata, keep } });

const group = (id: string, ids: [string, string], extra: Record<string, unknown> = {}) => ({
  id,
  class: "same_photo_reencoded",
  score: 0.93,
  applicable: true,
  keep: ids[0],
  reason: "largest_file",
  members: [member(ids[0], 3_000_000, 6, true), member(ids[1], 900_000, 2)],
  ...extra,
});

const suggestion = (id: string, ids: string[]) => ({
  id,
  kind: "burst",
  evidence: "time_and_look",
  primary: ids[0],
  span_ms: 900,
  items: ids.map((i) => rawItem(i)),
});

interface Setup {
  groups?: unknown[];
  pending?: number;
  failFirst?: boolean;
  bursts?: unknown[];
  total?: number;
  refuseStack?: boolean;
}

interface Backend {
  calls: Call[];
  posts: { url: string; body: unknown }[];
}

function backend(setup: Setup = {}): Backend {
  const b: Backend = { calls: [], posts: [] };
  let first = true;
  const groups = setup.groups ?? [group("dg_1", ["a1", "a2"]), group("dg_2", ["b1", "b2"], { score: 0.62, applicable: false })];
  b.calls = stubFetch((call) => {
    const url = new URL(call.url, "http://x");
    if (call.method === "POST") b.posts.push({ url: url.pathname, body: call.body });
    if (url.pathname === "/v1/media/duplicates" && call.method === "GET") {
      if (setup.failFirst && first) {
        first = false;
        return { status: 500, body: { error: { code: "boom", message: "boom" } } };
      }
      return {
        body: {
          ok: true,
          status: "open",
          hashes: { photos: 100, hashed: 100 - (setup.pending ?? 0), pending: setup.pending ?? 0 },
          total: setup.total ?? groups.length,
          applicable: groups.filter((g) => (g as { applicable: boolean }).applicable).length,
          min_score: 0.7,
          reclaimable_bytes: 2_100_000,
          stack_suggestions: setup.bursts?.length ?? 0,
          groups,
        },
      };
    }
    if (url.pathname === "/v1/media/duplicates/resolve") {
      const body = call.body as { keep: string[]; drop: string[] };
      return { body: { ok: true, merged: body.drop.map((id) => ({ id, into: body.keep[0] })), skipped: [], metadata: { captions: 0, location: false, taken_at: false } } };
    }
    if (url.pathname === "/v1/media/duplicates/resolve-all") {
      return { body: { ok: true, groups: 1, merged: 1, merged_ids: [{ id: "a2", into: "a1" }], skipped: {} } };
    }
    if (url.pathname === "/v1/media/duplicates/undo") return { body: { ok: true, undone: (call.body as { ids: string[] }).ids, not_merged: [] } };
    if (url.pathname === "/v1/media/duplicates/dismiss") return { body: { ok: true, pairs: 1 } };
    if (url.pathname === "/v1/media/stacks/suggestions" && call.method === "GET") {
      return { body: { ok: true, hashes_pending: 0, total: setup.bursts?.length ?? 0, suggestions: setup.bursts ?? [] } };
    }
    if (url.pathname === "/v1/media/stacks/suggestions/dismiss") return { body: { ok: true } };
    if (url.pathname === "/v1/media/stacks" && call.method === "POST") {
      if (setup.refuseStack) return { status: 409, body: { ok: false, error: "x: already_stacked", failed: [] } };
      const ids = (call.body as { ids: string[] }).ids;
      return { status: 201, body: { ok: true, stack: { id: "stk_1", kind: "burst", primary: ids[0], count: ids.length, created_at: "", items: ids.map((i) => rawItem(i)) } } };
    }
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
  return b;
}

function mount() {
  return renderAt("/gallery/duplicates", () => (
    <>
      <Route path="/gallery/duplicates" component={GalleryDuplicatesView} />
      <Route path="/gallery" component={() => <p>gallery page</p>} />
    </>
  ));
}

const groupCard = (n: number) => screen.getAllByRole("article")[n]!;

beforeEach(() => setLocale("en"));
afterEach(() => vi.unstubAllGlobals());

describe("the duplicate review", () => {
  it("shows each group side by side, the suggested photo marked with the reason, and a weak group flagged", async () => {
    backend();
    mount();
    expect(await screen.findByRole("heading", { name: "Duplicates" })).toBeInTheDocument();
    expect(await screen.findByText("2 groups to review")).toBeInTheDocument();
    const first = within(groupCard(0));
    expect(first.getByText("Same photo, saved again")).toBeInTheDocument();
    expect(first.getByText("93% sure")).toBeInTheDocument();
    expect(first.getByText("Suggested")).toBeInTheDocument();
    expect(first.getByText("Largest file")).toBeInTheDocument();
    expect(first.getAllByText(/ details?$/).map((e) => e.textContent)).toEqual(["6 details", "2 details"]);
    expect(first.queryByText(/Not sure enough/)).toBeNull();
    expect(within(groupCard(1)).getByText(/Not sure enough to apply automatically/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Gallery" })).toHaveAttribute("href", "/gallery");
  });

  it("keeps the chosen photo and hides the others, then offers Undo", async () => {
    const b = backend();
    mount();
    await screen.findByText("2 groups to review");
    fireEvent.click(within(groupCard(0)).getByRole("button", { name: "Keep a1.jpg and hide the others" }));
    await waitFor(() => expect(b.posts.at(-1)).toEqual({ url: "/v1/media/duplicates/resolve", body: { keep: ["a1"], drop: ["a2"] } }));
    expect(await screen.findByText("Hid 1 copy under the photo you kept.")).toBeInTheDocument();
    expect(screen.getAllByRole("article")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(b.posts.some((p) => p.url === "/v1/media/duplicates/undo" && JSON.stringify(p.body) === JSON.stringify({ ids: ["a2"] }))).toBe(true));
    expect(await screen.findByText("Copies restored.")).toBeInTheDocument();
  });

  it("keeps the other copy when asked to, hiding the suggested one", async () => {
    const b = backend();
    mount();
    await screen.findByText("2 groups to review");
    fireEvent.click(within(groupCard(0)).getByRole("button", { name: "Keep a2.jpg and hide the others" }));
    await waitFor(() => expect(b.posts.at(-1)!.body).toEqual({ keep: ["a2"], drop: ["a1"] }));
  });

  it("marks a group as not duplicates, remembers it, and can take that back", async () => {
    const b = backend();
    mount();
    await screen.findByText("2 groups to review");
    fireEvent.click(within(groupCard(1)).getByRole("button", { name: "Not duplicates" }));
    await waitFor(() => expect(b.posts.at(-1)).toEqual({ url: "/v1/media/duplicates/dismiss", body: { ids: ["b1", "b2"] } }));
    expect(await screen.findByText("Marked as not duplicates.")).toBeInTheDocument();
    expect(screen.getAllByRole("article")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(b.posts.at(-1)).toEqual({ url: "/v1/media/duplicates/dismiss", body: { ids: ["b1", "b2"], undo: true } }));
  });

  it("asks before applying all suggestions, says how many groups and how much, and applies on confirm", async () => {
    const b = backend();
    mount();
    await screen.findByText("2 groups to review");
    fireEvent.click(screen.getByRole("button", { name: "Apply all suggestions" }));
    const dialog = screen.getByRole("dialog", { name: "Apply all suggestions?" });
    expect(dialog).toHaveTextContent("1 sure group keeps the suggested photo");
    expect(dialog).toHaveTextContent("Nothing is deleted, and you can undo it.");
    expect(b.posts.some((p) => p.url.endsWith("resolve-all"))).toBe(false);
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(b.posts.some((p) => p.url.endsWith("resolve-all"))).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Apply all suggestions" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Apply" }));
    await waitFor(() => expect(b.posts.some((p) => p.url === "/v1/media/duplicates/resolve-all")).toBe(true));
    expect(await screen.findByText("Hid 1 copy under the photo you kept.")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("has no Apply all when no group is sure enough", async () => {
    backend({ groups: [group("dg_2", ["b1", "b2"], { score: 0.62, applicable: false })] });
    mount();
    await screen.findByText("1 group to review");
    expect(screen.queryByRole("button", { name: "Apply all suggestions" })).toBeNull();
  });

  it("says so when there are no duplicates", async () => {
    backend({ groups: [] });
    mount();
    expect(await screen.findByText("No duplicates found.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Apply all suggestions" })).toBeNull();
  });

  it("says how many photos are still being analysed, instead of claiming there are no duplicates", async () => {
    backend({ groups: [], pending: 120 });
    mount();
    expect(await screen.findByText(/120 photos are still being analysed/)).toBeInTheDocument();
    expect(screen.queryByText("No duplicates found.")).toBeNull();
  });

  it("offers a retry when the groups can't be loaded", async () => {
    backend({ failFirst: true });
    mount();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the duplicates.");
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("2 groups to review")).toBeInTheDocument();
  });

  it("opens the group's photos in the viewer at the one clicked", async () => {
    backend();
    mount();
    await screen.findByText("2 groups to review");
    fireEvent.click(within(groupCard(0)).getByRole("button", { name: "Open a2.jpg" }));
    const viewer = await screen.findByTestId("viewer");
    expect(viewer).toHaveTextContent("a1,a2");
    expect(viewer).toHaveAttribute("data-index", "1");
    fireEvent.click(screen.getByRole("button", { name: "close viewer" }));
    expect(screen.queryByTestId("viewer")).toBeNull();
  });

  it("offers more groups when the server has more than one page", async () => {
    const b = backend({ total: 120 });
    mount();
    await screen.findByText("120 groups to review");
    fireEvent.click(screen.getByRole("button", { name: "Show more" }));
    await waitFor(() => expect(b.calls.filter((c) => c.url.includes("/v1/media/duplicates?")).at(-1)!.url).toContain("limit=100"));
  });
});

describe("bursts that could be stacked", () => {
  const bursts = () => [suggestion("sg_1", ["x1", "x2", "x3"]), suggestion("sg_2", ["y1", "y2"])];

  it("shows the banner with the count only when there are suggestions", async () => {
    backend();
    mount();
    await screen.findByText("2 groups to review");
    expect(screen.queryByText(/could be stacked/)).toBeNull();
  });

  it("lists the suggestions on Review and stacks one only when told to", async () => {
    const b = backend({ bursts: bursts() });
    mount();
    expect(await screen.findByText("2 bursts could be stacked")).toBeInTheDocument();
    expect(b.posts.some((p) => p.url === "/v1/media/stacks")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    const list = await screen.findByRole("region", { name: "Bursts" });
    expect(within(list).getAllByRole("button", { name: "Stack" })).toHaveLength(2);
    expect(within(list).getAllByText("Found by capture time and look")).toHaveLength(2);
    fireEvent.click(within(list).getAllByRole("button", { name: "Stack" })[0]!);
    await waitFor(() => expect(b.posts.at(-1)).toEqual({ url: "/v1/media/stacks", body: { ids: ["x1", "x2", "x3"], primary: "x1", kind: "burst" } }));
    expect(await screen.findByText("Stacked 3 photos.")).toBeInTheDocument();
    expect(await screen.findByText("1 burst could be stacked")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Bursts" })).getAllByRole("button", { name: "Stack" })).toHaveLength(1);
  });

  it("stops proposing a set the owner says is not a stack", async () => {
    const b = backend({ bursts: bursts() });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Review" }));
    const list = await screen.findByRole("region", { name: "Bursts" });
    fireEvent.click(within(list).getAllByRole("button", { name: "Not a stack" })[1]!);
    await waitFor(() => expect(b.posts.at(-1)).toEqual({ url: "/v1/media/stacks/suggestions/dismiss", body: { ids: ["y1", "y2"] } }));
    await waitFor(() => expect(within(screen.getByRole("region", { name: "Bursts" })).getAllByRole("button", { name: "Stack" })).toHaveLength(1));
  });

  it("drops a stale suggestion when the server refuses it", async () => {
    backend({ bursts: bursts(), refuseStack: true });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Review" }));
    const list = await screen.findByRole("region", { name: "Bursts" });
    fireEvent.click(within(list).getAllByRole("button", { name: "Stack" })[0]!);
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't stack those photos");
    await waitFor(() => expect(within(screen.getByRole("region", { name: "Bursts" })).getAllByRole("button", { name: "Stack" })).toHaveLength(1));
  });

  it("hides the list again", async () => {
    backend({ bursts: bursts() });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Review" }));
    await screen.findByRole("region", { name: "Bursts" });
    fireEvent.click(screen.getByRole("button", { name: "Hide" }));
    expect(screen.queryByRole("region", { name: "Bursts" })).toBeNull();
  });
});
