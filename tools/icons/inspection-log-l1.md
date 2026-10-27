# Icon inspection log, lane l1 (section: produce)

Lane l1, branch `icons-l1` (from `item-icons` a586de0), 2026-10-22. 61 produce items, the ones the pilot did not
cover. The pilot's 6 produce icons (apple, banana, avocado, tomato, carrot, broccoli) are unchanged.

Every icon was drawn by hand as original artwork, built with `tools/icons/build.sh` (0 errors, 0 warnings), and
inspected in contact sheets at 2x and 1x (58 px) on the red and teal spec tiles, light and dark page, and on the web
tiles with the drop shadow. `icons contrast` passes for all 61 (gate: weak edge ≤ 25 % on both spec tiles). Lane
means: red 1.5 % / teal 0.4 % weak edge; worst raspberry 17.6 % on red.

Sheets: `~/scratch/icon-full/l1/` on nezopt (`produce-l1-{light,dark}-1x.png`, `produce-l1-light-2x.png`,
`produce-l1-dark-web.png`, `produce-all-light-1x.png` with the pilot icons for comparison, `contrast-l1.txt`).

## Palette notes

No colour was added. The palette has no purple, so the purple produce uses the closest colour, `wine` (`#7a1734`):
grapes, aubergine, red onion, red cabbage, beetroot, sweet-potato skin (with `crimson` as its lighter face), the
artichoke's bract tips. Chive flowers use `pink`/`rose` instead of lavender. Blueberries are `navy` + `blue`.

## Shape language

- Whole + cut piece for citrus and similar (orange + segment slice, lime + green slice, kiwi + slice, coconut + half,
  cucumber + slice) so oranges/limes/lemons/clementines stay apart: the lemon is a pointed yellow fruit with a leaf,
  clementines are two flattened fruits with a pair of leaves.
- Diagonal bottom-left to top-right for long vegetables (leek, cucumber, courgette, sweet potato, parsnip, corn,
  spring onions, chives, green beans), as the pilot carrot.
- Herbs are sprigs; each has its own leaf shape: basil broad glossy leaves, mint serrated lime leaves with veins,
  parsley dark trilobed leaflets, coriander pale round scalloped leaflets, dill needle fronds with a yellow umbel,
  rosemary needle sprigs on a brown stem, chives a tied bundle with a pink flower, fresh herbs a tied mixed bunch.
- Red objects carry the 2 px `crimson` tonal rim (strawberry, cherry, cherry tomato, watermelon flesh, bell pepper,
  chilli, radish); pale-green ones on teal a `green` rim (pear, celery, mint leaves, coriander leaves).

## Items (61)

