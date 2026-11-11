import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Parcel } from "../api/parcels";
import { deferred, stubFetch, type Call } from "../test/fetch";
import { daily, delivered, parcel } from "../test/parcelFixtures";
import { renderAt } from "../test/router";
import ParcelsView from "./Parcels";

const show = (path = "/parcels") => renderAt(path, () => <Route path="/parcels/:id?" component={ParcelsView} />);

/** A tiny Lucina: the list, one parcel, refresh, label, delete, add. */
function serve(initial: Parcel[], extra?: (c: Call) => ReturnType<Parameters<typeof stubFetch>[0]> | undefined) {
  const store = [...initial];
  const calls = stubFetch((c) => {
    const custom = extra?.(c);
    if (custom) return custom;
    const m = /^\/v1\/parcels(?:\/([^/]+))?(\/refresh)?$/.exec(c.url);
    if (!m) return { status: 404 };
    const id = m[1] && decodeURIComponent(m[1]);
    const found = store.find((p) => p.id === id);
    if (!id) {
      if (c.method === "GET") return { body: { parcels: store, digestHour: 9 } };
      const body = c.body as { number: string; carrier?: string; label?: string };
      const created = parcel({ id: "pc-new00000", number: body.number, label: body.label ?? null, carrier: body.carrier ?? "postnord" });
      store.unshift(created);
      return { body: { parcel: created } };
    }
    if (!found) return { status: 404, body: { error: { code: "not_found", message: "No such parcel." } } };
    if (c.method === "DELETE") {
      store.splice(store.indexOf(found), 1);
      return { body: { ok: true } };
    }
    if (c.method === "PATCH") {
      found.label = (c.body as { label: string | null }).label;
      return { body: { parcel: found } };
    }
    if (m[2]) {
      found.lastChecked = new Date().toISOString();
      found.status = { ...(found.status ?? parcel().status!), stage: "delivered", text: "Delivered" };
    }
    return { body: { parcel: found } };
  });
  return { calls, store };
}

