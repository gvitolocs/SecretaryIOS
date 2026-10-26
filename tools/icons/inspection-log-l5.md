# Icon inspection log, lane l5 (drinks, snacks-sweets, other)

Drawn 2026-10-22 on branch `icons-l5` (from `item-icons` a586de0). Style: variant B (no outline), as in
`docs/design/item-icons.md`. Every icon was built with `tools/icons/build.sh` (0 errors, 0 warnings), looked at in
contact sheets at 1x (true 58 px) and 2x on the spec red `#f16a6f` and teal `#6eb8ae` tiles, light and dark page, plus
the web tiles with the drop shadow, and checked with `icons contrast` (gate: weak edge <= 25 %).

Sheets: `~/scratch/icon-full/l5/` on nezopt (`<section>-light-1x.png`, `-dark-1x.png`, `-light-2x.png`,
`-web-dark-1x.png`, `contrast-l5.txt`). Pilot icons already in these sections (water, coffee, red-wine) were not touched.

Shape language: gable cartons (orange/apple juice) reuse the milk carton; wine bottles reuse the red-wine bottle and
glass; sparkling water reuses the water bottle; snack bags/boxes and nut piles share the same highlight bean and
lower-right ink crescent. No palette colour was added; none was needed.

Contrast over the 57 icons of the three sections (spec red / spec teal): mean weak edge 0.6 % / 0.3 %; worst rose-wine
15 % on red, then water and sparkling-water 5 % on teal, dried-fruit 5 % on red. All under the gate.

## Drinks (22 new)

| Icon | Result | Notes |
| --- | --- | --- |
| sparkling-water | pass | Water bottle with green cap, bubble-ring label and bubbles inside; `blue` tonal rim as on water (5 % on teal). |
| orange-juice | pass | Milk carton with orange label and an orange-slice motif. |
| apple-juice | pass | Same carton, yellow label, red apple motif. Distinct from orange juice by label colour and motif. |
| juice | pass (fix) | Flat-top juice box with straw, lime label, orange + cherries. v1 had 11 colours: straw red, stems green, hole linen (7). |
| cordial | pass | Crimson syrup bottle (white screw cap, berry label) + tumbler of diluted pink drink with ice. |
| cola | pass | Tumbler of dark cola, ice cubes, bubbles, lemon slice on the rim. |
| soft-drinks | pass | Yellow can with orange wave, citrus slice and bubbles. |
| coffee-beans | pass (fix) | Five beans with S crease. v1 had 2 colours: back beans darker (`cocoa`/`ink` crease) for depth. |
| coffee-capsules | pass (2 fixes) | v1 lid-up cups in a fan read as cones/flowers: redrawn dome-up with flange (the capsule silhouette). v2 had 9 colours: top highlight is white with opacity (6). |
| instant-coffee | pass (fix) | Jar with gold lid, granules, label with a cup. v1 had a translucent glass shoulder that showed tile colour (pink/teal band): jar filled with granules to the shoulder. |
| tea | pass | Brown-betty teapot (crust) with cream band and a tea-bag tag. |
| green-tea | pass | Green handleless cup with lime tea and two tea leaves. |
| herbal-tea | pass | White cup with golden infusion, big chamomile flower and a mint leaf. |
| hot-chocolate | pass | Blue mug, whipped cream with chocolate drizzle, cocoa dust, pink marshmallows. |
| beer | pass | Glass mug of beer with overflowing foam and glass handle. |
| alcohol-free-beer | pass (fix) | Brown bottle, label with wheat ear crossed by a red "no" sign. v1 bottle was too narrow and the sign too small: widened. |
| white-wine | pass | Red-wine bottle/glass shape: green bottle, gold capsule, pale wine. |
| rose-wine | pass (fix) | v1 pink bottle on red 14 %: clear glass upper bottle with pink wine below; still the worst of the lane (15 %), visibly separated at 1x. |
| sparkling-wine | pass | Dark green bottle with gold foil + flute with bubbles. |
| aperitif | pass | Balloon glass of orange spritz, ice, orange slice, straw. |
| spirits | pass | Amber decanter-style bottle + rocks glass with ice. |
| aquavit | pass | Slim clear bottle (red cap, dill label) with golden aquavit + small stemmed snaps glass. |

## Snacks & sweets (25 new)

| Icon | Result | Notes |
| --- | --- | --- |
| crisps | pass | Yellow crimped bag with blue oval label and crisps spilling out. |
| tortilla-chips | pass (2 fixes) | v1 broke the 2 px padding and had 8 colours; v2 chips side by side read as a crown: fanned at different angles with a gold rim, salsa bowl. |
| popcorn | pass | Red/white striped box (white outer stripes), cream popcorn heap. |
| pretzels | pass | Single knotted rope with brown edge, over-strand redrawn at the crossing, salt grains. |
| crackers | pass | Two square crackers with docking holes and salt. |
| nuts | pass | Wooden bowl of mixed nuts (almonds, hazelnut, cashews, pistachio, walnut). |
| peanuts | pass | Two netted shells + two kernels. |
| almonds | pass (fix) | v1 spanned 41 px and had 2 colours: scaled 1.17, grooves opaque `brown`, front almond blanched (`tan`). |
| walnuts | pass | Whole shell with seam + half shell with kernel. |
| hazelnuts | pass | Three hazelnuts with pale base, two husk leaves. |
| cashews | pass | Three C-shaped kernels, one roasted. |
| pine-nuts | pass | Pine cone + six cream kernels. |
| seeds | pass (fix) | Sunflower head, striped sunflower seeds, green pumpkin seeds. v1 3.2 KB: fewer shapes (2.2 KB). |
| raisins | pass | Heap of dark and golden raisins in a white bowl. |
| dried-fruit | pass | Dried apricots, prune, date, cranberries. |
| chocolate | pass | Milk chocolate (lighter segments) in a wine wrapper with a milk drop; tilted the other way from the section icon. |
| dark-chocolate | pass | Near-black bar, navy wrapper with gold band and cocoa bean, chocolate chips. |
| sweets | pass | Three twist-wrapped candies (yellow, blue, green). |
| liquorice | pass (fix) | v1 (filled disc with spiral grooves and pink centre) read as a vinyl record: now an open coiled rope with gaps, plus two salty pieces. |
| marzipan | pass | Marzipan log with red paper band, cut slice, two almonds. |
| biscuits | pass | Chocolate-chip cookie in front of a digestive. |
| kammerjunkere | pass | White bowl of koldskål with kammerjunkere floating, more beside. |
| pebernoedder | pass (fix) | v1 a regular pyramid of balls: loose mound in a blue bowl with two spilled. |
| floedeboller | pass | Glossy chocolate dome on wafer + a bitten one showing the white filling. |
| cereal-bars | pass | Oat bar with nuts and raisins + one in a green crimped wrapper. |

## Other (7 new)

| Icon | Result | Notes |
| --- | --- | --- |
| flowers | pass (fix) | Bouquet in a paper cone: daisy, red tulip (`crimson` rim), blue flower. v1 heads too small and 9 colours: bigger heads, 7 colours. |
| plants | pass | Terracotta pot with broad leaves. |
| potting-soil | pass | Open green sack with soil heap and a sprout, label with a seedling. |
| gift | pass | Blue box with yellow ribbon and bow. |
| wrapping-paper | pass | Yellow dotted roll with unrolled sheet and blue ribbon curl. |
| greeting-card | pass | Yellow card with a red heart in front of a white envelope. |
| shopping-bags | pass | Cream canvas tote with leaf print + blue bag behind (the section icon is a paper bag). |
