import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AppNotification } from "../api/notifications";
import { setLocale } from "../i18n";
import { resetNotifications, setNotificationsForTests, unreadCount } from "../state/notifications";
import { stubFetch, type Call } from "../test/fetch";
import { renderAt } from "../test/router";
import { NotificationBell, relativeTime } from "./NotificationBell";

const NOW = Date.now();

function n(id: string, fields: Partial<AppNotification> = {}): AppNotification {
  return { id, ts: NOW - 5 * 60_000, category: "lucina_message", body: `body ${id}`, route: "/", read: false, ...fields };
}

const ITEMS: AppNotification[] = [
  n("n_3", { channel: "telegram" }),
  n("n_2", { category: "reminder", body: "Call the dentist", read: true }),
  n("n_1", { category: "shopping", body: "Milk added", route: "/shopping", ts: NOW - 3 * 86_400_000 }),
];

function serve(): Call[] {
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/notifications?")) return { body: { items: ITEMS, unread: 2, nextBefore: null } };
    if (c.url === "/v1/notifications/read") return { body: { updated: 1, unread: 1 } };
    if (c.url === "/v1/push/key") return { body: { publicKey: "BAAA" } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

function mount() {
  const settings = vi.fn();
  renderAt("/", () => (
    <>
      <Route path="/" component={() => <NotificationBell onSettings={settings} />} />
      <Route path="/shopping" component={() => <p>Shopping page</p>} />
    </>
  ));
  return settings;
}

describe("NotificationBell", () => {
  beforeEach(() => {
    resetNotifications();
    setNotificationsForTests(ITEMS, 2);
  });
  afterEach(() => resetNotifications());

  it("shows the unread badge and an accessible label", () => {
    serve();
    mount();
    const bell = screen.getByRole("button", { name: "Notifications, 2 unread" });
    expect(bell).toHaveTextContent("2");
    expect(bell).toHaveAttribute("aria-expanded", "false");
  });

  it("lists notifications by day with category titles", async () => {
    serve();
    mount();
    fireEvent.click(screen.getByRole("button", { name: "Notifications, 2 unread" }));
    const dialog = await screen.findByRole("dialog", { name: /Notifications/ });
    expect(dialog).toHaveTextContent("Today");
    expect(dialog).toHaveTextContent("Earlier");
    expect(dialog).toHaveTextContent("Reminder");
    expect(dialog).toHaveTextContent("Call the dentist");
    expect(dialog).toHaveTextContent("via Telegram");
    expect(screen.getAllByRole("img", { name: "Unread" })).toHaveLength(2);
  });

  it("opens the route and marks the item read on click", async () => {
    const calls = serve();
    mount();
    fireEvent.click(screen.getByRole("button", { name: "Notifications, 2 unread" }));
    fireEvent.click(await screen.findByText("Milk added"));
    expect(await screen.findByText("Shopping page")).toBeInTheDocument();
    await waitFor(() => expect(calls.some((c) => c.url === "/v1/notifications/read")).toBe(true));
    const read = calls.find((c) => c.url === "/v1/notifications/read")!;
    expect(read.method).toBe("POST");
    expect(read.body).toEqual({ ids: ["n_1"] });
    expect(read.headers["x-secretary-csrf"]).toBe("1");
    expect(unreadCount()).toBe(1);
  });

  it("never asks for notification permission without a click", async () => {
    serve();
    const requestPermission = vi.fn(async () => "granted" as NotificationPermission);
    vi.stubGlobal("Notification", Object.assign(function Notification() {}, { permission: "default", requestPermission }));
    mount();
    fireEvent.click(screen.getByRole("button", { name: "Notifications, 2 unread" }));
    await screen.findByRole("dialog");
    expect(requestPermission).not.toHaveBeenCalled();
  });

  it("speaks Italian and Danish", async () => {
    serve();
    setLocale("it");
    mount();
    expect(screen.getByRole("button", { name: "Notifiche, 2 da leggere" })).toBeInTheDocument();
    setLocale("da");
    expect(screen.getByRole("button", { name: "Notifikationer, 2 ulæste" })).toBeInTheDocument();
    setLocale(null);
  });

  it("formats relative times", () => {
    expect(relativeTime(NOW - 20_000, NOW, "en")).toBe("now");
    expect(relativeTime(NOW - 5 * 60_000, NOW, "en")).toBe("5 min. ago");
    expect(relativeTime(NOW - 3 * 3_600_000, NOW, "en")).toBe("3 hr. ago");
  });
});
