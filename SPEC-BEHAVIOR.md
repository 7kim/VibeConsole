# KeyAI V3 specification — behavior changes

Status: draft for review (2026-10-07). V2 behavior, evidence and acceptance live in `archive/V2.0/` (local only; `archive/` is git-ignored). Presentation changes are in [SPEC-UI.md](SPEC-UI.md). **Principle:** users never need another application. Anything the controller supports is configured from inside KeyAI; nothing in this spec may tell the user to use the Akai editor or another tool. Every V2 safety rule stays in force unless a section here explicitly changes it: held-key release before transitions, fresh activation without replay, verified program before hardware feedback, and atomic saves that preserve the last valid configuration.

## Problem Statement

Daily use exposed friction V2 left in place: there is no way to clear configurations, `/run` and `/resume` overlap, KeyAI's own menus stop responding to arrows while a pad holds Shift, absolute knobs stop at their endpoints when mapped to arrows, Wispr Flow must be started in the right order by hand, and the login service supports only Program 1 with extra flags.

## Solution

Six targeted changes, each delivered as its own ticket:

1. Reset all settings.
2. One Run/Pause command.
3. Menu navigation that ignores modifiers.
4. Relative (endless) knob mode.
5. Wispr Flow starts with KeyAI.
6. A login service that follows the controller's program like the interactive session.

Plus one outstanding measurement from V2.

## User Stories

1. As a user, I can clear all KeyAI program configurations in one confirmed step, so I can start fresh without editing files.
2. As a user, I have one Run/Pause control, because KeyAI already starts running and `/resume` duplicates `/run`.
3. As a user, I can navigate KeyAI's menus while a pad is holding Shift, so a latched modifier does not lock me out.
4. As a user, I can map a knob in relative mode to arrow keys and turn it indefinitely, so it never stops at an endpoint.
5. As a user, starting KeyAI also starts Wispr Flow in the right order, so push-to-talk works without manual repair.
6. As a user, login startup follows whichever program the controller is on, with feedback, without special flags.

## Implementation Decisions

### 1. Reset all settings

- New command **Reset settings** (Configure group). It clears every mapping and idle-feedback setting for all eight programs in KeyAI's configuration file.
- **Confirmation:** the user must type `reset`. Esc or anything else cancels with no change.
- **Backup:** before writing, KeyAI copies the current file to `mappings.tsv.bak-<YYYYMMDD-HHMMSS>` beside it and shows the path. The reset itself uses the existing atomic save.
- **Safety:** release all synthetic keys and pause first, restore owned hardware feedback, then reset. The session stays paused afterward.
- **Never writes the controller:** stored program presets and names are untouched.

### 2. One Run/Pause command

- Replace `/run` and `/resume` with a single command whose label shows the action: **Run** when paused, **Pause** when running. `/pause` remains as a direct alias.
- Run keeps V2 semantics: piano confirmation when required, verified program before feedback, and fresh press before activation.
- **Command-line flags:** `--paused` stays. The `run` subcommand stays for scripts.

### 3. Menu navigation ignores modifiers

- KeyAI's own menus treat Shift/Ctrl/Alt combined with Up, Down, Enter, Esc and Space as the plain key. This matters because a latched synthetic Shift reaches the terminal too.
- Text-entry prompts (program names, Reset confirmation) still receive the typed characters as-is.
- The synthetic modifier keeps reaching other applications unchanged.

### 4. Relative (endless) knobs

