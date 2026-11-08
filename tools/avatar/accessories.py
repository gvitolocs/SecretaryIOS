"""Lucina's starter accessories: simple original props with toon-friendly flat materials.

Blender (Python is forced by the bpy API). Run headless, CPU only:

    nice blender --background --factory-startup --disable-autoexec \
        --python tools/avatar/accessories.py -- <out dir>

Writes acc-<id>.glb (rigid props in G's rest space: metres, Y up, facing +Z, the space of
lucina.glb at rest) and acc-<id>.png thumbnails (Cycles on the CPU, 256 px, transparent), plus
accessories.json, the `accessories` list for wardrobe.json. Every prop is authored here from
primitives: no third-party or game asset is used, so these files may be shared freely.

Positions come from G's rest pose (measured on the stage, see docs/design/avatar-parts.md):
head bone (0, 1.455, -0.020), eyes at y 1.509 with the face front at z 0.063, hair top 1.655,
tiara band over the crown, neck/collar around y 1.40-1.43, hands at (+-0.327, 0.9425, 0.0014)
with the forearm axis (+-0.469, -0.864, 0.185), the cape's back at z -0.20 by the chest.
"""

import json
import math
import os
import sys

import bpy
import bmesh
from mathutils import Matrix, Vector

OUT = sys.argv[sys.argv.index("--") + 1] if "--" in sys.argv else "/tmp/lucina-acc"
os.makedirs(OUT, exist_ok=True)


def P(x, y, z):
    """glTF (Y up, +Z forward) -> Blender (Z up): (x, y, z) -> (x, -z, y)."""
    return Vector((x, -z, y))


def clear():
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete()
    for block in (bpy.data.meshes, bpy.data.materials, bpy.data.cameras, bpy.data.lights):
        for b in list(block):
            block.remove(b)


MATS = {}


def mat(name, rgb, metallic=0.0, rough=0.55):
    if name in MATS:
        return MATS[name]
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    bsdf = m.node_tree.nodes.get("Principled BSDF")
    bsdf.inputs["Base Color"].default_value = (*rgb, 1.0)
    bsdf.inputs["Metallic"].default_value = metallic
    bsdf.inputs["Roughness"].default_value = rough
    MATS[name] = m
    return m


