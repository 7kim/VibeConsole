---
name: vibeconsole-add-controller
description: Use when the user wants to add a new controller to VibeConsole (another MIDI keyboard or pad controller, a gamepad, "set up my new controller"): walks them through choosing it, Detect, Configure and profile export/import, never by editing config files.
---

# Add a controller

The user does the steps; you say which menu row or command comes next and read back what it shows. Never edit the config files (see "Where things live" in the `vibeconsole` hub), and never guess a control's name: take it from Detect.

## What VibeConsole can use

- **A MIDI device** (keyboard, pad controller): yes. Pads, keys, knobs and joysticks get their own mappings per device. Only the Akai MPK mini 3 gets lights, octave and program features.
- **A gamepad** Linux reports as one (Xbox, and most others): yes, under the Xbox controller's button names (Ⓐ is the bottom face button).
- **Any other HID device** (a button box, a foot pedal that isn't MIDI): not yet. Say so; don't work around it.

## A MIDI device

1. **Plug it in, then choose it.** In the session: 5 Devices › MIDI device lists the MIDI devices connected now (and ones used before, marked "not connected"). Pick it; it is added to the list. `vibeconsole doctor` checks only the Akai's MIDI, so this list is where a new device shows.
2. **Relaunch.** The choice takes effect at the next launch: quit the session and open it again (for the login service, `vibeconsole startup-stop`, then `vibeconsole startup-start`).
3. **See its controls.** 5 Devices › /detect in the session shows each press and knob by name; nothing fires there. (`vibeconsole detect` from the command line opens the Akai only.)
4. **Assign controls.** 2 Mappings › /configure: press the control, pick the action. Use `vibeconsole-recipes` or `vibeconsole-profiles` for what to put where.

## A gamepad

1. **Plug it in or pair it** (VibeConsole never pairs devices), then `vibeconsole doctor`: it shows on the Xbox controller line. If that line says "gilrs default mapping · LT and RT may not register", the triggers may not work on this pad; say so.
2. **See its controls.** `vibeconsole describe` lists the Xbox controller's controls by name; 5 Devices › /detect shows each press live.
3. **Tune and assign.** 7 Xbox › Xbox tuning sets sticks, triggers and rumble. 2 Mappings › /configure assigns buttons; its Edit saved assignment lists the built-in default buttons, which can be changed.

## Then

- `vibeconsole list` shows what was saved (no MIDI needed).
- To share or move the setup: 6 Apps & profiles › App profiles › a profile › Export to a file writes it to the `profiles/` folder beside the config; on another machine, App profiles › Import a profile file. 2 Mappings › Presets saves or loads the whole setup by name.
- A control that should fire doesn't: load `vibeconsole-why-silent`.
