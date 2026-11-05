import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { VoiceEvent } from "../api/voice";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import { ago, device, event, events, status, turn } from "../test/voiceFixtures";
import VoiceView, { REFRESH_MS } from "./Voice";

const show = (path = "/voice/dab8") => renderAt(path, () => <Route path="/voice/:id?/:tab?" component={VoiceView} />);

type Page = { events: VoiceEvent[]; next?: string };

/** A fake device API; `eventPages(call)` answers the events route. */
function api(over: { status?: ReturnType<typeof status>; devices?: unknown; eventPages?: (call: Call) => Page } = {}) {
  return stubFetch((call) => {
    if (/\/v1\/voice\/devices$/.test(call.url)) return { body: over.devices ?? [device()] };
    if (/\/status$/.test(call.url)) return { body: over.status ?? status() };
    if (/\/events\?/.test(call.url)) return { body: (over.eventPages ?? (() => ({ events: events() })))(call) };
    return { status: 404, body: { error: { code: "not_found", message: "Not found." } } };
  });
}

afterEach(() => {
  vi.useRealTimers();
  delete (document as unknown as Record<string, unknown>).hidden;
});

describe("Voice device page", () => {
  it("shows the specs, live status, network and power", async () => {
    const calls = api();
    show();
    expect(await screen.findByRole("heading", { level: 1, name: "Kitchen speaker" })).toBeInTheDocument();
    expect(calls[0]!.url).toBe("/v1/voice/devices/dab8/status");

    const specs = screen.getByRole("region", { name: "Specs" });
    const row = (card: HTMLElement, label: string) => within(card).getByText(label).closest(".pg-stat") as HTMLElement;
    expect(row(specs, "Chip")).toHaveTextContent("ESP32, revision v3.1");
    expect(row(specs, "Cores")).toHaveTextContent("2 × 240 MHz");
    expect(row(specs, "Flash")).toHaveTextContent("4 MB");
    expect(row(specs, "PSRAM")).toHaveTextContent("None");
    expect(row(specs, "Firmware")).toHaveTextContent("0.4.1 (1f3ea08)");
    expect(row(specs, "MAC (last bytes)")).toHaveTextContent("da:b8");
    expect(row(specs, "Power sensor")).toHaveTextContent("None");
    expect(row(specs, "Temperature sensor")).toHaveTextContent("No");

    const live = screen.getByRole("region", { name: "Live" });
    expect(row(live, "Status")).toHaveTextContent(/Online\s*Online since 42 minutes ago$/);
    expect(row(live, "Uptime")).toHaveTextContent("3h 05m");
    expect(row(live, "Wi-Fi signal")).toHaveTextContent("-61 dBm, good");
    expect(row(live, "Heap free")).toHaveTextContent("182 KB");
    expect(row(live, "Heap lowest")).toHaveTextContent("141 KB");
    expect(row(live, "CPU")).toHaveTextContent("240 MHz");
    expect(within(live).queryByText("Temperature")).toBeNull();

    const net = screen.getByRole("region", { name: "Network" });
    expect(row(net, "Sent today")).toHaveTextContent("793 KB");
    expect(row(net, "Received today")).toHaveTextContent("2.5 MB");
    expect(row(net, "Sent in total")).toHaveTextContent("2.3 MB");
    expect(row(net, "Received in total")).toHaveTextContent("7.5 MB");

    const power = screen.getByRole("region", { name: "Power" });
    expect(row(power, "Now")).toHaveTextContent("618 mW");
    expect(within(row(power, "Now")).getByText("Estimated")).toBeInTheDocument();
    expect(row(power, "Energy today")).toHaveTextContent("2,315 mWh");
    expect(within(power).getByText(/INA219 or INA226 at I2C address 0x40/)).toBeInTheDocument();
  });

  it("shows the temperature and a measured badge when the device has the sensors", async () => {
    api({
      status: status({
        specs: { ...status().specs, ina: "ina226", temp_sensor: true },
        live: { ...status().live, temp_c: 41.26 },
        power: { est_mw: 590.4, measured: true, energy_mwh_today: 12 },
      }),
    });
    show();
    const power = await screen.findByRole("region", { name: "Power" });
    expect(within(power).getByText("Measured")).toBeInTheDocument();
    expect(within(power).getByText(/Measured by the INA226/)).toBeInTheDocument();
    expect(within(power).queryByText(/wire an INA219/)).toBeNull();
    expect(within(screen.getByRole("region", { name: "Live" })).getByText("41.3 °C")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Specs" })).getByText("INA226")).toBeInTheDocument();
  });

  it("draws each turn as a timing bar with the stage names and milliseconds", async () => {
    api();
    show();
    const list = await screen.findByRole("list", { name: "Last turns, newest first" });
    const rows = within(list).getAllByRole("listitem", { hidden: false }).filter((li) => li.classList.contains("vd-turn"));
    expect(rows).toHaveLength(3);

    const fast = rows[0]!;
    expect(within(fast).getByText("OK")).toBeInTheDocument();
    const bar = within(fast).getByRole("img");
    expect(bar).toHaveAccessibleName("Time from the end of speech: STT 380 ms, Lucina first text 380 ms, TTS first audio 250 ms, First audio 50 ms, Rest of the reply 2,340 ms");
    expect(bar.querySelectorAll(".vd-seg")).toHaveLength(5);
    const legend = [...fast.querySelectorAll(".vd-leg")].map((li) => li.textContent);
    expect(legend).toEqual(["STT380 ms", "Lucina first text380 ms", "TTS first audio250 ms", "First audio50 ms", "Rest of the reply2,340 ms"]);
    expect(within(fast).getByText("Speaks after 1,060 ms")).toBeInTheDocument();
    expect(within(fast).getByText("Total 3,400 ms")).toBeInTheDocument();
    expect(within(fast).getByText("Device heard it after 1,130 ms")).toBeInTheDocument();
    expect(within(fast).getByText("Sent 60 KB")).toBeInTheDocument();
    expect(within(fast).getByText("Received 184 KB")).toBeInTheDocument();

    // All bars share one scale: the 7.2 s turn is longer than the 3.4 s one, and the longest is full width.
    const widths = (el: HTMLElement) => [...el.querySelectorAll<HTMLElement>(".vd-seg")].reduce((sum, s) => sum + parseFloat(s.style.width), 0);
    expect(widths(rows[1]!)).toBeCloseTo(100 * (7200 / 8100), 3);
    expect(widths(fast)).toBeCloseTo(100 * (3400 / 8100), 3);

    // The failed turn keeps the moments it reached and says why it failed.
    const failed = rows[2]!;
    expect(within(failed).getByText("Error: lucina_unavailable")).toBeInTheDocument();
    expect([...failed.querySelectorAll(".vd-leg")].map((li) => li.textContent)).toEqual(["STT690 ms", "Rest of the reply7,410 ms"]);
    expect(within(failed).queryByText(/Device heard it/)).toBeNull();
  });

  it("shows the intent, a fast badge and the filler mark when Lucina routed the turn", async () => {
    api({
      status: status({
        turns: [
          turn({ intent: "temporal_query", confidence: 0.97, jev_ms: 180, fast: true, source: "jev", spans: { stt: 100, route: 280, lucina_first: 300, tts_first: 450, first_audio: 460, total: 900 } }),
          turn({ intent: "web_crawl", confidence: 0.91, jev_ms: 212, fast: false, source: "jev", spans: { stt: 100, route: 320, ack: 330, first_audio: 335, lucina_first: 2400, tts_first: 2800, total: 4000 } }),
          turn({}),
        ],
      }),
    });
    show();
    const list = await screen.findByRole("list", { name: "Last turns, newest first" });
    const rows = [...list.querySelectorAll<HTMLElement>(".vd-turn")];
    expect(rows).toHaveLength(3);

    const [quick, slow, plain] = rows as [HTMLElement, HTMLElement, HTMLElement];
    expect(within(quick).getByText("temporal_query")).toHaveAttribute("title", "temporal_query: confidence 0.97, Jev 180 ms, source jev");
    expect(within(quick).getByText("Fast")).toBeInTheDocument();
    expect(quick.querySelector(".vd-ack-mark")).toBeNull();

    expect(within(slow).getByText("web_crawl")).toBeInTheDocument();
    expect(within(slow).queryByText("Fast")).toBeNull();
    const mark = slow.querySelector<HTMLElement>(".vd-ack-mark")!;
    expect(mark).not.toBeNull();
    // Same scale as the bars: the longest turn here is 4,000 ms.
    expect(parseFloat(mark.style.left)).toBeCloseTo(100 * (330 / 4000), 3);
    expect(within(slow).getByText("Filler after 330 ms", { selector: "span.vd-ack" })).toBeInTheDocument();

    // A turn from a Lucina without the fast path looks as before.
    expect(plain.querySelector(".vd-intent, .vd-ack-mark, .vd-ack")).toBeNull();
    expect(within(plain).queryByText("Fast")).toBeNull();
  });

  it("shows the Jev layers that decided a turn as chips, and the Voice Learn counts", async () => {
    api({
      status: status({
        turns: [
          turn({
            intent: "chat",
            layers: [
              { layer: "intent", verdict: "chat", confidence: 0.91, ms: 210, used: true },
              { layer: "vocab", verdict: "Skibigårdsvej", confidence: 0.62, ms: null, used: false },
            ],
            stt_corrections: 2,
            stt_ambiguous: 1,
          }),
          turn(),
        ],
      }),
    });
    show();
    const list = await screen.findByRole("list", { name: "Last turns, newest first" });
    const [withLayers, plain] = [...list.querySelectorAll<HTMLElement>(".vd-turn")] as [HTMLElement, HTMLElement];
    const chips = within(withLayers).getByRole("list", { name: "Jev layers" });
    const [intent, vocab] = within(chips).getAllByRole("listitem");
    expect(intent).toHaveTextContent("Intent");
    expect(intent).toHaveTextContent("chat");
    expect(intent).toHaveAttribute("data-used", "true");
    expect(intent).toHaveAttribute("title", "Intent · chat · confidence 91% · 210 ms · used");
    expect(vocab).toHaveAttribute("data-used", "false");
    expect(vocab).toHaveAttribute("title", "Vocabulary · Skibigårdsvej · confidence 62% · not used");
    expect(within(withLayers).getByText("Voice Learn fixed 2")).toBeInTheDocument();
    expect(within(withLayers).getByText("1 unsure")).toBeInTheDocument();
    // A turn without layers has no chips and no counts.
    expect(within(plain).queryByRole("list", { name: "Jev layers" })).toBeNull();
    expect(within(plain).queryByText(/Voice Learn fixed/)).toBeNull();
  });

  it("links to Voice Learn from the list of devices", async () => {
    api();
    show("/voice");
    const link = await screen.findByRole("link", { name: "Voice Learn" });
    expect(link).toHaveAttribute("href", "/voice/learn");
  });

  it("says so when a turn has no timings, and when there are no turns", async () => {
    api({ status: status({ turns: [turn({ ok: false, spans: {} })] }) });
    show();
    expect(await screen.findByText("No timings for this turn.")).toBeInTheDocument();
  });

  it("shows an empty list of turns", async () => {
    api({ status: status({ turns: [] }) });
    show();
    expect(await screen.findByText("No turns yet.")).toBeInTheDocument();
  });

  it("shows an offline device with what it last reported", async () => {
    api({ status: status({ online: false, connected_since: null, last_seen: ago(95) }) });
    show();
    expect(await screen.findByText("The speaker is offline. This is what it last reported.")).toHaveAttribute("role", "status");
    const live = screen.getByRole("region", { name: "Live" });
    expect(within(live).getByText("Offline")).toBeInTheDocument();
    expect(within(live).getByText("Last seen 2 hours ago")).toBeInTheDocument();
    expect(within(live).getByText("Wi-Fi signal")).toBeInTheDocument();
  });

  it("says when the speaker never reported", async () => {
    api({ status: status({ online: false, connected_since: null, last_seen: null, live: {}, turns: [] }) });
    show();
    expect(await screen.findByText("The speaker hasn't reported yet.")).toBeInTheDocument();
    expect(screen.getByText("Never connected", { selector: ".vd-since" })).toBeInTheDocument();
  });

  it("shows an error with a retry, and a missing device", async () => {
    let fail = true;
    stubFetch(() => (fail ? { status: 500, body: { error: { code: "boom", message: "The server had a problem." } } } : { body: status() }));
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load this voice device.");
    fail = false;
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("region", { name: "Specs" })).toBeInTheDocument();
  });

  it("says a device that doesn't exist doesn't exist", async () => {
    stubFetch(() => ({ status: 404, body: { error: { code: "not_found", message: "Not found." } } }));
    show("/voice/nope");
    expect(await screen.findByRole("alert")).toHaveTextContent("This voice device doesn't exist.");
  });

  it("refreshes every 10 s while visible and pauses while the tab is hidden", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const calls = api();
    show();
    await screen.findByRole("region", { name: "Specs" });
    expect(calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(REFRESH_MS);
    await waitFor(() => expect(calls).toHaveLength(2));

    let hidden = true;
    Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
    await vi.advanceTimersByTimeAsync(REFRESH_MS * 3);
    expect(calls).toHaveLength(2);

    hidden = false;
    fireEvent(document, new Event("visibilitychange"));
    await waitFor(() => expect(calls).toHaveLength(3));
  });

  it("keeps the data on screen and says so when a refresh fails", async () => {
    let fail = false;
    stubFetch(() => (fail ? { status: 500, body: { error: { code: "boom", message: "The server had a problem." } } } : { body: status() }));
    show();
    await screen.findByRole("region", { name: "Specs" });
    fail = true;
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't refresh: The server had a problem.");
    expect(screen.getByRole("region", { name: "Specs" })).toBeInTheDocument();
  });
});

