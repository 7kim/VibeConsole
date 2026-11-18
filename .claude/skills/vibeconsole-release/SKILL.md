---
name: vibeconsole-release
description: Use when closing a VibeConsole ticket or finishing a version: the checks, the status change, the sign-off, the commit and the reinstall, in order.
---

# Closing a ticket

1. `cargo-gate` (fmt, check and test in one call; where it isn't installed, `cargo fmt --check`, `cargo check --locked`, `cargo test --locked`). Report failures and missing prerequisites; never skip them.
2. `tickets close NN --status resolved|ready-for-human --tick <the boxes built and checked> --comment "<what was built, the commit, what is left open>"`. In one call it ticks the boxes and sets the status, signs the Builder comment, closes the agent test once every agent box is ticked, and adds the developer test to the run order. It refuses a `(user)` box, `ready-for-human` with no `(user)` box left, and a second close the same day. Where `tickets` isn't installed, do the same by hand. Leave a box unticked only when the user must check it, and say so. Change a status only with evidence.
3. Re-read the file to confirm, then commit the code with the ticket number in the message. If the ticket changed `~/.local/share/claude-scripts`, commit and push that repo too, with the same ticket number.

## At a version's end

- `tickets check` prints nothing: no built ticket still says ready-for-agent, no box is ticked only in your head, and every test file exists. Where `tickets` isn't installed, confirm it by reading the tickets.
- `mise run install` (it runs `scripts/install.sh`) builds, stops the login service and any terminal session cleanly, installs, and starts the service again. A running VibeConsole is no reason to skip it. `vibeconsole --version` names the spec version, last ticket and commit that were built: check it matches before testing.
