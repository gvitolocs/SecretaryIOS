// Agents' URLs: /agents (the list), /agents/new, /agents/permissions[/<id>], /agents/schedules, /agents/usage,
// /agents/map[/<componentId>] (the Homelab service map; a catalog flip push opens a component),
// and /agents/<agentId>[/changes|/files|/terminal|/scripts]. A push for an external permission opens
// /agents/permissions/<id>; one for an agent opens /agents/<agentId> (see lucina-agentd's notify routes).

export type AgentTab = "chat" | "changes" | "files" | "terminal" | "scripts";
export const AGENT_TABS: readonly AgentTab[] = ["chat", "changes", "files", "terminal", "scripts"];

export type AgentsRoute =
  | { page: "list" }
  | { page: "new" }
  | { page: "permissions"; id?: string }
  | { page: "schedules" }
  | { page: "usage" }
  | { page: "map"; id?: string }
  | { page: "agent"; id: string; tab: AgentTab };

const dec = (s: string): string => {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
};

export function parseAgentsPath(pathname: string): AgentsRoute {
  const parts = pathname.replace(/^\/agents\/?/, "").split("/").filter(Boolean).map(dec);
  const [a, b] = parts;
  if (!a) return { page: "list" };
  if (a === "new") return { page: "new" };
  if (a === "schedules") return { page: "schedules" };
  if (a === "usage") return { page: "usage" };
  if (a === "map") return b ? { page: "map", id: b } : { page: "map" };
  if (a === "permissions") return b ? { page: "permissions", id: b } : { page: "permissions" };
  const tab = AGENT_TABS.find((t) => t === b) ?? "chat";
  return { page: "agent", id: a, tab };
}

export function agentHref(id: string, tab: AgentTab = "chat"): string {
  return tab === "chat" ? `/agents/${encodeURIComponent(id)}` : `/agents/${encodeURIComponent(id)}/${tab}`;
}

/** Where a notification of category `agent` leads: the agent in `data.agentId`, else the route it carries. */
export function agentNotificationRoute(n: { category: string; route: string; data?: Record<string, unknown> }): string | null {
  if (n.category !== "agent") return null;
  const id = n.data?.agentId;
  if (typeof id === "string" && /^[A-Za-z0-9_-]{1,80}$/.test(id)) return agentHref(id);
  return n.route.startsWith("/agents") ? n.route : null;
}

export function mapHref(componentId?: string): string {
  return componentId ? `/agents/map/${encodeURIComponent(componentId)}` : "/agents/map";
}
