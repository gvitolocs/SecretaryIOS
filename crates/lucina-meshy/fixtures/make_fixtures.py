# Sample GLBs for the Meshy mock and the tests (Python only because Blender's API needs it).
#   blender -b --factory-startup -P make_fixtures.py -- <out_dir>
# Writes chest_preview.glb (grey), chest.glb (textured colours), knight.glb (rigged, no clips),
# knight_walk.glb (rigged + one in-place walk clip), knight_anim.glb (rigged + walk + wave clips).
# Nothing is rendered: geometry, armature, keyframes and glTF export only (CPU).
import math
import os
import sys

import bpy

out = sys.argv[sys.argv.index("--") + 1] if "--" in sys.argv else "."
os.makedirs(out, exist_ok=True)


def reset():
    bpy.ops.wm.read_factory_settings(use_empty=True)


def mat(name, rgb, metal=0.0, rough=0.6):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    b = m.node_tree.nodes["Principled BSDF"]
    b.inputs["Base Color"].default_value = (*rgb, 1.0)
    b.inputs["Metallic"].default_value = metal
    b.inputs["Roughness"].default_value = rough
    return m


def assign(obj, m):
    obj.data.materials.clear()
    obj.data.materials.append(m)


def export(path, animations=False):
    bpy.ops.export_scene.gltf(
        filepath=path,
        export_format="GLB",
        export_animations=animations,
        export_animation_mode="ACTIONS",
        export_force_sampling=True,
        export_apply=False,
    )


def chest(textured):
    reset()
    wood = mat("Wood", (0.42, 0.24, 0.11), 0.0, 0.7) if textured else mat("Clay", (0.6, 0.6, 0.62))
    gold = mat("Brass", (0.85, 0.65, 0.2), 1.0, 0.35) if textured else wood
    bpy.ops.mesh.primitive_cube_add(size=1, location=(0, 0, 0.3))
    body = bpy.context.object
    body.scale = (1.0, 0.6, 0.6)
    assign(body, wood)
    bpy.ops.mesh.primitive_cylinder_add(vertices=24, radius=0.3, depth=1.0, location=(0, 0, 0.6), rotation=(0, math.pi / 2, 0))
    lid = bpy.context.object
    lid.scale = (1.0, 1.0, 1.0)
    assign(lid, wood)
    for x in (-0.38, 0.38):
        bpy.ops.mesh.primitive_cube_add(size=1, location=(x, 0, 0.3))
        band = bpy.context.object
        band.scale = (0.06, 0.62, 0.62)
        assign(band, gold)
        bpy.ops.mesh.primitive_cylinder_add(vertices=24, radius=0.31, depth=0.06, location=(x, 0, 0.6), rotation=(0, math.pi / 2, 0))
        band = bpy.context.object
        assign(band, gold)
    bpy.ops.mesh.primitive_cube_add(size=1, location=(0, -0.31, 0.55))
    lock = bpy.context.object
    lock.scale = (0.14, 0.04, 0.18)
    assign(lock, gold)
    export(os.path.join(out, "chest.glb" if textured else "chest_preview.glb"))


