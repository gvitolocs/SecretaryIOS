#!/usr/bin/env node
// Turntable preview MP4 of a GLB for Telegram/Discord (lucina-meshy `preview.rs` runs this).
//
// CPU only: headless Chrome with ANGLE on SwiftShader. The page checks the WebGL renderer and
// refuses to draw unless it is SwiftShader, so nothing ever lands on nezopt's GPU (Qwen owns it).
// three.js comes from packages/lucina-stage (the same version the apps use). The page is offline:
// every request is answered from local files (three, the GLB) or aborted. Frames are rendered one
// by one (deterministic time, no real-time clock), piped to ffmpeg/libx264 (yuv420p, faststart).
//
// node render-turntable.mjs --glb in.glb --out out.mp4 [--seconds 5] [--width 1280] [--height 720]
//      [--fps 30] [--label "Knight"] [--clip NAME] [--max-mb 8]
// Prints one JSON line: {"ok":true,"out":…,"bytes":…,"frames":…,"seconds":…,"clip":…,"renderer":…}
// or {"ok":false,"error":…}.
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync, readFileSync, renameSync, statSync, unlinkSync } from 'node:fs';
import { dirname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '../..');

function fail(error) {
  process.stdout.write(JSON.stringify({ ok: false, error: String(error).slice(0, 500) }) + '\n');
  process.exit(1);
}

function parseArgs(argv) {
  const out = { seconds: 5, width: 1280, height: 720, fps: 30, label: '', clip: '', maxMb: 8 };
  for (let i = 0; i < argv.length; i += 1) {
    const k = argv[i];
    const v = argv[i + 1];
    const take = () => { i += 1; return v; };
    if (k === '--glb') out.glb = take();
    else if (k === '--out') out.out = take();
    else if (k === '--seconds') out.seconds = Number(take());
    else if (k === '--width') out.width = Number(take());
    else if (k === '--height') out.height = Number(take());
    else if (k === '--fps') out.fps = Number(take());
    else if (k === '--label') out.label = String(take() ?? '');
    else if (k === '--clip') out.clip = String(take() ?? '');
    else if (k === '--max-mb') out.maxMb = Number(take());
    else fail(`unknown argument ${k}`);
  }
  if (!out.glb || !out.out) fail('--glb and --out are required');
  if (!(out.seconds >= 1 && out.seconds <= 12)) fail('--seconds: 1 to 12');
  if (!(out.fps >= 10 && out.fps <= 60)) fail('--fps: 10 to 60');
  if (!(out.width >= 160 && out.width <= 1920 && out.height >= 160 && out.height <= 1920)) fail('size: 160 to 1920');
  out.width -= out.width % 2;
  out.height -= out.height % 2;
  return out;
}

const opts = parseArgs(process.argv.slice(2));
const glbPath = resolve(opts.glb);
const outPath = resolve(opts.out);
if (outPath === glbPath) fail('--out must not be the GLB');
if (!existsSync(glbPath)) fail('GLB not found');
const glb = readFileSync(glbPath);
if (glb.length < 20 || glb.readUInt32LE(0) !== 0x46546c67) fail('not a binary glTF (GLB)');

const threeDir = resolve(process.env.MESHY_PREVIEW_THREE || join(REPO, 'packages/lucina-stage/node_modules/three'));
const pwDir = resolve(process.env.MESHY_PREVIEW_PLAYWRIGHT || '/home/nez/scratch/lucina-compare/stagetest/node_modules/playwright-core');
const chromePath = process.env.MESHY_PREVIEW_CHROME || '/usr/bin/google-chrome';
const ffmpegPath = process.env.FFMPEG_PATH || 'ffmpeg';
if (!existsSync(join(threeDir, 'build/three.module.js'))) fail(`three.js not found at ${threeDir}`);
if (!existsSync(join(pwDir, 'package.json'))) fail(`playwright-core not found at ${pwDir}`);
const { chromium } = createRequire(import.meta.url)(pwDir);

const ORIGIN = 'http://preview.lucina.invalid';

const PAGE = `<!doctype html><html><head><meta charset="utf-8">
<style>html,body{margin:0;background:#0b1020;overflow:hidden}canvas{display:block}</style>
<script type="importmap">{"imports":{"three":"/three/build/three.module.js","three/addons/":"/three/examples/jsm/"}}</script>
</head><body><script type="module" src="/main.js"></script></body></html>`;

