mod runtime;
mod terminal;
use mappings::{config_path, load};
use vibeconsole_core::{
    Control, Detector, Event, Message, ReleaseType, actions, feedback, input_port, keyboard,
    mappings, midi, motion, recording, screen, status,
};

pub use vibeconsole_core::Result;
/// Commands that only print die quietly on a closed pipe (`vibeconsole doctor | head`)
/// instead of panicking. Not for the session or daemon: they must outlive a dead child's
/// pipe so held keys are always released.
fn plain_output() {
    // SAFETY: restores the default disposition before any output; no handler is installed.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// `vibeconsole <version>` from Cargo.toml (ticket 23).
fn version() -> String {
    format!("vibeconsole {}", env!("CARGO_PKG_VERSION"))
}

/// Commands that open the session or read the controller (no command = the session).
const SESSION: [&str; 7] = [
    "--paused",
    "configure",
    "list",
    "detect",
    "get",
    "run",
    "feedback-read",
];

/// Colors only on a terminal, and never with NO_COLOR set (https://no-color.org).
fn color() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// Grouped help in the style of gh/mise (ticket 22).
fn help(color: bool) -> String {
    let paint = |code: &str, text: &str| {
        if color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    };
    let states = status::STATES.join("|");
    let sections: [(&str, Vec<(&str, String)>); 5] = [
        (
            "SESSION",
            vec![
                (
                    "(none)",
                    "Open the session; it detects the controller program and runs it".into(),
                ),
                ("--paused", "Open the session paused".into()),
                (
                    "configure",
                    "Learn a pad, knob or joystick and assign it".into(),
                ),
                ("list", "Show saved assignments (no MIDI needed)".into()),
                ("detect", "Show live controls and MIDI events".into()),
                ("get", "Inspect one pad in its original program".into()),
                (
                    "run",
                    "Run mappings in this terminal; Ctrl+C releases keys and exits".into(),
                ),
                (
                    "feedback-read",
                    "Read the controller's octave, tempo and arpeggiator".into(),
                ),
            ],
        ),
        (
            "LOGIN SERVICE",
            vec![
                ("startup-enable", "Start VibeConsole at every login".into()),
                ("startup-start", "Start it now".into()),
                (
                    "startup-stop",
                    "Stop it now (keys released, lights restored)".into(),
                ),
                ("startup-status", "Show whether it runs".into()),
                ("startup-disable", "Stop starting at login".into()),
                ("daemon", "Run as the service (the unit calls this)".into()),
            ],
        ),
        (
            "SETUP",
            vec![
                (
                    "install",
                    "Copy to ~/.local/bin and install the window extension".into(),
                ),
                (
                    "uninstall",
                    "Remove the program, service and extension (mappings kept)".into(),
                ),
                (
                    "doctor",
                    "Check MIDI, uinput, tools, hooks and the window extension".into(),
                ),
            ],
        ),
        (
            "AGENT HOOKS",
            vec![
                (
                    "agent-status",
                    "<agent|-> <state>  Report an agent's state (for hooks)".into(),
                ),
                ("", format!("state: {states}")),
            ],
        ),
        (
            "FLAGS",
            vec![
                ("-h, --help", "Show this help".into()),
                ("-V, --version", "Show the version".into()),
            ],
        ),
    ];
    let width = sections
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|(name, _)| name.len()))
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = format!(
        "Shortcuts, prompts and agent controls for the Akai MPK Mini MK3.\n\n{}\n  vibeconsole [command] [flags]\n",
        paint("1", "USAGE")
    );
    for (title, rows) in &sections {
        out += &format!("\n{}\n", paint("1", title));
        for (name, text) in rows {
            let padded = format!("{name:width$}");
            out += &format!("  {}{text}\n", paint("36", &padded));
        }
    }
    out += &format!(
        "\n{}\n  $ vibeconsole                  # open the session\n  $ vibeconsole configure        # assign a pad\n  $ vibeconsole startup-enable   # run at every login\n\n{}\n  vibeconsole doctor checks your setup; README.md has the full guide.\n",
        paint("1", "EXAMPLES"),
        paint("1", "LEARN MORE")
    );
    out
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--version"] || args == ["-V"] || args == ["version"] {
        plain_output();
        println!("{}", version());
        return Ok(());
    }
    if args == ["--help"] || args == ["-h"] || args == ["help"] {
        plain_output();
        print!("{}", help(color()));
        return Ok(());
    }
    // Called from agent hooks while the session or service runs: no MIDI, keyboard, or owner lock.
    if args.first().is_some_and(|s| s == "agent-status") {
        return status::command(&args[1..]);
    }
    if let Some(command) = args
        .first()
        .filter(|s| runtime::COMMANDS.contains(&s.as_str()))
    {
        plain_output();
        return runtime::command(command, &args[1..]);
    }
    if args.first().is_some_and(|s| s == "daemon") {
        if args[1..]
            .iter()
            .any(|s| s != "--feedback" && s != "--start-flow")
        {
            return Err("daemon flags: --feedback, --start-flow".into());
        }
        if args.len() > 1 {
            eprintln!(
                "Legacy daemon flags ignored; saved Flow setting and verified program now control startup."
            );
        }
        let _owner = runtime::Owner::acquire()?;
        let path = config_path()?;
        return terminal::daemon(&path, load(&path)?);
    }
    if args.len() > 1
        || args
            .first()
            .is_some_and(|arg| !SESSION.contains(&arg.as_str()))
    {
        eprintln!(
            "unknown command '{}'\n\nUsage: vibeconsole [command] [flags]\nRun vibeconsole --help for the commands.",
            args.join(" ")
        );
        std::process::exit(2);
    }
    let path = config_path()?;
    let mappings = load(&path)?;
    // A running login service is paused for the session and started on quit (ticket 20).
    let _owner = if args == ["list"] {
        plain_output();
        None
    } else {
        Some(runtime::session()?)
    };
    if args.is_empty() || args == ["configure"] || args == ["--paused"] || args == ["detect"] {
        return terminal::start(
            &path,
            mappings,
            args.first().map(String::as_str).unwrap_or("session"),
        );
    }
    println!("Configuration: {}", path.display());
    if mappings.is_empty() {
        println!("No saved assignments.");
    } else {
        for mapping in &mappings {
            println!("{mapping}");
        }
    }
    match args.first().map(String::as_str).unwrap_or("detect") {
        "list" => Ok(()),
        "feedback-read" => feedback::read_only(),
        "detect" => unreachable!("handled by terminal session"),
        "run" => terminal::start(&path, mappings, "run"),
        "get" => {
            let pad = learn_pad()?;
            match mappings
                .iter()
                .find(|mapping| mapping.context == 0 && mapping.control == pad)
            {
                Some(mapping) => println!("Current assignment: {mapping}"),
                None => println!(
                    "{} has no assignment.",
                    mappings::control_name(pad, None, 0, None)
                ),
            }
            Ok(())
        }
        _ => unreachable!("validated command"),
    }
}

