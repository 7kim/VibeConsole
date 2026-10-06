# 03 — Learn a pad and save its shortcut assignment

**What to build:** A terminal configuration flow: enter configure mode, press a pad, inspect its assignment, enter a key or combination, choose hold or toggle, and save. Restarting the CLI reloads and displays the same assignments without activating shortcuts.

**Blocked by:** 02 — Observe pad presses and releases in the terminal.

**Status:** complete — automated checks passed and the user verified physical configure/restart/inspect for Bank A Pad 1 mapped to Shift/hold. Keyboard output belongs to milestone 4.

- [x] Provide learn/identify and inspect operations equivalent to get_button, and assign/replace operations equivalent to set_button; `get` learns/inspects, `configure` learns/inspects/assigns, and `list` displays loaded mappings.
- [x] Bind one shortcut and one behavior to each learned input using the stable identity established in ticket 02.
- [x] Accept individual keyboard keys and simultaneous combinations such as Shift or Ctrl+Alt+K. Define supported key names and reject unknown names or invalid combinations clearly.
- [x] Allow hold and toggle assignments; these are held-key behaviors rather than timed macros or commands.
- [x] Keep learn/configure mode free of shortcut execution, including when the learned pad already has a mapping.
- [x] Save assignments in a readable local format in the user's configuration directory and validate them on load.
- [x] Replace the saved configuration only after a successful temporary-file write; invalid input or a failed save preserves the prior valid configuration.
- [x] Verify successful round trips, replacing an existing assignment, invalid configuration rejection, and preservation on a failed save.
- [x] Persist assignments only: no active pad, held-key, or toggle state.
- [x] Run formatting, compile, and relevant tests. Generate no desktop shortcuts.
- [x] Configure a physical pad, exit, restart, and inspect the saved assignment on that pad.

## Verification record

- Rechecked USB `09e8:1049`, ALSA input `hw:0,0,0` (MPK mini 3 MIDI 1), and GNOME Wayland before implementation. Device enumeration is verified; no physical pad press was requested or observed in this implementation run.
- `cargo fmt --check`, `cargo check`, and `cargo test` pass. Two built-in tests cover input normalization and saved mappings. The configuration behavior check exercises both banks, replacement, canonical shortcut names, invalid input/configuration, duplicate identities, and an occupied staging file; unsuccessful saves preserve the previous file.
- A simulated CLI check with temporary `lsusb`/`amidi` commands and a temporary configuration directory passed learning, inspecting existing mappings, replacing an assignment, restart loading, both banks, cancellation, terminal EOF, invalid shortcut/behavior/configuration, and failed-save preservation. These simulated messages do not establish physical learning.
- No dependencies were added. Configuration uses a versioned tab-separated format in the XDG configuration directory. A private sibling staging file is written and synced before rename; only assignments are persisted.
- Key names cover the documented Linux keyboard subset. Actual output, physical keyboard interaction, shared ownership, hold/toggle execution, and cleanup belong to milestone 4.
- Use one configuration editor at a time. An interrupted save may leave `mappings.tmp`; inspect/remove it only when no editor is saving. Power-loss recovery and concurrent editing are not established by these checks.

## User-provided live verification — 2026-10-06

The user supplied the terminal output from three successful, separate CLI invocations:

1. `cargo run -- configure` identified USB `09e8:1049` and ALSA input `hw:0,0,0`, learned a physical Note On for Bank A Pad 1 (channel 10, note 36), then saved `Shift` with behavior `hold` to `/home/developer/.config/keyai/mappings.tsv`.
2. `cargo run -- list` loaded and displayed that same assignment after the configure process exited.
3. `cargo run -- get` loaded the saved assignment, learned the same physical pad again, and displayed `Current assignment: Bank A Pad 1 | USB 09e8:1049 | Note channel 10 note 36 | Shift | hold`.

This satisfies the physical learn/save/restart/inspect acceptance check. Physical replacement, Bank B configuration, and toggle configuration were not exercised in this log; those paths have automated/simulated coverage above. No keyboard output or Wispr Flow behavior is established by this configuration check.

**Verification:** Configure a real pad, exit, reload, and inspect its assignment. Use temporary configuration storage for automated save/load and failure checks.
