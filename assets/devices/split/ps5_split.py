# Split the Sketchfab DualSense (3 meshes by material) into one named, pivoted part per control.
# Names are given in the file's own frame (face toward +Y, up = +Z), then the model is stood up
# (face toward +Z, up = +Y). Args after --: <in.glb> <out-prefix>
import bpy, bmesh, sys, math
from mathutils import Vector, Matrix

SRC, OUT = sys.argv[sys.argv.index("--") + 1:][:2]
for o in list(bpy.data.objects):
    bpy.data.objects.remove(o)
bpy.ops.import_scene.gltf(filepath=SRC)
meshes = [o for o in bpy.context.scene.objects if o.type == "MESH"]
for o in meshes:
    o.data = o.data.copy()
    mw = o.matrix_world.copy()
    o.parent = None
    o.data.transform(mw)
    o.matrix_world = Matrix.Identity(4)
for o in [o for o in bpy.context.scene.objects if o.type != "MESH"]:
    bpy.data.objects.remove(o)
bpy.ops.object.select_all(action="DESELECT")
for o in meshes:
    o.select_set(True)
bpy.context.view_layer.objects.active = meshes[0]
bpy.ops.object.join()
mesh = bpy.context.view_layer.objects.active
bpy.ops.object.mode_set(mode="EDIT")
bpy.ops.mesh.select_all(action="SELECT")
bpy.ops.mesh.separate(type="LOOSE")
bpy.ops.object.mode_set(mode="OBJECT")
pieces = [o for o in bpy.context.scene.objects if o.type == "MESH"]


def box(o):
    vs = [v.co for v in o.data.vertices]
    lo = Vector([min(v[k] for v in vs) for k in range(3)])
    hi = Vector([max(v[k] for v in vs) for k in range(3)])
    return lo, hi


LO = Vector([min(box(o)[0][k] for o in pieces) for k in range(3)])
HI = Vector([max(box(o)[1][k] for o in pieces) for k in range(3)])
SPAN = HI - LO


def nc(o):
    """Centre in 0..1 of the whole model; x from the controller's left as seen from the front.
    The file's +X is the controller's left: the renders (v7 ticket 74) show the cluster at the
    file's high x where the d-pad is, so x is flipped here, never the model."""
    lo, hi = box(o)
    c = Vector([((lo[k] + hi[k]) / 2 - LO[k]) / SPAN[k] for k in range(3)])
    c.x = 1 - c.x
    return c


def near(x, z, r, front=0.7):
    return [o for o in pieces if (Vector((nc(o).x, nc(o).z)) - Vector((x, z))).length < r and nc(o).y > front]


names = {}
def give(name, obs):
    for o in obs:
        names.setdefault(o.name, name)

big = lambda o: len(o.data.vertices) > 2000
# Placed against Sony's front diagram (CFI-ZCT2 manual): d-pad left of the sticks, action
# buttons right, Create left of the touch pad, Options right. Centres read from the file's parts.
give("dpad_up", near(0.19, 0.789, 0.03))
give("dpad_down", near(0.187, 0.637, 0.03))
give("dpad_left", near(0.139, 0.715, 0.03))
give("dpad_right", near(0.238, 0.711, 0.03))
give("button_triangle", near(0.811, 0.826, 0.035))
give("button_cross", near(0.811, 0.599, 0.035))
give("button_square", near(0.738, 0.712, 0.035))
give("button_circle", near(0.885, 0.712, 0.035))
for name, x in (("stick_left", 0.341), ("stick_right", 0.659)):
    give(name, [o for o in near(x, 0.499, 0.08, 0.85) if len(o.data.vertices) != 520 and not big(o)])
give("button_ps", near(0.5, 0.51, 0.045, 0.75))
give("button_mute", near(0.5, 0.42, 0.03, 0.7))
give("button_create", near(0.267, 0.879, 0.03, 0.75))
give("button_options", near(0.733, 0.879, 0.03, 0.75))
give("touchpad", [o for o in near(0.5, 0.82, 0.12, 0.75) if not big(o)])
for name, sel in (("bumper_left", lambda c: c.x < 0.5), ("bumper_right", lambda c: c.x > 0.5)):
    give(name, [o for o in pieces if len(o.data.vertices) == 423 and sel(nc(o))])
for name, sel in (("trigger_left", lambda c: c.x < 0.5), ("trigger_right", lambda c: c.x > 0.5)):
    give(name, [o for o in pieces if len(o.data.vertices) == 427 and sel(nc(o))])

groups = {}
for o in pieces:
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
print("PARTS", sorted((k, len(v)) for k, v in groups.items()))

