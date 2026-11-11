"""Build the Xbox Series X|S controller (Robot White) model for VibeConsole and export glTF.

Run: flatpak run org.blender.Blender -b --factory-startup -P assets/devices/xbox_series_white.py -- <out_dir>

Body from a subdivided cage (smooth, organic shell) with a parting-line seam and a dotted
grip texture on the back, triggers and bumpers. Every control is a separate node with its
origin on the real pivot, named as docs/v7/spec/3d-device-models.md lists. No logos: the
Xbox button is a plain lit disc. Sizes in millimetres (about 153 x 104 x 61 mm).
"""
import math
import os
import sys

import bmesh
import bpy
from mathutils import Vector
from mathutils.bvhtree import BVHTree

MM = 0.001
OUT = sys.argv[sys.argv.index("--") + 1] if "--" in sys.argv else os.path.dirname(__file__)


def srgb(h):
    c = [int(h[i : i + 2], 16) / 255 for i in (0, 2, 4)]
    return tuple(x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4 for x in c)


def material(name, color, rough, emit=None, strength=0.0, coat=0.0):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    p = m.node_tree.nodes["Principled BSDF"]
    p.inputs["Base Color"].default_value = (*srgb(color), 1)
    p.inputs["Roughness"].default_value = rough
    if coat:
        p.inputs["Coat Weight"].default_value = coat
    if emit:
        p.inputs["Emission Color"].default_value = (*srgb(emit), 1)
        p.inputs["Emission Strength"].default_value = strength
    return m


def dot_normals(px=512, dots=16):
    """Tileable normal map of small raised dots: the controller's grip texture."""
    import numpy as np

    u = (np.arange(px) + 0.5) / px * dots
    x, y = np.meshgrid(u, u)
    h = np.zeros((px, px))
    for o in (0.0, 0.5):  # two offset grids: a staggered dot field
        r = np.hypot((x + o) % 1 - 0.5, (y + o) % 1 - 0.5) / 0.24
        h = np.maximum(h, np.sqrt(np.clip(1 - r * r, 0, None)))
    k = px / dots * 0.18  # bump steepness
    gx = (np.roll(h, -1, 1) - np.roll(h, 1, 1)) / 2 * k  # wraps, so the tile repeats cleanly
    gy = (np.roll(h, -1, 0) - np.roll(h, 1, 0)) / 2 * k
    n = np.dstack((-gx, -gy, np.ones_like(h)))
    n /= np.linalg.norm(n, axis=2, keepdims=True)
    img = bpy.data.images.new("grip_dots", px, px)
    img.colorspace_settings.name = "Non-Color"
    img.pixels.foreach_set(np.dstack((n * 0.5 + 0.5, np.ones_like(h))).astype(np.float32).ravel())
    img.pack()
    return img


DOTS = dot_normals()
TILE = 16  # mm per texture repeat: 1 mm dot pitch


def dotted(name, color, rough):
    """Material with the grip dots as its normal map (exported as the glTF normalTexture)."""
    m = material(name, color, rough)
    nt = m.node_tree
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = DOTS
    nm = nt.nodes.new("ShaderNodeNormalMap")
    nt.links.new(tex.outputs["Color"], nm.inputs["Color"])
    nt.links.new(nm.outputs["Normal"], nt.nodes["Principled BSDF"].inputs["Normal"])
    return m


def box_uv(ob):
    """Cube-projected UVs, TILE mm per repeat, so the dots keep one size on every part."""
    me = ob.data
    uv = me.uv_layers.new(name="UVMap")
    for poly in me.polygons:
        a, b = [i for i in range(3) if i != max(range(3), key=lambda i: abs(poly.normal[i]))]
        for li in poly.loop_indices:
            co = me.vertices[me.loops[li].vertex_index].co
            uv.data[li].uv = (co[a] / (TILE * MM), co[b] / (TILE * MM))
    return ob


M = {
    "shell": material("shell_white", "eceeeb", 0.5),
    "grip": material("grip_dark", "1d1e20", 0.75),
    "grip_dots": dotted("grip_dark_dotted", "1d1e20", 0.8),
    "shell_dots": dotted("shell_white_dotted", "eceeeb", 0.6),
    "seam": material("seam", "0b0b0c", 0.7),
    "black": material("stick_black", "18181a", 0.6),
    "face": material("face_button", "151517", 0.25, coat=0.6),
    "white_btn": material("button_white", "dfe1df", 0.35),
    "xbox": material("button_xbox_light", "f4f5f2", 0.3, emit="ffffff", strength=0.6),
    "port": material("port", "050505", 0.6),
    "A": material("letter_a", "46c23a", 0.4, emit="46c23a", strength=0.4),
    "B": material("letter_b", "e3342f", 0.4, emit="e3342f", strength=0.4),
    "X": material("letter_x", "2f8de4", 0.4, emit="2f8de4", strength=0.4),
    "Y": material("letter_y", "f2c230", 0.4, emit="f2c230", strength=0.4),
}


