import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { LinkAction, LinksOverview } from "../api/links";
import { setLocale } from "../i18n";
import { nav } from "../lib/navigate";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import LinksView from "./Links";

function overview(over: Partial<LinksOverview> = {}): LinksOverview {
  return {
    timeZone: "Europe/Rome",
    google: { configured: true, status: "connected", account: "owner@example.com", services: ["calendar"] },
    groups: [
      {
        id: "calendars",
        cards: [
          {
            id: "google-calendar",
            kind: "google",
            service: "calendar",
            name: "Google Calendar",
            status: "connected",
            account: "owner@example.com",
            scopes: ["calendar.events", "calendar.readonly"],
            lastSyncAt: new Date(Date.now() - 120_000).toISOString(),
            count: 42,
            actions: ["sync", "disconnect"],
          },
          { id: "iphone-calendar", kind: "iphone", name: "iPhone Calendar", status: "disconnected", actions: [] },
          {
            id: "ics:ics_1",
            kind: "ics",
            name: "Serie A",
            status: "connected",
            account: "calendar.example",
            count: 38,
            actions: ["sync", "remove"],
          },
        ],
      },
      {
        id: "channels",
        cards: [
          { id: "channel:telegram", kind: "channel", name: "Telegram", status: "connected", actions: [] },
          { id: "channel:alexa", kind: "channel", name: "Alexa", status: "planned", actions: [] },
        ],
      },
      { id: "ai", cards: [{ id: "ai:claude-code", kind: "ai", name: "Claude Code", status: "connected", scopes: ["chat", "read"], actions: [] }] },
      {
        id: "google",
        cards: [{ id: "google-gmail", kind: "google", service: "gmail", name: "Gmail", status: "available", actions: ["connect"] }],
      },
      {
        id: "custom",
        cards: [
          {
            id: "custom:lnk_1",
            kind: "bookmark",
            name: "Bank",
            status: "connected",
            url: "https://bank.example",
            tags: ["money"],
            actions: ["edit", "remove"],
          },
        ],
      },
    ],
    proposals: [
      {
        id: "prop_1",
        event: { title: "Padel", start: "2026-11-07T10:00:00+01:00" },
        from: "mcp:claude-code",
        createdAt: "2026-10-22T10:00:00Z",
        expiresAtMs: Date.now() + 1e9,
      },
    ],
    ...over,
  };
}

const EVENTS = {
  from: "x",
  to: "y",
  timeZone: "Europe/Rome",
  events: [
    {
      id: "google:a",
      source: "google",
      sourceId: "a",
      calendar: "Google",
      title: "Dentist",
      start: "2026-10-23T07:30:00.000Z",
      end: "2026-10-23T08:00:00.000Z",
      allDay: false,
      startMs: Date.UTC(2026, 9, 23, 7, 30),
      endMs: Date.UTC(2026, 9, 23, 8),
      recurring: false,
      editable: true,
    },
  ],
};

function server(over: Partial<LinksOverview> = {}) {
  return stubFetch((c: Call) => {
    const path = c.url.split("?")[0];
    if (path === "/v1/links") return { body: overview(over) };
    if (path === "/v1/calendar/events") return { body: EVENTS };
    if (path === "/v1/links/google/connect") return { body: { authorizeUrl: "https://accounts.google.com/o/oauth2/v2/auth?x=1" } };
    if (path === "/v1/links/ics") return { status: 201, body: { id: "ics_2", label: "x", host: "h", eventCount: 3, feedName: null } };
    if (path === "/v1/links/custom") return { status: 201, body: {} };
    if (path === "/v1/calendar/sync") return { body: { google: { ok: true }, ics: [] } };
    if (path === "/v1/links/todoist/token") {
      const token = (c.body as { token: string }).token;
      return token === "good-token-0123456789abcdef"
        ? { body: { connected: true, account: "owner@example.com" } }
        : { status: 422, body: { error: { code: "todoist_token_rejected", message: "Todoist did not accept that token" } } };
    }
    if (path === "/v1/links/todoist/connect") return { body: { authorizeUrl: "https://app.todoist.com/oauth/authorize?client_id=x" } };
    return { body: { ok: true } };
  });
}

const show = (path = "/links") => renderAt(path, () => <Route path="/links" component={LinksView} />);

