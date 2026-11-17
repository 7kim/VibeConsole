---
name: vibeconsole-ask-improve
description: Use when VibeConsole's 0 AI opened you on VibeConsole's source ("Ask … how to improve VibeConsole"): turn the user's wish into a change to the Rust code, its tests and its docs.
---

# Improve VibeConsole

You were opened in a clone of https://github.com/7kim/vibeconsole (in `~/.local/share/vibeconsole/source`) with the user's wish as your first message. VibeConsole itself saved nothing.

## The code

- `src/` is the terminal app: `terminal.rs` (the ten tabs, menus, every Ask), `runtime.rs` (doctor, login service, skills install), `main.rs` (commands).
- `core/src/` is the library: `mappings.rs` (config format, validation, AI reply checks), `actions.rs` (every action and its dispatcher), `keyboard.rs` (pads and key output), `status.rs` (agent states and lights), `gamepad.rs` (Xbox), `feedback.rs` (Akai lights).
- `.claude/skills/` holds the VibeConsole skills; `CLAUDE.md` holds the project's rules.

## Steps

1. Read `CLAUDE.md`, then find the code the wish touches. Say in two or three sentences what you will change before changing it.
2. Keep every safety rule: input validation, config integrity (atomic saves; a format change bumps the config version), held keys released before every transition, risky actions only after a one-second hold, hardware feedback only on a verified program and restored on exit, identifiers never shell commands.
3. Write a test first where behaviour changes: the terminal module's scripted menu tests, the keyboard state machine with simulated events, or the injected runner for outside programs. Tests never start real programs or send real keys.
4. Run `cargo fmt --check`, `cargo check --locked` and `cargo test --locked`; all pass.
5. A new action family or form also needs `ACTION_FORMS` and `UPDATE_SKILLS=1 cargo test vibeconsole_skills`.
6. Show the user the diff and what they can try. New action kinds are v7.9 plugins: write those up as an idea instead of building them.
