//! v7.8 ticket 25: sound combos: a named speaker, microphone and, optionally, a card's mode
//! (the HyperX dongle's "Analog Stereo Output + Mono Input"). Read from `pw-dump`, set with
//! `wpctl set-profile` and `wpctl set-default` through an injected runner (the gamepad
//! module's shape: program first, one argument each, never a shell). Devices, cards and modes
//! are kept by name (ids change) with the label shown for them; a mode is set by the index it
//! has at that moment.
use crate::Result;

/// The longest device, card or mode name or label kept (PipeWire's are well under it).
const MAX_NAME: usize = 200;

/// A PipeWire name and the label shown for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Named {
    pub name: String,
    pub label: String,
}
/// A card (`device.name`) and one of its modes (profile name, e.g.
/// `output:analog-stereo+input:mono-fallback`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CardMode {
    pub card: Named,
    pub mode: Named,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Combo {
    pub name: String,
    /// `node.name` of the speaker (a sink) and the microphone (a source).
    pub speaker: Named,
    pub mic: Named,
    pub mode: Option<CardMode>,
}
impl Combo {
    pub fn validate(&self) -> Result<()> {
        if !crate::mappings::simple_name(&self.name) {
            return Err("a sound combo's name is letters, digits, ., - and _".into());
        }
        let named = [&self.speaker, &self.mic]
            .into_iter()
            .chain(self.mode.iter().flat_map(|m| [&m.card, &m.mode]));
        for text in named.flat_map(|n| [&n.name, &n.label]) {
            if text.is_empty() || text.len() > MAX_NAME || text.chars().any(char::is_control) {
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

/// A speaker (sink) or microphone (source) PipeWire lists now.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub id: u32,
    pub device: Named,
    pub sink: bool,
}
/// A sound card PipeWire lists now, its modes (index, name and label; "off" left out) and
/// the index of the mode it is in.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u32,
    pub device: Named,
    pub modes: Vec<(u32, Named)>,
    pub current: Option<u32>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub cards: Vec<Card>,
}
impl Graph {
    pub fn node(&self, name: &str, sink: bool) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|n| n.device.name == name && n.sink == sink)
    }
    pub fn card(&self, name: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.device.name == name)
    }
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
        let named = |name: &str, label: &str| {
            let name = props[name].as_str().unwrap_or_default().to_owned();
            let label = props[label]
                .as_str()
                .filter(|l| !l.is_empty())
                .unwrap_or(&name)
                .to_owned();
            (!name.is_empty()).then_some(Named { name, label })
        };
        match (object["type"].as_str(), props["media.class"].as_str()) {
            (Some("PipeWire:Interface:Node"), Some(class @ ("Audio/Sink" | "Audio/Source"))) => {
                if let Some(device) = named("node.name", "node.description") {
                    graph.nodes.push(Node {
                        id,
                        device,
                        sink: class == "Audio/Sink",
                    });
                }
            }
            (Some("PipeWire:Interface:Device"), Some("Audio/Device")) => {
                let Some(device) = named("device.name", "device.description") else {
                    continue;
                };
                let params = &object["info"]["params"];
                let modes = (params["EnumProfile"].as_array().into_iter().flatten())
                    .filter_map(|p| {
                        let index = u32::try_from(p["index"].as_u64()?).ok()?;
                        let name = p["name"].as_str()?.to_owned();
                        let label = p["description"].as_str().unwrap_or(&name).to_owned();
                        (name != "off").then_some((index, Named { name, label }))
                    })
                    .collect();
                let current = params["Profile"][0]["index"]
                    .as_u64()
                    .and_then(|i| u32::try_from(i).ok());
                graph.cards.push(Card {
                    id,
                    device,
                    modes,
                    current,
                });
            }
            _ => {}
        }
    }
    graph
}

/// Read what PipeWire lists now.
pub fn read(run: &mut dyn FnMut(&[&str]) -> Result<String>) -> Result<Graph> {
    Ok(graph(
        &run(&["pw-dump"]).map_err(|e| format!("sound: read PipeWire: {e}"))?,
    ))
}

/// Put `card` in mode `index` (`wpctl set-profile`).
pub fn set_mode(
    run: &mut dyn FnMut(&[&str]) -> Result<String>,
    card: u32,
    index: u32,
) -> Result<()> {
    run(&[
        "wpctl",
        "set-profile",
        &card.to_string(),
        &index.to_string(),
    ])
    .map(drop)
    .map_err(|e| format!("sound: set the card's mode: {e}").into())
}

