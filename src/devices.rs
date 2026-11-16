//! Device discovery.

use cpal::traits::{DeviceTrait, HostTrait};

/// An audio source. `loopback == true` captures what an *output* device is playing (WASAPI loopback).
#[derive(Clone, PartialEq, Debug)]
pub struct InputSource {
    pub name: String,
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
