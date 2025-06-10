use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(
        QmlModule::new("Blacksheep")
            .depend("QtQuick")
            .qml_file("qml/PopupWindow.qml")
            .qml_file("qml/InputRegion.qml"),
    )
        .files([
            "src/battery.rs",
            "src/bluetooth.rs",
            "src/blacksheep.rs",
            "src/clock.rs",
            "src/construct.rs",
            "src/desktop_entries.rs",
            "src/executables.rs",
            "src/icons.rs",
            "src/ipc.rs",
            "src/model.rs",
            "src/mpris.rs",
            "src/network.rs",
            "src/niri.rs",
            "src/notifications.rs",
            "src/pipewire.rs",
            "src/power.rs",
            "src/process.rs",
            "src/shell.rs",
            "src/store.rs",
            "src/tray.rs",
            "src/watcher.rs",
        ])
        // QQuickImageProvider lives in Qt Quick.
        .qt_module("Quick")
        .build()
        .export();
}
