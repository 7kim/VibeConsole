# Render a split model at rest and with each control pressed (top view), plus a 3/4 rest view,
# and write each control's on-image rectangle for click hotspots.
# Args after --: <split.glb> <out-dir> <prefix> <xbox|akai|ps5> [only,these,frames]
# VIEW=34 in the environment renders every frame, light overlay and hotspot from a 3/4 view
# instead (files named <prefix>-34-…): from behind the triggers and bumpers when the model has
# them, so they face the camera, else from the front.
import bpy, sys, os, json, math
from mathutils import Vector
from bpy_extras.object_utils import world_to_camera_view

ARGS = sys.argv[sys.argv.index("--") + 1:]
SRC, OUTDIR, PREFIX, KIND = ARGS[:4]
ONLY = set(ARGS[4].split(",")) if len(ARGS) > 4 else None  # frames to render; spots are always written
want = lambda name: ONLY is None or name in ONLY
VIEW = os.environ.get("VIEW", "top")
if VIEW not in ("top", "34"):
    raise SystemExit(f"VIEW must be top or 34, not {VIEW}")
MM = 0.001
for o in list(bpy.data.objects):
    bpy.data.objects.remove(o)
bpy.ops.import_scene.gltf(filepath=SRC)
meshes = {o.name: o for o in bpy.context.scene.objects if o.type == "MESH"}
for o in meshes.values():
    o.rotation_mode = "XYZ"  # glTF imports quaternions; the presses below set Euler angles
pivot = bpy.data.objects.new("pivot", None)
bpy.context.collection.objects.link(pivot)
for o in bpy.context.scene.objects:
    if o.parent is None and o is not pivot:
        o.parent = pivot
bpy.context.view_layer.update()
pts = [o.matrix_world @ Vector(c) for o in meshes.values() for c in o.bound_box]
lo = Vector([min(p[i] for p in pts) for i in range(3)])
hi = Vector([max(p[i] for p in pts) for i in range(3)])
s = 0.125 / max(hi - lo)
pivot.scale = (s, s, s)
pivot.location = -((lo + hi) / 2) * s
bpy.context.view_layer.update()

target = bpy.data.objects.new("target", None)
bpy.context.collection.objects.link(target)
sc = bpy.context.scene
sc.render.engine = "CYCLES"
sc.cycles.samples = 32
sc.cycles.use_denoising = True
sc.render.film_transparent = True
sc.render.resolution_x, sc.render.resolution_y = 1000, 667
sc.render.image_settings.compression = 100
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


TAG = PREFIX if VIEW == "top" else f"{PREFIX}-34"


def shot(name):
    sc.render.filepath = os.path.join(OUTDIR, f"{TAG}-{name}.png")
    bpy.ops.render.render(write_still=True)


cam.location = (0.12, -0.3, 0.3)
if VIEW == "top":
    if want("34"):
        shot("34")
    cam.location = (0.0, -0.01, 0.42)
else:
    # Behind the shoulder controls, raised and turned 15° off-axis, so the triggers and
    # bumpers face the camera and the top still shows.
    back = [meshes[n].matrix_world.translation for n in ("trigger_left", "trigger_right", "bumper_left", "bumper_right") if n in meshes]
    if back:
        d = Vector((sum(v.x for v in back), sum(v.y for v in back), 0)).normalized()
        d.rotate(__import__("mathutils").Euler((0, 0, math.radians(15))))
        cam.location = (d.x * 0.27, d.y * 0.27, 0.17)
bpy.context.view_layer.update()


