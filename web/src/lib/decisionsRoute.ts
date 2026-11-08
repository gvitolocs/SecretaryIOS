// Where a "Decision needed" notification leads: /decisions/<id>. The server sends that route and the id in
// `data.decisionId`; the id wins, so an older item whose route says "/" or "/chat" still opens the question.

const ID = /^dc-[A-Za-z0-9_-]{1,80}$/;

export function decisionHref(id: string): string {
  return `/decisions/${encodeURIComponent(id)}`;
}

export function decisionNotificationRoute(n: { route: string; data?: Record<string, unknown> }): string | null {
  const id = n.data?.decisionId;
  if (typeof id === "string" && ID.test(id)) return decisionHref(id);
  return n.route.startsWith("/decisions") ? n.route : null;
}
