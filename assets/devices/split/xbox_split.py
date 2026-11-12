# Split a single-mesh Sketchfab Xbox controller into one named, pivoted part per control.
# Args after --: <in.glb> <out-prefix> <debug 0|1>
import bpy, bmesh, sys, math, json, mathutils
from mathutils import Vector, Matrix

argv = sys.argv[sys.argv.index("--") + 1:]
SRC, OUT, DEBUG = argv[0], argv[1], argv[2] == "1"
ICON = argv[3] if len(argv) > 3 else "dark"  # icon print on View/Menu: dark on white, light on black

for o in list(bpy.data.objects):
    bpy.data.objects.remove(o)
bpy.ops.import_scene.gltf(filepath=SRC)
mesh = [o for o in bpy.context.scene.objects if o.type == "MESH"][0]
# Bake every parent transform into the mesh, then drop the empties.
mesh.data.transform(mesh.matrix_world)
mesh.parent = None
mesh.matrix_world = Matrix.Identity(4)
for o in list(bpy.context.scene.objects):
    if o is not mesh:
        bpy.data.objects.remove(o)


def islands(ob):
    bm = bmesh.new()
    bm.from_mesh(ob.data)
    bm.verts.ensure_lookup_table()
    seen, out = set(), []
    for v in bm.verts:
        if v.index in seen:
            continue
        stack, isl = [v], []
        seen.add(v.index)
        while stack:
            x = stack.pop()
            isl.append(x.co.copy())
            for e in x.link_edges:
                y = e.other_vert(x)
                if y.index not in seen:
                    seen.add(y.index)
                    stack.append(y)
        lo = Vector([min(p[i] for p in isl) for i in range(3)])
        hi = Vector([max(p[i] for p in isl) for i in range(3)])
        out.append({"n": len(isl), "lo": lo, "hi": hi, "c": (lo + hi) / 2})
    bm.free()
    return out


def bounds(items):
    lo = Vector([min(i["lo"][k] for i in items) for k in range(3)])
    hi = Vector([max(i["hi"][k] for i in items) for k in range(3)])
    return lo, hi


# 1. Level it: the A/B/X/Y caps (four similar islands on the right) define the face plane.
isl = islands(mesh)
lo, hi = bounds(isl)
span = hi - lo
nz = lambda v: Vector([(v[k] - lo[k]) / span[k] for k in range(3)])
caps = [i for i in isl if 450 <= i["n"] <= 700 and nz(i["c"]).x > 0.65]
caps.sort(key=lambda i: nz(i["c"]).x)
assert len(caps) == 4, [nz(i["c"]) for i in caps]
x_cap, b_cap = caps[0], caps[-1]
a_cap, y_cap = sorted(caps[1:3], key=lambda i: i["c"].y)
up = (y_cap["c"] - a_cap["c"]).normalized()
right = (b_cap["c"] - x_cap["c"]).normalized()
normal = right.cross(up).normalized()
right = up.cross(normal).normalized()
rot = Matrix((right, up, normal)).to_4x4()  # rows: new X, Y, Z in old coordinates
mesh.data.transform(rot)
# Real Xbox One controller: about 153 mm wide.
isl = islands(mesh)
lo, hi = bounds(isl)
scale = 0.153 / (hi - lo).x
mesh.data.transform(Matrix.Scale(scale, 4) @ Matrix.Translation(-(lo + hi) / 2))
isl = islands(mesh)
lo, hi = bounds(isl)
span = hi - lo
nz = lambda v: Vector([(v[k] - lo[k]) / span[k] for k in range(3)])
print("LEVELED span mm", [round(s * 1000, 1) for s in span])


# 2. Roll: the bumpers are a left/right pair, so their line must be level.
bl, br = sorted([i for i in isl if i["n"] == 440], key=lambda i: i["c"].x)
d = br["c"] - bl["c"]
mesh.data.transform(Matrix.Rotation(math.atan2(d.z, d.x), 4, "Y"))
isl = islands(mesh)
lo, hi = bounds(isl)
mesh.data.transform(Matrix.Translation(-(lo + hi) / 2))
isl = islands(mesh)
lo, hi = bounds(isl)
span = hi - lo
nz = lambda v: Vector([(v[k] - lo[k]) / span[k] for k in range(3)])
for it in isl:
    it["nc"] = nz(it["c"])

