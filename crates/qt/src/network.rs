//! `Network` singleton over systemd-networkd and iwd.

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
        /// Some link is routable.
        #[qproperty(bool, connected, READ, NOTIFY)]
        /// `wifi`, `ethernet`, `other` or `none`.
        #[qproperty(QString, primary, READ, NOTIFY)]
        /// Every link: name, kind, operational, carrier, ipv4.
        #[qproperty(*mut ListModel, links, READ, CONSTANT)]
        /// Wi-Fi present on this machine (iwd station found).
        #[qproperty(bool, wifi_available, cxx_name = "wifiAvailable", READ, NOTIFY)]
        #[qproperty(bool, wifi_powered, cxx_name = "wifiPowered", READ, NOTIFY)]
        /// iwd station state: connected, disconnected, connecting, roaming...
        #[qproperty(QString, wifi_state, cxx_name = "wifiState", READ, NOTIFY)]
        #[qproperty(QString, ssid, READ, NOTIFY)]
        /// 0..=100 for the connected network.
        #[qproperty(i32, wifi_strength, cxx_name = "wifiStrength", READ, NOTIFY)]
        #[qproperty(bool, wifi_scanning, cxx_name = "wifiScanning", READ, NOTIFY)]
        /// Visible Wi-Fi networks: name, security, strength, connected, known.
        #[qproperty(*mut ListModel, networks, READ, CONSTANT)]
        type Network = super::NetworkRust;

        #[qinvokable]
        fn scan(self: &Network);
        /// Connect to a known network by name; unknown ones need credentials
        /// iwd does not have yet.
        #[qinvokable]
        #[cxx_name = "connectWifi"]
        fn connect_wifi(self: &Network, name: &QString);
        #[qinvokable]
        #[cxx_name = "disconnectWifi"]
        fn disconnect_wifi(self: &Network);
    }

    impl cxx_qt::Initialize for Network {}
    impl cxx_qt::Threading for Network {}
}

use std::pin::Pin;

use blacksheep_dbus::network::{self, Command, Controller, State};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;
use crate::service::set_property;

const LINK_ROLES: &[&str] = &["name", "kind", "operational", "carrier", "ipv4"];
const NETWORK_ROLES: &[&str] = &["name", "security", "strength", "connected", "known"];

pub struct NetworkRust {
    connected: bool,
    primary: QString,
    links: *mut ListModel,
    wifi_available: bool,
    wifi_powered: bool,
    wifi_state: QString,
    ssid: QString,
    wifi_strength: i32,
    wifi_scanning: bool,
    networks: *mut ListModel,

    _models: [UniquePtr<ListModel>; 2],
    controller: Option<Controller>,
    task: Option<AbortHandle>,
}

impl Default for NetworkRust {
    fn default() -> Self {
        let mut links = new_list_model();
        links.pin_mut().configure(LINK_ROLES, Some("name"));
        let mut networks = new_list_model();
        networks.pin_mut().configure(NETWORK_ROLES, Some("name"));
        Self {
            connected: false,
            primary: QString::from("none"),
            links: links.as_mut_ptr(),
            wifi_available: false,
            wifi_powered: false,
            wifi_state: QString::default(),
            ssid: QString::default(),
            wifi_strength: 0,
            wifi_scanning: false,
            networks: networks.as_mut_ptr(),
            _models: [links, networks],
            controller: None,
            task: None,
        }
    }
}

impl Drop for NetworkRust {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Network {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let controller = network::start(tx);
        let task = crate::service::forward(self.qt_thread(), rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.controller = Some(controller);
        rust.task = Some(task);
    }
}

fn apply(mut obj: Pin<&mut qobject::Network>, state: State) {
    set_property!(obj, connected, state.connected);
    set_property!(obj, primary, QString::from(state.primary.as_str()));
    let wifi = state.wifi.clone().unwrap_or_default();
    set_property!(obj, wifi_available, state.wifi.is_some());
    set_property!(obj, wifi_powered, wifi.powered);
    set_property!(obj, wifi_state, QString::from(wifi.state.as_str()));
    set_property!(obj, ssid, QString::from(wifi.ssid.as_str()));
    set_property!(obj, wifi_strength, i32::from(wifi.strength));
    set_property!(obj, wifi_scanning, wifi.scanning);
    let (links, networks) = (obj.links, obj.networks);
    // SAFETY: owned by the UniquePtrs in this object, used on the Qt thread.
    unsafe {
        Pin::new_unchecked(&mut *links).sync(&state.links);
        Pin::new_unchecked(&mut *networks).sync(&state.networks);
    }
}

impl qobject::Network {
    fn send(&self, command: Command) {
        if let Some(controller) = &self.controller {
            controller.send(command);
        }
    }
    fn scan(&self) {
        self.send(Command::Scan);
    }
    fn connect_wifi(&self, name: &QString) {
        self.send(Command::ConnectWifi(name.to_string()));
    }
    fn disconnect_wifi(&self) {
        self.send(Command::DisconnectWifi);
    }
}
