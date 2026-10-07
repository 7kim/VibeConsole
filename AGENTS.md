# KeyAI agent instructions

## Start here

- Before implementation, debugging, or changing scope, read [SPEC-UI.md](SPEC-UI.md) and [SPEC-BEHAVIOR.md](SPEC-BEHAVIOR.md) for agreed V3 behavior and acceptance checks. V2 behavior, hardware limitations and evidence are in `archive/V2.0/SPEC.md` (local only; `archive/` is git-ignored).
- Inspect the current code and trace affected callers before choosing a change. Treat the specification's implementation-status statements as historical; verify progress against the working tree and runnable checks.
- Work on the requested ticket from `tickets/README.md` only after its blockers are complete. Report its verified outcome before advancing; a request for one ticket is not a request to build the entire application. Consult [archive/SPEC.md](archive/SPEC.md) when checking original MVP behavior or acceptance evidence.

## Implementation style

- Use Rust with a terminal interface. Apply Ponytail: reuse existing code, then the standard library or native platform capabilities, then existing dependencies before adding code or dependencies.
- Keep the smallest working implementation. Add modules and abstractions when current behavior needs them, rather than scaffolding future features.
- Preserve input validation, configuration integrity, and held-key cleanup when simplifying.
- Mark deliberate compromises with a `ponytail:` comment naming the actual limitation and when to replace it.

## Hardware and desktop work

- Recheck the connected device and desktop session before hardware or keyboard-output experiments; port numbers and permissions can change.
- Follow the specification's controller-write and restoration gates before sending device-specific messages. Distinguish device identification, successful message transmission, and physically observed behavior.
- Keep automated checks free of real desktop shortcut injection. Perform live checks deliberately and report which hardware/application effects were actually observed.
- Record unsupported or inconclusive capabilities honestly; consult the specification's fallback instead of inventing support.

## Verification and handoff

- Leave a small runnable behavior check for nontrivial logic, using Rust's built-in test support where practical. Follow the testing decisions in the relevant V3 spec for the behavior being implemented.
- Once a Cargo project exists, run `cargo fmt --check`, `cargo check`, and relevant `cargo test` checks after code changes. Report unavailable prerequisites or failed checks explicitly.
- A build or simulated test does not establish LED control or working Fedora Wayland shortcuts. Report hardware verification separately.
- Keep completion reports concise: what changed, commands/checks and results, and remaining limitations. Update milestone status only when supported by evidence.
