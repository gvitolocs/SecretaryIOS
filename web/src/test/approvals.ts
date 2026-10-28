import { vi } from "vitest";
import type { Approval } from "../api/approvals";
import type { SecretaryApprover } from "../api/desktopApprover";

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

/** A fake `window.secretaryApprover` (Secretary Code's Touch ID bridge). Remove it with `delete window.secretaryApprover`. */
export function stubApprover(over: Partial<SecretaryApprover> = {}): { [K in keyof SecretaryApprover]: SecretaryApprover[K] } & {
  status: ReturnType<typeof vi.fn>;
  setup: ReturnType<typeof vi.fn>;
  approve: ReturnType<typeof vi.fn>;
  revoke: ReturnType<typeof vi.fn>;
  remove: ReturnType<typeof vi.fn>;
} {
  const bridge = {
    version: 1 as const,
    status: vi.fn(async () => ({ available: true, hasKey: true, keyId: "key_mac" })),
    setup: vi.fn(async () => ({ ok: true as const, status: "active" as const })),
    approve: vi.fn(async () => ({ ok: true as const, status: "approved" as const })),
    revoke: vi.fn(async () => ({ ok: true as const, removed: true })),
    remove: vi.fn(async () => ({ ok: true as const })),
    ...over,
  };
  window.secretaryApprover = bridge as SecretaryApprover;
  return bridge as ReturnType<typeof stubApprover>;
}
