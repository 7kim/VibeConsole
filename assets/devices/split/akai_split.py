# Split the downloaded MPK Mini .blend (parts grouped by kind) into one named, pivoted part
# per control, levelled on its pads. Run on the opened .blend; args after --: <out-prefix>
import bpy, sys, os, math
from mathutils import Vector, Matrix

OUT = sys.argv[sys.argv.index("--") + 1:][0]

for o in list(bpy.data.objects):
    if o.type in {"CAMERA", "LIGHT"} or "cyclo" in o.name.lower() or o.name.lower().startswith("fond"):
        bpy.data.objects.remove(o)
# Textures the download doesn't include stay unlinked rather than pink.
for mat in bpy.data.materials:
    if mat.use_nodes:
        for node in mat.node_tree.nodes:
            if node.type == "TEX_IMAGE" and node.image and not node.image.packed_file and not os.path.exists(bpy.path.abspath(node.image.filepath)):
                for link in list(node.outputs[0].links):
                    mat.node_tree.links.remove(link)
for o in list(bpy.context.scene.objects):
    if o.type != "MESH":
        continue
    o.data = o.data.copy()  # shared meshes would be transformed twice
for o in [o for o in bpy.context.scene.objects if o.type == "MESH"]:
    mw = o.matrix_world.copy()
    o.parent = None
    o.data.transform(mw)
    o.matrix_world = Matrix.Identity(4)
for o in [o for o in bpy.context.scene.objects if o.type != "MESH"]:
    bpy.data.objects.remove(o)

GROUPED = ["Touches noires", "Touches blanches", "Potards", "Calottes potards", "Pads", "Contours pads rouges",
           "Contours pads gris", "Boutons", "Contours boutons rouges", "Contours boutons gris"]
kind = {}
for name in GROUPED:
    ob = bpy.data.objects[name]
    bpy.ops.object.select_all(action="DESELECT")
    ob.select_set(True)
    bpy.context.view_layer.objects.active = ob
    bpy.ops.object.mode_set(mode="EDIT")
    bpy.ops.mesh.select_all(action="SELECT")
    bpy.ops.mesh.separate(type="LOOSE")
    bpy.ops.object.mode_set(mode="OBJECT")
    for piece in bpy.context.selected_objects:
        kind[piece.name] = name


def box(ob):
    vs = [v.co for v in ob.data.vertices]
    lo = Vector([min(v[k] for v in vs) for k in range(3)])
    hi = Vector([max(v[k] for v in vs) for k in range(3)])
    return lo, hi, (lo + hi) / 2


def of(name):
    return [bpy.data.objects[n] for n, k in kind.items() if k == name]


# Level on the pads: their rows give "right", their columns "back".
pads = sorted(of("Pads"), key=lambda o: box(o)[2].x)
cols = [sorted(pads[i:i + 2], key=lambda o: box(o)[2].y) for i in range(0, 8, 2)]
right = (box(cols[3][0])[2] - box(cols[0][0])[2]).normalized()
back = (box(cols[0][1])[2] - box(cols[0][0])[2]).normalized()
up = right.cross(back).normalized()
back = up.cross(right).normalized()
rot = Matrix((right, back, up)).to_4x4()
meshes = [o for o in bpy.context.scene.objects if o.type == "MESH"]
for o in meshes:
    o.data.transform(rot)
lo = Vector([min(box(o)[0][k] for o in meshes) for k in range(3)])
hi = Vector([max(box(o)[1][k] for o in meshes) for k in range(3)])
for o in meshes:
    o.data.transform(Matrix.Translation(Vector((-(lo.x + hi.x) / 2, -(lo.y + hi.y) / 2, -lo.z))))
print("LEVELED mm", [round(x * 1000, 1) for x in (hi - lo)])

names = {}
keys = sorted(of("Touches blanches") + of("Touches noires"), key=lambda o: box(o)[2].x)
assert len(keys) == 25, len(keys)
for i, ob in enumerate(keys):
    names[ob.name] = f"key_{i:02d}"
pads = sorted(of("Pads"), key=lambda o: box(o)[2].x)
for c in range(4):
    front, rear = sorted(pads[2 * c:2 * c + 2], key=lambda o: box(o)[2].y)
    names[front.name], names[rear.name] = f"pad_{c + 1}", f"pad_{c + 5}"
nearest = lambda ob, pool: min(pool, key=lambda p: (box(p)[2].xy - box(ob)[2].xy).length)
for rim in of("Contours pads rouges") + of("Contours pads gris"):
    names[rim.name] = names[nearest(rim, pads).name] + "_light"
knobs = sorted(of("Potards"), key=lambda o: box(o)[2].x)
for c in range(4):
    front, rear = sorted(knobs[2 * c:2 * c + 2], key=lambda o: box(o)[2].y)
    names[rear.name], names[front.name] = f"knob_{c + 1}", f"knob_{c + 5}"
for cap in of("Calottes potards"):
    names[cap.name] = names[nearest(cap, knobs).name]  # joined into its knob below
buttons = sorted(of("Boutons"), key=lambda o: box(o)[2].x)
left, rightside = buttons[:6], buttons[6:]
labels = [["full_level", "octave_down", "arp_on_off"], ["note_repeat", "octave_up", "tap_tempo"]]
for c in range(2):
    for r, ob in enumerate(sorted(left[3 * c:3 * c + 3], key=lambda o: box(o)[2].y)):
        names[ob.name] = "button_" + labels[c][r]
for ob, label in zip(rightside, ["bank_a_b", "cc", "prog_change", "prog_select"]):
    names[ob.name] = "button_" + label
for rim in of("Contours boutons rouges") + of("Contours boutons gris"):
    names[rim.name] = names[nearest(rim, buttons).name] + "_light"
names["Joystick bouton rouge"] = "joystick"
names["Ecran verre"] = "display"

groups = {}
for o in meshes:
    groups.setdefault(names.get(o.name, "body"), []).append(o)
named = {}
for name, obs in groups.items():
    bpy.ops.object.select_all(action="DESELECT")
    for o in obs:
        o.select_set(True)
    bpy.context.view_layer.objects.active = obs[0]
    if len(obs) > 1:
        bpy.ops.object.join()
    ob = bpy.context.view_layer.objects.active
    ob.name = ob.data.name = name
    named[name] = ob
print("PARTS", len(named), sorted(named))


def set_origin(ob, where):
    lo, hi, c = box(ob)
    p = {"center": c, "base": Vector((c.x, c.y, lo.z)), "hinge": Vector((c.x, hi.y, lo.z))}[where]
    ob.data.transform(Matrix.Translation(-p))
    ob.location = p


for name, ob in named.items():
    set_origin(ob, "hinge" if name.startswith("key_") else "base" if name == "joystick" else "center")

# Each pad's light: its own material, unlit at rest, red when the app lights it.
base = bpy.data.materials.get("Contour pad gris")
for n in range(1, 9):
    ob = named[f"pad_{n}_light"]
    m = base.copy()
    m.name = f"pad_{n}_light"
    bsdf = [x for x in m.node_tree.nodes if x.type == "BSDF_PRINCIPLED"][0]
    bsdf.inputs["Emission Color"].default_value = (1.0, 0.05, 0.03, 1)
    bsdf.inputs["Emission Strength"].default_value = 0.0
    ob.data.materials.clear()
    ob.data.materials.append(m)

bpy.ops.object.select_all(action="SELECT")
bpy.ops.export_scene.gltf(filepath=OUT + ".glb", export_format="GLB", export_yup=True, export_cameras=False, export_lights=False)
print("EXPORTED", OUT + ".glb")
