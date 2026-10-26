"""Lucina GLB -> USDZ for the iOS AR view (RealityKit). Blender script (Blender's API forces Python).

    blender -b --factory-startup --python tools/lucina-usdz/convert_usdz.py -- \
        --glb ~/scratch/lucina-avatar-g/lucina.glb --manifest ~/scratch/lucina-avatar-g/manifest.json \
        --anims ~/scratch/lucina-anim/out/g/lucina-anims.glb --clips ~/scratch/lucina-anim/out/g/clips.json \
        --out ~/scratch/lucina-avatar-g/ar/lucina.usdz --table ~/scratch/lucina-avatar-g/ar/lucina-ar.json

What it does:
- imports the live model (meshopt + webp decoded by Blender), drops the nodes the manifest hides (the sword);
- puts the chosen body clips one after another on one timeline (NLA strips, 30 fps) and writes the clip table
  (`lucina-ar.json`: name, start, end in seconds, loop) the app trims them with;
- adds an inverted-hull outline: a copy of each mesh pushed out along its normals by the per-vertex outline
  width (COLOR_0 R), faces flipped, flat outline colour from the manifest's toon config (no shape keys);
- re-encodes the textures as PNG (USDZ has no WebP) and exports USDZ, Y up, metres, with the skeleton,
  the face shape keys (mouth_a etc. for lip-sync) and the animation.

The output is derived from the private model: it goes next to the model (scratch, then the server's
private assets dir), never into the repo.
"""

import argparse
import base64
import json
import os
import struct
import sys
import tempfile

import bmesh
import bpy

DEFAULT_CLIPS = [
    "idle_breathe",
    "greet_wave",
    "happy_bounce",
    "nod_yes",
    "cheer",
    "surprised_step",
    "point_right",
    "talk_gesture_1",
]
FPS = 30
GAP = 4  # frames of hold between clips
OUTLINE_WIDTH = 0.0022  # metres at COLOR_0 R = 1


