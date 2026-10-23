# VibeConsole agent instructions

Ponytail full, loaded by the session-start hook. If the session did not start with "PONYTAIL MODE ACTIVE", load the `ponytail` skill at level **full** before any work and keep it for the whole session. "stop ponytail" or "normal mode" turns it off.

## Research first

Before any stack, library, framework, SDK, API or CLI answer: fetch current docs with the `context7` MCP, then check news for the current month with the `firecrawl` MCP. Reply only from what you verified.

## Reply format

Structure every reply in these sections, skipping empty ones:

- **The idea**: 5 bullets max.
- **What we agreed on**: 5 bullets max.
- **What we can do to make this better**: 3 bullets max.
- **How we will do it**: 3 bullets max.
- **What needs to be done**: 3 bullets max.

Rules:

- Short sentences. One idea per bullet. No preamble, no conclusion.
- When asked "how does X work" (or similar), show it with an ASCII/UML diagram instead of prose.
- End with one line: what you skipped or did not check, plus any risk.

## Start here

- Before implementation, debugging, or changing scope, read [tickets/SPEC.md](tickets/SPEC.md) for behavior, safety rules and testing decisions, and [tickets/README.md](tickets/README.md) for open tickets.
- Recorded hardware limitations are under "Known limitations" in [tests/README.md](tests/README.md). Don't reopen them unless a ticket asks.
- Trace affected callers before choosing a change. Treat spec status statements as historical; verify against the working tree and runnable checks.
- Work a ticket only after its blockers are complete, and report its verified outcome before advancing. One ticket is not a request to build everything.

## Project rules

- Rust terminal app. Never simplify away input validation, config integrity or held-key cleanup.
- Recheck the connected device and desktop session before hardware or keyboard-output experiments; ports and permissions change.
- Follow the spec's controller-write and restoration gates before sending device-specific messages.
- Keep automated checks free of real desktop shortcut injection.
- Report hardware and desktop effects only when seen; a build or simulated test is not a pass. Record unsupported or inconclusive results honestly.

## Verification and handoff

- After code changes: `cargo fmt --check`, `cargo check`, `cargo test`; report failures or missing prerequisites.
- Follow the testing decisions in `tickets/SPEC.md`.
- Report in the reply format: what changed, checks and results, remaining limitations. Change a ticket's status only with evidence.

## Agent skills

- **Issue tracker:** one `NN-slug.md` per ticket in git-ignored `tickets/`, indexed by `tickets/README.md`. See `docs/agents/issue-tracker.md`.
- **Triage labels:** `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix` as each ticket's `Status:` line. See `docs/agents/triage-labels.md`.
- **Domain docs:** single context, root `CONTEXT.md` plus `docs/adr/`, created lazily. See `docs/agents/domain.md`.
- **Live tests:** checks only the user can perform, one `v<N>/NN-slug.md` per ticket in git-ignored `tests/`, ordered by `tests/README.md`. Agents write them when a ticket is code-complete; never ask the user to run them. See `docs/agents/live-tests.md`.
