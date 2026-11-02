// A decision card closes on every client when the server says so, however it was answered.
import { MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { reloadChatFromStorage } from "../state/chat";
import { installEventSource, MockEventSource } from "../test/eventsource";
import { stubFetch } from "../test/fetch";
import ChatView from "./Chat";

const ITEM = {
  id: "01M6CC55XHF1CBKXG1AA1P5A4B",
  ts: new Date().toISOString(),
  channel: "web",
  role: "lucina",
  text: "Deploy?",
  meta: {
    kind: "decision",
    decisionId: "dc-0123456789ab",
    format: "v1",
    bubbles: ["Deploy?"],
    actions: [
      { id: "dc-0123456789ab-1", label: "1. Yes", kind: "reply", payload: { text: "Decision dc-0123456789ab: 1. Yes" } },
      { id: "dc-0123456789ab-2", label: "2. No", kind: "reply", payload: { text: "Decision dc-0123456789ab: 2. No" } },
    ],
  },
};

function setup(cards: () => unknown) {
  installEventSource();
  const calls = stubFetch((c) => {
    if (c.url === "/v1/timeline/cards" && c.method === "GET") return { body: { cards: cards() } };
    if (c.url.startsWith("/v1/timeline/cards/")) return { body: { closed: true, status: "chosen", action: (c.body as { action: string }).action } };
    if (c.url.startsWith("/v1/timeline")) return { body: { items: [ITEM], nextBefore: null } };
    if (c.url === "/v1/secrets/requests") return { body: { requests: [], active: null, queued: 0 } };
    return { body: {} };
  });
  render(() => (
    <MemoryRouter>
      <Route path="/" component={ChatView} />
    </MemoryRouter>
  ));
  return calls;
}

const button = async (name: RegExp) => (await screen.findByRole("button", { name })) as HTMLButtonElement;

describe("closed cards", () => {
  beforeEach(() => {
    localStorage.clear();
    reloadChatFromStorage();
  });

  it("turns the buttons off when the stream says a decision was decided elsewhere", async () => {
    let cards: unknown = {};
    const calls = setup(() => cards);
    const yes = await button(/1\. Yes/);
    expect(yes.disabled).toBe(false);

    cards = { [ITEM.id]: { closed: true, status: "decided", action: "dc-0123456789ab-2" } };
    MockEventSource.latest().emit("cards", {});
    await waitFor(() => expect(yes.disabled).toBe(true));
    expect((await button(/2\. No/)).disabled).toBe(true);
    expect(calls.some((c) => c.url === "/v1/secrets/requests")).toBe(true);
  });

  it("turns them off for a withdrawn decision, which has no button to mark", async () => {
    setup(() => ({ [ITEM.id]: { closed: true, status: "withdrawn" } }));
    const yes = await button(/1\. Yes/);
    await waitFor(() => expect(yes.disabled).toBe(true));
  });

  it("tells the server which button was used here", async () => {
    const calls = setup(() => ({}));
    const yes = await button(/1\. Yes/);
    fireEvent.click(yes);
    await waitFor(() => {
      const posted = calls.find((c) => c.url === `/v1/timeline/cards/${ITEM.id}`);
      expect(posted?.body).toEqual({ action: "dc-0123456789ab-1" });
    });
  });
});
