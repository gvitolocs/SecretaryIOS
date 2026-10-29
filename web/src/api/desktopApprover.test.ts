import { afterEach, describe, expect, it } from "vitest";
import { stubApprover } from "../test/approvals";
import { desktopApprover } from "./desktopApprover";

describe("desktopApprover", () => {
  afterEach(() => {
    delete window.secretaryApprover;
  });

  it("is absent in a browser", () => {
    expect(desktopApprover()).toBeUndefined();
  });

  it("is the bridge when it has version 1 and the five functions", () => {
    const bridge = stubApprover();
    expect(desktopApprover()).toBe(bridge);
  });

  it("ignores another version or a partial bridge", () => {
    stubApprover();
    (window.secretaryApprover as unknown as { version: number }).version = 2;
    expect(desktopApprover()).toBeUndefined();
    (window.secretaryApprover as unknown as { version: number }).version = 1;
    delete (window.secretaryApprover as { revoke?: unknown }).revoke;
    expect(desktopApprover()).toBeUndefined();
  });
});
