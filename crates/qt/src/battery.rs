//! `Battery` singleton: UPower's aggregate display device plus a `devices`
//! model with one row per physical battery.

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
        #[qproperty(bool, present, READ, NOTIFY)]
        #[qproperty(f64, percentage, READ, NOTIFY)]
        #[qproperty(QString, state, READ, NOTIFY)]
        #[qproperty(i64, time_to_empty, cxx_name = "timeToEmpty", READ, NOTIFY)]
        #[qproperty(i64, time_to_full, cxx_name = "timeToFull", READ, NOTIFY)]
        #[qproperty(QString, icon_name, cxx_name = "iconName", READ, NOTIFY)]
        /// One row per physical battery, keyed by D-Bus path, sorted by native path.
        #[qproperty(*mut ListModel, devices, READ, CONSTANT)]
        type Battery = super::BatteryRust;
    }

    impl cxx_qt::Initialize for Battery {}
    impl cxx_qt::Threading for Battery {}
}

use std::pin::Pin;

use blacksheep_dbus::upower::{self, Update};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;
use crate::service::set_property;

const DEVICE_ROLES: &[&str] = &[
    "path",
    "native_path",
    "vendor",
    "model",
    "present",
    "percentage",
    "state",
    "time_to_empty",
    "time_to_full",
    "energy",
    "energy_full",
    "energy_full_design",
    "energy_rate",
    "capacity",
    "icon_name",
];

pub struct BatteryRust {
    present: bool,
    percentage: f64,
    state: QString,
    time_to_empty: i64,
    time_to_full: i64,
    icon_name: QString,
    devices: *mut ListModel,

    _model: UniquePtr<ListModel>,
    tasks: Vec<AbortHandle>,
}

impl Default for BatteryRust {
    fn default() -> Self {
        let mut model = new_list_model();
        model.pin_mut().configure(DEVICE_ROLES, Some("path"));
        Self {
            present: false,
            percentage: 0.0,
            state: QString::from("unknown"),
            time_to_empty: 0,
            time_to_full: 0,
            icon_name: QString::default(),
            devices: model.as_mut_ptr(),
            _model: model,
            tasks: Vec::new(),
        }
    }
}

impl Drop for BatteryRust {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Battery {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let producer = tokio::spawn(upower::run(tx)).abort_handle();
        let consumer = crate::service::forward(self.qt_thread(), rx, apply);
        self.as_mut().rust_mut().tasks = vec![producer, consumer];
    }
}

fn apply(mut obj: Pin<&mut qobject::Battery>, update: Update) {
    match update {
        Update::Aggregate(battery) => {
            set_property!(obj, present, battery.present);
            set_property!(obj, percentage, battery.percentage);
            set_property!(obj, state, QString::from(battery.state.as_str()));
            set_property!(obj, time_to_empty, battery.time_to_empty);
            set_property!(obj, time_to_full, battery.time_to_full);
            set_property!(obj, icon_name, QString::from(battery.icon_name.as_str()));
        }
        Update::Devices(devices) => {
            let model = obj.devices;
            // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
            unsafe { Pin::new_unchecked(&mut *model) }.sync(&devices);
        }
    }
}
