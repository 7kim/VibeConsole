# KeyAI

Turn an **Akai MPK Mini MK3** into a pad-operated keyboard shortcut tool on Fedora. KeyAI is a Rust terminal application with saved mappings, hold and toggle modes, and keyboard output tested on GNOME Wayland.

## Setup

You need a Rust toolchain with Cargo and Rust 2024 edition support, the MK3 connected by USB, and these Fedora utilities:

```bash
sudo dnf install alsa-utils usbutils acl
cargo build --locked
```

Run commands from the project directory as your desktop user. KeyAI uses `amidi` for MIDI input, `lsusb` for device identification, and Linux uinput for keyboard output.

Check that the controller is visible:

```bash
lsusb -d 09e8:1049
amidi --list-devices
```

KeyAI selects the matching MIDI input automatically. The verified pad program sends Note messages on channel 10: Bank A pads 1–8 use notes 36–43; Bank B pads 1–8 use notes 44–51. Other pad programs, CC mode, and Program Change mode are outside the supported configuration.

### Keyboard permissions

The `run` command requires read/write access to `/dev/uinput`:

```bash
test -r /dev/uinput && test -w /dev/uinput && echo "uinput access ready"
```

The tested desktop already had access through Wispr Flow's udev rule. If the check fails, load the module and grant your current user temporary access:

```bash
sudo modprobe uinput
sudo setfacl -m "u:$(id -un):rw" /dev/uinput
```

Repeat the access check afterward. This ACL may need reapplying after reboot or device-node recreation. KeyAI does not install persistent permission rules.

## Configure a pad

Stop any running KeyAI instance before configuring, then run:

```bash
cargo run -- configure
```

1. Press a pad in either bank. KeyAI displays its identity and any existing assignment.
2. Enter a shortcut, such as `Shift`, `Ctrl+C`, or `Ctrl+Alt+K`.
3. Enter `hold` or `toggle`.

The assignment is validated and saved. Configure the same pad again to replace its assignment. A blank shortcut cancels without saving. Learning and configuration do not execute mapped shortcuts.

Inspect the saved mappings or press a pad to inspect just its assignment:

```bash
cargo run -- list
cargo run -- get
```

## Run shortcuts

```bash
cargo run -- run
```

Wait for `Ready`, then use your configured pads. At least one saved mapping is required.

| Behavior | Pad press | Physical release | Next press |
|---|---|---|---|
| `hold` | Holds the assigned keys | Releases this pad's ownership | Holds again |
| `toggle` | Latches the assigned keys | Keeps them held | Releases this pad's ownership |

A combination holds its keys together. Modifiers go down before other keys and come up afterward. Duplicate presses while a pad is down and pressure changes do not activate extra holds or toggles.

Overlapping mappings share keys: with one pad holding `Ctrl` and another holding `Ctrl+C`, releasing the second pad releases `C` while `Ctrl` stays held until its final owner releases it. This applies to hold and toggle mappings.

The terminal reports `ACTIVE: keys held` and `INACTIVE: ownership released`. These describe KeyAI's synthetic key state; application acceptance must be observed separately.

Use **Ctrl+C** to stop. Exit and device disconnect release app-owned keys; handled errors and signals attempt the same cleanup. After disconnecting, reconnect and start KeyAI again. Every startup begins inactive, including all toggles.

### Wispr Flow push-to-talk

Configure a pad as `Shift` / `hold` and use Wispr Flow's Shift push-to-talk binding.

1. Start `cargo run -- run` and wait for `Ready`.
2. Fully quit and reopen Wispr Flow so its helper discovers the new virtual keyboard.
3. Hold the pad to talk, then release it to finish.

Repeat the Flow restart after each new KeyAI run. This startup order and Shift press/release behavior were verified on the tested GNOME Wayland session. `Shift` / `toggle` latching was verified through desktop typing; Wispr listening behavior for that mode was not established.

## Commands

| Command | Purpose |
|---|---|
| `cargo run -- detect` | Display pad identity, raw MIDI, press/release, and ignored events; also the default command |
| `cargo run -- list` | Display saved mappings without opening MIDI |
| `cargo run -- get` | Learn a physical pad and inspect its saved assignment |
| `cargo run -- configure` | Learn a pad and save or replace its assignment |
| `cargo run -- run` | Operate saved hold/toggle mappings; the only command that generates keyboard input |
| `cargo run -- --help` | Display command help |

