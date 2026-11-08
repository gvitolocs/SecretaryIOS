import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { stubWide } from "../test/wide";
import { deferred, stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { putItems, setState } from "../lib/chatdb";
import type { Conversation } from "../lib/chatSync";
import { clearChat, replyTarget, setChatDevice } from "../state/chat";
import Chats from "./Chats";
const item = { id: "1", ts: Date.now(), channel: "web" as const, role: "lucina" as const, text: "Hello from the other browser", meta: { device: "other" } };
const conversation: Conversation = { key: "device:other", kind: "device", title: "Other browser", channel: "web", count: 7, lastId: "1", lastTs: Date.now(), lastRole: "lucina", lastText: item.text };
const routes = () => <><Route path="/chats/:key?" component={Chats} /><Route path="/" component={() => <p>Main chat</p>} /></>;
beforeEach(() => { stubWide(); clearChat(); setChatDevice("mine"); });
afterEach(() => vi.unstubAllGlobals());
function frame(container: HTMLElement) {
  expect(screen.getByRole("heading", { level: 1, name: "All chats" })).toHaveClass("pg-title");
  expect(container.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
  expect(container.querySelector(".chats-page, .conversation-row, .conversation-kind, .pg-seg")).toBeNull();
}
it("shows cached rows under a persistent header while loading", async () => {
  await setState("conversations", [conversation]);
  const gate = deferred<void>();
  stubFetch(async () => { await gate.promise; return { body: { items: [conversation] } }; });
  const { container } = renderAt("/chats", routes);
  frame(container);
  expect(container.querySelector(".pg-grid--single")).toBeInTheDocument();
  await screen.findByRole("link", { name: "Other browser" });
  expect(screen.getByText("Conversations: 1")).toHaveClass("pg-context");
  expect(screen.getByRole("list", { name: "All chats" }).querySelectorAll(".pg-row")).toHaveLength(1);
  expect(screen.getByLabelText("7 messages")).toHaveTextContent("7");
  expect(screen.getByText("Device")).toHaveClass("pg-badge");
  expect(screen.getByRole("status")).toHaveClass("pg-loading");
  gate.resolve();
  await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
});
it("retries a failed list and renders the empty card", async () => {
  let fail = true;
  stubFetch(() => fail ? { status: 500, body: {} } : { body: { items: [] } });
  const { container } = renderAt("/chats", routes);
  expect(await screen.findByRole("alert")).toHaveClass("pg-error");
  frame(container);
  fail = false;
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect((await screen.findByText("No conversations yet.")).closest(".pg-empty")).toBeInTheDocument();
});
it("uses split layout with a selected list link and read-only conversation", async () => {
  await putItems([item]);
  stubFetch(c => ({ body: c.url.includes("conversations") ? { items: [conversation] } : { items: [item], cursor: "1" } }));
  const { container } = renderAt("/chats/device%3Aother", routes);
  await screen.findByRole("heading", { level: 2, name: "Other browser" });
  frame(container);
  expect(container.querySelector(".pg-grid--split")).toBeInTheDocument();
  const nav = screen.getByRole("navigation", { name: "All chats" });
  expect(within(nav).getByRole("link", { name: "Other browser" })).toHaveAttribute("aria-current", "page");
  const detail = screen.getByRole("region", { name: "Conversation with Lucina" });
  expect(await within(detail).findByText(item.text)).toBeInTheDocument();
  expect(screen.queryByRole("textbox")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Continue here" }));
  expect(replyTarget()).toEqual({ id: "1", text: item.text });
  await screen.findByText("Main chat");
});
it("shows an empty conversation and its agent location", async () => {
  stubFetch(c => ({ body: c.url.includes("conversations") ? { items: [{ ...conversation, key: "agent:codex", kind: "agent", title: "Codex" }] } : { items: [], cursor: null } }));
  renderAt("/chats/agent%3Acodex", routes);
  await screen.findByText("This conversation lives in Codex.");
  await screen.findByText("No messages in this conversation yet.");
  expect(screen.queryByRole("button", { name: "Continue here" })).toBeNull();
});
