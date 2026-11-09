# VibeConsole agent instructions

Ponytail full, loaded by the session-start hook. If the session did not start with "PONYTAIL MODE ACTIVE", load the `ponytail` skill at level **full** before any work and keep it for the whole session. "stop ponytail" or "normal mode" turns it off.

## Research first

Before any stack, library, framework, SDK, API or CLI answer: fetch current docs with the `context7` MCP, matched to the version in `Cargo.lock`, then check news for the current month with the `firecrawl` MCP. Reply only from what you verified, and name the URLs. Firecrawl down → say so, use WebFetch.

## Reply format

Sections in this order, skipping empty ones:

- **The idea**: ≤5 bullets.
- **What we agreed on**: ≤5 bullets.
- **How we will do it**: ≤3 bullets.
- **Next ticket**: one box with the ticket Order of work picks next. When I ask what's left, show **Still open** instead: the highest version's `ready-for-agent` and `ready-for-human` files as three box arrays titled with a count: Tickets left, Agent tests left, Developer tests left. Each box: number and file-name title, three per row, wrapped. Skip empty arrays.

  ```
  Tickets left (2)
  ┌────────────────────────┬────────────────────────┐
  │ 03 first press fires   │ 04 flow start or       │
  │                        │    repair              │
  └────────────────────────┴────────────────────────┘
  ```
- **What needs to be done**: ≤3 bullets.
- **What we can do to make this better**: ≤3 numbered items.

Rules:

- Short sentences, one idea per bullet, no preamble or conclusion.
- "How does X work?" → answer with an ASCII/UML diagram.
- Start "What needs to be done" only after I say `approved` or ask directly. One `approved` covers the whole list.
- `x` files every "make this better" item as a ticket, `x 2` only item 2; build nothing. Each gets the next free number in `docs/<highest>/tickets/<highest>-ideas/issues/` plus its agent-test and developer-test files, all `Status: needs-triage`.
- Second-to-last line: what you skipped or didn't check, and any risk.
- Last line, nothing after it: `Say approved to start.` when there's a "What needs to be done", `Reply x to file all as tickets, or x 2 for one.` when there's a "make this better".

## Start here

- Before implementation, debugging or changing scope, read the highest version's spec (`docs/<version>/spec/`) and its tickets. [docs/README.md](docs/README.md) lists the versions.
- Each spec restates the Project rules it relies on, under **Safety Rules** (`/to-spec` does it). A spec may change a rule only with the user's agreement, and says so.
- Recorded hardware limitations are under "Known limitations" in [docs/README.md](docs/README.md). Don't reopen them unless a ticket asks.
- V1–V6 tickets and tests are in `archive/tickets/` and `archive/tests/`. Read them for history; don't add to them.
- Trace affected callers before choosing a change. Treat spec status statements as historical; verify against the working tree and runnable checks.

## Project rules

- Rust terminal app. Never simplify away input validation, config integrity or held-key cleanup.
- Recheck the connected device and desktop session before hardware or keyboard-output experiments; ports and permissions change.
- Follow the spec's controller-write and restoration gates before sending device-specific messages.
- Keep automated checks free of real desktop shortcut injection.
- Report hardware and desktop effects only when seen; a build or simulated test is not a pass. Record unsupported or inconclusive results honestly.
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

Specs, tickets and tests are markdown under `docs/<version>/` (git-ignored: the repo is public; `docs/README.md` lists versions). Read `docs/agents/issue-tracker.md` before writing one. New work goes in the highest version; start the next only when I say so. Domain docs: `docs/agents/domain.md`. Triage labels: `docs/agents/triage-labels.md`.

- **Agent tests** (`docs/<version>/agent-tests/`): what you check yourself: `cargo test`, the installed build's output, files on disk.
- **Developer tests** (`docs/<version>/developer-tests/`): what only I can see: the controller, desktop effects, logins, reboots. Write them when a ticket is code-complete; never ask me to run them. Template and run order: `docs/agents/live-tests.md`.

### Order of work

1. After `approved`, implement the version's `ready-for-agent` tickets in number order (blockers first), without stopping between them. A ticket's own acceptance tests (TDD, `cargo test`) belong to implementing it. Still stop before any controller write or real keyboard output the spec gates.
2. Only when all are implemented, test: run the agent tests, write the developer tests and their run order, record results. Then `cargo build --release --locked && ./target/release/vibeconsole install`, unless VibeConsole is running.

### Closing a ticket (before the next ticket and before committing)

1. Run `cargo fmt --check`, `cargo check --locked`, `cargo test --locked`; report failures or missing prerequisites.
2. Tick each acceptance box you built and checked. Leave a box unticked only when I must check it, and say so in the comment.
3. Set `Status: resolved`, or `ready-for-human` when only my check is left. Change a status only with evidence.
4. Add `### Builder · <date>` under `## Comments`: what was built, the commit, what's left open.
5. Re-read the file to confirm, then commit the code with the ticket number in the message.

At a version's end, confirm no implemented ticket still says `ready-for-agent` or has a box ticked in your head but not in the file.

## Front end

Before changing any page or window UI (today there is none; the Tauri desktop app idea would be the first), design it as a Claude design artifact (claude-artifacts `list_types`, a Design type) with the `ui-ux-pro-max` skill (scripts: `python3`).

Components: add them with `bunx --bun shadcn@latest add @<registry>/<name>` in the front end's folder. Its registries are in that folder's `components.json`: `@shadcn` for dashboards, `@aceternity` for landing pages. Not there → 21st, through the `21st` plugin (`mcp__21st__*`, `21st:` skills). 21st free plan: search plus 2 code fetches a day, no AI generation.

- "Change X in the artifact" → publish a new artifact version as the draft; leave the page's code alone.
- `approved` → read the artifact's full code, `/to-spec` it with the artifact's slug and approved version, then build the page to match it exactly.
