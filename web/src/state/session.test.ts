import { describe, expect, it } from "vitest";
import { pageCache } from "../lib/pageCache";
import { stubFetch } from "../test/fetch";
import { checkSession, session, setSessionForTests, signOut } from "./session";

// The plugin pages' copies are the owner's private data: they leave with the session.
describe("the page cache and the session", () => {
  it("is emptied on sign-out", async () => {
    stubFetch(() => ({ body: { ok: true } }));
    await pageCache.put("instagram:profile:ann", "instagram", { name: "Ann" });
    await pageCache.put("pixiv:bookmarks:safe:0", "pixiv", { items: [] });
    pageCache.snapshots.set("instagram:profile:ann:scroll", 320);
    setSessionForTests({ status: "authed", deviceName: null });
    await signOut();
    expect(session().status).toBe("anon");
    expect(pageCache.peek("instagram:profile:ann")).toBeUndefined();
    expect(await pageCache.get("pixiv:bookmarks:safe:0")).toBeUndefined();
    expect(pageCache.snapshots.size).toBe(0);
  });

  it("is emptied when the session turns out to be gone (401)", async () => {
    stubFetch(() => ({ status: 401, body: { ok: false, error: "unauthorized" } }));
    await pageCache.put("notebooks:list:exclude", "notebooks", []);
    setSessionForTests({ status: "authed", deviceName: null });
    await checkSession();
    expect(session().status).toBe("anon");
    expect(await pageCache.get("notebooks:list:exclude")).toBeUndefined();
  });
});
