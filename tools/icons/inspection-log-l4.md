# Icon inspection log, lane l4 (2026-10-22)

Sections: `pasta-rice-grains`, `canned-jars`, `condiments-spices`. 75 new items (pasta, rice, flour and olive-oil were
already in the pilot). Style: variant B (no outline), palette from `tools/icons/palette.json` only, no new colours.

Checks per icon: `tools/icons/build.sh` (svgo + validate, 0 errors / 0 warnings for all 75), zoomed renders (150 px on
`#f16a6f` and `#6eb8ae`), section contact sheets at 1x (light and dark), 2x, dark web tiles with drop shadow, and
`icons contrast` (gate <= 25 % weak edge on every tile: all pass).

Sheets and the contrast report: `~/scratch/icon-full/l4/` on nezopt.

## Shared shape language

| Family | Used by | Distinguished by |
| --- | --- | --- |
| Paper sack (pilot `flour`) | wholemeal-flour, rye-flour, semolina, polenta | sack colour (tan / crust / butter / yellow), band colour, ear (brown; dark rye with awns; gold durum with awns), corn cob label |
| Carton box (milk-carton perspective) | risotto-rice, cornflour, potato-starch, baking-soda | band colour + motif (grain window, corn cob, potato, powder disc) |
| Glass jar (tall, screw lid) | pasta-sauce, pesto, gherkins, pickled-beetroot, pickled-red-cabbage, capers (86 %), sun-dried-tomatoes, artichoke-hearts, pickled-herring, anchovies, lentils, mayonnaise | contents colour + shape, lid colour, label motif |
| Open tin with peeled lid | canned-tomatoes, chickpeas, beans, baked-beans, sweetcorn, canned-fruit, cocoa-powder | heaped contents, label colour, small label motif |
| Oblong open tin with rolled key lid | sardines, canned-mackerel | whole silver fish in oil vs striped fillets in tomato sauce |
| Spice jar (ridged cap) | chili-flakes, paprika, oregano, cumin; three mini jars in `spices` | contents colour, cap, label motif (chilli, pepper, sprig, seeds) |
| Inverted squeeze bottle | ketchup, remoulade | red + crimson rim + tomato vs butter with relish flecks + gherkin |
| Isometric cubes | sugar, brown-sugar, stock-cubes, yeast | white / brown / gold foil with herb / single block on an opened wrapper |
| Bowl (pilot `rice`) | noodles, pudding-rice, soup, couscous | white + red band + chopsticks; cherry sauce; handles + steam; terracotta |

## Items

