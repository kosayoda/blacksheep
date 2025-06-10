//! `Bluetooth` singleton over BlueZ.

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
        /// BlueZ is running and an adapter exists.
        #[qproperty(bool, available, READ, NOTIFY)]
        #[qproperty(bool, powered, READ, NOTIFY)]
        #[qproperty(bool, discovering, READ, NOTIFY)]
        #[qproperty(QString, adapter_name, cxx_name = "adapterName", READ, NOTIFY)]
        /// Known devices: connected first, then paired, then by name.
        #[qproperty(*mut ListModel, devices, READ, CONSTANT)]
        type Bluetooth = super::BluetoothRust;

        #[qinvokable]
        #[cxx_name = "setPowered"]
        fn set_powered(self: &Bluetooth, on: bool);
        #[qinvokable]
        #[cxx_name = "startDiscovery"]
        fn start_discovery(self: &Bluetooth);
        #[qinvokable]
        #[cxx_name = "stopDiscovery"]
        fn stop_discovery(self: &Bluetooth);
        /// Connect to a device by address (`AC:80:0A:...`).
        #[qinvokable]
        fn connect(self: &Bluetooth, address: &QString);
        #[qinvokable]
        fn disconnect(self: &Bluetooth, address: &QString);
    }

    impl cxx_qt::Initialize for Bluetooth {}
    impl cxx_qt::Threading for Bluetooth {}
}

use std::pin::Pin;

use blacksheep_dbus::bluetooth::{self, Command, Controller, State};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;
use crate::service::set_property;

const ROLES: &[&str] = &["path", "address", "name", "icon", "connected", "paired", "trusted", "rssi", "battery"];

pub struct BluetoothRust {
    available: bool,
    powered: bool,
    discovering: bool,
    adapter_name: QString,
    devices: *mut ListModel,

    _model: UniquePtr<ListModel>,
    controller: Option<Controller>,
    task: Option<AbortHandle>,
}

impl Default for BluetoothRust {
    fn default() -> Self {
        let mut model = new_list_model();
        model.pin_mut().configure(ROLES, Some("address"));
        Self {
            available: false,
            powered: false,
            discovering: false,
            adapter_name: QString::default(),
            devices: model.as_mut_ptr(),
            _model: model,
            controller: None,
            task: None,
        }
    }
}

impl Drop for BluetoothRust {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Bluetooth {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let controller = bluetooth::start(tx);
        let task = crate::service::forward(self.qt_thread(), rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.controller = Some(controller);
        rust.task = Some(task);
    }
}

fn apply(mut obj: Pin<&mut qobject::Bluetooth>, state: State) {
    let adapter = state.adapter.clone().unwrap_or_default();
    set_property!(obj, available, state.available);
    set_property!(obj, powered, adapter.powered);
    set_property!(obj, discovering, adapter.discovering);
    set_property!(obj, adapter_name, QString::from(adapter.name.as_str()));
    let model = obj.devices;
    // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
    unsafe { Pin::new_unchecked(&mut *model) }.sync(&state.devices);
}

impl qobject::Bluetooth {
    fn send(&self, command: Command) {
        if let Some(controller) = &self.controller {
            controller.send(command);
        }
    }
    fn set_powered(&self, on: bool) {
        self.send(Command::SetPowered(on));
    }
    fn start_discovery(&self) {
        self.send(Command::StartDiscovery);
    }
    fn stop_discovery(&self) {
        self.send(Command::StopDiscovery);
    }
    fn connect(&self, address: &QString) {
        self.send(Command::Connect(address.to_string()));
    }
    fn disconnect(&self, address: &QString) {
        self.send(Command::Disconnect(address.to_string()));
    }
}
