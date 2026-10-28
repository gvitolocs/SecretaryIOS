import { Route, useLocation } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { prefill } from "../state/chat";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { ago, overview } from "../test/serverFixtures";
import ServerHealthView, { REFRESH_MS } from "./ServerHealth";

function Where() {
  const loc = useLocation();
  return <p data-testid="where">{loc.pathname + loc.search}</p>;
}

const show = (path = "/server-health") =>
  renderAt(path, () => (
    <>
      <Route path="/server-health/:section?" component={() => (<><ServerHealthView /><Where /></>)} />
      <Route path="/" component={Where} />
      <Route path="/agents/map/:id" component={Where} />
    </>
  ));

const where = () => screen.getByTestId("where").textContent;

describe("Server page", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("shows the status, hosts, what needs attention, healthy checks and recent events", async () => {
    const calls = stubFetch(() => ({ body: overview() }));
    show();
    const nezopt = (await screen.findByText("nezopt", { selector: ".sh-host-name" })).closest("a")!;
    expect(calls[0]!.url).toMatch(/\/v1\/system\/overview$/);

    // Header: 2 items + the unreachable host (nezopt's host component is not an item here, so it counts too).
    expect(screen.getByText("4 need attention")).toBeInTheDocument();
    expect(screen.getByText(/· updated/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Overview" })).toHaveAttribute("aria-current", "page");

    // Host cards.
    expect(nezopt).toHaveAttribute("href", "/server-health?host=nezopt");
    expect(nezopt).toHaveAttribute("data-state", "warn");
    expect(within(nezopt).getByText("home server")).toBeInTheDocument();
    expect(within(nezopt).getByText("Warning")).toBeInTheDocument();
    expect(within(nezopt).getByText(/^up 23d 4h$/)).toBeInTheDocument();
    expect(within(nezopt).getByRole("img", { name: /^CPU temperature, last 24 hours: now 95 °C, low 61, high 96$/ })).toBeInTheDocument();
    expect(within(nezopt).getByRole("img", { name: /^Load \(1 min\), last 24 hours: now 9\.58/ })).toBeInTheDocument();
    expect(within(nezopt).getByRole("img", { name: /^Memory, last 24 hours: now 33%/ })).toBeInTheDocument();
    expect(within(nezopt).getByLabelText("/data: 93% used, 70 GB free")).toHaveAttribute("data-level", "crit");
    expect(within(nezopt).getByLabelText("/: 78% used, 422 GB free")).toHaveAttribute("data-level", "ok");

    const nespc = screen.getByText("nespc", { selector: ".sh-host-name" }).closest("a")!;
    expect(within(nespc).getByText("Reachable · limited data")).toBeInTheDocument();
    expect(within(nespc).queryByRole("img")).toBeNull();
    const pi = screen.getByText("pi-home", { selector: ".sh-host-name" }).closest("a")!;
    expect(within(pi).getByText(/^Unreachable since /)).toBeInTheDocument();

    // Needs attention.
    const list = screen.getByRole("list", { name: "Needs attention" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]!).getByRole("link", { name: "gmaps-server" })).toHaveAttribute("href", "/server-health?item=catalog%3Agmaps-server");
    expect(within(rows[0]!).getByText("Critical")).toBeInTheDocument();
    expect(within(rows[0]!).getByText("Not running; port 18210 not listening")).toBeInTheDocument();
    expect(within(rows[0]!).getByText("breaks 1")).toBeInTheDocument();
    expect(within(rows[0]!).getByText(/^for 2 hr/)).toBeInTheDocument();
    expect(within(rows[1]!).getByText("homelab")).toBeInTheDocument();

    // Healthy checks, collapsed.
    const healthy = screen.getByText("All good · 122 checks").closest("details")!;
    expect(healthy.open).toBe(false);
    expect(within(healthy).getAllByRole("heading", { level: 3 }).map((h) => h.textContent)).toEqual(["Lucina", "Pokoin"]);

    // Recent: the latest events of any kind.
    const recent = within(screen.getByRole("list", { name: "Recent" })).getAllByRole("listitem");
    expect(recent.map((li) => li.getAttribute("data-kind"))).toEqual(["change", "deploy", "incident", "decision"]);
  });

  it("opens an attention item from ?item= with its impact, runbook and actions", async () => {
    stubFetch(() => ({ body: overview() }));
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    show("/server-health?item=catalog%3Agmaps-server");
    const dialog = await screen.findByRole("dialog", { name: "gmaps-server" });
    expect(within(dialog).getByText("port 18210: not listening")).toBeInTheDocument();
    expect(within(dialog).getByText(/142 failures in 24 h/)).toBeInTheDocument();
    expect(within(dialog).getByText("maps.example.com")).toBeInTheDocument();
    const broken = within(dialog).getAllByRole("link", { name: "Lucina Maps" });
    expect(broken[0]).toHaveAttribute("href", "/server-health/services?component=lucina-maps");
    expect(within(dialog).getByText("Also down because of this")).toBeInTheDocument();
    expect(within(dialog).getByRole("link", { name: "Open in map" })).toHaveAttribute("href", "/agents/map/gmaps-server");

    // The runbook is markdown with HTML escaped.
    expect(within(dialog).getByRole("heading", { name: "Fix" })).toBeInTheDocument();
    expect(within(dialog).getByText(/<script>alert\(1\)<\/script>/)).toBeInTheDocument();
    expect(dialog.querySelector("script")).toBeNull();

    // Copy the logs command (the command, not logs).
    expect(within(dialog).getByText("journalctl --user -u gmaps-server -n 100 --no-pager")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Copy logs command" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("journalctl --user -u gmaps-server -n 100 --no-pager"));
    expect(await within(dialog).findByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Copy restart command" })).toBeInTheDocument();
    delete (navigator as unknown as Record<string, unknown>).clipboard;

    // Closing a sheet opened by the URL drops the parameter.
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(where()).toBe("/server-health");
  });

  it("Ask Lucina prefills the chat with the item's context", async () => {
    stubFetch(() => ({ body: overview() }));
    show("/server-health?item=catalog%3Agmaps-server");
    const dialog = await screen.findByRole("dialog", { name: "gmaps-server" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Ask Lucina" }));
    await waitFor(() => expect(where()).toBe("/"));
    const text = prefill()!.text;
    expect(text).toContain("Problem: gmaps-server");
    expect(text).toContain("Host: nezopt");
    expect(text).toContain("It breaks: Lucina Maps");
    expect(text).toContain("Logs: journalctl --user -u gmaps-server -n 100 --no-pager");
  });

  it("opens a row in the sheet and Escape goes back", async () => {
    stubFetch(() => ({ body: overview() }));
    show();
    fireEvent.click(await screen.findByRole("link", { name: "backup" }));
    const dialog = await screen.findByRole("dialog", { name: "backup" });
    expect(where()).toBe("/server-health?item=sentinel%3Anezopt%3Atimer%3Abackup");
    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(where()).toBe("/server-health");
  });

  it("opens a host's detail from ?host=", async () => {
    stubFetch(() => ({ body: overview() }));
    show("/server-health?host=nezopt");
    const dialog = await screen.findByRole("dialog", { name: "nezopt" });
    expect(within(dialog).getByRole("img", { name: /^CPU temperature, last 24 hours/ })).toBeInTheDocument();
    expect(within(dialog).getAllByText("90 °C").length).toBeGreaterThan(0);
    expect(within(dialog).getByText("Tctl 95 °C")).toBeInTheDocument();
    expect(within(dialog).getByLabelText("/data: 93% used, 70 GB free")).toBeInTheDocument();
    expect(within(dialog).getByRole("link", { name: /Secretary server/ })).toHaveAttribute("href", "/server-health?component=secretary-server");
    // gmaps-server is both an attention item on this host and one of its components: both open the item.
    const gmaps = within(dialog).getAllByRole("link", { name: /gmaps-server/ });
    expect(gmaps.map((a) => a.getAttribute("href"))).toEqual(["/server-health?item=catalog%3Agmaps-server", "/server-health?item=catalog%3Agmaps-server"]);
    expect(within(dialog).getByText("Needs attention on this host")).toBeInTheDocument();
  });

  it("shows partial sources and an empty attention list calmly", async () => {
    stubFetch(() => ({
      body: overview({
        overall: "ok",
        attention: [],
        attention_count: 0,
        hosts: overview().hosts.slice(1, 2),
        sources: { catalog: { ok: false, at: null, error: "boom" }, sentinel: { ok: true, at: ago(1), error: null }, ops: { ok: true, at: ago(1), error: null } },
      }),
    }));
    show();
    expect(await screen.findByText("Catalog unavailable — showing the last host reports")).toBeInTheDocument();
    expect(screen.getByText("All systems normal")).toBeInTheDocument();
    expect(screen.getByText("Nothing needs attention")).toBeInTheDocument();
    expect(screen.queryByRole("list", { name: "Needs attention" })).toBeNull();
  });

  it("filters services and opens a component", async () => {
    stubFetch(() => ({ body: overview() }));
    show("/server-health/services");
    const search = await screen.findByRole("searchbox", { name: "Search services" });
    expect(screen.getByRole("link", { name: "Services" })).toHaveAttribute("aria-current", "page");
    // Unhealthy groups are open, healthy ones closed.
    expect(screen.getByText("Lucina", { selector: ".sh-group-name" }).closest("details")!.open).toBe(true);
    expect(screen.getByText("Pokoin", { selector: ".sh-group-name" }).closest("details")!.open).toBe(false);

    fireEvent.keyDown(document.body, { key: "/" });
    expect(document.activeElement).toBe(search);

    fireEvent.input(search, { target: { value: "p1" } });
    expect(screen.queryByText("Lucina", { selector: ".sh-group-name" })).toBeNull();
    const pokoin = screen.getByRole("link", { name: /Pokoin API/ });
    expect(pokoin.closest("details")!.open).toBe(true);
    expect(within(pokoin).getByText("High")).toBeInTheDocument();

    fireEvent.input(search, { target: { value: "zzz" } });
    expect(screen.getByText("No service matches “zzz”.")).toBeInTheDocument();

    fireEvent.input(search, { target: { value: "maps" } });
    expect(screen.getByRole("link", { name: /gmaps-server/ })).toHaveAttribute("href", "/server-health/services?item=catalog%3Agmaps-server");
    fireEvent.click(screen.getByRole("link", { name: /Lucina Maps/ }));
    const dialog = await screen.findByRole("dialog", { name: "Lucina Maps" });
    expect(within(dialog).getByRole("link", { name: "Open in map" })).toHaveAttribute("href", "/agents/map/lucina-maps");
  });

  it("shows incidents and deploys by day", async () => {
    stubFetch(() => ({ body: overview() }));
    show("/server-health/incidents");
    const today = await screen.findByRole("region", { name: "Today" });
    expect(within(today).getAllByRole("listitem").map((li) => li.getAttribute("data-kind"))).toEqual(["change", "incident"]);
    expect(within(today).getByText("High")).toBeInTheDocument();
    expect(within(today).getByRole("img", { name: "Incident" })).toBeInTheDocument();
    expect(within(today).getByText("OK")).toBeInTheDocument();
    expect(screen.queryByText("secretary-server deployed")).toBeNull();

    fireEvent.click(screen.getByRole("link", { name: "Deploys" }));
    expect(await screen.findByText("secretary-server deployed")).toBeInTheDocument();
    expect(screen.getByText("Move backups to p1")).toBeInTheDocument();
    expect(screen.queryByText("gmaps-server went down")).toBeNull();
  });

  it("shows an error with retry, refreshes fresh on demand and every minute while visible", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let fail = true;
    const calls = stubFetch(() => (fail ? { status: 500, body: { error: { code: "internal", message: "boom" } } } : { body: overview() }));
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the server overview.");
    expect(screen.getByText("Can't reach the server")).toBeInTheDocument();
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("nezopt", { selector: ".sh-host-name" });
    expect(screen.queryByRole("alert")).toBeNull();

    const n = calls.length;
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(calls.length).toBe(n + 1));
    expect(calls[n]!.url).toMatch(/\?fresh=1$/);
    await vi.advanceTimersByTimeAsync(REFRESH_MS);
    await waitFor(() => expect(calls.length).toBe(n + 2));
    expect(calls[n + 1]!.url).toMatch(/overview$/);

    // A failed refresh keeps the data and says so quietly.
    fail = true;
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/^Couldn't refresh/);
    expect(screen.getByText("nezopt", { selector: ".sh-host-name" })).toBeInTheDocument();

    // No refresh while hidden.
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    const m = calls.length;
    await vi.advanceTimersByTimeAsync(REFRESH_MS);
    expect(calls.length).toBe(m);
    delete (document as unknown as Record<string, unknown>).hidden;
  });

  it("is translated", async () => {
    setLocale("it");
    stubFetch(() => ({ body: overview() }));
    show();
    expect(await screen.findByText("Da controllare")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Panoramica" })).toBeInTheDocument();
    expect(screen.getByText("Tutto a posto · 122 controlli")).toBeInTheDocument();
  });
});