/// Switch to `combo`: the card's mode first (its speaker or mic may appear only in that
/// mode), then the default speaker and microphone. A device that isn't there makes the
/// combo unavailable, naming it; a mode changed for devices that then don't appear is put
/// back, so a failure leaves the sound as it was (unless `wpctl` itself fails part-way).
pub fn apply(combo: &Combo, run: &mut dyn FnMut(&[&str]) -> Result<String>) -> Result<String> {
    let unavailable = |what: &Named| {
        format!(
            "sound combo {}: unavailable: {} isn't connected",
            combo.name, what.label
        )
    };
    let mut now = read(run)?;
    if let Some(CardMode { card, mode }) = &combo.mode {
        let found = now.card(&card.name).ok_or_else(|| unavailable(card))?;
        let (id, before) = (found.id, found.current);
        let index = (found.modes.iter().find(|(_, m)| m.name == mode.name))
            .map(|(index, _)| *index)
            .ok_or_else(|| {
                format!(
                    "sound combo {}: unavailable: {} has no mode {}",
                    combo.name, card.label, mode.label
                )
            })?;
        if before != Some(index) {
            set_mode(run, id, index)?;
            // ponytail: polls pw-dump up to 10 × 100 ms for the mode's devices to appear;
            // wait on PipeWire's events if a card ever takes longer.
            for attempt in 0..10 {
                now = read(run)?;
                if now.node(&combo.speaker.name, true).is_some()
                    && now.node(&combo.mic.name, false).is_some()
                {
                    break;
                }
                if attempt < 9 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            let missing = [(&combo.speaker, true), (&combo.mic, false)]
                .into_iter()
                .find(|(device, sink)| now.node(&device.name, *sink).is_none());
            if let Some((device, _)) = missing {
                if let Some(before) = before {
                    set_mode(run, id, before)?; // nothing half set
                }
                return Err(unavailable(device).into());
            }
        }
    }
    let mut ids = Vec::new();
    for (device, sink) in [(&combo.speaker, true), (&combo.mic, false)] {
        ids.push(
            now.node(&device.name, sink)
                .ok_or_else(|| unavailable(device))?
                .id,
        );
    }
    for id in ids {
        run(&["wpctl", "set-default", &id.to_string()])
            .map_err(|e| format!("sound: set the default: {e}"))?;
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
    fn named(name: &str, label: &str) -> Named {
        Named {
            name: name.into(),
            label: label.into(),
        }
    }
    fn combo(mode: bool) -> Combo {
        Combo {
            name: "headset".into(),
            speaker: named("alsa_output.hyperx", "HyperX Analog Stereo"),
            mic: named("alsa_input.hyperx", "HyperX Mono"),
            mode: mode.then(|| CardMode {
                card: named("alsa_card.hyperx", "HyperX"),
                mode: named(
                    "output:analog-stereo+input:mono-fallback",
                    "Analog Stereo Output + Mono Input",
                ),
            }),
        }
    }
    /// A runner answering pw-dump from the card's current mode, which set-profile changes;
    /// with `mic_appears` false the mic never shows (a mode that doesn't bring it).
    fn runner<'a>(
        mode: &'a mut u32,
        calls: &'a mut Vec<String>,
        mic_appears: bool,
    ) -> impl FnMut(&[&str]) -> Result<String> + 'a {
        move |argv| {
            calls.push(argv.join(" "));
            if argv.get(1) == Some(&"set-profile") {
                *mode = argv[3].parse().unwrap();
            }
            Ok(match argv[0] {
                "pw-dump" if !mic_appears => dump(2),
                "pw-dump" => dump(*mode),
                _ => String::new(),
            })
        }
    }

    #[test]
    fn a_combo_sets_both_defaults() {
        let (mut mode, mut calls) = (1, Vec::new());
        assert_eq!(
            apply(&combo(false), &mut runner(&mut mode, &mut calls, true)).unwrap(),
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
        apply(&combo(true), &mut runner(&mut mode, &mut calls, true)).unwrap();
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
        apply(&combo(true), &mut runner(&mut mode, &mut calls, true)).unwrap();
        assert_eq!(
            calls,
            ["pw-dump", "wpctl set-default 80", "wpctl set-default 81"]
        );
    }

    #[test]
    fn a_missing_device_is_unavailable_by_its_label_and_nothing_is_left_changed() {
        // Speaker-only mode, no mode in the combo: the mic isn't there; nothing is set.
        let (mut mode, mut calls) = (2, Vec::new());
        let error = apply(&combo(false), &mut runner(&mut mode, &mut calls, true))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "sound combo headset: unavailable: HyperX Mono isn't connected"
        );
        assert_eq!(calls, ["pw-dump"]);
        // The mode is set but the mic never appears: the mode before is put back.
        let (mut mode, mut calls) = (2, Vec::new());
        let error = apply(&combo(true), &mut runner(&mut mode, &mut calls, false))
            .unwrap_err()
            .to_string();
        assert!(error.ends_with("HyperX Mono isn't connected"), "{error}");
        assert_eq!(calls[..2], ["pw-dump", "wpctl set-profile 45 1"]);
        assert_eq!(calls.last().unwrap(), "wpctl set-profile 45 2", "{calls:?}");
        assert!(!calls.iter().any(|c| c.contains("set-default")));
        // The card unplugged.
        let mut gone = combo(true);
        gone.mode.as_mut().unwrap().card = named("alsa_card.other", "Other card");
        let (mut mode, mut calls) = (1, Vec::new());
        let error = apply(&gone, &mut runner(&mut mode, &mut calls, true))
            .unwrap_err()
            .to_string();
        assert!(
            error.ends_with("unavailable: Other card isn't connected"),
            "{error}"
        );
    }

    #[test]
    fn names_are_validated() {
        let mut bad = combo(false);
        bad.name = "../x".into();
        assert!(bad.validate().is_err());
        let mut bad = combo(false);
        bad.mic.name = "a\nb".into();
        assert!(bad.validate().is_err());
        assert!(combo(true).validate().is_ok());
    }
}
