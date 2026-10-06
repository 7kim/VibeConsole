# KeyAI specification

Status: milestones 1, 2, and 3 complete for the connected device's current note-mode program; external lighting control remains inconclusive. Pad detection evidence and verification limits are recorded in tickets/02-pad-detection.md. Milestone 3 passes automated checks and the user verified physical configure/restart/inspect for Bank A Pad 1 mapped to Shift/hold; evidence and limits are recorded in tickets/03-saved-mappings.md. Milestone 4 hold/toggle logic and Linux uinput output are implemented and pass automated checks. Live GNOME Wayland registration and emitted Shift pairs are verified. A subsequently reproduced line-reader release delay is fixed and the open-stream regression passes; restarting Wispr Flow after KeyAI made its helper open the virtual keyboard, and the user confirmed working Shift/hold push-to-talk and release timing. Ticket 04 hold acceptance is complete: the user verified combinations, shared Ctrl overlap, disconnect/inactive restart, and physical Left Ctrl interaction. Ticket 05 toggle acceptance is complete: the user verified Shift latching, Ctrl+Shift combinations, mixed hold/toggle and toggle/toggle overlap, latched exit/disconnect, and inactive restart. Milestone 4 is complete on the tested Fedora GNOME Wayland session, with external lighting inconclusive and forced-termination/output-failure recovery limitations retained. See tickets/04-hold-shortcuts.md and tickets/05-toggle-shortcuts.md.

## Problem Statement

The user wants to use an Akai MPK Mini MK3 on Fedora as an accessibility and productivity toolbox. Physical pads should operate configurable desktop shortcuts, starting with Wispr Flow push-to-talk. A terminal configuration flow should learn which pad was pressed, associate it with a shortcut and behavior, and save that assignment.

The project must progress in small, verifiable steps: investigate lighting, detect pad events, configure and save mappings, then generate keyboard input.

## Solution

Build a Rust command-line application with two pad behaviors:

- **Hold / push-to-talk:** pressing a pad holds its assigned keys down; releasing the pad releases its hold.
- **Toggle:** the first press holds its assigned keys down until the next press releases its hold. Releasing the physical pad does not end a toggle.

Mappings support individual keys and key combinations, rather than being specific to Shift or Wispr Flow. For Wispr Flow, a hold mapping to Shift implements push-to-talk. A toggle mapping to Shift implements a latched Shift hold. Toggle does not mean sending the previously mentioned Ctrl+Shift application shortcut once per press.

If external control of the MK3's existing red pad lights is possible, illumination should reflect whether a mapping is active and return to its defined idle behavior when inactive. RGB colors are unavailable on this model. Software-controlled red lighting remains an experiment, not a promised capability.

## User Stories

1. As a Fedora user, I want a Rust CLI, so that I can operate the toolbox from a terminal.
2. As an MPK Mini MK3 owner, I want the connected MIDI device identified, so that commands reach the intended hardware.
3. As a user, I want an initial lighting experiment, so that I know which visual feedback the hardware actually supports.
4. As a user, I want unsupported lighting reported clearly, so that unavailable features do not block useful shortcut mappings.
5. As a user, I want to observe pad presses and releases, so that I can verify reliable input before enabling shortcuts.
6. As a user, I want configuration to learn a pad from a physical press, so that I do not need to know MIDI identifiers.
7. As a user, I want to assign a single key to a pad, so that I can hold keys such as Shift conveniently.
8. As a user, I want to assign a key combination, so that pads can operate desktop and application shortcuts.
9. As a user, I want a hold mode, so that a shortcut remains held only while I hold the pad.
10. As a user, I want a toggle mode, so that I can keep keys held without maintaining physical pressure.
11. As a user, I want to inspect a learned pad's assignment, so that I can see its shortcut and behavior.
12. As a user, I want to replace a pad's assignment, so that I can adapt the toolbox to my workflow.
13. As a user, I want mappings saved and loaded on startup, so that configuration survives restarts.
14. As a user, I want overlapping mappings to share held keys correctly, so that releasing one pad does not interrupt another.
15. As a user, I want the app to release its held keys on normal exit or device disconnect, so that modifiers do not remain stuck.
16. As a user, I want every toggle off at startup, so that restarting does not unexpectedly hold keys.
17. As a user, I want invalid shortcuts or configuration rejected clearly, so that an invalid assignment does not silently run.
18. As a user, I want configuration failures to preserve my last saved mappings, so that an unsuccessful save does not lose my setup.
19. As a user, I want any supported active light to represent the app's key-hold state, so that feedback has a precise meaning.

