"""
Lucina studio props: ten simple, original, toon-friendly props built from primitives.

Run headless on the CPU (never the GPU):
  CUDA_VISIBLE_DEVICES= nice blender --background --factory-startup -noaudio \
      --python tools/props/make_props.py -- --out assets/props [--preview]

Writes <out>/lucina-props.glb (one top-level node per prop, named after its id, origin at the
centre of its base, front facing glTF +Z) and, with --preview, <out>/props-preview.png
(Cycles on the CPU, low samples). Flat colours only: the stage swaps the materials for toon
ones with an outline. Units are metres, sized for a 1.65 m character.

Python only because Blender's API is Python (see the repo's Rust-first rule).
"""
import math
import sys

import bmesh
import bpy
from mathutils import Vector

argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
OUT = argv[argv.index("--out") + 1] if "--out" in argv else "assets/props"
PREVIEW = "--preview" in argv

# ---------- scene ----------
bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene
scene.unit_settings.system = "METRIC"

_mats = {}


def mat(name, rgb, rough=0.8, emit=0.0):
    if name in _mats:
        return _mats[name]
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    b = m.node_tree.nodes.get("Principled BSDF")
    b.inputs["Base Color"].default_value = (*rgb, 1)
    b.inputs["Roughness"].default_value = rough
    b.inputs["Metallic"].default_value = 0.0
    if emit > 0:
        b.inputs["Emission Color"].default_value = (*rgb, 1)
        b.inputs["Emission Strength"].default_value = emit
    _mats[name] = m
    return m


def srgb(h):
    """#rrggbb -> linear RGB (Blender colours are linear)."""
    h = h.lstrip("#")
    c = [int(h[i:i + 2], 16) / 255 for i in (0, 2, 4)]
    return tuple(x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4 for x in c)


def finish(obj, material, smooth=True):
    obj.data.materials.clear()
    obj.data.materials.append(material)
    bpy.context.view_layer.objects.active = obj
    obj.select_set(True)
    bpy.ops.object.transform_apply(location=False, rotation=True, scale=True)
    if smooth:
        bpy.ops.object.shade_smooth_by_angle(angle=math.radians(40))
    else:
        bpy.ops.object.shade_flat()
    obj.select_set(False)
    return obj


def deselect():
    for o in bpy.context.selected_objects:
        o.select_set(False)


def cyl(r, h, loc, material, r2=None, verts=24, rot=(0, 0, 0), cap="NGON", smooth=True):
    deselect()
    if r2 is None:
        bpy.ops.mesh.primitive_cylinder_add(vertices=verts, radius=r, depth=h, location=loc, rotation=rot, end_fill_type=cap)
    else:
        bpy.ops.mesh.primitive_cone_add(vertices=verts, radius1=r, radius2=r2, depth=h, location=loc, rotation=rot, end_fill_type=cap)
    return finish(bpy.context.active_object, material, smooth)


def box(size, loc, material, rot=(0, 0, 0), bevel=0.0):
    deselect()
    bpy.ops.mesh.primitive_cube_add(size=1, location=loc, rotation=rot)
    o = bpy.context.active_object
    o.scale = size
    finish(o, material, smooth=False)
    if bevel > 0:
        mod = o.modifiers.new("bevel", "BEVEL")
        mod.width = bevel
        mod.segments = 2
        bpy.context.view_layer.objects.active = o
        bpy.ops.object.modifier_apply(modifier=mod.name)
        bpy.ops.object.shade_smooth_by_angle(angle=math.radians(40))
    return o


def sphere(r, loc, material, scale=(1, 1, 1), rot=(0, 0, 0), seg=24, rings=12):
    deselect()
    bpy.ops.mesh.primitive_uv_sphere_add(segments=seg, ring_count=rings, radius=r, location=loc, rotation=rot)
    o = bpy.context.active_object
    o.scale = scale
    return finish(o, material)


def torus(R, r, loc, material, rot=(0, 0, 0), half=None):
    deselect()
    bpy.ops.mesh.primitive_torus_add(major_radius=R, minor_radius=r, major_segments=32, minor_segments=12, location=(0, 0, 0))
    o = bpy.context.active_object
    if half:
        bm = bmesh.new()
        bm.from_mesh(o.data)
        axis, sign = half
        kill = [v for v in bm.verts if (v.co[axis] * sign) > 1e-5]
        bmesh.ops.delete(bm, geom=kill, context="VERTS")
        bm.to_mesh(o.data)
        bm.free()
    o.rotation_euler = rot
    o.location = loc
    bpy.context.view_layer.objects.active = o
    o.select_set(True)
    bpy.ops.object.transform_apply(location=True, rotation=True, scale=True)
    o.select_set(False)
    return finish(o, material)


