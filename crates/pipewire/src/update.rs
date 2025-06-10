use serde::Serialize;

use crate::info::Volume;

#[derive(Debug, Clone)]
pub enum Update {
    VolumeChanged(Volume),
    /// The default audio sink changed (or its metadata became known).
    DefaultSink(Sink),
    /// Every audio sink, default first.
    Sinks(Vec<Sink>),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Sink {
    /// `node.name`, e.g. `alsa_output.usb-...analog-stereo`.
    pub name: String,
    /// `node.nick`, the short human name.
    pub nick: String,
    /// `node.description`, the long human name.
    pub description: String,
    pub is_default: bool,
    /// 0..=100, cubic scale like pavucontrol shows it.
    pub volume: u8,
    pub muted: bool,
}

/// Requests from the UI, executed on the PipeWire thread.
#[derive(Debug, Clone)]
pub enum Command {
    Terminate,
    /// Volume for a sink by node name, or the default sink when `None`.
    SetVolume { node: Option<String>, percent: u8 },
    SetMuted { node: Option<String>, muted: bool },
    SetDefaultSink(String),
}
