// Asks a development Paseo daemon for what its CLI doesn't print (diffs, the file
// explorer, checkout status), through Paseo's own client, for tests/paseo_diff.rs.
//   node paseo_oracle.mjs <host:port> <op> <cwd> [arg]
//   op: diff-uncommitted | diff-base | diff-all | status | ls <path> | read <path>
// Prints one JSON line; on a daemon error, {"error": "<message>"}.
import { pathToFileURL } from "node:url";

const CLI = process.env.PASEO_CLI_DIST ?? "/home/nez/.local/lib/node_modules/@getpaseo/cli/dist";
const { connectToDaemon } = await import(pathToFileURL(`${CLI}/utils/client.js`).href);
const [host, op, cwd, arg] = process.argv.slice(2);
const client = await connectToDaemon({ target: { kind: "endpoint", host } });
let out;
try {
  if (op.startsWith("diff-")) out = await client.getCheckoutDiff(cwd, { mode: op.slice(5) });
  else if (op === "status") {
    // Paseo serves a cached snapshot; refresh it first, as its UI does.
    await client.checkoutRefresh(cwd).catch(() => {});
    out = await client.getCheckoutStatus(cwd);
  }
  else if (op === "ls") out = await client.listDirectory(cwd, arg ?? ".");
  else if (op === "read") {
    const f = await client.readFile(cwd, arg);
    out = { ...f, bytes: undefined, text: Buffer.from(Object.values(f.bytes ?? {})).toString("utf8") };
  } else throw new Error(`unknown op ${op}`);
} catch (e) {
  out = { error: e instanceof Error ? e.message : String(e) };
} finally {
  await client.close().catch(() => {});
}
console.log(JSON.stringify(out, (k, v) => (k === "requestId" ? undefined : v)));
