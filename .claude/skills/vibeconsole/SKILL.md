---
name: vibeconsole
description: Use when the user mentions VibeConsole, their Akai MPK mini or Xbox controller pads, a profile, mappings, status lights, or asks what a pad or button does. The hub that says what VibeConsole is and which focused skill to load.
---

# VibeConsole

VibeConsole is a Rust terminal app that turns an Akai MPK mini 3 and Xbox controllers into shortcuts, prompts, app launchers and agent status lights on Linux (GNOME, Wayland).

## Start here

- Run `vibeconsole describe` first: it lists the connected devices, every control by the name the user sees, and every action. It is read-only.
- Never say a raw note number to the user. Use the names: "Bank A Pad 1", "Xbox A".
- Load the focused skill for the job:

| The user wants | Load |
|---|---|
| a profile for an app, or a setup for a task | `vibeconsole-profiles`, `vibeconsole-recipes` |
| to know why a pad or button did nothing | `vibeconsole-why-silent` |
| to read `vibeconsole doctor` | `vibeconsole-doctor` |
| agent states and lights | `vibeconsole-agents`, `vibeconsole-lights` |
| to change the code safely | `vibeconsole-safety-review`, `vibeconsole-hardware-test`, `vibeconsole-release` |
| an answer to one of VibeConsole's 0 AI asks | its own `vibeconsole-ask-*` skill (one per Ask: profile, mappings, setup-agents, map-agents, prompt, sequence, preset, setup, family, about, improve, recommendations) |
| to add a new kind of controller | `vibeconsole-add-controller` (reserved, not written yet) |

## Reference (shared by every skill)

- [File formats](reference/formats.md)
- [Every action](reference/actions.md)
- [Controls per device](reference/controls.md)
- [Safety Rules](reference/safety-rules.md)
- Example profiles: [browser tabs](examples/browser-tabs.profile), [editor](examples/editor.profile), [Orca IDE](examples/orca.profile)

## Where things live

- Config: one JSON file per topic in `~/.config/vibeconsole/`: `mappings.json`, `devices.json`, `lights.json`, `agents.json`, `settings.json` (each with its own `version`, saved atomically; defaults ship in the program). `mappings.tsv` is the old file, kept as the backup it was moved from and no longer read.
- Profiles, prompts and presets: folders beside it, `profiles/`, `prompts/`, `presets/`.
- Login service: `vibeconsole startup-status`, `startup-start`, `startup-stop`. Only one VibeConsole runs at a time.
- Agent terminals: `tmux attach -t vibe`.
