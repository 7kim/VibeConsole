"""An emulated Xbox controller for agent tests (v7.8 ticket 02). Standard library only.

    from xbox import Xbox
    with Xbox() as xbox:
        xbox.press("A"); xbox.release("A")   # or xbox.tap("A")
        xbox.stick("left", 0.0, 1.0)         # x, y from -1.0 to 1.0; y = 1.0 is up
        xbox.trigger("RT", 0.5)              # 0.0 to 1.0
        xbox.dpad("up"); xbox.dpad(None)
        xbox.guide()

    with Xbox(log=run_dir / "output.log"):   # v7.8 ticket 79: each rumble played is logged
        ...                                  # "rumble strong 40000 weak 0 300 ms"

A uinput device named `VibeConsole emulator · Xbox Controller` with the codes, ranges and
ids of the user's controller under xone, as /proc/bus/input/devices and EVIOCGABS read it
(25 Nov 2026): bus 0x06, 045e:0b12; BTN_A/B/X/Y, TL/TR, SELECT/START/MODE, THUMBL/R, KEY_RECORD
(Share); sticks ABS_X/Y/RX/RY -32768..32767 (fuzz 16, flat 128), triggers ABS_Z/RZ 0..1023,
D-pad ABS_HAT0X/Y -1..1. `Xbox("bluetooth")` is the same controller over Bluetooth under
xpadneo 0.10.2 (v7.8 ticket 04), as its source and the kernel log read it: bus 0x05, 045e:028e
version 0x1130 (xpadneo's ids, so gilrs picks SDL's "Xbox One Elite 2" line), no Share key,
sticks fuzz 32 flat 3072, triggers fuzz 4, and ABS_PROFILE 0..3.

Force feedback (v7.8 ticket 79) as the real controller reports it (B: FF=, 26 Nov 2026): rumble,
periodic, square, triangle, sine, gain; 16 effect slots, as ff-memless. A thread answers every
upload and erase (the uploader waits up to 30 s for the answer: the kernel's uinput.c), and every
gilrs that opens the device uploads and plays its rumble state each 50 ms tick, zero included
(gilrs 0.11.2 ff/server.rs). A rumble is a run of non-zero plays on one effect: from its first
play to the play, stop or erase that ends it.

Every program on the desktop sees a uinput device. A VibeConsole built before commit 7d505be
doesn't skip emulators and fires the user's real shortcuts from it (it happened once, 25 Nov
2026), so the device is refused while any running VibeConsole was built earlier (or from no
known commit, e.g. a debug build).
"""
import fcntl
import os
from pathlib import Path
import select
import struct
import subprocess
import threading
import time

NAME = "VibeConsole emulator · Xbox Controller"
BUS, VENDOR, PRODUCT, VERSION = 0x06, 0x045E, 0x0B12, 0x0509
BLUETOOTH = (0x05, 0x045E, 0x028E, 0x1130)  # xpadneo's ids (v7.8 ticket 04)
ABS_PROFILE = 0x21
EV_SYN, EV_KEY, EV_ABS, EV_FF, EV_UINPUT = 0, 1, 3, 0x15, 0x0101
UI_FF_UPLOAD, UI_FF_ERASE = 1, 2
FF_BITS = (0x50, 0x51, 0x58, 0x59, 0x5A, 0x60)  # rumble, periodic, square, triangle, sine, gain
FF_EFFECTS = 16
EVENT = struct.Struct("llHHi")
BUTTONS = {
    "A": 0x130, "B": 0x131, "X": 0x133, "Y": 0x134, "LB": 0x136, "RB": 0x137,
    "View": 0x13A, "Menu": 0x13B, "Xbox": 0x13C, "LS": 0x13D, "RS": 0x13E, "Share": 167,
}
ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ, ABS_HAT0X, ABS_HAT0Y = 0, 1, 2, 3, 4, 5, 16, 17
STICKS = {"left": (ABS_X, ABS_Y), "right": (ABS_RX, ABS_RY)}
TRIGGERS = {"LT": ABS_Z, "RT": ABS_RZ}
DPAD = {None: (0, 0), "up": (0, -1), "down": (0, 1), "left": (-1, 0), "right": (1, 0)}


