//! cxx-qt bridges exposing blacksheep services to QML under the `Blacksheep` import.
//!
//! Generic building blocks live in [`model`], [`variant`] and [`service`];
//! each service is a small typed QObject on top of them.

mod battery;
mod bluetooth;
mod blacksheep;
mod clipboard;
mod clock;
mod construct;
mod desktop_entries;
mod executables;
mod icons;
mod ipc;
mod model;
mod mpris;
mod network;
mod niri;
mod notifications;
mod pipewire;
mod power;
mod process;
mod service;
mod tray;
pub mod shell;
mod store;
mod variant;
mod watcher;