def emission(ob, color, strength, from_texture=False):
    """Give `ob` its own copies of its materials, glowing; returns the originals.
    from_texture: glow in the colours of its own base texture (the Xbox logo)."""
    saved = [slot.material for slot in ob.material_slots]
    for slot in ob.material_slots:
        m = slot.material.copy()
        tree = m.node_tree
        bsdf = [n for n in tree.nodes if n.type == "BSDF_PRINCIPLED"][0]
        for link in list(bsdf.inputs["Emission Color"].links):
            tree.links.remove(link)
        source = bsdf.inputs["Base Color"].links[0].from_socket if bsdf.inputs["Base Color"].is_linked else None
        if from_texture and source is not None:
            tree.links.new(source, bsdf.inputs["Emission Color"])
        else:
            bsdf.inputs["Emission Color"].default_value = (*color, 1)
        bsdf.inputs["Emission Strength"].default_value = strength
        slot.material = m
    return saved


# Rest pose: every light off.
for mat in bpy.data.materials:
    if mat.use_nodes and mat.name.startswith(("button_xbox_light", "pad_", "light_bar", "player_led", "button_mute_light")):
        for n in mat.node_tree.nodes:
            if n.type == "BSDF_PRINCIPLED":
                n.inputs["Emission Strength"].default_value = 0.0


ACCENT = (0.78, 0.96, 0.2)
DIRS = ["dpad_up", "dpad_down", "dpad_left", "dpad_right"]
if KIND == "xbox":
    controls = ["button_a", "button_b", "button_x", "button_y", "button_view", "button_menu", "button_share", "button_xbox"] + DIRS + \
        ["stick_left", "stick_right", "bumper_left", "bumper_right", "trigger_left", "trigger_right"]
elif KIND == "ps5":
    controls = ["button_triangle", "button_circle", "button_cross", "button_square"] + DIRS +         ["stick_left", "stick_right", "button_ps", "button_mute", "button_create", "button_options", "touchpad",
         "bumper_left", "bumper_right", "trigger_left", "trigger_right"]
else:
    controls = [f"key_{i:02d}" for i in range(25)] + [f"pad_{i}" for i in range(1, 9)] + [f"knob_{i}" for i in range(1, 9)] + \
        sorted(n for n in meshes if n.startswith("button_") and not n.endswith("_light")) + ["joystick"]


def part(name):
    """The object a control moves: the Xbox d-pad is one part rocking four ways."""
    return meshes.get(name) or meshes["dpad"]


def press(name):
    ob = part(name)
    if name.startswith("dpad_") and ob.name == "dpad":
        d = name[5:]
        ob.rotation_euler.x = math.radians({"up": -6, "down": 6}.get(d, 0))
        ob.rotation_euler.y = math.radians({"right": 6, "left": -6}.get(d, 0))
    elif name == "touchpad":
        ob.location.z -= 1.0 * MM
    elif name.startswith(("button_", "pad_", "dpad_")):
        ob.location.z -= (2.0 if name.startswith("pad_") else 1.5) * MM
    elif name.startswith(("stick_", "joystick")):
        ob.rotation_euler.x = math.radians(-18)
        ob.rotation_euler.y = math.radians(10)
    elif name == "dpad":
        ob.rotation_euler.x = math.radians(-6)
    elif name.startswith("bumper_"):
        ob.rotation_euler.x = math.radians(-8)
    elif name.startswith("trigger_"):
        ob.rotation_euler.x = math.radians(-20)
    elif name.startswith("key_"):
        ob.rotation_euler.x = math.radians(4)
    elif name.startswith("knob_"):
        ob.rotation_euler.z = math.radians(60)
    if name == "button_xbox":
        return [(ob, emission(ob, (1, 1, 1), 6.0, from_texture=True))]
    if name.startswith("pad_"):
        light = meshes.get(name + "_light") or meshes[name + "_glow"]  # split model / our own
        return [(light, emission(light, (1.0, 0.05, 0.03), 12.0))]
    return [(ob, emission(ob, ACCENT, 1.4))]


spots = {}
if want("rest"):
    shot("rest")
