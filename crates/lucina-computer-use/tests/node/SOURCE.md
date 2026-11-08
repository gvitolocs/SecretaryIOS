Runs the retired Node Lucina's own `parseComputerResult` so the Rust port can
be compared with it.

- `computer-use.mjs` is `src/computer-use.js` of the retired Node Lucina
  (repo `Projects/Archive/lucina-node`, tag `node-lucina-final`), copied
  **verbatim** (sha256 45c78a31ebfe02f35ef363048dd39d2a97867e336f1998be51d91b7a248fed1e,
  137 lines). Only the extension changed, `.js` → `.mjs`, so Node loads it as
  an ES module without a `package.json`. Do not edit it: the whole point is
  that `tests/differential.rs` compares the Rust port against the real Node
  implementation, not against a hand-written expectation.
- `diff.mjs` parses every fixture given on the command line and prints one
  JSON array: `{file, ok, value}` or `{file, ok: false, errorKind, message}`.
  `errorKind` is derived from the Node error message so Rust can compare a
  stable code instead of prose.

Re-vendor with:

    cp ~/Projects/Archive/lucina-node/src/computer-use.js \
       crates/lucina-computer-use/tests/node/computer-use.mjs
