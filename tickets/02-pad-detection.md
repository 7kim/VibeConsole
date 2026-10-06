# 02 — Observe pad presses and releases in the terminal

**What to build:** A Rust terminal mode that identifies each pressed pad and displays reliable press/release events, providing the input path needed for learning mappings.

**Blocked by:** 01 — Verify MK3 red-light control from Rust. The gate is completion of the agreed first investigation, not successful lighting control.

**Status:** completed for the current channel-10 note-mode program; live evidence and remaining verification limits are recorded below.

- [x] Show device identity, MIDI message type, channel, and control identifier without treating transient ALSA client numbers as persistent pad identity.
- [x] Verify events from the continuous detector on physical pads in both banks. The user pressed pads 1–8 in Bank A, then pads 1–8 in Bank B; all 16 identities were reported in that order.
- [x] Recognize Note Off and Note On with velocity zero as release. The prior hardware capture observed Note Off; a deterministic check covers velocity zero.
- [x] Normalize repeated presses and pressure messages so they do not create new press transitions while a pad is down. The live run reported one press and one pressure observation per pad, without extra transitions during pressure. Duplicate Note On suppression passes the deterministic check; no duplicate Note On while down occurred in this live log.
- [x] Report unavailable device/port and unexpected input termination; discard the detector state before restart.
- [x] Include a deterministic input-normalization check and keep live press/release observations separate.
- [x] Run formatting, compile, and test checks. Generate no desktop shortcuts.

## Live execution record

The user's successful cargo run opened USB device 09e8:1049 through ALSA input hw:0,0,0. All messages used MIDI channel 10. The user confirmed pressing each bank's pads in ascending order.

| Bank | Pads identified | Note identifiers | Release observations |
|---|---|---|---|
| A | 1–8 | 36–43 (0x24–0x2B) | Note Off for all eight pads |
| B | 1–8 | 44–51 (0x2C–0x33) | Note Off for pads 1–7 |

The log contains 16 PRESS events, 16 PRESSURE IGNORED observations, and 15 RELEASE events. Raw Note On messages begin with 99, polyphonic pressure with A9, and Note Off with 89. Examples: Bank A Pad 1 pressed with 99 24 07 and released with 89 24 00; Bank B Pad 1 pressed with 99 2C 23 and released with 89 2C 00.

Verification limits:

- Ctrl+C ended the capture after Bank B Pad 8's pressure observation; its release is not present in this log.
- Physical releases used Note Off. Note On velocity-zero release and duplicate Note On suppression were verified by the deterministic Rust check.
- Device disconnection was not exercised live. Unexpected input termination is handled by exiting and discarding the detector state; no automatic reconnection is implemented.
- Pad identification uses the verified current channel and note ranges. Other device programs or CC/Program Change pad modes are outside this verified configuration.

The implementation's last cargo fmt --check, cargo check, and cargo test run passed, including the single normalization behavior check. No desktop keyboard input was generated.

**Verification:** Press, hold, vary pressure, and release pads while inspecting terminal events. Exercise the velocity-zero release path with a deterministic check even if the physical unit uses another release encoding.
