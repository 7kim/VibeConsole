"""Check the built-in device models against docs/v7/spec/3d-device-models.md.

Run: python3 assets/devices/check_models.py [dir]   (default: this folder)
Fails (exit 1) when a named node is missing, a file is 8 MB or more, or a texture is over 2K.
"""
import json
import os
import struct
import sys

MPK = (["body", "joystick", "display"] + [f"key_{i:02}" for i in range(25)]
       + [f"pad_{i}" for i in range(1, 9)] + [f"pad_{i}_glow" for i in range(1, 9)]
       + [f"knob_{i}" for i in range(1, 9)]
       + [f"button_{b}" for b in ("arp_on_off", "tap_tempo", "octave_down", "octave_up", "full_level",
                                   "note_repeat", "bank_a_b", "cc", "prog_change", "prog_select")])
XBOX = ["body", "stick_left", "stick_right", "dpad", "button_a", "button_b", "button_x", "button_y",
        "button_view", "button_menu", "button_share", "button_xbox", "bumper_left", "bumper_right",
        "trigger_left", "trigger_right", "port_usb_c", "port_headset"]
MODELS = {"mpk-mini-mk3.glb": (MPK, ["pad_1_light", "pad_8_light"]),
          "xbox-series-robot-white.glb": (XBOX, ["button_xbox_light"])}


def glb(path):
    data = open(path, "rb").read()
    size = struct.unpack_from("<I", data, 12)[0]
    doc = json.loads(data[20:20 + size])
    binary = data[20 + size + 8:]
    return len(data), doc, binary


def problems(path, nodes, materials):
    size, doc, binary = glb(path)
    names = {n.get("name") for n in doc.get("nodes", [])}
    out = [f"missing node {n}" for n in nodes if n not in names]
    mats = {m.get("name") for m in doc.get("materials", [])}
    out += [f"missing material {m}" for m in materials if m not in mats]
    if size >= 8 * 1024 * 1024:
        out.append(f"{size} bytes, not under 8 MB")
    for img in doc.get("images", []):
        view = doc["bufferViews"][img["bufferView"]]
        head = binary[view.get("byteOffset", 0):][:24]
        if head[:8] == b"\x89PNG\r\n\x1a\n":
            w, h = struct.unpack(">II", head[16:24])
            if max(w, h) > 2048:
                out.append(f"texture {img.get('name')} is {w}x{h}, over 2K")
    return out, size, len(names)


def main():
    folder = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
    failed = False
    for file, (nodes, materials) in MODELS.items():
        if not os.path.exists(os.path.join(folder, file)):
            print(f"{file}: FAIL, not found")
            failed = True
            continue
        bad, size, count = problems(os.path.join(folder, file), nodes, materials)
        print(f"{file}: {size / 1e6:.2f} MB, {count} nodes, {'OK' if not bad else 'FAIL'}")
        for b in bad:
            print("  " + b)
        failed |= bool(bad)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
