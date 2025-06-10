//! `Mpris` singleton: every MPRIS2 player as a list model, plus transport controls.

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
        #[qproperty(*mut ListModel, players, READ, CONSTANT)]
        type Mpris = super::MprisRust;

        #[qinvokable]
        #[cxx_name = "playPause"]
        fn play_pause(self: &Mpris, bus_name: &QString);
        #[qinvokable]
        fn play(self: &Mpris, bus_name: &QString);
        #[qinvokable]
        fn pause(self: &Mpris, bus_name: &QString);
        #[qinvokable]
        fn stop(self: &Mpris, bus_name: &QString);
        #[qinvokable]
        fn next(self: &Mpris, bus_name: &QString);
        #[qinvokable]
        fn previous(self: &Mpris, bus_name: &QString);
    }

    impl cxx_qt::Initialize for Mpris {}
    impl cxx_qt::Threading for Mpris {}
}

use std::pin::Pin;

use blacksheep_dbus::mpris::{self, Action, Controller, Player};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;

const PLAYER_ROLES: &[&str] = &[
    "bus_name",
    "identity",
    "playback_status",
    "title",
    "artist",
    "album",
    "art_url",
    "length_us",
    "can_play",
    "can_pause",
    "can_go_next",
    "can_go_previous",
];

pub struct MprisRust {
    players: *mut ListModel,
    _model: UniquePtr<ListModel>,
    controller: Option<Controller>,
    task: Option<AbortHandle>,
}

impl Default for MprisRust {
    fn default() -> Self {
        let mut model = new_list_model();
        model.pin_mut().configure(PLAYER_ROLES, Some("bus_name"));
        Self {
            players: model.as_mut_ptr(),
            _model: model,
            controller: None,
            task: None,
        }
    }
}

impl Drop for MprisRust {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Mpris {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let controller = mpris::start(tx);
        let task = crate::service::forward(self.qt_thread(), rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.controller = Some(controller);
        rust.task = Some(task);
    }
}

fn apply(obj: Pin<&mut qobject::Mpris>, players: Vec<Player>) {
    let model = obj.players;
    // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
    unsafe { Pin::new_unchecked(&mut *model) }.sync(&players);
}

impl qobject::Mpris {
    fn act(&self, bus_name: &QString, action: Action) {
        if let Some(controller) = &self.controller {
            controller.send(bus_name.to_string(), action);
        }
    }

    fn play_pause(&self, bus_name: &QString) {
        self.act(bus_name, Action::PlayPause);
    }
    fn play(&self, bus_name: &QString) {
        self.act(bus_name, Action::Play);
    }
    fn pause(&self, bus_name: &QString) {
        self.act(bus_name, Action::Pause);
    }
    fn stop(&self, bus_name: &QString) {
        self.act(bus_name, Action::Stop);
    }
    fn next(&self, bus_name: &QString) {
        self.act(bus_name, Action::Next);
    }
    fn previous(&self, bus_name: &QString) {
        self.act(bus_name, Action::Previous);
    }
}
