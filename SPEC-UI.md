# KeyAI V3 specification — terminal UI

Status: draft for review (2026-10-07). V2 behavior, evidence and acceptance live in `archive/V2.0/` (local only; `archive/` is git-ignored). Behavior changes are in [SPEC-BEHAVIOR.md](SPEC-BEHAVIOR.md). This spec changes presentation only: no mapping semantics, hardware writes or lifecycle rules change.

## Problem Statement

V2 works on real hardware, but the terminal is hard to read. Commands appear as one flat list, controls are shown by raw MIDI identity (`Note channel 1 note 29`) instead of the pad the user pressed, the edit screen does not show which shortcut is currently saved, detection mixes both banks, and toggle state is hard to scan.

## Solution

One consistent multi-pane layout used by every screen, commands grouped by purpose, physical control names everywhere, and clearer detection/toggle views. Existing behavior, menus and keyboard controls stay the same unless listed here.

## User Stories

1. As a user, I see the same layout on every screen, so I always know where status, commands and details are.
2. As a user, I see commands grouped by category, so I find an action without reading the whole list.
3. As a user, when I edit an assignment, the currently saved shortcut is visibly marked as selected, so I know what I am changing.
4. As a user, `/detect` shows Bank A and Bank B in separate panes with distinct colours, so I can tell banks apart at a glance.
5. As a user, the toggle view reads like `Toggle 1: Bank A Pad 1 (Shift)` with its bulb in a box, so active latches are obvious.
6. As a user, controls are named by their physical position (`Bank B Pad 1`, `Knob 3`, `Piano C4`) instead of raw note/CC numbers, so lists and prompts match what I pressed.

## Implementation Decisions

### Layout (all screens)

- **Style:** keyboard-driven tiled panes in the spirit of Neovim/Omarchy: thin borders, one focused pane, a bottom statusline. One built-in dark palette (Tokyo Night–like) with the existing no-colour fallback.
- **Header:** MIDI connection, current program number and name with verified/unverified state, RUNNING/PAUSED, and Flow state when Flow is set to start with KeyAI.
- **Left pane — commands, grouped:**
  - Run: Run/Pause, Release all, Quit
  - Configure: Configure, List
  - Programs: Program select, Program name
  - Feedback: Idle feedback, Feedback check
  - Wispr Flow: Start Flow, Repair Flow
  - Inspect: Detect
  Group headings are not selectable; Up/Down skip them.
- **Right pane:** context for the highlighted command or the active screen (detection, edit form, toggle list).
- **Footer:** key hints, unchanged (`Up/Down`, `Enter`, `Space`, `Esc`, `Ctrl+C`).
- Errors and notices keep their current wording and appear in the right pane, not a modal that hides status.
- Extend `src/screen.rs` (`frame`, `fit`, `wrap`, `lights`). No TUI dependency is added.
- **Responsive widths (in terminal columns, one column = one character):**
  - **≥ 160:** three panes side by side: commands | details | live controls & toggles.
  - **100–159:** two panes: commands | details, with live controls below details.
  - **< 100:** panes stacked vertically in the same order.
  No horizontal scrolling or truncated status at any width.
- **Colour:** colour is never the only signal. Every coloured element also carries text. The existing plain/no-colour fallback stays.

### Edit assignment (story 3)

- When editing an existing mapping, the selector opens on the saved action and marks it `[x] (saved)`. Moving the cursor does not change the mark until a new choice is confirmed.

### Detection view (story 4)

- Two panes side by side, titled **Bank A** and **Bank B**, each listing its pads with live bulbs. Bank A uses red accents and Bank B green, with the bank name always shown as text.
- Piano, knobs and joystick appear in a third pane below the banks.
- Bank membership comes from the verified program's pad table (see physical names). If the program is unverified, controls are listed by raw identity in a single "Unidentified" pane.

### Toggle view (story 5)

- One line per toggle-capable mapping: `Toggle N: <physical name> (<shortcut>)`, preceded by a boxed bulb, e.g. `[●] Toggle 1: Bank A Pad 1 (Shift)`. Numbering follows the saved mapping order of the current program.

### Physical names (story 6)

- Each stored program on the controller contains its pad, knob and joystick assignments. After a program is verified, KeyAI derives display names from that program's payload: which note belongs to which bank and pad, which CC to which knob.
- **Gate:** locate the pad/knob tables in the payload using the captured payloads in `testdata/` and confirm them against live `/detect` before relying on them.
- **Display only:** saved mapping identities (message, channel, id) are unchanged, so existing configurations keep working without migration.
- **Fallbacks:** if a control's identity is not in the current program's table, or the program is unverified, show the raw identity, e.g. `Note ch1 #29 (not on a pad in Program 2)`. Never guess a name.
- Piano keys show note names, e.g. `Piano C4 (note 60)`.

## Out of Scope

- Any behavior change (see [SPEC-BEHAVIOR.md](SPEC-BEHAVIOR.md)).
- Mouse support, multiple themes or user-configurable colours.
- Renaming individual pads to custom labels (program names already exist).

## Open Questions

None. Resolved 2026-10-07: Flow state is shown only when Flow starts with KeyAI; widths as above.
