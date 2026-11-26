# /// script
# requires-python = ">=3.12"
# dependencies = ["pyte"]
# ///
"""v7.8 ticket 51: the real binary with the Akai and Xbox emulators together drives the shared
program picker. Run after cargo build --locked: uv run scripts/emulate/check-program-pick.py

Checks: /prog-select lists both devices' programs; picking the Xbox's "Editor" shows it in the
header with the Akai still connected; the first Akai pad press after the pick records exactly
one F1. Both emulators are isolated (drive.py: --emulated, bwrap, recorded keys).
"""
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from akai import load_programs  # noqa: E402
from drive import Drive  # noqa: E402
from run import mapping  # noqa: E402


def main():
    ram = load_programs()[1]  # the program the Akai emulator selects
    pad = mapping(0, "Bank A Pad 1", "note", ram[0x10] + 1, ram[0x24], "F1")
    names = ["Apps", "Editor"] + [""] * 6
    with Drive(akai=True, xbox=True, mappings=[pad], devices={"xbox_programs": names}) as d:
        d.wait("Pads fire on their next press", 15)
        d.run.xbox_ready(d.xbox)
        d.type("/prog-select", "enter")
        d.pick("/prog-select")
        d.wait("Apps · Xbox")
        assert any("TradingView · Akai" in row for row in d.screen()), "\n".join(d.screen())
        mark = d.mark()
        d.pick("Editor · Xbox")
        d.wait("Xbox · Editor", since=mark)
        header = "\n".join(d.screen()[:4])
        assert "MIDI 💡 ON connected VibeConsole emulator · MPK mini 3" in header, header
        print("ok: picking the Xbox's Editor shows it; the Akai stays connected")
        sent = len(d.recorded())
        d.akai.pad(1)
        d.wait_recorded("key KEY_F1 up")
        time.sleep(0.5)
        assert d.recorded()[sent:] == ["key KEY_F1 down", "key KEY_F1 up"], d.recorded()[sent:]
        print("ok: the first Akai pad press after the pick records exactly one F1")


if __name__ == "__main__":
    main()
