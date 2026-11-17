---
name: vibeconsole-hardware-test
description: Use when a VibeConsole change is code-complete and needs a test only the user can see (controller, desktop effects, logins, reboots): writes the developer test and keeps it apart from what the builder checks.
---

# Hardware tests

Two kinds of test, never mixed:

- **Agent test:** what the builder checks alone: `cargo test`, the installed build's output, files on disk.
- **Developer test:** what only the user can see. The builder writes it and never asks the user to run it.

## A developer test has

- a title with where it is run ("at the Xbox controller"), the ticket it belongs to, a `Status:` line and a `Blocked by:` line;
- **How:** the build to install, the devices to connect, the app to focus;
- **Checks:** the one sentence that must be true;
- **How to do:** Prepare (back up the config folder's `*.json` files), numbered steps each with a `- [ ]` box that says what to see, then Restore;
- **Result:** date and notes, filled by the user.

## Rules

- Recheck the connected devices and the desktop session first; ports and permissions change.
- Follow the controller-write and restoration gates before any device-specific message.
- Report an effect only when it was seen. A build or a simulated test is not a pass; record unsupported or inconclusive results as such.
- Never delete a test file, even when its ticket is test-only.
