//! Device discovery.

use cpal::traits::{DeviceTrait, HostTrait};

/// An audio source. `loopback == true` captures what an *output* device is playing (WASAPI loopback).
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct InputSource {
    #[cfg_attr(feature = "serde", serde(default))]
    pub name: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub loopback: bool,
}

impl InputSource {
    pub fn label(&self) -> String {
        if self.loopback {
            format!("[Loopback] {}", self.name)
        } else {
            self.name.clone()
        }
    }
}

pub fn list_inputs() -> Vec<InputSource> {
    let host = cpal::default_host();
    let mut v = vec![];
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                v.push(InputSource {
                    name,
                    loopback: false,
                });
            }
        }
    }
    for name in list_outputs() {
        v.push(InputSource {
            name,
            loopback: true,
        });
    }
    v
}

pub fn list_outputs() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

pub fn default_output_name() -> Option<String> {
    cpal::default_host().default_output_device()?.name().ok()
}

pub fn default_input_name() -> Option<String> {
    cpal::default_host().default_input_device()?.name().ok()
}

/// The render endpoint that *feeds* a capture source, by Windows naming convention.
///
/// * a loopback tap is fed by the very output device it taps;
/// * a virtual cable's capture side (`CABLE Output (...)`) is fed by its render side
///   (`CABLE Input (...)`), because that is all a cable is: a wire between its two ends;
/// * a microphone is fed by nothing, so this returns `None`.
///
/// Pure string work on purpose: it is cheap enough to answer on every frame, and it never touches
/// COM, so it cannot perturb an audio thread.
pub fn fed_by(name: &str, loopback: bool) -> Option<String> {
    if loopback {
        Some(name.to_string())
    } else {
        capture_peer(name)
    }
}

/// The capture endpoint that hears a render endpoint: the other end of [`fed_by`].
/// `CABLE Input (...)` -> `CABLE Output (...)`. `None` for a real speaker.
pub fn capture_peer(render_name: &str) -> Option<String> {
    swap_io(render_name)
}

/// Whether two endpoint names are the same device, ignoring case and padding.
pub fn same_device(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Flip the first whole-word `Input`/`Output` token, leaving the rest of the name untouched so the
/// two halves of one cable compare equal afterwards. Works in both directions, because a render
/// endpoint is asked for its capture peer and a capture endpoint for the render one.
fn swap_io(name: &str) -> Option<String> {
    const PAIRS: [(&str, &str); 6] = [
        ("Input", "Output"),
        ("Output", "Input"),
        ("INPUT", "OUTPUT"),
        ("OUTPUT", "INPUT"),
        ("input", "output"),
        ("output", "input"),
    ];
    // Earliest whole-word hit in the name wins: `CABLE Input` and `CABLE Output` map onto each other.
    let mut best: Option<(usize, &str, &str)> = None;
    for (from, to) in PAIRS {
        let mut start = 0;
        while let Some(rel) = name[start..].find(from) {
            let i = start + rel;
            let before = &name[..i];
            let after = &name[i + from.len()..];
            let word_end = before
                .as_bytes()
                .last()
                .is_none_or(|b| !b.is_ascii_alphanumeric());
            let word_start = after
                .as_bytes()
                .first()
                .is_none_or(|b| !b.is_ascii_alphanumeric());
            if word_end && word_start {
                if best.is_none_or(|(at, _, _)| i < at) {
                    best = Some((i, from, to));
                }
                break;
            }
            start = i + from.len();
        }
    }
    best.map(|(i, from, to)| format!("{}{}{}", &name[..i], to, &name[i + from.len()..]))
}

pub(crate) fn find_output(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.output_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

pub(crate) fn find_input(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.input_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RENDER: &str = "CABLE Input (VB-Audio Virtual Cable)";
    const CAPTURE: &str = "CABLE Output (VB-Audio Virtual Cable)";

    #[test]
    fn the_two_ends_of_a_cable_find_each_other() {
        assert_eq!(capture_peer(RENDER).as_deref(), Some(CAPTURE));
        assert_eq!(fed_by(CAPTURE, false).as_deref(), Some(RENDER));
    }

    #[test]
    fn a_loopback_tap_is_fed_by_the_device_it_taps() {
        assert_eq!(fed_by(RENDER, true).as_deref(), Some(RENDER));
    }

    #[test]
    fn a_microphone_is_fed_by_nothing() {
        assert_eq!(fed_by("Headset Microphone (MH752)", false), None);
        assert_eq!(capture_peer("Speakers (Realtek(R) Audio)"), None);
    }

    #[test]
    fn only_whole_words_are_swapped() {
        assert_eq!(capture_peer("MultiOutput Device"), None);
    }

    #[test]
    fn device_names_compare_loosely() {
        assert!(same_device(" CABLE Input ", "cable input"));
        assert!(!same_device(RENDER, CAPTURE));
    }
}
