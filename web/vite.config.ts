import { createReadStream, existsSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";

/**
 * MediaPipe Face Landmarker runtime (face tracking, F1), served from our own origin: the WASM files
 * from the pinned npm package at /mediapipe/wasm/ (dev: straight from node_modules; build: copied
 * into dist). The model is vendored in public/mediapipe/ (face_landmarker.task, float16 v1, from
 * storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/, sha256
 * 64184e22…bc9ff; terms in its MediaPipe model card). Nothing is fetched from a CDN, and the
 * files are only requested when the owner turns face tracking on.
 */
const MEDIAPIPE_WASM = ["vision_wasm_internal.js", "vision_wasm_internal.wasm", "vision_wasm_nosimd_internal.js", "vision_wasm_nosimd_internal.wasm"];
function mediapipeWasm(): Plugin {
  const dir = join(dirname(createRequire(import.meta.url).resolve("@mediapipe/tasks-vision")), "wasm");
  const type = (f: string) => (f.endsWith(".wasm") ? "application/wasm" : "text/javascript");
  return {
    name: "secretary-mediapipe-wasm",
    configureServer(server) {
      server.middlewares.use("/mediapipe/wasm", (req, res, next) => {
        const name = (req.url ?? "/").split("?")[0]!.replace(/^\//, "");
        const file = join(dir, name);
        if (!MEDIAPIPE_WASM.includes(name) || !existsSync(file)) return next();
        res.setHeader("Content-Type", type(name));
        res.setHeader("Content-Length", String(statSync(file).size));
        createReadStream(file).pipe(res);
      });
    },
    generateBundle() {
      for (const name of MEDIAPIPE_WASM) {
        this.emitFile({ type: "asset", fileName: `mediapipe/wasm/${name}`, source: readFileSync(join(dir, name)) });
      }
    },
  };
}

// The API lives on the same origin in production. In development `/v1` is
// proxied to the mock server (mock/server.mjs) or to SECRETARY_API if set.
const apiTarget =
  process.env.SECRETARY_API ?? `http://127.0.0.1:${process.env.MOCK_PORT ?? "8791"}`;

const proxy = {
  "/v1": { target: apiTarget, changeOrigin: false },
};

export default defineConfig({
  plugins: [solid(), mediapipeWasm()],
  // The Lucina stage package is linked from ../packages; use one copy of three.
  resolve: { dedupe: ["three"], alias: { "@secretary/lucina-stage": join(import.meta.dirname, "../packages/lucina-stage/src/index.ts") } },
  // The shopping illustrations live in ../assets/icons (shared with iOS); let Vite read them.
  server: { port: 5173, strictPort: true, proxy, fs: { allow: [".."] } },
  preview: { port: 4173, strictPort: true, proxy },
  build: {
    // The server's CSP serves fonts and icons from this origin, without data:.
    assetsInlineLimit: 0,
    outDir: "dist",
    assetsDir: "assets",
    target: "es2022",
    sourcemap: false,
    // The polyfill is unnecessary for the browsers we target and keeps the
    // output free of anything resembling inline script.
    modulePreload: { polyfill: false },
    // The Lucina stage chunk (three.js, ~700 kB) is loaded lazily by Chat only.
    chunkSizeWarningLimit: 800,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
