# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Read-only capture of an Xbox controller's evdev reports (v7.8 ticket 04). Sends nothing to
the device and never grabs it: the login service keeps reading it as usual.

    uv run scripts/capture-xbox.py [seconds]   (default 20)

Prints the controller's ids, every axis with its range, and each event with its code name while
you press LT, RT and move the right stick. Paste the output into ticket 04.
"""
import fcntl
import os
from pathlib import Path
import select
import struct
import sys
import time

ABS = {0: "ABS_X", 1: "ABS_Y", 2: "ABS_Z", 3: "ABS_RX", 4: "ABS_RY", 5: "ABS_RZ", 6: "ABS_THROTTLE",
       7: "ABS_RUDDER", 8: "ABS_WHEEL", 9: "ABS_GAS", 10: "ABS_BRAKE", 16: "ABS_HAT0X", 17: "ABS_HAT0Y",
       0x21: "ABS_PROFILE", 0x28: "ABS_MISC"}
KEYS = {0x130: "BTN_A", 0x131: "BTN_B", 0x133: "BTN_X", 0x134: "BTN_Y", 0x136: "BTN_TL", 0x137: "BTN_TR",
        0x13A: "BTN_SELECT", 0x13B: "BTN_START", 0x13C: "BTN_MODE", 0x13D: "BTN_THUMBL", 0x13E: "BTN_THUMBR"}


def controllers():
    for block in Path("/proc/bus/input/devices").read_text().split("\n\n"):
        lines = dict(line.split(": ", 1) for line in block.splitlines() if ": " in line)
        name = lines.get("N", "")
        if ("xbox" in name.lower() or "045e" in lines.get("I", "").lower()) and "emulator" not in name.lower():
            event = next((h for h in lines.get("H", "").split() if h.startswith("event")), None)
            if event:
                yield lines.get("I", ""), name, f"/dev/input/{event}"


def main():
    seconds = float(sys.argv[1]) if len(sys.argv) > 1 else 20
    found = list(controllers())
    if not found:
        raise SystemExit("No Xbox controller in /proc/bus/input/devices; connect it and try again.")
    fds = {}
    for ids, name, node in found:
        print(f"{node}: {name} | {ids}")
        fd = os.open(node, os.O_RDONLY | os.O_NONBLOCK)
        bits = bytearray(8)
        fcntl.ioctl(fd, 0x80084523, bits)  # EVIOCGBIT(EV_ABS, 8)
        for code in range(64):
            if bits[code // 8] >> (code % 8) & 1:
                info = bytearray(24)
                fcntl.ioctl(fd, 0x80184540 + code, info)  # EVIOCGABS(code)
                value, low, high, fuzz, flat, _ = struct.unpack("6i", info)
                print(f"  axis {ABS.get(code, hex(code))}: {low}..{high} (fuzz {fuzz}, flat {flat}, now {value})")
        fds[fd] = node
    print(f"Capturing {seconds:.0f} s: pull LT, then RT, then move the right stick around.")
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        for fd in select.select(list(fds), [], [], 0.2)[0]:
            data = os.read(fd, 24 * 64)
            for i in range(0, len(data), 24):
                _, _, kind, code, value = struct.unpack("llHHi", data[i:i + 24])
                if kind == 3:
                    print(f"  {fds[fd]} {ABS.get(code, hex(code))} {value}")
                elif kind == 1:
                    print(f"  {fds[fd]} {KEYS.get(code, hex(code))} {value}")
    for fd in fds:
        os.close(fd)


if __name__ == "__main__":
    main()
