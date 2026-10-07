# VibeConsole

Turn an **Akai MPK Mini MK3** into a shortcut and desktop-action controller on **Fedora GNOME Wayland**. VibeConsole is a Rust terminal application with saved per-program mappings, hold/toggle shortcuts, application launching, audio controls, and optional musical-setting feedback.

## Features

- Learn Note-mode pads from both banks and piano keys; select shortcuts from menus or record a physical keyboard chord.
- Use hold/toggle shortcuts with shared-key ownership, duplicate suppression, release guards, and cleanup on pause, program transitions, disconnect, and handled exit.
- Configure absolute, non-wrapping knob directions/steps and calibrated joystick axes with neutral regions, hysteresis, and supported diagonals.
- Follow the controller's current hardware program automatically at startup and during use. Explicitly select any of eight hardware programs from VibeConsole when desired.
- Display and rename stored hardware programs while keeping mappings attached to their numeric program.
- Save independent idle octave, tempo, and arpeggiator preferences for each compatible program; add per-pad active feedback and restore owned settings.
- Launch installed applications and request native output-volume, output-mute, or microphone-mute changes from supported controls.
- Start Wispr Flow after the virtual keyboard is ready, or explicitly request targeted graceful repair/reopening.
- Inspect MIDI/input activity and toggle state, retain the virtual keyboard through ordinary editing/reconnection, and install a command with optional login startup.

