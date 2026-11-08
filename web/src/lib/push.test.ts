import { afterEach, describe, expect, it, vi } from "vitest";

// registerServiceWorker keeps its first answer for the page's life, so each case loads a fresh copy of the module.
async function freshPush() {
  vi.resetModules();
  return import("./push");
}

function stubWorkers(registrations: { unregister: () => Promise<boolean> }[]) {
  const register = vi.fn(async () => ({}) as ServiceWorkerRegistration);
  const getRegistrations = vi.fn(async () => registrations);
  Object.defineProperty(navigator, "serviceWorker", { configurable: true, value: { register, getRegistrations } });
  const deleted: string[] = [];
  vi.stubGlobal("caches", {
    keys: async () => ["secretary-static-v2", "secretary-share-inbox", "secretary-static-v1"],
    delete: async (k: string) => (deleted.push(k), true),
  });
  return { register, getRegistrations, deleted };
}

describe("registerServiceWorker", () => {
  afterEach(() => {
    delete window.secretaryShell;
    delete (navigator as { serviceWorker?: unknown }).serviceWorker;
    vi.unstubAllGlobals();
  });

  it("registers /sw.js in a browser", async () => {
    const { register } = stubWorkers([]);
    const { registerServiceWorker } = await freshPush();
    await registerServiceWorker();
    expect(register).toHaveBeenCalledWith("/sw.js", { scope: "/" });
  });

  it("registers none inside Secretary Code, unregisters any old one and deletes its static caches", async () => {
    const old = { unregister: vi.fn(async () => true) };
    const { register, deleted } = stubWorkers([old]);
    window.secretaryShell = { showCode() {} } as unknown as NonNullable<Window["secretaryShell"]>;
    const { registerServiceWorker } = await freshPush();
    expect(await registerServiceWorker()).toBeNull();
    expect(register).not.toHaveBeenCalled();
    expect(old.unregister).toHaveBeenCalledOnce();
    expect(deleted).toEqual(["secretary-static-v2", "secretary-static-v1"]);
  });
});
