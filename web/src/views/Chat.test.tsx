import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { chat, reloadChatFromStorage } from "../state/chat";
import { installEventSource, MockEventSource } from "../test/eventsource";
import { deferred, stubFetch, type Call, type Handler } from "../test/fetch";
import ChatView from "./Chat";

// No WebGL or audio in tests: the Lucina stage is a fake.
vi.mock("../lib/stageLoader", () => import("../test/stage"));

const EMPTY_PAGE = { body: { items: [], nextBefore: null } };

function type(text: string) {
  const box = screen.getByLabelText("Message Lucina") as HTMLTextAreaElement;
  fireEvent.input(box, { target: { value: text } });
  fireEvent.keyDown(box, { key: "Enter" });
  return box;
}

/** Route timeline requests to `timeline` and everything else to `rest`. */
function serve(rest: Handler, timeline: Handler = () => EMPTY_PAGE): Call[] {
  return stubFetch((c) => (c.url.startsWith("/v1/timeline") ? timeline(c) : rest(c)));
}

const posts = (calls: Call[]) => calls.filter((c) => c.url === "/v1/chat");

const now = Date.now();
const H = 3_600_000;
const at = (msAgo: number) => new Date(now - msAgo).toISOString();

const MIXED = [
  { id: "9", ts: at(1000), channel: "web", role: "lucina", text: "Web reply" },
  { id: "8", ts: at(2000), channel: "ios", role: "user", text: "From the iPhone" },
  { id: "7", ts: at(3000), channel: "alexa", role: "user", text: "From Alexa" },
  { id: "6", ts: at(4000), channel: "discord", role: "user", text: "From Discord", meta: { guildName: "Lucina HQ", channelName: "general" } },
  { id: "5", ts: at(5000), channel: "telegram", role: "user", text: "From Telegram", meta: { chatTitle: "Family" } },
  { id: "4", ts: at(6000), channel: "other", role: "lucina", text: "Water the plants.", meta: { kind: "reminder" } },
  { id: "3", ts: at(30 * H), channel: "telegram", role: "user", text: "An old one", backfilled: true },
];

