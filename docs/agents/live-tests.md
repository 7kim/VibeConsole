# Developer tests: Local Markdown (`docs/<version>/developer-tests/`)

Checks only the user can perform (hardware, desktop effects, installs, logins, reboots, real Flow/agents) live in `docs/<version>/developer-tests/`, git-ignored. What the agent can check itself goes in `agent-tests/` instead (see `issue-tracker.md`). A build or simulated test never counts as a developer-test pass. V1–V6 tests are in `archive/tests/`.

## Conventions

- One file per ticket: `docs/<version>/developer-tests/<NN>-<slug>.md`, numbered like the ticket. Never a single combined tests file.
- `docs/<version>/developer-tests/README.md` is the run order in phases: setup, no controller writes, controller writes, reboot last. Keep it in sync whenever a test is added or its blockers change.
- Common setup and **Known limitations** (observed unsupported behavior, recorded instead of tested) are in `docs/README.md`.
- Files a test needs (probe scripts, original payloads) sit beside it.
- `Status:` is `ready-for-human` while open, `done <date>` once every box is ticked from observed effects.

## Making a test

When a ticket is code-complete and its cargo checks pass, write its developer test with this template, add it to the run order, and name it in the ticket's `Tests:` line. Don't ask the user to run it.

```markdown
# Test NN: <Title> (<where: at the controller, on the desktop, ...>)

Ticket: `docs/<version>/tickets/<version>-<feature>/issues/NN-<slug>.md`
Status: ready-for-human
Blocked by: <other tests that set up what this one needs>, or none
How: by the user, <one line: program, pads, service state>.

## Checks

All of these must pass. Tick them in **How to do** as you go.

- <one observable pass criterion per line>

## How to do

### 0. Prepare

Common setup in `docs/README.md`, then <exact commands, program, pads>.

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

Follow the run order top to bottom: common setup once per sitting, then each test in order. Tick boxes only for effects you saw yourself, fill in **Result**, set **Status:** to `done <date>`, and tick the matching `(user)` box in the ticket. A failed test keeps its notes and stays open; continue with the next test unless it is blocked by the failed one.

## When a skill or the user says "read the test results"

Read the filled developer tests. Update only the ticket boxes supported by ticked observations, then close the ticket (CLAUDE.md, Closing a ticket). Partial or failed results stay open, and bugs get fixed before the test is rerun.
