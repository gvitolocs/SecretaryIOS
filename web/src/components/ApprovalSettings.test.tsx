import { fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { stubApprover } from "../test/approvals";
import { AccountDialog } from "./AccountDialog";
import { ApprovalSettings, PENDING_POLL_MS } from "./ApprovalSettings";

const SETTINGS = {
  mainDeviceId: null,
  escalateAfterSecs: 30,
  thisDeviceId: "dev_web",
  devices: [
    { id: "dev_phone", name: "Giuseppe's iPhone", isMain: false, hasBiometricKey: true, lastSeen: "2026-10-28T10:00:00Z" },
    { id: "dev_web", name: "Chrome", isMain: false, hasBiometricKey: false },
  ],
};

async function until(check: () => void, tries = 100): Promise<void> {
  for (let i = 0; ; i++) {
    try {
      check();
      return;
    } catch (e) {
      if (i >= tries) throw e;
      await new Promise((r) => setTimeout(r, 5));
    }
  }
}

function server(over: (c: Call) => { status?: number; body?: unknown } | undefined = () => undefined) {
  return stubFetch((c) => {
    const r = over(c);
    if (r) return r;
    if (c.url === "/v1/approvals/settings" && c.method === "GET") return { body: SETTINGS };
    if (c.url === "/v1/approvals/settings" && c.method === "PUT") return { body: {} };
    return { status: 404, body: {} };
  });
}

describe("Approvals in the account panel", () => {
  it("saves the main device with PUT and the CSRF header", async () => {
    const calls = server();
    render(() => <ApprovalSettings />);
    const select = (await screen.findByLabelText("Main device for approvals")) as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual([
      "None: all devices at once",
      "Giuseppe's iPhone",
      "Chrome (this device)",
    ]);
    expect(select.value).toBe("");

    fireEvent.change(select, { target: { value: "dev_phone" } });
    await until(() => screen.getByText("Saved"));
    const put = calls.find((c) => c.method === "PUT")!;
    expect(put.url).toBe("/v1/approvals/settings");
    expect(put.body).toEqual({ mainDeviceId: "dev_phone" });
    expect(put.headers["x-secretary-csrf"]).toBe("1");
    expect(select.value).toBe("dev_phone");

    fireEvent.change(select, { target: { value: "" } });
    await until(() => expect(calls.filter((c) => c.method === "PUT")).toHaveLength(2));
    expect(calls.filter((c) => c.method === "PUT")[1]!.body).toEqual({ mainDeviceId: null });
  });

  it("shows the current main device and which devices have Face ID", async () => {
    server((c) => (c.method === "GET" ? { body: { ...SETTINGS, mainDeviceId: "dev_phone" } } : undefined));
    render(() => <ApprovalSettings />);
    const select = (await screen.findByLabelText("Main device for approvals")) as HTMLSelectElement;
    expect(select.value).toBe("dev_phone");
    expect(screen.getByTestId("apv-faceid-devices")).toHaveTextContent("Face ID is set up on: Giuseppe's iPhone.");
  });

  it("says when no device has Face ID", async () => {
    server((c) =>
      c.method === "GET" ? { body: { ...SETTINGS, devices: SETTINGS.devices.map((d) => ({ ...d, hasBiometricKey: false })) } } : undefined,
    );
    render(() => <ApprovalSettings />);
    await screen.findByLabelText("Main device for approvals");
    expect(screen.getByTestId("apv-faceid-devices")).toHaveTextContent("No device has Face ID set up yet.");
  });

  it("reports a failed save and keeps what the server has", async () => {
    server((c) => (c.method === "PUT" ? { status: 500, body: {} } : undefined));
    render(() => <ApprovalSettings />);
    const select = (await screen.findByLabelText("Main device for approvals")) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "dev_phone" } });
    await until(() => expect(screen.getByRole("alert")).toHaveTextContent("Couldn't save the main device."));
    expect(select.value).toBe("");
  });

  it("is hidden while the server has no approvals routes", async () => {
    const calls = stubFetch(() => ({ status: 404, body: {} }));
    render(() => <ApprovalSettings />);
    await until(() => expect(calls.length).toBeGreaterThan(0));
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByText("Approvals")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("is a section of the account dialog, in the chosen language", async () => {
    server();
    setLocale("it");
    render(() => <AccountDialog open={true} onClose={vi.fn()} />);
    expect(await screen.findByLabelText("Dispositivo principale per le approvazioni")).toBeInTheDocument();
  });
});

