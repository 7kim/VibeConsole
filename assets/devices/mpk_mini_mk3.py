"""Build the MPK Mini MK3 model for VibeConsole and export it as glTF.

Run: flatpak run org.blender.Blender -b --factory-startup -P assets/devices/mpk_mini_mk3.py -- <out_dir>

Every control is its own node, named as docs/v7/spec/3d-device-models.md lists, with its
origin on the real pivot, so the app can press, tilt and turn it. No brand logos.
Layout from a top-view reference of the MK3; sizes in millimetres (body 318 x 181 mm).
"""
import math
import os
import sys

import bmesh
import bpy

MM = 0.001
OUT = sys.argv[sys.argv.index("--") + 1] if "--" in sys.argv else os.path.dirname(__file__)


# ---------------------------------------------------------------- materials
def material(name, color, rough, metal=0.0, emit=None, strength=0.0, coat=0.0):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    p = m.node_tree.nodes["Principled BSDF"]
    p.inputs["Base Color"].default_value = (*color, 1)
    p.inputs["Roughness"].default_value = rough
    p.inputs["Metallic"].default_value = metal
    if coat:
        p.inputs["Coat Weight"].default_value = coat
    if emit:
        p.inputs["Emission Color"].default_value = (*emit, 1)
        p.inputs["Emission Strength"].default_value = strength
    return m


def srgb(h):
    """Hex sRGB -> linear RGB tuple."""
    c = [int(h[i : i + 2], 16) / 255 for i in (0, 2, 4)]
    return tuple(x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4 for x in c)


M = {
    "body": material("body", srgb("1c1c1f"), 0.55),
    "cheek": material("cheek", srgb("c4161c"), 0.38, coat=0.2),
    "white": material("key_white", srgb("ecebe8"), 0.32, coat=0.3),
    "black": material("key_black", srgb("0b0b0c"), 0.45),
    "pad": material("pad_rubber", srgb("232326"), 0.85),
    "knob": material("knob", srgb("141416"), 0.42),
    "knob_line": material("knob_line", srgb("e6e6e6"), 0.4),
    "stick": material("joystick_red", srgb("d81f26"), 0.6),
    "button": material("button_rubber", srgb("26262a"), 0.7),
    "print": material("print_white", srgb("d8d8d8"), 0.5),
    "print_red": material("print_red", srgb("ff3b30"), 0.5, emit=srgb("ff3b30"), strength=2.0),
    "screen": material("display", srgb("050506"), 0.15, emit=srgb("9ad7ff"), strength=0.0),
}


# ---------------------------------------------------------------- mesh helpers
def new_obj(name, bm, mat, pivot, parent=None):
    me = bpy.data.meshes.new(name)
    bm.to_mesh(me)
    bm.free()
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    ob.location = [v * MM for v in pivot]
    if isinstance(mat, list):
        for m in mat:
            me.materials.append(m)
    else:
        me.materials.append(mat)
    if parent:
        ob.parent = parent
        ob.location = [(a - b) * MM for a, b in zip(pivot, parent["pivot_mm"])]
    ob["pivot_mm"] = pivot
    for poly in me.polygons:
        poly.use_smooth = True
    return ob


def bevel(ob, width_mm, segments=4):
    mod = ob.modifiers.new("bevel", "BEVEL")
    mod.width = width_mm * MM
    mod.segments = segments
    mod.limit_method = "ANGLE"
    mod.harden_normals = True
    return ob


def box(name, size, center, mat, pivot=None, bev=0.6, parent=None):
    """size/center in mm; the object's origin sits at `pivot` (default: center)."""
    pivot = pivot or center
    bm = bmesh.new()
    bmesh.ops.create_cube(bm, size=1.0)
    for v in bm.verts:
        v.co.x = v.co.x * size[0] * MM + (center[0] - pivot[0]) * MM
        v.co.y = v.co.y * size[1] * MM + (center[1] - pivot[1]) * MM
        v.co.z = v.co.z * size[2] * MM + (center[2] - pivot[2]) * MM
    ob = new_obj(name, bm, mat, pivot, parent)
    return bevel(ob, bev) if bev else ob


def cylinder(name, r, h, center_base, mat, r_top=None, segs=64, knurl=0, pivot=None, bev=0.5, parent=None):
    pivot = pivot or center_base
    bm = bmesh.new()
    bmesh.ops.create_cone(bm, cap_ends=True, segments=segs, radius1=r * MM, radius2=(r_top or r) * MM, depth=h * MM)
    for v in bm.verts:
        if knurl:
            a = math.atan2(v.co.y, v.co.x)
            if round(a / (2 * math.pi / segs)) % 2:
                v.co.x *= knurl
                v.co.y *= knurl
        v.co.x += (center_base[0] - pivot[0]) * MM
        v.co.y += (center_base[1] - pivot[1]) * MM
        v.co.z += (center_base[2] - pivot[2] + h / 2) * MM
    ob = new_obj(name, bm, mat, pivot, parent)
    return bevel(ob, bev) if bev else ob


