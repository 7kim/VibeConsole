# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Idle cost of a VibeConsole build: CPU and bytes written to the terminal in 10 s on its first
screen, with nothing pressed (v7.7 ticket 251, the method of ticket 225).
Run after a release build: uv run scripts/check-idle.py [binary ...]
(default target/release/vibeconsole; give two to compare a before and an after build).

Isolated like scripts/check-terminal.py: bwrap hides /dev/input and the journal and gives the
copy its own network namespace (the per-user lock, ticket 252); a fake lsusb keeps it off the
real Akai's MIDI, which /dev/input doesn't hide (18 Nov 2026: a run without it knocked the
login service's program to unverified); a fake systemctl; temporary XDG folders, so no real
config. Every process it starts is stopped; it checks that none is left.
"""
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import time
import fcntl
import termios

ROOT = Path(__file__).resolve().parents[1]
if not shutil.which("bwrap"):
    raise SystemExit("check-idle: bwrap is needed to hide /dev/input and the journal from the copy; nothing was run")


def descendants(pid):
    """Every process under pid, from /proc's children lists."""
    found, todo = [], [pid]
    while todo:
        current = todo.pop()
        try:
            tasks = os.listdir(f"/proc/{current}/task")
        except FileNotFoundError:
            continue
        for task in tasks:
            try:
                children = Path(f"/proc/{current}/task/{task}/children").read_text().split()
            except FileNotFoundError:
                continue
            found += [int(c) for c in children]
            todo += [int(c) for c in children]
    return found


def measure(binary):
    with tempfile.TemporaryDirectory() as directory:
        bin_dir = Path(directory) / "bin"
        bin_dir.mkdir()
        for fake in ("lsusb", "systemctl"):
            (bin_dir / fake).write_text("#!/bin/sh\nexit 1\n")
            (bin_dir / fake).chmod(0o755)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
        env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"], XDG_CONFIG_HOME=directory,
                   XDG_RUNTIME_DIR=directory, XDG_DATA_HOME=directory, XDG_DATA_DIRS=directory,
                   XDG_STATE_HOME=directory, TERM="xterm-256color")
        child = subprocess.Popen(
            ["bwrap", "--dev-bind", "/", "/", "--unshare-net", "--tmpfs", "/dev/input", "--tmpfs",
             "/run/systemd/journal", str(binary), "--paused"],
            stdin=slave, stdout=slave, stderr=slave, env=env)
        started = []

        def drain(seconds):
            count, end = 0, time.time() + seconds
            while time.time() < end:
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        count += len(os.read(master, 65536))
                    except OSError:
                        break
            return count

        try:
            drain(6)  # the first screen settles (Esc would quit it with a fresh config)
            started = descendants(child.pid)
            app = [p for p in started if Path(f"/proc/{p}/comm").read_text().strip() == "vibeconsole"]
            if child.poll() is not None or not app:
                raise SystemExit(f"check-idle: {binary} didn't stay running ({child.poll()})")
            pid = app[0]

            def ticks():
                fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
                return int(fields[11]) + int(fields[12])

            before, at = ticks(), time.time()
            written = drain(10)
            cpu = (ticks() - before) / os.sysconf("SC_CLK_TCK") / (time.time() - at) * 100
            return cpu, written
        finally:
            for pid in [*started, child.pid]:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            child.wait(10)
            deadline = time.time() + 5
            while time.time() < deadline and any(Path(f"/proc/{p}").exists() for p in started):
                time.sleep(0.1)
            left = [p for p in started if Path(f"/proc/{p}").exists()]
            os.close(master)
            os.close(slave)
            if left:
                raise SystemExit(f"check-idle: processes left running: {left}")


binaries = [Path(b) for b in sys.argv[1:]] or [ROOT / "target/release/vibeconsole"]
for binary in binaries:
    cpu, written = measure(binary.resolve())
    print(f"{binary}: idle CPU {cpu:.1f}%, {written} bytes written in 10 s")
