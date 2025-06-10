//! `Power` singleton: suspend, reboot, power off and hibernate through logind.

#[cxx_qt::bridge]
pub mod qobject {
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[namespace = "blacksheep"]
        #[qproperty(bool, can_suspend, cxx_name = "canSuspend", READ, NOTIFY)]
        #[qproperty(bool, can_reboot, cxx_name = "canReboot", READ, NOTIFY)]
        #[qproperty(bool, can_power_off, cxx_name = "canPowerOff", READ, NOTIFY)]
        #[qproperty(bool, can_hibernate, cxx_name = "canHibernate", READ, NOTIFY)]
        #[qproperty(bool, can_hybrid_sleep, cxx_name = "canHybridSleep", READ, NOTIFY)]
        type Power = super::PowerRust;

        #[qinvokable]
        fn suspend(self: &Power);
        #[qinvokable]
        fn reboot(self: &Power);
        #[qinvokable]
        #[cxx_name = "powerOff"]
        fn power_off(self: &Power);
        #[qinvokable]
        fn hibernate(self: &Power);
        #[qinvokable]
        #[cxx_name = "hybridSleep"]
        fn hybrid_sleep(self: &Power);
    }

    impl cxx_qt::Initialize for Power {}
    impl cxx_qt::Threading for Power {}
}

use std::pin::Pin;

use blacksheep_dbus::login1::{self, Action, Capabilities};
use cxx_qt::{CxxQtType, Threading};
use tokio::task::AbortHandle;

use crate::service::set_property;

#[derive(Default)]
pub struct PowerRust {
    can_suspend: bool,
    can_reboot: bool,
    can_power_off: bool,
    can_hibernate: bool,
    can_hybrid_sleep: bool,

    task: Option<AbortHandle>,
}

impl Drop for PowerRust {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Power {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            match login1::capabilities().await {
                Ok(caps) => {
                    let _ = tx.send(caps).await;
                }
                Err(err) => tracing::warn!("power: cannot query logind: {err}"),
            }
        });
        let task = crate::service::forward(self.qt_thread(), rx, apply);
        self.as_mut().rust_mut().task = Some(task);
    }
}

fn apply(mut obj: Pin<&mut qobject::Power>, caps: Capabilities) {
    set_property!(obj, can_suspend, caps.suspend);
    set_property!(obj, can_reboot, caps.reboot);
    set_property!(obj, can_power_off, caps.power_off);
    set_property!(obj, can_hibernate, caps.hibernate);
    set_property!(obj, can_hybrid_sleep, caps.hybrid_sleep);
}

fn perform(action: Action) {
    tokio::spawn(async move {
        if let Err(err) = login1::perform(action).await {
            tracing::warn!("power: {action:?} failed: {err}");
        }
    });
}

impl qobject::Power {
    fn suspend(&self) {
        perform(Action::Suspend);
    }
    fn reboot(&self) {
        perform(Action::Reboot);
    }
    fn power_off(&self) {
        perform(Action::PowerOff);
    }
    fn hibernate(&self) {
        perform(Action::Hibernate);
    }
    fn hybrid_sleep(&self) {
        perform(Action::HybridSleep);
    }
}