const MAIN = `
import * as THREE from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';
import { MeshoptDecoder } from 'three/addons/libs/meshopt_decoder.module.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';

window.setup = async ({ width, height, label, clip }) => {
  const canvas = document.createElement('canvas');
  canvas.width = width; canvas.height = height;
  const gl = canvas.getContext('webgl2', { antialias: true, preserveDrawingBuffer: true });
  if (!gl) throw new Error('no WebGL2');
  const dbg = gl.getExtension('WEBGL_debug_renderer_info');
  const rendererName = String(dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER));
  if (!/swiftshader/i.test(rendererName)) throw new Error('refusing to render on a GPU: ' + rendererName);
  const renderer = new THREE.WebGLRenderer({ canvas, context: gl, antialias: true, preserveDrawingBuffer: true });
  renderer.setSize(width, height, false);
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 0.85;
  const scene = new THREE.Scene();
  const bg = document.createElement('canvas'); bg.width = 4; bg.height = 256;
  const g = bg.getContext('2d'); const grad = g.createLinearGradient(0, 0, 0, 256);
  grad.addColorStop(0, '#26304f'); grad.addColorStop(1, '#0b1020'); g.fillStyle = grad; g.fillRect(0, 0, 4, 256);
  const bgTex = new THREE.CanvasTexture(bg); bgTex.colorSpace = THREE.SRGBColorSpace; scene.background = bgTex;
  const pmrem = new THREE.PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environmentIntensity = 0.55;
  scene.add(new THREE.HemisphereLight(0xdfe8ff, 0x1a1d29, 0.6));
  const key = new THREE.DirectionalLight(0xffffff, 1.6); key.position.set(3, 5, 4); scene.add(key);

  const loader = new GLTFLoader(); loader.setMeshoptDecoder(MeshoptDecoder);
  const buf = await (await fetch('/model.glb')).arrayBuffer();
  const gltf = await loader.parseAsync(buf, '/');
  const model = gltf.scene;
  scene.add(model);
  model.updateMatrixWorld(true);
  const mixer = new THREE.AnimationMixer(model);
  let action = null; let clipName = null; let clipDuration = 0;
  if (gltf.animations.length) {
    const want = (clip || '').toLowerCase();
    const c = gltf.animations.find((a) => want && a.name.toLowerCase().includes(want)) || gltf.animations[0];
    action = mixer.clipAction(c); action.play(); clipName = c.name; clipDuration = c.duration;
    mixer.setTime(0);
  }
  const box = new THREE.Box3().setFromObject(model, true);
  const size = box.getSize(new THREE.Vector3()); const center = box.getCenter(new THREE.Vector3());
  model.position.sub(new THREE.Vector3(center.x, box.min.y, center.z));
  const pivot = new THREE.Group(); scene.remove(model); pivot.add(model); scene.add(pivot);
  const radius = Math.max(size.x, size.z) * 0.5 || 1;
  const disc = new THREE.Mesh(new THREE.CircleGeometry(radius * 1.25, 64), new THREE.MeshBasicMaterial({ color: 0x000000, transparent: true, opacity: 0.25 }));
  disc.rotation.x = -Math.PI / 2; disc.position.y = 0.001; scene.add(disc);
  // Fit the bounding sphere (it never leaves the frame while the model turns).
  const sphereR = Math.max(size.length() * 0.5, 1e-3);
  const vfov = 30;
  const camera = new THREE.PerspectiveCamera(vfov, width / height, sphereR / 100, sphereR * 100);
  const halfV = THREE.MathUtils.degToRad(vfov / 2);
  const halfH = Math.atan(Math.tan(halfV) * (width / height));
  const dist = (sphereR / Math.sin(Math.min(halfV, halfH))) * 1.08;
  const target = new THREE.Vector3(0, size.y * 0.5, 0);
  const elev = THREE.MathUtils.degToRad(12);
  camera.position.set(0, target.y + dist * Math.sin(elev), dist * Math.cos(elev));
  camera.lookAt(target);
  const out = document.createElement('canvas'); out.width = width; out.height = height;
  const ctx = out.getContext('2d');
  let triangles = 0;
  model.traverse((o) => { if (o.isMesh && o.geometry) { const i = o.geometry.index; triangles += (i ? i.count : o.geometry.attributes.position.count) / 3; } });
  window.frame = (t, turn) => {
    if (action) mixer.setTime(t);
    pivot.rotation.y = turn * Math.PI * 2;
    renderer.render(scene, camera);
    ctx.drawImage(canvas, 0, 0);
    if (label) {
      const pad = Math.round(height * 0.035); const fs = Math.round(height * 0.04);
      ctx.font = '600 ' + fs + 'px sans-serif'; ctx.textBaseline = 'bottom';
      ctx.fillStyle = 'rgba(0,0,0,0.45)';
      const w = ctx.measureText(label).width;
      ctx.fillRect(pad - fs * 0.4, height - pad - fs * 1.35, w + fs * 0.8, fs * 1.6);
      ctx.fillStyle = '#ffffff'; ctx.fillText(label, pad, height - pad);
    }
    return out.toDataURL('image/jpeg', 0.93);
  };
  return { renderer: rendererName, clip: clipName, clipDuration, clips: gltf.animations.map((a) => a.name), triangles: Math.round(triangles) };
};
window.ready = true;
`;