describe("Voice device events", () => {
  it("lists every kind of event, newest first, one compact line each", async () => {
    const calls = api();
    show("/voice/dab8/events");
    const list = await screen.findByRole("list", { name: "Events, newest first" });
    expect(calls.map((c) => c.url)).toContain("/v1/voice/devices/dab8/events?limit=100");
    const rows = within(list).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("data-kind"))).toEqual(["stats", "turn", "turn", "error", "link", "wifi", "boot"]);
    expect(rows[0]).toHaveTextContent("Stats");
    expect(rows[0]).toHaveTextContent("Heap 182 KB free, up 3h 05m, -62 dBm");
    expect(rows[1]).toHaveTextContent("Turn ok (link), first audio 1,130 ms, total 3,400 ms");
    expect(rows[2]).toHaveTextContent("Turn failed (http): lucina_unavailable");
    expect(rows[3]).toHaveTextContent("i2s_underrun: player starved for 120 ms");
    expect(rows[4]).toHaveTextContent("Link to the server up in 912 ms");
    expect(rows[5]).toHaveTextContent("Wi-Fi lost: beacon_timeout");
    expect(rows[6]).toHaveTextContent("Started (poweron), boot 7");
    expect(screen.getByRole("link", { name: "Events" })).toHaveAttribute("aria-current", "page");
  });

  it("filters by kind on the server with the chips", async () => {
    const calls = api({
      eventPages: (call) => {
        const kind = new URL(call.url, "http://x").searchParams.get("kind");
        return { events: events().filter((e) => !kind || e.k === kind) };
      },
    });
    show("/voice/dab8/events");
    await screen.findByRole("list", { name: "Events, newest first" });
    const group = screen.getByRole("group", { name: "Filter events by kind" });
    expect(within(group).getAllByRole("button").map((b) => b.textContent)).toEqual(["All", "Boot", "Wi-Fi", "Link", "Turn", "Stats", "Error"]);
    expect(within(group).getByRole("button", { name: "All" })).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(within(group).getByRole("button", { name: "Turn" }));
    await waitFor(() => expect(screen.getAllByRole("listitem").map((r) => r.getAttribute("data-kind"))).toEqual(["turn", "turn"]));
    expect(calls.at(-1)!.url).toBe("/v1/voice/devices/dab8/events?kind=turn&limit=100");
    expect(within(group).getByRole("button", { name: "Turn" })).toHaveAttribute("aria-pressed", "true");
    expect(within(group).getByRole("button", { name: "All" })).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(within(group).getByRole("button", { name: "All" }));
    await waitFor(() => expect(screen.getAllByRole("listitem")).toHaveLength(7));
  });

  it("loads the next page with the cursor and hides the button on the last page", async () => {
    const first = events().slice(0, 4);
    const rest = events().slice(4);
    const cursor = first.at(-1)!.at;
    const calls = api({
      eventPages: (call) => (new URL(call.url, "http://x").searchParams.get("before") === cursor ? { events: rest } : { events: first, next: cursor }),
    });
    show("/voice/dab8/events");
    await screen.findByRole("list", { name: "Events, newest first" });
    expect(screen.getAllByRole("listitem")).toHaveLength(4);
    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(screen.getAllByRole("listitem")).toHaveLength(7));
    expect(calls.at(-1)!.url).toBe(`/v1/voice/devices/dab8/events?limit=100&before=${encodeURIComponent(cursor)}`);
    expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
    // Appended below, in order.
    expect(screen.getAllByRole("listitem").map((r) => r.getAttribute("data-kind"))).toEqual(["stats", "turn", "turn", "error", "link", "wifi", "boot"]);
  });

  it("shows empty states for no events and for a kind with none", async () => {
    api({ eventPages: (call) => ({ events: new URL(call.url, "http://x").searchParams.get("kind") ? [] : [event("boot", 5, { reset: "poweron" })] }) });
    show("/voice/dab8/events");
    await screen.findByRole("list", { name: "Events, newest first" });
    expect(screen.getByText("Started (poweron)")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Wi-Fi" }));
    expect(await screen.findByText("No Wi-Fi events.")).toBeInTheDocument();
  });

  it("shows an error with a retry when the events can't load", async () => {
    let fail = true;
    stubFetch((call) => {
      if (/\/status$/.test(call.url)) return { body: status() };
      return fail ? { status: 500, body: { error: { code: "boom", message: "The server had a problem." } } } : { body: { events: events() } };
    });
    show("/voice/dab8/events");
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("The server had a problem.");
    fail = false;
    fireEvent.click(within(alert).getByRole("button", { name: "Try again" }));
    expect(await screen.findByRole("list", { name: "Events, newest first" })).toBeInTheDocument();
  });

  it("shows a kind it doesn't know as its fields", async () => {
    api({ eventPages: () => ({ events: [event("mystery", 2, { a: 1, b: "x" })] }) });
    show("/voice/dab8/events");
    const row = (await screen.findAllByRole("listitem"))[0]!;
    expect(row).toHaveTextContent("mystery");
    expect(row).toHaveTextContent("a=1 b=x");
  });
});