def tube(points, radius, material):
    """A bevelled bezier through `points`, converted to a mesh."""
    curve = bpy.data.curves.new("tube", "CURVE")
    curve.dimensions = "3D"
    curve.bevel_depth = radius
    curve.bevel_resolution = 4
    curve.use_fill_caps = True
    sp = curve.splines.new("BEZIER")
    sp.bezier_points.add(len(points) - 1)
    for bp, p in zip(sp.bezier_points, points):
        bp.co = p
        bp.handle_left_type = bp.handle_right_type = "AUTO"
    o = bpy.data.objects.new("tube", curve)
    bpy.context.collection.objects.link(o)
    deselect()
    bpy.context.view_layer.objects.active = o
    o.select_set(True)
    bpy.ops.object.convert(target="MESH")
    o = bpy.context.active_object
    o.select_set(False)
    return finish(o, material)


def join(name, parts):
    deselect()
    for p in parts:
        p.select_set(True)
    bpy.context.view_layer.objects.active = parts[0]
    bpy.ops.object.join()
    o = bpy.context.active_object
    o.name = name
    o.data.name = name
    # Origin at the centre of the base.
    xs = [v.co.x for v in o.data.vertices]
    ys = [v.co.y for v in o.data.vertices]
    zs = [v.co.z for v in o.data.vertices]
    base = Vector(((min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2, min(zs)))
    for v in o.data.vertices:
        v.co -= base
    o.location = (0, 0, 0)
    o["prop"] = name
    o.select_set(False)
    return o


# Palette (flat, pastel, toon-friendly).
WOOD = mat("wood", srgb("#b07a4f"))
WOOD_D = mat("wood_dark", srgb("#7a5236"))
CREAM = mat("cream", srgb("#f6efe4"))
PINK = mat("pink", srgb("#f2a7c3"))
ROSE = mat("rose", srgb("#e0607e"))
SKY = mat("sky", srgb("#8fc7f0"))
NAVY = mat("navy", srgb("#2f3e6b"))
MINT = mat("mint", srgb("#9be0c0"))
LEAF = mat("leaf", srgb("#4fa86a"))
LEAF_D = mat("leaf_dark", srgb("#2f7a4a"))
TERRA = mat("terracotta", srgb("#d4734d"))
SOIL = mat("soil", srgb("#4a3426"))
COFFEE = mat("coffee", srgb("#5a3520"))
GOLD = mat("gold", srgb("#f1c75b"), rough=0.5)
GREY = mat("grey", srgb("#8a8f9c"), rough=0.6)
INK = mat("ink", srgb("#23202a"))
LILAC = mat("lilac", srgb("#c8b3f0"))
BULB = mat("bulb", srgb("#fff2c4"), emit=3.0)
WHITE = mat("white", srgb("#ffffff"))

props = []

# 1. chair: wooden seat with a pink cushion, four legs, a slatted back (back at +Y = glTF -Z).
p = []
p.append(box((0.42, 0.42, 0.04), (0, 0, 0.44), WOOD, bevel=0.008))
p.append(box((0.38, 0.38, 0.035), (0, 0, 0.475), PINK, bevel=0.012))
for x in (-0.18, 0.18):
    for y in (-0.18, 0.18):
        p.append(cyl(0.017, 0.44, (x, y, 0.22), WOOD_D, r2=0.014, verts=12))
for x in (-0.18, 0.18):
    p.append(cyl(0.016, 0.46, (x, 0.19, 0.69), WOOD_D, verts=12))
p.append(box((0.4, 0.03, 0.16), (0, 0.19, 0.84), WOOD, bevel=0.01))
p.append(box((0.36, 0.02, 0.04), (0, 0.19, 0.66), WOOD))
props.append(join("chair", p))

# 2. table: round café table on a pedestal.
p = [
    cyl(0.38, 0.035, (0, 0, 0.72), CREAM, verts=48),
    cyl(0.385, 0.012, (0, 0, 0.698), WOOD_D, verts=48),
    cyl(0.03, 0.68, (0, 0, 0.36), GREY, verts=16),
    cyl(0.22, 0.03, (0, 0, 0.02), GREY, r2=0.16, verts=32),
]
props.append(join("table", p))

# 3. cup: a mug with coffee and a handle.
p = [
    cyl(0.045, 0.095, (0, 0, 0.0475), SKY, r2=0.048, verts=32),
    cyl(0.04, 0.004, (0, 0, 0.088), COFFEE, verts=32),
    torus(0.025, 0.007, (0.05, 0, 0.05), SKY, rot=(math.pi / 2, 0, 0), half=(0, -1)),
]
props.append(join("cup", p))

# 4. book: a closed book lying flat, cover and pages.
p = [
    box((0.17, 0.235, 0.008), (0, 0, 0.004), NAVY, bevel=0.002),
    box((0.17, 0.235, 0.008), (0, 0, 0.036), NAVY, bevel=0.002),
    box((0.012, 0.235, 0.04), (-0.079, 0, 0.02), NAVY, bevel=0.003),
    box((0.158, 0.225, 0.026), (0.004, 0, 0.02), CREAM),
    box((0.05, 0.06, 0.002), (0.02, 0.03, 0.041), GOLD),
]
props.append(join("book", p))

# 5. plant: terracotta pot, soil, a crown of leaves.
p = [
    cyl(0.11, 0.24, (0, 0, 0.12), TERRA, r2=0.15, verts=32),
    torus(0.15, 0.014, (0, 0, 0.24), TERRA),
    cyl(0.14, 0.01, (0, 0, 0.23), SOIL, verts=32),
    cyl(0.01, 0.16, (0, 0, 0.3), LEAF_D, verts=8),
]
for i in range(8):
    a = i * math.tau / 8 + 0.2
    tilt = 0.55 + 0.25 * (i % 2)
    h = 0.34 + 0.05 * (i % 3)
    r = 0.07
    p.append(sphere(0.12, (math.cos(a) * r, math.sin(a) * r, h), LEAF if i % 2 else LEAF_D,
                    scale=(0.32, 0.12, 1.0), rot=(0, tilt, a), seg=16, rings=8))
props.append(join("plant", p))

# 6. lamp: a floor lamp with a glowing bulb.
p = [
    cyl(0.15, 0.03, (0, 0, 0.015), GOLD, r2=0.13, verts=32),
    cyl(0.012, 1.4, (0, 0, 0.73), GOLD, verts=12),
    cyl(0.2, 0.26, (0, 0, 1.42), CREAM, r2=0.11, verts=32, cap="NOTHING"),
    sphere(0.045, (0, 0, 1.38), BULB),
]
props.append(join("lamp", p))

# 7. cushion: a soft square cushion with a button.
deselect()
bpy.ops.mesh.primitive_cube_add(size=1, location=(0, 0, 0.07))
c = bpy.context.active_object
c.scale = (0.42, 0.42, 0.14)
bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
sub = c.modifiers.new("soft", "SUBSURF")
sub.levels = 3
bpy.ops.object.modifier_apply(modifier=sub.name)
finish(c, LILAC)
props.append(join("cushion", [c, cyl(0.02, 0.012, (0, 0, 0.132), PINK, verts=16)]))

# 8. umbrella: an open umbrella standing on its handle.
p = [
    cyl(0.48, 0.2, (0, 0, 0.98), ROSE, r2=0.015, verts=8, cap="NOTHING", smooth=False),
    cyl(0.009, 0.95, (0, 0, 0.6), INK, verts=8),
    sphere(0.02, (0, 0, 1.09), INK),
    torus(0.04, 0.012, (0.04, 0, 0.13), WOOD_D, rot=(math.pi / 2, 0, 0), half=(1, 1)),
]
for i in range(8):
    a = i * math.tau / 8 + math.pi / 8
    p.append(sphere(0.012, (math.cos(a) * 0.45, math.sin(a) * 0.45, 0.875), INK, seg=8, rings=6))
props.append(join("umbrella", p))

# 9. guitar: an acoustic guitar standing upright (front at -Y = glTF +Z).
p = [
    cyl(0.19, 0.1, (0, 0, 0.19), WOOD, verts=40, rot=(math.pi / 2, 0, 0)),
    cyl(0.15, 0.1, (0, 0, 0.42), WOOD, verts=40, rot=(math.pi / 2, 0, 0)),
    box((0.2, 0.1, 0.12), (0, 0, 0.3), WOOD),
    cyl(0.055, 0.004, (0, -0.051, 0.36), INK, verts=32, rot=(math.pi / 2, 0, 0)),
    cyl(0.065, 0.002, (0, -0.0505, 0.36), CREAM, verts=32, rot=(math.pi / 2, 0, 0)),
    box((0.11, 0.012, 0.025), (0, -0.054, 0.17), WOOD_D),
    box((0.05, 0.025, 0.44), (0, -0.02, 0.78), WOOD_D),
    box((0.07, 0.025, 0.14), (0, -0.015, 1.06), INK, bevel=0.006),
]
for i in range(3):
    for s in (-1, 1):
        p.append(cyl(0.007, 0.03, (s * 0.045, -0.015, 1.02 + i * 0.04), GOLD, verts=8, rot=(0, math.pi / 2, 0)))
props.append(join("guitar", p))

# 10. cat plush: a sitting cat (front at -Y), ears, tail, face.
p = [
    sphere(0.12, (0, 0, 0.11), CREAM, scale=(1.0, 0.9, 0.95)),
    sphere(0.1, (0, -0.02, 0.29), CREAM, scale=(1.08, 0.95, 0.92)),
    cyl(0.04, 0.07, (-0.06, -0.01, 0.385), CREAM, r2=0.004, verts=16, rot=(0, -0.3, 0)),
    cyl(0.04, 0.07, (0.06, -0.01, 0.385), CREAM, r2=0.004, verts=16, rot=(0, 0.3, 0)),
    cyl(0.022, 0.04, (-0.058, -0.03, 0.38), PINK, r2=0.003, verts=12, rot=(0, -0.3, 0)),
    cyl(0.022, 0.04, (0.058, -0.03, 0.38), PINK, r2=0.003, verts=12, rot=(0, 0.3, 0)),
    sphere(0.014, (-0.035, -0.108, 0.3), INK, scale=(1, 0.6, 1.3)),
    sphere(0.014, (0.035, -0.108, 0.3), INK, scale=(1, 0.6, 1.3)),
    sphere(0.01, (0, -0.115, 0.272), PINK, scale=(1.3, 0.8, 0.9)),
    sphere(0.035, (-0.055, -0.09, 0.03), CREAM, scale=(1, 1.3, 0.8)),
    sphere(0.035, (0.055, -0.09, 0.03), CREAM, scale=(1, 1.3, 0.8)),
    tube([(0.08, 0.08, 0.03), (0.17, 0.06, 0.08), (0.15, 0.02, 0.2), (0.12, 0.0, 0.25)], 0.022, CREAM),
    torus(0.085, 0.01, (0, -0.02, 0.205), ROSE, rot=(0.1, 0, 0)),
    sphere(0.016, (0, -0.105, 0.19), GOLD),
]
props.append(join("cat_plush", p))

# ---------- export ----------
import os

os.makedirs(OUT, exist_ok=True)
for o in bpy.data.objects:
    o.select_set(o in props)
bpy.ops.export_scene.gltf(
    filepath=os.path.join(OUT, "lucina-props.glb"),
    export_format="GLB",
    use_selection=True,
    export_apply=True,
    export_extras=True,
    export_yup=True,
    export_materials="EXPORT",
    export_texcoords=False,
    export_normals=True,
    export_animations=False,
)
print("[props] exported", [o.name for o in props])

if PREVIEW:
    # Contact sheet: Cycles on the CPU only, low samples.
    try:
        bpy.context.preferences.addons["cycles"].preferences.compute_device_type = "NONE"
    except Exception:
        pass
    scene.render.engine = "CYCLES"
    scene.cycles.device = "CPU"
    scene.cycles.samples = 24
    scene.cycles.use_denoising = False
    scene.render.resolution_x = 1800
    scene.render.resolution_y = 520
    scene.render.film_transparent = False
    x = 0.0
    gaps = {"chair": 0.55, "table": 0.85, "cup": 0.25, "book": 0.3, "plant": 0.45, "lamp": 0.5,
            "cushion": 0.55, "umbrella": 1.05, "guitar": 0.5, "cat_plush": 0.4}
    for o in props:
        w = gaps[o.name]
        o.location = (x + w / 2, 0, 0)
        x += w + 0.08
    deselect()
    bpy.ops.mesh.primitive_plane_add(size=40, location=(x / 2, 0, 0))
    floor = bpy.context.active_object
    floor.data.materials.append(mat("floor", srgb("#ecebf3"), rough=1))
    cam_data = bpy.data.cameras.new("cam")
    cam_data.type = "ORTHO"
    cam_data.ortho_scale = x + 0.3
    cam = bpy.data.objects.new("cam", cam_data)
    scene.collection.objects.link(cam)
    cam.location = (x / 2, -6, 1.55)
    cam.rotation_euler = (math.radians(84), 0, 0)
    scene.camera = cam
    sun = bpy.data.lights.new("sun", "SUN")
    sun.energy = 3.5
    so = bpy.data.objects.new("sun", sun)
    scene.collection.objects.link(so)
    so.rotation_euler = (math.radians(50), math.radians(10), math.radians(-30))
    world = bpy.data.worlds.new("w")
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs["Color"].default_value = (0.9, 0.9, 0.95, 1)
    world.node_tree.nodes["Background"].inputs["Strength"].default_value = 0.8
    scene.world = world
    scene.render.filepath = os.path.join(OUT, "props-preview.png")
    bpy.ops.render.render(write_still=True)
    print("[props] preview", scene.render.filepath)
