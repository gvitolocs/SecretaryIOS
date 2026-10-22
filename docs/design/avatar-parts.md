# Avatar parts: outfit slots, accessories and saved looks

This is the contract between the asset pipelines and the stage. The outfit refits come from
`~/scratch/lucina-wardrobe/` and land in `~/scratch/wardrobe-stage-g/`. The editor's accessories
come from `tools/avatar/accessories.py`. The stage side is `packages/lucina-stage/src/editor/`.
All asset names are served from the private assets directory (`/v1/assets/<name>`) and listed
in its `wardrobe.json`.

## 1. Whole outfits (unchanged)

`lucina-<outfit>.glb` plus an optional `lucina-<outfit>.manifest.json`, listed under
`outfits`. The wardrobe swaps the whole model, and the editor re-applies the current look to
the new model before it is revealed (stage option `prepareModel`).

## 2. Mix and match: the base and slot parts

### The base

`lucina-g-base.glb` is listed as the outfit `{"id": "base", "tags": ["base"], ...}`. It has G's
head, face, hair, tiara and skeleton, the full body skin and plain underwear
(`Clothes_Underwear`). The skin material is `SkinTinted`, which the editor's skin tone tints.
The body skin is split into **region meshes** named:

```
Body_neck  Body_chest  Body_belly  Body_back_upper  Body_back_lower  Body_hips
Body_upperarm_L/R  Body_forearm_L/R  Body_hand_L/R
Body_thigh_L/R  Body_knee_L/R  Body_shin_L/R  Body_foot_L/R
```

When a look picks any part, the stage switches to `base` and puts the parts on it.

### A part

- **File:** `lucina-<outfit>-<slot>.glb`, where the slot is one of `top`, `bottom`, `onepiece`,
  `legwear`, `shoes`, `gloves`.
- **Meshes:** skinned meshes only, bound to G's bone names, in G's rest pose.
- **Skeleton:** the GLB carries the full G skeleton. Extra spring bones of the outfit, such as
  `skirt0_0..skirt7_2` or `ptail_0..3`, are grafted onto the base under their parent bone of the
  same name.
- **Materials:** named `Clothes_<Outfit>_<Slot>[_n]`.

Per-part manifest `lucina-<outfit>-<slot>.json`. The same fields are merged into `wardrobe.json`
under `parts`:

```json
{
  "id": "maid-onepiece",          // <outfit>-<slot>
  "outfit": "maid",
  "slot": "onepiece",
  "glb": "lucina-maid-onepiece.glb",
  "thumbnail": "lucina-maid-onepiece.png",
  "name": {"it": "...", "en": "...", "da": "..."},
  "hides": ["Body_chest", "Body_belly", "Body_hips"],
  "occupies": ["top", "bottom"],
  "toon": {"materials": {"Clothes_Maid_Onepiece": {"preset": "lit", "ramp": "dress", "outline": {}}}},
  "credits": "...",
  "licence": "..."
}
```

- **`hides`** is the culling mask. It lists the base's `Body_*` region meshes that are hidden
  while the part is worn. The wardrobe pipeline computes it by raycasting from each region to
  the part with a margin. A region is hidden when 85% or more of its area is covered. Named
  regions are used instead of a per-triangle bitmask, because meshopt reorders vertices.
- **`occupies`** lists the slots the part fills besides its own. A `onepiece` always occupies
  `top` and `bottom`. The last pick wins: picking a top removes a onepiece, and picking a
  onepiece removes the top and the bottom (`resolveParts`).
- **`toon`** uses the same keys as a manifest's `toon.materials`. Materials it does not name
  inherit the base's `Dress` look.

The server (`valid_part`) checks the id, outfit, slot, asset names, `Body_*` names, slots and
string sizes. Invalid entries are dropped and logged.

## 3. Accessories

`acc-<id>.glb` is a prop **modelled in G's rest space**: metres, Y up, facing +Z, the space of
`lucina.glb` at rest. Wardrobe entry:

```json
{"id": "beret", "slot": "head", "glb": "acc-beret.glb", "thumbnail": "acc-beret.png",
 "name": {"it": "Basco", "en": "Beret", "da": "Baret"}, "bone": "head", "hideTiara": true,
 "credits": "...", "licence": "..."}
```

- **Slots and default bones:** `head`, `hair`, `ears` and `eyes` ride on `head`. `neck` rides on
  `neck`, `back` on `chest`, `wristL` on `leftHand` and `wristR` on `rightHand` (the hand's
  origin is the wrist).
- **Attaching:** a rigid prop is parented to its bone through the bone's rest transform, so it
  sits where it was modelled and then follows the bone. A skinned prop is re-bound by bone name.
- **Body sliders:** they never stretch a prop. Body deforms happen in the skinning matrices only.
  Only `head` and the hands carry a uniform node scale, which a head or wrist prop should follow.
- **`hideTiara`:** hides her own tiara while the prop is worn. The look also has a tiara on/off
  switch.

The starter set (`tools/avatar/accessories.py`; original, made from primitives):

| Slot | Props |
| --- | --- |
| head | headphones, beret, crown, halo |
| hair | ribbon, hairpin, flower |
| ears | cat-ears |
| eyes | round-glasses |
| neck | choker, pendant |
| back | backpack, wings |
| wristL | wristband |
| wristR | wristband-r |

The script also writes `accessories.json`, ready to paste into `wardrobe.json`. The wardrobe
pipeline adds `acc-chef-hat` and `acc-maid-headband` (head).

## 4. Looks

A look is the stage's `Look` JSON (`src/editor/look.ts`, `v: 1`):

```json
{
  "v": 1,
  "body":  {"height": 50, "head": 50, "...": 50},
  "face":  {"eyeSize": 50, "...": 50},
  "eyes":  {"iris": "#3d6fd1", "highlight": true},
  "skin":  {"tone": null},
  "makeup": {"lipstick": {"color": "#d0445e", "amount": 0}, "eyeliner": {}, "eyeshadow": {}, "blush": {}},
  "hair":  {"style": "ponytail", "color": "#2b3a67", "highlight": null},
  "accessories": {"head": "beret", "wristL": "wristband"},
  "tiara": true,
  "outfit": {"id": "maid", "parts": {"top": "casual-top"}}
}
```

- **Values:** sliders are integers 0..100, and 50 is the model as authored. A null colour or id
  means "whatever the wardrobe says". The `hair.style` and `outfit.id` override the wardrobe's
  pins and rules while the look is worn.
- **Server:** it stores up to 20 named looks and the one she wears (`GET /v1/avatar` →
  `looks`, `current`). Patches:
  - `saveLook: {id?, name, look}`;
  - `deleteLook: "<id>"`;
  - `looks: [...]`, which replaces them all;
  - `current: "<id or name>" | null`.
- **Name matching:** a name in `current` matches case- and accent-insensitively, and so does a
  single word of the name. Lucina's tool can therefore send `{"current": "serata"}` when she is
  asked "mettiti il look serata". An unknown name answers `422 unknown_look` and lists the names.