## Implementation Decisions

### Scope and environment

- Use Rust and a terminal interface. Begin with pads only.
- The connected device was identified through USB and ALSA as AKAI MPK mini 3, USB vendor/product ID `09e8:1049`.
- The inspected session reported Fedora GNOME on Wayland. Keyboard output must be verified in that environment; an X11-only solution does not meet the requirement.
- The workspace was initially empty. The current Rust CLI implements pad detection and saved mappings with built-in tests; inspect the working tree before extending it.
- Start with the smallest executable appropriate to each milestone. No GUI, service framework, or plugin architecture is required.

### Hardware lighting gate

- Akai's official comparison explicitly lists no RGB pad backlighting for the MK3. Red-to-green transitions cannot be delivered on this device.
- Establish whether the existing red lights accept external control, which messages are supported, and whether physical presses override that control.
- Do not treat locally triggered pad illumination as evidence that software can control it.
- Use documented or otherwise justified device-specific messages. Do not brute-force unknown SysEx or change persistent device programs just to search for lighting control.
- If safe restoration requires knowing prior settings, establish those settings before changing them. Do not claim arbitrary prior hardware state can be restored unless it can be read or reliably tracked.
- If external control is unsupported or inconclusive, report that outcome and proceed to pad detection. Provide terminal state feedback; a graphical indicator is deferred.

### Pad identification and configuration

- Provide the user-facing equivalents of `get_button` and `set_button`: learn/identify a physical pad and read its assignment; assign or replace its shortcut and behavior. These names describe operations, not mandatory public function names.
- Configuration flow: enter configure mode, press a pad, enter a shortcut such as `Ctrl+Alt+K`, choose hold or toggle, validate, then save.
- Learning a pad must not execute its existing shortcut.
- Identify controls from device identity and their MIDI message type, channel, and control identifier. Do not use transient ALSA client numbers as saved identities.
- Treat distinct pad-bank messages as distinct inputs when the hardware exposes them. Verify actual emitted events during the detection milestone.
- Configure/use press-and-release-capable pad messages for hold behavior. If the current device settings emit no usable release event, explain that limitation instead of inventing release timing.
- A single pad has one shortcut and one behavior in this version. Shortcut members form a simultaneous held combination, not a timed macro.
- Support keyboard keys and modifier combinations through the selected Linux input backend. Reject unknown or unsupported key names; do not promise arbitrary text input or shell commands.
- Save assignments locally using a simple readable format in the user's configuration directory. Save through replacement of a successfully written temporary file to preserve the prior valid configuration on failure.
- Load and validate configuration before activating mappings. Never persist active holds or toggle states.

### Input and held-key behavior

- On a hold pad's press transition, acquire its assigned keys. On release, relinquish them.
- On a toggle pad's press transition, switch its assignment between active and inactive. Physical release only permits the next press transition.
- Repeated press messages while a pad is already down must not repeatedly toggle or acquire extra holds. Pressure changes must not trigger mappings.
- Handle MIDI Note On with velocity zero as a release when using note-based pad input.
- Track each active pad's ownership of its mapped keys. Emit key-down when the first owner acquires a key and key-up when the last owner releases it.
- Example: pad A holds Ctrl and pad B holds Ctrl+C. Releasing B releases C while Ctrl remains held by A. This also applies when one or both pads use toggle mode.
- Press modifiers before other keys and release other keys before modifiers, subject to shared ownership.
- Ownership applies to synthetic input generated by KeyAI. Interaction with physically held keyboard modifiers must be checked on the real desktop, not assumed from internal ownership accounting.
- Normal exit and device disconnect clear the app's active pad state and release all synthetic keys it owns. Attempt the same cleanup on handled runtime errors and shutdown signals.
- Crash, forced termination, and output-backend failure recovery depend on the chosen backend and require explicit verification. Do not promise a cleanup handler can run after an uncatchable termination.
- Startup and any subsequent device reconnection begin inactive. Saved toggle states are never replayed.

