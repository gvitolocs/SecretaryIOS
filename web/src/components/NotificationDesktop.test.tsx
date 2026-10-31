import { render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { isDesktopApp, pushSupport } from "../lib/push";
import { refreshPushState, resetNotifications } from "../state/notifications";
import { stubFetch } from "../test/fetch";
import { PushCta } from "./NotificationBell";
import { NotificationSettings } from "./NotificationSettings";

const LINE = "Secretary Code shows Lucina's notifications as Mac notifications.";

// Inside Secretary Code the page runs in Electron, which has no push service: no "Turn on notifications" button.
describe("notifications inside Secretary Code", () => {
  afterEach(() => {
    delete window.secretaryShell;
    vi.unstubAllGlobals();
    resetNotifications();
  });

  it("detects the desktop app by its bridge or by Electron in the user agent", () => {
    expect(isDesktopApp()).toBe(false);
    window.secretaryShell = { showCode: vi.fn() };
    expect(isDesktopApp()).toBe(true);
    expect(pushSupport()).toBe("desktop-app");
    delete window.secretaryShell;
    expect(isDesktopApp({ userAgent: "Mozilla/5.0 (Macintosh) Chrome/130 Electron/33.0.0 Safari/537.36" } as Navigator)).toBe(true);
    expect(isDesktopApp({ userAgent: "Mozilla/5.0 (Macintosh) Safari/605" } as Navigator)).toBe(false);
  });

  it("shows a line instead of the enable button in the bell panel", async () => {
    window.secretaryShell = { showCode: vi.fn() };
    stubFetch(() => ({ status: 404, body: {} }));
    render(() => <PushCta />);
    await refreshPushState();
    expect(await screen.findByText(LINE)).toBeInTheDocument();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("shows the line in the notification settings and offers no enable button", async () => {
    window.secretaryShell = { showCode: vi.fn() };
    stubFetch(() => ({ status: 404, body: {} }));
    render(() => <NotificationSettings />);
    await waitFor(() => expect(screen.getByText(LINE)).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "Enable notifications" })).toBeNull();
    expect(screen.queryByText("Turn on notifications for this device.")).toBeNull();
  });
});
