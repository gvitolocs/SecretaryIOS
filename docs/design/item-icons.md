# Shopping item icons: style guide

Status: **pilot** (30 items and 17 sections, branch `item-icons`), waiting for the owner's approval before the full
catalog (395 items) is drawn.

The shopping tiles show an illustration per catalog item and section. All of them are **original drawings** made for
Secretary (see `assets/icons/CREDITS.md`). Bring!'s artwork is proprietary; nothing here copies, traces or matches it.
The goal is the same *quality*: friendly, detailed flat illustrations with a soft highlight, not the same drawings.

- Sources (hand-written, readable): `assets/icons/src/{items,sections}/<id>.svg`
- Shipped files (svgo output): `assets/icons/items/<catalogId>.svg`, `assets/icons/sections/<sectionId>.svg`
- Tooling: `tools/icons/` (Rust crate `icon-tools`, binary `icons`; see [Tooling](#tooling))

## Where they appear

Bring!-style tiles: 108 px tall, a **58 px icon area**, white bold label, on a **red** to-buy tile or a **teal**
recent/catalog tile. Two tile palettes exist and the icons must work on both, in light and dark mode:

| Tile | Spec (iOS, Bring-like) | Web today |
| --- | --- | --- |
| To buy | `#f16a6f` | `#ff3b6b` gradient |
| Recent / catalog | `#6eb8ae` | `#14cfc1` gradient |

The tile colour does not change between light and dark mode; only the page around it does. The web adds
`drop-shadow(0 3px 6px rgb(0 0 0 / .25))` under the icon.

## The rules

### Canvas

- `viewBox="0 0 64 64"`, no `width`/`height` (the client sizes it).
- **Safe padding 2 px**: every painted pixel, strokes included, stays inside `[2, 62]`. Aim for 3-4 px; a long object
  (baguette, bottle) may touch 2 px on its long axis.
- The object fills the box: its longer side spans **≥ 44 px** (the validator warns below that). Compact objects are
  centred; elongated ones go diagonal (bottom-left to top-right) to use the square.
- **≤ 2.5 KB** (2560 B) per shipped file after svgo. The pilot ranges from 0.47 KB to 1.85 KB.

### Shapes and light

- Flat fills, rounded shapes (round caps and joins, curved corners). No hairlines under 1.2 px.
- **3-7 flat palette colours per icon**, most 4-6. A simple object (a loaf, a tomato) reads best as three tones of one
  hue plus the highlight and shadow; forcing a fourth colour only adds noise. (The brief asked for 4-7; the pilot showed
  three is right for single-hue objects, so the floor is 3.)
- **Light comes from the top left.** Per object:
  - one **highlight**: white, `opacity` .4-.7, a soft bean or stroke on the upper-left of the object;
  - one **shadow shape**: ink `#3d2b2b`, `opacity` .12-.2, a crescent or band on the lower-right side.
  A repeated object (three penne, two bananas) may carry one highlight/shadow each. Several sub-shapes of one shadow go
  in one `<path>`.
- Depth comes from the shadow shape and from darker/lighter shades of the same hue (top face lighter, side face
  darker), never from gradients.

### Outline: none (variant B), with a tonal rim where needed

There is **no dark outline**. An object whose main colour sits too close to a tile colour gets a **tonal rim**: a 2 px
stroke on its silhouette shape in its own darker palette shade (red fruit: `crimson`; sky-blue plastic: `blue`).
`icons contrast` decides when that is needed (see [Contrast gate](#contrast-gate)). In the pilot: apple, tomato,
water, frozen.

Why, with evidence, in [Decision: outline or not](#decision-outline-or-not).

### Forbidden

No `<text>`, `<image>`, `<script>`, `<style>`, `<foreignObject>`, `<a>`, `<use>`, filters, animation, event handlers,
`style` attributes, external references or `url()` other than `url(#…)`. At most **one** tiny gradient (warned;
none in the pilot). No faces or characters on objects. No brand names, logos or real packaging.

### Palette

26 named colours (`tools/icons/palette.json`, enforced by the validator). Highlights are `white` with opacity, shadows
`ink` with opacity.

| Name | Hex | Typical use |
| --- | --- | --- |
| ink | `#3d2b2b` | shadow shapes (with opacity), pupils, darkest details |
| white | `#ffffff` | highlights (with opacity), paper, porcelain, milk |
| cream | `#fff6e0` | eggs, bones, labels, flour bag |
| linen | `#e6dccb` | shaded white (carton side, saucer, roll top) |
| silver | `#c3ccd6` | foil, tins, metal, fish |
| steel | `#7d8b99` | dark metal, fins, foil curl |
| red | `#e0352b` | tomato, apple, cherry, steak |
| crimson | `#a91e2c` | shade/rim for red, salami, jam |
| pink | `#f9a8b3` | ham face, yogurt band, strawberry ice cream, soap |
| rose | `#e2727f` | ham marbling, salami face |
| orange | `#ff8f1f` | carrot, juice, tin label |
| amber | `#d9661a` | carrot ridges, shade for orange |
| yellow | `#ffd43b` | banana, cheese face, sponge, butter side |
| gold | `#e9a91c` | banana shade, cheese holes, wheat, pasta ridges |
| butter | `#ffe896` | butter, cheese top, pasta, avocado heart |
| tan | `#f0c27b` | croissant, dough, brown eggs, cone, paper bag |
| crust | `#c97d36` | bread crust, roast chicken, ham rind |
| brown | `#8b5530` | stems, rye crumb, chocolate, pit |
| cocoa | `#5a3520` | coffee, rye crust, banana tips |
| lime | `#a8d84e` | stalks, oil, dish soap, avocado flesh |
| green | `#3e9b3e` | leaves, broccoli |
| forest | `#256b2f` | leaf shade, avocado skin, caps |
| sky | `#9fd6f7` | water, ice, bubbles' shine, glass |
| blue | `#3a86d6` | labels, caps, bowls; rim for sky |
| navy | `#24477e` | dark caps, shade for blue |
| wine | `#7a1734` | red wine, bottle glass |

Colour roles that matter on the tiles:

- **On red tiles** the weak colours are `red`, `rose`, `pink`. Red objects get the `crimson` rim; pink objects need
  a light enough fill (pink is fine) or a white/cream part on the silhouette.
- **On teal tiles** the weak colours are `sky`, `silver`, `linen`, `lime`. Sky objects get the `blue` rim; prefer
  white for plastic bottles and packages.
- **White and cream objects are welcome**: they read best on both tiles.

## Decision: outline or not

Three variants were rendered from the same 47 drawings on all four tile colours (sheets in
`~/scratch/icon-pilot/sheets/` on nezopt and `~/Desktop/Secretary-icons-pilot/sheets/` on the Mac):

- **A**: 1.5 px ink outline on every shape (`icons … --outline`), the OpenMoji-like look;
- **A'**: 1.5 px ink outline around the silhouette only (`--silhouette`, evaluation only: it uses a filter);
- **B**: no outline.

`icons contrast` measures each silhouette's outer opaque ring against the tile at the true 58 px size: the share of
ring pixels closer than ΔE76 25 to the tile ("weak edge"), the median ΔE, and the median luminance contrast ratio (CR).
Means over the 47 icons, after the B fixes below:

| Variant | red `#f16a6f` weak / ΔE / CR | teal `#6eb8ae` weak / ΔE / CR | web red | web teal |
| --- | --- | --- | --- | --- |
| A (every shape) | 1.3 % / 60 / 3.20 | 0.2 % / 58 / 4.08 | 0 % / 75 / 2.89 | 0 % / 74 / 4.79 |
| A' (silhouette) | 0.1 % / 64 / 4.36 | 0.2 % / 60 / 5.61 | 0 % / 77 / 3.83 | 0 % / 76 / 6.56 |
| **B (chosen)** | 1.3 % / 61 / 2.06 | 0.5 % / 54 / 1.93 | 0 % / 79 / 2.17 | 0 % / 67 / 1.97 |

Before the fixes, B's worst cases were tomato and apple on red (41 % and 28 % weak), the soap bar and water bottle on
teal (25 %, 15 %), the pet bowl on red (19 %) and the ice cube and spray bottle on teal (15 %, 13 %). Recolouring three
section icons and the tonal rim on four objects brought the worst to apple 22 % (its leaf and stem side), with every
icon visibly separated at 1x.

What the sheets show at the real 58 px:

- **A** separates everything, but every inner shape gets a line too: broccoli turns into a pile of outlined circles,
  rice grains and the egg carton become busy, small details (seeds, pores) clump. It looks like a sticker set, not the
  soft illustrations asked for.
- **A'** is clean and has the strongest edges. Its cost: a real vector silhouette outline means duplicating the
  silhouette shapes under the art, which roughly doubles file size (the larger pilot icons would pass 2.5 KB), and it
  makes the set heavier and darker on the bright tiles.
- **B** is the friendliest and lightest look, and with the tonal-rim rule its weak spots are gone. The web's drop
  shadow adds separation on top.

**Choice: B.** If the owner prefers the sticker look, A' can be added later in the clients (one drop-shadow stack in
CSS, a shadow modifier in SwiftUI) without redrawing anything.

## Contrast gate

`icons contrast` must show, for every icon on both spec tiles, **weak edge ≤ 25 %** and the silhouette must be
clearly visible in the 1x sheet. Above that, fix the drawing: lighten or darken the fill, give the object a white or
cream part on its outline, or add the tonal rim. The 25 % ceiling allows a leaf or stem in a near-tile colour.

## Tooling

All in `tools/icons/` (standalone Rust crate `icon-tools`, not a workspace member):

```sh
bash tools/icons/build.sh     # svgo (npx svgo@4.0.0, tools/icons/svgo.config.mjs) src -> assets/icons, then validate
bash tools/icons/sheets.sh    # contact sheets + contrast reports -> ~/scratch/icon-pilot/sheets
tools/icons/target/release/icons validate assets/icons/items        # one directory or files
tools/icons/target/release/icons render assets/icons/items/banana.svg /tmp/b.png --size 256 --bg '#f16a6f'
tools/icons/target/release/icons sheet --catalog assets/catalog/catalog.v1.json --tiles all --scale 2 \
    --out /tmp/s.png assets/icons/items/{apple,tomato}.svg
tools/icons/target/release/icons contrast assets/icons/items assets/icons/sections
```

- **validate**: well-formed XML, `<svg>` root in the SVG namespace, `viewBox 0 0 64 64`, size ≤ 2560 B, element
  allow-list, no event handlers / `style` / external references, every colour in the palette, at most one gradient,
  painted box inside the 2 px padding (computed with resvg's `usvg`, strokes included), warnings for 3-7 colours and for
  objects spanning less than 44 px.
- **render**: one icon to PNG with resvg (`--outline`/`--silhouette` for the rejected variants).
- **sheet**: contact sheet. Each icon on spec tiles (`--tiles spec`), web tiles (`web`) or both (`all`), with the real
  tile label; `--scale 2` for the 2x zoom, `--theme dark`, `--shadow` for the web drop shadow.
- **contrast**: the edge metric above.
- **outline**: writes the variant-A SVG of one icon.
- `ui-preview.mjs`: screenshots of the web Shopping view (Playwright; needs the mock API and `vite preview`, see its
  header).

## Workflow for a new icon

1. Draw `assets/icons/src/items/<catalogId>.svg` by hand (or from a sibling: recolour the milk carton for oat milk).
2. `bash tools/icons/build.sh` until it reports no errors.
3. Render a sheet with the icon and its section neighbours at 1x and 2x; **look at it** on both tiles. Check: what is
   it at 58 px without the label, does it match the others in size, weight, light and palette, no clipping, no stray
   shapes.
4. `icons contrast` within the gate.
5. Note the result in `tools/icons/inspection-log.md`.

## Clients

- **Web**: `web/src/lib/itemArt.ts` maps catalog ids (and `svg:<key>` icons) to the bundled files through
  `import.meta.glob(…, { query: "?no-inline" })`: each SVG becomes its own hashed file under `/assets/`, shown with a
  same-origin `<img>` (CSP `img-src 'self'`; an SVG in `<img>` cannot run script), cached cache-first by the service
  worker. `itemVisual()` picks what a tile shows, most specific first: an `svg:<key>` icon; the art of the row's
  catalog item (a row whose emoji differs from its catalog entry's, picked by the user, keeps the emoji); for rows with
  no catalog id (free text, Lucina's custom objects) the art of the catalog item the name names (`guessByName`: exact
  name or alias, else the longest leading words, "Pane di segale danese" -> rye bread, else trailing English/Danish
  words, "organic milk"), only when that item is in the row's section; a specific emoji of the row (not the cart, not
  the section's own emoji); the section's art; else the first letter. `ItemIcon` / `SectionIcon`
  (`web/src/views/shopping/ItemIcon.tsx`) render it.
- **iOS** (branch `ios-shopping-icons`): `ios/scripts/sync-item-icons.sh` copies `assets/icons/{items,sections}` into
  `ios/Secretary/Resources/ItemIcons.xcassets` (vector, `item-<id>` / `section-<id>`); `ItemVisual` mirrors the web
  order.
- The catalog's `icon` field stays an emoji, so clients without the set are unchanged.