# 3. Name every island: a control, or the static body.
def near(center, r):
    return [i for i in isl if (Vector((i["nc"].x, i["nc"].y)) - Vector((center.x, center.y))).length < r]
part = {}
caps = sorted([i for i in isl if 450 <= i["n"] <= 700 and i["nc"].x > 0.6 and (i["hi"] - i["lo"]).x / span.x < 0.09], key=lambda i: i["nc"].x)
assert len(caps) == 4, len(caps)
x_cap, b_cap = caps[0], caps[-1]
a_cap, y_cap = sorted(caps[1:3], key=lambda i: i["nc"].y)
for name, cap in (("button_a", a_cap), ("button_b", b_cap), ("button_x", x_cap), ("button_y", y_cap)):
    part[id(cap)] = name
sticks = sorted([i for i in isl if i["n"] == 854], key=lambda i: i["nc"].x)
for name, stick in zip(("stick_left", "stick_right"), sticks):
    for i in near(stick["nc"], 0.075):
        if i["n"] != 490 and i["n"] < 1000:  # 490: the well around the stick stays on the body
            part[id(i)] = name
dpad = [i for i in isl if i["n"] == 1711][0]
part[id(dpad)] = "dpad"
guide = [i for i in isl if i["n"] == 485][0]
for i in near(guide["nc"], 0.04):
    if i["n"] < 1000 and abs(i["nc"].z - guide["nc"].z) < 0.08:
        part[id(i)] = "button_xbox"
small = sorted([i for i in isl if i["n"] in (405, 433)], key=lambda i: i["nc"].x)
for name, b in zip(("button_view", "button_menu"), small):
    for i in near(b["nc"], 0.025):
        if i["n"] < 1000 and abs(i["nc"].z - b["nc"].z) < 0.08:
            part[id(i)] = name
for name, it in zip(("bumper_left", "bumper_right"), sorted([i for i in isl if i["n"] == 440], key=lambda i: i["nc"].x)):
    part[id(it)] = name
for name, it in zip(("trigger_left", "trigger_right"), sorted([i for i in isl if i["n"] == 366], key=lambda i: i["nc"].x)):
    part[id(it)] = name

# 4. Split the mesh by island into named objects.
bpy.context.view_layer.objects.active = mesh
mesh.select_set(True)
bpy.ops.object.mode_set(mode="EDIT")
bpy.ops.mesh.select_all(action="SELECT")
bpy.ops.mesh.separate(type="LOOSE")
bpy.ops.object.mode_set(mode="OBJECT")
pieces = [o for o in bpy.context.scene.objects if o.type == "MESH"]
def key(ob):
    vs = [v.co for v in ob.data.vertices]
    l = Vector([min(v[k] for v in vs) for k in range(3)]); h = Vector([max(v[k] for v in vs) for k in range(3)])
    return len(vs), tuple(round(x, 6) for x in (l + h) / 2)
lookup = {(i["n"], tuple(round(x, 6) for x in i["c"])): part.get(id(i), "body") for i in isl}
groups = {}
for ob in pieces:
    groups.setdefault(lookup.get(key(ob), "body"), []).append(ob)
named = {}
for name, obs in groups.items():
    bpy.ops.object.select_all(action="DESELECT")
    for ob in obs:
        ob.select_set(True)
    bpy.context.view_layer.objects.active = obs[0]
    if len(obs) > 1:
        bpy.ops.object.join()
    ob = bpy.context.view_layer.objects.active
    ob.name = ob.data.name = name
    named[name] = ob
print("PARTS", sorted((k, len(v)) for k, v in groups.items()))

# 4b. Grip dots on the triggers, bumpers and the back of the grips: the source has no texture
# there. Ticket 38's dot normal map, on its own box-projected UV map so the colour atlas stays.
import os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from xbox_series_white import DOTS, box_uv


