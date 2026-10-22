# Live tests: Local Markdown (`tests/`)

Checks only the user can perform (hardware, desktop effects, installs, reboots, real Flow/agents) live as markdown files in `tests/` at the repo root. `tests/` is git-ignored, so tests stay local. Automated `cargo test` checks stay in the code; a build or simulated test never counts as a live pass.

## Conventions

- One file per ticket under its version: `tests/v<N>/<NN>-<slug>.md`, numbered after the ticket it verifies. Never a single combined tests file.
- `tests/README.md` is the index: run order in phases (setup, no controller writes, controller writes, reboot last), common setup, shared pad tables, and **Known limitations** (observed unsupported behavior, recorded instead of tested). Keep it in sync whenever a test is added or its blockers change.
- Files that a test needs (probe scripts, original payloads) sit beside it in `tests/v<N>/`, never in `archive/`.
- Triage state is the `**Status:**` line: `ready-for-human` while open, `done <date>` once every box is ticked from observed effects.

## Making a test

When a ticket is code-complete and its cargo checks pass, write its live test using this template, add it to the run order in `tests/README.md`, and point the ticket's live-check row at it. Don't ask the user to run it.

```markdown
# V<N>-<NN>: <Title>

**Version:** V<N> · **Source:** <ticket it verifies and its state>.

**Blocked by:** <other tests that set up what this one needs>, or "None (can start immediately)".

**Status:** ready-for-human

## Tests

All of these must pass. Tick them in **How to do** below as you go.

- <one observable pass criterion per line>

## How to do

### 0. Prepare

Common setup in [../README.md](../README.md#common-setup), then <exact commands, program, pads>.

### 1. <Step>

<exact commands and menu paths, and what the user should see>

- [ ] <what was observed>

## Result

- Date:
- <what to record: versions, effects seen, errors, restore result>
- Failures / notes:
```

Rules for the content:

- Exact commands to copy, exact menu paths, and the expected effect for each step. One `- [ ]` per observation.
- Any step that writes the controller or changes user settings records the original first and ends with a verified restore.
- Separate what was sent from what was physically seen. Unsupported or inconclusive is a valid recorded result.

## Doing a test (the user)

Follow `tests/README.md` top to bottom: common setup once per sitting, then each test in order. Tick boxes only for effects you saw yourself, fill in **Result**, set **Status:** to `done <date>`, and tick the matching live row in the ticket. A failed test keeps its notes and stays open; continue with the next test unless it is blocked by the failed one.

## When a skill or the user says "read the test results"

Read the filled `tests/v<N>/` files. Update only the ticket rows supported by ticked observations. Partial or failed results stay open, and bugs get fixed before the test is rerun.
