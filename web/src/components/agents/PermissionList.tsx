import { createEffect, createMemo, createSignal, For } from "solid-js";
import type { PermissionView } from "../../api/agents";
import { agentsState, pendingPermissions, permKey } from "../../state/agents";
import { PermissionCard } from "./PermissionCard";

/**
 * The pending permissions (of one agent, or all). A request that was shown stays on screen, saying how it ended
 * (allowed, denied, expired, answered elsewhere), until the view is left.
 */
export function PermissionList(props: { agentId?: string; showAgent?: boolean; focusId?: string }) {
  const [seen, setSeen] = createSignal<Record<string, PermissionView>>({});
  createEffect(() => {
    const now = pendingPermissions().filter((p) => !props.agentId || p.agentId === props.agentId);
    setSeen((prev) => {
      let next = prev;
      for (const p of now) {
        const k = permKey(p);
        if (!next[k]) next = { ...next, [k]: p };
      }
      return next;
    });
  });
  const rows = createMemo(() =>
    Object.entries(seen())
      .map(([k, old]) => agentsState.permissions[k] ?? old)
      .sort((a, b) => a.createdAt.localeCompare(b.createdAt)),
  );
  return (
    <For each={rows()}>
      {(p) => (
        <div class="ag-permwrap" data-focus={props.focusId === p.id ? "yes" : "no"}>
          <PermissionCard
            permission={p}
            showAgent={props.showAgent}
            request={p.agentId ? agentsState.snapshots[p.agentId]?.pendingPermissions.find((r) => r.id === p.id) : undefined}
          />
        </div>
      )}
    </For>
  );
}