def grip(ob, faces=None):
    me = box_uv(ob, "grip").data
    m = ob.material_slots[0].material.copy()
    m.name = "grip_dotted"
    nt = m.node_tree
    uvn = nt.nodes.new("ShaderNodeUVMap")
    uvn.uv_map = "grip"
    tex = nt.nodes.new("ShaderNodeTexImage")
    tex.image = DOTS
    nm = nt.nodes.new("ShaderNodeNormalMap")
    nm.uv_map = "grip"
    nt.links.new(uvn.outputs["UV"], tex.inputs["Vector"])
    nt.links.new(tex.outputs["Color"], nm.inputs["Color"])
    nt.links.new(nm.outputs["Normal"], [n for n in nt.nodes if n.type == "BSDF_PRINCIPLED"][0].inputs["Normal"])
    if faces is None:
        ob.material_slots[0].material = m
        return
    me.materials.append(m)
    for poly in me.polygons:
        if faces(poly):
            poly.material_index = len(me.materials) - 1


for name in ("trigger_left", "trigger_right", "bumper_left", "bumper_right"):
    grip(named[name])
# The back of the grips: faces turned away from the face plane, below the centre line.
# ponytail: normal/centre test, not a painted mask; paint one if the dots spill where they shouldn't.
grip(named["body"], lambda p: p.normal.z < -0.3 and p.center.y < 0)

# 5. The View and Menu icons printed on their caps (two overlapping rectangles; three lines),
# joined to their buttons so they move with them.
import bmesh as _bm
ink = bpy.data.materials.new("icon_print")
ink.use_nodes = True
_bsdf = ink.node_tree.nodes["Principled BSDF"]
_bsdf.inputs["Base Color"].default_value = (0.03, 0.03, 0.035, 1) if ICON == "dark" else (0.72, 0.72, 0.75, 1)
_bsdf.inputs["Roughness"].default_value = 0.6
MMx = 0.001


def slab(cx, cy, w, h, z):
    me = bpy.data.meshes.new("icon")
    b = _bm.new()
    _bm.ops.create_cube(b, size=1.0)
    for v in b.verts:
        v.co = Vector((cx + v.co.x * w, cy + v.co.y * h, z + (v.co.z + 0.5) * 0.25 * MMx))
    b.to_mesh(me)
    b.free()
    ob = bpy.data.objects.new("icon", me)
    bpy.context.collection.objects.link(ob)
    me.materials.append(ink)
    return ob


def print_icon(button, kind):
    vs = [v.co for v in button.data.vertices]
    c = Vector(((min(v.x for v in vs) + max(v.x for v in vs)) / 2, (min(v.y for v in vs) + max(v.y for v in vs)) / 2))
    z = max(v.z for v in vs) - 0.12 * MMx
    t = 0.34 * MMx
    parts = []
    if kind == "view":
        for dx, dy in ((-0.45, 0.4), (0.45, -0.4)):
            x, y, w, h = c.x + dx * MMx, c.y + dy * MMx, 2.4 * MMx, 1.8 * MMx
            parts += [slab(x, y + h / 2, w, t, z), slab(x, y - h / 2, w, t, z), slab(x - w / 2, y, t, h, z), slab(x + w / 2, y, t, h, z)]
    else:
        for dy in (0.8, 0.0, -0.8):
            parts.append(slab(c.x, c.y + dy * MMx, 2.8 * MMx, 0.4 * MMx, z))
    bpy.ops.object.select_all(action="DESELECT")
    for o in parts + [button]:
        o.select_set(True)
    bpy.context.view_layer.objects.active = button
    bpy.ops.object.join()


print_icon(named["button_view"], "view")
print_icon(named["button_menu"], "menu")

# 6. Pivots where each part really moves.
# 5b. A Share button, as on the user's Series X|S controller (the Xbox One mesh has none):
# centred below the Xbox button, between View and Menu and a little lower, sitting on the shell.
from mathutils.bvhtree import BVHTree


def centre(ob):
    vs = [v.co for v in ob.data.vertices]
    return Vector([(min(v[k] for v in vs) + max(v[k] for v in vs)) / 2 for k in range(3)])


