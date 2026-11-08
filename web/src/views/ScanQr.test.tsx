import { Route, useLocation } from "@solidjs/router";
import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderAt } from "../test/router";
import ScanQr from "./ScanQr";

function Landed() {
  const loc = useLocation();
  return <p data-testid="landed">{loc.pathname + loc.search}</p>;
}

const open = () =>
  renderAt("/scan", () => (
    <>
      <Route path="/scan" component={ScanQr} />
      <Route path="/approve" component={Landed} />
    </>
  ));

function camera(getUserMedia: () => Promise<unknown>) {
  const stop = vi.fn();
  Object.defineProperty(navigator, "mediaDevices", {
    configurable: true,
    value: { getUserMedia: vi.fn(async () => ({ ...((await getUserMedia()) as object), getTracks: () => [{ stop }] })) },
  });
  return stop;
}

function detector(values: string[][]) {
  let i = 0;
  vi.stubGlobal(
    "BarcodeDetector",
    class {
      static getSupportedFormats = async () => ["qr_code"];
      detect = async () => (values[Math.min(i++, values.length - 1)] ?? []).map((rawValue) => ({ rawValue }));
    },
  );
}

describe("ScanQr", () => {
  afterEach(() => {
    Object.defineProperty(navigator, "mediaDevices", { configurable: true, value: undefined });
  });

  it("opens the approve screen for a scanned sign-in code and stops the camera", async () => {
    const stop = camera(async () => ({}));
    detector([[], ["https://example.org/not-ours"], ["https://getsecretary.tech/approve?c=w0CwKBb_4q6MacWLPEja7w"]]);
    open();
    await waitFor(() => expect(screen.getByTestId("landed")).toHaveTextContent("/approve?c=w0CwKBb_4q6MacWLPEja7w"));
    expect(stop).toHaveBeenCalled();
  });

  it("says when a QR code is not a Secretary one", async () => {
    camera(async () => ({}));
    detector([["https://example.org/menu"]]);
    open();
    expect(await screen.findByText("That isn't a Secretary sign-in code.")).toBeInTheDocument();
  });

  it("takes the 6-digit code typed instead", async () => {
    camera(async () => ({}));
    detector([[]]);
    open();
    const input = screen.getByLabelText("Or type the 6-digit code");
    fireEvent.input(input, { target: { value: "12" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Type the 6 digits shown under the QR code.");
    fireEvent.input(input, { target: { value: "482 913" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));
    await waitFor(() => expect(screen.getByTestId("landed")).toHaveTextContent("/approve?c=482913"));
  });

  it("explains a refused camera and still offers the code", async () => {
    camera(async () => {
      throw Object.assign(new Error("no"), { name: "NotAllowedError" });
    });
    open();
    expect(await screen.findByRole("alert")).toHaveTextContent("Camera access is off for Secretary.");
    expect(screen.getByLabelText("Or type the 6-digit code")).toBeInTheDocument();
  });

  it("works without a camera at all", async () => {
    open();
    expect(await screen.findByRole("alert")).toHaveTextContent("No camera here.");
  });
});