def args():
    argv = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    p = argparse.ArgumentParser()
    p.add_argument("--glb", required=True)
    p.add_argument("--manifest", required=True)
    p.add_argument("--anims", required=True)
    p.add_argument("--clips", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--table", required=True)
    p.add_argument("--only", default=",".join(DEFAULT_CLIPS), help="comma-separated clip names, idle first")
    p.add_argument("--no-outline", action="store_true")
    p.add_argument("--blend", default="", help="also save the prepared scene here (debugging)")
    return p.parse_args(argv)


def log(*a):
    print("[usdz]", *a, flush=True)


def import_glb(path):
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(
        filepath=path, import_webp_texture=True, import_pack_images=True, guess_original_bind_pose=True, disable_bone_shape=True
    )
    return [o for o in bpy.data.objects if o not in before]


def read_glb(path):
    b = open(path, "rb").read()
    magic, _, _ = struct.unpack("<III", b[:12])
    assert magic == 0x46546C67, path
    off, doc, binary = 12, None, b""
    while off < len(b):
        n, kind = struct.unpack("<II", b[off : off + 8])
        chunk = b[off + 8 : off + 8 + n]
        if kind == 0x4E4F534A:
            doc = json.loads(chunk)
        elif kind == 0x004E4942:
            binary = chunk
        off += 8 + n
    return doc, binary


def write_glb(path, doc, binary):
    js = json.dumps(doc, separators=(",", ":")).encode()
    js += b" " * (-len(js) % 4)
    binary += b"\0" * (-len(binary) % 4)
    total = 12 + 8 + len(js) + 8 + len(binary)
    with open(path, "wb") as f:
        f.write(struct.pack("<III", 0x46546C67, 2, total))
        f.write(struct.pack("<II", len(js), 0x4E4F534A) + js)
        f.write(struct.pack("<II", len(binary), 0x004E4942) + binary)


def merge_clips(model_path, anims_path, out_path):
    """The model plus the library's animations retargeted by node name, so Blender's glTF importer turns
    them into armature actions (the library animates bare nodes; imported alone they become empties)."""
    doc, binary = read_glb(model_path)
    lib, lib_bin = read_glb(anims_path)
    by_name = {n.get("name"): i for i, n in enumerate(doc["nodes"])}
    buf_map = {}
    for i, buf in enumerate(lib.get("buffers", [])):
        nb = dict(buf)
        if i == 0 and "uri" not in buf:
            nb["uri"] = "data:application/octet-stream;base64," + base64.b64encode(lib_bin).decode()
        buf_map[i] = len(doc["buffers"])
        doc["buffers"].append(nb)
    bv_off = len(doc["bufferViews"])
    for bv in lib["bufferViews"]:
        nbv = json.loads(json.dumps(bv))
        nbv["buffer"] = buf_map[bv["buffer"]]
        ext = nbv.get("extensions", {}).get("EXT_meshopt_compression")
        if ext:
            ext["buffer"] = buf_map[ext["buffer"]]
        doc["bufferViews"].append(nbv)
    acc_off = len(doc["accessors"])
    for acc in lib["accessors"]:
        na = dict(acc)
        if "bufferView" in na:
            na["bufferView"] += bv_off
        doc["accessors"].append(na)
    doc.setdefault("animations", [])
    missing = set()
    for anim in lib.get("animations", []):
        channels = []
        for ch in anim["channels"]:
            name = lib["nodes"][ch["target"]["node"]].get("name")
            if name not in by_name:
                missing.add(name)
                continue
            channels.append({"sampler": ch["sampler"], "target": {"node": by_name[name], "path": ch["target"]["path"]}})
        samplers = [dict(s, input=s["input"] + acc_off, output=s["output"] + acc_off) for s in anim["samplers"]]
        doc["animations"].append({"name": anim.get("name"), "channels": channels, "samplers": samplers})
    for ext in lib.get("extensionsUsed", []):
        if ext not in doc.setdefault("extensionsUsed", []):
            doc["extensionsUsed"].append(ext)
    write_glb(out_path, doc, binary)
    log("merged", len(lib.get("animations", [])), "clips; nodes missing in the model:", sorted(missing))


def srgb_to_linear(rgb):
    """The toon config's colours are sRGB (three.js `setRGB(..., SRGBColorSpace)`); Blender's shader inputs are linear."""
    return [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in rgb]


def logical_outline_colors(manifest):
    """Game material name -> outline RGB, from the manifest's toon config."""
    toon = manifest.get("toon") or {}
    default = ((toon.get("default") or {}).get("outline") or {}).get("color") or [0.2, 0.2, 0.35]
    by_logical = {k: ((v or {}).get("outline") or {}).get("color") for k, v in (toon.get("materials") or {}).items()}
    # Imported materials carry the logical names (FaceSkin, Dress, ...); game names map to them too.
    out = {k: v for k, v in by_logical.items() if v}
    for logical, names in (manifest.get("materials") or {}).items():
        color = by_logical.get(logical)
        for n in names:
            if color and n not in out:
                out[n] = color
    return out, default


def outline_material(name, rgb):
    key = f"Outline_{name}"
    m = bpy.data.materials.get(key)
    if m:
        return m
    m = bpy.data.materials.new(key)
    m.use_nodes = True
    m.use_backface_culling = True
    bsdf = m.node_tree.nodes.get("Principled BSDF")
    bsdf.inputs["Base Color"].default_value = (*srgb_to_linear(rgb), 1.0)
    bsdf.inputs["Roughness"].default_value = 1.0
    return m


def add_outline(obj, colors, default):
    """Inverted hull: a skinned copy pushed out by COLOR_0 R * OUTLINE_WIDTH, faces flipped."""
    attr = None
    for a in obj.data.color_attributes:
        attr = a
        break
    if attr is None:
        log("no colour attribute on", obj.name, "- uniform outline")
    hull = obj.copy()
    hull.data = obj.data.copy()
    hull.name = f"{obj.name}_Outline"
    hull.data.name = f"{obj.data.name}_Outline"
    for c in obj.users_collection:
        c.objects.link(hull)
    if hull.data.shape_keys:
        hull.shape_key_clear()
    me = hull.data
    widths = [1.0] * len(me.vertices)
    if attr is not None:
        a = me.color_attributes[attr.name]
        if a.domain == "POINT":
            widths = [d.color[0] for d in a.data]
        else:  # corner
            acc = [0.0] * len(me.vertices)
            cnt = [0] * len(me.vertices)
            for loop, d in zip(me.loops, a.data):
                acc[loop.vertex_index] += d.color[0]
                cnt[loop.vertex_index] += 1
            widths = [acc[i] / cnt[i] if cnt[i] else 0.0 for i in range(len(acc))]
    bm = bmesh.new()
    bm.from_mesh(me)
    bm.verts.ensure_lookup_table()
    bm.normal_update()
    drop = [v for v in bm.verts if widths[v.index] < 0.02]
    keep_w = {v.index: widths[v.index] for v in bm.verts}
    for v in bm.verts:
        w = keep_w[v.index]
        if w >= 0.02:
            v.co += v.normal * (OUTLINE_WIDTH * w)
    bmesh.ops.delete(bm, geom=drop, context="VERTS")
    bmesh.ops.reverse_faces(bm, faces=bm.faces[:])
    bm.to_mesh(me)
    bm.free()
    for i, slot in enumerate(hull.material_slots):
        src = slot.material.name if slot.material else ""
        hull.material_slots[i].link = "DATA"
        me.materials[i] = outline_material(src or "default", colors.get(src, default))
    log("outline", hull.name, len(me.vertices), "verts")
    return hull


def base_color_image(mat):
    """(Principled node, image texture node) feeding Base Color, if any."""
    if not mat or not mat.node_tree:
        return None, None
    bsdf = next((n for n in mat.node_tree.nodes if n.type == "BSDF_PRINCIPLED"), None)
    if not bsdf:
        return None, None
    links = bsdf.inputs["Base Color"].links
    node = links[0].from_node if links else None
    while node is not None and node.type != "TEX_IMAGE":
        ins = [i for i in node.inputs if i.links]
        node = ins[0].links[0].from_node if ins else None
    return bsdf, node


def apply_toon_colors(manifest, tex_dir):
    """Bakes the toon colour tints into what an unlit renderer sees, on a per-material copy of the texture:
    solid colours replace its colour (its alpha stays), tints multiply it. No texture: a flat colour."""
    import numpy as np

    toon = manifest.get("toon") or {}
    cfgs = toon.get("materials") or {}
    for mat in bpy.data.materials:
        cfg = cfgs.get(mat.name)
        if not cfg or not cfg.get("color"):
            continue
        rgb = list(cfg["color"])
        bsdf, tex = base_color_image(mat)
        if bsdf is None:
            continue
        if tex is None or tex.image is None:
            for l in list(bsdf.inputs["Base Color"].links):
                mat.node_tree.links.remove(l)
            bsdf.inputs["Base Color"].default_value = (*srgb_to_linear(rgb), 1.0)
            log("solid colour", mat.name, [round(c, 3) for c in rgb])
            continue
        src = tex.image
        w, h = src.size
        px = np.empty(w * h * 4, dtype=np.float32)
        src.pixels.foreach_get(px)
        px = px.reshape(-1, 4)
        if cfg.get("solidColor"):
            px[:, :3] = np.array(rgb, dtype=np.float32)  # the colour only; the texture keeps its alpha
        else:
            px[:, :3] *= np.array(rgb, dtype=np.float32)
        img = bpy.data.images.new(f"{mat.name}_tinted", w, h, alpha=True)
        img.colorspace_settings.name = src.colorspace_settings.name
        img.pixels.foreach_set(px.ravel())
        path = os.path.join(tex_dir, f"{mat.name}_tinted.png")
        img.filepath_raw = path
        img.file_format = "PNG"
        img.save()
        img.source = "FILE"
        img.filepath = path
        tex.image = img
        log("tinted", mat.name, src.name, [round(c, 3) for c in rgb])


def blend_alpha_maps():
    """Alpha-mapped materials (lashes, eye highlights) as alpha blending, not an alpha clip: RealityKit keeps
    the opacity texture only for blended materials (with a clip threshold it renders the lash cards opaque)."""
    for mat in bpy.data.materials:
        bsdf = next((n for n in mat.node_tree.nodes if n.type == "BSDF_PRINCIPLED"), None) if mat.node_tree else None
        if not bsdf or not bsdf.inputs["Alpha"].links:
            continue
        # Drop an alpha-clip stage (Math nodes between the alpha and the BSDF): the exporter turns it into
        # opacityThreshold.
        link = bsdf.inputs["Alpha"].links[0]
        node, guard = link.from_node, 0
        while node.type == "MATH" and node.operation in {"GREATER_THAN", "LESS_THAN", "ROUND", "SUBTRACT", "ADD", "MULTIPLY_ADD"} and guard < 4:
            src = next((i.links[0] for i in node.inputs if i.links), None)
            if src is None:
                break
            mat.node_tree.links.new(src.from_socket, bsdf.inputs["Alpha"])
            node, guard = src.from_node, guard + 1
        mat.blend_method = "BLEND"
        mat.surface_render_method = "BLENDED"
        log("alpha blend", mat.name)


def find_action(name, actions):
    for a in actions:
        if a.name == name or a.name.startswith(name + "_") or a.name.startswith(name + "."):
            return a
    return None


def main():
    a = args()
    manifest = json.load(open(os.path.expanduser(a.manifest)))
    clips_meta = {c["name"]: c for c in json.load(open(os.path.expanduser(a.clips)))["clips"]}

    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.render.fps = FPS

    merged = os.path.join(tempfile.mkdtemp(prefix="lucina-usdz-"), "merged.glb")
    merge_clips(os.path.expanduser(a.glb), os.path.expanduser(a.anims), merged)
    before_actions = set(bpy.data.actions)
    model = import_glb(merged)
    actions = [x for x in bpy.data.actions if x not in before_actions]
    arm = next(o for o in model if o.type == "ARMATURE")
    for name in manifest.get("hide") or []:
        for o in [o for o in model if o.name == name or (o.data and getattr(o.data, "name", "") == name)]:
            bpy.data.objects.remove(o, do_unlink=True)
    meshes = [o for o in bpy.data.objects if o.type == "MESH"]
    log("model", arm.name, [m.name for m in meshes])

    for x in actions:
        x.use_fake_user = True
    log("actions", [(x.name, tuple(x.frame_range)) for x in actions])

    if arm.animation_data is None:
        arm.animation_data_create()
    arm.animation_data.action = None
    for t in list(arm.animation_data.nla_tracks):
        arm.animation_data.nla_tracks.remove(t)
    track = arm.animation_data.nla_tracks.new()
    track.name = "AR"
    table = []
    cursor = 1
    for name in [n.strip() for n in a.only.split(",") if n.strip()]:
        act = find_action(name, actions)
        meta = clips_meta.get(name)
        if not act or not meta:
            log("skip clip (not found)", name)
            continue
        strip = track.strips.new(name, cursor, act)
        if hasattr(strip, "action_slot") and act.slots:
            strip.action_slot = act.slots[0]
        strip.extrapolation = "HOLD_FORWARD" if not table else "NOTHING"
        strip.blend_type = "REPLACE"
        length = int(round(strip.frame_end - strip.frame_start))
        table.append({
            "name": name,
            "start": round((cursor - 1) / FPS, 4),
            "end": round((cursor - 1 + length) / FPS, 4),
            "loop": bool(meta.get("loop")),
            "tags": meta.get("tags") or [],
        })
        cursor += length + GAP
    if not table:
        raise SystemExit("no clips")
    scene.frame_start = 1
    scene.frame_end = cursor - GAP
    log("timeline", scene.frame_end, "frames", [(c["name"], c["start"], c["end"]) for c in table])

    if not a.no_outline:
        colors, default = logical_outline_colors(manifest)
        for m in meshes:
            add_outline(m, colors, default)

    # USDZ carries PNG/JPEG only: re-encode the (WebP) textures.
    tex_dir = os.path.join(os.path.dirname(os.path.abspath(os.path.expanduser(a.out))), "tex")
    os.makedirs(tex_dir, exist_ok=True)
    apply_toon_colors(manifest, tex_dir)
    blend_alpha_maps()
    for img in bpy.data.images:
        if img.type != "IMAGE" or (not img.has_data and not img.packed_file) or img.name.endswith("_tinted"):
            continue
        safe = "".join(ch if ch.isalnum() or ch in "-_" else "_" for ch in os.path.splitext(img.name)[0])
        path = os.path.join(tex_dir, safe + ".png")
        img.pixels[0:1]  # force load
        img.filepath_raw = path
        img.file_format = "PNG"
        img.save()
        if img.packed_file:
            img.unpack(method="REMOVE")
        img.filepath = path
        img.reload()
    log("textures", len(bpy.data.images))

    if a.blend:
        bpy.ops.wm.save_as_mainfile(filepath=os.path.expanduser(a.blend))

    out = os.path.expanduser(a.out)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    bpy.ops.wm.usd_export(
        filepath=out,
        export_animation=True,
        export_armatures=True,
        only_deform_bones=False,
        export_shapekeys=True,
        export_materials=True,
        generate_preview_surface=True,
        export_textures_mode="NEW",
        overwrite_textures=True,
        convert_orientation=True,
        export_global_forward_selection="NEGATIVE_Z",
        export_global_up_selection="Y",
        export_lights=False,
        export_cameras=False,
        triangulate_meshes=True,
        evaluation_mode="RENDER",
        root_prim_path="/Lucina",
        convert_world_material=False,
        accessibility_label="Lucina",
    )
    table_doc = {
        "file": os.path.basename(out),
        "fps": FPS,
        "height_m": manifest.get("height_m"),
        "facing": "+Z",
        "morphs": {k: k for k in ("mouth_a", "mouth_o", "blink", "smile")},
        "clips": table,
    }
    with open(os.path.expanduser(a.table), "w") as f:
        json.dump(table_doc, f, indent=1)
    log("wrote", out, os.path.getsize(out), "bytes;", a.table)


main()
