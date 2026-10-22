# Icon inspection log, lane l2 (dairy-eggs, bakery, breakfast-spreads)

2026-10-22, branch `icons-l2` (from `item-icons` a586de0). 60 new items, variant B (no outline), drawn by hand in
`assets/icons/src/items/`, optimised with `tools/icons/build.sh`. Every icon was looked at in contact sheets at 2x
while drawing and at 1x (real 58 px) on the spec tiles, light and dark page, plus the dark web sheet. Sheets and
contrast reports: `~/scratch/icon-full/l2/` on nezopt (`<section>-{light-1x,dark-1x,light-2x,dark-web}.png`,
`contrast-<section>.txt`).

Result: `icons validate` 0 errors, 0 warnings (3-7 colours each); `icons contrast` gate passed for all 60 on all four
tiles. Worst weak edge: cottage-cheese 8 % (teal), cake 8 % (red). No new palette colours were needed.

Shape language shared across the lane:

- **Gable carton** (the pilot milk carton, same geometry): label colour + a motif tell the drinks apart.
  Squat carton for cream.
- **Pot** (pilot yogurt pot): skyr, crème fraîche. **Wide tub** (open, rim + surface): Greek yogurt, cottage
  cheese, mascarpone.
- **Wedge** (pilot cheese wedge geometry): parmesan, blue cheese, cake.
- **Jar** (one template: lid, cream label, motif): jam, honey, hazelnut spread, peanut butter.
- **Box** (front + side + top, two white text bars, never text): breakfast cereal, pancake mix.

## dairy-eggs (28)

| Icon | Result | Notes |
| --- | --- | --- |
| lactose-free-milk | pass | Carton, lime label, white drop crossed by a forest slash ("free from"). |
| buttermilk | pass | Carton, sky label, drop + small butter pat (churning by-product). |
| oat-milk | pass | Carton, butter label, crust oat sprig. |
| soy-milk | pass | Carton, green label, lime pod with three beans. |
| almond-milk | pass | Carton, tan label, two brown almonds. |
| chocolate-milk | pass | Carton, brown label, cream drop. Told apart by colour only. |
| ymer | pass | Carton, orange label, white spoon. |
| koldskaal | pass | Carton, yellow label, lemon slice + two kammerjunker biscuits. |
| cream | pass (fix) | Squat carton, whipped-cream swirl on a blue label. v1 darkened the side with a second ink layer: replaced by the navy side shade. |
| margarine | pass | Low tub, yellow lid, green leaf on the side. Clearly not the butter block. |
| creme-fraiche | pass | Open pot, green band, white dollop above the rim. |
| greek-yogurt | pass | Wide open tub, blue band with a white meander, thick dollop. |
| skyr | pass | Pilot pot with a navy band and white mountain peaks. |
| pudding | pass | Flan with caramel top and drips on a plate. |
| mozzarella | pass | Two white balls in a blue bowl of brine, basil leaf. |
| burrata | pass | White pouch with a knotted top, open front showing cream, on a green leaf. |
| parmigiano | pass (fix) | v1 had a yellow band through the middle and read as a layer cake: redrawn as a craggy wedge with a crust rind and grain dots. |
| pecorino | pass | Dark-rind wheel with a wedge cut, cream paste. |
| ricotta | pass | White basket-ridged dome on a blue plate. |
| blue-cheese | pass | White/cream wedge with blue veins and a gold rind end. |
| feta | pass | White crumbly block on a blue dish with a wine-coloured olive and crumbs. |
| cream-cheese | pass (2 fixes) | v1 silver foil tray: 27 % weak edge on teal and read as a tray. v2: block in blue/navy foil + knife (2.7 %). Knife handle navy to stay at 7 colours. |
| cottage-cheese | pass | Wide tub heaped with white curds (sky band). 8 % weak on teal from the white curds, visibly fine. |
| halloumi | pass | Two slices with brown grill bars. |
| brie | pass | White-rind wheel + wedge with a butter paste. |
| grated-cheese | pass (fix) | Box grater + heap of yellow shreds. v1 grater too thin: widened. |
| tofu | pass (2 fixes) | Smooth block + small cube + chopsticks. Chopsticks clipped the padding twice: shortened. |
| mascarpone | pass | Wide tub, gold band, cream surface with butter swirl, spoon. |