fn learn_pad() -> Result<Control> {
    let input = midi::Input::open()?;
    println!("Press a pad in either bank to learn it. No shortcuts are generated; Ctrl+C cancels.");
    // The input closes when `input` drops, before terminal editing, including on failures.
    let mut detector = Detector::default();
    loop {
        match input.next()? {
            Some(message) => {
                if let Some(Event::Press(pad)) = detector.observe(message) {
                    println!(
                        "Learned: {}",
                        describe_event(&Event::Press(pad), |c| mappings::control_name(
                            c, None, 0, None
                        ))
                    );
                    return Ok(pad);
                }
            }
            None => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
}

fn describe_event(event: &Event, name: impl Fn(Control) -> String) -> String {
    let (pad, action) = match event {
        Event::Press(pad) => (pad, "PRESS: Note On"),
        Event::Release(pad, ReleaseType::NoteOff) => (pad, "RELEASE: Note Off"),
        Event::Release(pad, ReleaseType::NoteOnVelocityZero) => {
            (pad, "RELEASE: Note On velocity 0")
        }
        Event::Release(pad, ReleaseType::Neutral) => {
            (pad, "RELEASE: joystick neutral region / prior direction")
        }
        Event::Release(pad, ReleaseType::PulseComplete) => {
            (pad, "RELEASE: shortcut pulse complete")
        }
        Event::DuplicatePress(pad) => (pad, "REPEAT IGNORED: pad already down"),
        Event::UnmatchedRelease(pad) => (pad, "RELEASE IGNORED: pad was not down"),
        Event::Pulse(pad) => (pad, "PULSE: movement step"),
        Event::Pressure(pad) => (pad, "PRESSURE IGNORED: does not change pad state"),
    };
    format!(
        "{action} | {} | MIDI {} channel {} | id {} (0x{:02X})",
        name(*pad),
        pad.message.text(),
        pad.channel,
        pad.id,
        pad.id
    )
}

#[cfg(test)]
mod tests {
    use super::{Detector, Event};

    #[test]
    fn press_pressure_duplicate_and_velocity_zero_release() {
        let mut detector = Detector::default();
        let press = detector.observe([0x99, 0x24, 0x20]).unwrap();
        assert!(matches!(press, Event::Press(_)));
        assert!(matches!(
            detector.observe([0x99, 0x24, 0x30]),
            Some(Event::DuplicatePress(_))
        ));
        assert!(matches!(
            detector.observe([0xA9, 0x24, 0x40]),
            Some(Event::Pressure(_))
        ));
        assert!(detector.observe([0xA9, 0x24, 0x50]).is_none());
        assert!(matches!(
            detector.observe([0x99, 0x24, 0]),
            Some(Event::Release(_, super::ReleaseType::NoteOnVelocityZero))
        ));
        assert!(detector.down.is_empty());

        match detector.observe([0x99, 0x2C, 0x20]) {
            Some(Event::Press(pad)) => {
                assert_eq!(pad.label(), "Bank B Pad 1");
            }
            event => panic!("expected Bank B Pad 1 press, got {event:?}"),
        }
    }

    /// Ticket 22/23: help lists every accepted command; piped help is plain; version has no I/O.
    #[test]
    fn help_lists_every_command_and_pipes_plain() {
        let plain = super::help(false);
        let commands = super::SESSION
            .iter()
            .chain(crate::runtime::COMMANDS.iter())
            .chain(&["agent-status", "daemon", "--help", "--version"]);
        for command in commands {
            assert!(plain.contains(command), "{command} missing from help");
        }
        for state in crate::status::STATES {
            assert!(plain.contains(state), "{state} missing from help");
        }
        assert!(!plain.contains('\x1b'));
        assert!(super::help(true).contains("\x1b[1mUSAGE\x1b[0m"));
        assert_eq!(
            super::version(),
            format!("vibeconsole {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
