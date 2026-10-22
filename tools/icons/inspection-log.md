# Icon inspection log

Every icon is looked at in the contact sheets (`tools/icons/sheets.sh`): at 1x (the real 58 px) and 2x, on the spec
tiles (`#f16a6f`, `#6eb8ae`) and the web tiles (`#ff3b6b`, `#14cfc1`), light and dark page. Checks: recognisable at
58 px without its label, consistent with the set (size, light from the top left, palette), separated from both tile
colours (`icons contrast` gate ≤ 25 % weak edge), no clipping, no stray shapes. Style: variant B (no outline).

## Pilot, 2026-10-22

### Items (30)

| Icon | Result | Notes |
| --- | --- | --- |
| apple | pass (fix) | Red on the red tile was weak (28 % weak edge): added the 2 px `crimson` tonal rim. Now 22 %, the leaf/stem side. |
| avocado | pass | Half with pit reads at once; `forest` skin separates from teal. |
| banana | pass (3 fixes) | v1: back banana poked out and was clipped (-0.8 px). v2: two tips, thin, highlight stroke fell outside the body. v3: redrawn from two circular arcs (tapered crescent), back banana rotated round the shared stem. |
| bread | pass | Golden loaf with three slashes; 3 colours by design. |
| broccoli | pass | Floret cluster in two greens + `lime` stalk; distinct from teal by hue and value. |
| butter | pass (fix) | v1 (block with a blue wrapper) read as cheese or a sponge. v2: block on a white butter dish with the foil peeled back. |
| carrot | pass | Diagonal, ridges, three fronds. |
| cheese | pass | Wedge with holes; 3 yellows. |
| chicken | pass | Roast chicken on a plate; drumstick bones read at 1x. |
| coffee | pass | Cup, saucer, steam. Steam is white and reads on both tiles. |
| croissant | pass (2 fixes) | v1 (five separate blobs) read as a fan. v2: one crescent silhouette with band lines. v3: darker `brown` tips (validator: only 2 colours). |
| dish-soap | pass | Green squeeze bottle, drop label, bubbles. Long label ("Washing-up liquid") is a tile-text matter, not the icon. |
| eggs | pass | Two brown and one white egg in a carton. |
| flour | pass | Paper sack with wheat ear and blue band. |
| ham | pass | Leg with pink cut face, fat ring and bone; `crust` rind keeps it off the red tile. |
| ice-cream | pass | Cone, strawberry and chocolate scoops, cherry. |
| milk | pass | Gable carton, blue label with a drop. |
| olive-oil | pass | Bottle with olive-branch label. |
| pasta | pass (2 fixes) | v1: flat penne read as zigzag ribbons. v2: thicker tubes with visible openings. v3: scaled 1.2 (spanned only 44/64). |
| red-wine | pass | Bottle + glass; translucent upper glass reads as glass on both tiles. |
| rice | pass | Bowl heaped with rice, blue bowl with wave. |
| rye-bread | pass | Dark loaf + slice with seeds; clearly different from `bread`. |
| shampoo | pass | Pink bottle, blue cap, bubbles; pink stays lighter than the red tile. |
| sponges | pass | Yellow sponge with green scourer and suds. |
| toilet-paper | pass (fix) | v1: the loose sheet looked like a separate box. v2: sheet hangs from the roll front with a perforation line. |
| tomato | pass (fix) | Red on red was the weakest (41 %): `crimson` tonal rim, now 17 %. |
| toothpaste | pass (2 fixes) | Scaled 1.1 it clipped (-0.5 px); then thickened (1.25 across) and fitted in the padding. |
| water | pass (fix) | Sky bottle on teal was weak (15 %): `blue` tonal rim, now 5 %. |
| white-fish | pass | Silver-white fish, eye, fins. |
| yogurt | pass | Pot with foil lid and strawberry band. |

### Sections (17)

| Icon | Result | Notes |
| --- | --- | --- |
| produce | pass (2 fixes) | v1 (apple + carrot + broccoli) was 2.7 KB and broke the padding: now apple + broccoli. |
| bakery | pass | Baguette. |
| dairy-eggs | pass | Milk bottle + egg. |
| meat-fish | pass (fix) | v1 steak with pink stripes looked like lips: new outline, softer marbling, bone. |
| deli | pass (fix) | v1 salami log was lumpy (read as a sweet potato): straight rounded log + slice. |
| frozen | pass (fix) | Ice cube with snowflakes; sky on teal (15 %): `blue` rim. |
| pasta-rice-grains | pass (fix) | Wheat sheaf; scaled 0.9 into the padding. |
| canned-jars | pass | Tin with label. |
| condiments-spices | pass | Salt shaker. |
| breakfast-spreads | pass | Jam jar with dotted cloth lid. |
| snacks-sweets | pass (fix) | Chocolate bar; rotation clipped the corners: scaled 0.86. |
| drinks | pass | Juice glass, straw, citrus slice. |
| household | pass (fix) | Spray bottle; sky body on teal (13 %): white body, sky label. |
| personal-care | pass (fix) | Soap bar; sky on teal (25 %): pink soap. |
| baby | pass | Feeding bottle. |
| pets | pass (fix) | Bowl with bone; red bowl on red tile (19 %): blue bowl. |
| other | pass (fix) | Paper bag; v1 had a smiley face (no faces in the set): small red heart. |

Contrast after fixes (spec red / spec teal, mean over 47): weak edge 1.3 % / 0.5 %, median ΔE 61 / 54, luminance
contrast 2.06 / 1.93. Worst: apple 22 %, tomato 17 %, produce 17 % on red; all under the 25 % gate.

Web UI check (`tools/icons/ui-preview.mjs`, 390 and 1440 px, light and dark): 36 illustrations per view, none broken;
to-buy (red), recent (teal) and catalog tiles all show the art at the emoji's size.
