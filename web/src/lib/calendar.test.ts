import { describe, expect, it } from "vitest";
import type { CalEvent, CalProposal } from "../api/calendar";
import {
  addDaysYmd,
  calendarColor,
  calendarLinkRoute,
  calendarName,
  calendarRoute,
  defaultStart,
  draftFromForm,
  eventRoute,
  findMeetingUrl,
  formForEvent,
  formForRange,
  GOOGLE_COLOR,
  googleCalendarLinkRoute,
  googleEidToId,
  LUCINA_COLOR,
  matchDeepLink,
  meetingUrl,
  PALETTE,
  plainNotes,
  proposalToEc,
  proposalWhen,
  rangeQuery,
  shiftEnd,
  startLabel,
  timesOf,
  toEcEvent,
  whenLabel,
} from "./calendar";

function ev(over: Partial<CalEvent> = {}): CalEvent {
  const startMs = new Date(2026, 9, 30, 13, 30).getTime();
  return {
    id: "lucina:ev_1",
    source: "lucina",
    sourceId: "ev_1",
    calendar: "Lucina",
    title: "Mentor",
    start: new Date(startMs).toISOString(),
    end: new Date(startMs + 3_600_000).toISOString(),
    allDay: false,
    startMs,
    endMs: startMs + 3_600_000,
    recurring: false,
    editable: true,
    ...over,
  };
}

const b64url = (s: string) => btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

describe("colours", () => {
  it("gives Lucina the accent, Google blue and other calendars a stable palette colour", () => {
    expect(calendarColor("Lucina", "lucina")).toBe(LUCINA_COLOR);
    expect(calendarColor("Google", "google")).toBe(GOOGLE_COLOR);
    const c = calendarColor("Aarhus Universitet", "iphone");
    expect(PALETTE).toContain(c);
    expect(calendarColor("Aarhus Universitet", "iphone")).toBe(c);
  });

  it("names the calendar of an event", () => {
    expect(calendarName(ev())).toBe("Lucina");
    expect(calendarName(ev({ source: "iphone", calendar: " Home " }))).toBe("Home");
    expect(calendarName(ev({ source: "google", calendar: undefined }))).toBe("Google");
  });
});

describe("deep links", () => {
  it("builds the Calendar route of an event", () => {
    expect(eventRoute("iphone:A:B@1793363400000", 1793363400000)).toBe(
      "/calendar?event=iphone%3AA%3AB%401793363400000&at=1793363400000",
    );
    expect(eventRoute("lucina:ev_1")).toBe("/calendar?event=lucina%3Aev_1");
  });

  it("decodes Google's eid and rewrites calendar.google.com event links", () => {
    expect(googleEidToId(b64url("abc123 owner@example.com"))).toBe("abc123");
    expect(googleEidToId("!!!")).toBeNull();
    expect(googleCalendarLinkRoute(`https://calendar.google.com/calendar/event?eid=${b64url("abc123 me@x.com")}`)).toBe(
      "/calendar?event=google%3Aabc123",
    );
    expect(googleCalendarLinkRoute("https://calendar.google.com/calendar/r")).toBeNull();
    expect(googleCalendarLinkRoute("https://evil.example/event?eid=YWJj")).toBeNull();
  });

  it("opens the app's own /calendar links in the app", () => {
    const origin = "http://localhost:5173";
    expect(calendarLinkRoute("https://getsecretary.tech/calendar?event=lucina%3Aev_abc", origin)).toBe(
      "/calendar?event=lucina%3Aev_abc",
    );
    expect(calendarLinkRoute(`${origin}/calendar?event=lucina%3Aev_abc&at=1`, origin)).toBe("/calendar?event=lucina%3Aev_abc&at=1");
    expect(calendarLinkRoute("https://getsecretary.tech/shopping", origin)).toBeNull();
    expect(calendarLinkRoute("https://getsecretary.tech.evil.example/calendar?event=x", origin)).toBeNull();
    expect(calendarLinkRoute("https://example.com/calendar", origin)).toBeNull();
  });

  it("routes calendar notifications to the event, old ones from their data", () => {
    expect(calendarRoute({ category: "calendar", route: "/calendar?event=lucina%3Aev_1" })).toBe("/calendar?event=lucina%3Aev_1");
    expect(calendarRoute({ category: "calendar", route: "/", data: { eventId: "CAL:EK1", start: 1793363400000 } })).toBe(
      "/calendar?event=iphone%3ACAL%3AEK1%401793363400000&at=1793363400000",
    );
    expect(calendarRoute({ category: "calendar", route: "/", data: { eventId: "lucina:ev_9", start: 5 } })).toBe(
      "/calendar?event=lucina%3Aev_9&at=5",
    );
    expect(calendarRoute({ category: "calendar", route: "/" })).toBeNull();
    expect(calendarRoute({ category: "reminder", route: "/", data: { eventId: "x", start: 1 } })).toBeNull();
  });

  it("matches a deep link by id, then by start and provider id, then the only event at that time", () => {
    const at = 1793624400000;
    const google = ev({ id: "google:ooae68", source: "google", startMs: at });
    const other = ev({ id: "lucina:ev_2", startMs: at + 3_600_000 });
    expect(matchDeepLink([google, other], "lucina:ev_2")).toBe(other);
    expect(matchDeepLink([google, other], "iphone:6F79:ooae68@google.com@1793624400000", at)).toBe(google);
    const lone = ev({ id: "iphone:X:Y@1", startMs: at });
    expect(matchDeepLink([lone], "iphone:Z@1", at)).toBe(lone);
    expect(matchDeepLink([lone, google], "iphone:Z@1", at)).toBeNull();
    expect(matchDeepLink([other], "lucina:gone", at)).toBeNull();
  });
});

