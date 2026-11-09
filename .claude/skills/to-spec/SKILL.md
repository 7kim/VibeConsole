---
name: to-spec
description: "Turn the current conversation into a spec for a VibeConsole version and publish it to docs/<version>/spec/: no interview, just synthesis of what you've already discussed. Use when the user says /to-spec, /to-spec v6.1, or asks to turn a discussion into a spec."
---

This skill takes the current conversation context and codebase understanding and produces a spec. Do NOT interview the user; just synthesize what you already know.

The tracker is described in `docs/agents/issue-tracker.md`; read it first.

## Process

1. Explore the repo to understand the current state of the codebase, if you haven't already. Use the domain vocabulary in `CONTEXT.md` (if it exists) and respect any ADRs in `docs/adr/` in the area you're touching.

2. Sketch the seams at which you're going to test the feature. Prefer the existing seams in CLAUDE.md's Testing seams; use the highest seam possible. If new seams are needed, propose them at the highest point you can. The fewer seams, the better; the ideal number is one.

Check with the user that these seams match their expectations.

3. Write the spec with the template below to `docs/<version>/spec/SPEC-<version>-<name>.md` (the version the user named, else the highest in `docs/README.md`). Set `Status: ready-for-agent`. Update that version's row in `docs/README.md`.

<spec-template>

# SPEC · VibeConsole <version> · <name>

*<weekday> <date>.*

Status: ready-for-agent

- **Made with:** the to-spec skill (`/to-spec <version>`), from the user's messages of <dates>:
  - "<the user's words, quoted exactly>";
- **The user's decisions (<date>):**
  - <each decision, one bullet>
- **Facts checked (<date>):**
  - <what you verified in the code, the installed build or the device, and how>

## Safety Rules

The rules from CLAUDE.md's Project rules that this spec relies on, restated. Name any rule this spec changes, and why the user agreed.

## Problem Statement

The problem that the user is facing, from the user's perspective.

## Solution

The solution to the problem, from the user's perspective.

## User Stories

A LONG, numbered list of user stories, each: 1. As an <actor>, I want a <feature>, so that <benefit>. Cover all aspects of the feature.

## Implementation Decisions

A list of implementation decisions: modules built or modified, their interfaces, technical clarifications from the user, architectural decisions, config format and migration (version bump, old configs' defaults), specific interactions.

Do NOT include specific file paths or code snippets; name crate modules instead. Exception: a prototype snippet that encodes a decision more precisely than prose (state machine, type shape), trimmed to the decision-rich parts.

## Testing Decisions

- What makes a good test (external behavior only, not implementation details).
- Which seams and modules will be tested, with prior-art test names.
- Which checks are developer tests (controller, desktop, login, reboot), reported separately from automated results.

## Out of Scope

What this spec doesn't do.

## Further Notes

Anything else.

</spec-template>