def _iow(nr, size):
    return (1 << 30) | (size << 16) | (ord("U") << 8) | nr


UI_SET_EVBIT, UI_SET_KEYBIT, UI_SET_ABSBIT = _iow(100, 4), _iow(101, 4), _iow(103, 4)
UI_DEV_SETUP, UI_ABS_SETUP = _iow(3, 92), _iow(4, 28)
UI_DEV_CREATE, UI_DEV_DESTROY = 0x5501, 0x5502
UI_GET_SYSNAME = (2 << 30) | (64 << 16) | (ord("U") << 8) | 44
UI_SET_FFBIT = _iow(107, 4)
# struct uinput_ff_upload (request_id, retval, ff_effect effect, old: 104 bytes on x86-64) and
# uinput_ff_erase (request_id, retval, effect_id), linux/uinput.h.
UPLOAD, ERASE = 104, 12
UI_BEGIN_FF_UPLOAD, UI_END_FF_UPLOAD = (3 << 30) | _iow(200, UPLOAD), _iow(201, UPLOAD)
UI_BEGIN_FF_ERASE, UI_END_FF_ERASE = (3 << 30) | _iow(202, ERASE), _iow(203, ERASE)


# The first commit whose VibeConsole skips an emulated gamepad (by its kernel name: gilrs's
# name() is the SDL mapping's for these ids, which ticket 01's filter read).
SKIPS_EMULATORS = "7d505be"
REPO = Path(__file__).resolve().parents[2]


def running_builds():
    """(pid, binary, its --version) of each running VibeConsole but emulated runs (they read
    only emulators and send nothing to the desktop)."""
    pids = subprocess.run(["pgrep", "-x", "vibeconsole"], capture_output=True, text=True).stdout.split()
    for pid in pids:
        if b"--emulated" in Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0"):
            continue
        exe = os.readlink(f"/proc/{pid}/exe")
        replaced = exe.endswith(" (deleted)")
        yield pid, exe, "" if replaced else subprocess.run([exe, "--version"], capture_output=True, text=True).stdout


def refuse_unless_safe(running=None):
    """Every running VibeConsole must skip emulators: its binary is the one on disk (not a
    replaced older copy) and was built from a commit that contains SKIPS_EMULATORS.
    `running`: (pid, binary, --version) triples instead of the live ones (the self-check)."""
    def refuse(why):
        raise SystemExit(f"{why}; it would fire real shortcuts from the emulator. Run mise run install "
                         "first. Nothing was created.")
    for pid, exe, version in running_builds() if running is None else running:
        if exe.endswith(" (deleted)"):
            refuse(f"VibeConsole pid {pid} runs a replaced binary ({exe})")
        # A build from a tree with edits reads "commit abc1234+changes": its base commit is what
        # git can check (the edits are the user's own, outside the filter's file).
        commit, dirty, _ = version.rpartition("commit ")[2].strip(" )\n").partition("+")
        built = commit and subprocess.run(
            ["git", "-C", str(REPO), "merge-base", "--is-ancestor", SKIPS_EMULATORS, commit],
            capture_output=True).returncode == 0
        if built:
            continue
        if dirty:  # v7.8 ticket 54: said apart from an older clean build
            refuse(f"VibeConsole pid {pid} ({exe}, {version.strip()}) is a build with uncommitted changes "
                   f"on {commit}, which isn't {SKIPS_EMULATORS} or later, so its emulator filter can't be "
                   "verified")
        refuse(f"VibeConsole pid {pid} ({exe}, {version.strip()}) isn't built from {SKIPS_EMULATORS} or later")


