# Safety Rules

These hold for every change and every suggestion.

- **Held keys are released** before every transition: start, pause, resume, reconnect, program change, profile change, layer change, quit, error.
- **A pad fires on the first press after a transition.** Queued MIDI is drained, a release whose press was not seen is ignored, nothing replays.
- **Risky controls** (quit an app, run a terminal command, write another app's key map) fire only after a one-second hold and cancel on release, pause or clear.
- **Hardware feedback** (pad lights, octave, tempo, rumble, keyboard lights) is sent only on a verified program or an owned controller, and restored on exit.
- **Config saves are atomic.** A failed save keeps the last valid file. A format change bumps the config version and old configs load with safe defaults.
- **Errors name their stage.** A failure saves nothing partial, releases held keys and restores owned feedback.
- **Identifiers, never commands:** desktop IDs and names are validated and are never run through a shell.
- **Unprovable desktop behaviour is reported as unavailable,** never guessed.
- **Automated checks never inject real desktop shortcuts** or start the real apps.
- **Hardware and desktop effects are reported only when seen.** A build or a simulated test is not a pass.
