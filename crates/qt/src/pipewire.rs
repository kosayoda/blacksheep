//! `Pipewire` singleton: default sink volume and mute state, every sink,
//! and the controls for them.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("blacksheep-qt/src/model.cxxqt.h");
        #[namespace = "blacksheep"]
        type ListModel = crate::model::qobject::ListModel;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[namespace = "blacksheep"]
        #[qproperty(u8, volume, READ, NOTIFY)]
        #[qproperty(bool, muted, READ, NOTIFY)]
        /// PipeWire node name of the default sink.
        #[qproperty(QString, sink_name, cxx_name = "sinkName", READ, NOTIFY)]
        /// Short human name of the default sink (`node.nick`).
        #[qproperty(QString, sink_nick, cxx_name = "sinkNick", READ, NOTIFY)]
        /// Long human name of the default sink (`node.description`).
        #[qproperty(QString, sink_description, cxx_name = "sinkDescription", READ, NOTIFY)]
        /// Every output: name, nick, description, isDefault, volume, muted. Default first.
        #[qproperty(*mut ListModel, sinks, READ, CONSTANT)]
        type Pipewire = super::PipewireRust;

        /// Set the default sink's volume, 0..=100.
        #[qinvokable]
        #[cxx_name = "setVolume"]
        fn set_volume(self: &Pipewire, percent: i32);
        #[qinvokable]
        #[cxx_name = "setMuted"]
        fn set_muted(self: &Pipewire, muted: bool);
        #[qinvokable]
        #[cxx_name = "toggleMute"]
        fn toggle_mute(self: &Pipewire);
        /// Volume of a specific sink by node name.
        #[qinvokable]
        #[cxx_name = "setSinkVolume"]
        fn set_sink_volume(self: &Pipewire, name: &QString, percent: i32);
        /// Make a sink the default output, by node name.
        #[qinvokable]
        #[cxx_name = "setDefaultSink"]
        fn set_default_sink(self: &Pipewire, name: &QString);
    }

    impl cxx_qt::Initialize for Pipewire {}
    impl cxx_qt::Threading for Pipewire {}
}

use std::pin::Pin;

use blacksheep_pipewire::{Command, Update};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;
use crate::service::set_property;

const SINK_ROLES: &[&str] = &["name", "nick", "description", "is_default", "volume", "muted"];

pub struct PipewireRust {
    volume: u8,
    muted: bool,
    sink_name: QString,
    sink_nick: QString,
    sink_description: QString,
    sinks: *mut ListModel,

    _model: UniquePtr<ListModel>,
    commands: Option<pipewire::channel::Sender<Command>>,
    task: Option<AbortHandle>,
}

impl Default for PipewireRust {
    fn default() -> Self {
        let mut model = new_list_model();
        model.pin_mut().configure(SINK_ROLES, Some("name"));
        Self {
            volume: 0,
            muted: false,
            sink_name: QString::default(),
            sink_nick: QString::default(),
            sink_description: QString::default(),
            sinks: model.as_mut_ptr(),
            _model: model,
            commands: None,
            task: None,
        }
    }
}

impl Drop for PipewireRust {
    fn drop(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(Command::Terminate);
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Pipewire {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let (command_tx, command_rx) = pipewire::channel::channel();

        // PipeWire wants its own blocking main loop on a dedicated thread.
        let spawned = std::thread::Builder::new()
            .name("pipewire".into())
            .spawn(move || {
                if let Err(err) = blacksheep_pipewire::Pipewire::connect(tx, command_rx) {
                    tracing::error!("pipewire: {err}");
                }
            });
        if let Err(err) = spawned {
            tracing::error!("pipewire: cannot spawn thread: {err}");
        }

        let task = crate::service::forward(self.qt_thread(), rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.commands = Some(command_tx);
        rust.task = Some(task);
    }
}

fn apply(mut obj: Pin<&mut qobject::Pipewire>, update: Update) {
    match update {
        Update::VolumeChanged(volume) => {
            set_property!(obj, volume, volume.value);
            set_property!(obj, muted, volume.muted);
        }
        Update::DefaultSink(sink) => {
            set_property!(obj, sink_name, QString::from(sink.name.as_str()));
            set_property!(obj, sink_nick, QString::from(sink.nick.as_str()));
            set_property!(obj, sink_description, QString::from(sink.description.as_str()));
        }
        Update::Sinks(sinks) => {
            let model = obj.sinks;
            // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
            unsafe { Pin::new_unchecked(&mut *model) }.sync(&sinks);
        }
    }
}

impl qobject::Pipewire {
    fn send(&self, command: Command) {
        if let Some(commands) = &self.commands
            && commands.send(command).is_err()
        {
            tracing::warn!("pipewire: command channel closed");
        }
    }

    fn set_volume(&self, percent: i32) {
        self.send(Command::SetVolume {
            node: None,
            percent: percent.clamp(0, 100) as u8,
        });
    }
    fn set_muted(&self, muted: bool) {
        self.send(Command::SetMuted { node: None, muted });
    }
    fn toggle_mute(&self) {
        self.set_muted(!self.muted);
    }
    fn set_sink_volume(&self, name: &QString, percent: i32) {
        self.send(Command::SetVolume {
            node: Some(name.to_string()),
            percent: percent.clamp(0, 100) as u8,
        });
    }
    fn set_default_sink(&self, name: &QString) {
        self.send(Command::SetDefaultSink(name.to_string()));
    }
}
