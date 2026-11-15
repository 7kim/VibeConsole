---
name: vibeconsole-safety-review
description: Use when reviewing or writing a VibeConsole code change, a diff or a pull request: checks it against the Safety Rules and names each rule that is at risk.
---

# Safety review

Read the diff, then ask each question. A "no" is a finding; name the file and line.

1. Does every new transition (start, pause, resume, reconnect, program, profile, layer, quit, error) release held keys first?
2. After a transition, does the first press fire, queued input drain, and nothing replay?
3. Is anything risky (quit an app, run a command, write another app's key map) behind the one-second hold?
4. Is hardware feedback sent only on a verified program or an owned controller, and restored on exit and on every error path?
5. Are config writes atomic, and does a format change bump the version and keep old files loading with safe defaults?
6. Does every error name its stage, save nothing partial, release keys and restore feedback?
7. Are desktop IDs and names validated and never passed through a shell or out of their folder?
8. Does any automated test inject real desktop shortcuts or start a real app? Tests use an injected runner.
9. Is a hardware or desktop effect claimed that nobody saw? Reports say "built" or "tested in simulation" until seen.
10. Was input validation, config integrity or held-key cleanup simplified away?

The rules in full: [Safety Rules](../vibeconsole/reference/safety-rules.md). Run `cargo fmt --check`, `cargo check --locked` and `cargo test --locked` before calling it reviewed.