describe("notes and meeting links", () => {
  it("turns HTML notes into plain text", () => {
    expect(plainNotes("<p>Agenda:</p><ul><li>One &amp; two</li></ul>Bring <b>cake</b><br>Thanks")).toBe(
      "Agenda:\nOne & two\nBring cake\nThanks",
    );
    expect(plainNotes(undefined)).toBe("");
  });

  it("finds video-call links on the known hosts only", () => {
    expect(findMeetingUrl("Join: https://meet.google.com/abc-defg-hij.")).toBe("https://meet.google.com/abc-defg-hij");
    expect(findMeetingUrl("https://us02web.zoom.us/j/123?pwd=x")).toBe("https://us02web.zoom.us/j/123?pwd=x");
    expect(findMeetingUrl("https://example.com/meet.google.com")).toBeNull();
    expect(meetingUrl({ conferenceUrl: "https://teams.microsoft.com/l/x" })).toBe("https://teams.microsoft.com/l/x");
    expect(meetingUrl({ location: "Room 4", description: "https://whereby.com/team" })).toBe("https://whereby.com/team");
  });
});

describe("EventCalendar mapping", () => {
  it("maps a timed event with its colour, editability and the event itself", () => {
    const e = ev();
    const x = toEcEvent(e, e.startMs);
    expect(x.start).toEqual(new Date(e.startMs));
    expect(x.end).toEqual(new Date(e.endMs));
    expect(x.editable).toBe(true);
    expect(x.styles).toEqual([`--cal-color: ${LUCINA_COLOR}`]);
    expect(x.classNames).toEqual(["cal-ev", "cal-ev--lucina"]);
    expect((x.extendedProps as { ev: CalEvent }).ev).toBe(e);
  });

  it("maps all-day and read-only past events", () => {
    const e = ev({ source: "iphone", calendar: "Holidays", allDay: true, start: "2026-10-31", end: "2026-11-01", editable: false });
    const x = toEcEvent(e, e.endMs + 1);
    expect(x.allDay).toBe(true);
    expect(x.start).toBe("2026-10-31");
    expect(x.end).toBe("2026-11-01");
    expect(x.editable).toBe(false);
    expect(x.classNames).toEqual(["cal-ev", "cal-ev--iphone", "cal-ev--ro", "cal-ev--past"]);
  });

  it("shows proposals as dashed read-only events", () => {
    const p: CalProposal = {
      id: "prop_1",
      event: { title: "Padel", start: "2026-11-02T17:00:00Z", end: "2026-11-02T18:30:00Z" },
      from: "mcp:claude-code",
      createdAt: "",
      expiresAtMs: 0,
    };
    const x = proposalToEc(p)!;
    expect(x.id).toBe("proposal:prop_1");
    expect(x.editable).toBe(false);
    expect(x.classNames).toContain("cal-ev--proposal");
    expect(proposalToEc({ ...p, event: { title: "x", start: "nope" } })).toBeNull();
    expect(proposalWhen({ ...p, event: { title: "Trip", start: "2026-11-08", end: "2026-11-11" } }, "en-GB")).toBe(
      "Sun 8 Nov – Tue 10 Nov",
    );
  });

  it("asks for the visible range", () => {
    expect(rangeQuery(new Date("2026-10-26T00:00:00Z"), new Date("2026-11-02T00:00:00Z"))).toBe(
      "from=2026-10-26T00%3A00%3A00.000Z&to=2026-11-02T00%3A00%3A00.000Z",
    );
  });
});