# ---------------------------------------------------------------- body
# Top-view outline of the right half (mm), from the top centre clockwise to the bottom centre.
HALF = [(0, 41), (18, 42), (36, 45), (54, 43), (67, 36), (75, 23), (78, 8), (77, -8), (74, -24),
        (69, -40), (62, -53), (53, -61), (44, -62), (37, -55), (31, -42), (25, -29), (16, -22), (0, -20)]
OUTLINE = HALF[:-1] + [(-x, y) for x, y in reversed(HALF[1:])]
grip = lambda y: max(0.0, -y - 15) / 47  # 0 at the centre, 1 at the grip ends


def smoothed(points, passes):
    """Laplacian-smoothed closed outline: removes the sharp notch so insets can't cross."""
    n = len(points)
    for _ in range(passes):
        points = [((points[i - 1][0] + 2 * points[i][0] + points[(i + 1) % n][0]) / 4,
                   (points[i - 1][1] + 2 * points[i][1] + points[(i + 1) % n][1]) / 4) for i in range(n)]
    return points


def inward(pts, i):
    """Unit inward normal of a clockwise (seen from above) closed outline at point i."""
    (ax, ay), (bx, by) = pts[i - 1], pts[(i + 1) % len(pts)]
    length = math.hypot(bx - ax, by - ay) or 1.0
    return -(by - ay) / length, (bx - ax) / length


def body():
    """Cage of rings around the outline, smoothed by subdivision: a domed top, rounded edge
    and grips that hang lower than the centre."""
    outline, n = OUTLINE, len(OUTLINE)

    def ring(inset, passes, z_top, z_grip):
        pts = smoothed(outline, passes)
        out = []
        for i, (x, y) in enumerate(pts):
            nx, ny = inward(pts, i)
            px, py = x + nx * inset, y + ny * inset
            slope = 0.08 * py  # the top leans back: higher at the bumpers than at the grips
            out.append((px * MM, py * MM, (z_top - (z_top - z_grip) * grip(y) + slope) * MM))
        return out

    rings = [
        ring(15, 8, 20, 11),     # top, inner: the dome
        ring(6, 3, 16, 8),       # top, outer
        ring(0, 0, 5, -7),       # widest edge
        ring(4, 3, -11, -30),    # underside, outer: grips hang low
        ring(14, 8, -16, -34),   # underside, inner
    ]
    bm = bmesh.new()
    vs = [[bm.verts.new(p) for p in r] for r in rings]
    for a, b in zip(vs, vs[1:]):
        for i in range(n):
            bm.faces.new((a[i], a[(i + 1) % n], b[(i + 1) % n], b[i]))
    bm.faces.new(list(reversed(vs[0])))
    bm.faces.new(vs[-1])
    bmesh.ops.recalc_face_normals(bm, faces=bm.faces)
    me = bpy.data.meshes.new("body")
    bm.to_mesh(me)
    bm.free()
    for poly in me.polygons:
        poly.use_smooth = True
    shell = bpy.data.objects.new("body", me)
    bpy.context.collection.objects.link(shell)
    sub = shell.modifiers.new("smooth", "SUBSURF")
    sub.levels = sub.render_levels = 3
    # Apply the subdivision so colour and texture follow the fine faces, not the cage.
    fine = bpy.data.meshes.new_from_object(shell.evaluated_get(bpy.context.evaluated_depsgraph_get()))
    shell.modifiers.clear()
    shell.data = fine
    fine.materials.append(M["shell"])
    fine.materials.append(M["grip_dots"])
    # Top shell white; below the parting line dark and textured (the reference's two-tone look).
    # Cut the faces exactly on the line: flatten it to z = 0, bisect there, then put it back.
    bm = bmesh.new()
    bm.from_mesh(fine)
    for v in bm.verts:
        v.co.z -= seam_z(v.co.y / MM) * MM
    bmesh.ops.bisect_plane(bm, geom=bm.verts[:] + bm.edges[:] + bm.faces[:], plane_co=(0, 0, 0), plane_no=(0, 0, 1))
    for f in bm.faces:
        f.material_index = 1 if f.calc_center_median().z < 0 else 0
    for v in bm.verts:
        v.co.z += seam_z(v.co.y / MM) * MM
    bm.to_mesh(fine)
    bm.free()
    return box_uv(shell)


