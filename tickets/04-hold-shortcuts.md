# 04 — Run saved hold shortcuts on Fedora Wayland

**What to build:** Run a saved hold mapping end to end: physical pad down holds the assigned keyboard keys; pad release relinquishes them. Support general key combinations, overlapping holds, reliable shutdown/disconnect cleanup, and truthful terminal feedback.

**Blocked by:** 03 — Learn a pad and save its shortcut assignment.

**Status:** completed — automated checks and required live hold-shortcut acceptance pass on the tested Fedora GNOME Wayland session. Forced termination while held, output-failure recovery, and external lighting retain the documented limitations.

- [x] Select and verify a Linux input backend on the actual Fedora Wayland session; document required permissions and setup. An X11-only demonstration is insufficient.
- [x] Validate all shortcut key names against the output backend before activating mappings. The user authorized SPEC milestone 4, so ticket 05 toggle support is included in the same implementation.
- [x] Press mapped keys on a pad press transition and release its hold on release. Ignore duplicate press transitions and pressure events.
- [x] Press modifiers before other keys and release other keys before modifiers, while respecting shared ownership.
- [x] For overlapping mappings, emit key-down only on the first owner's acquisition and key-up only on the last owner's release. Verify Ctrl alongside Ctrl+C.
- [x] Release all app-owned synthetic keys on normal exit and device disconnect; attempt cleanup on handled errors and shutdown signals. Any subsequent connection starts inactive.
- [x] Investigate process-death behavior and physical-keyboard modifier interaction on the chosen backend; document remaining limits instead of promising an uncatchable-signal cleanup handler.
- [x] Display active/inactive state as the app's maintained key-hold state, not confirmed application listening. Report output failures without presenting activation as successful.
- [x] Integrate active/idle red-light feedback only if ticket 01 established support; otherwise retain terminal feedback and its explicit limitation.
- [x] Leave a small runnable check that feeds pad events and records output transitions for holds, duplicates, overlap, and cleanup without injecting real desktop shortcuts.
- [x] Run formatting, compile, and relevant tests; verify live Shift push-to-talk, general combinations, and exit while held.
- [x] Verify live shared-key overlap.
- [x] Verify disconnect while held and inactive restart.
- [x] Verify interaction with physically held keyboard modifiers; forced termination while held and output-failure recovery remain documented limits.

**Verification:** Demonstrate the complete saved-mapping path on the actual desktop, including Wispr Flow's existing Shift binding. Record application observations separately from synthetic key-output success.

## Execution record (2026-10-06)

Implemented `cargo run -- run` using evdev's Linux uinput virtual keyboard, leaving `detect`, `get`, and `configure` free of keyboard output. Input normalization rejects malformed/non-note payloads; saved names resolve to Linux key codes before activation. Shared ownership applies to hold and toggle mappings. Cleanup tracks attempted key-downs so partially failed output still receives a release attempt, tries all releases, then destroys the keyboard. Handled SIGINT/SIGTERM/SIGHUP/SIGQUIT and input termination use that cleanup. The amidi child receives SIGKILL on parent death through Linux PR_SET_PDEATHSIG.

Rechecked USB 09e8:1049, ALSA hw:0,0,0, GNOME-Classic:GNOME on Wayland, and /dev/uinput permissions. Existing Wispr Flow udev rules give the current user access; no system permission changes were made. Starting `run` with the existing Bank A Pad 1 Shift/hold mapping created `KeyAI virtual keyboard`, /dev/input/event21 in this run. udev reported ID_INPUT_KEYBOARD=1 and libinput reported DEVICE_ADDED on seat0 with keyboard capability. That proves backend registration; subsequent live Shift events are recorded below. Application acceptance is separate.

Automated format/compile/test checks pass. The recording-output test covers Ctrl with Ctrl+C, mixed hold/toggle and toggle/toggle ownership, key ordering, duplicate events, pressure, inactive startup, cleanup, and partial-output/release failures. No automated test injects desktop shortcuts. Upstream kernel source was reviewed for descriptor-close device destruction and unregister key release; held-key recovery after SIGKILL remains unverified on this installed desktop.

Remaining live checks: Wispr Flow Shift press/release, a general modifier combination, overlapping pads, disconnect and exit while held, physical keyboard modifier interaction, process death while held, and application recovery after an output failure. External lighting remains inconclusive; terminal feedback is the implemented fallback. README.md records permissions and manual acceptance instructions. Milestone 4 remains incomplete until physical acceptance is observed.

Live Shift output: the current saved Bank A Pad 1 Shift/hold mapping produced three ACTIVE/INACTIVE cycles. A libinput monitor attached only to KeyAI's virtual keyboard received three KEY_LEFTSHIFT (42) press/release pairs, with hold durations approximately 1.264 s, 29.677 s, and 1.984 s. The user reported pressing the pad but seeing no application response and deferred further live checks until later in the chat. Successful Wispr Flow push-to-talk is therefore not verified, despite libinput receiving the Shift events. Ctrl+C stopped KeyAI cleanly, printed synthetic-key cleanup, and libinput observed DEVICE_REMOVED. This shutdown occurred after pad release, so exit while active remains unverified.

