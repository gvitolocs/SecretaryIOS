"""Renders a few frames of the exported USDZ (re-imported) for a visual check. Blender script.

    blender -b --factory-startup --python tools/lucina-usdz/render_check.py -- <lucina.usdz> <out_dir> <frame>...
"""
import math
import os
import sys

import bpy

argv = sys.argv[sys.argv.index("--") + 1 :]
src, out_dir, frames = argv[0], argv[1], [int(f) for f in argv[2:]] or [60]
bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.wm.usd_import(filepath=src)
scene = bpy.context.scene
scene.render.engine = "BLENDER_WORKBENCH"
scene.display.shading.light = "FLAT"
scene.display.shading.color_type = "TEXTURE"
scene.display.shading.show_backface_culling = True
scene.render.resolution_x, scene.render.resolution_y = 600, 900
scene.render.film_transparent = False
world = bpy.data.worlds.new("w")
scene.world = world
cam_data = bpy.data.cameras.new("cam")
cam_data.lens = 50
for name, loc, rot in (
    ("front", (0, -4.2, 0.85), (math.radians(90), 0, 0)),
    ("side", (4.2, 0, 0.85), (math.radians(90), 0, math.radians(90))),
):
    cam = bpy.data.objects.new(name, cam_data)
    cam.location, cam.rotation_euler = loc, rot
    scene.collection.objects.link(cam)
    scene.camera = cam
    for f in frames:
        scene.frame_set(f)
        scene.render.filepath = os.path.join(out_dir, f"{name}_{f:04d}.png")
        bpy.ops.render.render(write_still=True)
