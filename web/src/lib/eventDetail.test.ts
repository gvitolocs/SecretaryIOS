import { describe, expect, it } from "vitest";
import type { CalEvent } from "../api/calendar";
import {
  alarmParts,
  dayStrip,
  eventPhones,
  leaveAtMs,
  moveTargets,
  noteSegments,
  notesCollapse,
  phoneHref,
  phoneSpans,
  setAlarm,
  suggestedTravelMin,
} from "./eventDetail";

const H = 3_600_000;
const base = new Date(2026, 10, 2, 0, 0, 0, 0).getTime();

function ev(id: string, startH: number, endH: number, extra: Partial<CalEvent> = {}): CalEvent {
  return {
    id,
    source: "lucina",
    sourceId: id,
    title: id,
    start: new Date(base + startH * H).toISOString(),
    end: new Date(base + endH * H).toISOString(),
    allDay: false,
    startMs: base + startH * H,
    endMs: base + endH * H,
    recurring: false,
    editable: true,
    ...extra,
  };
}

describe("phone numbers", () => {
  it("finds Danish and international numbers", () => {
    expect(phoneSpans("Ring 52 20 01 44 i morgen").map((p) => p.href)).toEqual(["tel:+4552200144"]);
    expect(phoneSpans("Tlf: 5220 0144").map((p) => p.href)).toEqual(["tel:+4552200144"]);
    expect(phoneSpans("52200144").map((p) => p.href)).toEqual(["tel:+4552200144"]);
    expect(phoneSpans("+45 52 20 01 44").map((p) => p.href)).toEqual(["tel:+4552200144"]);
    expect(phoneSpans("Mario: +39 333 123 4567.").map((p) => [p.text, p.href])).toEqual([["+39 333 123 4567", "tel:+393331234567"]]);
    expect(phoneSpans("0045 52200144").map((p) => p.href)).toEqual(["tel:+4552200144"]);
    expect(phoneSpans("+1 (415) 555-0100").map((p) => p.href)).toEqual(["tel:+14155550100"]);
  });

  it("leaves room numbers, dates, times, codes and links alone", () => {
    for (const t of [
      "5220-014 Seminarlokale",
      "52 20 01 4",
      "12:00–16:00",
      "14.00-15.30",
      "2026-11-02",
      "02.11.2026",
      "Universitetsbyen 76, 8000 Aarhus C",
      "https://teams.microsoft.com/l/meetup-join/19%3a52200144",
      "id 1234567890123",
      "ref ABC52200144",
      "mail 52200144@x.dk",
      "12345678",
    ]) {
      expect(phoneSpans(t), t).toEqual([]);
    }
    expect(phoneHref("+45 12")).toBeNull();
  });

  it("collects each number once from place, title and notes", () => {
    const e = { title: "Call 52 20 01 44", location: "Kitchen, +4552200144", description: "or 2345 6789, or 52200144" };
    expect(eventPhones(e).map((p) => p.href)).toEqual(["tel:+4552200144", "tel:+4523456789"]);
  });
});

describe("notes", () => {
  const note =
    "Join: https://teams.microsoft.com/l/meetup-join/abc. Questions: anna@au.dk or 52 20 01 44.\n" +
    "Where: Boiler, Kitchen, Universitetsbyen 76, 8000 Aarhus C.";

  it("marks links, emails, phones and addresses", () => {
    const segs = noteSegments(note).filter((s) => s.kind !== "text");
    expect(segs).toEqual([
      { kind: "link", text: "https://teams.microsoft.com/l/meetup-join/abc", href: "https://teams.microsoft.com/l/meetup-join/abc" },
      { kind: "email", text: "anna@au.dk", href: "mailto:anna@au.dk" },
      { kind: "phone", text: "52 20 01 44", href: "tel:+4552200144" },
      { kind: "address", text: "Universitetsbyen 76, 8000 Aarhus C", query: "Universitetsbyen 76, 8000 Aarhus C" },
    ]);
    expect(noteSegments(note).map((s) => s.text).join("")).toBe(note);
  });

  it("finds Italian addresses and keeps plain text plain", () => {
    expect(noteSegments("Ci vediamo in Via Roma 12, 00184 Roma alle 9").filter((s) => s.kind === "address").map((s) => s.text)).toEqual([
      "Via Roma 12, 00184 Roma",
    ]);
    expect(noteSegments("SoftSec class is 12:00–16:00")).toEqual([{ kind: "text", text: "SoftSec class is 12:00–16:00" }]);
  });

  it("collapses long notes", () => {
    expect(notesCollapse("short")).toBe(false);
    expect(notesCollapse("x".repeat(281))).toBe(true);
    expect(notesCollapse("1\n2\n3\n4\n5")).toBe(false);
    expect(notesCollapse("1\n2\n3\n4\n5\n6")).toBe(true);
  });
});

