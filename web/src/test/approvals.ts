import type { Approval } from "../api/approvals";

export const NOW = new Date("2026-10-28T12:00:00Z");

/** A pending, not-sensitive request expiring `ttl` seconds after NOW (wire shape, as the server sends it). */
export function approvalJson(id: string, over: Partial<Approval> & { ttl?: number } = {}): Record<string, unknown> {
  const { ttl = 300, ...rest } = over;
  return {
    id,
    requester: { kind: "lucina", name: "Lucina", detail: "ask_lucina from Claude Code" },
    action: "tool.run",
    summary: `Restart the tunnel (${id})`,
    details: "systemctl restart oracle-poko-tunnel\nsystemctl status oracle-poko-tunnel",
    risk: "medium",
    sensitive: false,
    payloadHash: `sha256:${id.padEnd(64, "a")}`,
    createdAt: NOW.toISOString(),
    expiresAt: new Date(NOW.getTime() + ttl * 1000).toISOString(),
    escalated: false,
    youAreMain: true,
    ...rest,
  };
}