describe("Chat", () => {
  beforeEach(() => reloadChatFromStorage());

  it("shows an optimistic bubble, a thinking indicator, then the reply as plain text", async () => {
    const gate = deferred<void>();
    const calls = serve(async () => {
      await gate.promise;
      return { body: { reply: "Line one\n<b>not bold</b> https://example.com", action: null, job: null } };
    });
    render(() => <ChatView />);
    const box = type("Hello Lucina");

    expect(await screen.findByText("Hello Lucina")).toBeInTheDocument();
    expect(screen.getAllByText("Lucina is thinking…").length).toBeGreaterThan(0);
    expect(box).toHaveAttribute("readonly");
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();

    gate.resolve();
    const link = await screen.findByRole("link", { name: "https://example.com" });
    expect(link).toHaveAttribute("href", "https://example.com/");
    expect(link).toHaveAttribute("rel", expect.stringContaining("noopener"));
    // Markup in replies stays text.
    expect(document.querySelector(".msg--lucina b")).toBeNull();
    expect(document.querySelector(".msg--lucina .bubble")?.textContent).toBe(
      "Line one\n<b>not bold</b> https://example.com",
    );
    // The live region announces the new reply.
    expect(document.querySelector("[aria-live=polite]")?.textContent).toContain("Lucina: Line one");

    const post = posts(calls)[0]!;
    const body = post.body as { message: string; sessionId: string; clientMessageId: string };
    expect(post.headers["x-secretary-csrf"]).toBe("1");
    expect(post.headers["x-secretary-client"]).toBe("web");
    expect(body.message).toBe("Hello Lucina");
    expect(body.sessionId).toMatch(/[0-9a-f-]{36}/);
    expect(body.clientMessageId).toMatch(/[0-9a-f-]{36}/);
    await waitFor(() => expect(box).not.toHaveAttribute("readonly"));

    // Cached for a fast first paint.
    const stored = JSON.parse(sessionStorage.getItem("secretary.timeline.v1") ?? "[]") as unknown[];
    expect(stored).toHaveLength(2);
  });

  it("Shift+Enter does not send", () => {
    const calls = serve(() => ({ body: { reply: "x", job: null } }));
    render(() => <ChatView />);
    const box = screen.getByLabelText("Message Lucina");
    fireEvent.input(box, { target: { value: "draft" } });
    fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
    expect(posts(calls)).toHaveLength(0);
  });

  it("offers retry on failure and resends with the same clientMessageId", async () => {
    let attempt = 0;
    const calls = serve(() => {
      attempt++;
      return attempt === 1
        ? { status: 502, body: { error: { code: "lucina_unreachable", message: "down" } } }
        : { body: { reply: "Back again.", action: null, job: null } };
    });
    render(() => <ChatView />);
    type("Are you there?");

    const retry = await screen.findByRole("button", { name: "Retry" });
    expect(screen.getByText("Lucina is offline. Not delivered.")).toBeInTheDocument();
    fireEvent.click(retry);

    expect(await screen.findByText("Back again.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    const ids = posts(calls).map((c) => (c.body as { clientMessageId: string }).clientMessageId);
    expect(ids[0]).toBe(ids[1]);
    expect(chat.messages.map((m) => m.role)).toEqual(["user", "lucina"]);
  });

  it("shows one bubble per message when the stream and the reply both deliver it", async () => {
    installEventSource();
    const gate = deferred<void>();
    serve(async () => {
      await gate.promise;
      return { body: { reply: "Hi there.", job: null, userItemId: "u1", lucinaItemId: "l1" } };
    });
    render(() => <ChatView />);
    const es = MockEventSource.latest();
    es.open();
    type("Hello");
    await screen.findByText("Hello");
    const cmid = chat.messages[0]!.clientMessageId!;
    es.emit("item", { id: "u1", ts: new Date().toISOString(), channel: "web", role: "user", text: "Hello", clientMessageId: cmid });
    es.emit("item", { id: "l1", ts: new Date(Date.now() + 5).toISOString(), channel: "web", role: "lucina", text: "Hi there.", replyTo: "u1" });
    gate.resolve();
    await waitFor(() => expect(screen.queryByText("Lucina is thinking…")).toBeNull());
    expect(screen.getAllByText("Hello")).toHaveLength(1);
    expect(screen.getAllByText("Hi there.")).toHaveLength(1);
    expect(document.querySelectorAll(".messages .msg")).toHaveLength(2);
  });

  it("labels every bubble with its source and context", async () => {
    serve(() => ({ status: 404 }), () => ({ body: { items: MIXED, nextBefore: null } }));
    render(() => <ChatView />);
    await screen.findByText("From Discord");
    const badge = (ch: string) => document.querySelector(`.src-badge[data-channel=${ch}]`);
    expect(badge("web")?.textContent).toBe("via Web");
    expect(badge("ios")?.textContent).toBe("via iPhone");
    expect(badge("alexa")?.textContent).toBe("via Alexa");
    expect(badge("discord")?.textContent).toBe("via Discord · #general · Lucina HQ");
    const tg = screen.getByText("From Telegram").closest(".msg")!.querySelector(".src-badge");
    expect(tg?.textContent).toBe("via Telegram · Family");
    // Each badge has a glyph, hidden from assistive tech.
    expect(badge("discord")?.querySelector("svg")).toHaveAttribute("aria-hidden", "true");
    // Your messages from any channel sit on your side.
    expect(screen.getByText("From Telegram").closest(".msg")).toHaveClass("msg--user");
  });

  it("renders reminders as system bubbles and marks imported history", async () => {
    serve(() => ({ status: 404 }), () => ({ body: { items: MIXED, nextBefore: null } }));
    render(() => <ChatView />);
    const reminder = (await screen.findByText("Water the plants.")).closest(".msg")!;
    expect(reminder).toHaveClass("msg--reminder");
    expect(reminder).not.toHaveClass("msg--lucina");
    expect(reminder.querySelector(".bubble--reminder")?.textContent).toContain("Reminder");
    expect(reminder.textContent).toContain("Reminder:");

    const old = screen.getByText("An old one").closest(".msg")!;
    expect(old).toHaveClass("msg--backfilled");
    expect(old.textContent).toContain("Imported from history");
    expect(screen.getByText("From Alexa").closest(".msg")!.textContent).not.toContain("Imported from history");
  });

  it("separates days and reveals a message's time on tap", async () => {
    serve(() => ({ status: 404 }), () => ({ body: { items: MIXED, nextBefore: null } }));
    render(() => <ChatView />);
    await screen.findByText("Web reply");
    const seps = [...document.querySelectorAll(".day-sep")].map((s) => s.textContent);
    expect(seps).toHaveLength(2);
    expect(seps[1]).toBe("Today");

    const msg = screen.getByText("From Alexa").closest(".msg")!;
    expect(msg.querySelector("time")).toHaveAttribute("datetime", MIXED[2]!.ts);
    fireEvent.click(msg.querySelector(".bubble")!);
    expect(msg).toHaveClass("msg--show-time");
  });

  it("filters by source on the client only", async () => {
    const calls = serve(() => ({ status: 404 }), () => ({ body: { items: MIXED, nextBefore: null } }));
    render(() => <ChatView />);
    await screen.findByText("Web reply");
    const fetched = calls.length;

    fireEvent.click(screen.getByRole("button", { name: "Discord" }));
    expect(screen.getByRole("button", { name: "Discord" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("From Discord")).toBeInTheDocument();
    expect(screen.queryByText("From Telegram")).toBeNull();
    expect(screen.queryByText("Web reply")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "App" }));
    expect(screen.getByText("Web reply")).toBeInTheDocument();
    expect(screen.getByText("From the iPhone")).toBeInTheDocument();
    expect(screen.queryByText("From Alexa")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "All" }));
    expect(document.querySelectorAll(".messages .msg")).toHaveLength(MIXED.length);
    expect(calls).toHaveLength(fetched);
  });

  it("loads earlier messages on request", async () => {
    const calls = serve(
      () => ({ status: 404 }),
      (c) =>
        c.url.includes("before=")
          ? { body: { items: [{ id: "1", ts: at(50 * H), channel: "discord", role: "user", text: "The very first" }], nextBefore: null } }
          : { body: { items: MIXED.slice(0, 2), nextBefore: "8" } },
    );
    render(() => <ChatView />);
    fireEvent.click(await screen.findByRole("button", { name: "Load earlier messages" }));
    expect(await screen.findByText("The very first")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/v1/timeline?before=8&limit=50")).toBe(true);
    expect(await screen.findByText("Start of the conversation")).toBeInTheDocument();
  });

  it("shows when the stream is live and when it is reconnecting", async () => {
    installEventSource();
    serve(() => ({ status: 404 }));
    render(() => <ChatView />);
    expect(screen.getByText("Connecting…")).toBeInTheDocument();
    const es = MockEventSource.latest();
    es.open();
    expect(await screen.findByText("Live")).toBeInTheDocument();
    es.emit("item", { id: "42", ts: new Date().toISOString(), channel: "telegram", role: "user", text: "Live from Telegram" });
    expect(await screen.findByText("Live from Telegram")).toBeInTheDocument();
    es.drop();
    expect(await screen.findByText("Reconnecting…")).toBeInTheDocument();
  });

  it("offers a retry when the timeline cannot load", async () => {
    let fail = true;
    serve(
      () => ({ status: 404 }),
      () => (fail ? { status: 500 } : { body: { items: MIXED.slice(0, 1), nextBefore: null } }),
    );
    render(() => <ChatView />);
    const retry = await screen.findByRole("button", { name: "Try again" });
    fail = false;
    fireEvent.click(retry);
    expect(await screen.findByText("Web reply")).toBeInTheDocument();
    expect(screen.queryByText("Couldn't load the conversation.")).toBeNull();
  });
});
