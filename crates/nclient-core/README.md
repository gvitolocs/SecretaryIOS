# nclientv3-rust — Rust port of the NClientV3 portable core

Port of the portable (non-Android) core of [NClientV3](https://github.com/maxwai/NClientV3)
(`app/src/main/java/com/maxwai/nclientv3/api/**` + `utility/Utility.java` pure logic)
to Rust, with **JVM-enforced parity** and **Verus verification**.

```
crates/nclient-core/
├── src/                  the port (pure, no I/O, no Android)
│   └── tests/parity.rs   golden parity tests vs JVM outputs
├── java-goldens/
│   ├── JavaGolden.java   harness: original Java logic, method bodies copied
│   │                     verbatim (Android plumbing stripped), run on a JVM
│   └── goldens.json      generated ground truth
├── verus/verify_core.rs  Verus proofs of core invariants
└── run_all.sh            full pipeline
```

## What is ported

| Java source | Rust module | Notes |
|---|---|---|
| `api/enums/*` | `enums.rs` | TagType/TagStatus/TitleType/Language/SortType/ApiRequestType/ImageType/SpecialTagIds |
| `api/components/Tag.java`, `TagList.java` | `tag.rs` | equality by id only (Java quirk), CSV scraped format, query-tag form |
| `api/components/Ranges.java` | `ranges.rs` | date+unit modeled as `Option<(i32, TimeUnit)>` so Java's null-unit NPE state is unrepresentable |
| `api/components/Page.java`, `Size` | `page.rs` | i1/t1 host URL building |
| `api/components/GalleryData.java` | `gallery_data.rs` | v2 JSON parse (media_id string-or-int, title null→"" + unescape, error→invalid), page-path serialization, favorites refresh state as a value instead of a thread |
| `api/SimpleGallery.java` | `simple_gallery.rs` | `from_v2_list_item` incl. title precedence + tag-id/tag-array paths; DB access replaced by `TagProvider` trait |
| `api/components/Gallery.java` | `gallery.rs` | language from special tag ids, min/max sizes, title fallback chain (`length > 2`, "Unnamed"), path-title sanitization |
| `api/InspectorV3.java` (pure parts) | `inspector.rs` | request-type decision, URL construction, search/single response parsing, bookmark/search-title helpers; RANDOM short-form → `NeedsDetailFetch` instead of inline fetch |
| `api/local/LocalSortType.java` | `local_sort.rs` | 0x80 sign-extension byte semantics, modulo decode |
| `api/comments/*` | `comments.rs` | preserves the upstream bug: username parsed from the `"post_date"` key |
| `utility/Utility.java` (pure) | `util.rs` | `unescape_unicode_string` (incl. EOF→U+FFFF and invalid-hex quirks), `java_url_encode` (URLEncoder parity) |

**Not ported** (Android-only, no Rust analogue): Activities/adapters/views/widgets,
Parcelable, OkHttp client (caller supplies HTTP), SQLite `Queries.*` (replaced by
`TagProvider`), download/database services, WebView-based flows, mirror settings
(host is a parameter, default `nhentai.net`).

## Parity methodology

1. `java-goldens/JavaGolden.java` contains the **original Java method bodies copied
   verbatim** (only Android-only plumbing removed) and executes them on a JVM to
   produce `goldens.json` — the ground truth is the original code, not a
   re-transcription.
2. `core/tests/parity.rs` asserts the Rust port reproduces every golden exactly.
   Covers: unescape (13 inputs incl. malformed escapes), query tags, scraped CSV
   roundtrip, ranges, sort discovery, LocalSortType roundtrip, path titles,
   language detection, a 13-case URL-construction matrix, bookmark/search-title,
   URLEncoder vectors, and the request-type decision matrix.
3. Behavioral tests for the JSON parsers encode the Java `JsonReader` switch
   semantics (key-by-key) with end-to-end vectors.

Quirks preserved on purpose (documented at each site): trailing-backslash escape
writes U+FFFF, `\uZZZZ` garbage code points, `User` username-from-`post_date`
bug, dup-tag URL `contains` skip, whole-URL space→`+` replacement.

## Verification (Verus)

`verus/verify_core.rs` (run with verus ≥ 0.2026.10) proves:
- `hex_digit_value` total correctness + the `\uXXXX` accumulator stays in
  `[0, 0xFFFF]` (no i32 overflow for well-formed escapes),
- `LocalSortType` encode/decode **roundtrip theorem** and decode totality
  (modulo can never index out of bounds) — bitwise u32 formulation matching
  Java's byte 0x80 sign-extension,
- `build_query_tag` structural postconditions (`-` prefix iff avoided,
  `<type>:"<name>"` shape),
- `TagType` id roundtrip.

Status: **14 verified, 0 errors.**

## Run

```bash
./run_all.sh          # goldens → cargo test → verus
```

Requires: JDK 11+, cargo, Verus (set `VERUS_BIN` if not in the default locations).
