import { createMemoryHistory, MemoryRouter, Route, useNavigate } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import type { JSX } from "solid-js";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { chat, loadLatest, reloadChatFromStorage } from "../state/chat";
import { installEventSource, MockEventSource } from "../test/eventsource";
import { deferred, stubFetch, type Call, type Handler } from "../test/fetch";
import ChatView from "./Chat";

const EMPTY_PAGE = { body: { items: [], nextBefore: null } };

function renderAtWithHistory(path: string, routes: () => JSX.Element) {
  const history = createMemoryHistory();
  history.set({ value: path, scroll: false, replace: true });
  const r = render(() => <MemoryRouter history={history}>{routes()}</MemoryRouter>);
  return { ...r, history };
}

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

  it("pins cached messages through late growth, ignores the initial top, preserves older messages and handles latest taps", async () => {
    installEventSource();
    let height = 1200;
    let resized: (() => void) | undefined;
    const originalObserver = globalThis.ResizeObserver;
    globalThis.ResizeObserver = class {
      constructor(callback: ResizeObserverCallback) { resized = () => callback([], this); }
      observe() {}
      unobserve() {}
      disconnect() {}
    };
    const heightSpy = vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(function (this: HTMLElement) {
      return this.classList.contains("messages") ? height : 0;
    });
    const clientSpy = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(400);
    let olderCalls = 0;
    const gate = deferred<{ body: { items: typeof MIXED; nextBefore: null } }>();
    serve(() => ({ body: {} }), (c) => {
      if (c.url.includes("before=")) {
        olderCalls++;
        return gate.promise;
      }
      return { body: { items: MIXED, nextBefore: "3" } };
    });
    try {
      await loadLatest();
      render(() => <ChatView />);
      const log = screen.getByRole("log");
      let top = 0;
      log.scrollTo = (options?: ScrollToOptions | number) => {
        log.scrollTop = typeof options === "number" ? options : options?.top ?? 0;
      };
      Object.defineProperty(log, "scrollTop", {
        configurable: true,
        get: () => top,
        set: (value: number) => { top = Math.min(value, height - 400); },
      });
      fireEvent.scroll(log);
      expect(olderCalls).toBe(0);
      await waitFor(() => expect(top).toBe(800));
      height = 1500;
      resized?.();
      expect(top).toBe(1100);
      // Even a small upward scroll releases the pin before content grows again.
      fireEvent.scroll(log);
      fireEvent.wheel(log, { deltaY: -50 });
      top = 1050;
      fireEvent.scroll(log);
      height = 1600;
      resized?.();
      expect(top).toBe(1050);
      top = 0;
      fireEvent.scroll(log);
      await waitFor(() => expect(olderCalls).toBe(1));
      height = 1800;
      resized?.();
      expect(top).toBe(0);
      gate.resolve({ body: { items: [{ ...MIXED[0]!, id: "1", ts: at(40 * H), text: "Earlier page" }], nextBefore: null } });
      await screen.findByText("Earlier page");
      await waitFor(() => expect(top).toBe(200));
      window.dispatchEvent(new CustomEvent("secretary:chat-latest"));
      expect(top).toBe(1400);
      window.dispatchEvent(new CustomEvent("secretary:chat-latest"));
      expect(top).toBe(1400);
    } finally {
      heightSpy.mockRestore();
      clientSpy.mockRestore();
      globalThis.ResizeObserver = originalObserver;
    }
  });

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
    // Markup in replies stays text (her replies go through the safe markdown renderer).
    expect(document.querySelector(".msg--lucina b")).toBeNull();
    const bubble = document.querySelector(".msg--lucina .bubble")!;
    expect(bubble.textContent).toBe("Line one<b>not bold</b> https://example.com");
    expect(bubble.querySelector("br")).not.toBeNull();
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

    expect(sessionStorage.getItem("secretary.timeline.v1")).toBeNull();
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

  it("labels MCP turns and replies, strips legacy prefixes, and filters Agents separately", async () => {
    const items = [
      ...MIXED,
      { id: "m1", ts: at(10), channel: "web", role: "user", text: "Agent question", meta: { via: "mcp", caller: "claude-code" } },
      { id: "m2", ts: at(9), channel: "web", role: "lucina", text: "Agent answer", meta: { via: "mcp", caller: "www" } },
      { id: "m3", ts: at(8), channel: "web", role: "user", text: "[via Lucina MCP, from claude-code] Legacy question" },
    ];
    serve(() => ({ status: 404 }), () => ({ body: { items, nextBefore: null } }));
    render(() => <ChatView />);
    await screen.findByText("Legacy question");
    expect(screen.queryByText(/\[via Lucina MCP/)).toBeNull();
    const badge = screen.getByText("Agent answer").closest(".msg")!.querySelector(".src-badge")!;
    expect(badge).toHaveAttribute("data-channel", "mcp");
    expect(badge.textContent).toBe("via Cursor");
    expect(badge.querySelector("svg")).toHaveAttribute("aria-hidden", "true");
    expect(screen.getByText("Legacy question").closest(".msg")!.querySelector(".src-badge")!.textContent).toBe("via Claude Code");

    fireEvent.click(screen.getByRole("button", { name: "App" }));
    expect(screen.queryByText("Agent question")).toBeNull();
    expect(screen.queryByText("Agent answer")).toBeNull();
    expect(screen.queryByText("Legacy question")).toBeNull();
    expect(screen.getByText("Web reply")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Agents" }));
    expect(screen.getByText("Agent question")).toBeInTheDocument();
    expect(screen.getByText("Agent answer")).toBeInTheDocument();
    expect(screen.getByText("Legacy question")).toBeInTheDocument();
    expect(screen.queryByText("Web reply")).toBeNull();
    expect(document.querySelectorAll(".messages .msg")).toHaveLength(3);
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
    expect(calls.some((c) => c.url === "/v1/timeline?before=8&limit=50&scope=mine")).toBe(true);
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
    es.emit("item", { id: "42", ts: new Date().toISOString(), channel: "web", role: "user", text: "Live from this device" });
    expect(await screen.findByText("Live from this device")).toBeInTheDocument();
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

  describe("notes in the conversation", () => {
    it("cites [note:<slug>] as a link into Keep and leaves the rest of the text", async () => {
      serve(() => ({ status: 404 }), () => ({
        body: { items: [{ id: "30", ts: at(500), channel: "web", role: "lucina", text: "Done, it is in [note:gift-ideas] with **bold** words.", meta: {} }], nextBefore: null },
      }));
      render(() => <ChatView />);
      const link = await screen.findByRole("link", { name: "note:gift-ideas" });
      expect(link).toHaveAttribute("href", "/keep/note/gift-ideas");
      expect(link.closest(".bubble")).toHaveTextContent("Done, it is in note:gift-ideas with bold words.");
    });

    it("shows an Open note link under a message with meta.kind note and a slug", async () => {
      serve(() => ({ status: 404 }), () => ({
        body: {
          items: [
            { id: "32", ts: at(400), channel: "web", role: "lucina", text: "Saved.", meta: { kind: "note", slug: "rome-weekend" } },
            { id: "31", ts: at(500), channel: "web", role: "lucina", text: "No slug here.", meta: { kind: "note", slug: "Not A Slug!" } },
          ],
          nextBefore: null,
        },
      }));
      render(() => <ChatView />);
      const link = await screen.findByRole("link", { name: "Open note" });
      expect(link).toHaveAttribute("href", "/keep/note/rome-weekend");
      expect(screen.getAllByRole("link", { name: "Open note" })).toHaveLength(1);
    });
  });

  describe("Lucina reached out", () => {
    const ID = "po_01K85Z3Q4X5Y6Z7A8B9C0D1E2F";
    const PROACTIVE = {
      id: "20",
      ts: at(500),
      channel: "other",
      role: "lucina",
      text: "CRAFT-IT4SD starts in an hour. I put a short prep in your notes.",
      meta: {
        kind: "proactive",
        topic: "meeting_prep",
        priority: "high",
        outreachId: ID,
        why: "starts in 60 min; 1 note · score 0.83 ≥ send 0.70",
        actions: [
          { id: "open_note", label: "Open the note", route: "/notes/prep-20261022-craft-it4sd" },
          { id: "useful", label: "Useful", feedback: true },
          { id: "not_now", label: "Not now", feedback: true },
          { id: "stop", label: "Stop these", feedback: true },
        ],
      },
    };

    it("renders a card with priority, a content link, feedback and the why", async () => {
      const calls = serve(
        (c) => (c.url.startsWith("/v1/proactive/") ? { status: 201, body: { feedback: { id: "fb_1", outreachId: ID, action: "not_now", at: 1 } } } : { status: 404 }),
        () => ({ body: { items: [PROACTIVE, ...MIXED], nextBefore: null } }),
      );
      render(() => <ChatView />);
      const card = await screen.findByRole("region", { name: "Lucina reached out" });
      expect(card).toHaveTextContent("Important");
      expect(card).toHaveTextContent("CRAFT-IT4SD starts in an hour.");
      expect(screen.getByRole("link", { name: "Open the note" })).toHaveAttribute("href", "/notes/prep-20261022-craft-it4sd");
      // Not a plain bubble.
      expect(document.querySelector(".msg--proactive .bubble")).toBeNull();

      expect(screen.queryByText(/score 0.83/)).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "Why?" }));
      expect(screen.getByText(/score 0.83/)).toBeInTheDocument();

      fireEvent.click(screen.getByRole("button", { name: "Not now" }));
      expect(await screen.findByText("Got it: not now.")).toBeInTheDocument();
      const post = calls.find((c) => c.url === `/v1/proactive/outreach/${ID}/feedback`)!;
      expect(post.method).toBe("POST");
      expect(post.body).toEqual({ action: "not_now" });
      expect(post.headers["x-secretary-csrf"]).toBe("1");
      for (const name of ["Useful", "Not now", "Stop these"]) {
        expect(screen.getByRole("button", { name })).toBeDisabled();
      }
    });

    it("shows the event's place under a calendar reminder", async () => {
      serve(
        () => ({ status: 404 }),
        () => ({
          body: {
            items: [
              {
                ...PROACTIVE,
                meta: {
                  ...PROACTIVE.meta,
                  place: { title: "CRAFT-IT4SD", address: "Folkehuset Møllestien, Aarhus", lat: 56.1611, lng: 10.2006, kind: "event" },
                },
              },
            ],
            nextBefore: null,
          },
        }),
      );
      render(() => <ChatView />);
      const card = await screen.findByRole("region", { name: "Lucina reached out" });
      const place = card.querySelector<HTMLAnchorElement>("a.place-card")!;
      expect(place).toHaveTextContent("Folkehuset Møllestien, Aarhus");
      expect(place.getAttribute("href")).toMatch(/^\/map\/place\?lat=56\.1611&lng=10\.2006&title=CRAFT-IT4SD/);
    });

    it("shows a failed tap and keeps the buttons", async () => {
      serve(
        () => ({ status: 500, body: { error: { code: "internal", message: "x" } } }),
        () => ({ body: { items: [PROACTIVE], nextBefore: null } }),
      );
      render(() => <ChatView />);
      fireEvent.click(await screen.findByRole("button", { name: "Stop these" }));
      expect(await screen.findByText("Couldn't send that. Try again.")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Stop these" })).not.toBeDisabled();
    });

    it("the App filter keeps her proactive messages", async () => {
      serve(() => ({ status: 404 }), () => ({ body: { items: [PROACTIVE, ...MIXED], nextBefore: null } }));
      render(() => <ChatView />);
      await screen.findByRole("region", { name: "Lucina reached out" });
      fireEvent.click(screen.getByRole("button", { name: "App" }));
      expect(screen.getByRole("region", { name: "Lucina reached out" })).toBeInTheDocument();
      expect(screen.queryByText("From Telegram")).toBeNull();
    });

    it("a deep link focuses the message and leaves the address clean", async () => {
      window.history.pushState({}, "", `/?outreach=${ID}`);
      serve(() => ({ status: 404 }), () => ({ body: { items: [PROACTIVE, ...MIXED], nextBefore: null } }));
      render(() => <ChatView />);
      await screen.findByRole("region", { name: "Lucina reached out" });
      await waitFor(() => expect(document.querySelector(".msg--focus")).not.toBeNull());
      expect(window.location.search).toBe("");
    });

    it("a deep link to a message that is not loaded still takes feedback", async () => {
      window.history.pushState({}, "", `/?outreach=${ID}`);
      const calls = serve(
        () => ({ status: 201, body: { feedback: { id: "fb_2", outreachId: ID, action: "useful", at: 1 } } }),
        () => ({ body: { items: MIXED, nextBefore: null } }),
      );
      render(() => <ChatView />);
      expect(await screen.findByText(/isn't in the loaded conversation/)).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Useful" }));
      expect(await screen.findByText("Thanks, noted.")).toBeInTheDocument();
      expect(calls.some((c) => c.url === `/v1/proactive/outreach/${ID}/feedback`)).toBe(true);
      fireEvent.click(screen.getByRole("button", { name: "Close" }));
      expect(screen.queryByText(/isn't in the loaded conversation/)).toBeNull();
      window.history.pushState({}, "", "/");
    });
  });
});

describe("Lucina reached out, inside the router", () => {
  beforeEach(() => reloadChatFromStorage());

  it("a notification click while the chat is open focuses the message", async () => {
    const ID = "po_01K85Z3Q4X5Y6Z7A8B9C0D1E2F";
    const item = {
      id: "21",
      ts: new Date().toISOString(),
      channel: "other",
      role: "lucina",
      text: "pokoin.com is back up.",
      meta: { kind: "proactive", priority: "normal", outreachId: ID, actions: [] },
    };
    serve(() => ({ status: 404 }), () => ({ body: { items: [item], nextBefore: null } }));
    let go: ((to: string) => void) | undefined;
    function Probe() {
      go = useNavigate();
      return null;
    }
    const { history } = renderAtWithHistory("/", () => (
      <Route
        path="/"
        component={() => (
          <>
            <Probe />
            <ChatView />
          </>
        )}
      />
    ));
    await screen.findByRole("region", { name: "Lucina reached out" });
    expect(document.querySelector(".msg--focus")).toBeNull();
    go!(`/?outreach=${ID}`);
    await waitFor(() => expect(document.querySelector(".msg--focus")).not.toBeNull());
    await waitFor(() => expect(history.get()).toBe("/"));
  });
});
