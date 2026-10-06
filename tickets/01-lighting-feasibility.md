# 01 — Verify MK3 red-light control from Rust

**What to build:** A minimal, user-guided Rust terminal experiment that connects to the MPK Mini MK3, verifies incoming MIDI packets through prompted physical actions, and then investigates external control of its existing red pad lights. Report connection, packet reception, and lighting results separately; lack of lighting support must not block later input work.

**Status:** completed — MIDI pad input verified; external lighting control inconclusive.

- [x] Create the Rust executable and reproducible run instructions.
- [x] Identify the connected MPK mini 3 (USB 09e8:1049) and select ALSA port hw:0,0,0.
- [x] Open the input port and receive raw MIDI packets during physical pad actions. Two earlier retries failed (one capture-order bug, then a busy port); the final guided run opened successfully.
- [x] Prompt for Bank A and B Pad 1 press/release actions with capture already armed, bounded to 15 seconds.
- [x] Display timestamps, raw hexadecimal MIDI bytes, and per-window packet counts.
- [x] Verify the input path from observed Note On and Note Off messages in both banks. Pressure messages also arrived while pads were held.
- [x] Check Akai's official material for a justified external LED-control method. The quick-start guide describes pad MIDI output and red/green banks, but gives no host LED-control protocol; no output message was sent.
- [x] Record lighting as inconclusive. No software-controlled illumination or physical-press interaction was tested; the user did not report an external light observation.
- [x] No hardware settings were changed, so no restoration was needed.
- [x] Record no RGB support and terminal feedback as the fallback.
- [x] Run formatting, compile, and test commands. No desktop keyboard injection occurred.

## Execution record

Successful command: cargo run. USB identity: 09e8:1049; ALSA input: hw:0,0,0. The user completed the prompted physical sequence.

| Action window | Packets | Captured evidence |
|---|---:|---|
| Bank A, press/hold Pad 1 | 81 | 99 24 16 Note On; A9 24 .. poly pressure |
| Bank A, release Pad 1 | 19 | 89 24 00 Note Off |
| Bank B, press/hold Pad 1 | 45 | 99 2C 0F Note On; A9 2C .. poly pressure; 89 2C 00 Note Off |
| Bank B, release Pad 1 | 3 | 99 2C 08 then 89 2C 00 |

An earlier zero-packet result came from a version that began capture after the user's Enter key, so it was not evidence of silence. The current program starts capture before printing the action prompt.

Akai sources: [MPK mini Quick-start Guide](https://cdn.inmusicbrands.com/akai/mpk3mini/MPK-mini-Quickstart-Guide-v1_3.pdf) and [MPK mini IV FAQ comparison](https://support.akaipro.com/en/support/solutions/articles/69000872348-mpk-mini-iv-frequently-asked-questions). Akai's comparison lists RGB backlit pads as absent from MK3. The lack of a documented external-control method is not proof that no undocumented method exists.

**Verification:** A reproducible guided-session report including commands, selected ports, connection/open errors, prompts, packet counts and sample raw messages, no-data/retry outcomes, protocol sources, user-reported lighting observations, and restoration results. Distinguish a port opening successfully, packets actually arriving, an output message being sent, and a light visibly changing. Do not mark an unperformed user action or unanswered visual check as passed.