def knight(clips):
    reset()
    steel = mat("Steel", (0.72, 0.74, 0.8), 0.9, 0.3)
    cloth = mat("Tabard", (0.55, 0.08, 0.1), 0.0, 0.8)
    skin = mat("Visor", (0.15, 0.15, 0.18), 0.6, 0.4)
    parts = []

    def part(kind, loc, scale, m, rot=(0, 0, 0)):
        if kind == "cube":
            bpy.ops.mesh.primitive_cube_add(size=1, location=loc, rotation=rot)
        elif kind == "cyl":
            bpy.ops.mesh.primitive_cylinder_add(vertices=16, radius=0.5, depth=1, location=loc, rotation=rot)
        else:
            bpy.ops.mesh.primitive_uv_sphere_add(segments=24, ring_count=12, radius=0.5, location=loc)
        o = bpy.context.object
        o.scale = scale
        assign(o, m)
        parts.append(o)
        return o

    part("cube", (0, 0, 1.25), (0.42, 0.24, 0.55), steel)  # torso
    part("cube", (0, -0.125, 1.15), (0.38, 0.01, 0.6), cloth)  # tabard
    part("sphere", (0, 0, 1.7), (0.26, 0.28, 0.3), steel)  # helmet
    part("cube", (0, -0.13, 1.7), (0.16, 0.02, 0.05), skin)  # visor slit
    for s in (-1, 1):
        part("cyl", (s * 0.12, 0, 0.72), (0.15, 0.15, 0.5), steel)  # upper leg
        part("cyl", (s * 0.12, 0, 0.27), (0.13, 0.13, 0.46), steel)  # lower leg
        part("cube", (s * 0.12, -0.05, 0.03), (0.14, 0.26, 0.06), skin)  # boot
        part("cyl", (s * 0.29, 0, 1.33), (0.11, 0.11, 0.36), steel)  # upper arm
        part("cyl", (s * 0.29, 0, 1.0), (0.1, 0.1, 0.34), steel)  # forearm
    bpy.ops.object.select_all(action="DESELECT")
    for p in parts:
        p.select_set(True)
    bpy.context.view_layer.objects.active = parts[0]
    bpy.ops.object.join()
    body = bpy.context.object
    body.name = "Knight"
    bpy.ops.object.transform_apply(location=False, rotation=True, scale=True)

    bpy.ops.object.armature_add(location=(0, 0, 0))
    arm = bpy.context.object
    arm.name = "Armature"
    bpy.ops.object.mode_set(mode="EDIT")
    eb = arm.data.edit_bones
    root = eb[0]
    root.name = "Hips"
    root.head, root.tail = (0, 0, 0.95), (0, 0, 1.15)
    spine = eb.new("Spine")
    spine.head, spine.tail, spine.parent = (0, 0, 1.15), (0, 0, 1.55), root
    head = eb.new("Head")
    head.head, head.tail, head.parent = (0, 0, 1.55), (0, 0, 1.9), spine
    for side, s in (("L", 1), ("R", -1)):
        ul = eb.new(f"UpperLeg.{side}")
        ul.head, ul.tail, ul.parent = (s * 0.12, 0, 0.95), (s * 0.12, 0, 0.5), root
        ll = eb.new(f"LowerLeg.{side}")
        ll.head, ll.tail, ll.parent = (s * 0.12, 0, 0.5), (s * 0.12, 0, 0.05), ul
        ua = eb.new(f"UpperArm.{side}")
        ua.head, ua.tail, ua.parent = (s * 0.29, 0, 1.5), (s * 0.29, 0, 1.17), spine
        la = eb.new(f"LowerArm.{side}")
        la.head, la.tail, la.parent = (s * 0.29, 0, 1.17), (s * 0.29, 0, 0.83), ua
    bpy.ops.object.mode_set(mode="OBJECT")
    bpy.ops.object.select_all(action="DESELECT")
    body.select_set(True)
    arm.select_set(True)
    bpy.context.view_layer.objects.active = arm
    bpy.ops.object.parent_set(type="ARMATURE_AUTO")

    def key(action_name, frames):
        arm.animation_data_create()
        act = bpy.data.actions.new(action_name)
        arm.animation_data.action = act
        bpy.ops.object.mode_set(mode="POSE")
        for f, pose in frames:
            for bone, (x, y, z) in pose.items():
                pb = arm.pose.bones[bone]
                pb.rotation_mode = "XYZ"
                pb.rotation_euler = (math.radians(x), math.radians(y), math.radians(z))
                pb.keyframe_insert("rotation_euler", frame=f)
        bpy.ops.object.mode_set(mode="OBJECT")
        arm.animation_data.action = None
        act.use_fake_user = True
        track = arm.animation_data.nla_tracks.new()
        track.name = action_name
        track.strips.new(action_name, 1, act)
        return act

    if "walk" in clips:
        w = []
        for i, f in enumerate((1, 9, 17, 25, 33)):
            a = 28 * math.sin(i * math.pi / 2)
            w.append((f, {
                "UpperLeg.L": (a, 0, 0), "UpperLeg.R": (-a, 0, 0),
                "LowerLeg.L": (max(0, -a) * 0.9, 0, 0), "LowerLeg.R": (max(0, a) * 0.9, 0, 0),
                "UpperArm.L": (-a * 0.8, 0, 0), "UpperArm.R": (a * 0.8, 0, 0),
                "Spine": (0, 4 * math.sin(i * math.pi / 2), 0),
            }))
        key("Casual_Walk_inplace", w)
    if "wave" in clips:
        v = []
        for i, f in enumerate((1, 7, 13, 19, 25, 31, 37)):
            v.append((f, {
                "UpperArm.R": (0, 0, -150 if 0 < i < 6 else 0),
                "LowerArm.R": (0, 0, (25 * math.sin(i * math.pi / 1.5)) if 0 < i < 6 else 0),
                "Head": (0, 0, 8 * math.sin(i * math.pi / 3)),
            }))
        key("Wave_One_Hand", v)
    bpy.context.scene.frame_start = 1
    bpy.context.scene.frame_end = 37
    return bool(clips)


chest(False)
chest(True)
knight([])
export(os.path.join(out, "knight.glb"))
knight(["walk"])
export(os.path.join(out, "knight_walk.glb"), animations=True)
knight(["walk", "wave"])
export(os.path.join(out, "knight_anim.glb"), animations=True)
print("fixtures written to", out)


def export_formats(stem):
    base = os.path.join(out, stem)
    bpy.ops.export_scene.fbx(filepath=base + ".fbx")
    bpy.ops.wm.obj_export(filepath=base + ".obj")
    bpy.ops.wm.stl_export(filepath=base + ".stl")
    bpy.ops.wm.usd_export(filepath=base + ".usdz")


chest(True)
export_formats("chest")
knight([])
export_formats("knight")
print("formats written to", out)