def seam_z(y):
    """Height (mm) of the parting line at y: just under the cage's widest ring."""
    return 5 - 12 * grip(y) + 0.08 * y - 1.0


def seam(bvh, shell, r=0.3, per_segment=8):
    """Parting line: a thin dark bead half sunk into the shell's side, all the way round.
    Each point is a ray cast in from outside the outline at the widest edge's height."""
    pts = smoothed(OUTLINE, 3)
    path = []
    for i, (x, y) in enumerate(pts):
        (x2, y2) = pts[(i + 1) % len(pts)]
        for k in range(per_segment):
            t = k / per_segment
            px, py = x + (x2 - x) * t, y + (y2 - y) * t
            nx, ny = inward(pts, i)
            hit, zc = None, seam_z(py)
            for _ in range(4):  # re-aim at the line's height where the ray actually lands
                hit, *_ = bvh.ray_cast(Vector(((px - nx * 30) * MM, (py - ny * 30) * MM, zc * MM)), Vector((nx, ny, 0)))
                if not hit:
                    break
                zc = seam_z(hit.y / MM)
            if hit:
                path.append(hit)
    cu = bpy.data.curves.new("seam", "CURVE")
    cu.dimensions, cu.bevel_depth, cu.bevel_resolution = "3D", r * MM, 2
    sp = cu.splines.new("POLY")
    sp.points.add(len(path) - 1)
    for p, co in zip(sp.points, path):
        p.co = (*co, 1)
    sp.use_cyclic_u = True
    ob = bpy.data.objects.new("seam", cu)
    bpy.context.collection.objects.link(ob)
    ob = to_mesh(ob)
    ob.data.materials.append(M["seam"])
    return attach(ob, shell)


def surface(bvh, x, y):
    """Top surface height (mm) of the shell at x, y."""
    hit, *_ = bvh.ray_cast(Vector((x * MM, y * MM, 0.2)), Vector((0, 0, -1)))
    return hit.z / MM if hit else 0.0


# ---------------------------------------------------------------- parts
def attach(child, parent):
    """Parent without moving: new objects' world matrices are stale until the layer updates."""
    bpy.context.view_layer.update()
    world = child.matrix_world.copy()
    child.parent = parent
    child.matrix_world = world
    return child


def to_mesh(ob):
    """Convert a curve or text object to a mesh object (the glTF exporter wants meshes)."""
    bpy.context.view_layer.objects.active = ob
    for o in bpy.context.view_layer.objects:
        o.select_set(o == ob)
    bpy.ops.object.convert(target="MESH")
    return bpy.context.view_layer.objects.active


def mesh_obj(name, bm, mats, pivot):
    me = bpy.data.meshes.new(name)
    bm.to_mesh(me)
    bm.free()
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    ob.location = [v * MM for v in pivot]
    for m in mats if isinstance(mats, list) else [mats]:
        me.materials.append(m)
    for p in me.polygons:
        p.use_smooth = True
    return ob


def bevel(ob, w, seg=4):
    mod = ob.modifiers.new("bevel", "BEVEL")
    mod.width, mod.segments, mod.limit_method, mod.harden_normals = w * MM, seg, "ANGLE", True
    return ob


def disc(name, r, h, at, mat, r_top=None, segs=64, bev=0.6, parent=None, knurl=0.0):
    """Cylinder standing on `at` (its pivot)."""
    bm = bmesh.new()
    bmesh.ops.create_cone(bm, cap_ends=True, segments=segs, radius1=r * MM, radius2=(r_top or r) * MM, depth=h * MM)
    for v in bm.verts:
        if knurl:
            a = math.atan2(v.co.y, v.co.x)
            if round(a / (2 * math.pi / segs)) % 2 and v.co.z > 0:
                v.co.x *= knurl
                v.co.y *= knurl
        v.co.z += h / 2 * MM
    ob = mesh_obj(name, bm, mat, at)
    if parent:
        attach(ob, parent)
    return bevel(ob, bev) if bev else ob


def slab(name, size, center, pivot, mat, rot=(0, 0, 0), bev=1.5):
    bm = bmesh.new()
    bmesh.ops.create_cube(bm, size=1.0)
    for v in bm.verts:
        v.co = Vector((v.co.x * size[0], v.co.y * size[1], v.co.z * size[2])) * MM
        v.co.rotate(__import__("mathutils").Euler(rot))
        v.co += Vector([(c - p) * MM for c, p in zip(center, pivot)])
    return bevel(mesh_obj(name, bm, mat, pivot), bev)