describe("Touch ID on this Mac (Secretary Code)", () => {
  afterEach(() => {
    delete window.secretaryApprover;
    vi.useRealTimers();
  });

  const MAC = { id: "dev_web", name: "MacBook Air", isMain: false, hasBiometricKey: false };
  const PHONE = {
    id: "dev_phone",
    name: "Giuseppe's iPhone",
    isMain: false,
    hasBiometricKey: true,
    keyLabel: "Face ID on iPhone",
    keyCreatedAt: "2026-10-01T10:00:00Z",
  };
  const withDevices = (devices: object[]) => (c: Call) =>
    c.url === "/v1/approvals/settings" && c.method === "GET" ? { body: { ...SETTINGS, devices } } : undefined;

  it("shows nothing about this Mac in a browser", async () => {
    server(withDevices([PHONE, MAC]));
    render(() => <ApprovalSettings />);
    await screen.findByLabelText("Main device for approvals");
    expect(screen.queryByTestId("apv-mac")).toBeNull();
    expect(screen.queryByRole("button", { name: "Revoke" })).toBeNull();
    // Labels come from the server and show everywhere.
    expect(screen.getByTestId("apv-device-dev_phone")).toHaveTextContent("Face ID on iPhone");
  });

  it("shows each device's key label and when it was added", async () => {
    server(withDevices([PHONE, MAC]));
    render(() => <ApprovalSettings />);
    const row = await screen.findByTestId("apv-device-dev_phone");
    expect(row).toHaveTextContent("Giuseppe's iPhone");
    expect(row).toHaveTextContent("Face ID on iPhone, added");
    expect(row).toHaveTextContent("2026");
    expect(screen.queryByTestId("apv-device-dev_web")).toBeNull();
  });

  it("sets this Mac up: active at once when it is the first approver", async () => {
    const bridge = stubApprover({ status: vi.fn().mockResolvedValueOnce({ available: true, hasKey: false }).mockResolvedValue({ available: true, hasKey: true }) });
    let active = false;
    server((c) =>
      c.url === "/v1/approvals/settings" && c.method === "GET"
        ? { body: { ...SETTINGS, devices: [{ ...MAC, hasBiometricKey: active, ...(active ? { keyLabel: "Touch ID on MacBook Air" } : {}) }] } }
        : undefined,
    );
    bridge.setup.mockImplementation(async () => {
      active = true;
      return { ok: true, status: "active" };
    });
    render(() => <ApprovalSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Set up Touch ID approvals" }));
    await until(() => screen.getByText("Touch ID approvals are on"));
    expect(bridge.setup).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Turn off" })).toBeInTheDocument();
    expect(screen.getByTestId("apv-device-dev_web")).toHaveTextContent("Touch ID on MacBook Air");
  });

  it("sets this Mac up: waits for the iPhone and polls until the key is active", async () => {
    const bridge = stubApprover({ status: vi.fn().mockResolvedValueOnce({ available: true, hasKey: false }).mockResolvedValue({ available: true, hasKey: true }) });
    let approved = false;
    let gets = 0;
    server((c) => {
      if (c.url !== "/v1/approvals/settings" || c.method !== "GET") return undefined;
      gets++;
      const mac =
        !bridge.setup.mock.calls.length
          ? MAC
          : approved
            ? { ...MAC, hasBiometricKey: true, keyLabel: "Touch ID on MacBook Air" }
            : { ...MAC, pendingApprovalId: "apr_enroll" };
      return { body: { ...SETTINGS, devices: [PHONE, mac] } };
    });
    bridge.setup.mockImplementation(async () => ({ ok: true, status: "pending_approval", approvalId: "apr_enroll" }));
    // Only the poll's interval is faked; the awaits in `until` stay real.
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    render(() => <ApprovalSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Set up Touch ID approvals" }));
    await until(() => screen.getByTestId("apv-mac-pending"));
    expect(screen.getByTestId("apv-mac-pending")).toHaveTextContent("Approve “Add MacBook Air as an approver device” on your iPhone to finish.");
    expect(screen.getByTestId("apv-device-dev_web")).toHaveTextContent("waiting for approval");
    expect(screen.queryByRole("button", { name: "Set up Touch ID approvals" })).toBeNull();

    // It keeps looking while the request is open ...
    const before = gets;
    vi.advanceTimersByTime(PENDING_POLL_MS);
    await until(() => expect(gets).toBeGreaterThan(before));
    expect(screen.getByTestId("apv-mac-pending")).toBeInTheDocument();
    // ... and stops once the iPhone approved it.
    approved = true;
    vi.advanceTimersByTime(PENDING_POLL_MS);
    await until(() => screen.getByText("Touch ID approvals are on"));
    expect(screen.queryByTestId("apv-mac-pending")).toBeNull();
    const after = gets;
    vi.advanceTimersByTime(PENDING_POLL_MS * 3);
    await new Promise((r) => setTimeout(r, 20));
    expect(gets).toBe(after);
  });

  it("says nothing when the owner cancels Touch ID during setup, and offers it again", async () => {
    const bridge = stubApprover({ status: vi.fn(async () => ({ available: true, hasKey: false })) });
    bridge.setup.mockResolvedValue({ ok: false, error: "cancelled" });
    server(withDevices([PHONE, MAC]));
    render(() => <ApprovalSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Set up Touch ID approvals" }));
    await until(() => expect(bridge.setup).toHaveBeenCalled());
    await until(() => expect(screen.getByRole("button", { name: "Set up Touch ID approvals" })).toBeEnabled());
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("reports a failed setup", async () => {
    const bridge = stubApprover({ status: vi.fn(async () => ({ available: true, hasKey: false })) });
    bridge.setup.mockResolvedValue({ ok: false, error: "network" });
    server(withDevices([PHONE, MAC]));
    render(() => <ApprovalSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Set up Touch ID approvals" }));
    await until(() => expect(screen.getByRole("alert")).toHaveTextContent("Couldn't set up Touch ID approvals."));
  });

  it("turns Touch ID approvals off with remove()", async () => {
    const bridge = stubApprover();
    let on = true;
    bridge.remove.mockImplementation(async () => {
      on = false;
      return { ok: true };
    });
    bridge.status.mockImplementation(async () => ({ available: true, hasKey: on }));
    server((c) =>
      c.url === "/v1/approvals/settings" && c.method === "GET"
        ? { body: { ...SETTINGS, devices: [PHONE, { ...MAC, hasBiometricKey: on, keyLabel: "Touch ID on MacBook Air" }] } }
        : undefined,
    );
    render(() => <ApprovalSettings />);
    fireEvent.click(await screen.findByRole("button", { name: "Turn off" }));
    await until(() => screen.getByRole("button", { name: "Set up Touch ID approvals" }));
    expect(bridge.remove).toHaveBeenCalledTimes(1);
  });

  it("says when Touch ID is not available on this Mac", async () => {
    stubApprover({ status: vi.fn(async () => ({ available: false, hasKey: false })) });
    server(withDevices([PHONE, MAC]));
    render(() => <ApprovalSettings />);
    await until(() => screen.getByText("Touch ID isn't available on this Mac."));
    expect(screen.queryByRole("button", { name: "Set up Touch ID approvals" })).toBeNull();
  });

  it("revokes another device after asking, through the bridge", async () => {
    const bridge = stubApprover();
    let revoked = false;
    bridge.revoke.mockImplementation(async () => {
      revoked = true;
      return { ok: true, removed: true };
    });
    server((c) =>
      c.url === "/v1/approvals/settings" && c.method === "GET"
        ? {
            body: {
              ...SETTINGS,
              devices: [
                { ...PHONE, hasBiometricKey: !revoked, keyLabel: revoked ? undefined : PHONE.keyLabel },
                { ...MAC, hasBiometricKey: true, keyLabel: "Touch ID on MacBook Air" },
              ],
            },
          }
        : undefined,
    );
    render(() => <ApprovalSettings />);
    // Only the other device can be revoked, not this Mac.
    await screen.findByTestId("apv-device-dev_phone");
    await until(() => expect(screen.getAllByRole("button", { name: "Revoke" })).toHaveLength(1));
    fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
    expect(bridge.revoke).not.toHaveBeenCalled();
    expect(screen.getByText("Revoke approvals for Giuseppe's iPhone?")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
    await until(() => expect(bridge.revoke).toHaveBeenCalledWith("dev_phone"));
    await until(() => expect(screen.queryByText("Revoke approvals for Giuseppe's iPhone?")).toBeNull());
    await until(() => expect(screen.queryByTestId("apv-device-dev_phone")).toBeNull());
  });

  it("can cancel a revoke, and offers none without a key on this Mac", async () => {
    const bridge = stubApprover();
    server(withDevices([PHONE, { ...MAC, hasBiometricKey: true, keyLabel: "Touch ID on MacBook Air" }]));
    render(() => <ApprovalSettings />);
    await until(() => screen.getByRole("button", { name: "Revoke" }));
    fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByText("Revoke approvals for Giuseppe's iPhone?")).toBeNull();
    expect(bridge.revoke).not.toHaveBeenCalled();
  });

  it("offers no revoke while this Mac has no key", async () => {
    stubApprover({ status: vi.fn(async () => ({ available: true, hasKey: false })) });
    server(withDevices([PHONE, MAC]));
    render(() => <ApprovalSettings />);
    await screen.findByRole("button", { name: "Set up Touch ID approvals" });
    expect(screen.queryByRole("button", { name: "Revoke" })).toBeNull();
  });
});