Two further inactive startup/termination checks passed: SIGTERM exited 0 with cleanup, and SIGKILL exited -9. In both cases the virtual keyboard disappeared and the exact child amidi process was removed; a final process listing contained no keyai or amidi processes. These checks injected no keyboard events and do not prove held-key recovery after SIGKILL. `cargo fmt --check`, `cargo check`, and `cargo test` passed, with 3 built-in tests.

## Release-delay repair (2026-10-06)

The user reported that physical Left Shift works with Wispr Flow, while pad-generated Shift affects the IDE but did not trigger Flow; releases could remain ACTIVE during silence. Reproduction used the installed alsa-utils 1.2.16 dump formatter's upstream source and KeyAI's existing parser: a full Note Off stayed pending until the next status byte. amidi emits the newline at the start of the next message, whereas KeyAI used BufRead::lines. This affected `run`, `detect`, and learning. The earlier complete-message tests and emitted Shift pairs did not establish timely release.

A built-in regression first failed with a release timeout on the old line reader. All three callers now use one streaming decoder and `amidi --receive=/dev/stdout`, emitting packets when their data bytes arrive, without requiring a later event or EOF. The regression feeds press, pressure, and Note Off into an open Unix stream and observes recorded Shift down/up before closing it. A separate framing check covers fragmented reads, running status, realtime interleaving, unrelated channel/system messages, truncated EOF, and read errors. No new dependencies were added. `cargo fmt --check`, `cargo check`, and all 5 tests pass.

Rechecked the same USB identity, ALSA hw:0,0,0, GNOME Wayland, and existing uinput access before the live test. Before Flow restart its helper had only physical keyboard descriptors open. After KeyAI reached Ready and Flow was restarted, the new helper (PID 91221 in this check) opened KeyAI's /dev/input/event21 as well as the physical keyboards. This verifies capture-device discovery after restart; Wispr application start/stop and immediate physical release still require user observation. Documented startup order in README.md. No Wispr source or system settings were changed.

Live repair outcome: the user replied “yes it is working” to the requested held-pad release-timing and Wispr start/stop check after restarting Flow with KeyAI running. The native libinput monitor recorded 12 paired KEY_LEFTSHIFT down/up cycles during the check. Flow's new helper opened the virtual keyboard. This establishes the Shift/hold path for this session; general combinations, overlaps, toggles, disconnect while active, shutdown while latched, and forced-death recovery remain separate unverified milestone checks. The test KeyAI and libinput monitor were then stopped; starting another KeyAI instance requires fully reopening Flow afterward so its helper discovers the new device.

The last live trial activated Shift again before Ctrl+C; libinput then observed its release and DEVICE_REMOVED during shutdown. This verifies cleanup while a hold was active. The first shutdown printed a misleading MIDI EOF error because amidi could exit before the parent rechecked its signal flag. The runtime now rechecks that flag after the receive wait, so requested shutdown takes precedence over EOF. Two subsequent Ctrl+C-style SIGINT checks targeting the whole process group exited 0, reported cleanup, and showed no spurious EOF error; they injected no key events. Final state contains no KeyAI/amidi processes or KeyAI virtual keyboard. All 5 tests, formatting, and compile checks pass after the race fix.

General-combination outcome: the user configured Bank A Pad 1 to Ctrl+C/hold and Bank A Pad 2 to Ctrl+V/hold, and reported that both work well. `cargo run -- list` confirmed both saved assignments. This passes the general-combination check; it does not establish shared-key overlap. The previously verified Shift assignment has been replaced in the current configuration.

Shared-key overlap outcome: the user added Bank A Pad 3 to Ctrl/hold and confirmed the prescribed test worked: holding Pad 3, pressing/releasing Pad 1 (Ctrl+C), then pressing physical A still selected all text; releasing Pad 3 restored ordinary typing. This verifies that releasing Ctrl+C preserves the other pad's Ctrl ownership and releasing the last owner releases Ctrl. `cargo run -- list` confirmed all three saved hold assignments.

Disconnect/restart outcome: the user confirmed normal typing returned after unplugging the controller while Pad 3 (Ctrl/hold) was active. Their log shows ACTIVE, then synthetic-key cleanup and a MIDI-input-ended error. This is the expected error exit for lost input, with cleanup completed. After reconnecting, a fresh `cargo run -- run` loaded the same assignments and reported all mappings inactive; the user confirmed ordinary typing and then exited cleanly with Ctrl+C. Both disconnect while held and inactive restart pass.

Physical-modifier outcome: the user explicitly approved both prescribed Left Ctrl tests. Holding physical Left Ctrl across a Pad 3 press/release preserved Ctrl+A; holding Pad 3 across a physical Left Ctrl press/release also preserved Ctrl+A. Releasing the remaining hold restored ordinary typing in both cases. Their supplied log shows Pad 3 ACTIVE/INACTIVE transitions and clean Ctrl+C shutdown. This completes ticket 04's required hold acceptance. Ticket 05's physical toggle checks remain pending; the full SPEC milestone 4 is not yet complete.