def letter(ch, at, mat, parent):
    cu = bpy.data.curves.new("txt", "FONT")
    cu.body, cu.size, cu.align_x, cu.align_y, cu.extrude = ch, 6.5 * MM, "CENTER", "CENTER", 0.08 * MM
    ob = bpy.data.objects.new("txt", cu)
    bpy.context.collection.objects.link(ob)
    ob.location = [v * MM for v in at]
    ob = to_mesh(ob)
    ob.data.materials.clear()
    ob.data.materials.append(mat)
    ob.name = f"letter_{ch.lower()}"
    attach(ob, parent)


def glyph(button, bars, top):
    """Dark printed icon on a small button: (w, h, dx, dy, angle) bars, in mm from its centre."""
    x, y, _ = [c / MM for c in button.location]
    for i, (w, h, ox, oy, a) in enumerate(bars):
        bar = slab(f"{button.name}_icon_{i}", (w, h, 0.3), (x + ox, y + oy, top), (x + ox, y + oy, top),
                   M["seam"], rot=(0, 0, math.radians(a)), bev=0.1)
        attach(bar, button)


def build():
    shell = body()
    deps = bpy.context.evaluated_depsgraph_get()
    bvh = BVHTree.FromObject(shell, deps)
    z = lambda x, y: surface(bvh, x, y)

    seam(bvh, shell)

    # Thumbsticks: pivot at the stick's base on the shell; tilt and click.
    for name, (x, y) in (("stick_left", (-36, 14)), ("stick_right", (20, -8))):
        # The well sits on the plane fitted to the shell under its rim, deep enough that the
        # dome's curve can't poke through: its top clears the highest rim point by 0.4 mm.
        rim = [(12.5 * math.cos(a), 12.5 * math.sin(a)) for a in (i * math.pi / 12 for i in range(24))]
        h = [z(x + dx, y + dy) for dx, dy in rim]
        c = sum(h) / len(h)
        gx = sum(dx * (hi - c) for (dx, _), hi in zip(rim, h)) / sum(dx * dx for dx, _ in rim)
        gy = sum(dy * (hi - c) for (_, dy), hi in zip(rim, h)) / sum(dy * dy for _, dy in rim)
        rest = [hi - c - gx * dx - gy * dy for (dx, dy), hi in zip(rim, h)]
        bottom, top = c + min(rest) - 0.8, c + max(rest) + 0.4
        well = disc(f"{name}_well", 12, top - bottom, (x, y, bottom), M["grip"], r_top=11.5, bev=0.6)
        well.rotation_euler = (math.atan(gy), -math.atan(gx), 0)
        stick = disc(name, 3.4, 6.5, (x, y, top - 0.5), M["black"], bev=0.3)
        disc(f"{name}_cap", 9.6, 4.2, (x, y, top + 5.5), M["black"], r_top=9.0, segs=72, bev=1.6, parent=stick, knurl=0.975)

    # Hybrid D-pad: a dish with a raised cross, one node that tilts toward 8 directions.
    dx, dy = -20, -8
    dz = z(dx, dy) - 1.0
    dpad = disc("dpad", 12, 1.6, (dx, dy, dz), M["grip"], r_top=11.4, bev=0.7)
    for rot, lift in ((0, 0.0), (math.pi / 2, 0.06)):  # tiny lift: no flicker where the arms overlap
        arm = slab("dpad_cross", (20, 6.4, 2.6), (dx, dy, dz + 2.4 + lift), (dx, dy, dz + 2.4 + lift), M["black"], rot=(0, 0, rot), bev=0.9)
        attach(arm, dpad)

    # A B X Y: dark glossy buttons, coloured letters.
    fx, fy, off = 40, 12, 9.4
    for ch, (ox, oy) in (("A", (0, -off)), ("B", (off, 0)), ("X", (-off, 0)), ("Y", (0, off))):
        x, y = fx + ox, fy + oy
        bz = z(x, y) - 1.0
        b = disc(f"button_{ch.lower()}", 4.9, 3.0, (x, y, bz), M["face"], r_top=4.6, bev=1.2)
        letter(ch, (x, y, bz + 3.05), M[ch], b)

    # View, Share, Menu and the Xbox button. The small three sit in dark rings and carry dark
    # icons, so they read against the white shell.
    icons = {
        "button_view": [(1.6, 1.2, -0.4, 0.35, 0), (1.6, 1.2, 0.4, -0.35, 0)],  # two overlapping windows
        "button_menu": [(2.2, 0.35, 0, d, 0) for d in (-0.7, 0, 0.7)],  # three lines
        "button_share": [(0.35, 1.6, 0, -0.2, 0), (0.35, 1.0, -0.3, 0.45, -45), (0.35, 1.0, 0.3, 0.45, 45)],  # up arrow
    }
    for name, (x, y), r in (("button_view", (-11, 12), 2.9), ("button_menu", (11, 12), 2.9), ("button_share", (0, 4), 2.6)):
        disc(f"{name}_ring", r + 0.8, 1.2, (x, y, z(x, y) - 0.8), M["grip"], bev=0.4)
        b = disc(name, r, 2.0, (x, y, z(x, y) - 0.6), M["white_btn"], bev=0.6)
        glyph(b, icons[name], z(x, y) + 1.4)
    xb = (0, 27)
    disc("button_xbox_ring", 7.6, 1.2, (*xb, z(*xb) - 0.8), M["grip"], bev=0.4)
    disc("button_xbox", 6.6, 2.0, (*xb, z(*xb) - 0.6), M["xbox"], r_top=6.3, bev=0.8)

    # Bumpers hinge at the back; triggers hinge at their top edge.
    for s, side in ((-1, "left"), (1, "right")):
        bx, by, bz = s * 42, 45, 6.5
        box_uv(slab(f"bumper_{side}", (40, 8, 5), (bx, by, bz), (bx, by + 4, bz), M["shell_dots"], rot=(0, 0, s * math.radians(-9)), bev=2.2))
        tx, ty = s * 46, 50
        box_uv(slab(f"trigger_{side}", (22, 10, 15), (tx, ty, bz - 9), (tx, ty, bz - 2), M["grip_dots"], rot=(math.radians(-25), 0, 0), bev=3.0))

    # Ports: USB-C at the top edge, headset jack at the bottom.
    slab("port_usb_c", (9, 3, 3.2), (0, 47, z(0, 40) - 8), (0, 47, z(0, 40) - 8), M["port"], bev=1.2)
    slab("port_headset", (4, 6, 4), (0, -16, z(0, -10) - 9), (0, -16, z(0, -10) - 9), M["port"], bev=1.6)
    bpy.context.view_layer.update()
    for name in ("dpad", "dpad_cross", "dpad_cross.001", "stick_left_cap"):
        o = bpy.data.objects.get(name)
        if o:
            print("WHERE", name, [round(c / MM, 1) for c in o.matrix_world.translation], "dims", [round(c / MM, 1) for c in o.dimensions])
    return shell


