# Icon inspection log, lane l3 (meat-fish, deli, frozen)

2026-10-22. 55 new item icons, drawn by hand in the pilot style (variant B, no outline). After each change, every icon
was checked in a zoomed review sheet (160 px on `#f16a6f` and `#6eb8ae`). The final check used the section contact
sheets at 1x (spec tiles, light and dark), at 2x, and on dark web tiles with the drop shadow (`~/scratch/icon-full/l3/`
on nezopt).

- `bash tools/icons/build.sh`: 0 errors, 0 warnings.
- `icons contrast`: every icon in the three sections is at or below the 25 % gate. The worst is mortadella, 18 % on red.
- No new palette colours were needed.

## Shared shape language

- **Raw meat on a white foam tray** (chicken breast and the three minces): the white tray keeps the red or pink meat
  apart from the red tile. The minces differ by colour: beef is red with a `crimson` rim, pork is `pink`, and the mixed
  mince is half and half.
- **Plates** (white ellipse with a `linen` well) for cooked or plated food: turkey, duck, meatballs, veal, smoked salmon.
- **Wooden board** (`tan` top, `crust` edge) for cuts: pork tenderloin.
- **Red meat on the red tile** gets a `crimson` rim. Where that was still weak, it gets a `wine` rim (stewing beef,
  frankfurters, tuna steak, salami) or a cream fat edge on the silhouette (steak, pork chops, the mortadella casing).
- **Frozen bags** share one silhouette: a crimped top band, puffy sides, a white window and a white snowflake badge
  at the top right. Bag colour and window content tell them apart:

  | Bag | Colour | Window |
  | --- | --- | --- |
  | peas | green | peas |
  | vegetables | white | mixed vegetables on blue |
  | spinach | navy | a leaf and portion blocks |
  | berries | `wine` | blue, red and black berries |
  | fries | blue | fries |

  Loose frozen food (pizza, fish fingers, nuggets, dumplings) carries the same snowflake badge.

## Meat & fish (31)

| Icon | Result | Notes |
| --- | --- | --- |
| chicken-breast | pass | Two raw fillets on a white tray, fibre lines in `rose`. |
| chicken-thighs | pass | Two golden drumsticks with cream bone knobs ("cosce" = legs). |
| chicken-wings | pass (2 fixes) | v1 (stroked segments) read as sticks. v2 (single-path wing) was still blobby. v3: a boomerang-shaped wing with a joint crease, two wings. |
| turkey | pass | A darker (`brown`) roast than the chicken, with paper frills on the legs and lettuce and cranberries on the platter. |
| duck | pass (2 fixes) | v1 (scored breast) read as a pie. v2: the slices read as jars and failed contrast (35 %). v3: scored breast on a plate with an orange slice. Colours cut from 8 to 7 (orange rind in `crust`). |
| minced-beef | pass | Red heap with a `crimson` rim and squiggle texture, on a tray (11 %). |
| minced-pork | pass | Same, `pink` with `rose` squiggles. |
| minced-pork-veal | pass | Two-tone heap (red half, pink half). |
| steak | pass (3 fixes) | v1 (T-bone) read as the letter T and failed (29 %). v2: ribeye with a cream fat cap (14 %). Its fat swirl read as a face at 1x; now one diagonal streak. |
| stewing-beef | pass (2 fixes) | v1 (isometric cubes with white stripes) read as gift boxes (32 %). v2: irregular chunks in a white bowl with a `wine` rim (12 %). |
| burger-patties | pass | Two grilled patties with paper between them. |
| meatballs | pass | Five meatballs on a plate, with parsley. |
| pork-chops | pass (fix) | Had only 2 colours: added `linen` shading on the fat and the bone. |
| pork-tenderloin | pass (fix) | v1 (diagonal pink log) read as a sausage or salami and had 2 colours. Now a tapered loin with a cut end on a board, plus one medallion. |
| pork-roast | pass | Rolled roast joint with white strings and a pink cut face. |
| pork-belly | pass | Slab with a skin top and fat/meat layers. |
| sausages | pass | Three grilled links with grill marks. |
| medister | pass | Raw pink coil (a two-centre spiral) with a wooden skewer: the Danish coil shape. |
| frankfurters | pass (fix) | The `crimson` rim was weak (30 %); a `wine` rim brought it to 2 %. Mustard zigzag. |
| lamb | pass | Rack with frenched bones and a herb crust. |
| veal | pass (2 fixes) | v1 (loose escalopes) read as brains. v2: two thin escalopes on a plate with lemon and sage. Colours cut from 8 to 7 (lemon without `gold`). |
| salmon | pass | Orange fillet with fat lines and grey skin. |
| smoked-salmon | pass | Draped slices on a plate, with dill and a lemon wedge. |
| cod | pass | Speckled `tan` back, cream belly, three dorsal fins and a chin barbel (unlike the silver white-fish). |
| plaice | pass (fix) | Flatfish seen from above with orange spots and both eyes on one side. In v1 the fin rays poked out like sun rays and broke the padding: rays removed, scaled to 0.92. |
| tuna-steak | pass (fix) | v1 read as a red tin (its navy skin band looked like a label). Now an irregular `rose` slab with a dark bloodline, pale muscle arcs and a `wine` rim. |
| fish-cakes | pass | Three golden patties, a lemon wedge and dill. |
| prawns | pass | One curled prawn with segments, a tail fan and antennae. |
| mussels | pass (fix) | The closed mussel was hidden behind the open one; moved them apart. |
| clams | pass (fix) | v1 (cream shells with tan bands) read as meringues. Now `brown` growth lines and a `steel` rim and hinge. |
| squid | pass (fix) | The tentacles clipped at scale 1.15; now at scale 1. |