def srgb(hex_):
    """sRGB hex -> linear RGB (Blender colours are linear)."""
    h = hex_.lstrip("#")
    c = [int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    return tuple(x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4 for x in c)


def finish(obj, material, smooth=True):
    obj.data.materials.clear()
    obj.data.materials.append(material)
    if smooth:
        for p in obj.data.polygons:
            p.use_smooth = True
    return obj


def torus(name, centre, axis, major, minor, material, seg=40, ring=10, scale=(1, 1, 1)):
    bpy.ops.mesh.primitive_torus_add(major_segments=seg, minor_segments=ring, major_radius=major, minor_radius=minor)
    o = bpy.context.active_object
    o.name = name
    o.scale = scale
    a = axis.normalized()
    o.rotation_euler = Vector((0, 0, 1)).rotation_difference(a).to_euler()
    o.location = centre
    return finish(o, material)


def sphere(name, centre, r, material, scale=(1, 1, 1), seg=24, rings=12):
    bpy.ops.mesh.primitive_uv_sphere_add(segments=seg, ring_count=rings, radius=r)
    o = bpy.context.active_object
    o.name = name
    o.scale = scale
    o.location = centre
    return finish(o, material)


def cylinder(name, a, b, r, material, verts=24):
    d = b - a
    bpy.ops.mesh.primitive_cylinder_add(vertices=verts, radius=r, depth=d.length)
    o = bpy.context.active_object
    o.name = name
    o.rotation_euler = Vector((0, 0, 1)).rotation_difference(d.normalized()).to_euler()
    o.location = (a + b) / 2
    return finish(o, material)


def cone(name, base, tip, r, material, verts=24, r2=0.0):
    d = tip - base
    bpy.ops.mesh.primitive_cone_add(vertices=verts, radius1=r, radius2=r2, depth=d.length)
    o = bpy.context.active_object
    o.name = name
    o.rotation_euler = Vector((0, 0, 1)).rotation_difference(d.normalized()).to_euler()
    o.location = (base + tip) / 2
    return finish(o, material)


def box(name, centre, size, material, rot=(0, 0, 0), bevel=0.0):
    bpy.ops.mesh.primitive_cube_add(size=1)
    o = bpy.context.active_object
    o.name = name
    o.scale = size
    o.rotation_euler = rot
    o.location = centre
    bpy.ops.object.transform_apply(scale=True)
    if bevel > 0:
        mod = o.modifiers.new("bevel", "BEVEL")
        mod.width = bevel
        mod.segments = 3
        bpy.ops.object.modifier_apply(modifier=mod.name)
    return finish(o, material, smooth=bevel > 0)


def petal_shape(name, centre, normal, length, width, angle, material, thick=0.003):
    """A flat rounded petal (a squashed sphere) pointing at `angle` in the plane of `normal`."""
    o = sphere(name, Vector((0, 0, 0)), 1.0, material, seg=16, rings=8)
    o.scale = (length / 2, width / 2, thick)
    bpy.ops.object.transform_apply(scale=True)
    o.location = (length / 2, 0, 0)
    bpy.ops.object.transform_apply(location=True)
    q = Vector((0, 0, 1)).rotation_difference(normal.normalized())
    o.rotation_euler = (q.to_matrix().to_4x4() @ Matrix.Rotation(angle, 4, "Z")).to_euler()
    o.location = centre
    return o


def bow(prefix, centre, normal, up, size, material):
    """A ribbon bow: two loops (flattened tori) and a knot."""
    # Work in the bow's plane: petal angles are measured from `side` around `normal`.
    q = Vector((0, 0, 1)).rotation_difference(normal.normalized())
    local_x = q @ Vector((1, 0, 0))
    side = up.cross(normal).normalized()
    base = math.atan2(local_x.cross(side).dot(normal), local_x.dot(side))
    parts = []
    for k, (ang, ln, wd) in enumerate(((0.35, 1.0, 0.62), (-0.35, 1.0, 0.62), (math.pi - 0.35, 1.0, 0.62), (math.pi + 0.35, 1.0, 0.62))):
        parts.append(petal_shape(f"{prefix}_loop{k}", centre, normal, size * ln, size * wd, ang + base, material, thick=size * 0.12))
    for k, ang in enumerate((-math.pi / 2 - 0.35, -math.pi / 2 + 0.35)):
        parts.append(petal_shape(f"{prefix}_tail{k}", centre, normal, size * 1.15, size * 0.3, ang + base, material, thick=size * 0.06))
    parts.append(sphere(f"{prefix}_knot", centre, size * 0.2, material, scale=(1, 1, 0.8)))
    return parts


def join(objs, name):
    bpy.ops.object.select_all(action="DESELECT")
    for o in objs:
        o.select_set(True)
    bpy.context.view_layer.objects.active = objs[0]
    if len(objs) > 1:
        bpy.ops.object.join()
    o = bpy.context.active_object
    o.name = name
    o.data.name = name
    bpy.ops.object.transform_apply(location=False, rotation=True, scale=True)
    return o


# ---------------------------------------------------------------------------------------------
# The props. Each builder returns its objects; they are exported together as one GLB.

HEAD = P(0, 1.455, -0.020)
EYE_Y = 1.509


def ribbon():
    m = mat("Acc_Ribbon", srgb("#e0446a"), rough=0.45)
    c = P(-0.072, 1.615, -0.035)
    normal = (P(-1, 0.25, -0.35) - P(0, 0, 0)).normalized()
    up = (P(0, 1, 0) - P(0, 0, 0))
    return bow("ribbon", c, normal, (up - normal * up.dot(normal)).normalized(), 0.05, m)


def hairpin():
    gold = mat("Acc_Pin_Gold", srgb("#e8c35a"), metallic=0.6, rough=0.3)
    star = mat("Acc_Pin_Star", srgb("#9fd3ff"), rough=0.35)
    objs = []
    for i, (y, z) in enumerate(((1.575, 0.045), (1.588, 0.030))):
        objs.append(cylinder(f"pin{i}", P(0.074, y, z), P(0.086, y + 0.012, z - 0.022), 0.0022, gold, verts=10))
    c = P(0.083, 1.593, 0.028)
    n = (P(1, 0.1, 0.3) - P(0, 0, 0)).normalized()
    for k in range(5):
        objs.append(petal_shape(f"star{k}", c, n, 0.016, 0.008, k * 2 * math.pi / 5, star, thick=0.0025))
    objs.append(sphere("star_c", c, 0.0045, gold))
    return objs


def flower():
    petal = mat("Acc_Flower_Petal", srgb("#ffd1e3"), rough=0.6)
    heart = mat("Acc_Flower_Heart", srgb("#ffcf4a"), rough=0.5)
    leaf = mat("Acc_Flower_Leaf", srgb("#6fbf73"), rough=0.6)
    c = P(0.084, 1.600, 0.004)
    n = (P(1, 0.25, 0.25) - P(0, 0, 0)).normalized()
    objs = [petal_shape(f"petal{k}", c, n, 0.026, 0.016, k * 2 * math.pi / 6, petal) for k in range(6)]
    objs.append(sphere("heart", c + n * 0.002, 0.0075, heart, scale=(1, 1, 0.7)))
    objs.append(petal_shape("leaf0", c - n * 0.002, n, 0.03, 0.012, -2.2, leaf))
    objs.append(petal_shape("leaf1", c - n * 0.002, n, 0.026, 0.011, -1.5, leaf))
    return objs


def cat_ears():
    outer = mat("Acc_Ears_Outer", srgb("#3a3f6b"), rough=0.7)
    inner = mat("Acc_Ears_Inner", srgb("#ffb3c8"), rough=0.7)
    band = mat("Acc_Ears_Band", srgb("#2b2f52"), rough=0.5)
    objs = []
    for s in (-1, 1):
        base = P(s * 0.058, 1.632, -0.028)
        tip = P(s * 0.085, 1.71, -0.03)
        e = cone(f"ear{s}", base, tip, 0.032, outer, verts=3)
        e.scale = (1, 0.45, 1)
        objs.append(e)
        i = cone(f"inner{s}", base + Vector((0, -0.0075, 0)), tip + Vector((0, -0.006, -0.012)), 0.02, inner, verts=3)
        i.scale = (1, 0.3, 1)
        objs.append(i)
    # A thin headband over the crown, under the hair line.
    hb = torus("band", P(0, 1.545, -0.025), Vector((1, 0, 0)), 0.105, 0.0035, band, seg=48, ring=6)
    objs.append(hb)
    return objs


def round_glasses():
    frame = mat("Acc_Glasses_Frame", srgb("#5a3d2b"), rough=0.4)
    lens = mat("Acc_Glasses_Lens", srgb("#d8eefc"), rough=0.1)
    objs = []
    z = 0.077
    for s in (-1, 1):
        c = P(s * 0.033, EYE_Y - 0.002, z)
        objs.append(torus(f"rim{s}", c, Vector((0, -1, 0)), 0.02, 0.0016, frame, seg=40, ring=6))
        l = sphere(f"lens{s}", c, 0.0195, lens, scale=(1, 0.06, 1))
        objs.append(l)
        objs.append(cylinder(f"arm{s}", P(s * 0.053, EYE_Y + 0.001, z - 0.002), P(s * 0.088, EYE_Y + 0.004, -0.025), 0.0014, frame, verts=8))
    objs.append(cylinder("bridge", P(-0.013, EYE_Y + 0.004, z + 0.002), P(0.013, EYE_Y + 0.004, z + 0.002), 0.0015, frame, verts=8))
    return objs


def choker():
    band = mat("Acc_Choker_Band", srgb("#20202a"), rough=0.5)
    gem = mat("Acc_Choker_Gem", srgb("#e0446a"), rough=0.2)
    t = torus("band", P(0, 1.405, -0.006), Vector((0, 0, 1)), 0.052, 0.005, band, seg=48, ring=8, scale=(1.0, 1.08, 1.0))
    g = sphere("gem", P(0, 1.392, 0.052), 0.008, gem, scale=(1, 0.6, 1.2))
    return [t, g]


def backpack():
    body = mat("Acc_Pack_Body", srgb("#f2a7c3"), rough=0.7)
    trim = mat("Acc_Pack_Trim", srgb("#ffffff"), rough=0.6)
    strap = mat("Acc_Pack_Strap", srgb("#d0789c"), rough=0.6)
    objs = [box("pack", P(0, 1.23, -0.285), (0.17, 0.2, 0.08), body, bevel=0.025)]
    objs.append(box("pocket", P(0, 1.18, -0.33), (0.12, 0.08, 0.025), trim, bevel=0.012))
    objs.append(box("flap", P(0, 1.305, -0.285), (0.172, 0.05, 0.085), strap, bevel=0.015))
    for s in (-1, 1):
        objs.append(cylinder(f"strap{s}", P(s * 0.06, 1.31, -0.25), P(s * 0.08, 1.37, -0.09), 0.006, strap, verts=8))
    return objs


def wristband(side):
    s = 1 if side == "L" else -1
    m = mat("Acc_Wristband", srgb("#7fd1c7"), rough=0.75)
    hand = P(s * 0.327, 0.9425, 0.0014)
    axis = (P(s * 0.4692, -0.8635, 0.1848) - P(0, 0, 0)).normalized()
    c = hand - axis * 0.035
    return [torus(f"band{side}", c, axis, 0.052, 0.011, m, seg=40, ring=10)]


def headphones():
    band = mat("Acc_Phones_Band", srgb("#f4f4f8"), rough=0.4)
    cup = mat("Acc_Phones_Cup", srgb("#ff8fb1"), rough=0.5)
    objs = []
    # An arc over the head from ear to ear (half torus, Y-Z plane in glTF = across the head).
    bpy.ops.mesh.primitive_torus_add(major_segments=48, minor_segments=8, major_radius=0.112, minor_radius=0.007)
    arc = bpy.context.active_object
    arc.name = "arc"
    bm = bmesh.new()
    bm.from_mesh(arc.data)
    bmesh.ops.delete(bm, geom=[v for v in bm.verts if v.co.y < -0.01], context="VERTS")
    bm.to_mesh(arc.data)
    bm.free()
    arc.rotation_euler = (math.pi / 2, 0, 0)  # torus in X-Y -> X-Z (Blender) = across the head, upright
    arc.location = P(0, 1.53, -0.025)
    finish(arc, band)
    objs.append(arc)
    for s in (-1, 1):
        objs.append(cylinder(f"cup{s}", P(s * 0.098, 1.50, -0.025), P(s * 0.128, 1.50, -0.025), 0.034, cup, verts=28))
        objs.append(sphere(f"pad{s}", P(s * 0.101, 1.50, -0.025), 0.03, band, scale=(0.35, 1, 1)))
    return objs


def beret():
    m = mat("Acc_Beret", srgb("#c8344a"), rough=0.8)
    stem = mat("Acc_Beret_Stem", srgb("#a02236"), rough=0.8)
    c = P(0.012, 1.632, -0.03)
    b = sphere("beret", c, 1.0, m, scale=(0.118, 0.13, 0.045), seg=36, rings=14)
    b.rotation_euler = (0.18, -0.22, 0)
    s = cylinder("stem", c + Vector((0.004, 0.0, 0.04)), c + Vector((0.006, 0.0, 0.056)), 0.006, stem, verts=10)
    return [b, s]


def crown():
    gold = mat("Acc_Crown_Gold", srgb("#f1c84b"), metallic=0.7, rough=0.3)
    gem = mat("Acc_Crown_Gem", srgb("#6fa8ff"), rough=0.15)
    c = P(0, 1.645, -0.025)
    ring = cylinder("ring", c, c + Vector((0, 0, 0.022)), 0.06, gold, verts=40)
    bm = bmesh.new()
    bm.from_mesh(ring.data)
    bmesh.ops.delete(bm, geom=[f for f in bm.faces if abs(f.normal.z) > 0.9], context="FACES_ONLY")
    bm.to_mesh(ring.data)
    bm.free()
    ring.modifiers.new("solid", "SOLIDIFY").thickness = 0.003
    bpy.context.view_layer.objects.active = ring
    bpy.ops.object.modifier_apply(modifier="solid")
    objs = [ring]
    for k in range(5):
        a = k * 2 * math.pi / 5 + math.pi / 2
        base = c + Vector((math.cos(a) * 0.06, math.sin(a) * 0.06, 0.018))
        objs.append(cone(f"spike{k}", base, base + Vector((0, 0, 0.03)), 0.012, gold, verts=4))
        objs.append(sphere(f"ball{k}", base + Vector((0, 0, 0.033)), 0.005, gold))
    objs.append(sphere("gem", c + Vector((0, -0.062, 0.011)), 0.008, gem, scale=(1, 0.5, 1)))
    return objs


def halo():
    m = mat("Acc_Halo", srgb("#fff2a8"), rough=0.2)
    return [torus("halo", P(0, 1.70, -0.03), Vector((0, 0.18, 1)), 0.075, 0.008, m, seg=48, ring=10)]


def pendant():
    chain = mat("Acc_Pendant_Chain", srgb("#e6e6ee"), metallic=0.7, rough=0.3)
    gem = mat("Acc_Pendant_Gem", srgb("#8a5cf0"), rough=0.15)
    g = P(0, 1.315, 0.078)
    objs = [sphere("gem", g, 0.012, gem, scale=(0.8, 0.45, 1.2))]
    for s in (-1, 1):
        objs.append(cylinder(f"chain{s}", P(s * 0.05, 1.385, 0.045), g + Vector((s * 0.003, 0, 0.012)), 0.0016, chain, verts=8))
    objs.append(sphere("bail", g + Vector((0, 0, 0.014)), 0.0035, chain))
    return objs


def wings():
    m = mat("Acc_Wings", srgb("#ffffff"), rough=0.5)
    objs = []
    for s in (-1, 1):
        for k, (ln, wd, ang) in enumerate(((0.32, 0.1, 0.35), (0.27, 0.085, 0.0), (0.21, 0.07, -0.35))):
            base = P(s * 0.04, 1.30, -0.265)
            n = (P(0, 0, -1) - P(0, 0, 0)).normalized()
            angle = (math.pi / 2 - 0.55 + ang) if s > 0 else (math.pi / 2 + 0.55 - ang)
            o = petal_shape(f"feather{s}{k}", base, n, ln, wd, angle if s > 0 else math.pi - (math.pi - angle), m, thick=0.006)
            objs.append(o)
    return objs


PROPS = [
    # id, slot, builder, names (it, en, da), extra
    ("ribbon", "hair", ribbon, ("Fiocco", "Ribbon", "Sløjfe"), {}),
    ("hairpin", "hair", hairpin, ("Forcine", "Hairpins", "Hårnåle"), {}),
    ("flower", "hair", flower, ("Fiore", "Flower", "Blomst"), {}),
    ("cat-ears", "ears", cat_ears, ("Orecchie da gatto", "Cat ears", "Katteører"), {}),
    ("round-glasses", "eyes", round_glasses, ("Occhiali tondi", "Round glasses", "Runde briller"), {}),
    ("choker", "neck", choker, ("Girocollo", "Choker", "Choker"), {}),
    ("pendant", "neck", pendant, ("Ciondolo", "Pendant", "Vedhæng"), {}),
    ("backpack", "back", backpack, ("Zainetto", "Backpack", "Rygsæk"), {}),
    ("wings", "back", wings, ("Ali", "Wings", "Vinger"), {}),
    ("wristband", "wristL", lambda: wristband("L"), ("Polsino (sx)", "Wristband (left)", "Svedbånd (venstre)"), {}),
    ("wristband-r", "wristR", lambda: wristband("R"), ("Polsino (dx)", "Wristband (right)", "Svedbånd (højre)"), {}),
    ("headphones", "head", headphones, ("Cuffie", "Headphones", "Høretelefoner"), {}),
    ("beret", "head", beret, ("Basco", "Beret", "Baret"), {"hideTiara": True}),
    ("crown", "head", crown, ("Corona", "Crown", "Krone"), {"hideTiara": True}),
    ("halo", "head", halo, ("Aureola", "Halo", "Glorie"), {}),
]


def render_thumb(obj, path):
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    scene.cycles.device = "CPU"
    scene.cycles.samples = 24
    scene.cycles.use_denoising = False
    scene.render.resolution_x = scene.render.resolution_y = 256
    scene.render.film_transparent = True
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA"
    bb = [obj.matrix_world @ Vector(c) for c in obj.bound_box]
    lo = Vector((min(v.x for v in bb), min(v.y for v in bb), min(v.z for v in bb)))
    hi = Vector((max(v.x for v in bb), max(v.y for v in bb), max(v.z for v in bb)))
    centre = (lo + hi) / 2
    size = max((hi - lo).length, 0.02)
    cam_data = bpy.data.cameras.new("cam")
    cam_data.lens = 85
    cam = bpy.data.objects.new("cam", cam_data)
    scene.collection.objects.link(cam)
    direction = Vector((0.55, -1.0, 0.45)).normalized()  # front-right-above (Blender: -Y is the front)
    cam.location = centre + direction * size * 3.4
    cam.rotation_euler = (centre - cam.location).to_track_quat("-Z", "Y").to_euler()
    scene.camera = cam
    for name, loc, energy in (("key", (1.5, -2.0, 2.5), 300), ("fill", (-2.0, -1.0, 1.0), 120), ("rim", (0, 2.5, 2.0), 160)):
        ld = bpy.data.lights.new(name, "POINT")
        ld.energy = energy * size * size * 4
        lo_ = bpy.data.objects.new(name, ld)
        lo_.location = centre + Vector(loc) * size * 2.0
        scene.collection.objects.link(lo_)
    world = scene.world or bpy.data.worlds.new("w")
    scene.world = world
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs[1].default_value = 0.6
    scene.render.filepath = path
    bpy.ops.render.render(write_still=True)


entries = []
for pid, slot, build, (it, en, da), extra in PROPS:
    clear()
    MATS.clear()
    objs = build()
    obj = join(objs, f"Acc_{pid}")
    glb = f"acc-{pid}.glb"
    bpy.ops.object.select_all(action="DESELECT")
    obj.select_set(True)
    bpy.ops.export_scene.gltf(filepath=os.path.join(OUT, glb), export_format="GLB", use_selection=True,
                              export_apply=True, export_yup=True, export_texcoords=False, export_normals=True,
                              export_materials="EXPORT")
    render_thumb(obj, os.path.join(OUT, f"acc-{pid}.png"))
    entry = {"id": pid, "slot": slot, "glb": glb, "thumbnail": f"acc-{pid}.png", "name": {"it": it, "en": en, "da": da},
             "credits": "Original prop modelled for Secretary (tools/avatar/accessories.py).", "licence": "Same as the Secretary repository"}
    entry.update(extra)
    entries.append(entry)
    print("wrote", glb)

with open(os.path.join(OUT, "accessories.json"), "w") as f:
    json.dump(entries, f, indent=1, ensure_ascii=False)
print("done", len(entries))