# ---------------------------------------------------------------- scene, renders, export
def studio(target):
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
    for name, loc, power, size in (("key", (-0.25, -0.3, 0.4), 3.0, 0.4), ("fill", (0.3, -0.2, 0.25), 1.0, 0.5), ("rim", (0.0, 0.35, 0.3), 2.5, 0.3)):
        light = bpy.data.lights.new(name, "AREA")
        light.energy, light.size = power, size
        ob = bpy.data.objects.new(name, light)
        bpy.context.collection.objects.link(ob)
        ob.location = loc
        ob.constraints.new("TRACK_TO").target = target
    cam = bpy.data.objects.new("camera", bpy.data.cameras.new("camera"))
    bpy.context.collection.objects.link(cam)
    cam.data.lens = 70
    cam.constraints.new("TRACK_TO").target = target
    sc.camera = cam
    return cam


def main():
    for o in list(bpy.data.objects):
        bpy.data.objects.remove(o)
    shell = build()
    os.makedirs(OUT, exist_ok=True)
    bpy.ops.export_scene.gltf(filepath=os.path.join(OUT, "xbox-series-robot-white.glb"), export_format="GLB", export_apply=True, export_yup=True, export_cameras=False, export_lights=False)
    cam = studio(shell)
    for name, loc in (("xbox-series-robot-white-34", (0.12, -0.3, 0.3)), ("xbox-series-robot-white-top", (0.0, -0.01, 0.42))):
        cam.location = loc
        bpy.context.scene.render.filepath = os.path.join(OUT, name + ".png")
        bpy.ops.render.render(write_still=True)
    o = bpy.data.objects
    o["button_a"].location.z -= 1.5 * MM
    o["stick_left"].rotation_euler.x = math.radians(-20)
    o["trigger_right"].rotation_euler.x = math.radians(-18)
    cam.location = (0.12, -0.3, 0.3)
    bpy.context.scene.render.filepath = os.path.join(OUT, "xbox-series-robot-white-pressed.png")
    bpy.ops.render.render(write_still=True)


main()