for name in controls:
    ob = part(name)
    loc, rot = ob.location.copy(), ob.rotation_euler.copy()
    corners = [world_to_camera_view(sc, cam, ob.matrix_world @ Vector(c)) for c in ob.bound_box]
    x0, y0 = min(c.x for c in corners), 1 - max(c.y for c in corners)
    x1, y1 = max(c.x for c in corners), 1 - min(c.y for c in corners)
    if name.startswith("dpad_") and ob.name == "dpad":  # one arm of the cross: a third of it
        w, h = (x1 - x0) / 3, (y1 - y0) / 3
        col, row = {"up": (1, 0), "down": (1, 2), "left": (0, 1), "right": (2, 1)}[name[5:]]
        x0, y0, x1, y1 = x0 + col * w, y0 + row * h, x0 + (col + 1) * w, y0 + (row + 1) * h
    spots[name] = [round(x0, 4), round(y0, 4), round(x1, 4), round(y1, 4)]
    if not want(name):
        continue
    before = ob.matrix_world.copy()
    restore = press(name)
    bpy.context.view_layer.update()
    # A press that doesn't move its part is a bug (e.g. Euler angles set on a quaternion
    # object): stop before rendering or uploading anything wrong.
    if ob.matrix_world == before:
        raise SystemExit(f"PRESS DID NOT MOVE: {name} ({ob.name}, rotation mode {ob.rotation_mode})")
    shot(name)
    ob.location, ob.rotation_euler = loc, rot
    for o, mats in restore:
        for slot, m in zip(o.material_slots, mats):
            slot.material = m
json.dump(spots, open(os.path.join(OUTDIR, f"{TAG}-spots.json"), "w"))

# Light overlays: only the lit parts, everything else held out, to lay over any frame.
OVERLAYS = {
    "xbox": {"xbox": [("button_xbox", (1, 1, 1), 6.0, True)]},
    "akai": {"connected": [("display", (0.75, 0.88, 1.0), 4.0)]},
    "ps5": {"lightbar": [("light_bar", (0.0, 0.2, 1.0), 3.0)],
            "player": [("player_led_3", (1, 1, 1), 8.0)],
            "mute": [("button_mute", (1.0, 0.35, 0.0), 8.0)]},
}.get(KIND, {})
# Where each light sits on the image: one rectangle per separate piece (the light bar is two strips).
import bmesh
lightspots = {}
for oname, lit in OVERLAYS.items():
    rects = []
    for t in lit:
        ob = meshes[t[0]]
        b = bmesh.new()
        b.from_mesh(ob.data)
        b.verts.ensure_lookup_table()
        seen = set()
        for v in b.verts:
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
            pts = [world_to_camera_view(sc, cam, ob.matrix_world @ p) for p in isl]
            rects.append([round(min(p.x for p in pts), 4), round(1 - max(p.y for p in pts), 4),
                          round(max(p.x for p in pts), 4), round(1 - min(p.y for p in pts), 4)])
        b.free()
    # Tiny islands (screws, seams) don't need their own glow: keep the larger pieces.
    big = max((r[2] - r[0]) * (r[3] - r[1]) for r in rects)
    lightspots[oname] = [r for r in rects if (r[2] - r[0]) * (r[3] - r[1]) > big * 0.05]
json.dump(lightspots, open(os.path.join(OUTDIR, f"{TAG}-lightspots.json"), "w"))
for oname, lit in OVERLAYS.items():
    if not want("overlay-" + oname) and not want("overlays"):
        continue
    names_lit = {t[0] for t in lit}
    for o in meshes.values():
        o.hide_render = o.name not in names_lit  # lights sit on top; nothing needs to hide them
    restore = [(meshes[t[0]], emission(meshes[t[0]], t[1], t[2], *t[3:])) for t in lit]
    shot("light-" + oname)
    for o, mats in restore:
        for slot, m in zip(o.material_slots, mats):
            slot.material = m
    for o in meshes.values():
        o.hide_render = False
print("SPOTS", len(spots))