function serveFile(route, file, type) {
  return route.fulfill({ status: 200, contentType: type, body: readFileSync(file) });
}

const started = Date.now();
const tmpOut = outPath + '.part.mp4';
let browser;
try {
  browser = await chromium.launch({
    executablePath: chromePath,
    headless: true,
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist', '--mute-audio', '--no-first-run'],
  });
  const page = await browser.newPage({ viewport: { width: Math.min(opts.width, 1280), height: Math.min(opts.height, 1280) } });
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.route('**/*', async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== ORIGIN) return route.abort();
    const p = decodeURIComponent(url.pathname);
    if (p === '/' || p === '/index.html') return route.fulfill({ status: 200, contentType: 'text/html', body: PAGE });
    if (p === '/main.js') return route.fulfill({ status: 200, contentType: 'text/javascript', body: MAIN });
    if (p === '/model.glb') return route.fulfill({ status: 200, contentType: 'model/gltf-binary', body: glb });
    if (p.startsWith('/three/')) {
      const file = normalize(join(threeDir, p.slice('/three/'.length)));
      if (!file.startsWith(threeDir + sep) || !existsSync(file)) return route.fulfill({ status: 404, body: '' });
      return serveFile(route, file, 'text/javascript');
    }
    return route.fulfill({ status: 404, body: '' });
  });
  await page.goto(ORIGIN + '/index.html');
  await page.waitForFunction(() => window.ready === true, null, { timeout: 60000 });
  const info = await page.evaluate((o) => window.setup(o), { width: opts.width, height: opts.height, label: opts.label, clip: opts.clip });
  const frames = Math.round(opts.seconds * opts.fps);
  // Static models turn once; animated ones play the clip (looping) with a slower half turn.
  const turns = info.clip ? 0.5 : 1;
  const ff = spawn(ffmpegPath, ['-hide_banner', '-loglevel', 'error', '-y', '-f', 'image2pipe', '-framerate', String(opts.fps), '-i', '-',
    '-c:v', 'libx264', '-preset', 'veryfast', '-crf', '21', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', '-an', '-f', 'mp4', tmpOut],
  { stdio: ['pipe', 'ignore', 'pipe'] });
  let ffErr = '';
  ff.stderr.on('data', (d) => { ffErr += String(d); });
  const ffDone = new Promise((ok) => ff.on('close', ok));
  for (let i = 0; i < frames; i += 1) {
    const t = info.clip && info.clipDuration > 0 ? (i / opts.fps) % info.clipDuration : i / opts.fps;
    const dataUrl = await page.evaluate(([tt, turn]) => window.frame(tt, turn), [t, (i / frames) * turns]);
    const jpg = Buffer.from(dataUrl.slice(dataUrl.indexOf(',') + 1), 'base64');
    if (!ff.stdin.write(jpg)) await new Promise((ok) => ff.stdin.once('drain', ok));
  }
  ff.stdin.end();
  const code = await ffDone;
  if (code !== 0) throw new Error('ffmpeg failed: ' + ffErr.slice(0, 300));
  let bytes = statSync(tmpOut).size;
  if (bytes > opts.maxMb * 1024 * 1024) {
    // Re-encode at a capped bitrate (length and frame rate stay).
    const kbps = Math.floor((opts.maxMb * 8 * 1024 * 0.92) / opts.seconds);
    const capped = outPath + '.cap.mp4';
    const r = spawn(ffmpegPath, ['-hide_banner', '-loglevel', 'error', '-y', '-i', tmpOut, '-c:v', 'libx264', '-preset', 'veryfast', '-b:v', kbps + 'k', '-maxrate', kbps + 'k', '-bufsize', kbps * 2 + 'k', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', '-an', capped]);
    const rc = await new Promise((ok) => r.on('close', ok));
    if (rc !== 0) throw new Error('ffmpeg re-encode failed');
    renameSync(capped, tmpOut);
    bytes = statSync(tmpOut).size;
  }
  renameSync(tmpOut, outPath);
  process.stdout.write(JSON.stringify({ ok: true, out: outPath, bytes, frames, seconds: opts.seconds, width: opts.width, height: opts.height,
    fps: opts.fps, clip: info.clip, clips: info.clips, triangles: info.triangles, renderer: info.renderer, renderS: (Date.now() - started) / 1000,
    pageErrors: errors.slice(0, 3) }) + '\n');
} catch (e) {
  try { if (existsSync(tmpOut)) unlinkSync(tmpOut); } catch { /* ignore */ }
  fail(e && e.message ? e.message : e);
} finally {
  if (browser) await browser.close().catch(() => {});
}
