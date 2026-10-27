import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it } from "vitest";
import { putItems, readItems, setState } from "../lib/chatdb";
import type { Conversation } from "../lib/chatSync";
import { chat, clearChat, replyTarget, setChatDevice, sendMessage } from "../state/chat";
import { deferred, stubFetch } from "../test/fetch";
import ChatView from "./Chat";
import ChatsView from "./Chats";
const item = { id: "1", ts: Date.now(), channel: "web" as const, role: "lucina" as const, text: "A message from the other browser", meta: { device: "other", deviceName: "Other browser" } };
const conversation: Conversation = { key: "device:other", kind: "device", title: "Other browser", channel: "web", device: "other", count: 7, lastId: "1", lastTs: Date.now(), lastRole: "lucina", lastText: item.text };
function renderChats(path = "/chats") {
 const history = createMemoryHistory(); history.set({ value: path, replace: true, scroll: false });
 render(() => <MemoryRouter history={history}><Route path="/chats/:key?" component={ChatsView} /><Route path="/" component={() => <p>Main chat</p>} /></MemoryRouter>);
 return history;
}
beforeEach(() => { clearChat(); setChatDevice("mine"); });
describe("All chats", () => {
 it("renders cached conversations while the server is pending, then refreshes", async () => {
   await setState("conversations", [conversation]);
   const gate = deferred<void>();
   stubFetch(async () => { await gate.promise; return { body: { items: [{ ...conversation, count: 9 }] } }; });
   renderChats();
   expect(await screen.findByText("Other browser")).toBeInTheDocument();
   expect(screen.getByText("Device")).toBeInTheDocument(); expect(screen.getByText("7")).toBeInTheDocument();
   gate.resolve(); expect(await screen.findByText("9")).toBeInTheDocument();
 });
 it("marks this device and opens the main chat", async () => {
   stubFetch(() => ({ body: { items: [{ ...conversation, key: "device:mine", device: "mine" }] } }));
   const history = renderChats();
   fireEvent.click(await screen.findByRole("button", { name: /This device/ }));
   await waitFor(() => expect(history.get()).toBe("/"));
 });
 it("shows cached messages without a composer, and Continue here sets replyTo", async () => {
   await putItems([item]);
   const gate = deferred<void>();
   stubFetch(async c => c.url.includes("conversations") ? { body: { items: [conversation] } } : (await gate.promise, { body: { items: [item], cursor: "1" } }));
   const history = renderChats("/chats/device%3Aother");
   expect(await screen.findByText(item.text)).toBeInTheDocument();
   expect(screen.queryByRole("textbox")).toBeNull();
   fireEvent.click(screen.getByRole("button", { name: "Continue here" }));
   expect(replyTarget()).toEqual({ id: "1", text: item.text });
   await waitFor(() => expect(history.get()).toBe("/"));
   gate.resolve();
   const calls = stubFetch(() => ({ body: { reply: "Reply" } }));
   await sendMessage("Continuing");
   expect(calls[0]!.body).toMatchObject({ message: "Continuing", replyTo: "1" });
 });
 it("keeps agent actions read-only and explains where the conversation lives", async () => {
   const agentItem = { ...item, meta: { via: "mcp", caller: "chatgpt", format: "v1", bubbles: [{ text: "Agent response" }], actions: [{ id: "yes", kind: "reply", label: "Send answer", payload: { text: "yes" } }] } };
   stubFetch(c => ({ body: c.url.includes("conversations") ? { items: [{ ...conversation, key: "agent:chatgpt", kind: "agent", title: "ChatGPT" }] } : { items: [agentItem], cursor: "1" } }));
   renderChats("/chats/agent%3Achatgpt");
   expect(await screen.findByText("This conversation lives in ChatGPT.")).toBeInTheDocument();
   expect(screen.queryByRole("button", { name: "Continue here" })).toBeNull();
   expect(await screen.findByRole("button", { name: "Send answer" })).toBeDisabled();
 });
 it("pages cached older messages before calling the server", async () => {
   const rows = Array.from({ length: 55 }, (_, i) => ({ ...item, id: String(i + 1), ts: item.ts + i, text: `other ${i + 1}` }));
   await putItems(rows); await setState("conversation:device:other", "55");
   const calls = stubFetch(c => ({ body: c.url.includes("conversations") ? { items: [conversation] } : { items: [], cursor: "55", hasMore: false } }));
   renderChats("/chats/device%3Aother");
   await screen.findByText("other 55");
   await waitFor(() => expect(screen.getByRole("button", { name: "Load earlier messages" })).not.toBeDisabled());
   fireEvent.click(screen.getByRole("button", { name: "Load earlier messages" }));
   expect(await screen.findByText("other 1")).toBeInTheDocument();
   expect(calls.some(c => c.url.includes("before="))).toBe(false);
 });
});
it("paints cached main messages and records cache timing before sync returns", async () => {
 Reflect.deleteProperty(window, "__secretaryPerf");
 await putItems([{ ...item, meta: { device: "mine" } }]);
 const gate = deferred<void>();
 stubFetch(async () => { await gate.promise; return { body: { items: [], cursor: "1" } }; });
 render(() => <ChatView />);
 expect(await screen.findByText(item.text)).toBeInTheDocument();
 await waitFor(() => expect(window.__secretaryPerf?.firstMessageSource).toBe("cache"));
 expect((() => window.__secretaryPerf)()?.syncedMs).toBeUndefined();
 gate.resolve();
 await waitFor(() => expect(window.__secretaryPerf?.syncedMs).toBeTypeOf("number"));
 expect(chat.messages).toHaveLength(1);
 expect(await readItems("mine", "mine")).toHaveLength(1);
});
