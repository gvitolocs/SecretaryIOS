import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseConnection, pixivApi } from "../api/pixiv";
import { setShowAdult } from "../lib/adult";
import { stubFetch } from "../test/fetch";
import { PixivSettings } from "./PixivSettings";

const connected = { state: "connected", user: { id: "1", name: "Ann", account: "ann" }, premium: true, r18: true, store: "saved" };
const defaults = { defaultContent: "safe", defaultSort: "date_d" };
function serve(connection: unknown = { state: "disconnected" }) {
  return stubFetch(c => ({ body: { ok: true, data: c.url.endsWith("/status") ? { connection, settings: defaults } : c.url.endsWith("/start") ? { loginUrl: "https://app-api.pixiv.net/web/v1/login?test", state: "state-1", expiresInSecs: 600 } : c.url.endsWith("/settings") ? { ...defaults, ...c.body as object } : connected } }));
}
const bridge = (result: { code: string } | { error: "cancelled" | "failed" | "bad_url" } = { code: "code-1" }) => {
  const login = vi.fn().mockResolvedValue(result);
  window.secretaryShell = { showCode: vi.fn(), pixivLogin: login };
  return login;
};
describe("Pixiv settings", () => {
  beforeEach(() => setShowAdult(true));
  afterEach(() => { cleanup(); delete window.secretaryShell; vi.restoreAllMocks(); setShowAdult(true); });
  it("shows the connected identity and Premium", async () => {
    serve(connected); render(() => <PixivSettings />);
    expect(await screen.findByText("Connected as Ann (@ann) · Premium: yes")).toBeInTheDocument();
  });
  it("shows disconnected and the plain browser hint", async () => {
    serve(); render(() => <PixivSettings />);
    expect(await screen.findByText("Not connected. Without an account Pixiv shows all-ages works only.")).toBeInTheDocument();
    expect(screen.getByText(/Connect from the Secretary app/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Connect Pixiv" })).toBeNull();
  });
  it("passes the start URL to the bridge and the state and code to finish", async () => {
    const calls = serve(), login = bridge(); render(() => <PixivSettings />);
    await screen.findByText(/Not connected/);
    fireEvent.click(screen.getByRole("button", { name: "Connect Pixiv" }));
    await screen.findByText("Connected as Ann (@ann) · Premium: yes");
    expect(login).toHaveBeenCalledWith("https://app-api.pixiv.net/web/v1/login?test");
    expect(calls.map(c => c.url)).toEqual(["/v1/pixiv/status", "/v1/pixiv/connect/start", "/v1/pixiv/connect/finish"]);
    expect(calls[2]?.body).toEqual({ state: "state-1", code: "code-1" });
  });
  it("silently handles cancellation", async () => {
    const calls = serve(); bridge({ error: "cancelled" }); render(() => <PixivSettings />);
    await screen.findByText(/Not connected/);
    fireEvent.click(screen.getByRole("button", { name: "Connect Pixiv" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Connect Pixiv" })).toBeEnabled());
    expect(screen.queryByRole("alert")).toBeNull();
    expect(calls.some(c => c.url.endsWith("/finish"))).toBe(false);
  });
  it.each(["failed", "bad_url"] as const)("reports bridge %s", async error => {
    serve(); bridge({ error }); render(() => <PixivSettings />);
    await screen.findByText(/Not connected/); fireEvent.click(screen.getByRole("button", { name: "Connect Pixiv" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't finish the Pixiv sign-in.");
  });
  it("shows the R-18 viewing notice and external link", async () => {
    serve({ ...connected, r18: false, r18g: false }); render(() => <PixivSettings />);
    expect(await screen.findByText(/Pixiv only lets you change this/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open pixiv.net settings" })).toHaveAttribute("href", "https://www.pixiv.net/settings/viewing");
    expect(screen.getByRole("link", { name: "Open pixiv.net settings" })).toHaveAttribute("rel", "noopener");
  });
  it("disables adult defaults and saves a sort change", async () => {
    setShowAdult(false); const calls = serve(); render(() => <PixivSettings />);
    await screen.findByText(/Not connected/);
    const content = within(screen.getByRole("group", { name: "Default content filter" }));
    expect(content.getByRole("button", { name: "R-18" })).toBeDisabled();
    expect(content.getByRole("button", { name: /^All$/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Popular" }));
    expect(await screen.findByText("Saved")).toBeInTheDocument();
    expect(calls.find(c => c.method === "PUT")).toMatchObject({ url: "/v1/pixiv/settings", body: { defaultSort: "popular" } });
  });
  it("requires confirmation before disconnecting", async () => {
    const calls = serve(connected); render(() => <PixivSettings />); await screen.findByText(/Connected as/);
    fireEvent.click(screen.getByRole("button", { name: "Disconnect" }));
    expect(calls.some(c => c.url.endsWith("/disconnect"))).toBe(false);
    const confirm = screen.getByRole("group", { name: "Disconnect" });
    expect(within(confirm).getByText(/saved sign-in is deleted/)).toBeInTheDocument();
    fireEvent.click(within(confirm).getByRole("button", { name: "Disconnect" }));
    await waitFor(() => expect(calls.some(c => c.url.endsWith("/disconnect"))).toBe(true));
  });
  it("polls pending approval every 3 seconds, stops on settlement and cleans up", async () => {
    vi.useFakeTimers();
    const status = vi.spyOn(pixivApi, "status").mockResolvedValue({ connection: parseConnection({ ...connected, store: "waiting_approval" }), settings: defaults as { defaultContent: "safe"; defaultSort: "date_d" }, account: { session: "ok", premium: true, r18: true } });
    const view = render(() => <PixivSettings />);
    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByText(/approve on your iPhone/)).toBeInTheDocument();
    await vi.advanceTimersByTimeAsync(3000); expect(status).toHaveBeenCalledTimes(2);
    status.mockResolvedValue({ connection: parseConnection(connected), settings: { defaultContent: "safe", defaultSort: "date_d" }, account: { session: "ok", premium: true, r18: true } });
    await vi.advanceTimersByTimeAsync(3000); expect(status).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(9000); expect(status).toHaveBeenCalledTimes(3);
    view.unmount(); expect(vi.getTimerCount()).toBe(0);
  });
  it("stops polling after five minutes and on unmount while pending", async () => {
    vi.useFakeTimers();
    const status = vi.spyOn(pixivApi, "status").mockResolvedValue({ connection: parseConnection({ removal: "waiting_approval" }), settings: { defaultContent: "safe", defaultSort: "date_d" }, account: { session: "none", premium: null, r18: null } });
    const view = render(() => <PixivSettings />); await vi.advanceTimersByTimeAsync(300_000);
    const count = status.mock.calls.length;
    await vi.advanceTimersByTimeAsync(30_000); expect(status).toHaveBeenCalledTimes(count);
    expect(vi.getTimerCount()).toBe(0); view.unmount();
    const next = render(() => <PixivSettings />); await vi.advanceTimersByTimeAsync(0); next.unmount();
    const last = status.mock.calls.length; await vi.advanceTimersByTimeAsync(9000); expect(status).toHaveBeenCalledTimes(last);
  });
});
