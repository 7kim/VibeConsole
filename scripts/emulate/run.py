"""The emulated run, for the emulator checks (v7.8 tickets 01, 02): start the real binary in a
PTY with `--emulated <dir>`, read its screen, read what it recorded, stop it."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import signal
import struct
import subprocess
import termios
import threading
import time

BINARY = Path(__file__).resolve().parents[2] / "target/debug/vibeconsole"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b[()][A-Z0-9]")


def hide(*nodes):
    """bwrap: no input device but `nodes` (an emulator's own), no journal, and no system or
    session bus: the emulated run records every program instead of running it, and should one
    ever slip through (systemctl suspend, gsettings), it reaches nothing (v7.8 ticket 12)."""
    binds = [arg for node in nodes for arg in ("--dev-bind", str(node), str(node))]
    bus = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}") + "/bus"
    return ["bwrap", "--dev-bind", "/", "/", "--tmpfs", "/dev/input", *binds, "--tmpfs", "/run/systemd/journal",
            "--tmpfs", "/run/dbus", "--ro-bind", "/dev/null", bus]


def mapping(context, label, message, channel, id, keys, behavior="hold", action="shortcut"):
    """One mappings.json entry: a shortcut (`keys`), or another action word with no keys."""
    return {"context": context, "label": label, "input": "pad", "message": message, "channel": channel,
            "id": id, "direction": 0, "behavior": behavior, "keys": keys, "octave": None, "tempo": None,
            "arp": None, "action": action, "risky": False, "status": False}


def screen(data):
    return ANSI.sub(b" ", data).decode(errors="replace")


def without_bus(**extra):
    env = {k: v for k, v in os.environ.items() if k != "DBUS_SESSION_BUS_ADDRESS"}
    return env | extra


class Run:
    def __init__(self, directory, mappings, sandbox, command=(), tuning=None, devices=None):
        assert BINARY.exists(), "build first: cargo build --locked"
        self.dir = Path(directory)
        config = self.dir / ".config/vibeconsole"
        config.mkdir(parents=True)
        (config / "mappings.json").write_text(json.dumps({
            "version": 1, "vars": [], "profiles": [], "lock": None, "favourites": [], "sequences": [],
            "mappings": mappings}))
        if tuning or devices:  # the shipped devices.json with these values (v7.8 tickets 23, 51, 53)
            shipped = json.loads((Path(__file__).resolve().parents[2] / "core/defaults/devices.json").read_text())
            shipped["tuning"] |= tuning or {}
            devices = shipped | (devices or {})
            (config / "devices.json").write_text(json.dumps(devices))
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 110, 0, 0))
        self.child = subprocess.Popen([*sandbox, BINARY, "--emulated", directory, *command], stdin=self.slave,
                                      stdout=self.slave, stderr=self.slave,
                                      env=without_bus(TERM="xterm-256color"), start_new_session=True)
        self.data = b""
        # Read the screen all the time: a full PTY blocks the app's next draw (and its loop).
        self._lock = threading.Lock()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        while True:
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                return
            if not chunk:
                return
            with self._lock:
                self.data += chunk

    def __enter__(self):
        return self

    def __exit__(self, *_):
        # v7.8 ticket 65: bwrap dies on SIGINT without passing it on, which left the app (an
        # emulated detect) running. The run has its own process group (bwrap and the app, only
        # what this Run started), so SIGINT goes to both: the app quits cleanly, releasing keys.
        group = self.child.pid
        try:
            os.killpg(group, signal.SIGINT)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                self.child.poll()  # reap bwrap, or the group never empties
                os.killpg(group, 0)
                time.sleep(0.05)
            os.killpg(group, signal.SIGKILL)
            self.child.wait()  # reap bwrap: no zombie left behind
            raise TimeoutError(f"the emulated run didn't quit within 5 s of SIGINT: {self.child.args}")
        except ProcessLookupError:
            pass  # the group is empty: everything it started has quit
        finally:
            os.close(self.slave)
            time.sleep(0.1)
            os.close(self.master)

    def mark(self):
        """Where the screen is now, for `read_until(since=…)`."""
        return len(self.data)

    def read_until(self, token, seconds=8, since=0):
        """Wait for `token` on screen (only in what's drawn after the mark `since`)."""
        flat = lambda text: " ".join(text.split())  # a screen's spacing is layout, not content
        deadline = time.monotonic() + seconds
        while flat(token) not in flat(screen(self.data[since:])) and time.monotonic() < deadline:
            time.sleep(0.05)
        assert flat(token) in flat(screen(self.data[since:])), (token, flat(screen(self.data[since:]))[-1500:])

    def xbox_ready(self, xbox, seconds=15):
        """Tap A until the run records its Enter (the built-in default): the controller is open
        and startup is over. Not the "Xbox controller connected" notice: under load gilrs's
        Connected event is drained with startup's transition and never drawn (v7.8 ticket 68)."""
        deadline = time.monotonic() + seconds
        while "key KEY_ENTER up" not in self.recorded():
            assert time.monotonic() < deadline, ("the run never read the Xbox emulator", screen(self.data)[-1500:])
            xbox.tap("A")
            ends = time.monotonic() + 1
            while "key KEY_ENTER up" not in self.recorded() and time.monotonic() < ends:
                time.sleep(0.05)

    def recorded(self, file="output.log"):
        path = self.dir / file
        return path.read_text().splitlines() if path.exists() else []

    def wait_recorded(self, line, seconds=5):
        deadline = time.monotonic() + seconds
        while line not in self.recorded() and time.monotonic() < deadline:
            time.sleep(0.05)
        assert line in self.recorded(), (line, self.recorded())


def describe(sandbox):
    """A normal (not emulated) run's view of the devices; read-only, opens nothing."""
    return subprocess.run([*sandbox, BINARY, "describe"], capture_output=True, text=True,
                          env=without_bus(NO_COLOR="1")).stdout
