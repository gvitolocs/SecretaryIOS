// One web_visit per domain every 5 minutes, handed to the app's native handler, which only
// sends it when "Safari websites" is on in Secretary → Lucina's access.
const recent = new Map();
const QUIET_MS = 5 * 60 * 1000;

browser.runtime.onMessage.addListener((message) => {
  if (!message || message.type !== "visit" || typeof message.domain !== "string") return;
  const now = Date.now();
  if ((recent.get(message.domain) ?? 0) > now - QUIET_MS) return;
  recent.set(message.domain, now);
  if (recent.size > 500) recent.delete(recent.keys().next().value);
  return browser.runtime
    .sendNativeMessage("tech.getsecretary.app", { type: "visit", domain: message.domain, title: message.title ?? "", ts: now })
    .catch(() => {});
});
