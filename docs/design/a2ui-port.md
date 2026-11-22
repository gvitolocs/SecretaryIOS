# A2UI port: record

The port of A2UI (Agent-to-User Interface, <https://github.com/a2ui-project/a2ui>) into Lucina's chat. This file is the
adopt / adapt / reference / reject record that `docs/porting-guidelines.md` section 3 asks for, and the running list of
what is done, what is left and what was rejected.

- Original: commit `5ff1b05f02d39f969b52a942e78e864ef3ac01fa` (2026-11-20). Licence read from its `LICENSE`: Apache-2.0.
  Pin and licence are in `docs/upstreams.toml` (id `a2ui`).
- Specification ported: v1.0 (`specification/v1_0/`), the version the original marks current.
- Oracle: the original's conformance suite, catalogues and v1.0 specification, vendored unmodified under
  `third_party/a2ui/upstream/` (see `third_party/a2ui/README.md`). A case is never edited, deleted or
  skipped to make a change pass. A case we cannot meet is listed below with its reason.

## Where we started

Lucina message format v1 (`docs/lucina-message-format.md`): 1-4 text bubbles, up to 8 flat widgets from a closed list,
up to 4 buttons of five kinds. A reply arrives whole. Widgets cannot nest, cannot take input and are never changed after
they are sent. The format is written by hand three times (Rust, TypeScript, Swift). Android shows the web chat.

## What A2UI is

The server sends a user interface as data: a flat list of components that reference each other by id (a tree), a data
model the components bind to, and the user's actions come back with the bound values. v1.0 has six agent-to-renderer
messages (`createSurface`, `updateComponents`, `updateDataModel`, `deleteSurface`, `callRendererFunction`,
`agentFunctionResponse`) and four renderer-to-agent messages (`action`, `callAgentFunction`,
`rendererFunctionResponse`, `error`). The basic catalogue has 18 components and 14 functions. The original has no Rust
implementation.

## Verdicts

ADOPT = used as is. ADAPT = behaviour reimplemented our way, Rust first. REFERENCE = read only. REJECT = not taken.

| # | A2UI capability | Before the port | Verdict |
|---|---|---|---|
| 1 | Component tree with nesting: Row, Column, List, Card, Tabs, Modal, Divider | flat widgets | ADAPT |
| 2 | Display components: Text, Image, Icon, Video, AudioPlayer | bubbles; image on web only | ADAPT |
| 3 | Input components: TextField, CheckBox, ChoicePicker, Slider, DateTimeInput | none | ADAPT |
| 4 | Data model and binding: JSON Pointer paths, literal or bound values, input write-back, list templates, `@index` | none | ADAPT |
| 5 | Functions: required, regex, length, numeric, email, formatString, formatNumber, formatCurrency, formatDate, pluralize, and, or, not, openUrl | none | ADAPT |
| 6 | Actions with context (event name, resolved values, readable sentence) | five button kinds | ADAPT; the five kinds stay |
| 7 | Surfaces: create, update components, update data, delete; several at once | append-only messages | ADAPT |
| 8 | Progressive rendering while the stream arrives | reply arrives whole | ADAPT |
| 9 | Capability negotiation and explicit catalogue resolution | none | ADAPT |
| 10 | Custom catalogues | closed hand-written list | ADOPT the mechanism: format v1's widgets become the Lucina catalogue |
| 11 | Validation, error messages, generate-validate-retry | normalise and drop, one retry | ADAPT |
| 12 | Composition rules, identifier rules, reserved `@` keys | none | ADAPT |
| 13 | Accessibility attributes | by hand per widget | ADAPT |
| 14 | `sendDataModel` (surface data sent with an action, only for surfaces that agent made) | none | ADAPT |
| 15 | Two-way function calls with caller allow-list and user-gesture gate | none | ADAPT |
| 16 | Theme left to the host (v1.0 removed agent-set colours) | Lucina design tokens | ADOPT the stance |
| 17 | Chart catalogue (VegaChart) | no chart widget | ADAPT the capability as a native chart component; REJECT the Vega engine (large JavaScript, web only) |
| 18 | MCP catalogue: McpApp (third-party HTML application in a sandboxed frame), callMcpTool | none | REJECT: no raw HTML in Lucina's chat, media only from our own API. The owner may overturn this. |
| 19 | Model output notations Direct JSON and Express, prompt text generated from the catalogue, streaming parser | a composer call restyles a draft into format v1 | ADAPT |
| 20 | Experimental notations Atom and Elemental; the legacy parser | none | REJECT: experimental and in one language only; legacy is closed to new implementations |
| 21 | Builder (trees built in code) and macros | fixed code builds v1 widgets | ADAPT |
| 22 | Agent-side renderer state | none | ADAPT |
| 23 | Transports A2A and AG-UI | our own HTTP and timeline stream | REJECT: one server and our own apps, nothing to interoperate with |
| 24 | A2UI over MCP | MCP callers get plain text | REFERENCE |
| 25 | Conformance suite | four vectors of our own | ADOPT as the oracle |
| 26 | The 43 basic-catalogue examples | a few mock replies | ADOPT as fixtures for a picture gallery |
| 27 | TypeScript core and Lit, React, Angular renderers | SolidJS | REFERENCE |
| 28 | Swift core and SwiftUI renderer | SwiftUI over the shared Rust core | REFERENCE |
| 29 | Dart and Flutter; Kotlin legacy agent library | not our stack | REJECT |
| 30 | Python and TypeScript agent libraries | Rust server | REFERENCE |
| 31 | Composer tool, eval suite, end-to-end tests, documentation site, skills | - | REFERENCE |
| 32 | Security rules | our own rules | ADOPT theirs; ours stay where stricter |

## Security

- Taken as stated: no executable code on the wire; only catalogue components and functions, resolved explicitly and never
  by fallback; `openUrl` only on a user gesture and only `http`, `https`, `mailto`, `tel`; a function is callable by the
  agent only when the catalogue says so; the agent sees a surface's data only for surfaces it created and only with an
  action; the regex dialect's limits; catalogue references are never fetched.
- Ours stay: media and image sources only from our own API or our own domain (the original has no rule for Image, Video
  and AudioPlayer URLs); the link rules of format v1; no raw HTML ever; hard caps on tree size and depth, and paths and
  written values are capped at 128 levels; a secret is never typed into a form field (the secret sheet stays the only way).
- Every surface also carries a plain-text form, so Telegram, Discord, voice and old app versions keep working. The
  original has none.
- The vendored original under `third_party/a2ui/` is not scanned file by file by the merge gate; it was scanned in one
  invocation at vendoring (2026-11-22: 3.96 MB, no leaks found) and the scan is repeated and recorded in
  `third_party/a2ui/README.md` at every pin move.

## Shape of the port

One Rust crate, `crates/lucina-a2ui`, holds the core: messages, validator, data model, expressions and functions, message
processor, action resolution. The server uses it to check everything before it is stored; the web app uses the same code
compiled to WebAssembly; the iPhone app gets it through SecretaryCore. Drawing stays native and in Lucina's look: SolidJS
components on the web (Android shows them through its WebView), SwiftUI views on the iPhone. Format v1 keeps working.

## Steps

| Step | Content | State |
|---|---|---|
| 1 | Upstream row, vendored oracle, this record | in work |
| 2 | Rust core in the original's build order: data model and expressions; catalogue and schemas; validator and message processor; functions and actions | in work: data model, path resolution and expression parser done |
| 3 | Web: layouts and display components, gallery of the 43 examples, a surface carried in a reply | not started |
| 4 | Forms: inputs, binding, validation, actions back; the Rust builder | not started |
| 5 | Cards that update in place | not started |
| 6 | Lucina composes surfaces herself | not started |
| 7 | iPhone renderer | not started |
| 8 | Streaming build-up, charts, accessibility pass, two-way function calls | not started |

## Conformance status

What `cargo test -p lucina-a2ui` asserts of the suites step 2a answers — the data model, the scope
path and the expression template parser. The counts are the tests' own, not a note beside them.

| suite | cases | passed | skipped (reason) |
|---|---|---|---|
| `core/data_model.yaml` | 52 | 52 | 0 |
| `core/data_context.yaml` | 11 | 11 | 0 |
| `core/expressions.yaml` | 98 | 92 | 6 — `UNSUPPORTED["validate"]`: message processor: step 2b |

## Deliberate differences from the original

Where the port does something the originals do not, decided and stated here rather than found later. Each line: what we
do, and what each original does.

- A float literal worth zero is written `0`, sign and all. TypeScript and the harness guide normalise `-0` the same way;
  Python keeps `-0.0`.
- Whitespace inside an expression is Unicode `White_Space`. Python's `isspace` also takes U+001C–U+001F, and JavaScript's
  `\s` takes U+FEFF but not U+0085. No case of any suite holds one of those characters.
- A JSON Pointer array index is ASCII digits only. Python also accepts a trailing newline there, because `re.match`
  anchors and `int()` strips.
- An integer literal is exact up to `u64::MAX` and a double beyond it. Python is exact at any size; TypeScript is a
  double from 2^53.
- A boolean never equals a number: `true` and `1` are different values here. Python's `==` says they are the same;
  TypeScript's `===` and `Object.is` say they are not.
- A notification is queued by a write and handed out when the host drains, not delivered by callback inside the write;
  the original calls its listeners from the write itself, on a live list (`state/data_model.py:342`). Draining skips an
  observer that is gone by then, which the original's live list also does.
- A pointer of more than 128 segments, or a value nesting deeper than 128, is refused. Neither original refuses it; the
  cap is serde_json's own parsing limit, taken over so that a structure the model can hold is one the wire can deliver.
- `DataModel.resolve_path` is ported as `DataModel::resolve_path`, the join a caller hands a path to
  rather than a second scope resolution, and it keeps its three differences from
  `DataContext.resolve_path`: no trailing-slash stripping, no `.` case, and an empty context joined
  as `"/" + path` instead of answered as the root.

## Not yet ported

What the suites of step 2a do not reach and the crate therefore does not have yet. Each of these is a line of the oracle,
not a rejection.

- The fields of the original's errors beyond a category and a message (`exceptions.py:22-205`): `details`, `code`, `path`,
  `surface_id` and `function_call_id`, the default code each class carries, `A2uiExpressionError.expression`,
  `RpcErrorCode` and `A2uiRpcError`. They arrive with the message processor and the function calls, steps 2b and 2c.
- The executed-case manifest the harness guide asks for: a record of which cases ran, per run, beside the counts. The
  suites of step 2a run the loader's own count of passed and skipped; the manifest comes when the runner does.

## Conformance cases we do not meet

None recorded yet. Each entry: suite file, case name, reason, who decided.

## Old-against-new review findings

None recorded yet (`docs/porting-guidelines.md` section 6).
