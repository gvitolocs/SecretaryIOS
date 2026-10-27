import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import type { TimelineItem } from "../api/timeline";
import { clearChatDB, conversationKey, getState, isMine, putItems, readItems, resetScope, setState } from "./chatdb";
const item = (id: string, extra: Partial<TimelineItem> = {}): TimelineItem => ({ id, ts: Date.now(), channel: "web", role: "user", text: id, ...extra });
describe("conversation keys", () => {
 it.each([
   [{ channel: "web", meta: { via: "mcp", caller: "Claude-Code!?", device: "D" } }, "agent:claude-code"],
   [{ meta: { via: "mcp", caller: "!!" } }, "agent:mcp"],
   [{ meta: { via: "mcp" } }, "agent:mcp"],
   [{ meta: { device: "D" } }, "device:D"],
   [{ channel: "ios", meta: { device: "I" } }, "device:I"],
   [{ channel: "web" }, "legacy:web"], [{ channel: "ios" }, "legacy:ios"],
   [{ channel: "telegram" }, "telegram"], [{ channel: "discord" }, "discord"],
   [{ channel: "alexa" }, "alexa"], [{ channel: "other" }, "lucina"],
 ] as [Partial<TimelineItem>, string][]) ("maps %j to %s", (extra, key) => expect(conversationKey(item("1", extra))).toBe(key));
 it("restricts mine to this device, legacy web and lucina", () => {
   expect(isMine(item("1", { meta: { device: "D" } }), "D")).toBe(true);
   expect(isMine(item("1", { meta: { device: "D" } }))).toBe(false);
   expect(isMine(item("1", { meta: { device: "other" } }), "D")).toBe(false);
   expect(isMine(item("1"))).toBe(true);
   expect(isMine(item("1", { channel: "other" }))).toBe(true);
   expect(isMine(item("1", { channel: "ios" }), "D")).toBe(false);
 });
});
describe("IndexedDB cache", () => {
 it("upserts raw items, pages by id and isolates scopes", async () => {
   await putItems([item("2"), item("10"), item("3", { meta: { device: "D" } }), item("4", { channel: "telegram" })]);
   await putItems([item("2", { text: "edited" })]);
   expect((await readItems("mine", "D")).map(i => i.id)).toEqual(["10", "3", "2"]);
   expect((await readItems("mine", "D", "10", 1))[0]!.id).toBe("3");
   expect((await readItems("conversation:legacy:web"))[1]!.text).toBe("edited");
   await resetScope("mine", "D");
   expect(await readItems("mine", "D")).toEqual([]);
   expect(await readItems("conversation:telegram")).toHaveLength(1);
 });
 it("keeps cursor states independent and clears all local data", async () => {
   await setState("mine", "A"); await setState("conversation:telegram", "B");
   expect(await getState("mine")).toBe("A"); expect(await getState("conversation:telegram")).toBe("B");
   await putItems([item("1")]); await clearChatDB();
   expect(await getState("mine")).toBeUndefined(); expect(await readItems("mine")).toEqual([]);
 });
 it("caps the database at 5000 newest items", async () => {
   await putItems(Array.from({ length: 5005 }, (_, i) => item(String(i + 1))));
   const rows = await readItems("mine", undefined, null, 6000);
   expect(rows).toHaveLength(5000); expect(rows.at(-1)!.id).toBe("6");
 });
 it("works in memory when IndexedDB cannot open", async () => {
   vi.resetModules(); vi.stubGlobal("indexedDB", undefined);
   const fallback = await import("./chatdb");
   await fallback.putItems([item("1")]); await fallback.setState("mine", "1");
   expect(await fallback.readItems("mine")).toHaveLength(1); expect(await fallback.getState("mine")).toBe("1");
   await fallback.clearChatDB(); expect(await fallback.readItems("mine")).toEqual([]);
 });
});
