# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""v7.8 ticket 01: the real binary, run with --emulated, against the emulated Akai.
Run after cargo build --locked: uv run scripts/emulate/check-akai.py

Checks: a pad mapped to F1 shows on Live and output.log records F1 down and up; the program
query verifies against the emulator's stored program; the login service is left as it was;
a normal (not emulated) run never picks the emulator's port. Nothing reaches the desktop:
keys go to output.log, external programs to tools.log, and /dev/input is hidden from the run.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from akai import Akai  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / "target/debug/vibeconsole"
if not shutil.which("bwrap"):
    raise SystemExit("bwrap not found: install it (sudo dnf install bubblewrap); nothing was run.")
# The emulated run opens no input device itself; bwrap hides them anyway, and the journal.
HIDE = ["bwrap", "--dev-bind", "/", "/", "--tmpfs", "/dev/input", "--tmpfs", "/run/systemd/journal"]
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b[()][A-Z0-9]")


def screen(data):
    return ANSI.sub(b" ", data).decode(errors="replace")


def read_until(master, token, seconds=8, data=b""):
    deadline = time.monotonic() + seconds
    while token not in screen(data) and time.monotonic() < deadline:
        if select.select([master], [], [], 0.1)[0]:
            try:
                data += os.read(master, 65536)
            except OSError:
                break
    assert token in screen(data), (token, screen(data)[-1500:])
    return data


def service():
    """The login service's state, read only."""
    active = subprocess.run(["systemctl", "--user", "is-active", "vibeconsole.service"],
                            capture_output=True, text=True).stdout.strip()
    runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    state = Path(runtime) / "vibeconsole/run-state"
    return active, state.read_text() if state.exists() else None


def main():
    assert BINARY.exists(), "build first: cargo build --locked"
    before = service()
    with tempfile.TemporaryDirectory() as directory, Akai() as akai:
        dir = Path(directory)
        akai.program(1)
        channel, note = akai.ram[0x10] + 1, akai.ram[0x24]
        config = dir / ".config/vibeconsole"
        config.mkdir(parents=True)
        (config / "mappings.json").write_text(json.dumps({"version": 1, "vars": [], "profiles": [], "lock": None, "favourites": [],
                                                       "sequences": [], "mappings": [{
            "context": 0, "label": "Bank A Pad 1", "input": "pad", "message": "note",
            "channel": channel, "id": note, "direction": 0, "behavior": "hold", "keys": "F1",
            "octave": None, "tempo": None, "arp": None, "action": "shortcut", "risky": False,
            "status": False}]}))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 110, 0, 0))
        child = subprocess.Popen([*HIDE, BINARY, "--emulated", directory], stdin=slave, stdout=slave,
                                 stderr=slave, env=dict(os.environ, TERM="xterm-256color"))
        log = dir / "output.log"
        try:
            # The run detects the program, then fires on the next press.
            data = read_until(master, "Pads fire on their next press", 15)
            queries = [f for f in akai.received if f[4] == 0x66]
            assert queries, "VibeConsole never queried the program"
            assert "Program 1" in screen(data), screen(data)[-1500:]
            print(f"ok: program verified against the emulator ({len(queries)} program reads)")
            akai.pad(1, hold=0.5)
            read_until(master, "◆Pad 1 F1", 5)  # Live's controller map: Pad 1 held
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and "key KEY_F1 up" not in (log.read_text() if log.exists() else ""):
                time.sleep(0.05)
            keys = log.read_text().splitlines()
            assert keys[:2] == ["key KEY_F1 down", "key KEY_F1 up"], keys
            print("ok: akai.pad(1) shows on Live and output.log records F1 down and up")
            # A normal run never picks the emulator's port (read-only: describe opens nothing).
            described = subprocess.run([*HIDE, BINARY, "describe"], capture_output=True, text=True,
                                       env=dict(os.environ, NO_COLOR="1")).stdout
            assert "emulator" not in described.lower() and "multiple" not in described.lower(), described
            print("ok: a normal run ignores the emulator's port")
        finally:
            child.send_signal(signal.SIGINT)
            try:
                child.wait(5)
            except subprocess.TimeoutExpired:
                child.kill()
                raise
            os.close(master)
            os.close(slave)
        tools = dir / "tools.log"
        print(f"tools recorded instead of run: {len(tools.read_text().splitlines()) if tools.exists() else 0}")
    after = service()
    assert after == before, (before, after)
    print(f"ok: the login service is unchanged ({after[0]})")


if __name__ == "__main__":
    main()
