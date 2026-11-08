# ink-render

Page model, text layout, pagination and grayscale rendering for the Lucina Ink reader on the Kindle
Paperwhite 3 panel (1072×1448, 300 dpi, 8-bit gray). `Reader` drives any `ink_kindle_hal::Display`: page turns
send only the changed regions (dirty rectangles) as partial GL16 updates; font changes re-paginate and do a
full GC16 refresh; the reading position is an anchor (chapter + character offset) that survives re-pagination.

## Licences

| Component | Licence | Notes |
|---|---|---|
| this crate | MIT (workspace) | |
| [`fontdue`](https://github.com/mooman219/fontdue) 0.9.5 | MIT OR Apache-2.0 OR Zlib | Pure-Rust TrueType/OpenType parser and rasterizer. Its dependencies (`ttf-parser`, `hashbrown`, `foldhash`, `core_maths`, `libm`, `allocator-api2`) are MIT/Apache-2.0 or Zlib. |
| `fixtures/LiberationSerif-Regular.ttf` | SIL Open Font License 1.1 | Liberation Fonts 2.1.5, an **unmodified** copy (Ubuntu `fonts-liberation`). Full licence and copyright lines in `fixtures/LICENSE-LiberationSerif-OFL.txt`. "Liberation" is a Reserved Font Name, so the file must not be modified (e.g. subset) under that name. Used by the tests only. |

No proprietary fonts are bundled. Fonts are loaded from a path at runtime (`Fonts::load`, plus optional bold and
italic faces with `Fonts::with_face`); missing bold is synthesised by double-striking, missing italic falls back to
the regular face.

## Layout rules

- Greedy line filling with kerning; hyphen-free justification. A line whose word gaps would exceed 3× the
  space width is set ragged instead. Words wider than the line are split between characters.
- First-line indent (1.5 em) except after headings, images, preformatted text and at chapter start; block quotes
  are indented 2 em on both sides; list items hang their marker.
- Headings (h1–h3 scaled 1.6/1.35/1.17×, bold) are kept with the following line. Every chapter starts a page.
- Preformatted text keeps its line breaks but not its leading indentation (known limitation).
- Images are sized and drawn through an `Images` source (see below); without one they are placeholders (a framed
  box with the alt text).

## Pictures

ink-render does no image decoding. ink-epub records each picture's `src` (as written in the book) and `alt`; the app
implements `Images` and passes it to `paginate_with` / `render_page_with` / `Reader::open_at_with`:

- `size` gives the intrinsic pixel size for the layout. Pictures are shown at 2× (book pictures target ~150 dpi
  screens), shrunk to fit the text area; a chapter that is only one picture (a cover) fills the text area. Unknown
  size → placeholder.
- `draw` paints the pixels into the box the layout chose. `Pending` (still decoding) leaves a light frame; the app
  calls `Reader::redraw` when the picture is ready (partial update, page and anchor kept). `Missing` → placeholder.
- `prefetch` is called after every page shown with the pictures of the pages likely next (`page+1`, `page+2`,
  `page-1`), so they can be decoded before a page turn needs them.

A picture covers one character offset, so reading anchors are the same with and without pictures; only page
boundaries move. `NoImages` (used by `paginate`, `render_page`, `Reader::open`) keeps the placeholder behaviour.
