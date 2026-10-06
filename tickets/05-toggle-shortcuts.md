# 05 — Latch shortcuts with toggle pads

**What to build:** Enable saved toggle mappings alongside hold mappings: the first press holds the shortcut, physical release leaves it active, and the next press releases it. Preserve shared-key ownership, cleanup, and available active-state feedback.

**Blocked by:** 04 — Run saved hold shortcuts on Fedora Wayland.

**Status:** completed — automated checks and required live toggle acceptance pass on the tested Fedora GNOME Wayland session. Forced termination while latched, output-failure recovery, and external lighting retain the documented limitations.

- [x] Activate a toggle on the first press transition and deactivate it on the next distinct press; physical release only rearms detection of the next press.
- [x] Repeated press messages and pressure changes cannot repeatedly toggle or add held-key ownership.
- [x] Support individual keys and combinations with the same validation and key ordering as hold mappings. Toggle holds the assigned keys; it does not send an application toggle shortcut once per press.
- [x] Share ownership correctly between hold/toggle and toggle/toggle mappings. Releasing or disabling one mapping leaves keys held by another active mapping down.
- [x] Display toggle state after physical release. Where external lighting was proven, keep red illumination active until toggled off and restore established idle behavior afterward.
- [x] Apply the existing exit, disconnect, and handled-failure cleanup to latched keys. Reloaded mappings and subsequent device connections start with toggles off.
- [x] Extend the small recorded-output behavior check for toggle cycles, duplicate messages, overlap with holds and other toggles, and cleanup while latched.
- [x] Run formatting, compile, and relevant tests.
- [x] Demonstrate live Shift latching and unlatching.
- [x] Verify mixed hold/toggle shared-key overlap in both release orders.
- [x] Verify a toggle combination.
- [x] Verify toggle/toggle shared-key overlap.
- [x] Verify disconnect and exit while latched, and inactive restart.

**Verification:** Tap a mapped pad, release it, confirm the shortcut remains held, then tap again to release. Repeat with an overlapping hold mapping and verify each key is released only when its last owner deactivates.

## Execution record (2026-10-06)

The user clarified that “implement 4” meant SPEC milestone 4, including holds and toggles, rather than only ticket 04. `run` therefore accepts saved toggle assignments. Each physical release rearms the next press without releasing a latched mapping. Active pad state is kept only in memory and is initially off. The recording-output check exercises a toggle cycle with a duplicate press and pressure, release while latched, shared Ctrl ownership between a hold and two toggles, and cleanup while latched. Terminal feedback changes only after successful output; hardware lighting remains inconclusive.

Required live checks are complete. Physical Left Ctrl interaction was verified for the shared backend in ticket 04. See ticket 04 and README.md for cleanup limitations.

Live Shift-toggle outcome: the user configured Bank A Pad 4 to Shift/toggle. After tapping and releasing the pad, repeated typing produced `AAAAAAAAAAAA`; after the next tap/release, typing produced `aaaaaaaa`. The user confirmed success with multiple keystrokes, verifying that Shift remains latched after physical release and is released by the next distinct press. `cargo run -- list` confirmed the saved assignment alongside the three existing hold mappings. This establishes desktop Shift latching, not Wispr Flow application listening state.

Mixed-overlap outcome: the user added Bank A Pad 5 to Shift/hold and confirmed both prescribed release orders worked. Turning Pad 4 off while Pad 5 stayed held preserved uppercase typing until Pad 5 was released; releasing Pad 5 while Pad 4 stayed latched preserved uppercase typing until Pad 4 was toggled off. Both returned to lowercase after the final owner released Shift. `cargo run -- list` confirmed all five saved assignments. This passes mixed hold/toggle shared-key ownership on the desktop.

Latched-cleanup outcome: the user explicitly approved both prescribed checks. After latching Pad 4 Shift/toggle, SIGTERM stopped KeyAI and restored lowercase typing; a fresh run kept Shift off. Unplugging the controller while Shift was latched also stopped KeyAI and restored lowercase typing, and reconnecting/restarting kept Shift off. This passes handled exit, disconnect, and inactive restart with a toggle active. Forced termination and output-backend failure recovery remain the documented limitations.

Toggle-combination outcome: the user configured Bank A Pad 6 to Ctrl+Shift/toggle and approved the prescribed editor test. After tapping/releasing Pad 6, Arrow Right selected text word by word; after the next tap, ordinary cursor movement and lowercase typing returned. `cargo run -- list` confirmed the saved assignment alongside the existing five mappings. This verifies a general simultaneous modifier combination remaining latched after physical release and releasing on the next distinct press.

Toggle/toggle overlap outcome: the user approved both prescribed release orders for Pad 4 Shift/toggle and Pad 6 Ctrl+Shift/toggle. Turning Pad 6 off first preserved uppercase typing until Pad 4 was turned off. Turning Pad 4 off first preserved Ctrl+Shift word selection until Pad 6 was turned off. Releasing the final owner restored ordinary cursor movement and lowercase typing. This completes ticket 05 and the required keyboard-operation acceptance for SPEC milestone 4 on the tested session, with the previously documented lighting and recovery limitations retained.