describe("Voice devices list", () => {
  it("lists the devices and opens one", async () => {
    api({ devices: [device(), device({ id: "e1c2", name: "Desk speaker", online: false, last_seen: ago(95), rssi: null, est_mw: null })] });
    show("/voice");
    const list = await screen.findByRole("list", { name: "Devices" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(within(rows[0]!).getByRole("link", { name: "Kitchen speaker" })).toHaveAttribute("href", "/voice/dab8");
    expect(rows[0]).toHaveTextContent("Online");
    expect(rows[0]).toHaveTextContent("firmware 0.4.1");
    expect(rows[0]).toHaveTextContent("-61 dBm, good");
    expect(rows[0]).toHaveTextContent("618 mW");
    expect(rows[1]).toHaveTextContent("Offline");
    expect(rows[1]).toHaveTextContent("Last seen 2 hours ago");
    expect(screen.getByRole("link", { name: /Plugins/ })).toHaveAttribute("href", "/plugins");
  });

  it("explains an empty list", async () => {
    api({ devices: [] });
    show("/voice");
    expect(await screen.findByText("No voice devices yet")).toBeInTheDocument();
  });

  it("shows an error with a retry", async () => {
    stubFetch(() => ({ status: 500, body: { error: { code: "boom", message: "The server had a problem." } } }));
    show("/voice");
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load the voice devices.");
  });
});

describe("Voice page language", () => {
  it("is translated", async () => {
    setLocale("it");
    api();
    show();
    expect(await screen.findByRole("region", { name: "Specifiche" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Ultimi turni" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Eventi" })).toBeInTheDocument();
  });
});
