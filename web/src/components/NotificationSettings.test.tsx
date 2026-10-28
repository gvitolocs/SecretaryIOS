import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import { resetNotifications } from "../state/notifications";
import { stubFetch, type Call } from "../test/fetch";
import { NotificationSettings, tomorrowMorning } from "./NotificationSettings";

const SETTINGS = {
  categories: {
    lucina_message: true,
    reminder: true,
    calendar: true,
    device_confirm: true,
    agent: true,
    shopping: true,
    presence: false,
    system: false,
  },
  quietHours: { enabled: true, start: "22:30", end: "07:30" },
  timeZone: "Europe/Rome",
  dndUntil: null,
  calendarLeadMinutes: 10,
  remindersBypassQuiet: true,
  digest: true,
  previews: true,
  language: "en",
  devices: {},
};

function view(settings: unknown = SETTINGS) {
  return {
    settings,
    subscriptions: [
      {
        id: "ps_1",
        kind: "webpush",
        deviceId: "dev_1",
        deviceName: "web",
        label: "iPhone · Safari (app)",
        service: "web.push.apple.com",
        createdAt: 1,
        lastSuccessAt: null,
        failures: 0,
        lastError: null,
      },
    ],
    currentDevice: { id: "dev_1", name: "web" },
    webPush: { enabled: true, publicKey: "BAAA" },
    apns: { enabled: false, environment: null },
    present: false,
  };
}

function serve(): Call[] {
  return stubFetch((c) => {
    if (c.url === "/v1/notifications/settings" && c.method === "GET") return { body: view() };
    if (c.url === "/v1/notifications/settings" && c.method === "PUT") return { body: view(c.body) };
    if (c.url === "/v1/push/subscriptions/ps_1") return { body: { removed: 1 } };
    return { status: 404, body: { error: { code: "not_found", message: "no" } } };
  });
}

afterEach(() => resetNotifications());

describe("NotificationSettings", () => {
  it("shows every category with its switch and saves a change", async () => {
    const calls = serve();
    render(() => <NotificationSettings />);
    const shopping = await screen.findByRole("switch", { name: /Shopping list/ });
    expect(shopping).toBeChecked();
    expect(screen.getByRole("switch", { name: /Secretary/ })).not.toBeChecked();
    expect(screen.getAllByRole("switch", { name: /./ }).length).toBeGreaterThanOrEqual(11);
    fireEvent.click(shopping);
    await waitFor(() => expect(calls.some((c) => c.method === "PUT")).toBe(true));
    const put = calls.find((c) => c.method === "PUT")!;
    expect(put.headers["x-secretary-csrf"]).toBe("1");
    expect((put.body as typeof SETTINGS).categories.shopping).toBe(false);
    expect((put.body as typeof SETTINGS).categories.reminder).toBe(true);
  });

  it("edits quiet hours and Do Not Disturb", async () => {
    const calls = serve();
    render(() => <NotificationSettings />);
    const from = (await screen.findByLabelText("From")) as HTMLInputElement;
    expect(from.value).toBe("22:30");
    fireEvent.change(from, { target: { value: "23:15" } });
    await waitFor(() => expect(calls.filter((c) => c.method === "PUT")).toHaveLength(1));
    expect((calls.at(-1)!.body as typeof SETTINGS).quietHours.start).toBe("23:15");
    fireEvent.click(screen.getByRole("button", { name: "1 hour" }));
    await waitFor(() => expect(calls.filter((c) => c.method === "PUT")).toHaveLength(2));
    const until = (calls.at(-1)!.body as { dndUntil: number }).dndUntil;
    expect(until).toBeGreaterThan(Date.now() + 3_500_000);
    expect(await screen.findByText(/On until/)).toBeInTheDocument();
  });

  it("lists devices and mutes one", async () => {
    const calls = serve();
    render(() => <NotificationSettings />);
    const device = await screen.findByRole("switch", { name: "Notify this device: iPhone · Safari (app)" });
    fireEvent.click(device);
    await waitFor(() => expect(calls.some((c) => c.method === "PUT")).toBe(true));
    expect((calls.find((c) => c.method === "PUT")!.body as typeof SETTINGS).devices).toEqual({
      ps_1: { enabled: false, categories: {} },
    });
    expect(screen.getByText("Nothing delivered yet")).toBeInTheDocument();
  });

  it("computes tomorrow morning", () => {
    const d = new Date(tomorrowMorning(new Date(2026, 9, 22, 23, 10)));
    expect([d.getDate(), d.getHours(), d.getMinutes()]).toEqual([23, 8, 0]);
  });
});
