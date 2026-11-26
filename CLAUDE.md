# VibeConsole agent instructions

Ponytail full (session-start hook). No "PONYTAIL MODE ACTIVE" at start → load the `ponytail` skill at **full** before any work, for the whole session. "stop ponytail" or "normal mode" ends it.

## Research first

Library, framework, SDK, API or CLI behaviour → its docs in the `context7` MCP, matched to `Cargo.lock`. `firecrawl` only if context7 is missing or stale, or for recent changes (down → say so, use WebFetch). Answer only what you verified; name the URLs.

## Reply format

Sections in this order, empty ones skipped:

- **The idea**: ≤5 bullets.
- **What we agreed on**: ≤5 bullets.
- **How we will do it**: ≤3 bullets.
- **Next ticket**: the `tickets next` box. When I ask what's left: **Still open**, the `tickets left` boxes. Output as is.
- **What needs to be done**: ≤3 bullets.
- **What we can do to make this better**: ≤3 numbered items.

Rules:

- Short sentences, one idea per bullet, no preamble or conclusion.
- "How does X work?" → ASCII/UML diagram.
- "What needs to be done" only after `approved`, `implement`, `go ahead` or a direct ask. One go-ahead, one ticket.
- `x` files every "make this better" item as a ticket, `x 2` only item 2; build nothing. How: "Filing with `x`" in `docs/agents/issue-tracker.md`.
- Before the closing lines: what you skipped or didn't check, and any risk.
- Closing lines, nothing after: `Reply x to file all as tickets, or x 2 for one.` with a "make this better", then `Say approved to start.` with a "What needs to be done".

## Start here

- Ticket work (implement, debug, change scope): `tickets begin NN` first; read its output.
- No ticket: `tickets spec <heading>` for **Safety Rules** and the sections the task touches.
- Don't read other tickets; use `tickets next`, `agent`, `blocked`, `triage`. `tickets use <version>` sets the working version; [docs/README.md](docs/README.md) lists versions.
- Each spec restates the Project rules it uses under **Safety Rules** (`/to-spec` does it). It changes a rule only with my agreement, and says so.
- Hardware limits: "Known limitations" in [docs/README.md](docs/README.md). Don't reopen them unless a ticket asks.
- V1–V6: `archive/tickets/`, `archive/tests/`. History only; don't add to them.
- Trace callers before choosing a change: LSP first for Rust (definition, references, incoming calls; may take a minute to index), then `grep`/`sed`. Spec status lines are history; verify against the tree and runnable checks.

## Project rules

- Rust terminal app. Never simplify away input validation, config integrity or held-key cleanup.
- Recheck the connected device and desktop session before hardware or keyboard-output experiments; ports and permissions change.
- Device-specific messages only through the spec's controller-write and restoration gates.
- Automated checks inject no real desktop shortcuts.
- Report hardware and desktop effects only when seen. A build or simulated test isn't a pass; an emulator run showing every box of a developer test is (agreed 25 Nov 2026). Record unsupported or inconclusive results honestly.
- Release held keys before every transition (start, pause, resume, reconnect, program change, quit, error).
- A pad fires on the first note-on after a transition. Queued MIDI is drained during it; a release without a seen press is ignored. No replay.
- Risky controls fire after a one-second hold; release, pause or clear cancels.
- Hardware feedback (pad lights, octave, tempo): only on a verified program, restored on exit.
- Atomic config saves: a failed save keeps the last valid file. A format change bumps the config version; old configs load with safe defaults.
- Every shown error names its stage. A failure saves nothing partial, releases held keys, restores owned feedback.
- Desktop IDs are identifiers, never shell commands. Names (prompts, variables) are validated and stay in their directory.
- Unprovable desktop behaviour (e.g. minimizing another app's window on GNOME Wayland) is reported unavailable. Never guess.

## Testing seams

- A good test drives VibeConsole as a user does and checks what they'd see or what was saved, never private helper structure.
- Menu flows: **terminal** scripted menu tests (scripted keys, fake controller, temp config).
- Pad events: the **keyboard** state machine, simulated events.
- External processes (gio, tmux, Flow, systemctl, terminal): an injected runner; tests never start or stop the real ones.
- Config migration and atomic save: **mappings** round-trip tests. PTY smoke check: `scripts/check-terminal.py`.

## Agent skills

Read `docs/agents/issue-tracker.md` before writing a spec, ticket or test. New work goes in the working version (`tickets use`); a new version only when I say so. Domain: `docs/agents/domain.md`. Triage labels: `docs/agents/triage-labels.md`.

- **Developer tests:** write them when a ticket is code-complete. Run every box you can (emulators in `scripts/emulate/`, PTY checks, installed build's output, files on disk); close each test a run fully shows with `tickets done NN --run "…"`. Hand me only real hardware effects (lights, rumble, sound heard, Bluetooth, battery, the real device's own bytes or timing), keys reaching a real app, logins, reboots and judgement calls. Offer only those when I ask which test to do; never ask otherwise. Template and run order: `docs/agents/live-tests.md`.

### Order of work

1. After a go-ahead: implement the next `ready-for-agent` ticket from `tickets next`, close it, stop, show the next. Its own acceptance tests (TDD, `cargo test`) are part of it. Stop before any controller write or real keyboard output the spec gates.
2. When all are implemented: run the agent tests, write the developer tests and run order, run and close all you can, record results. Then `mise run install` (builds, stops the service and any terminal session, installs, restarts). Run it even if VibeConsole is running.

### Closing a ticket (before the next one and before committing)

Follow the closing steps `tickets begin NN` prints. `tickets close` sets status, checks and Builder sign-off; a pass I report goes in with `--user-said "<my words>"`.

Version end: `tickets check` prints nothing.

## Front end

Before changing any page or window UI (none yet), load the `front-end` skill.
