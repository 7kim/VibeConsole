---
name: vibeconsole-profiles
description: Use when asked to make, suggest or review a VibeConsole app profile (pads and buttons that change while one app is focused), for the Akai, the Xbox controller, or both.
---

# Making a profile

1. **Ask first** (skip what the user already said): which app, what it should do, and which devices: the Akai MPK mini 3, the Xbox controller, or both. The same functions can be mapped on both.
2. **Run `vibeconsole describe`.** Use only controls it lists, by the names it prints. Never invent a control and never write a raw note number.
3. **Write the profile** in the [profile format](../vibeconsole/reference/formats.md): one override per control per device, a plain `shortcut` action with `Key+Key` names, behaviour `hold`. Copy a line from an [example](../vibeconsole/examples/browser-tabs.profile) and change only the keys.
4. **Say it back in plain words** before anything is saved: "Bank A Pad 1 on the Akai presses Ctrl+Tab in Firefox." The user confirms; VibeConsole shows the same restatement on its review screen.
5. **Never save over a working setup.** Profiles are imported whole or not at all, and the user keeps a backup of `mappings.json` before trying one.

## Do not

- Map LB or the Xbox button: VibeConsole keeps them for layer 2 and profile cycling.
- Give an override a light, octave or tempo: profile overrides carry none.
- Put a desktop shortcut that GNOME or Wayland swallows without saying so (see `vibeconsole-recipes`).

Shared rules: [Safety Rules](../vibeconsole/reference/safety-rules.md).