## Supported keys

Join names with `+`. Names are case-insensitive, surrounding spaces are accepted, and duplicate or unknown keys are rejected.

| Group | Names |
|---|---|
| Letters, digits, function keys | `A`–`Z`, `0`–`9`, `F1`–`F12` |
| Modifiers | `Ctrl`, `Shift`, `Alt`, `Meta`, `RightCtrl`, `RightShift`, `RightAlt`, `RightMeta` |
| Editing | `Escape`, `Enter`, `Tab`, `Space`, `Backspace`, `Delete`, `Insert`, `CapsLock` |
| Navigation | `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right` |
| Punctuation | `Minus`, `Equal`, `LeftBracket`, `RightBracket`, `Backslash`, `Semicolon`, `Apostrophe`, `Grave`, `Comma`, `Period`, `Slash` |

Aliases: `Control`/`LeftCtrl` → `Ctrl`, `LeftShift` → `Shift`, `LeftAlt` → `Alt`, `Super`/`Win`/`LeftMeta` → `Meta`, `AltGr` → `RightAlt`, `Esc` → `Escape`, `Return` → `Enter`, `Del` → `Delete`, and `Ins` → `Insert`.

Names represent physical keyboard keys; the desktop layout determines the resulting characters. Shortcuts are simultaneous held keys, with no timed macros, text expansion, or shell commands.

## Saved configuration

Mappings are stored in `$XDG_CONFIG_HOME/keyai/mappings.tsv`, or `~/.config/keyai/mappings.tsv` when `XDG_CONFIG_HOME` is unset or empty. The configuration directory must be absolute.

The readable, versioned TSV contains the device identity, MIDI message type, channel, note, behavior, and shortcut. KeyAI validates it before use. Saves write and sync a private temporary file before replacing the previous configuration; invalid assignments and failed saves preserve the previous file. Active holds and toggle states are never saved.

Use one configuration editor at a time. If an interrupted save leaves `mappings.tmp` beside the configuration, inspect and remove that staging file only after confirming no editor is saving. Power-loss recovery and concurrent editing are not established.

## Troubleshooting

| Symptom | Check |
|---|---|
| Device unavailable or no matching MIDI input | Check the USB connection, `lsusb`, and `amidi --list-devices` |
| MIDI port busy | Stop another KeyAI instance or another process using that MIDI input |
| Pads are not recognized | Use `detect` and confirm channel 10, notes 36–51, and usable release messages |
| Cannot create the virtual keyboard | Check `/dev/uinput` read/write access as described above |
| Terminal shows active Shift, but Flow does not respond | Fully reopen Flow after KeyAI reaches `Ready` and check Flow's Shift binding |
| Save reports an occupied staging file | Check for an active editor before removing a stale `mappings.tmp` |

## Verification and limits

```bash
cargo fmt --check
cargo check --locked
cargo test --locked
```

Five built-in tests cover MIDI framing and normalization, saved configuration and failure preservation, hold/toggle ownership and cleanup, and release delivery during silence. They record keyboard transitions without injecting desktop shortcuts. The release-timing check requires local Unix socket support.

Recorded live acceptance covers both pad banks, saved learning/restart/inspection, Shift push-to-talk, hold and toggle combinations, shared keys, active exit/disconnect, inactive restart, and physical Left Ctrl interaction. Details are in the [specification](SPEC.md) and tickets: [lighting](tickets/01-lighting-feasibility.md), [pad detection](tickets/02-pad-detection.md), [saved mappings](tickets/03-saved-mappings.md), [holds](tickets/04-hold-shortcuts.md), and [toggles](tickets/05-toggle-shortcuts.md).

- External red-pad LED control remains inconclusive; KeyAI provides terminal feedback. The MK3 has no RGB pad support.
- Held-key recovery after forced termination such as SIGKILL, and desktop/application recovery after output-backend failure, remain unverified.
- Pads are the supported controls. Piano keys, knobs, joystick, and pedal mappings are outside this version.
- There is no automatic reconnection, graphical interface, background-service installation, or direct Wispr listening-state integration. Other desktops and controller models have not been verified.