| Icon | Result | Weak edge red / teal | Notes |
| --- | --- | --- | --- |
| pear | pass | 0 / 0 % | Lime pear with `green` rim, stem, leaf. |
| orange | pass | 0 / 0 % | Whole orange with dimples + segment slice in front. |
| clementine | pass | 0 / 0 % | Two flattened fruits, two leaves on a stem; back fruit separated by a soft cast shadow. |
| lemon | pass | 0 / 0 % | Pointed lemon, diagonal, pores, leaf. |
| lime | pass | 0 / 9 % | Green whole lime + lime slice; differs from lemon by colour and the slice. |
| grapes | pass | 0 / 0 % | Wine bunch 4-3-2-1, one shadow crescent per grape, stem and leaf. |
| strawberry | pass | 11 / 0 % | Red berry with crimson rim, seeds, calyx. |
| blueberry | pass | 0 / 0 % | Three navy/blue berries with dark crown stars, leaf. |
| raspberry | pass (fix) | 18 / 0 % | v1 red drupelets read as a strawberry: `rose` drupelets on a `crimson` body. |
| cherry | pass (fix) | 9 / 0 % | Two cherries on joined stems; v1 shadow crescents were malformed, redrawn as clean arcs. |
| watermelon | pass | 17 / 0 % | Wedge with green/lime rind, crimson rim on the flesh, seeds. |
| melon | pass (fix) | 0 / 0 % | Cantaloupe half: netted tan skin, orange flesh, seed cavity. Removed a second highlight. |
| pineapple | pass (2 fixes) | 0 / 0 % | v1 diamond grid lines extended far outside the box (the validator counts clipped paint): lines trimmed to the body box. v2 body was narrow: wider body, bigger crown. |
| mango | pass | 0 / 0 % | Yellow/orange two-tone kidney shape, stem, leaf. |
| kiwi | pass | 0 / 0 % | Fuzzy brown kiwi + green slice with cream core and seeds. |
| peach | pass | 0 / 0 % | Orange peach with cleft, soft red blush, two leaves. |
| coconut | pass | 0 / 0 % | Brown fibrous coconut with eyes + white half. |
| cherry-tomato | pass | 6 / 0 % | Three small tomatoes on a vine (the pilot tomato is one large fruit). |
| potato | pass (fix) | 0 / 0 % | Two tan potatoes with eyes; added `brown` eye dots (validator: 2 colours). |
| sweet-potato | pass (2 fixes) | 0 / 0 % | v1 `crimson` skin was 29 % weak on red and too thin (read as a feather): plump `wine` tuber with orange cut end. |
| onion | pass (fix) | 0 / 0 % | Gold bulb with meridians, tip, roots. Scaled 0.93 into the padding. |
| red-onion | pass (fix) | 8 / 0 % | Same bulb in `wine` with `rose` lines. Scaled 0.93. |
| spring-onion | pass | 0 / 0 % | Three stalks, white bulbs, roots, lime-to-green leaves. |
| shallot | pass (fix) | 0 / 0 % | Two elongated copper bulbs; scaled 0.93 into the padding. |
| leek | pass (fix) | 0 / 1 % | v1 too small and thin: thicker shaft at full scale. |
| garlic | pass (fix) | 0 / 0 % | White bulb with clove lines and pink streaks; scaled 0.93. |
| parsnip | pass | 0 / 2 % | Cream carrot-shaped root, wider shoulder, fronds. |
| beetroot | pass | 0 / 0 % | Wine bulb, tail, leaves with crimson veins. |
| celeriac | pass | 0 / 0 % | Knobbly tan root with root tangle and stalk stubs. |
| celery | pass (fix) | 0 / 0 % | Three lime stalks with `green` rim and leaf tops; scaled 0.93. |
| fennel | pass | 0 / 0 % | White bulb with layers, lime stalks, fronds. |
| radish | pass (fix) | 12 / 0 % | Two red radishes with crimson rim, white tips, leaves; tails thinned (v1 looked like fangs). |
| cucumber | pass | 0 / 0 % | Bumpy green cucumber + slice with seeds. |
| zucchini | pass | 0 / 1 % | Dark striped courgette, lime stem, yellow flower at the tip (differs from cucumber). |
| eggplant | pass | 0 / 0 % | Wine aubergine with green cap. |
| bell-pepper | pass (fix) | 2 / 0 % | Three-lobed red pepper; at 2 px rim it was 25.1 % weak on red: rim 2.6 px. |
| chili | pass | 3 / 0 % | Red chilli in front of a green one. |
| pumpkin | pass | 0 / 0 % | Ribbed orange pumpkin, brown stem, leaf. |
| lettuce | pass (fix) | 0 / 0 % | Ruffled head, lime heart, veins; added a `forest` fold line (validator: 2 colours). |
| salad-mix | pass (fix) | 0 / 0 % | Crimped bag with a window of mixed leaves (green, lime, wine), blue band; scaled 0.9. |
| rocket | pass | 0 / 3 % | Three lobed leaves. |
| spinach | pass (2 fixes) | 0 / 0 % | Three smooth spade leaves; side leaves left the viewBox: less rotation, scaled 0.82; stem shortened. |
| cabbage | pass | 0 / 0 % | Smooth lime head in outer leaves, veins. |
| red-cabbage | pass (fix) | 8 / 0 % | v1 cut section read as an orb with a white tree: redrawn as the cabbage head in `wine`/`crimson` with pink veins. |
| kale | pass (2 fixes) | 0 / 1 % | Cavolo nero: two long blistered leaves; widened 1.35x, thinner midribs. |
| cauliflower | pass | 0 / 0 % | White florets in green leaves. |
| brussels-sprouts | pass | 0 / 1 % | Three sprouts with leaf layers and cut stems. |
| mushroom | pass | 0 / 0 % | Big and small brown-cap mushrooms. |
| corn-on-the-cob | pass | 0 / 0 % | Kernel cob with husk leaves, diagonal. |
| green-beans | pass (2 fixes) | 0 / 0 % | v1 thin strokes looked like grass: thicker curved pods with tails and lime highlights; scaled 0.92. |
| asparagus | pass | 0 / 3 % | Bundle of spears with scaled tips and a blue band. |
| artichoke | pass | 0 / 1 % | Layered bracts with wine tips, stem. |
| ginger | pass | 0 / 0 % | Branched tan rhizome with rings and a cut end. |
| fresh-herbs | pass | 0 / 0 % | Tied bunch: basil leaf, rosemary sprig, coriander leaflet. |
| basil | pass (2 fixes) | 0 / 0 % | v1 leaves too narrow and a shadow spilled onto the tile: broader leaves, shadow inside the right leaf. |
| parsley | pass (2 fixes) | 0 / 1 % | Lopsided leaflets and stray vein lines outside the leaves: symmetric trilobed leaflets; scaled 0.9. |
| dill | pass (2 fixes) | 0 / 0 % | Side fronds left the viewBox: smaller, less splayed; stray shadow stroke moved onto the stem. |
| chives | pass | 1 / 0 % | Tied bundle of tubes, pink flower. |
| rosemary | pass (fix) | 0 / 0 % | Two needle sprigs; recentred (sat in the lower right). |
| mint | pass (2 fixes) | 0 / 0 % | v1 read as a fir tree: broader serrated leaves in pairs, `forest` veins. |
| coriander | pass (fix) | 0 / 0 % | Pale scalloped leaflets on thin stems; scaled 0.86 into the viewBox. |
