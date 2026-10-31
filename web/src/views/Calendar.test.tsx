import { Route, type RouteSectionProps } from "@solidjs/router";
import type { Calendar } from "@event-calendar/core";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { For } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CalEvent } from "../api/calendar";
import { setLocale } from "../i18n";
import { MockEventSource } from "../test/eventsource";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";

// EventCalendar itself needs a real layout: the view is tested with a stand-in that renders each event as a button
// and hands the view's callbacks to the test.
const ec = vi.hoisted(() => ({
  options: null as Calendar.Options | null,
  api: {
    next: vi.fn(),
    prev: vi.fn(),
    gotoDate: vi.fn(),
    unselect: vi.fn(),
    setOption: vi.fn(),
    getView: () => ({ type: "list30" }),
  },
}));

vi.mock("../components/calendar/EventCalendar", () => ({
  default: (props: { options: Calendar.Options; events: Calendar.EventInput[]; view: string; onReady?: (api: unknown) => void }) => {
    ec.options = props.options;
    queueMicrotask(() => {
      props.onReady?.(ec.api);
      props.options.datesSet?.({
        start: new Date(2026, 9, 26),
        end: new Date(2026, 10, 2),
        startStr: "",
        endStr: "",
        view: { type: props.view, title: "Oct 26 – Nov 1, 2026" } as Calendar.View,
      });
    });
    return (
      <div data-testid="ec" data-view={props.view}>
        <For each={props.events}>
          {(x) => (
            <button
              type="button"
              data-id={String(x.id)}
              onClick={() => props.options.eventClick?.({ event: x as unknown as Calendar.Event } as Calendar.EventClickInfo)}
            >
              {String(x.title)}
            </button>
          )}
        </For>
      </div>
    );
  },
}));

import CalendarView from "./Calendar";
import Shell from "./Shell";

const START = new Date(2026, 9, 30, 15, 0).getTime();

function event(over: Partial<CalEvent> = {}): CalEvent {
  return {
    id: "lucina:ev_1",
    source: "lucina",
    sourceId: "ev_1",
    calendar: "Lucina",
    title: "Dentist",
    start: new Date(START).toISOString(),
    end: new Date(START + 3_600_000).toISOString(),
    allDay: false,
    startMs: START,
    endMs: START + 3_600_000,
    location: "Søndergade 20, Aarhus",
    description: "Bring the card.",
    recurring: false,
    editable: true,
    addedBy: "Lucina",
    ...over,
  };
}

const IPHONE = event({
  id: "iphone:CAL:EK1@1793343600000",
  source: "iphone",
  sourceId: "CAL:EK1",
  calendar: "Aarhus Universitet",
  title: "Ethical Hacking",
  startMs: START - 6 * 3_600_000,
  endMs: START - 2 * 3_600_000,
  editable: false,
  addedBy: undefined,
  description: "Join: https://meet.google.com/abc-defg-hij",
});

let calls: Call[];

function serve(events: CalEvent[] = [event(), IPHONE], proposals: unknown[] = []) {
  calls = stubFetch((c) => {
    const u = new URL(c.url, "http://test.local");
    if (u.pathname === "/v1/calendar/events" && c.method === "GET") return { body: { from: "", to: "", timeZone: "Europe/Copenhagen", events } };
    if (u.pathname === "/v1/calendar/events" && c.method === "POST") {
      const b = c.body as { title: string; start: string; end: string };
      return { status: 201, body: { created: true, event: event({ id: "lucina:ev_new", title: b.title }) } };
    }
    if (u.pathname.startsWith("/v1/calendar/events/") && c.method === "PATCH") return { body: { updated: true, event: event() } };
    if (u.pathname.startsWith("/v1/calendar/events/") && c.method === "DELETE") return { body: { deleted: true, id: "x" } };
    if (u.pathname === "/v1/calendar/proposals") return { body: { proposals } };
    if (u.pathname.endsWith("/accept") && c.method === "POST") {
      proposals.splice(0);
      return { status: 201, body: { created: true, event: event({ id: "lucina:ev_p" }) } };
    }
    return { status: 404, body: {} };
  });
}

const sent = (method: string, path: string) =>
  calls.filter((c) => c.method === method && new URL(c.url, "http://test.local").pathname === path);

function open(path = "/calendar") {
  return renderAt(path, () => <Route path="/calendar" component={CalendarView} />);
}

