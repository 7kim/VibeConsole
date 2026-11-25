//! v7.8 ticket 25: sound combos: a named speaker, microphone and, optionally, a card's mode
//! (the HyperX dongle's "Analog Stereo Output + Mono Input"). Read from `pw-dump`, set with
//! `wpctl set-profile` and `wpctl set-default` through an injected runner, never a shell.
//! Nodes and cards are kept by name (their ids change); a mode by its name, set by the index
//! it has at that moment.
use crate::Result;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Combo {
    pub name: String,
    /// The speaker's and microphone's PipeWire `node.name`.
    pub speaker: String,
    pub mic: String,
    /// A card's `device.name` and the name of the mode (profile) to set, e.g.
    /// `output:analog-stereo+input:mono-fallback`.
    pub mode: Option<(String, String)>,
}
impl Combo {
    pub fn validate(&self) -> Result<()> {
        if !crate::mappings::simple_name(&self.name) {
            return Err("a sound combo's name is letters, digits, ., - and _".into());
        }
        let names = [&self.speaker, &self.mic]
            .into_iter()
            .chain(self.mode.iter().flat_map(|(card, mode)| [card, mode]));
        for name in names {
            if name.is_empty() || name.len() > 200 || name.chars().any(char::is_control) {
                return Err(format!(
                    "sound combo {}: a device or mode name is invalid",
                    self.name
                )
                .into());
            }
        }
        Ok(())
    }
}

/// A speaker (sink) or microphone (source).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: u32,
    pub name: String,
    pub label: String,
    pub sink: bool,
}
/// A sound card and its modes: (index, name, label).
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u32,
    pub name: String,
    pub label: String,
    pub modes: Vec<(u32, String, String)>,
    pub current: Option<u32>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub cards: Vec<Card>,
}

/// The speakers, microphones and cards in a `pw-dump`; anything unreadable is left out.
pub fn graph(pw_dump: &str) -> Graph {
    let mut graph = Graph::default();
    let Ok(serde_json::Value::Array(objects)) = serde_json::from_str(pw_dump) else {
        return graph;
    };
    for object in &objects {
        let props = &object["info"]["props"];
        let Some(id) = object["id"].as_u64().and_then(|id| u32::try_from(id).ok()) else {
            continue;
        };
        let text = |key: &str| props[key].as_str().unwrap_or_default().to_owned();
        match (object["type"].as_str(), props["media.class"].as_str()) {
            (Some("PipeWire:Interface:Node"), Some(class @ ("Audio/Sink" | "Audio/Source"))) => {
                let name = text("node.name");
                if !name.is_empty() {
                    let label = Some(text("node.description")).filter(|l| !l.is_empty());
                    graph.nodes.push(Node {
                        id,
                        label: label.unwrap_or_else(|| name.clone()),
                        name,
                        sink: class == "Audio/Sink",
                    });
                }
            }
            (Some("PipeWire:Interface:Device"), Some("Audio/Device")) => {
                let params = &object["info"]["params"];
                let modes = (params["EnumProfile"].as_array().into_iter().flatten())
                    .filter_map(|p| {
                        let index = u32::try_from(p["index"].as_u64()?).ok()?;
                        let name = p["name"].as_str()?.to_owned();
                        let label = p["description"].as_str().unwrap_or(&name).to_owned();
                        (name != "off").then_some((index, name, label))
                    })
                    .collect();
                let current = params["Profile"][0]["index"]
                    .as_u64()
                    .and_then(|i| u32::try_from(i).ok());
                graph.cards.push(Card {
                    id,
                    name: text("device.name"),
                    label: text("device.description"),
                    modes,
                    current,
                });
            }
            _ => {}
        }
    }
    graph
}