describe("Parcels", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("lists active parcels, with a collapsed delivered section", async () => {
    serve([delivered(), parcel(), daily()]);
    const { container } = show();
    const rows = await screen.findAllByRole("listitem");
    const active = within(screen.getByRole("region", { name: "Tracked parcels" })).getAllByRole("listitem");
    expect(active).toHaveLength(2);
    expect(rows.length).toBe(3);
    expect(screen.getByText("2 active parcels")).toBeInTheDocument();

    // Newest change first: the parcel added just now (no status yet) before the one last updated in September.
    const first = active[1]!;
    expect(within(first).getByText("Headphones")).toBeInTheDocument();
    expect(within(first).getByText("PostNord · 00357059930440074440")).toBeInTheDocument();
    expect(within(first).getByText("Out for delivery")).toHaveAttribute("data-stage", "out_for_delivery");
    expect(within(first).getByText("The delivery of the shipment item is in progress.")).toBeInTheDocument();
    expect(within(first).getByText(/AARHUS DEPOT · /)).toBeInTheDocument();
    expect(within(first).getByText("Hourly · near you")).toBeInTheDocument();
    expect(within(first).getByText(/^next check in/)).toBeInTheDocument();

    const second = active[0]!;
    expect(within(second).getByText("RR123456789DK")).toBeInTheDocument();
    expect(within(second).getByText("Daily")).toBeInTheDocument();
    expect(within(second).getByText("the carrier has no record of it yet")).toBeInTheDocument();

    const details = container.querySelector("details.parcel-done") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    expect(within(details).getByText("Delivered and stopped (1)")).toBeInTheDocument();
    expect(within(details).getByText("DHL JD014600003SE")).toBeInTheDocument();
  });

  it("explains the empty state", async () => {
    serve([]);
    show();
    expect(await screen.findByText("No parcels yet")).toBeInTheDocument();
    expect(screen.getByText(/No parcel is tracked yet\. Add a number here, or tell Lucina “track this <number>”\./)).toBeInTheDocument();
  });

  it("adds a parcel and opens it", async () => {
    const { calls } = serve([]);
    show();
    await screen.findByText("No parcels yet");
    fireEvent.click(screen.getAllByRole("button", { name: "Add parcel" })[0]!);
    const submit = await screen.findByRole("button", { name: "Track" });
    expect(submit).toBeDisabled();
    fireEvent.input(screen.getByLabelText("Tracking number"), { target: { value: " 00357059930440074440 " } });
    fireEvent.input(screen.getByLabelText("Label (optional)"), { target: { value: "Shoes" } });
    fireEvent.click(submit);
    expect(await screen.findByRole("heading", { level: 2, name: "Shoes" })).toBeInTheDocument();
    const post = calls.find((c) => c.method === "POST")!;
    expect(post.body).toEqual({ number: "00357059930440074440", label: "Shoes" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("shows the server's message on a 400 and keeps the form", async () => {
    serve([], (c) => (c.method === "POST" ? { status: 400, body: { error: { code: "bad_request", message: "The carrier says that number is invalid." } } } : undefined));
    show();
    await screen.findByText("No parcels yet");
    fireEvent.click(screen.getAllByRole("button", { name: "Add parcel" })[0]!);
    fireEvent.input(await screen.findByLabelText("Tracking number"), { target: { value: "nonsense" } });
    fireEvent.input(screen.getByLabelText("Carrier (optional)"), { target: { value: "gls" } });
    fireEvent.click(screen.getByRole("button", { name: "Track" }));
    expect(await screen.findByText("The carrier says that number is invalid.")).toBeInTheDocument();
    expect(screen.getByLabelText("Tracking number")).toHaveValue("nonsense");
  });

  it("shows the timeline newest first with the details", async () => {
    serve([parcel(), delivered()]);
    show("/parcels/pc-1a2b3c4d");
    const detail = await screen.findByRole("region", { name: "Headphones" });
    expect(within(detail).getByText("PostNord")).toBeInTheDocument();
    expect(within(detail).getByText("00357059930440074440")).toBeInTheDocument();
    expect(within(detail).getByText("DK")).toBeInTheDocument();
    expect(within(detail).getByText(/^Sep 28, 2026$/)).toBeInTheDocument();
    expect(within(detail).getByText("Hourly · near you")).toBeInTheDocument();
    const events = within(within(detail).getByRole("list")).getAllByRole("listitem");
    expect(events.map((e) => e.querySelector(".parcel-event-text")?.textContent)).toEqual([
      "Out for delivery",
      "Arrived at the terminal",
      "The item has been registered",
    ]);
    expect(events[0]!.querySelector(".parcel-event-where")).toHaveTextContent("AARHUS DEPOT");
    expect(events[0]!.dataset.stage).toBe("out_for_delivery");
  });

  it("copies the number", async () => {
    serve([parcel()]);
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    show("/parcels/pc-1a2b3c4d");
    fireEvent.click(await screen.findByRole("button", { name: "Copy number" }));
    expect(writeText).toHaveBeenCalledWith("00357059930440074440");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("renames the parcel", async () => {
    const { calls } = serve([parcel()]);
    show("/parcels/pc-1a2b3c4d");
    fireEvent.click(await screen.findByRole("button", { name: "Edit label" }));
    const input = screen.getByLabelText("Label");
    expect(input).toHaveValue("Headphones");
    fireEvent.input(input, { target: { value: "Cans" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("heading", { level: 2, name: "Cans" })).toBeInTheDocument();
    expect(calls.find((c) => c.method === "PATCH")?.body).toEqual({ label: "Cans" });
  });

  it("checks now, disabled while it runs", async () => {
    const gate = deferred<void>();
    const { calls } = serve([parcel()], (c) => (c.url.endsWith("/refresh") ? undefined : undefined));
    const base = (globalThis.fetch as typeof fetch);
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input).endsWith("/refresh")) await gate.promise;
      return base(input, init);
    }));
    show("/parcels/pc-1a2b3c4d");
    fireEvent.click(await screen.findByRole("button", { name: "Check now" }));
    const busy = await screen.findByRole("button", { name: "Checking…" });
    expect(busy).toBeDisabled();
    gate.resolve();
    expect(await screen.findByRole("button", { name: "Check now" })).toBeEnabled();
    expect(calls.filter((c) => c.url.endsWith("/refresh"))).toHaveLength(1);
    expect(screen.getAllByText("Delivered").length).toBeGreaterThan(0);
  });

  it("stops tracking after the usual confirm and goes back to the list", async () => {
    const { calls, store } = serve([parcel(), daily()]);
    const confirmSpy = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmSpy);
    show("/parcels/pc-1a2b3c4d");
    fireEvent.click(await screen.findByRole("button", { name: "Stop tracking" }));
    expect(confirmSpy).toHaveBeenCalledWith(expect.stringContaining("Headphones"));
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    confirmSpy.mockReturnValue(true);
    fireEvent.click(screen.getByRole("button", { name: "Stop tracking" }));
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE" && c.url === "/v1/parcels/pc-1a2b3c4d")).toBe(true));
    expect(store.map((p) => p.id)).toEqual(["pc-22222222"]);
    expect(await screen.findByRole("region", { name: "Tracked parcels" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Headphones" })).toBeNull();
  });

  it("says a parcel is gone on a 404", async () => {
    serve([parcel()]);
    show("/parcels/pc-missing");
    expect(await screen.findByText("This parcel is not tracked any more.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "All parcels" })).toHaveAttribute("href", "/parcels");
  });

  it("explains a 503 and retries", async () => {
    let down = true;
    serve([parcel()], (c) => (down && c.method === "GET" ? { status: 503, body: { error: { code: "parcels_unavailable", message: "x" } } } : undefined));
    show();
    expect(await screen.findByText("Parcel tracking is not reachable right now.")).toBeInTheDocument();
    down = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Headphones")).toBeInTheDocument();
    expect(screen.queryByText("Parcel tracking is not reachable right now.")).toBeNull();
  });

  it("shows the network error when offline", async () => {
    stubFetch(() => {
      throw new TypeError("offline");
    });
    show();
    expect(await screen.findByText("Can't reach the server. Check your connection.")).toBeInTheDocument();
  });

  it("asks again when the page is shown again", async () => {
    const { calls } = serve([parcel()]);
    show();
    await screen.findByText("Headphones");
    const before = calls.filter((c) => c.url === "/v1/parcels").length;
    fireEvent(window, new Event("focus"));
    await waitFor(() => expect(calls.filter((c) => c.url === "/v1/parcels").length).toBe(before + 1));
  });
});
