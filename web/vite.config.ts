import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

// The API lives on the same origin in production. In development `/v1` is
// proxied to the mock server (mock/server.mjs) or to SECRETARY_API if set.
const apiTarget =
  process.env.SECRETARY_API ?? `http://127.0.0.1:${process.env.MOCK_PORT ?? "8791"}`;

const proxy = {
  "/v1": { target: apiTarget, changeOrigin: false },
};

export default defineConfig({
  plugins: [solid()],
  server: { port: 5173, strictPort: true, proxy },
  preview: { port: 4173, strictPort: true, proxy },
  build: {
    outDir: "dist",
    assetsDir: "assets",
    target: "es2022",
    sourcemap: false,
    // The polyfill is unnecessary for the browsers we target and keeps the
    // output free of anything resembling inline script.
    modulePreload: { polyfill: false },
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
