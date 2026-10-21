#!/usr/bin/env node
// `npm run dev:mock`: start the mock API and the Vite dev server together.
// Extra arguments are passed to Vite (e.g. `npm run dev:mock -- --host`).

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const server = fileURLToPath(new URL("./server.mjs", import.meta.url));
const vite = fileURLToPath(new URL("../node_modules/vite/bin/vite.js", import.meta.url));

const children = [];
let exiting = false;

function shutdown(code) {
  if (exiting) return;
  exiting = true;
  for (const c of children) if (c.exitCode === null) c.kill("SIGTERM");
  setTimeout(() => process.exit(code), 300);
}

function run(args) {
  const child = spawn(process.execPath, args, { stdio: "inherit", env: process.env });
  child.on("exit", (code) => shutdown(code ?? 0));
  children.push(child);
}

run([server]);
run([vite, ...process.argv.slice(2)]);

for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => shutdown(0));
