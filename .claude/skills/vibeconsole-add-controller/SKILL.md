---
name: vibeconsole-add-controller
description: Use when the user wants to add a new controller to VibeConsole (another MIDI keyboard or pad controller, a gamepad, "set up my new controller"): walks them through doctor, Detect, Configure and profile export/import, never by editing config files.
---

# Add a controller

The user does the steps; you say which command or menu row comes next and read back what it shows. Never write `~/.config/vibeconsole/*.json` or `mappings.tsv` yourself, and never guess a control's name: take it from `vibeconsole describe` or Detect.

## What VibeConsole can use

- **A MIDI device** (keyboard, pad controller): yes. Pads, keys, knobs and joysticks get their own mappings per device. Only the Akai MPK mini 3 gets lights, octave and program features.
- **A gamepad** Linux reports as one (Xbox, and most others): yes, under the Xbox controller's button names (Ⓐ is the bottom face button).
- **Any other HID device** (a button box, a foot pedal that isn't MIDI): not yet. Say so; don't work around it.

## Steps

1. **Recheck what is connected.** Ask the user to plug it in, then run `lsusb` (USB) or check Bluetooth, and `vibeconsole doctor`. Read its lines with `vibeconsole-doctor`. A gamepad shows on the Xbox controller line; `vibeconsole doctor --all` lists the MIDI ports it sees.
2. **See its controls.** `vibeconsole describe` lists every device and control by the name the user sees. For live presses: `vibeconsole detect`, or 5 Devices › /detect in the session. Pressing each control shows its name; nothing fires there.
3. **A MIDI device: choose it.** 5 Devices › MIDI device, pick it (it is added to the list). It takes effect at the next launch: quit and start VibeConsole again (`vibeconsole startup-stop`, then `vibeconsole startup-start` for the login service).
4. **A gamepad:** nothing to choose. 7 Xbox › Xbox tuning sets sticks, triggers and rumble. Its built-in default buttons are listed in 2 Mappings › /configure › Edit saved assignment.
5. **Assign controls.** `vibeconsole configure`, or 2 Mappings › /configure in the session: press the control, pick the action. Use `vibeconsole-recipes` or `vibeconsole-profiles` for what to put where.
6. **Check what was saved.** `vibeconsole list` (no MIDI needed).
7. **Share or move the setup.** 6 Apps & profiles › App profiles › a profile › Export to a file writes it to the `profiles/` folder beside the config; on another machine, App profiles › Import a profile file. 2 Mappings › Presets saves or loads the whole setup by name.

## If something is missing

- Nothing in Detect: the device isn't seen. Recheck the cable and `vibeconsole doctor`; for MIDI also that no other program holds the port (only one VibeConsole runs at a time: `vibeconsole startup-status`).
- A control that should fire doesn't: load `vibeconsole-why-silent`.