# Stand it up: face toward +Z, the face's up toward +Y; real DualSense ~160 mm wide.
rot = Matrix(((-1, 0, 0), (0, 0, 1), (0, 1, 0))).to_4x4()  # proper rotation: det = +1
for ob in named.values():
    ob.data.transform(rot)
lo = Vector([min(box(o)[0][k] for o in named.values()) for k in range(3)])
hi = Vector([max(box(o)[1][k] for o in named.values()) for k in range(3)])
s = 0.160 / (hi - lo).x
for ob in named.values():
    ob.data.transform(Matrix.Scale(s, 4) @ Matrix.Translation(-(lo + hi) / 2))


def set_origin(ob, where):
    l, h = box(ob)
    c = (l + h) / 2
    p = {"center": c, "base": Vector((c.x, c.y, l.z)), "back": Vector((c.x, h.y, c.z)), "top": Vector((c.x, h.y, h.z))}[where]
    ob.data.transform(Matrix.Translation(-p))
    ob.location = p


for name, ob in named.items():
    set_origin(ob, "base" if name.startswith("stick") else "back" if name.startswith("bumper") else "top" if name.startswith("trigger") else "center")

# Lights the downloaded file doesn't model separately: the light bar (two strips beside the
# touchpad), the five player-indicator LEDs under it, and the Mute button's orange light.
def light_material(name, rgb):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    bsdf = m.node_tree.nodes["Principled BSDF"]
    bsdf.inputs["Base Color"].default_value = (0.05, 0.05, 0.06, 1)
    bsdf.inputs["Emission Color"].default_value = (*rgb, 1)
    bsdf.inputs["Emission Strength"].default_value = 1.0  # kept in the glTF; the app sets it (0 at rest)
    return m


def block(name, center, size, mat):
    import bmesh as bm_
    me = bpy.data.meshes.new(name)
    b = bm_.new()
    bm_.ops.create_cube(b, size=1.0)
    for v in b.verts:
        v.co = Vector((v.co.x * size.x, v.co.y * size.y, v.co.z * size.z))
    b.to_mesh(me)
    b.free()
    ob = bpy.data.objects.new(name, me)
    bpy.context.collection.objects.link(ob)
    ob.location = center
    me.materials.append(mat)
    return ob


tl, th = box(named["touchpad"])
tl, th = tl + named["touchpad"].location, th + named["touchpad"].location
MMs = 0.001
bar = light_material("light_bar", (0.08, 0.3, 1.0))
strips = [block("light_bar", Vector((x, (tl.y + th.y) / 2, th.z - 1.0 * MMs)), Vector((1.6 * MMs, (th.y - tl.y) * 0.8, 1.5 * MMs)), bar)
          for x in (tl.x - 1.2 * MMs, th.x + 1.2 * MMs)]
bpy.ops.object.select_all(action="DESELECT")
for o in strips:
    o.select_set(True)
bpy.context.view_layer.objects.active = strips[0]
bpy.ops.object.join()
named["light_bar"] = bpy.context.view_layer.objects.active
named["light_bar"].name = "light_bar"
led = light_material("player_led", (1.0, 1.0, 1.0))
for i in range(5):
    x = (tl.x + th.x) / 2 + (i - 2) * 4.0 * MMs
    named[f"player_led_{i + 1}"] = block(f"player_led_{i + 1}", Vector((x, tl.y - 2.5 * MMs, tl.z + 0.3 * MMs)), Vector((2.4 * MMs, 0.9 * MMs, 0.6 * MMs)), led)
mute = named["button_mute"]
for slot in mute.material_slots:
    m = slot.material.copy()
    m.name = "button_mute_light"
    bsdf = [n for n in m.node_tree.nodes if n.type == "BSDF_PRINCIPLED"][0]
    for link in list(bsdf.inputs["Emission Color"].links):
        m.node_tree.links.remove(link)
    bsdf.inputs["Emission Color"].default_value = (1.0, 0.35, 0.0, 1)
    bsdf.inputs["Emission Strength"].default_value = 1.0
    slot.material = m

# Textures to 2K so the file stays small.
for img in bpy.data.images:
    if img.size[0] > 2048:
        img.scale(2048, 2048 * img.size[1] // img.size[0])
bpy.ops.object.select_all(action="SELECT")
bpy.ops.export_scene.gltf(filepath=OUT + ".glb", export_format="GLB", export_yup=True, export_cameras=False,
                          export_lights=False, export_image_format="JPEG", export_jpeg_quality=85)
print("EXPORTED", OUT + ".glb")