- **KeyAI sets the knob's mode on the controller itself.** No external editor is involved.
- **Evidence so far:** each stored program holds eight 20-byte knob entries starting at payload offset `0x54`: `[mode?, CC, min, max, name×16]`. In the captured payloads Knob 1 is CC 70 (matching V2's live capture) and the first byte is `0` for every knob, which is most likely Absolute.
- **Gate (hardware first):** on one knob of one test program, record the original payload, write the candidate relative value to that byte through the verified stored-preset write used by program rename (fresh read, change only that byte, write, readback), reselect the program, and capture messages for slow and fast turns in both directions. Record the encoding (likely offset-64 or two's-complement). If the knob does not become relative, write the original payload back and verify it; relative support then stays unsupported.
- **Learning:** Learn knob offers **Absolute** (V2 behavior: endpoints, no wrap) or **Relative** (unlimited). Choosing Relative switches that knob on the controller (as above), then confirms the mode from observed messages. Ambiguous captures are rejected, as in V2.
- **Relative operation:** each received tick produces one pulse in its direction, scaled by the chosen step. Nothing is sent while the knob is stationary, and nothing replays after pause or reconnect.
- **Saved data:** the mode is stored with the mapping (configuration migration). Existing absolute knob mappings migrate unchanged.
- **Program following:** changing a knob's mode changes the stored program and RAM together, so automatic program detection still matches afterward.

### 5. Wispr Flow starts with KeyAI

- On interactive start, and in the login service, KeyAI creates its keyboard first and then:
  - **Flow not running:** start Flow through its installed launcher (V2 `/start-flow` path).
  - **Flow already running:** Flow cannot see the new keyboard, so KeyAI performs the V2 graceful repair (quit, reopen) **once** at startup, before enabling output.
- **When capture is confirmed** (Flow's helper opens the current keyboard): continue in the requested state (running by default, or `--paused`), with fresh-press arming and no replay.
- **On failure:** report the stage and the manual recovery, stay paused, and never retry in a loop.
- **Configuration:** a single setting, `Start Wispr Flow with KeyAI`, default **On**, toggled in the Flow command group and shared by session and service. If Flow is not installed, startup continues without it and the header says so.
- `/start-flow` and `/repair-flow` remain as manual commands.

### 6. Login service follows the controller program

- The service uses the same automatic program detection as the interactive session, rather than the legacy Program-1-only path. Feedback is enabled once a program is verified, so `--feedback` is no longer needed. Flow startup follows decision 5.
- **Old flags:** `startup-enable` writes a unit without flags. A unit still passing `--feedback` or `--start-flow` keeps working; the daemon accepts and ignores those flags with a one-line notice.
- **Behavior:** unmatched or ambiguous programs keep output inactive and are reported in the journal. The service never prompts and never selects a program itself.
- **Piano programs:** they require interactive Run confirmation, so the service leaves piano mappings inactive and logs why.

### 7. Outstanding V2 measurement

- Finish guide 22 condition 2 (archive): computer-reboot persistence of octave, tempo and arpeggiator, measured with `feedback-read` before KeyAI starts.

## Testing Decisions

- **Reset:** a Rust test covers backup creation, the cleared configuration, cancel paths and failed-write preservation. No hardware write occurs (fake transport records none).
- **Run/Pause:** existing run/pause/resume tests are updated, keeping piano confirmation and fresh-press arming.
- **Modifiers:** terminal tests feed Shift+Arrow, Ctrl+Enter and similar combinations and assert plain navigation, with text prompts unchanged.
- **Relative knobs:** recorded-sequence tests from the captured encoding cover per-tick pulses, fast turns, direction reversal, stationary silence, pause/reconnect without replay, and migration of absolute mappings.
- **Flow at startup:** lifecycle tests with recorded stages cover running/not-running/not-installed/failure, the single repair, and the final run/pause state. Automated checks never launch or stop Flow.
- **Service:** unit text and daemon tests cover program detection, unmatched/piano inactivity, and old-flag tolerance.
- **Live, one guide per ticket:**
  - Reset with backup restore
  - Shift-latched menu navigation
  - Relative knob → arrows in an editor past 127 ticks
  - KeyAI start with Flow closed and with Flow already open → push-to-talk works
  - Login with the controller on two different programs
  - Guide 22 reboot

## Out of Scope

- UI layout and naming (see [SPEC-UI.md](SPEC-UI.md)).
- Automatic Flow repair outside startup (manual `/repair-flow` stays).
- Running transposed piano keys, distinct joystick Up/Down on this controller, and suspend hooks; these remain as recorded in V2.

## Open Questions

None. Resolved 2026-10-07: Reset needs no restore command (the backup file is copied back by hand); KeyAI switches knob modes itself; Flow already running at startup is always repaired automatically once.