class Xbox:
    def __init__(self, link="usb", running=None, log=None):
        """`link`: "usb" (xone, the default) or "bluetooth" (xpadneo); `running`: see
        refuse_unless_safe; `log`: the file each rumble played is appended to."""
        bluetooth = link == "bluetooth"
        refuse_unless_safe(running)
        self.log = log
        self.fd = os.open("/dev/uinput", os.O_RDWR | os.O_NONBLOCK)
        try:
            for ev in (EV_KEY, EV_ABS, EV_FF):
                fcntl.ioctl(self.fd, UI_SET_EVBIT, ev)
            for bit in FF_BITS:
                fcntl.ioctl(self.fd, UI_SET_FFBIT, bit)
            for button, code in BUTTONS.items():
                if not (bluetooth and button == "Share"):  # xpadneo clears BTN_SHARE
                    fcntl.ioctl(self.fd, UI_SET_KEYBIT, code)
            stick = (-32768, 32767, 32, 3072) if bluetooth else (-32768, 32767, 16, 128)
            trigger = (0, 1023, 4, 0) if bluetooth else (0, 1023, 0, 0)
            ranges = {code: stick for code in (ABS_X, ABS_Y, ABS_RX, ABS_RY)}
            ranges |= {ABS_Z: trigger, ABS_RZ: trigger}
            ranges |= {ABS_HAT0X: (-1, 1, 0, 0), ABS_HAT0Y: (-1, 1, 0, 0)}
            if bluetooth:
                ranges[ABS_PROFILE] = (0, 3, 0, 0)
            for code, (low, high, fuzz, flat) in ranges.items():
                fcntl.ioctl(self.fd, UI_SET_ABSBIT, code)
                fcntl.ioctl(self.fd, UI_ABS_SETUP, struct.pack("HH6i", code, 0, 0, low, high, fuzz, flat, 0))
            name = NAME.encode()
            ids = BLUETOOTH if bluetooth else (BUS, VENDOR, PRODUCT, VERSION)
            fcntl.ioctl(self.fd, UI_DEV_SETUP, struct.pack("4H80sI", *ids, name, FF_EFFECTS))
            fcntl.ioctl(self.fd, UI_DEV_CREATE)
        except BaseException:
            os.close(self.fd)
            raise
        sysname = bytearray(64)
        fcntl.ioctl(self.fd, UI_GET_SYSNAME, sysname)
        self._stop = False
        self._feedback = threading.Thread(target=self._answer, daemon=True)
        self._feedback.start()
        input_dir = Path("/sys/devices/virtual/input") / sysname.split(b"\0")[0].decode()
        deadline = time.monotonic() + 3
        while not list(input_dir.glob("event*")) and time.monotonic() < deadline:
            time.sleep(0.02)
        event = next(input_dir.glob("event*"))
        self.node = Path("/dev/input") / event.name
        # v7.8 ticket 68: ready only once udev has finished with it, as a controller plugged in
        # before the run. The node exists ~30 ms before udev tags it ID_INPUT_JOYSTICK and gives
        # the user access; gilrs lists only tagged nodes, and a run started in that gap never
        # saw the emulator.
        record = Path("/run/udev/data") / f"c{(event / 'dev').read_text().strip()}"
        while time.monotonic() < deadline:
            try:
                if "E:ID_INPUT_JOYSTICK=1" in record.read_text().splitlines():
                    os.close(os.open(self.node, os.O_RDWR | os.O_NONBLOCK))
                    return
            except OSError:
                pass
            time.sleep(0.01)
        self.close()
        raise TimeoutError(f"udev didn't finish with the emulator's {self.node} within 3 s")

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def close(self):
        if self.fd is not None:
            self._stop = True
            self._feedback.join()
            fcntl.ioctl(self.fd, UI_DEV_DESTROY)
            os.close(self.fd)
            self.fd = None

    def _answer(self):
        """Answer each force-feedback request and log each rumble (v7.8 ticket 79)."""
        effects, playing = {}, {}  # effect id: (strong, weak); id: (since, strong, weak)

        def end(effect):
            if effect in playing:
                since, strong, weak = playing.pop(effect)
                if self.log:
                    with open(self.log, "a") as log:
                        log.write(f"rumble strong {strong} weak {weak} {round((time.monotonic() - since) * 1000)} ms\n")

        poll = select.poll()
        poll.register(self.fd, select.POLLIN)
        while not self._stop:
            if not poll.poll(50):
                continue
            try:
                data = os.read(self.fd, EVENT.size * 64)
            except BlockingIOError:
                continue
            for *_, kind, code, value in EVENT.iter_unpack(data):
                if (kind, code) == (EV_UINPUT, UI_FF_UPLOAD):
                    request = bytearray(UPLOAD)
                    struct.pack_into("I", request, 0, value)
                    fcntl.ioctl(self.fd, UI_BEGIN_FF_UPLOAD, request)
                    effect, = struct.unpack_from("h", request, 10)  # effect.id
                    effects[effect] = struct.unpack_from("HH", request, 24)  # effect.u.rumble
                    struct.pack_into("i", request, 4, 0)
                    fcntl.ioctl(self.fd, UI_END_FF_UPLOAD, bytes(request))
                elif (kind, code) == (EV_UINPUT, UI_FF_ERASE):
                    request = bytearray(ERASE)
                    struct.pack_into("I", request, 0, value)
                    fcntl.ioctl(self.fd, UI_BEGIN_FF_ERASE, request)
                    effect, = struct.unpack_from("I", request, 8)
                    end(effect)
                    effects.pop(effect, None)
                    struct.pack_into("i", request, 4, 0)
                    fcntl.ioctl(self.fd, UI_END_FF_ERASE, bytes(request))
                elif kind == EV_FF and code < FF_EFFECTS:  # play (value 1) or stop (0); not gain
                    strong, weak = effects.get(code, (0, 0))
                    if not value or not (strong or weak):
                        end(code)
                    elif code not in playing:
                        playing[code] = (time.monotonic(), strong, weak)

    def _emit(self, *events):
        now = time.time()
        sec, usec = int(now), int((now % 1) * 1_000_000)
        data = b"".join(struct.pack("llHHi", sec, usec, t, c, v) for t, c, v in events)
        os.write(self.fd, data + struct.pack("llHHi", sec, usec, EV_SYN, 0, 0))

    def press(self, button):
        """Hold a button: A B X Y LB RB View Menu Xbox LS RS Share."""
        self._emit((EV_KEY, BUTTONS[button], 1))

    def release(self, button):
        self._emit((EV_KEY, BUTTONS[button], 0))

    def tap(self, button, hold=0.05):
        self.press(button)
        time.sleep(hold)
        self.release(button)

    def guide(self, hold=0.05):
        """The Xbox button."""
        self.tap("Xbox", hold)

    def stick(self, side, x, y):
        """A stick's position, each axis -1.0 to 1.0; y = 1.0 is up (evdev's Y grows down)."""
        if not (-1 <= x <= 1 and -1 <= y <= 1):
            raise ValueError("stick axes are -1.0 to 1.0")
        ax, ay = STICKS[side]
        self._emit((EV_ABS, ax, round(x * (32767 if x > 0 else 32768))),
                   (EV_ABS, ay, round(-y * (32768 if y > 0 else 32767))))

    def trigger(self, side, pull):
        """LT or RT pulled from 0.0 (rest) to 1.0 (full)."""
        if not 0 <= pull <= 1:
            raise ValueError("trigger pull is 0.0 to 1.0")
        self._emit((EV_ABS, TRIGGERS[side], round(pull * 1023)))

    def dpad(self, direction):
        """up, down, left, right, or None for released."""
        x, y = DPAD[direction]
        self._emit((EV_ABS, ABS_HAT0X, x), (EV_ABS, ABS_HAT0Y, y))


if __name__ == "__main__":
    # v7.8 ticket 54's self-check (python3 scripts/emulate/xbox.py): no device is made. A
    # dirty build and a clean one, both on a commit before SKIPS_EMULATORS, are refused with
    # their own messages before /dev/uinput is opened.
    old = subprocess.run(["git", "-C", str(REPO), "rev-parse", "--short", f"{SKIPS_EMULATORS}~1"],
                         capture_output=True, text=True, check=True).stdout.strip()
    opened = []
    os.open = lambda *args, **kwargs: opened.append(args)  # nothing may be opened
    for suffix, says in [("+changes", "uncommitted changes"), ("", f"isn't built from {SKIPS_EMULATORS}")]:
        try:
            Xbox(running=[("1", "/usr/bin/vibeconsole", f"vibeconsole 7.8.0 (commit {old}{suffix})\n")])
            raise AssertionError("not refused")
        except SystemExit as refused:
            assert says in str(refused), refused
            assert ("uncommitted" in str(refused)) == bool(suffix), refused
    assert not opened, opened
    print("ok: a dirty old build and a clean old one are refused, each with its own message, before any device")
