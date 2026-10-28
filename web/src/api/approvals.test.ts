import { describe, expect, it } from "vitest";
import { approvalJson } from "../test/approvals";
import { parseApproval, parseApprovals, parseSettings } from "./approvals";
import { parseNotification } from "./notifications";

describe("approvals API parsing", () => {
  it("reads the contract's item", () => {
    const a = parseApproval(approvalJson("apr_1", { sensitive: true, risk: "high" }));
    expect(a).toMatchObject({
      id: "apr_1",
      requester: { kind: "lucina", name: "Lucina", detail: "ask_lucina from Claude Code" },
      sensitive: true,
      risk: "high",
      status: "pending",
      youAreMain: true,
    });
  });

  it("drops malformed items and accepts a bare requester name", () => {
    expect(parseApprovals([{ id: 1 }, null, { ...approvalJson("apr_2"), requester: "Broker" }])).toEqual([
      expect.objectContaining({ id: "apr_2", requester: { kind: "", name: "Broker" } }),
    ]);
    expect(parseApprovals({ items: [approvalJson("apr_3")] })).toHaveLength(1);
    // The server sends {approvals: [...]}.
    expect(parseApprovals({ approvals: [approvalJson("apr_4"), approvalJson("apr_5")] }).map((a) => a.id)).toEqual(["apr_4", "apr_5"]);
    expect(parseApprovals("nope")).toEqual([]);
  });

  it("reads the settings", () => {
    expect(parseSettings({ mainDeviceId: null, devices: [{ id: "d1", name: "iPhone", isMain: false, hasBiometricKey: true }], thisDeviceId: "d1" })).toMatchObject({
      mainDeviceId: null,
      escalateAfterSecs: 30,
      devices: [{ id: "d1", hasBiometricKey: true }],
    });
    expect(parseSettings({})).toBeNull();
  });

  it("keeps notifications of the approval category", () => {
    expect(parseNotification({ id: "n1", ts: 1, category: "approval", body: "x", route: "/approvals/apr_1" })).toMatchObject({
      category: "approval",
      route: "/approvals/apr_1",
    });
  });
});