describe("day strip", () => {
  it("shows the hours around the event with the overlapping seminar beside it", () => {
    const kitchen = ev("kitchen", 14, 15.5);
    const seminar = ev("seminar", 12, 16);
    const later = ev("later", 18, 19);
    const strip = dayStrip(kitchen, [seminar, kitchen, later, ev("allday", 0, 24, { allDay: true })])!;
    expect(strip.fromMs).toBe(base + 13 * H);
    expect(strip.toMs).toBe(base + 17 * H);
    expect(strip.hours.length).toBe(5);
    expect(strip.blocks.map((b) => [b.ev.id, b.col, b.cols, b.self])).toEqual([
      ["seminar", 0, 2, false],
      ["kitchen", 1, 2, true],
    ]);
    const k = strip.blocks.find((b) => b.self)!;
    expect(k.top).toBeCloseTo(0.25);
    expect(k.height).toBeCloseTo(0.375);
    expect(strip.blocks[0]!.top).toBe(0);
  });

  it("is at least three hours and skips all-day events", () => {
    const s = dayStrip(ev("a", 9, 9.25), [])!;
    expect((s.toMs - s.fromMs) / H).toBe(3);
    expect(s.blocks).toHaveLength(1);
    expect(dayStrip(ev("d", 0, 24, { allDay: true }), [])).toBeNull();
  });
});

describe("settings", () => {
  it("labels alerts", () => {
    expect(alarmParts(0)).toEqual({ at: true });
    expect(alarmParts(15)).toEqual({ unit: "min", count: 15 });
    expect(alarmParts(120)).toEqual({ unit: "h", count: 2 });
    expect(alarmParts(2880)).toEqual({ unit: "d", count: 2 });
    expect(alarmParts(10080)).toEqual({ unit: "w", count: 1 });
  });

  it("edits the alert rows", () => {
    expect(setAlarm([60, 1440], 2, 15)).toEqual([15, 60, 1440]);
    expect(setAlarm([60, 1440], 0, null)).toEqual([1440]);
    expect(setAlarm([60, 1440], 1, 60)).toEqual([60]);
    expect(setAlarm([0, 5, 10, 15, 30], 5, 60)).toEqual([0, 5, 10, 15, 30]);
  });

  it("works out when to leave", () => {
    expect(leaveAtMs({ startMs: 10 * H, travelMin: 20 })).toBe(10 * H - 20 * 60_000);
    expect(leaveAtMs({ startMs: 10 * H })).toBeNull();
    expect(suggestedTravelMin(17 * 60 + 5)).toBe(20);
    expect(suggestedTravelMin(30)).toBe(5);
  });

  it("offers a move between Lucina and Google", () => {
    expect(moveTargets({ source: "lucina", editable: true, recurring: false }, true)).toEqual(["lucina", "google"]);
    expect(moveTargets({ source: "google", editable: true, recurring: false }, true)).toEqual(["google", "lucina"]);
    expect(moveTargets({ source: "google", editable: true, recurring: true }, true)).toEqual([]);
    expect(moveTargets({ source: "lucina", editable: true, recurring: false }, false)).toEqual([]);
    expect(moveTargets({ source: "ics", editable: false, recurring: false }, true)).toEqual([]);
  });
});
