# /// script
# requires-python = ">=3.12"
# dependencies = ["pyte"]
# ///
"""A driver for developer-test runs with the emulators (v7.8 ticket 67): it starts the real
binary with `--emulated` (inside `hide()`'s bwrap, nothing sent to the desktop), renders its
screen, types keys as a user does, waits for text on screen or in the output history, and picks
a menu row by its label, so a test's steps become a short list.

    uv run scripts/emulate/drive.py          # replays developer test 17 (one-per-press sequence)

    with Drive(akai=True) as d:              # or xbox=True, or both
        d.wait("Pads fire on their next press")
        d.type("/configure", "enter"); d.pick("/configure"); d.pick("Learn a pad")
        d.akai.pad(1); d.recorded()          # output.log: the keys it would have sent

It never starts or stops the login service, and checks afterwards that no `--emulated` process
is left.
"""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

import pyte

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run import Run, hide  # noqa: E402

COLUMNS, ROWS = 110, 30  # run.Run's PTY size
KEYS = {"up": "\x1b[A", "down": "\x1b[B", "right": "\x1b[C", "left": "\x1b[D", "enter": "\r", "esc": "\x1b",
        "tab": "\t", "space": " ", "backspace": "\x7f"}


def emulated_left():
    """Pids of `vibeconsole --emulated` processes still running."""
    pids = subprocess.run(["pgrep", "-x", "vibeconsole"], capture_output=True, text=True).stdout.split()
    return [p for p in pids if b"--emulated" in Path(f"/proc/{p}/cmdline").read_bytes().split(b"\0")]


class Drive:
    def __init__(self, akai=False, xbox=False, mappings=(), command=(), devices=None):
        assert shutil.which("bwrap"), "bwrap not found: install it (sudo dnf install bubblewrap)"
        assert not emulated_left(), f"an emulated run is already running: {emulated_left()}"
        self.akai_on, self.xbox_on, self.mappings, self.command = akai, xbox, list(mappings), command
        self.devices = devices

    def __enter__(self):
        self.run = None
        try:
            return self._start()
        except BaseException:
            self.__exit__()  # what did start (an emulator, the temp folder) is closed again
            raise

    def _start(self):
        self._tmp = tempfile.TemporaryDirectory()
        directory = Path(self._tmp.name)
        self.akai = self.xbox = None
        if self.akai_on:
            from akai import Akai
            self.akai = Akai().__enter__()
            self.akai.program(1)
        if self.xbox_on:
            from xbox import Xbox
            self.xbox = Xbox().__enter__()
        sandbox = hide(*([self.xbox.node] if self.xbox else []))
        path = os.environ["PATH"]
        if not self.akai:  # Xbox only: a fake lsusb so no real Akai is looked for
            bin_dir = directory / "bin"
            bin_dir.mkdir()
            (bin_dir / "lsusb").write_text("#!/bin/sh\nexit 1\n")
            (bin_dir / "lsusb").chmod(0o755)
            path = f"{bin_dir}{os.pathsep}{path}"
        run_dir = directory / "run"
        run_dir.mkdir()
        old = os.environ["PATH"]
        os.environ["PATH"] = path  # run.Run passes the environment on
        try:
            self.run = Run(str(run_dir), self.mappings, sandbox, self.command, devices=self.devices).__enter__()
        finally:
            os.environ["PATH"] = old
        return self

    def __exit__(self, *exc):
        try:
            if self.run:
                self.run.__exit__(*exc)
        finally:
            for device in (getattr(self, "xbox", None), getattr(self, "akai", None)):
                if device:
                    device.__exit__(*exc)
            if hasattr(self, "_tmp"):
                self._tmp.cleanup()
        left = emulated_left()
        assert not left, f"emulated run left running: {left}"

    def screen(self):
        """The screen as the user sees it now, one string per row."""
        stream = pyte.ByteStream(screen := pyte.Screen(COLUMNS, ROWS))
        stream.feed(self.run.data)
        return [row.rstrip() for row in screen.display]

    def show(self):
        print("\n".join(self.screen()))

    def type(self, *keys):
        """Type text, or named keys (`up`, `enter`, `esc` …), as a user does."""
        for key in keys:
            os.write(self.run.master, KEYS.get(key, key).encode())
            time.sleep(0.15)

    def wait(self, text, seconds=8, since=0):
        """Wait for `text` in the output history after the mark `since` (a later notice can
        replace an earlier one on screen)."""
        self.run.read_until(text, seconds, since)

    def mark(self):
        return self.run.mark()

    def pick(self, label, key="enter", seconds=5):
        """Move the `> ` cursor to the first row with `label` as a whole word (scrolling down to
        it when it's off screen), then press `key` (`space` toggles a checkbox row)."""
        word = re.compile(r"(?<!\w)" + re.escape(label) + r"(?!\w)")
        deadline = time.monotonic() + seconds
        while True:
            rows = self.screen()
            at = next((i for i, r in enumerate(rows) if re.search(r"(^|[│ ])> ", r)), None)
            want = next((i for i, r in enumerate(rows) if word.search(r)), None)
            if at is not None and want is not None and at == want:
                return self.type(key)
            if time.monotonic() > deadline:
                raise AssertionError(f"no row {label!r} under the cursor:\n" + "\n".join(rows))
            if at is None:
                time.sleep(0.1)
                continue
            # A label below the visible rows of a long list: move down until it scrolls in.
            self.type("down" if want is None or want > at else "up")

    def recorded(self, file="output.log"):
        return self.run.recorded(file)

    def wait_recorded(self, line, seconds=5):
        self.run.wait_recorded(line, seconds)


def test_17():
    """Developer test 17: a 3-step One per press sequence built in /configure; three presses
    run steps 1, 2, 3; holding a second at the end starts it over."""
    with Drive(akai=True) as d:
        d.wait("Pads fire on their next press", 15)
        d.type("/configure", "enter")
        d.pick("/configure")
        d.pick("Learn a pad")
        d.akai.pad(1)
        d.pick("Use observed control")
        d.pick("Sequence of steps")
        for key in ("F5", "F6", "F7"):
            d.pick("Add step")
            d.pick("Shortcut")
            d.pick("Select keys")
            d.pick("Function keys")
            d.pick(key, "space")
            d.type("enter")
            d.pick("Done selecting")
        d.pick("Done")
        d.pick("One per press")
        d.pick("Pressed")
        d.pick("Save and finish")
        d.wait("Sequence, one")
        time.sleep(0.5)
        for key in ("F5", "F6", "F7"):
            d.akai.pad(1)
            d.wait_recorded(f"key KEY_{key} up")
        mark, sent = d.mark(), len(d.recorded())
        d.akai.pad(1, hold=1.4)
        d.wait("sequence starts over at step 1", 5, since=mark)
        assert len(d.recorded()) == sent, d.recorded()[sent:]  # the hold sends nothing
        d.akai.pad(1)
        d.wait_recorded("key KEY_F5 up")
        assert d.recorded()[sent:] == ["key KEY_F5 down", "key KEY_F5 up"], d.recorded()
    print("ok: test 17: presses 1, 2, 3 sent F5, F6, F7; the 1 s hold started over; the next press sent F5")


if __name__ == "__main__":
    test_17()