## Deli (14)

| Icon | Result | Notes |
| --- | --- | --- |
| prosciutto | pass (2 fixes) | v1 (draped slices) read as bacon and failed (26 %); v2 read as a meat loaf. Now a cured leg: dark `brown` rind, a hoof, and a deep-red face with a cream fat ring. Distinct from the pilot ham, which has a pink face and a bone knob. |
| salami | pass | Hanging salami with a string net and a slice (the section icon is a log lying flat). |
| mortadella | pass (fix) | The pink slices failed badly on red (100 % weak); a cream casing ring brought it to 18 %. Fat cubes and pistachio dots. |
| guanciale | pass (fix) | Peppered jowl slab; the tiny bar became three lardons. |
| pancetta | pass | Two rolled (arrotolata) spiral slices. |
| bacon | pass (2 fixes) | v1 was too wavy (read as worms) and clipped. Now two wide rashers with cream, rose and crimson stripes, scaled into the padding. |
| liver-pate | pass | Foil tray (with a `steel` rim for the teal tile) holding baked pâté and a bay leaf. |
| rullepoelse | pass | Rectangular pressed slices with marbling and herb flecks (pancetta has round spirals). |
| roast-beef | pass | Fanned slices (brown edge, pink centre) and the joint. |
| sliced-turkey | pass (2 fixes) | v1's green rim read as an outline. Now a `tan` roasted edge with herb dots on the front slice (size cut from 2.78 KB to under the budget). |
| olives | pass | White bowl with green olives with pimento, dark olives and leaves. |
| hummus | pass (fix) | Had 8 colours: dropped the oil pool. |
| sushi | pass | A maki roll and two salmon nigiri. |
| ready-meal | pass (fix) | Compartment tray with a film sheen. Colours cut from 8 to 7. |

## Frozen (10)

| Icon | Result | Notes |
| --- | --- | --- |
| frozen-peas | pass | Green bag with peas in the window. |
| frozen-vegetables | pass | White bag with carrot, corn, peas and bean on blue. |
| frozen-spinach | pass | Navy bag with a spinach leaf and two portion blocks. |
| frozen-berries | pass | `wine` bag with blueberries, raspberries and blackberries (12 % weak on red, from the wine bag edge). |
| french-fries | pass | Blue bag with fries. |
| frozen-pizza | pass (fix) | Pizza on a blue box with a snowflake. Colours cut from 8 to 7 (no basil). |
| fish-fingers | pass | Four breaded sticks; one is broken and shows the white fish. |
| chicken-nuggets | pass | Four irregular golden nuggets. |
| dumplings | pass (fix) | Three pleated gyoza. Size cut from 2.7 KB to 2.2 KB (dropped the base band). |
| ice-cubes | pass | Three cubes with a `blue` rim and drops (no snowflakes, unlike the frozen section icon). |