view_c, menu_c = centre(named["button_view"]), centre(named["button_menu"])
spot = Vector(((view_c.x + menu_c.x) / 2, (view_c.y + menu_c.y) / 2 - 2.5 * MMx))
# The shell's top within 3 mm of the spot (a ray-cast here found an inner layer and buried it).
near_top = [v.co.z for v in named["body"].data.vertices if (Vector((v.co.x, v.co.y)) - spot).length < 3 * MMx]
assert near_top, "no shell under the Share button"
hit = Vector((spot.x, spot.y, max(near_top)))
cap = bpy.data.materials.new("button_share")
cap.use_nodes = True
_c = cap.node_tree.nodes["Principled BSDF"]
_c.inputs["Base Color"].default_value = (0.78, 0.78, 0.8, 1) if ICON == "dark" else (0.035, 0.035, 0.04, 1)
_c.inputs["Roughness"].default_value = 0.45
me = bpy.data.meshes.new("button_share")
b = _bm.new()
_bm.ops.create_uvsphere(b, u_segments=32, v_segments=16, radius=1.0)
for v in b.verts:
    v.co = Vector((spot.x + v.co.x * 3.1 * MMx, spot.y + v.co.y * 2.0 * MMx, hit.z - 0.3 * MMx + v.co.z * 1.4 * MMx))
b.to_mesh(me)
b.free()
share = bpy.data.objects.new("button_share", me)
bpy.context.collection.objects.link(share)
me.materials.append(cap)
named["button_share"] = share


def tilted(cx, cy, w, h, z, angle):
    """A slab of the icon print, turned by `angle` (radians) about its centre."""
    ob = slab(cx, cy, w, h, z)
    ob.data.transform(Matrix.Translation(Vector((cx, cy, 0))) @ Matrix.Rotation(angle, 4, "Z") @ Matrix.Translation(Vector((-cx, -cy, 0))))
    return ob


# The share icon: an arrow up out of a tray.
top = max(v.co.z for v in me.vertices) - 0.12 * MMx
t = 0.32 * MMx
cx, cy = spot.x, spot.y
parts = [slab(cx, cy + 0.15 * MMx, t, 1.5 * MMx, top),  # shaft
         tilted(cx - 0.33 * MMx, cy + 0.62 * MMx, t, 0.85 * MMx, top, math.radians(-45)),  # arrowhead
         tilted(cx + 0.33 * MMx, cy + 0.62 * MMx, t, 0.85 * MMx, top, math.radians(45)),
         slab(cx, cy - 0.85 * MMx, 2.0 * MMx, t, top),  # tray
         slab(cx - 1.0 * MMx, cy - 0.55 * MMx, t, 0.75 * MMx, top),
         slab(cx + 1.0 * MMx, cy - 0.55 * MMx, t, 0.75 * MMx, top)]
bpy.ops.object.select_all(action="DESELECT")
for o in parts + [share]:
    o.select_set(True)
bpy.context.view_layer.objects.active = share
bpy.ops.object.join()
named["button_share"] = bpy.context.view_layer.objects.active
named["button_share"].name = "button_share"


def set_origin(ob, where):
    vs = [v.co for v in ob.data.vertices]
    l = Vector([min(v[k] for v in vs) for k in range(3)]); h = Vector([max(v[k] for v in vs) for k in range(3)])
    c = (l + h) / 2
    p = {"center": c, "base": Vector((c.x, c.y, l.z)), "back": Vector((c.x, h.y, c.z)), "top": Vector((c.x, h.y, h.z))}[where]
    ob.data.transform(Matrix.Translation(-p))
    ob.location = p
for name, ob in named.items():
    set_origin(ob, "base" if name.startswith("stick") else "back" if name.startswith("bumper") else "top" if name.startswith("trigger") else "center")

# 7. The Xbox button lights: its own material, emissive from its own texture (the logo).
gb = named["button_xbox"]
for slot in gb.material_slots:
    m = slot.material.copy()
    m.name = "button_xbox_light"
    nodes, links = m.node_tree.nodes, m.node_tree.links
    bsdf = [n for n in nodes if n.type == "BSDF_PRINCIPLED"][0]
    tex = [n for n in nodes if n.type == "TEX_IMAGE"]
    if tex:
        links.new(tex[0].outputs["Color"], bsdf.inputs["Emission Color"])
    else:
        bsdf.inputs["Emission Color"].default_value = (1, 1, 1, 1)
    # Strength 1 keeps the emissive texture in the glTF; the app sets the intensity (0 at rest).
    bsdf.inputs["Emission Strength"].default_value = 1.0
    slot.material = m

bpy.ops.object.select_all(action="SELECT")
bpy.ops.export_scene.gltf(filepath=OUT + ".glb", export_format="GLB", export_yup=True, export_cameras=False, export_lights=False)
print("EXPORTED", OUT + ".glb")
