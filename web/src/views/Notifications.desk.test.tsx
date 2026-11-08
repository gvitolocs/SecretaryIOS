import { Route } from "@solidjs/router";
import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { AppNotification } from "../api/notifications";
import { resetNotifications, setNotificationsForTests, unreadCount } from "../state/notifications";
import { stubFetch } from "../test/fetch";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import { NotificationsView } from "./ShellSheets";

const NOW = Date.now();
function n(id: string, fields: Partial<AppNotification> = {}): AppNotification {
  return { id, ts: NOW - 5 * 60_000, category: "lucina_message", body: `body ${id}`, route: "/", read: false, ...fields };
}
const ITEMS: AppNotification[] = [
  n("n_3", { channel: "telegram" }),
  n("n_2", { category: "reminder", body: "Call the dentist", read: true }),
  n("n_1", { category: "shopping", body: "Milk added", route: "/shopping", ts: NOW - 3 * 86_400_000 }),
];

function serve(items: AppNotification[], unread: number, status = 200) {
  return stubFetch((c) => {
    if (c.url.startsWith("/v1/notifications?"))
      return status === 200
        ? { body: { items, unread, nextBefore: null } }
        : { status, body: { error: { code: "boom", message: "no" } } };
    if (c.url === "/v1/notifications/read" || c.url === "/v1/notifications/read-all") return { body: { updated: 1, unread: 0 } };
    if (c.url === "/v1/push/key") return { body: { publicKey: "BAAA" } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}
const mount = () =>
  renderAt("/notifications", () => (
    <>
      <Route path="/notifications" component={NotificationsView} />
      <Route path="/shopping" component={() => <p>Shopping page</p>} />
      <Route path="/account" component={() => <p>Account page</p>} />
    </>
  ));

beforeEach(() => {
  stubWide();
  resetNotifications();
});
afterEach(() => {
  vi.unstubAllGlobals();
  resetNotifications();
});

it("lists notifications in day cards under a page header with the unread count", async () => {
  setNotificationsForTests(ITEMS, 2);
  serve(ITEMS, 2);
  const { container } = mount();
  expect(await screen.findByRole("heading", { level: 1, name: "Notifications" })).toBeInTheDocument();
  expect(container.querySelector(".pg-context")).toHaveTextContent("2 unread");
  expect(container.querySelectorAll(".pg-cta").length).toBeLessThanOrEqual(1);
  // The panel (dialog) is gone; the cards are regions.
  expect(screen.queryByRole("dialog")).toBeNull();
  const today = screen.getByRole("region", { name: "Today" });
  const earlier = screen.getByRole("region", { name: "Earlier" });
  expect(within(today).getAllByRole("listitem")).toHaveLength(2);
  expect(within(earlier).getByText("Milk added")).toBeInTheDocument();
  expect(within(today).getByText(/via Telegram/)).toBeInTheDocument();
  expect(within(today).getAllByRole("img", { name: "Unread" })).toHaveLength(1);
  expect(screen.getByRole("link", { name: "Notification settings" })).toHaveAttribute("href", "/account?section=notifications");
});

it("marks all read from the header and opens an item's route", async () => {
  setNotificationsForTests(ITEMS, 2);
  serve(ITEMS, 2);
  mount();
  fireEvent.click(await screen.findByRole("button", { name: "Mark all read" }));
  await waitFor(() => expect(unreadCount()).toBe(0));
  expect(screen.queryByRole("button", { name: "Mark all read" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Shopping list" }));
  expect(await screen.findByText("Shopping page")).toBeInTheDocument();
});

it("shows an empty state and a load error", async () => {
  serve([], 0);
  const first = mount();
  expect(await screen.findByText("Nothing new")).toBeInTheDocument();
  first.unmount();
  resetNotifications();
  serve([], 0, 500);
  mount();
  expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't load notifications.");
  expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
});