/// Switch to `combo`: the card's mode first (its speaker or mic may appear only in that
/// mode), then the default speaker and microphone. `run(program, args)` runs one program.
/// A device that isn't there makes the combo unavailable, naming it; nothing is half set
/// unless `wpctl` itself fails between the two defaults.
pub fn apply(
    combo: &Combo,
    mut run: impl FnMut(&str, &[String]) -> Result<String>,
) -> Result<String> {
    let unavailable = |what: &str| {
        format!(
            "sound combo {}: unavailable: {what} isn't connected",
            combo.name
        )
    };
    let mut graph = graph(&run("pw-dump", &[])?);
    if let Some((card_name, mode)) = &combo.mode {
        let card = (graph.cards.iter().find(|c| c.name == *card_name))
            .ok_or_else(|| unavailable(card_name))?;
        let (index, _, _) =
            (card.modes.iter().find(|(_, name, _)| name == mode)).ok_or_else(|| {
                format!(
                    "sound combo {}: unavailable: {} has no mode {mode}",
                    combo.name, card.label
                )
            })?;
        if card.current != Some(*index) {
            run(
                "wpctl",
                &["set-profile".into(), card.id.to_string(), index.to_string()],
            )?;
            // ponytail: polls pw-dump up to 10 × 100 ms for the mode's devices to appear;
            // wait on PipeWire's events if a card ever takes longer.
            for attempt in 0.. {
                graph = self::graph(&run("pw-dump", &[])?);
                let found =
                    |name: &str, sink| graph.nodes.iter().any(|n| n.name == name && n.sink == sink);
                if (found(&combo.speaker, true) && found(&combo.mic, false)) || attempt == 9 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let mut ids = Vec::new();
    for (name, sink) in [(&combo.speaker, true), (&combo.mic, false)] {
        let node = (graph
            .nodes
            .iter()
            .find(|n| n.name == *name && n.sink == sink))
        .ok_or_else(|| unavailable(name))?;
        ids.push(node.id);
    }
    for id in ids {
        run("wpctl", &["set-default".into(), id.to_string()])?;
    }
    Ok(format!("Sound: {}", combo.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pw-dump with the HyperX dongle in `mode` (1: speaker and mic, 2: speaker only) and
    /// the Nommo speakers.
    fn dump(mode: u32) -> String {
        let node = |id: u32, name: &str, label: &str, class: &str| {
            serde_json::json!({"id": id, "type": "PipeWire:Interface:Node",
                "info": {"props": {"node.name": name, "node.description": label, "media.class": class}}})
        };
        let mut objects = vec![
            node(
                80,
                "alsa_output.hyperx",
                "HyperX Analog Stereo",
                "Audio/Sink",
            ),
            node(90, "alsa_output.nommo", "Nommo Chroma", "Audio/Sink"),
            serde_json::json!({"id": 45, "type": "PipeWire:Interface:Device", "info": {
                "props": {"device.name": "alsa_card.hyperx", "device.description": "HyperX", "media.class": "Audio/Device"},
                "params": {
                    "EnumProfile": [
                        {"index": 0, "name": "off", "description": "Off"},
                        {"index": 1, "name": "output:analog-stereo+input:mono-fallback", "description": "Analog Stereo Output + Mono Input"},
                        {"index": 2, "name": "output:analog-stereo", "description": "Analog Stereo Output"}],
                    "Profile": [{"index": mode}]}}}),
        ];
        if mode == 1 {
            objects.push(node(81, "alsa_input.hyperx", "HyperX Mono", "Audio/Source"));
        }
        serde_json::Value::Array(objects).to_string()
    }
    fn combo(mode: bool) -> Combo {
        Combo {
            name: "headset".into(),
            speaker: "alsa_output.hyperx".into(),
            mic: "alsa_input.hyperx".into(),
            mode: mode.then(|| {
                (
                    "alsa_card.hyperx".into(),
                    "output:analog-stereo+input:mono-fallback".into(),
                )
            }),
        }
    }
    /// A runner answering pw-dump from the card's current mode, which set-profile changes.
    fn runner(
        mode: &mut u32,
        calls: &mut Vec<String>,
    ) -> impl FnMut(&str, &[String]) -> Result<String> {
        move |program, args| {
            calls.push(format!("{program} {}", args.join(" ")).trim().to_owned());
            if args.first().map(String::as_str) == Some("set-profile") {
                *mode = args[2].parse().unwrap();
            }
            Ok(if program == "pw-dump" {
                dump(*mode)
            } else {
                String::new()
            })
        }
    }

    #[test]
    fn a_combo_sets_both_defaults() {
        let (mut mode, mut calls) = (1, Vec::new());
        assert_eq!(
            apply(&combo(false), runner(&mut mode, &mut calls)).unwrap(),
            "Sound: headset"
        );
        assert_eq!(
            calls,
            ["pw-dump", "wpctl set-default 80", "wpctl set-default 81"]
        );
    }

    #[test]
    fn the_mode_is_set_by_index_before_the_defaults() {
        let (mut mode, mut calls) = (2, Vec::new());
        apply(&combo(true), runner(&mut mode, &mut calls)).unwrap();
        assert_eq!(
            calls,
            [
                "pw-dump",
                "wpctl set-profile 45 1",
                "pw-dump",
                "wpctl set-default 80",
                "wpctl set-default 81"
            ]
        );
        // Already in that mode: not set again.
        let (mut mode, mut calls) = (1, Vec::new());
        apply(&combo(true), runner(&mut mode, &mut calls)).unwrap();
        assert_eq!(
            calls,
            ["pw-dump", "wpctl set-default 80", "wpctl set-default 81"]
        );
    }

    #[test]
    fn a_missing_device_is_unavailable_and_nothing_is_set() {
        // Speaker-only mode, no mode in the combo: the mic isn't there.
        let (mut mode, mut calls) = (2, Vec::new());
        let error = apply(&combo(false), runner(&mut mode, &mut calls))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "sound combo headset: unavailable: alsa_input.hyperx isn't connected"
        );
        assert_eq!(calls, ["pw-dump"]);
        // The card unplugged.
        let mut gone = combo(true);
        gone.mode.as_mut().unwrap().0 = "alsa_card.other".into();
        let (mut mode, mut calls) = (1, Vec::new());
        let error = apply(&gone, runner(&mut mode, &mut calls))
            .unwrap_err()
            .to_string();
        assert!(
            error.ends_with("unavailable: alsa_card.other isn't connected"),
            "{error}"
        );
    }

    #[test]
    fn the_graph_lists_labels_and_modes_without_off() {
        let g = graph(&dump(1));
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.nodes[0].label, "HyperX Analog Stereo");
        assert_eq!(g.cards[0].modes.len(), 2);
        assert_eq!(g.cards[0].current, Some(1));
        assert!(graph("not json").nodes.is_empty());
    }

    #[test]
    fn names_are_validated() {
        let mut bad = combo(false);
        bad.name = "../x".into();
        assert!(bad.validate().is_err());
        let mut bad = combo(false);
        bad.mic = "a\nb".into();
        assert!(bad.validate().is_err());
        assert!(combo(true).validate().is_ok());
    }
}
