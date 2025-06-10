//! `Blacksheep` singleton: facts about the running shell and host actions.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++Qt" {
        include!(<QScreen>);
        #[qobject]
        type QScreen;
    }

    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qstringlist.h");
        type QStringList = cxx_qt_lib::QStringList;

        include!("blacksheep-qt/cpp/shell.h");
        #[namespace = "blacksheep"]
        fn screen_named(name: &QString) -> *mut QScreen;
        #[namespace = "blacksheep"]
        fn set_input_region(object: Pin<&mut QObject>, x: i32, y: i32, width: i32, height: i32);
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[namespace = "blacksheep"]
        /// Directory the configuration was loaded from.
        #[qproperty(QString, config_dir, cxx_name = "configDir", READ, CONSTANT)]
        #[qproperty(QString, version, READ, CONSTANT)]
        type Blacksheep = super::BlacksheepRust;

        /// Reload the whole configuration, as a file change would.
        #[qinvokable]
        fn reload(self: &Blacksheep);

        /// Run a program detached from the shell. Returns false if it could
        /// not be started.
        #[qinvokable]
        fn exec(self: &Blacksheep, argv: &QStringList) -> bool;

        /// The screen with the given connector name (`DP-2`), or null. Use it
        /// for `LayerShell.Window.screen` to put a surface on that output.
        #[qinvokable]
        #[cxx_name = "screenNamed"]
        unsafe fn screen_named(self: &Blacksheep, name: &QString) -> *mut QScreen;

        /// Make a window accept input only inside a rectangle; clicks outside
        /// it reach whatever is beneath. Pass zero size to accept everywhere.
        /// Lets a layer surface stay large (so its content can animate
        /// without resizing it) while behaving like a small one.
        #[qinvokable]
        #[cxx_name = "setInputRegion"]
        unsafe fn set_input_region(self: &Blacksheep, window: *mut QObject, x: i32, y: i32, width: i32, height: i32);

        /// Copy text to the clipboard, through wlr-data-control so it works
        /// from any window whether or not it has focus.
        #[qinvokable]
        fn copy(self: &Blacksheep, text: &QString);
    }
}

use cxx_qt_lib::{QString, QStringList};

pub struct BlacksheepRust {
    config_dir: QString,
    version: QString,
}

impl Default for BlacksheepRust {
    fn default() -> Self {
        Self {
            config_dir: crate::shell::config_dir()
                .map(|dir| QString::from(dir.as_str()))
                .unwrap_or_default(),
            version: QString::from(env!("CARGO_PKG_VERSION")),
        }
    }
}

impl qobject::Blacksheep {
    fn reload(&self) {
        crate::shell::request_reload();
    }

    unsafe fn set_input_region(&self, window: *mut qobject::QObject, x: i32, y: i32, width: i32, height: i32) {
        if window.is_null() {
            return;
        }
        // SAFETY: QML passes a live QObject; the callee only casts and calls setMask.
        let window = unsafe { std::pin::Pin::new_unchecked(&mut *window) };
        qobject::set_input_region(window, x, y, width, height);
    }

    fn copy(&self, text: &QString) {
        crate::clipboard::set_text(text.to_string());
    }

    unsafe fn screen_named(&self, name: &QString) -> *mut qobject::QScreen {
        qobject::screen_named(name)
    }

    fn exec(&self, argv: &QStringList) -> bool {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        match blacksheep_core::exec::spawn_detached(&argv, None) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("exec {argv:?}: {err}");
                false
            }
        }
    }
}