### Feedback meaning

- Active feedback means KeyAI has issued and is maintaining the mapping's key-down state. It does not confirm that Wispr Flow is listening or that another application accepted the shortcut.
- Where external LED control works, use red illumination for active mappings and restore the established idle behavior when inactive. A toggle must remain visibly active after physical release if the hardware permits it.
- If an input/output operation fails, report the failure and do not present it as successful shortcut activation.

### Incremental milestones

1. **Lighting feasibility:** a minimal Rust experiment identifies the device and attempts only justified control of existing red illumination. Record externally observed results and restoration behavior. Supported, unsupported, and inconclusive are distinct outcomes. No keyboard injection in this milestone.
2. **Pad detection:** show pad identity, press, and release events in the terminal. Verify both pad banks as available, release encoding, pressure messages, and repeated-event behavior on the actual unit.
3. **Saved mappings:** implement learn, inspect, assign/replace, validate, and save/load through the terminal. Display the resulting mappings without generating desktop shortcuts.
4. **Keyboard operation:** implement hold and toggle output on Fedora Wayland, shared-key ownership, cleanup, and terminal feedback. Verify Shift push-to-talk and a general modifier combination on the desktop. Integrate hardware lighting only to the extent proven in milestone 1.

Each milestone must have a reported check and explicit limitations before it is called complete. Completing this specification does not authorize skipping directly to the full application.

## Testing Decisions

- Test observable event sequences rather than private functions or a proposed module layout. No prior tests exist in the workspace.
- Proposed main automated seam: supply normalized pad events and mappings, then observe the emitted key-down/key-up sequence. Use a recording output sink rather than sending real desktop shortcuts during automated tests.
- Keep one small runnable behavior test covering hold, toggle, duplicate presses, shared-key overlap, and cleanup. For the overlap example, Ctrl must go down once and remain down until both owners are inactive.
- Verify configuration with a save/load round trip and rejection of invalid shortcuts without replacement of the last valid saved configuration.
- Verify MIDI normalization at the input boundary, particularly velocity-zero release and ignored pressure events.
- Test real lighting and desktop behavior manually on the connected hardware. Automated state tests do not prove physical illumination or successful Wayland injection.
- Keyboard acceptance checks include holding/releasing Shift, latching/unlatching Shift, a combination such as Ctrl plus another key, overlapping mappings, disconnect while active, normal shutdown while active, and restart with toggles off.
- Check the chosen backend's actual behavior on process death and alongside physical keyboard modifiers; document any remaining limitation.
- These test seams are proposed by this specification. The user requested an end to interviewing; no additional test-design approval round was conducted.

## Out of Scope

- RGB pad colors or hardware modification.
- Piano keys, knobs, joystick, and pedal mappings in the initial version.
- Graphical configuration, tray UI, or graphical status indicators.
- Confirmation of Wispr Flow's microphone/listening state or direct Wispr Flow integration.
- Timed macros, text expansion, launching programs, shell commands, and application-specific profiles.
- Automatic startup, packaging, background-service installation, and support for other operating systems or controller models.
- Persisting active toggles across sessions.

## Further Notes

- The user explicitly agreed to pads first, terminal configuration, saved mappings, hold/toggle semantics, release-on-exit/disconnect, inactive restart, and shared ownership of overlapping keys.
- The initial red-to-green request is constrained by the identified hardware. External control of the existing red lights has not yet been tested.
- Reference: [Akai MPK mini IV FAQ — MK3/IV comparison](https://support.akaipro.com/en/support/solutions/articles/69000872348-mpk-mini-iv-frequently-asked-questions).
- MIDI input currently reuses the native `amidi` command. Keyboard output uses Linux uinput through evdev with read/write permission to /dev/uinput; the exact lighting protocol remains inconclusive. The saved-mapping key vocabulary is documented in README.md; verify its output in milestone 4.
- No repository remote or issue tracker is configured. This is a local specification; no issue has been published or labeled.
