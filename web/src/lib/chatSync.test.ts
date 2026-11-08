import { describe, expect, it } from "vitest";
import { stubFetch } from "../test/fetch";
import { getState, putItems, readItems, setState } from "./chatdb";
import { syncScope } from "./chatSync";
const item = (id: string) => ({ id, ts: Date.now(), role: "user" as const, channel: "web" as const, text: id });
describe("scoped timeline sync", () => {
 it("loads a fresh mine page and saves its cursor", async () => {
   const calls = stubFetch(() => ({ body: { items: [item("2")], cursor: "2", nextBefore: "2", hasMore: false } }));
   const result = await syncScope("mine");
   expect(calls[0]!.url).toBe("/v1/timeline?limit=50&scope=mine");
   expect(result.items.map(i => i.id)).toEqual(["2"]);
   expect(await getState("mine")).toBe("2");
 });
 it("drains after pages with the returned cursor and stores every item", async () => {
   await setState("conversation:agent:chatgpt", "1");
   const calls = stubFetch(c => ({ body: c.url.endsWith("after=1")
     ? { items: [item("2")], cursor: "2", hasMore: true }
     : { items: [item("3")], cursor: "3", hasMore: false } }));
   const result = await syncScope("conversation:agent:chatgpt");
   expect(calls.map(c => c.url)).toEqual(["/v1/timeline?limit=50&conversation=agent%3Achatgpt&after=1", "/v1/timeline?limit=50&conversation=agent%3Achatgpt&after=2"]);
   expect(result.items.map(i => i.id)).toEqual(["2", "3"]);
   expect(await getState("conversation:agent:chatgpt")).toBe("3");
 });
 it("resets only mine after ten incremental calls", async () => {
   await putItems([item("1"), { ...item("other"), channel: "telegram" }]);
   await setState("mine", "1");
   let n = 1;
   const calls = stubFetch(c => ({ body: c.url.includes("after=")
     ? { items: [item(String(++n))], cursor: String(n), hasMore: true }
     : { items: [item("99")], cursor: "99", hasMore: false } }));
   expect((await syncScope("mine")).reset).toBe(true);
   expect(calls).toHaveLength(11);
   expect(calls[10]!.url).not.toContain("after=");
   expect((await readItems("mine")).map(i => i.id)).toEqual(["99"]);
   expect(await readItems("conversation:telegram")).toHaveLength(1);
 });
 it("forgets an expired cursor and replaces the scoped cache", async () => {
   await putItems([item("1")]); await setState("mine", "expired");
   const calls = stubFetch(c => c.url.includes("after=") ? { status: 410, body: { error: { code: "cursor_expired" } } } : { body: { items: [item("8")], cursor: "8" } });
   expect((await syncScope("mine")).reset).toBe(true);
   expect(calls).toHaveLength(2);
   expect(await getState("mine")).toBe("8");
   expect((await readItems("mine")).map(i => i.id)).toEqual(["8"]);
 });
 it("does not store responses from an invalidated session", async () => {
   stubFetch(() => ({ body: { items: [item("1")], cursor: "1" } }));
   await syncScope("mine", undefined, () => false);
   expect(await readItems("mine")).toEqual([]);
 });
});
