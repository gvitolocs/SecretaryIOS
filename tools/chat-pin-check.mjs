#!/usr/bin/env node
// The Chat must land on, and stay on, the newest message: a long history with photos and links, in the
// Secretary Code shell layout (?shell=1 with a window.secretaryShell bridge) at 1440x900 and 390x844.
// Each scenario passes when the list is within 8 px of its end 3 s later and no older page was fetched.
//
// Needs the mock API with a long history and a built web app (both local):
//   (cd web && npm run build && MOCK_PORT=8891 MOCK_CHAT_HISTORY=600 MOCK_LATENCY_MS=250 node mock/server.mjs &
//    MOCK_PORT=8891 npx vite preview --port 4391 &)
// then: PLAYWRIGHT_DIR=/path/to/node_modules/playwright [CHROME_PATH=/usr/bin/google-chrome] node tools/chat-pin-check.mjs [base-url]
// SHELL_MODE=0 tests the plain web layout, STAGE=1 keeps Lucina's 3D stage (heavy without a GPU), SHOT=<prefix>
// saves a screenshot per width. On nezopt run it in a capped scope (systemd-run --user --scope -p CPUQuota=150%).
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_DIR ?? "playwright");
const base = process.argv[2] ?? "http://127.0.0.1:4391";
const SHELL = process.env.SHELL_MODE !== "0";

async function api(page, method, path, body) {
  return page.evaluate(async ([method, path, body]) => {
    const r = await fetch(path, { method, headers: { "Content-Type": "application/json", "X-Secretary-CSRF": "1" }, body: body === undefined ? undefined : JSON.stringify(body), credentials: "same-origin" });
    return { status: r.status, json: await r.json().catch(() => null) };
  }, [method, path, body]);
}

const state = (page) => page.evaluate(() => {
  const l = document.querySelector(".messages");
  if (!l) return { none: true, path: location.pathname };
  return { path: location.pathname, dist: Math.round(l.scrollHeight - l.scrollTop - l.clientHeight), top: Math.round(l.scrollTop), h: l.scrollHeight, ch: l.clientHeight, msgs: document.querySelectorAll(".msg").length };
});

async function sample(page, label, olderReqs) {
  const out = [];
  const t0 = Date.now();
  const before = olderReqs.n;
  for (const at of [50, 200, 500, 1000, 2000, 3000]) {
    await page.waitForTimeout(Math.max(0, at - (Date.now() - t0)));
    const s = await state(page);
    out.push(s.none ? `${at}ms:none` : `${at}ms:d=${s.dist},top=${s.top},n=${s.msgs}`);
  }
  const s = await state(page);
  const ok = !s.none && s.dist < 8 && olderReqs.n === before;
  console.log(`${ok ? "PASS" : "FAIL"} ${label}: ${out.join(" | ")} olderLoads=${olderReqs.n - before}`);
  return ok;
}

const go = (page, path) => page.evaluate((p) => { history.pushState(null, "", p); dispatchEvent(new PopStateEvent("popstate")); }, path);

const browser = await chromium.launch({ executablePath: process.env.CHROME_PATH, args: ["--disable-gpu"] });
let failures = 0;
try {
  for (const [name, viewport] of [["1440", { width: 1440, height: 900 }], ["390", { width: 390, height: 844 }]]) {
    const ctx = await browser.newContext({ viewport, locale: "en-GB" });
    if (process.env.STAGE !== "1") await ctx.addInitScript(() => { try { localStorage.setItem("secretary.stage.hidden", "1"); } catch {} });
    if (SHELL) await ctx.addInitScript(() => { window.secretaryShell = { version: 1, isDesktop: true, showCode() {}, show() {} }; });
    const page = await ctx.newPage();
    const olderReqs = { n: 0 };
    page.on("request", (r) => { if (/\/v1\/timeline\?[^#]*before=/.test(r.url())) olderReqs.n++; });
    await page.goto(`${base}/${SHELL ? "?shell=1" : ""}`);
    const r = await api(page, "POST", "/v1/session", { email: "owner@example.test", password: "dev-password-123", deviceName: "mock-web" });
    if (r.status !== 200) throw new Error(`sign-in ${r.status}`);
    await page.goto(`${base}/${SHELL ? "?shell=1" : ""}`);
    await page.waitForSelector(".msg", { timeout: 20000 });
    const run = async (label, fn) => { await fn(); if (!(await sample(page, `${name} ${label}`, olderReqs))) failures++; };
    await run("first load", async () => {});
    await run("cold start (cache warm)", async () => { await page.reload(); await page.waitForSelector(".msg", { timeout: 20000 }); });
    await run("rail Keep -> Lucina", async () => { await go(page, "/keep"); await page.waitForTimeout(1200); await go(page, "/"); });
    await run("rail Shopping -> Lucina", async () => { await go(page, "/shopping"); await page.waitForTimeout(800); await go(page, "/"); });
    await run("scrolled up, re-tap (chat-latest)", async () => {
      const box = await page.locator(".messages").boundingBox();
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      for (let i = 0; i < 6; i++) { await page.mouse.wheel(0, -600); await page.waitForTimeout(60); }
      await page.waitForTimeout(400);
      await page.evaluate(() => dispatchEvent(new CustomEvent("secretary:chat-latest")));
    });
    await run("new message arrives", async () => {
      await api(page, "POST", "/v1/_mock/timeline", { channel: "web", role: "lucina", text: "A new message " + Date.now(), meta: { device: "mock-web" } });
    });
    await run("view resize (shell card)", async () => {
      await page.setViewportSize({ width: Math.round(viewport.width * 0.7), height: Math.round(viewport.height * 0.6) });
      await page.waitForTimeout(150);
      await page.setViewportSize(viewport);
    });
    await run("hidden then shown", async () => {
      await page.evaluate(() => { Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true }); document.dispatchEvent(new Event("visibilitychange")); });
      await page.waitForTimeout(300);
      await page.evaluate(() => { Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true }); document.dispatchEvent(new Event("visibilitychange")); });
    });
    const say = (text) => api(page, "POST", "/v1/_mock/timeline", { channel: "web", role: "lucina", text, meta: { device: "mock-web" } });
    const decision = (i) => `claude-code (session ${i}) needs a decision: May I do the thing number ${i}? `.repeat(4) + "\n\n1. Yes (recommended)\n2. No\n\nTap an option.";
    await run("view grows, then a message arrives", async () => {
      await page.setViewportSize({ width: viewport.width, height: viewport.height - 200 });
      await page.waitForTimeout(300);
      await page.setViewportSize(viewport);
      await page.waitForTimeout(300);
      await say(decision(1));
    });
    await run("background tab while 4 decisions arrive", async () => {
      const other = await ctx.newPage();
      await other.goto("about:blank");
      await other.bringToFront();
      await page.waitForTimeout(300);
      for (let i = 2; i < 6; i++) { await say(decision(i)); await page.waitForTimeout(250); }
      await page.waitForTimeout(800);
      await page.bringToFront();
      await other.close();
    });
    await run("content above shrinks", async () => {
      await page.evaluate(() => { const msgs = [...document.querySelectorAll(".msg")]; for (const m of msgs.slice(-6, -1)) m.style.display = "none"; });
      await page.waitForTimeout(300);
      await say(decision(7));
    });
    if (process.env.SHOT) await page.screenshot({ path: `${process.env.SHOT}-${name}.png` });
    await ctx.close();
  }
} finally {
  await browser.close();
}
console.log(failures ? `${failures} FAILED` : "ALL PASS");
process.exit(failures ? 1 : 0);
