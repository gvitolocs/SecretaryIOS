import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { session, setSessionForTests } from "../state/session";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import { AccountView, NotificationsView } from "./ShellSheets";

const routes = () => (
  <>
    <Route path="/" component={() => <p>Main chat</p>} />
    <Route path="/notifications" component={NotificationsView} />
    <Route path="/account" component={AccountView} />
  </>
);

describe("shell sheets", () => {
  afterEach(() => {
    delete window.secretaryShell;
  });

  it("/notifications opens the notifications panel over the chat and closes to it", async () => {
    stubFetch(() => ({ body: { items: [], nextBefore: null, cursor: null, hasMore: false } }));
    renderAt("/notifications", routes);
    const panel = await screen.findByRole("dialog", { name: /Notifications/ });
    fireEvent.keyDown(panel, { key: "Escape" });
    expect(await screen.findByText("Main chat")).toBeInTheDocument();
  });

  it("/account signs out of the web app and makes Secretary Code forget its device token", async () => {
    setSessionForTests({ status: "authed", deviceName: "Mac", deviceId: "dev_mac" });
    const order: string[] = [];
    window.secretaryShell = {
      version: 1,
      showCode: vi.fn(),
      signOut: vi.fn(() => {
        order.push("shell");
      }),
    };
    const calls = stubFetch((c) => {
      if (c.url === "/v1/session" && c.method === "DELETE") order.push("web");
      return { body: { items: [], nextBefore: null, cursor: null, hasMore: false } };
    });
    renderAt("/account", routes);
    fireEvent.click(await screen.findByRole("button", { name: "Log out" }));
    await waitFor(() => expect(session().status).toBe("anon"));
    expect(window.secretaryShell.signOut).toHaveBeenCalledOnce();
    expect(order).toEqual(["shell", "web"]);
    expect(calls.some((c) => c.url === "/v1/session" && c.method === "DELETE")).toBe(true);
  });
});

describe("plugin settings route", () => {
  it.each([false, true])("opens the plugins section (desktop=%s)", async desktop => {
    if (desktop) stubWide();
    stubFetch(() => ({ body: { ok: true, data: { connection: { state: "disconnected" } }, items: [] } }));
    renderAt("/account?section=plugins", routes);
    expect(await screen.findByRole("heading", { name: "Pixiv" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Show adult plugins on this device" })).toBeInTheDocument();
    if (desktop) {
      expect(screen.getByRole("link", { name: "Plugins" })).toHaveAttribute("aria-current", "page");
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(screen.queryByLabelText("Current password")).toBeNull();
      expect(document.querySelector(".pg-card #plugin-settings")).toBeInTheDocument();
    } else expect(screen.getByRole("dialog", { name: "Account" })).toBeInTheDocument();
  });
});
