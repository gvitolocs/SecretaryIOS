// Reports the page's domain and title (never the full URL or its content) to background.js.
(() => {
  if (window.top !== window) return; // top-level pages only, no iframes
  const host = location.hostname.replace(/^www\./, "");
  if (!host || location.protocol !== "https:" && location.protocol !== "http:") return;
  browser.runtime.sendMessage({ type: "visit", domain: host, title: (document.title || "").slice(0, 200) }).catch(() => {});
})();
