# Icon inspection log, lane l6 (household, personal-care, baby, pets)

Date 2026-10-22, branch `icons-l6` (from `item-icons` a586de0). Style: variant B (no outline), palette only, no new colours.
Every icon was looked at in 220 px inspection renders (red/teal), in the 1x (58 px) and 2x contact sheets on the spec
tiles, light and dark, and in the dark web sheet. `icons validate`: 0 errors, 0 warnings. `icons contrast`: all under
the 25 % gate on both spec tiles (worst: painkillers 12 % on teal, the silver blister).

Sheets: `~/scratch/icon-full/l6/` on nezopt (`<section>-{light-1x,dark-1x,light-2x,dark-web}.png`, `contrast-<section>.txt`).

Shared shape language across the lane: trigger sprays (all-purpose, glass) reuse the household section's spray head;
jugs with a handle hole (laundry, bleach); foil/cling/baking paper share one open dispenser box, told apart by box
colour and sheet (silver foil, sky film, cream parchment with cookies); sacks (charcoal, dog food, bird food) share the
flour sack form with their own colour and label motif; bottles carry a white label with one motif.

## household (23 new; dish-soap, sponges, toilet-paper from the pilot)

| Icon | Result | Notes |
| --- | --- | --- |
| dishwasher-tablets | pass | Two blue/white tablets with a red ball. |
| laundry-detergent | pass | Blue jug with handle hole, T-shirt on the label. |
| fabric-softener | pass | Pink waisted bottle, ridged white cap, folded towels on the label. |
| stain-remover | pass | White T-shirt with a brown stain and a sparkle, small orange bottle. |
| all-purpose-cleaner | pass | Yellow trigger spray, sparkles on the label. |
| bleach | pass | White jug with handle, gold child-safe cap, blue label with a white burst. |
| toilet-cleaner | pass | Bent-neck bottle (the distinguishing shape), toilet-bowl motif. |
| limescale-remover | pass | Chrome tap with drop and sparkles + small green bottle. Silver tap has a `steel` rim (teal 5 %). |
| glass-cleaner | pass | Sky trigger spray with `blue` rim, window-pane label. |
| dishcloths | pass (fix) | Stack of three folded cloths; v1 had a hanging loop that read as a stray hook, removed. |
| rubber-gloves | pass (fix) | Yellow glove; v1 had 2 colours (validator warning): cuff lip in `amber`. |
| paper-towels | pass | Kitchen roll on a wooden stand (tells it from toilet paper). |
| bin-bags | pass (fix) | Tied navy bag; v1 had 2 colours: the tied ears in `blue`. |
| aluminium-foil | pass | Navy/blue box, silver crinkled sheet with `steel` rim (teal 6 %). |
| cling-film | pass | Green box, translucent sky film with `blue` rim. |
| baking-paper | pass | Orange box, cream parchment with cookies. |
| freezer-bags | pass | Zip bag with snowflake and write-on strip; sky with `blue` rim. |
| disposable-tableware | pass | Paper plate + striped paper cup. |
| light-bulbs | pass | Yellow bulb, filament, screw base, glow ticks. |
| batteries | pass | Two cells, gold top with a plus shape. |
| candles | pass | Two pillar candles with drips and flames. |
| matches | pass (fix) | Box with match heads + lit match; v1 had 8 colours, burnt head now `red`. |
| charcoal | pass (fix) | Paper sack with flame label + charcoal lumps; removed a stray highlight dot. |

## personal-care (21 new; toothpaste, shampoo from the pilot)

| Icon | Result | Notes |
| --- | --- | --- |
| toothbrush | pass | Diagonal blue brush with white grip and bristles. Slimmer than most icons by nature. |
| dental-floss | pass | Dispenser with floss thread and a tooth. |
| mouthwash | pass | Green bottle, ribbed cap, tooth on the label. |
| conditioner | pass | Butter bottle standing on its navy cap (upside down, unlike shampoo), hair-wave motif. |
| shower-gel | pass | Orange waisted bottle, water drops, bubbles. |
| soap | pass | Cream bar on a blue dish with bubbles (the section icon is a pink bar). |
| deodorant | pass | Slim navy spray can with mist dots. |
| razors | pass | Orange handle, navy head with blades and a lime strip. Silver blades on teal 8 %. |
| shaving-foam | pass | Blue can + foam cloud. |
| body-lotion | pass | White pump bottle with a cream dollop. |
| sunscreen | pass | Orange bottle, sun and sea on the label. |
| lip-balm | pass (fix) | Stick + cap. Pink balm tip on red was 17.5 %: rim changed `rose` -> `crimson`, now 4 %. |
| cotton-pads | pass | Banded stack + one quilted pad. |
| tissues | pass (fix) | v1 tissue read as a flame/cone; redrawn as a wide scalloped sheet. |
| sanitary-pads | pass | Winged pad, diagonal. |
| tampons | pass (fix) | Unwrapped + pink-wrapped tampon; rotation clipped (0 px): scaled 0.92. |
| painkillers | pass (fix) | Blister pack + loose tablet; blister clipped the left padding (1.8 px), shifted 1.5 px. Worst contrast of the lane (teal 12 %), silver with `steel` rim. |
| vitamins | pass | Amber jar, citrus label, two capsules. |
| plasters | pass | Two crossed plasters. |
| hand-sanitiser | pass | Sky gel pump bottle with `blue` rim, hand on the label. |
| condoms | pass | Two serrated foil packets with ring. |

## baby (9 new)

| Icon | Result | Notes |
| --- | --- | --- |
| nappies | pass (fix) | v1 U-shape read as a shield; redrawn with leg cut-outs and side tabs. |
| baby-wipes | pass | Pink soft pack, lid, wipe popping out. |
| baby-formula | pass | Tin with blue lid, feeding-bottle motif. |
| baby-food | pass | Orange puree jar with carrot label + spoon. |
| baby-cereal | pass (fix) | Cereal box + bowl and spoon; 8 colours, linen dropped. |
| baby-wash | pass | Rounded sky bottle, duck silhouette (no eye), bubbles. |
| nappy-cream | pass | Open tub with cream swirl, lid behind. |
| dummy | pass | Pacifier: shield, ring, teat. |
| baby-bottle | pass | Diagonal bottle with pink collar (the section icon is upright with blue). |

## pets (7 new)

| Icon | Result | Notes |
| --- | --- | --- |
| cat-food | pass | Tin with pull ring, cat-head silhouette (no face). |
| cat-treats | pass | Wine pouch with fish window + fish-shaped treats. |
| cat-litter | pass | Litter tray with scoop and paw print. |
| dog-food | pass | Green sack with dog-head silhouette + kibble. |
| dog-treats | pass (2 fixes) | v1 shadow shapes broke the bone outline and clipped the padding; redrawn as two layered bones, scaled 0.9. |
| dog-poop-bags | pass (fix) | v1 had a highlight spilling off the roll and a detached bag strip; strip now comes from under the roll. |
| bird-food | pass (fix) | Yellow seed sack, blue bird silhouette + seed pile; 9 colours cut to 7. |

No palette colour was missing; no new colour needed.
