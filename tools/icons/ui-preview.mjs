#!/usr/bin/env node
// Screenshots of the web Shopping view with the bundled item icons, at 390 and 1440 px, light and dark.
//
// Needs the mock API and a built web app running (both local):
//   (cd web && npm run build && node mock/server.mjs & npx vite preview &)
// then: PLAYWRIGHT_DIR=/path/to/node_modules/playwright [CHROME_PATH=/usr/bin/google-chrome] node tools/icons/ui-preview.mjs [out-dir] [base-url]
// Signs in with the mock owner account from web/mock/server.mjs and seeds the default list with
// the icon-pilot items (half to buy, half recent).
import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_DIR ?? "playwright");

const out = process.argv[2] ?? `${process.env.HOME}/scratch/icon-pilot/ui`;
const base = process.argv[3] ?? "http://127.0.0.1:4173";
const EMAIL = process.env.MOCK_OWNER_EMAIL ?? "owner@example.test";
const PASSWORD = process.env.MOCK_OWNER_PASSWORD ?? "dev-password-123";
const PILOT = [
  "banana", "apple", "tomato", "broccoli", "carrot", "avocado", "milk", "yogurt", "cheese", "butter", "eggs", "ham",
  "chicken", "white-fish", "bread", "rye-bread", "croissant", "pasta", "rice", "olive-oil", "flour", "coffee", "water",
  "red-wine", "toilet-paper", "dish-soap", "sponges", "toothpaste", "shampoo", "ice-cream",
];
mkdirSync(out, { recursive: true });

async function api(page, method, path, body) {
  return page.evaluate(
    async ([method, path, body]) => {
      const r = await fetch(path, {
        method,
        headers: { "Content-Type": "application/json", "X-Secretary-CSRF": "1" },
        body: body === undefined ? undefined : JSON.stringify(body),
        credentials: "same-origin",
      });
      return { status: r.status, json: await r.json().catch(() => null) };
    },
    [method, path, body],
  );
}

async function signIn(page) {
  await page.goto(`${base}/`);
  const r = await api(page, "POST", "/v1/session", { email: EMAIL, password: PASSWORD, deviceName: "icon-preview" });
  if (r.status !== 200) throw new Error(`sign-in failed: ${r.status} ${JSON.stringify(r.json)}`);
}

async function seed(page) {
  const list = await api(page, "GET", "/v1/shopping/lists/default");
  let revision = list.json?.revision ?? list.json?.list?.revision;
  const opId = () => `pilot-${Math.random().toString(36).slice(2)}`;
  // Start from an empty list so the screenshot shows only pilot items.
  const existing = (list.json?.items ?? list.json?.list?.items ?? []).filter((i) => i.status !== "removed");
  const ops = existing.map((i) => ({ type: "remove", opId: opId(), itemId: i.id }));
  ops.push(...PILOT.map((id) => ({ type: "add", opId: opId(), itemId: `pilot-${id}`, catalogId: id })));
  ops.push(...PILOT.slice(16).map((id) => ({ type: "purchase", opId: opId(), itemId: `pilot-${id}` })));
  const r = await api(page, "POST", "/v1/shopping/lists/default/ops", { expectedRevision: revision, ops });
  if (r.status >= 300) throw new Error(`seed failed: ${r.status} ${JSON.stringify(r.json)}`);
}

const browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {});
try {
  {
    const ctx = await browser.newContext();
    const page = await ctx.newPage();
    await signIn(page);
    await seed(page);
    await ctx.close();
  }
  for (const [name, viewport, dpr] of [
    ["390", { width: 390, height: 844 }, 3],
    ["1440", { width: 1440, height: 900 }, 1],
  ]) {
    for (const scheme of ["light", "dark"]) {
      const ctx = await browser.newContext({ viewport, deviceScaleFactor: dpr, colorScheme: scheme, locale: "en-GB" });
      await ctx.addInitScript(() => {
        try {
          localStorage.setItem("secretary.locale", "en");
        } catch {}
      });
      const page = await ctx.newPage();
      await signIn(page);
      await page.goto(`${base}/shopping`);
      await page.waitForSelector(".tile .item-art", { timeout: 15000 });
      await page.waitForFunction(() => [...document.images].every((i) => i.complete && i.naturalWidth > 0));
      await page.waitForTimeout(600); // tile entry animation
      await page.screenshot({ path: `${out}/shopping-${name}-${scheme}.png`, fullPage: true });
      // A catalog section expanded, to show catalog (teal) tiles and the section list.
      const head = page.locator(".bring-section-toggle", { hasText: /Fruit|Frutta|Frugt/ }).first();
      if (await head.count()) {
        await head.click();
        await page.waitForTimeout(500);
        await head.scrollIntoViewIfNeeded();
        await page.screenshot({ path: `${out}/shopping-${name}-${scheme}-produce.png`, fullPage: false });
      }
      const broken = await page.evaluate(() => [...document.images].filter((i) => i.complete && i.naturalWidth === 0).map((i) => i.src));
      const arts = await page.locator(".item-art").count();
      console.log(`${name} ${scheme}: ${arts} illustrations, ${broken.length} broken ${broken.join(" ")}`);
      await ctx.close();
    }
  }
} finally {
  await browser.close();
}