| Icon | Result | Notes |
| --- | --- | --- |
| egg-pasta | pass | Tagliatelle nest + egg behind it (the egg is the "egg pasta" motif). |
| filled-pasta | pass | Two ravioli: crimped edge (dash-dot stroke), fork line, filling pillow. |
| lasagne-sheets | pass (2 fixes) | Three ruffled sheets. v2 scaled 1.12 and clipped right (65.6 px); v3 recentred, scale 1.08. |
| gnocchi | pass (2 fixes) | v1 used zsh-unsplit transforms (all at origin, 3.3 KB). v2: six ridged gnocchi in grouped absolute shapes, 1.4 KB. |
| noodles | pass | White bowl with red band, noodle mound, chopsticks lifting strands. |
| instant-noodles | pass | Cream cup, orange band with noodle wave, peeled lid. |
| risotto-rice | pass | Carton with green band and a window of long grains (rice itself is the pilot bowl). |
| pudding-rice | pass | White bowl of creamy rice with cherry sauce and a spoon (risalamande / riso tondo). |
| couscous | pass | Terracotta bowl with zigzag, heaped dotted couscous, mint leaves. |
| quinoa | pass | Wooden scoop of three-colour grains, a few spilled. |
| spelt-grains | pass | Open burlap sack of grains with an ear. |
| polenta | pass | Yellow sack, corn cob label. |
| lentils | pass | Jar of brown/tan lentils, wooden lid, lentil label. Grain rows a bit regular. |
| wholemeal-flour | pass | Brown-paper sack, brown ear, bran flecks, green band. |
| rye-flour | pass | Dark sack, cream label, dark rye ear with awns. |
| semolina | pass | Butter sack, durum ear with awns, orange band. |
| cornflour | pass | White carton, yellow band, corn cob. |
| potato-starch | pass | White carton, brown band, potato. |
| breadcrumbs | pass | Bread slice and a heap of crumbs. |
| passata | pass | Glass bottle, red with `crimson` rim (19 % on red), tomato label, gold cap. |
| canned-tomatoes | pass | Open tin with peeled tomatoes, green label with tomato. |
| tomato-paste | pass | Pilot toothpaste tube geometry recoloured: red label with tomato, green cap (differs from toothpaste's blue label/red cap). |
| pasta-sauce | pass | Squat jar, red with `crimson` rim (22 % on red, the lane's worst), label tomato + basil. |
| pesto | pass | Green jar with flecks, basil label, silver lid. |
| canned-tuna | pass | Two stacked flat tins, blue label with fish, ring pull. |
| canned-mackerel | pass (2 fixes) | v1 fillet stripes as chevrons read as arrows: wavy tiger stripes. Silver tin 20 % on teal: `steel` rim 1.6 -> 2.2 px. |
| pickled-herring | pass | Rollmops (silver-skinned spirals) in brine with dill, blue lid. |
| anchovies | pass | Gold oil jar with rose/brown fillet layers, fish label. |
| sardines | pass (fix) | Three silver sardines in oil in an oblong tin; `steel` rim 2.2 px (was 20 % on teal). |
| chickpeas | pass | Open tin heaped with chickpeas, yellow label. |
| beans | pass | Kidney beans, navy label. |
| baked-beans | pass | Orange beans in sauce, blue label. |
| sweetcorn | pass | Kernel heap, green label. |
| coconut-milk | pass | Blue tin + coconut half in front. |
| gherkins | pass | Three bumpy gherkins and dill in brine. |
| pickled-beetroot | pass | Ringed beet slices in dark juice (`wine` stands in for purple; no new colour). |
| pickled-red-cabbage | pass | Pink/rose shreds in `wine` juice, green lid. Palette has no purple: `wine` is the closest. |
| capers | pass | Smaller jar (86 %) packed with caper dots, blue lid. |
| sun-dried-tomatoes | pass | Wrinkled tomato pieces in gold oil, herb leaves. |
| artichoke-hearts | pass | Four artichoke halves (layered greens) in oil. |
| canned-fruit | pass (fix) | Peach halves + cherry in syrup. v1 had 9 colours: lid inner -> white, stem -> amber (7). |
| soup | pass (fix) | Bowl with handles, orange soup with vegetables, steam. Handles were 1.3 px from the edge: moved in. |
| vegetable-oil | pass | Yellow PET bottle, sunflower label. |
| vinegar | pass | Pale wine vinegar bottle, grape label. |
| balsamic-vinegar | pass | Dark round flask, gold cap and drop label. |
| salt | pass (fix) | Blue canister + salt heap. v1 spout hidden behind the lid: moved to the rim. |
| black-pepper | pass | Wooden mill + peppercorns. |
| chili-flakes | pass | Flake-speckled jar, chilli label. |
| paprika | pass (fix) | Red powder jar (`crimson` rim). v1 pepper motif read as an apple: lobed pepper with creases. |
| oregano | pass | Green flakes, sprig label. |
| dried-herbs | pass (fix) | Tied herb bundle; v1 entered the padding: scaled 0.9. |
| bay-leaves | pass | Two bay leaves with veins (15 % on teal, the leaf side). |
| cinnamon | pass (fix) | Three rolled sticks; twine re-angled across the bundle. |
| curry-powder | pass | Gold powder heap in a white ramekin. |
| cumin | pass | Seed jar, seed label. |
| nutmeg | pass | Whole nut + halved nut with marbling. |
| spices | pass | Three mini spice jars (red, green, gold). |
| stock-cubes | pass (fix) | Two gold-foil cubes with herb print; v1 too small: scaled 1.15. |
| ketchup | pass | Inverted squeeze bottle, `crimson` rim (20 % on red). |
| mayonnaise | pass | Cream jar, yellow label with halved egg, blue lid. |
| mustard | pass | Yellow nozzle bottle, mustard-seed label. |
| remoulade | pass | Inverted squeeze bottle, butter with relish flecks, gherkin label, green cap. |
| crispy-onions | pass | Tub with orange onion-ring band, heap of fried onion curls. |
| soy-sauce | pass | Dark bottle with green dispenser cap (not the well-known red one), soybean pod label. |
| hot-sauce | pass (fix) | Slim red bottle (`crimson` rim), flame label, chilli; chilli moved up out of the padding. |
| bbq-sauce | pass | Brown squeeze bottle, flame label. |
| sugar | pass | Three white sugar cubes. |
| brown-sugar | pass | Same cubes in tan/crust/brown. |
| icing-sugar | pass | Sieve dusting a white heap. |
| baking-powder | pass (fix) | Crimped blue sachet with cupcake; scaled 0.92 into the padding. |
| baking-soda | pass | White carton, orange band, powder disc with fizz. |
| yeast | pass (3 fixes) | v1/v2 block with blue wrapper read as a layer cake; v3 block on an unfolded wrapper; cast shadow kept on the wrapper. |
| vanilla | pass | Two pods + vanilla flower. |
| cocoa-powder | pass | Open tin heaped with cocoa, brown label with bean. |
| gelatine | pass (fix) | Three fanned sheets with diamond texture. v1 used a clipPath, which the validator's box ignores (clipped): lines computed inside the sheet instead. |

Contrast (spec red / spec teal, mean over the 79 icons of the three sections): weak edge 1.4 % / 0.9 %, median ΔE 60 / 55.
Worst: pasta-sauce 22 %, ketchup 20 %, passata 19 %, paprika 16 % on red; bay-leaves 15 %, oregano 14 % on teal.
