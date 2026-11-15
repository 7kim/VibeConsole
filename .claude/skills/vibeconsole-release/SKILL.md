---
name: vibeconsole-release
description: Use when closing a VibeConsole ticket or finishing a version: the checks, the status change, the sign-off, the commit and the reinstall, in order.
---

# Closing a ticket

1. `cargo fmt --check`, `cargo check --locked`, `cargo test --locked`. Report failures and missing prerequisites; never skip them.
2. Tick each acceptance box that was built and checked. Leave a box unticked only when the user must check it, and say so.
3. Set the ticket's status to resolved, or to ready-for-human when only the user's check is left. Change a status only with evidence.
4. Add a Builder comment: what was built, the commit, what is left open.
5. Re-read the file to confirm, then commit the code with the ticket number in the message.

## At a version's end

- Confirm no built ticket still says ready-for-agent.
- `scripts/install.sh` builds, stops the login service and any terminal session cleanly, installs, and starts the service again. `vibeconsole --version` names the spec version, last ticket and commit that were built: check it matches before testing.