## bakery (19)

| Icon | Result | Notes |
| --- | --- | --- |
| wholegrain-bread | pass | Seeded crust loaf + slice with dark flecks and grains; lighter than rye. |
| sliced-bread | pass | Two soft white slices (tan crust, cream crumb). |
| baguette | pass (fix) | Baguette in a paper sleeve with a blue stripe; v1 sleeve covered half the loaf: made smaller. |
| bread-rolls | pass (fix) | Three scored rolls; added a brown base shade (validator: 2 colours). |
| burger-buns | pass (fix) | v1 split bun with a cream gap read as a burger: now a closed sesame bun with a thin seam. |
| hot-dog-buns | pass | Long bun, diagonal, open slit. |
| danish-pastry | pass | Diamond pastry, folded corners, custard centre, icing. |
| cinnamon-roll | pass | Spiral roll with icing drizzle. |
| doughnuts | pass | Pink icing ring with sprinkles; tan dough keeps the edge off the red tile; the hole shows the tile. |
| cake | pass (fix) | Layer-cake slice (pilot wedge geometry), strawberry on top. Was 8 colours: dropped linen. 8 % weak on red (strawberry). |
| panettone | pass (fix) | v1 pleated gold paper read as a muffin: now a taller dome in a navy decorated paper band. |
| pita | pass (3 fixes) | v1 half-pita read as a bowl, v2 two thick ovals read as potatoes. v3: flat oval behind + tilted half with a narrow pocket slit. |
| piadina | pass | Folded half-moon with rocket and ham showing. |
| tortilla-wraps | pass | Stack of thin round tortillas with char spots. |
| focaccia | pass | Golden slab with dimples, rosemary and olives. |
| crispbread | pass | Two rectangular crackers with dot holes. |
| breadsticks | pass (fix) | Fan of sticks in a paper band; scaled 0.94 into the padding. |
| pizza-dough | pass | Dough ball on a floured wooden board. |
| puff-pastry | pass | Rolled sheet on baking paper with the spiral end showing. |

## breakfast-spreads (13)

| Icon | Result | Notes |
| --- | --- | --- |
| muesli | pass (fix) | Blue bowl heaped with flakes, raisins, berries. Dropped a sky rim line (8 colours). |
| granola | pass (fix) | Kraft paper bag with a window of clusters; removed loose clusters beside the bag (stray shapes). |
| breakfast-cereal | pass (fix) | Orange box with a bowl of rings; v1 rings were thin gold outlines, invisible at 1x: now filled loops. |
| rolled-oats | pass (fix) | Wooden scoop of oat flakes; handle and a spilled flake clipped the padding: moved in. |
| ymerdrys | pass (fix) | White bowl of ymer topped with dark rye crumbs, spoon. Spoon simplified to one colour (8 colours). |
| rusks | pass (fix) | v1 stacked slabs read as a layer cake: now three golden toasted slices fanned. |
| jam | pass | Crimson jar, gold lid, strawberry label. |
| honey | pass | Gold jar, honey drips from the lid, honeycomb cells on the label. |
| hazelnut-spread | pass | Cocoa jar, navy lid, hazelnut label. |
| peanut-butter | pass | Tan jar, green lid, peanut-shell label. |
| chocolate-slices | pass | Bread slice topped with thin chocolate slices (Danish pålægschokolade). |
| maple-syrup | pass | Amber jug with handle, maple-leaf label. |
| pancake-mix | pass (fix) | Blue box with a pancake stack, butter and syrup; top face white (8 colours). |

## Least confident

- **ymer, koldskaal, buttermilk, chocolate-milk**: cartons told apart only by label colour and a small motif; the
  names are local products with no universal symbol.
- **creme-fraiche vs greek-yogurt vs mascarpone vs cottage-cheese**: open pots/tubs; the bands and toppings differ,
  but at 58 px the label carries much of the meaning.
- **pita**: third version; reads as flatbread, the pocket is subtle.
- **rusks vs crispbread**: both golden rectangles (fan of toasted slices vs crackers with holes).
- **pizza-dough**: a dough ball on a board can read as mozzarella or bread dough in general.
- **cream-cheese**: foil-wrapped block with a knife; less obvious than a tub.