def label(text, at, size, mat, parent=None, rot=0.0):
    """Printed text, converted to a thin mesh. `at` is the text centre in mm."""
    cu = bpy.data.curves.new("txt", "FONT")
    cu.body = text
    cu.size = size * MM
    cu.align_x = "CENTER"
    cu.align_y = "CENTER"
    cu.extrude = 0.05 * MM
    ob = bpy.data.objects.new("txt", cu)
    bpy.context.collection.objects.link(ob)
    ob.location = [v * MM for v in at]
    ob.rotation_euler.z = rot
    bpy.context.view_layer.objects.active = ob
    for o in bpy.context.view_layer.objects:
        o.select_set(o == ob)
    bpy.ops.object.convert(target="MESH")
    ob = bpy.context.view_layer.objects.active
    ob.data.materials.clear()
    ob.data.materials.append(mat)
    ob.name = "print_" + text.lower().replace(" ", "_").replace("/", "_")
    if parent:
        world = ob.matrix_world.copy()
        ob.parent = parent
        ob.matrix_world = world
    return ob


# ---------------------------------------------------------------- the device
def build():
    W, D, H = 318, 181, 26  # body without cheeks
    top = H
    body = box("body", (W, D, H), (0, 0, H / 2), M["body"], pivot=(0, 0, 0), bev=1.6)
    for side in (-1, 1):
        box(f"cheek_{'left' if side < 0 else 'right'}", (7, D + 2, H + 4), (side * (W / 2 + 3.5), 0, (H + 4) / 2), M["cheek"], bev=1.8)

    # Keybed: 25 keys, C to C, lowest on the left. Hinge at the back of each key.
    whites, blacks = 15, (0, 1, 3, 4, 5)  # black keys after C, D, F, G, A
    ww, gap = 19.6, 0.7
    x0 = -(whites * ww) / 2 + ww / 2
    front, back = -D / 2 + 3, -D / 2 + 3 + 98
    box("keybed_well", (whites * ww + 4, back - front + 4, 4), (0, (front + back) / 2, top - 2), M["black"], bev=0.4)
    n = 0
    wi = 0
    for octave in range(3):
        for step in range(12 if octave < 2 else 1):
            black = step in (1, 3, 6, 8, 10)
            if not black:
                cx = x0 + wi * ww
                box(f"key_{n:02d}", (ww - gap, back - front, 10), (cx, (front + back) / 2, top - 3 + 5), M["white"], pivot=(cx, back, top - 3), bev=0.9)
                wi += 1
            else:
                cx = x0 + (wi - 0.5) * ww
                bl = 58
                box(f"key_{n:02d}", (11, bl, 9), (cx, back - bl / 2, top + 2 + 4.5), M["black"], pivot=(cx, back, top + 2), bev=0.9)
            n += 1

    # Printed function legend strip above the keys.
    legend = ["1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/32T", "UP", "DOWN", "EXCL", "INCL", "ORDER", "RAND", "LATCH"]
    for i, t in enumerate(legend):
        label(t, (x0 + i * ww, back + 5, top + 0.01), 2.4, M["print"])

    panel_y = back + 12  # controls start here

    # Joystick (pitch/mod): pivot at the top of its base, tilts on two axes.
    jx, jy = -136, panel_y + 50
    cylinder("joystick_base", 13, 2.5, (jx, jy, top), M["button"], r_top=11, bev=0.8)
    stick = cylinder("joystick", 3, 7, (jx, jy, top + 2.5), M["knob"], pivot=(jx, jy, top + 2.5), bev=0.3)
    cap = cylinder("joystick_cap", 10, 7, (jx, jy, top + 9), M["stick"], r_top=9, segs=64, bev=2.0, parent=stick)
    cap.name = "joystick_cap"

    # Arpeggiator / octave / pad mode buttons: 2 columns x 3 rows, red printed labels.
    btns = [
        ("arp_on_off", "ON/OFF"), ("tap_tempo", "TAP TEMPO"),
        ("octave_down", "OCT -"), ("octave_up", "OCT +"),
        ("full_level", "FULL LEVEL"), ("note_repeat", "NOTE REPEAT"),
    ]
    for i, (key, text) in enumerate(btns):
        bx = -148 + (i % 2) * 25
        by = panel_y + 28 - (i // 2) * 13
        b = box(f"button_{key}", (21, 9, 3), (bx, by, top + 1.5), M["button"], pivot=(bx, by, top), bev=1.0)
        label(text, (bx, by, top + 3.02), 1.9, M["print_red"], parent=b)
    label("ARPEGGIATOR", (-135.5, panel_y + 36, top + 0.01), 2.2, M["print"])

    # Eight pads: PAD 5-8 back row, PAD 1-4 front row. Each presses down; glow under each.
    ps, pg = 28, 3.6
    px0 = -96 + ps / 2
    for row, base in ((1, 5), (0, 1)):
        for col in range(4):
            num = base + col
            cx, cy = px0 + col * (ps + pg), panel_y + 14 + row * (ps + 6)
            glow_mat = material(f"pad_{num}_light", srgb("ff2d2d"), 0.5, emit=srgb("ff2020"), strength=6.0)
            box(f"pad_{num}_glow", (ps + 1.6, ps + 1.6, 1.4), (cx, cy, top + 0.7), glow_mat, bev=0.6)
            box(f"pad_{num}", (ps, ps, 5), (cx, cy, top + 2.5), M["pad"], pivot=(cx, cy, top), bev=1.4)
            label(f"PAD {num}", (cx - ps / 2 + 4, cy + ps / 2 + 2.2, top + 0.01), 1.9, M["print"])

    # Display and the four pad-control buttons.
    sx, sy = 46, panel_y + 64
    box("display_bezel", (30, 13, 1.2), (sx, sy, top + 0.6), M["button"], bev=0.5)
    box("display", (26, 10, 0.4), (sx, sy, top + 1.3), M["screen"], bev=0)
    label("PAD CONTROLS", (98, panel_y + 72, top + 0.01), 2.0, M["print"])
    for i, (key, text) in enumerate([("bank_a_b", "BANK A/B"), ("cc", "CC"), ("prog_change", "PROG CHANGE"), ("prog_select", "PROG SELECT")]):
        bx = 71 + i * 18.5
        b = box(f"button_{key}", (15, 8, 3), (bx, panel_y + 64, top + 1.5), M["button"], pivot=(bx, panel_y + 64, top), bev=1.0)
        label(text, (bx, panel_y + 64, top + 3.02), 1.5, M["print_red"], parent=b)

    # Eight knurled knobs, K1-K4 back row, K5-K8 front row. Each turns about its own axis.
    for i in range(8):
        kx = 43 + (i % 4) * 28
        ky = panel_y + 41 - (i // 4) * 27
        k = cylinder(f"knob_{i + 1}", 9.5, 13, (kx, ky, top), M["knob"], r_top=9.0, segs=72, knurl=0.94, pivot=(kx, ky, top), bev=0.6)
        cylinder(f"knob_{i + 1}_cap", 8.2, 1.2, (kx, ky, top + 13), M["knob"], segs=64, bev=0.5, parent=k)
        box(f"knob_{i + 1}_line", (1.0, 6, 0.3), (kx, ky + 4, top + 14.3), M["knob_line"], bev=0, parent=k)
        label(f"K{i + 1}", (kx - 11, ky + 9, top + 0.01), 2.0, M["print"])
    return body


# ---------------------------------------------------------------- scene, renders, export
def studio(target_obj):
    sc = bpy.context.scene
    sc.render.engine = "CYCLES"
    sc.cycles.samples = 96
    sc.cycles.use_denoising = True
    sc.render.film_transparent = True
    sc.render.resolution_x, sc.render.resolution_y = 1600, 1000
    sc.view_settings.view_transform = "AgX"
    world = bpy.data.worlds.new("world")
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs["Color"].default_value = (0.02, 0.02, 0.022, 1)
    sc.world = world
    for name, loc, power, size in (("key", (-0.35, -0.45, 0.6), 9, 0.6), ("fill", (0.45, -0.3, 0.35), 3, 0.8), ("rim", (0.0, 0.5, 0.45), 7, 0.5)):
        light = bpy.data.lights.new(name, "AREA")
        light.energy, light.size = power, size
        ob = bpy.data.objects.new(name, light)
        bpy.context.collection.objects.link(ob)
        ob.location = loc
        con = ob.constraints.new("TRACK_TO")
        con.target = target_obj
    cam = bpy.data.objects.new("camera", bpy.data.cameras.new("camera"))
    bpy.context.collection.objects.link(cam)
    cam.data.lens = 70
    con = cam.constraints.new("TRACK_TO")
    con.target = target_obj
    sc.camera = cam
    return cam


def main():
    for o in list(bpy.data.objects):
        bpy.data.objects.remove(o)
    body = build()
    os.makedirs(OUT, exist_ok=True)
    bpy.ops.export_scene.gltf(filepath=os.path.join(OUT, "mpk-mini-mk3.glb"), export_format="GLB", export_apply=True, export_yup=True, use_selection=False, export_cameras=False, export_lights=False)
    cam = studio(body)
    for name, loc in (("mpk-mini-mk3-34", (0.0, -0.52, 0.55)), ("mpk-mini-mk3-top", (0.0, -0.02, 0.78))):
        cam.location = loc
        bpy.context.scene.render.filepath = os.path.join(OUT, name + ".png")
        bpy.ops.render.render(write_still=True)
    # Check render: the moves the app makes, applied to the exported pivots (not exported).
    o = bpy.data.objects
    o["pad_2"].location.z -= 2 * MM
    o["key_04"].rotation_euler.x = math.radians(-4)
    o["key_06"].rotation_euler.x = math.radians(-4)
    o["knob_3"].rotation_euler.z = math.radians(-120)
    o["joystick"].rotation_euler.y = math.radians(18)
    o["button_tap_tempo"].location.z -= 1 * MM
    cam.location = (0.0, -0.52, 0.55)
    bpy.context.scene.render.filepath = os.path.join(OUT, "mpk-mini-mk3-pressed.png")
    bpy.ops.render.render(write_still=True)


main()
