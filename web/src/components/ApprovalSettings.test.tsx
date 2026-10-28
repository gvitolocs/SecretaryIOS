import { fireEvent, render, screen } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { setLocale } from "../i18n";
import { stubFetch, type Call } from "../test/fetch";
import { AccountDialog } from "./AccountDialog";
import { ApprovalSettings } from "./ApprovalSettings";

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