beforeEach(() => {
  setLocale("en");
  try {
    localStorage.clear();
  } catch {
    // Ignore.
  }
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("Calendar", () => {
  it("loads the visible range and shows the events", async () => {
    serve();
    open();
    expect(await screen.findByRole("button", { name: "Dentist" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Ethical Hacking" })).toBeInTheDocument();
    const q = new URL(sent("GET", "/v1/calendar/events")[0]!.url, "http://test.local").searchParams;
    expect(q.get("from")).toBe(new Date(2026, 9, 26).toISOString());
    expect(q.get("to")).toBe(new Date(2026, 10, 2).toISOString());
    // Phone layout (jsdom measures 0 px): List by default, the legend lists every calendar.
    expect(screen.getByTestId("ec").dataset.view).toBe("list30");
    expect(screen.getByRole("button", { name: /Aarhus Universitet/ })).toHaveAttribute("aria-pressed", "true");
  });

  it("opens the event a deep link names, with its place, notes and who added it", async () => {
    serve();
    open(`/calendar?event=${encodeURIComponent("lucina:ev_1")}&at=${START}`);
    const sheet = await screen.findByRole("dialog", { name: "Dentist" });
    expect(within(sheet).getByText("Søndergade 20, Aarhus")).toBeInTheDocument();
    expect(within(sheet).getByRole("link", { name: "Show on map" }).getAttribute("href")).toMatch(/^\/map\/place\?/);
    expect(within(sheet).getByText("Bring the card.")).toBeInTheDocument();
    expect(within(sheet).getByText("Added by Lucina")).toBeInTheDocument();
    expect(within(sheet).getByText("Lucina's calendar")).toBeInTheDocument();
    expect(ec.api.gotoDate).not.toHaveBeenCalled();
  });

  it("finds an iPhone event from a notification's EventKit id and says when it is gone", async () => {
    serve();
    open(`/calendar?event=${encodeURIComponent("iphone:CAL:EK1@1793343600000")}&at=${START - 6 * 3_600_000}`);
    const sheet = await screen.findByRole("dialog", { name: "Ethical Hacking" });
    expect(within(sheet).getByRole("link", { name: "Join video call" })).toHaveAttribute("href", "https://meet.google.com/abc-defg-hij");
    expect(within(sheet).getByText(/read-only, change it on the phone/)).toBeInTheDocument();
    expect(within(sheet).queryByRole("button", { name: "Delete" })).toBeNull();
    expect(within(sheet).queryByRole("button", { name: "Edit" })).toBeNull();
  });

  it("says when a linked event is no longer there", async () => {
    serve();
    open(`/calendar?event=${encodeURIComponent("lucina:gone")}&at=${START + 1}`);
    expect(await screen.findByText("This event is no longer in the calendar.")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("asks before deleting, then deletes with confirm=true", async () => {
    serve();
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Dentist" }));
    const sheet = await screen.findByRole("dialog", { name: "Dentist" });
    fireEvent.click(within(sheet).getByRole("button", { name: "Delete" }));
    const confirm = await screen.findByRole("dialog", { name: "Delete event?" });
    expect(within(confirm).getByText(/Delete “Dentist” on /)).toBeInTheDocument();
    expect(sent("DELETE", "/v1/calendar/events/lucina%3Aev_1")).toHaveLength(0);
    fireEvent.click(within(confirm).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(sent("DELETE", "/v1/calendar/events/lucina%3Aev_1")).toHaveLength(1));
    expect(new URL(sent("DELETE", "/v1/calendar/events/lucina%3Aev_1")[0]!.url, "http://test.local").searchParams.get("confirm")).toBe("true");
    expect(await screen.findByText("Deleted “Dentist”.")).toBeInTheDocument();
  });

  it("creates an event in Lucina's calendar from the + button", async () => {
    serve();
    open();
    await screen.findByRole("button", { name: "Dentist" });
    fireEvent.click(screen.getByRole("button", { name: "New event" }));
    const editor = await screen.findByRole("dialog", { name: "New event" });
    fireEvent.click(within(editor).getByRole("button", { name: "Save" }));
    expect(await within(editor).findByRole("alert")).toHaveTextContent("Give the event a title.");
    fireEvent.input(within(editor).getByLabelText("Title"), { target: { value: "Haircut" } });
    fireEvent.input(within(editor).getByLabelText("Place"), { target: { value: "Barber, Aarhus" } });
    fireEvent.click(within(editor).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(sent("POST", "/v1/calendar/events")).toHaveLength(1));
    const body = sent("POST", "/v1/calendar/events")[0]!.body as Record<string, unknown>;
    expect(body).toMatchObject({ title: "Haircut", allDay: false, location: "Barber, Aarhus", description: "" });
    expect(Date.parse(body.end as string) - Date.parse(body.start as string)).toBe(3_600_000);
    expect(await screen.findByText("Added “Haircut”.")).toBeInTheDocument();
  });

  it("creates from a tapped slot", async () => {
    serve();
    open();
    await screen.findByRole("button", { name: "Dentist" });
    ec.options!.dateClick!({ date: new Date(2026, 10, 2, 10, 30), allDay: false, view: { type: "timeGridDay" } } as Calendar.DateClickInfo);
    const editor = await screen.findByRole("dialog", { name: "New event" });
    expect(within(editor).getByLabelText("Start time")).toHaveValue("10:30");
    expect(within(editor).getByLabelText("End time")).toHaveValue("11:30");
  });

  it("saves an edit with confirm: true", async () => {
    serve();
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Dentist" }));
    fireEvent.click(within(await screen.findByRole("dialog", { name: "Dentist" })).getByRole("button", { name: "Edit" }));
    const editor = await screen.findByRole("dialog", { name: "Edit event" });
    expect(within(editor).getByLabelText("Title")).toHaveValue("Dentist");
    fireEvent.input(within(editor).getByLabelText("Title"), { target: { value: "Dentist (check-up)" } });
    fireEvent.click(within(editor).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(sent("PATCH", "/v1/calendar/events/lucina%3Aev_1")).toHaveLength(1));
    expect(sent("PATCH", "/v1/calendar/events/lucina%3Aev_1")[0]!.body).toMatchObject({
      confirm: true,
      title: "Dentist (check-up)",
      location: "Søndergade 20, Aarhus",
      start: new Date(START).toISOString(),
      end: new Date(START + 3_600_000).toISOString(),
    });
  });

  it("moves a dragged event with confirm: true and offers an undo", async () => {
    serve();
    open();
    await screen.findByRole("button", { name: "Dentist" });
    const e = event();
    const revert = vi.fn();
    const moved = { start: new Date(START + 3_600_000), end: new Date(START + 2 * 3_600_000), allDay: false, extendedProps: { ev: e } };
    const old = { start: new Date(START), end: new Date(START + 3_600_000), allDay: false, extendedProps: { ev: e } };
    ec.options!.eventDrop!({ event: moved, oldEvent: old, revert } as unknown as Calendar.EventDropInfo);
    await waitFor(() => expect(sent("PATCH", "/v1/calendar/events/lucina%3Aev_1")).toHaveLength(1));
    expect(sent("PATCH", "/v1/calendar/events/lucina%3Aev_1")[0]!.body).toEqual({
      confirm: true,
      allDay: false,
      start: new Date(START + 3_600_000).toISOString(),
      end: new Date(START + 2 * 3_600_000).toISOString(),
    });
    expect(revert).not.toHaveBeenCalled();
    fireEvent.click(await screen.findByRole("button", { name: "Undo" }));
    await waitFor(() => expect(sent("PATCH", "/v1/calendar/events/lucina%3Aev_1")).toHaveLength(2));
    expect(sent("PATCH", "/v1/calendar/events/lucina%3Aev_1")[1]!.body).toMatchObject({ start: new Date(START).toISOString() });
  });

  it("puts a read-only event back when it is dragged", async () => {
    serve();
    open();
    await screen.findByRole("button", { name: "Dentist" });
    const revert = vi.fn();
    const x = { start: new Date(START), end: new Date(START + 1), allDay: false, extendedProps: { ev: IPHONE } };
    ec.options!.eventDrop!({ event: x, oldEvent: x, revert } as unknown as Calendar.EventDropInfo);
    expect(revert).toHaveBeenCalled();
    expect(sent("PATCH", `/v1/calendar/events/${encodeURIComponent(IPHONE.id)}`)).toHaveLength(0);
  });

  it("shows Lucina's proposals and adds one to her calendar", async () => {
    serve([event()], [
      {
        id: "prop_1",
        event: { title: "Padel with Marco", start: new Date(START + 86_400_000).toISOString(), location: "Club Nord" },
        from: "mcp:claude-code",
        reason: "Google Calendar isn't connected",
        createdAt: "",
        expiresAtMs: Date.now() + 86_400_000,
      },
    ]);
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Padel with Marco" }));
    const sheet = await screen.findByRole("dialog", { name: "Padel with Marco" });
    expect(within(sheet).getByText("Proposed by mcp:claude-code")).toBeInTheDocument();
    expect(within(sheet).getByText("Why it's a proposal: Google Calendar isn't connected")).toBeInTheDocument();
    fireEvent.click(within(sheet).getByRole("button", { name: "Add to calendar" }));
    await waitFor(() => expect(sent("POST", "/v1/calendar/proposals/prop_1/accept")).toHaveLength(1));
    expect(sent("POST", "/v1/calendar/proposals/prop_1/accept")[0]!.body).toEqual({ confirm: true });
    expect(await screen.findByText("Added “Padel with Marco” to your calendar.")).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("button", { name: "Padel with Marco" })).toBeNull());
  });

  it("hides a calendar from the legend", async () => {
    serve();
    open();
    await screen.findByRole("button", { name: "Ethical Hacking" });
    fireEvent.click(screen.getByRole("button", { name: /Aarhus Universitet/ }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Ethical Hacking" })).toBeNull());
    expect(screen.getByRole("button", { name: "Dentist" })).toBeInTheDocument();
  });
});

describe("Shell: the Calendar tab", () => {
  it("is in the tab bar, right after Chat", () => {
    vi.stubGlobal("EventSource", MockEventSource);
    stubFetch(() => ({ status: 404, body: {} }));
    renderAt("/calendar", () => <Route path="*" component={(p: RouteSectionProps) => <Shell>{p.children}</Shell>} />);
    const tabs = [...document.querySelectorAll<HTMLAnchorElement>(".tabbar a")];
    expect(tabs.map((a) => a.getAttribute("href"))).toEqual(["/", "/calendar", "/shopping", "/training", "/plugins"]);
    expect(tabs[1]).toHaveTextContent("Calendar");
    expect(tabs[1]).toHaveAttribute("aria-current", "page");
  });
});
