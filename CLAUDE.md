# VibeConsole agent instructions

Ponytail full, loaded by the session-start hook. If the session did not start with "PONYTAIL MODE ACTIVE", load the `ponytail` skill at level **full** before any work and keep it for the whole session. "stop ponytail" or "normal mode" turns it off.

## Research first

When an answer depends on how a library, framework, SDK, API or CLI behaves: read its docs in the `context7` MCP, matched to `Cargo.lock`. Use `firecrawl` only if context7 is missing or stale, or for recent changes (down → say so, use WebFetch). Reply only from what you verified, and name the URLs.

## Reply format

Sections in this order, skipping empty ones:

- **The idea**: ≤5 bullets.
- **What we agreed on**: ≤5 bullets.
- **How we will do it**: ≤3 bullets.
- **Next ticket**: the box `tickets next` prints. When I ask what's left, show **Still open** instead: the boxes `tickets left` prints. Show their output as is.
- **What needs to be done**: ≤3 bullets.
- **What we can do to make this better**: ≤3 numbered items.

Rules:

- Short sentences, one idea per bullet, no preamble or conclusion.
- "How does X work?" → answer with an ASCII/UML diagram.
- Start "What needs to be done" only after I say `approved`, `implement` or `go ahead`, or ask directly. One go-ahead covers one ticket.
- `x` files every "make this better" item as a ticket, `x 2` only item 2; build nothing. How: "Filing with `x`" in `docs/agents/issue-tracker.md`.
- Before the closing lines: what you skipped or didn't check, and any risk.
- Closing lines, nothing after them: `Reply x to file all as tickets, or x 2 for one.` when there's a "make this better", then `Say approved to start.` when there's a "What needs to be done".

## Start here

- Before implementing, debugging, or changing scope on a ticket, run `tickets begin NN` and read its output.
- For work without a ticket, use `tickets spec <heading>` to read the working version's **Safety Rules** and the sections the task touches.
- Don't read the other tickets. Use `tickets next`, `agent`, `blocked` and `triage` to inspect the queue. Set the working version with `tickets use <version>`. [docs/README.md](docs/README.md) lists the versions.
- Each spec restates the Project rules it relies on, under **Safety Rules** (`/to-spec` does it). A spec may change a rule only with the user's agreement, and says so.
- Recorded hardware limitations are under "Known limitations" in [docs/README.md](docs/README.md). Don't reopen them unless a ticket asks.
- V1–V6 tickets and tests are in `archive/tickets/` and `archive/tests/`. Read them for history; don't add to them.
- Trace affected callers before choosing a change. For Rust symbols use the LSP tool first (definition, references, incoming calls), then `grep`/`sed`; it may need a minute to index after a session starts. Treat spec status statements as historical; verify against the working tree and runnable checks.

## Project rules

- Rust terminal app. Never simplify away input validation, config integrity or held-key cleanup.
- Recheck the connected device and desktop session before hardware or keyboard-output experiments; ports and permissions change.
- Follow the spec's controller-write and restoration gates before sending device-specific messages.
- Keep automated checks free of real desktop shortcut injection.
- Report hardware and desktop effects only when seen; a build or simulated test is not a pass, except an emulator run that shows every box of a developer test (agreed 25 Nov 2026). Record unsupported or inconclusive results honestly.
- Held keys are released before every transition (start, pause, resume, reconnect, program change, quit, error).
- A pad fires on the first note-on after a transition; queued MIDI is drained during the transition, and a release whose press wasn't seen is ignored. No replay.
- Risky controls fire only after a one-second hold, and cancel on release, pause or clear.
- Hardware feedback (pad lights, octave, tempo) is sent only on a verified program, and restored on exit.
- Config saves are atomic: a failed save keeps the last valid file. A format change bumps the config version, and old configs load with safe defaults.
- Every error shown names its stage. A failure saves nothing partial, releases held keys and restores owned feedback.
- Desktop IDs are identifiers, never shell commands. Names (prompts, variables) are validated and can't leave their directory.
- A desktop behavior that can't be proven (e.g. minimizing another app's window on GNOME Wayland) is reported as unavailable. Never guess.

## Testing seams

- A good test drives VibeConsole the way a user does and checks what the user would see or what was saved. It never asserts private helper structure.
- Menu flows: the **terminal** module's scripted menu tests (scripted keys, fake controller, temporary config).
- Pad events: the **keyboard** state machine with simulated events.
- External processes (gio, tmux, Flow, systemctl, terminal): an injected runner. Automated tests never start or stop the real ones.
- Config migration and atomic save: **mappings** round-trip tests. `scripts/check-terminal.py` is the PTY smoke check.

## Agent skills

Specs, tickets and tests: read `docs/agents/issue-tracker.md` (folders, templates, what each test holds) before writing one. New work goes in the working version (`tickets use`); start a new version only when I say so. Domain docs: `docs/agents/domain.md`. Triage labels: `docs/agents/triage-labels.md`.

- **Developer tests:** write them when a ticket is code-complete. Then run every box you can yourself (the emulators in `scripts/emulate/` for every device that has one, the PTY checks, the installed build's output, files on disk) and close each test your run fully shows with `tickets done NN --run "…"` (boxes ticked, `done <date>`, its Result naming the run). Hand me only what strictly needs me: real hardware effects (lights, rumble, sound heard, Bluetooth, battery, the real device's own bytes or timing), keys reaching a real app, logins, reboots, and judgement calls. When I ask which test to do, offer only those; never ask me to run them otherwise. Template and run order: `docs/agents/live-tests.md`.

### Order of work

1. After a go-ahead, implement the next `ready-for-agent` ticket `tickets next` gives (number order, blockers first), close it, then stop and show the next one. A ticket's own acceptance tests (TDD, `cargo test`) belong to implementing it. Still stop before any controller write or real keyboard output the spec gates.
2. Only when all are implemented, test: run the agent tests, write the developer tests and their run order, run and close every developer test you can (above), record results. Then `mise run install`: it builds, stops VibeConsole (the login service and any terminal session), installs, and starts the service again. A running VibeConsole is no reason to skip it.

### Closing a ticket (before the next ticket and before committing)

Before starting the next ticket, follow the closing steps printed by `tickets begin NN`. Use `tickets close` for the status, acceptance checks, and Builder sign-off; a pass I report goes in with `--user-said "<my words>"`.

At a version's end, `tickets check` prints nothing (no implemented ticket still says `ready-for-agent`, no box ticked only in your head).

## Front end

Before changing any page or window UI (today there is none), load the `front-end` skill.