describe("Links", () => {
  afterEach(() => vi.restoreAllMocks());

  it("renders groups, statuses, scopes and the upcoming events", async () => {
    server();
    show();
    await screen.findByText("Google Calendar");
    expect(screen.getByRole("heading", { name: "Calendars" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "AI clients" })).toBeInTheDocument();
    expect(screen.getByText("owner@example.com")).toBeInTheDocument();
    expect(screen.getByText("calendar.events")).toBeInTheDocument();
    expect(screen.getByText("42 items synced")).toBeInTheDocument();
    expect(screen.getByText("Open Secretary on your iPhone and allow Calendar access.")).toBeInTheDocument();
    expect(screen.getByText("Planned")).toBeInTheDocument();
    expect(await screen.findByText("Dentist")).toBeInTheDocument();
    const bank = screen.getByText("Bank").closest("li") as HTMLElement;
    expect(within(bank).getByRole("link", { name: "Open" })).toHaveAttribute("href", "https://bank.example");
    expect(within(bank).getByText("#money")).toBeInTheDocument();
  });

  it("connects Google through the consent URL", async () => {
    const calls = server();
    const assign = vi.spyOn(nav, "assign").mockImplementation(() => {});
    show();
    const gmail = (await screen.findByText("Gmail")).closest("li") as HTMLElement;
    fireEvent.click(within(gmail).getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("https://accounts.google.com/o/oauth2/v2/auth?x=1"));
    const post = calls.find((c) => c.url === "/v1/links/google/connect");
    expect(post?.body).toEqual({ services: ["gmail"], client: "web" });
    expect(post?.headers["x-secretary-csrf"]).toBe("1");
  });

  it("asks before disconnecting and removing", async () => {
    const calls = server();
    show();
    const card = (await screen.findByText("Google Calendar")).closest("li") as HTMLElement;
    fireEvent.click(within(card).getByRole("button", { name: "Disconnect" }));
    expect(calls.some((c) => c.method === "DELETE")).toBe(false);
    fireEvent.click(within(card).getByRole("button", { name: "Yes, disconnect" }));
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE" && c.url === "/v1/links/google")).toBe(true));
    const ics = screen.getByText("Serie A").closest("li") as HTMLElement;
    fireEvent.click(within(ics).getByRole("button", { name: "Remove" }));
    fireEvent.click(within(ics).getByRole("button", { name: "Yes, remove" }));
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE" && c.url === "/v1/links/ics/ics_1")).toBe(true));
  });

  it("subscribes to an ICS calendar and adds a link", async () => {
    const calls = server();
    show();
    await screen.findByText("Google Calendar");
    const ics = screen.getByRole("form", { name: "Subscribe to a calendar" });
    fireEvent.input(within(ics).getByLabelText("iCal address"), { target: { value: "webcal://cal.example/x.ics" } });
    fireEvent.input(within(ics).getByLabelText("Name"), { target: { value: "Calcio" } });
    fireEvent.submit(ics);
    await screen.findByText("Subscribed: 3 events.");
    expect(calls.find((c) => c.url === "/v1/links/ics")?.body).toEqual({ label: "Calcio", url: "webcal://cal.example/x.ics" });

    const form = screen.getByRole("form", { name: "Add a link" });
    fireEvent.click(within(form).getByRole("button", { name: "MCP server" }));
    fireEvent.input(within(form).getByLabelText("Label"), { target: { value: "Notion" } });
    fireEvent.input(within(form).getByLabelText("Address"), { target: { value: "https://mcp.notion.example/mcp" } });
    fireEvent.input(within(form).getByLabelText(/Tags/), { target: { value: "work, notes" } });
    fireEvent.submit(form);
    await screen.findByText("Link added.");
    expect(calls.find((c) => c.url === "/v1/links/custom")?.body).toEqual({
      kind: "mcp",
      label: "Notion",
      url: "https://mcp.notion.example/mcp",
      tags: ["work", "notes"],
    });
  });

  it("accepts a proposal with confirm: true", async () => {
    const calls = server();
    show();
    const region = await screen.findByRole("region", { name: "Waiting for your OK" });
    expect(within(region).getByText("Proposed by mcp:claude-code")).toBeInTheDocument();
    fireEvent.click(within(region).getByRole("button", { name: "Add to calendar" }));
    await screen.findByText("Added to your calendar.");
    const accept = calls.find((c) => c.url === "/v1/calendar/proposals/prop_1/accept");
    expect(accept?.body).toEqual({ confirm: true });
  });

  it("shows the reconnect state and the OAuth return, in Italian", async () => {
    setLocale("it");
    const o = overview();
    const cal = o.groups[0]!;
    cal.cards[0] = { ...cal.cards[0]!, status: "reconnect", actions: ["reconnect", "disconnect"] };
    server(o);
    show("/links?google=error&reason=access_denied");
    expect(await screen.findByText("L'accesso a Google non è stato completato (access_denied).")).toBeInTheDocument();
    expect(await screen.findByText("Da ricollegare")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Ricollega" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Calendari" })).toBeInTheDocument();
  });

  it("connects Todoist with a pasted API token (never shown again) or OAuth", async () => {
    const todoist = {
      id: "tasks",
      cards: [
        {
          id: "todoist",
          kind: "todoist",
          name: "Todoist",
          status: "disconnected",
          oauth: true,
          scopes: ["data:read_write"],
          actions: ["connect", "token"] as LinkAction[],
        },
      ],
    };
    const calls = server({ groups: [todoist] });
    show();
    expect(await screen.findByRole("heading", { name: "Tasks" })).toBeInTheDocument();
    expect(screen.getByText(/Connect with Todoist, or paste the API token/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Paste API token" }));
    const input = await screen.findByLabelText("Todoist API token");
    expect(input).toHaveAttribute("type", "password");
    const form = screen.getByRole("form", { name: "Paste API token" });
    fireEvent.input(input, { target: { value: "bad-token-0123456789abcdef" } });
    fireEvent.click(within(form).getByRole("button", { name: "Connect" }));
    expect(await screen.findByText(/didn't accept that token/)).toBeInTheDocument();
    fireEvent.input(input, { target: { value: "good-token-0123456789abcdef" } });
    fireEvent.click(within(form).getByRole("button", { name: "Connect" }));
    expect(await screen.findByText("Todoist is connected (owner@example.com). Importing your tasks…")).toBeInTheDocument();
    const posts = calls.filter((c) => c.url === "/v1/links/todoist/token");
    expect(posts.at(-1)?.body).toEqual({ token: "good-token-0123456789abcdef" });
    expect(posts.at(-1)?.headers["x-secretary-csrf"]).toBe("1");

    const assign = vi.spyOn(nav, "assign").mockImplementation(() => {});
    const card = screen.getByText("Todoist").closest("li") as HTMLElement;
    fireEvent.click(within(card).getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(assign).toHaveBeenCalledWith("https://app.todoist.com/oauth/authorize?client_id=x"));
  });
});

describe("Links desktop page", () => {
  afterEach(() => vi.unstubAllGlobals());
  it("keeps the header while loading, with proposals and agenda beside source lists", async () => {
    stubWide();
    server({ proposals: [] });
    show();
    expect(
      screen.getByRole("heading", { level: 1, name: "Links" }),
    ).toBeInTheDocument();
    const proposals = await screen.findByRole("region", {
      name: "Waiting for your OK",
    });
    expect(
      within(proposals).getByText(
        "There are no proposals waiting for your OK.",
      ),
    ).toBeInTheDocument();
    expect(await screen.findByText("Dentist")).toBeInTheDocument();
    expect(
      screen
        .getByRole("region", { name: "Calendars" })
        .querySelector(".pg-rows"),
    ).toBeInTheDocument();
    expect(document.querySelector(".pg-grid--rail")).toBeInTheDocument();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(1);
    expect(
      document.querySelector(".links-head, .links-grid, .view-title, .pg-seg"),
    ).toBeNull();
    fireEvent.click(
      screen.getByRole("button", { name: "Subscribe to a calendar" }),
    );
    expect(
      screen.getByRole("dialog", { name: "Subscribe to a calendar" }),
    ).toBeInTheDocument();
  });
  it("shows one retry state under the title on load failure", async () => {
    stubWide();
    stubFetch(() => ({ status: 503, body: { error: "Offline" } }));
    show();
    expect(await screen.findByRole("alert")).toHaveClass("pg-error");
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(
      "Links",
    );
    expect(document.querySelectorAll(".pg-error")).toHaveLength(1);
  });
});
