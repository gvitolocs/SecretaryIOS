import { createReadStream, existsSync, mkdirSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, extname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vitest/config';
import { synthSpeechWav } from './src/synth.ts';

/**
 * Dev-only helpers (never part of the library build):
 * - `/dev-assets/*` serves LUCINA_ASSETS_DIR (default ~/scratch/lucina-avatar/out), where the
 *   converter writes lucina.glb + manifest.json. The model must not be redistributed, so it is
 *   never copied into the repo or the build.
 * - `POST /mock/v1/tts` returns synthetic speech-like WAV, to exercise voice.speak() without the
 *   private server.
 */
function devAssets(): Plugin {
  const dir = resolve(process.env.LUCINA_ASSETS_DIR ?? join(homedir(), 'scratch/lucina-avatar/out'));
  const types: Record<string, string> = {
    '.glb': 'model/gltf-binary',
    '.gltf': 'model/gltf+json',
    '.json': 'application/json',
    '.png': 'image/png',
    '.webp': 'image/webp',
    '.jpg': 'image/jpeg',
    '.ktx2': 'image/ktx2',
    '.bin': 'application/octet-stream',
  };
  return {
    name: 'lucina-dev-assets',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use('/dev-assets', (req, res, next) => {
        const rel = decodeURIComponent((req.url ?? '/').split('?')[0] ?? '/');
        const file = resolve(dir, '.' + rel);
        if (!file.startsWith(dir + sep) || !existsSync(file) || !statSync(file).isFile()) {
          res.statusCode = 404;
          res.end('not found');
          return;
        }
        res.setHeader('Content-Type', types[extname(file)] ?? 'application/octet-stream');
        res.setHeader('Cache-Control', 'no-store');
        res.setHeader('Content-Length', String(statSync(file).size));
        createReadStream(file).on('error', next).pipe(res);
      });
      // Saves canvas captures (PNG data URLs) for visual review: POST /dev-capture?name=foo
      server.middlewares.use('/dev-capture', (req, res) => {
        const name = (new URL(req.url ?? '/', 'http://x').searchParams.get('name') ?? 'capture').replace(/[^\w.-]/g, '_');
        const chunks: Buffer[] = [];
        req.on('data', (c: Buffer) => chunks.push(c));
        req.on('end', () => {
          const b64 = Buffer.concat(chunks).toString('utf8').replace(/^data:image\/png;base64,/, '');
          const out = join(process.env.LUCINA_CAPTURE_DIR ?? '/tmp/lucina-stage-captures', `${name}.png`);
          mkdirSync(dirname(out), { recursive: true });
          writeFileSync(out, Buffer.from(b64, 'base64'));
          res.end(out);
        });
      });
      server.middlewares.use('/mock/v1/tts', (req, res) => {
        if (req.method !== 'POST') {
          res.statusCode = 405;
          res.end();
          return;
        }
        let body = '';
        req.on('data', (c: Buffer) => (body += c.toString('utf8')));
        req.on('end', () => {
          let text = 'ciao';
          try {
            text = String((JSON.parse(body) as { text?: unknown }).text ?? text);
          } catch {
            res.statusCode = 400;
            res.end('bad json');
            return;
          }
          const vowels = text.replace(/[^aeiou .,!?-]/gi, '').slice(0, 80) || 'a e i o u';
          const wav = Buffer.from(synthSpeechWav({ pattern: vowels }));
          res.setHeader('Content-Type', 'audio/wav');
          res.setHeader('Content-Length', String(wav.length));
          res.end(wav);
        });
      });
    },
  };
}

export default defineConfig(({ mode }) => {
  const standalone = mode === 'standalone';
  return {
    plugins: [devAssets()],
    server: { host: '0.0.0.0', port: 5181, strictPort: true },
    build: {
      target: 'es2022',
      sourcemap: true,
      emptyOutDir: !standalone,
      lib: {
        entry: fileURLToPath(new URL('./src/index.ts', import.meta.url)),
        formats: ['es'],
        fileName: () => (standalone ? 'lucina-stage.standalone.js' : 'lucina-stage.js'),
      },
      rollupOptions: {
        // The default build leaves three to the host bundler (the SolidJS app); the standalone
        // build bundles it for a plain <script type="module"> page (iOS WKWebView).
        external: standalone ? [] : [/^three($|\/)/],
        // Lib-mode ES output keeps whitespace; ask the bundler's minifier directly for the standalone file.
        output: standalone ? { minify: true } : {},
      },
    },
    test: {
      include: ['test/**/*.test.ts'],
      environment: 'node',
    },
  };
});
