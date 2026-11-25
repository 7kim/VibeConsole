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
    """bwrap: no input device but `nodes` (an emulator's own), and no journal."""
    binds = [arg for node in nodes for arg in ("--dev-bind", str(node), str(node))]
    return ["bwrap", "--dev-bind", "/", "/", "--tmpfs", "/dev/input", *binds, "--tmpfs", "/run/systemd/journal"]


def mapping(context, label, message, channel, id, keys, behavior="hold"):
    return {"context": context, "label": label, "input": "pad", "message": message, "channel": channel,
            "id": id, "direction": 0, "behavior": behavior, "keys": keys, "octave": None, "tempo": None,
            "arp": None, "action": "shortcut", "risky": False, "status": False}


def screen(data):
    return ANSI.sub(b" ", data).decode(errors="replace")


class Run:
    def __init__(self, directory, mappings, sandbox, command=()):
        assert BINARY.exists(), "build first: cargo build --locked"
        self.dir = Path(directory)
        config = self.dir / ".config/vibeconsole"
        config.mkdir(parents=True)
        (config / "mappings.json").write_text(json.dumps({
            "version": 1, "vars": [], "profiles": [], "lock": None, "favourites": [], "sequences": [],
            "mappings": mappings}))
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 110, 0, 0))
        self.child = subprocess.Popen([*sandbox, BINARY, "--emulated", directory, *command], stdin=self.slave,
                                      stdout=self.slave, stderr=self.slave,
                                      env=dict(os.environ, TERM="xterm-256color"))
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
        self.child.send_signal(signal.SIGINT)
        try:
            self.child.wait(5)
        except subprocess.TimeoutExpired:
            self.child.kill()
            raise
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
                          env=dict(os.environ, NO_COLOR="1")).stdout