Implemented features and user-approved hardware/application behavior are distinguished in [Verification and acceptance](#verification-and-acceptance).

## Build and start

Use a Rust toolchain supporting the 2024 edition. Connect the controller by USB, then run from the repository:

```bash
sudo dnf install alsa-utils usbutils acl
cargo build --locked
./target/debug/vibeconsole doctor
./target/debug/vibeconsole
```

Start explicitly paused with:

```bash
./target/debug/vibeconsole --paused
```

VibeConsole uses `amidi` for MIDI input, `lsusb` for device identification, and Linux uinput for keyboard output. App launching uses `gio`; audio actions require `wpctl`; optional login startup uses the native user `systemctl`. `doctor` reports prerequisites without creating a keyboard, injecting events, launching applications, or changing controller settings.

To inspect the connected device:

```bash
lsusb -d 09e8:1049
amidi --list-devices
```

Device numbers can change. VibeConsole discovers the matching port; close other MIDI readers/editors before using it. Run VibeConsole as your desktop user, not as root.

### Keyboard permissions

Check output access:

```bash
test -r /dev/uinput && test -w /dev/uinput && echo "uinput access ready"
```

If needed, load uinput and grant temporary access:

```bash
sudo modprobe uinput
sudo setfacl -m "u:$(id -un):rw" /dev/uinput
```

Recheck afterward. The ACL may need reapplying after reboot or device-node recreation; VibeConsole does not install persistent permission rules. Physical shortcut recording separately needs read access to the detected `/dev/input/event*` keyboard nodes. Recording errors identify the missing access; menu-based key selection remains available.

## Programs and automatic detection

The interactive app starts with **running intent**, but mapped effects wait until the current hardware program is identified. `--paused` performs detection while retaining PAUSED. Empty programs have no mapped effects. Piano programs retain the explicit `/run` confirmation for physical arpeggiator Off and intended octave.

The **controller's current state takes priority** over the last program selected in VibeConsole. For example, selecting Program 3 in VibeConsole, quitting, physically selecting Program 1, and reopening should use Program 1 when its RAM matches uniquely. This uses the hardware as the source of truth; it does not blindly restore a saved program number.

VibeConsole reads the eight stored presets and compares their complete payloads with current RAM. One exact match identifies the numeric program; names alone do not. Programs with identical names can still be distinguished by different settings. Identical full presets or externally edited RAM can be ambiguous: mapped output stays inactive and `/prog-select` remains the explicit recovery path.

`/prog-select` deliberately selects a hardware slot, reads back RAM, and enables that context only after verification. Hardware slots 1–8 correspond to saved contexts 0–7. Renaming never moves mappings between contexts.

### Why the 250 ms check exists

The observed physical program switches produced no unsolicited MIDI notification. VibeConsole therefore asks for current RAM periodically through the existing coordinated MIDI response stream.

- The worker **sleeps for 250 ms between idle checks**, then requests RAM. This is approximately four periodic checks per second at most, excluding other operation-specific queries.
- If the reply differs from VibeConsole's last confirmed state, VibeConsole clears old ownership and re-identifies the program. It recognizes its own confirmed temporary feedback changes.
- **250 ms is not an extra delay after detection**, and it is not an artificial delay on ordinary pad/key events. MIDI release handling runs separately from the query worker.
- Total visible switching time includes the polling wait, MIDI response time, re-identification, and UI processing. Wait for the header to show the new verified program before using mapped controls.
- The worker waits rather than busy-spinning. Some CPU and MIDI traffic is added, but **CPU usage and timing have not been benchmarked**; no measured percentage or guaranteed response time is claimed. The interval is currently fixed in code, not a menu preference.

On a physical switch, old feedback ownership is discarded: VibeConsole must not restore the previous program's snapshot into the newly selected preset. On a VibeConsole-requested switch, outgoing owned feedback is restored before selection. Held keys/toggles start inactive in the incoming program; explicit pause is preserved. Hardware changes during configuration can interrupt the operation rather than silently save under a different context.

External octave/arpeggiator/tempo edits can invalidate exact matching. This affects the remaining piano-transposition test, which needs review before continuing. Automatic following belongs to the interactive session; the optional legacy daemon still has the narrower original-pad scope described below.

## Terminal commands

Highlight commands with Up/Down and press Enter. These are menu entries, not shell commands. They are grouped as Run, Configure, Programs, Feedback, Wispr Flow and Inspect; Up/Down skip the group headings, and the Details pane describes the highlighted command.

| Menu command | Purpose |
|---|---|
| `/configure` | Learn controls; edit, save, or explicitly remove assignments |
| `/run` | Enable the verified program's mappings; retain applicable safety confirmations |
| `/resume` | Resume paused output with fresh-activation guards |
| `/pause` | Release synthetic keys, cancel pending mapped actions, and remain paused |
| `/release-all` | Clear holds/toggles while retaining running mode |
| `/list` | Inspect saved mappings and toggle states |
| `/get` | Inspect an observed Note control in the selected context |
| `/detect` | Show Bank A / Bank B pads, knobs, joystick and other notes live, without mapped effects |
| `/prog-select` | Select and verify an actual hardware program |
| `/program-name` | Review, rename, and verify a stored program name |
| `/feedback-idle` | Configure the selected program's idle musical settings |
| `/feedback-check` | Run the bounded original-program feedback/restoration trial |
| `/start-flow` | Prepare the virtual keyboard, start Flow if absent, and check helper capture |
| `/repair-flow` | Pause output and explicitly request targeted graceful Flow reopening |
| `/quit` | Release keys, attempt owned-feedback restoration, and restore the terminal |

Esc cancels/returns; Ctrl+C performs handled shutdown. Configuration has learning and saved-assignment panes, with Tab/Left/Right navigation. The interface supports narrow layouts, a minimum 40×16 terminal size, and `NO_COLOR=1`. Only changed screen rows are redrawn.

Every screen uses one tiled layout: a header, the Commands pane, a Details pane for the active screen, errors and notices, a Live pane for held controls and toggles, and a key-hint status line. At 160+ columns the three panes sit side by side; at 100–159 Live sits below Details; narrower terminals stack them, showing only the active command and hiding Live while another screen is open. The header shows MIDI connection, input activity, selected program, verification, and run/pause state. Quick Note taps remain visible for 200 ms. The Toggles pane shows actual synthetic latch state; `/list` shows the full list when the preview is too small. These indicators do not establish application response or physical LED control. Routine keyboard startup diagnostics no longer print over the menu; selecting Run before identification shows guidance instead of treating missing selection as an operation failure.

## Configure controls and shortcuts

1. Open `/configure` and choose Learn a pad, Learn a piano key, Learn a knob, or Learn joystick. Observe the requested press/release or calibration sequence.
2. Choose the action family: Keyboard shortcut, Launch application, or Fedora system action, where supported.
3. For a shortcut, use Select keys or Record shortcut. Review the chord, choose the supported behavior and optional feedback, then save.
4. Save and configure another, or Save and finish. Cancel and failed validation preserve earlier successful saves; removal requires confirmation.

| Input | Supported behavior and conditions |
|---|---|
| Pads | Learn Note-mode pads in either bank; hold/toggle shortcuts or discrete Trigger actions |
| Piano | Identity follows emitted channel/note; arpeggiator Off, explicit Run confirmation, and no musical feedback in that context |
| Knobs | Confirm absolute/no-wrap encoding and both endpoints; assign increasing/decreasing directions and a movement step |
| Joystick | Observe each axis's endpoints and released neutral; configure supported sides, neutral region, hysteresis, and physical labels |

Original bank labels describe the tested channel-10 notes 36–51; they are not universal preset identities. Custom Note-mode pads can be learned. Identical pad/piano messages cannot become separate assignments.

Knobs emit at most one immediate shortcut pulse per sample crossing a configured movement threshold. Stationary input does nothing, the first post-reset sample seeds position, and large jumps do not queue a burst. Joystick directions hold shortcuts until returning through the release threshold; independently mapped axes combine on diagonals. Neutral must be observed after resume/reconnect. A one-sided axis cannot provide distinct Up/Down actions if both directions emit indistinguishable messages. Motion feedback is disabled.

| Shortcut behavior | Press | Physical release | Next fresh press |
|---|---|---|---|
| Hold | Hold assigned keys | Release this control's ownership | Hold again |
| Toggle | Latch assigned keys | Keep them held | Release the latch |

Shared keys remain down until their final owner releases them. Modifiers go down before other chord keys and come up afterward. Pressure and duplicate presses do not acquire extra ownership. Startup/resume/program transitions/reconnect never replay old toggles; an initial press/release may only arm a Note control.

Recording reads physical keyboard devices only for the capture step, excludes virtual keyboards, and keeps no keystroke log. Release all keys before recording, then press/release one chord. Use, Retry, and Cancel happen after capture closes; unfinished capture times out after 60 seconds. Recording **does not suppress real desktop shortcuts**. Escape can be recorded; choose Ctrl+C through menus because it exits the session.

### Supported shortcut keys

| Group | Keys |
|---|---|
| Letters, digits, function keys | `A`–`Z`, `0`–`9`, `F1`–`F12` |
| Modifiers | `Ctrl`, `Shift`, `Alt`, `Meta`, `RightCtrl`, `RightShift`, `RightAlt`, `RightMeta` |
| Editing | `Escape`, `Enter`, `Tab`, `Space`, `Backspace`, `Delete`, `Insert`, `CapsLock` |
| Navigation | `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right` |
| Punctuation | `Minus`, `Equal`, `LeftBracket`, `RightBracket`, `Backslash`, `Semicolon`, `Apostrophe`, `Grave`, `Comma`, `Period`, `Slash` |

Aliases: `Control`/`LeftCtrl` → `Ctrl`, `LeftShift` → `Shift`, `LeftAlt` → `Alt`, `Super`/`Win`/`LeftMeta` → `Meta`, `AltGr` → `RightAlt`, `Esc` → `Escape`, `Return` → `Enter`, `Del` → `Delete`, and `Ins` → `Insert`.

Names represent physical keyboard keys; the desktop layout determines the resulting characters. Shortcuts are simultaneous held keys, with no timed macros, text expansion, or shell commands.

## Application and audio actions

Launch application lists installed visible desktop entries by name and saves their exact identifier/path. GIO performs native desktop launching; VibeConsole does not run a parsed `Exec` line as a shell command. A successful launch request is separate from an observed window or application readiness. Start/Repair Wispr Flow are also selectable application actions.

Audio choices are **Output volume up**, **Output volume down**, **Output mute**, and **Microphone mute**. Volume steps are selectable from 1–100%; ordinary output volume is capped at 100%. `wpctl` resolves the current default sink/source at activation, changes that exact object, and reads it back. Calibrated knob directions support audio actions per movement step.

These actions use **Trigger**, not synthetic hold/toggle semantics. A fresh pad/key press makes one request; holding, pressure, duplicate input, and stationary knobs do not repeatedly activate it. Configuration and explicit pause suppress mapped effects. Bounded/cancellable dispatch keeps key release responsive; pause/disconnect cancel pending work. Cleanup does not close launched applications or undo intentional audio changes.

## Program names and musical feedback

`/program-name` accepts **1–16 printable ASCII characters**, requires review, changes only the stored name field, and verifies readback. Empty, unsupported, and overlong names are rejected without truncation. Duplicate names are allowed because numeric slots identify mappings. Stored renaming does not itself refresh current RAM; reselect deliberately when that refresh is wanted. Power-cycle persistence is separate from successful readback.

`/feedback-idle` saves independent preferences for each compatible program. Old single-program idle settings migrate to Program 1 only; other programs default to Unchanged. Optional per-pad feedback supports octave offsets or absolute octave (-4..+4), tempo (1..300 BPM), and arpeggiator On/Off.

Offsets use the fresh controller baseline and do not accumulate. The most recently activated remaining owner wins independently for each setting; release falls back to the remaining owner or idle settings. Active tempo feedback forces arpeggiator On. Idle tempo with arpeggiator Off may be stored without requesting blinking.

These are **real musical settings**, not independent pad LEDs. Temporary feedback writes current RAM (protocol target 0), not stored programs 1–8. Piano contexts reject musical feedback. Pause returns compatible feedback to idle; handled quit restores owned original fields when the controller remains available. Fresh reads protect unrelated fields and reject stale settings. Disconnect cannot guarantee restoration over a missing connection; forced termination cannot execute cleanup.

`/feedback-check` remains a bounded trial for the confirmed original Program 1 Note-mode setup. Its snapshot, sent-update, readback, and restoration messages are separate from physical observations. Protocol “Program 0” means RAM, not a ninth selectable program. The tested octave +1 was not retained across USB power loss; computer-reboot and tempo/arpeggiator persistence remain unmeasured.

## Wispr Flow

Use a pad mapped to **Shift / hold** with Flow's Shift push-to-talk binding. `/start-flow` creates/retains the paused virtual keyboard, starts Flow if absent, and checks whether its helper opened the current device. Starting an already-running Flow is not a repair.

`/repair-flow` first releases synthetic keys and pauses, then requests the intended app's graceful quit, waits boundedly for the app/helper to stop, and launches it again. It does not force-kill processes, reset settings, modify third-party files, or restart unrelated apps. Both commands finish paused; check physical Shift and actual dictation before explicitly resuming.

Ordinary configuration, pause/resume, context changes, and MIDI reconnect retain the virtual keyboard instead of implicitly restarting Flow. If recovery fails, its stage and manual fallback are reported: fully quit Flow, keep VibeConsole's keyboard ready, reopen Flow, test physical Shift, then resume. Helper capture is not proof of listening or dictated text. The inspected adapter targets the installed `/usr/lib/wispr-flow` package; application updates may require rechecking it. Start/repair and ordinary continuity passed user acceptance in guides 13–15. A fresh VibeConsole keyboard is still missed when Flow was already running; explicit repair recovers it and intentionally leaves VibeConsole paused, including when triggered from a pad. Use `/resume` afterward.

## CLI commands and installation

Use `./target/debug/vibeconsole` for the current local build. After installation, use `vibeconsole`. Equivalent Cargo invocation: `cargo run --locked -- COMMAND` (omit `-- COMMAND` for the default session).

| CLI command | Purpose |
|---|---|
| `vibeconsole` | Interactive session with automatic detection and running intent |
| `vibeconsole --paused` | Interactive detection with output explicitly paused |
| `vibeconsole run` | Start the interactive run workflow |
| `vibeconsole list` | Read saved assignments without opening MIDI |
| `vibeconsole detect` | Direct Note/event detection view |
| `vibeconsole get` | Direct Note inspection in the original context; use interactive `/get` for others |
| `vibeconsole configure` | Direct configuration menus; use the interactive session for program-aware setup |
| `vibeconsole feedback-read` | Read/validate current RAM musical settings without changing them |
| `vibeconsole doctor` | Inspect session, device, configuration, and prerequisite availability |
| `vibeconsole install` | Copy the invoking executable to `~/.local/bin/vibeconsole` |
| `vibeconsole uninstall` | Remove managed installation/service; preserve mappings |
| `vibeconsole startup-enable [--feedback] [--start-flow]` | Opt into the managed user service without starting it immediately |
| `vibeconsole startup-start` | Start the managed service |
| `vibeconsole startup-status` | Show service status |
| `vibeconsole startup-stop` | Stop the service |
| `vibeconsole startup-disable` | Disable and stop the service |
| `vibeconsole daemon [--feedback] [--start-flow]` | Headless original-pad runtime used by the optional service |
| `vibeconsole --help` / `vibeconsole help` | Show CLI help |

Install/update after quitting the running VibeConsole session:

```bash
cargo build --locked
./target/debug/vibeconsole install
~/.local/bin/vibeconsole doctor
```

Ensure `~/.local/bin` is on PATH. Rebuilding alone does not update the installed executable. Installation does not enable/start login startup. One runtime owns MIDI, keyboard output, and editing; a second session refuses the conflict, while `list` remains readable. Keep ordinary editing in the same foreground session to preserve keyboard continuity.

### Optional login service

The legacy service supports the **original Program 1 pad setup**, not interactive eight-program following. Use the interactive session for the current multi-program workflow. `--feedback` explicitly confirms the original hardware setup; `--start-flow` opts into keyboard-before-Flow startup and does not implicitly repair an already-running Flow.

```bash
vibeconsole startup-enable
vibeconsole startup-start
vibeconsole startup-status
journalctl --user -u vibeconsole.service
vibeconsole startup-stop
vibeconsole startup-disable
```

Only opt in when this scope matches your setup. Live login behavior remains unverified. Service configuration is inspectable/removable, unmanaged units are not overwritten, and mappings survive uninstall. Stop the service before starting a foreground owner; a new process creates a new keyboard and may require Flow discovery/recovery.

## Saved configuration

Mappings live at `$XDG_CONFIG_HOME/vibeconsole/mappings.tsv`, or `~/.config/vibeconsole/mappings.tsv` when the variable is unset/empty. The configuration directory must be absolute.

VibeConsole was previously named **KeyAI**. On first start, an existing `keyai/mappings.tsv` is copied to the new location (the old file stays as a backup) and old `keyai-mappings-v*` headers remain readable. An earlier install is replaced with `keyai uninstall`, then `vibeconsole install` with the same startup options.

Versions 1–4 remain readable; successful saves use **version 5 TSV**, including per-program idle preferences, numeric contexts, labels, input calibration, actions, shortcuts, and feedback. Validation precedes saving; a private temporary file is written/synced before replacing the previous configuration. Failed saves preserve the prior valid file. Active holds and toggle states are never saved.

Do not concurrently edit an active configuration. If an interrupted save leaves `mappings.tmp`, inspect it and confirm no editor is saving before removing that staging file. Power-loss recovery is not established. The [manual test setup](archive/V2.0/tests-to-do/README.md) uses a separate configuration copy so test assignments do not overwrite everyday mappings.

## Troubleshooting and limits

| Symptom | Action or explanation |
|---|---|
| No controller / busy port | Check USB and `amidi --list-devices`; close competing MIDI readers/editors |
| Current program unresolved | Wait for detection; identical presets or edited RAM may require explicit `/prog-select` |
| First tap does nothing | It may only arm the Note control; release, then press again |
| Unexpected pad identity | Inspect emitted channel/note/release; do not assume the original bank labels apply to every preset |
| Cannot create keyboard | Check `/dev/uinput` access; do not run the full application as root |
| Cannot record shortcut | Check the named physical keyboard node's read permission or select keys through menus |
| Shift works in an editor but not Flow | Test `/start-flow` or explicit `/repair-flow`; verify actual push-to-talk separately |
| Installed command behaves differently | Rebuild, quit VibeConsole, then install the new local executable |
| Program changes feel delayed | Allow polling plus response/re-identification time; check the verified header before playing mapped controls |

Unsupported or unverified areas include relative/wrapping knobs, absolute-volume mapping, CC/Program Change pad holds, joystick system actions, indistinguishable joystick directions, pedals, brightness/media/lock actions, timed macros, shell commands, independent pad LED control, and direct Flow listening-state integration. There is no GUI. Other desktop environments/controller models have not been verified.

Suspend/resume, forced-kill recovery, real output-backend failure recovery, and full daily-use acceptance remain open. Exact program matching also means externally transposed/edited RAM may need deliberate recovery; do not treat the older piano-transposition procedure as approved under the new monitor.

## Verification and acceptance

Latest recorded software checks passed: **45 Rust tests, 13 isolated terminal checks, installation checks, formatting, check, and build**. Reproduce with:

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
cargo build --locked
python3 scripts/check-terminal.py
python3 scripts/check-installation.py
```

Rust checks use recorded/simulated MIDI, keyboard, feedback, application/audio, and Flow operations. PTY checks isolate configuration/runtime paths and block access to real MIDI. Installation checks use isolated paths/fake service requests. These checks do not establish physical indicator changes, actual desktop effects, or working dictation. The release-timing check needs local Unix socket support.

| Area | User-observed acceptance |
|---|---|
| Existing pads, recording, hold/toggle/shared keys, pause/release, feedback/restoration and MIDI reconnect | Approved within the recorded original/ticket-specific scopes |
| Program contexts and piano hold/toggle/shared ownership | Approved; guide 07 passive arpeggiator observation approved; octave transposition remains pending |
| Hardware selection, naming/validation/restore, independent idle and feedback cleanup | Guides 01–05 approved for the tested build; later automatic-following behavior is separate |
| Automatic detection/following while paused | **Guide 23A approved:** physical Program 3 detected at startup; physical Program 1 followed while retaining PAUSED |
| Automatic following while running, offline switch/restart, and held cleanup in the new flow | **Guide 23B/C approved** |
| Knobs, app launching, output audio, microphone mute, Flow start/repair/continuity, and installed startup/ownership | Guides 08 and 10–16 approved; Flow-first startup still requires repair, which intentionally finishes paused |
| Joystick, transposition, final integration, controlled failures, forced termination, suspend and remaining persistence | Joystick guide 09 partially observed; USB reconnect fix awaits live retest. Others unperformed or blocked; optional login guide 17 deferred |

Use [tests-to-do](archive/V2.0/tests-to-do/README.md) for individual procedures and approval checkboxes, [the ticket audit](archive/V2.0/tests-to-do/TICKET-AUDIT.md) for dependencies, and [acceptance.md](archive/V2.0/tests-to-do/acceptance.md) for evidence. Only update acceptance checkboxes after the user's approval. Current behavior and boundaries are in [SPEC.md](archive/V2.0/SPEC.md) and the [ticket index](archive/V2.0/tickets/README.md); earlier MVP evidence remains in [archive/SPEC.md](archive/SPEC.md).
