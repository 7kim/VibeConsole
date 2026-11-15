---
name: vibeconsole-recipes
description: Use when the user says what they want to do ("I want to code with Claude", "stream", "edit video") rather than naming a pad: turns the goal into a whole setup of pads, lights and actions.
---

# Recipes

Ask which devices and which app, run `vibeconsole describe`, then offer the recipe closest to the goal. Build the profile with `vibeconsole-profiles`.

- **Coding with agents:** pads that send a saved prompt to an agent (`prompt:` action, paste or enter), one pad that starts the next step of a prompt sequence (`prompts:`), and the status lights so you can see which agent needs input.
- **Meetings and streaming:** `microphone-mute` and `output-mute` on two pads, `volume-up` and `volume-down` on a knob or two Xbox buttons.
- **Browsing:** tabs and reload as shortcuts (see the browser example).
- **Launcher board:** one `app-toggle` pad per app (launch, then minimize when focused); `app-toggle:minimize+hold-quit` adds a quit on a one-second hold.
- **Editors and DAWs:** ask for the five shortcuts used most and map them as plain shortcuts on the first pads.

Shortcut caveats: GNOME on Wayland swallows some combinations (Super keys, some Alt+Tab forms) and can't prove that another app's window minimized. Say so rather than promising it.