describe("the editor", () => {
  it("builds a timed request body as UTC instants", () => {
    const f = formForRange(new Date(2026, 10, 2, 14, 0), new Date(2026, 10, 2, 15, 30), false);
    f.title = "  Dentist ";
    f.location = "Søndergade 20";
    const r = draftFromForm(f);
    expect(r).toEqual({
      draft: {
        title: "Dentist",
        allDay: false,
        start: new Date(2026, 10, 2, 14, 0).toISOString(),
        end: new Date(2026, 10, 2, 15, 30).toISOString(),
        location: "Søndergade 20",
        description: "",
      },
    });
  });

  it("sends all-day events with the exclusive end and the video link", () => {
    const f = formForRange(new Date(2026, 10, 8), new Date(2026, 10, 11), true);
    expect(f.endDate).toBe("2026-11-10");
    f.title = "Trip";
    f.video = "https://meet.google.com/x";
    expect(draftFromForm(f)).toEqual({
      draft: { title: "Trip", allDay: true, start: "2026-11-08", end: "2026-11-11", location: "", description: "", conferenceUrl: "https://meet.google.com/x" },
    });
  });

  it("refuses a missing title or an end before the start", () => {
    const f = formForRange(new Date(2026, 10, 2, 14), new Date(2026, 10, 2, 15), false);
    expect(draftFromForm(f)).toEqual({ error: "needTitle" });
    expect(draftFromForm({ ...f, title: "x", endTime: "13:00" })).toEqual({ error: "needEndAfterStart" });
  });

  it("fills the form from an event, all-day ends shown inclusive", () => {
    const f = formForEvent(ev({ allDay: true, start: "2026-11-08", end: "2026-11-11", description: "<b>hi</b>", conferenceUrl: "https://meet.google.com/a" }));
    expect([f.startDate, f.endDate, f.allDay, f.notes, f.video]).toEqual(["2026-11-08", "2026-11-10", true, "hi", "https://meet.google.com/a"]);
    expect(formForEvent(ev({ title: "(no title)" })).title).toBe("");
  });

  it("moves the end with the start", () => {
    const f = { ...formForRange(new Date(2026, 10, 2, 14), new Date(2026, 10, 2, 15, 30), false), title: "x" };
    const moved = shiftEnd(f, { ...f, startTime: "23:00" });
    expect([moved.endDate, moved.endTime]).toEqual(["2026-11-03", "00:30"]);
    const day = { ...formForRange(new Date(2026, 10, 8), new Date(2026, 10, 11), true), title: "x" };
    expect(shiftEnd(day, { ...day, startDate: "2026-11-09" }).endDate).toBe("2026-11-11");
  });

  it("picks the next full hour today, else 09:00 on the day in view", () => {
    const now = new Date(2026, 9, 30, 13, 20);
    expect(defaultStart(now, now)).toEqual(new Date(2026, 9, 30, 14, 0));
    expect(defaultStart(new Date(2026, 10, 4, 0, 0), now)).toEqual(new Date(2026, 10, 4, 9, 0));
  });

  it("turns a dragged event's times into the PATCH body", () => {
    const s = new Date(2026, 10, 2, 14);
    expect(timesOf(s, new Date(2026, 10, 2, 15), false)).toEqual({ allDay: false, start: s.toISOString(), end: new Date(2026, 10, 2, 15).toISOString() });
    expect(timesOf(new Date(2026, 10, 2), null, true)).toEqual({ allDay: true, start: "2026-11-02", end: "2026-11-03" });
    expect(addDaysYmd("2026-12-31", 1)).toBe("2027-01-01");
  });
});

describe("labels", () => {
  it("says when an event is", () => {
    const e = ev();
    expect(whenLabel(e, "en-GB")).toBe("Fri 30 Oct · 13:30–14:30");
    expect(whenLabel(ev({ allDay: true, start: "2026-10-31", end: "2026-11-01" }), "en-GB")).toBe("Sat 31 Oct");
    expect(whenLabel(ev({ allDay: true, start: "2026-10-31", end: "2026-11-03" }), "en-GB")).toBe("Sat 31 Oct – Mon 2 Nov");
    const late = new Date(2026, 9, 30, 23).getTime();
    expect(whenLabel(ev({ startMs: late, endMs: late + 2 * 3_600_000 }), "en-GB")).toBe("Fri 30 Oct, 23:00 – Sat 31 Oct, 01:00");
    expect(startLabel(new Date(2026, 9, 30, 14), false, "en-GB")).toBe("Fri 30 Oct, 14:00");
  });
});
