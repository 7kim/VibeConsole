# UI testing (deferred)

Moved out of [SPEC-UI.md](../SPEC-UI.md) on 2026-10-07, to be done later.

- Rust tests render screens to text and assert pane order, grouping, the saved-selection mark, the bank split, toggle lines and fallbacks at a wide and a narrow width, with and without colour.
- A payload-table test uses `testdata/mpk-mini3-programs.hex` to check pad/knob names for at least two programs, including one where a pad sends an unexpected note (guide 09 found Pad 1 sending note 37).
- Existing PTY/terminal tests must still pass. Every command remains reachable by keyboard.
- Live check: one guided pass per screen on the real controller, confirming names match the physical pads in two different programs.
