import { Route } from "@solidjs/router";
import { screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderAt } from "../test/router";
import { stubWide } from "../test/wide";
import ScanQr from "./ScanQr";

describe("desktop Scan QR", () => {
  beforeEach(() => stubWide());
  afterEach(() => vi.unstubAllGlobals());

  it("uses the page header and scanner card", async () => {
    renderAt("/scan", () => <Route path="/scan" component={ScanQr} />);
    expect(await screen.findByRole("heading", { level: 1 })).toHaveTextContent(/Scan QR/i);
    expect(document.querySelector(".scan-head")).toBeNull();
    expect(document.querySelector(".scan-desk-card")).toBeInTheDocument();
    expect(document.querySelectorAll(".pg-cta")).toHaveLength(0);
    expect(await screen.findByRole("button", { name: "Try again" })).toBeInTheDocument();
  });
});
