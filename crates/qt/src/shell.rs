//! The shell host: owns the QML engine, loads the user's configuration and
//! reloads it when files under the configuration directory change.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qqmlapplicationengine.h");
        type QQmlApplicationEngine = cxx_qt_lib::QQmlApplicationEngine;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;

        include!("blacksheep-qt/cpp/shell.h");
        #[namespace = "blacksheep"]
        fn engine_reload(engine: Pin<&mut QQmlApplicationEngine>, url: &QUrl);
    }

    extern "RustQt" {
        #[qobject]
        #[namespace = "blacksheep"]
        type Shell = super::ShellRust;

        /// Load (or reload) the configuration into the engine.
        fn reload(self: Pin<&mut Shell>);
    }

    impl cxx_qt::Threading for Shell {}
}

use std::pin::Pin;
use std::sync::OnceLock;

use camino::{Utf8Path, Utf8PathBuf};
use cxx::UniquePtr;
use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::{QQmlApplicationEngine, QString, QUrl};
use notify_debouncer_full::{DebounceEventResult, Debouncer, NoCache, new_debouncer};

/// Handle to the running shell, for reload requests from anywhere.
static SHELL: OnceLock<CxxQtThread<qobject::Shell>> = OnceLock::new();
/// Directory the configuration was loaded from.
static CONFIG_DIR: OnceLock<Utf8PathBuf> = OnceLock::new();

pub fn config_dir() -> Option<&'static Utf8Path> {
    CONFIG_DIR.get().map(Utf8PathBuf::as_path)
}

/// Ask the shell to reload its configuration. Safe from any thread.
pub fn request_reload() {
    if let Some(shell) = SHELL.get() {
        let _ = shell.queue(|shell| shell.reload());
    }
}

pub struct ShellRust {
    engine: UniquePtr<QQmlApplicationEngine>,
    main: QUrl,
    watcher: Option<Debouncer<notify::RecommendedWatcher, NoCache>>,
}

impl Default for ShellRust {
    fn default() -> Self {
        Self {
            engine: UniquePtr::null(),
            main: QUrl::default(),
            watcher: None,
        }
    }
}

/// Resolve the path given on the command line to (directory, main file).
pub fn resolve(config: &Utf8Path) -> (Utf8PathBuf, Utf8PathBuf) {
    if config.is_file() {
        let dir = config.parent().map(Utf8Path::to_path_buf).unwrap_or_default();
        (dir, config.to_path_buf())
    } else {
        (config.to_path_buf(), config.join("Main.qml"))
    }
}

/// Create the engine, load the configuration and start watching it.
/// The returned object must outlive the application's event loop.
pub fn start(config: &Utf8Path, watch: bool) -> UniquePtr<qobject::Shell> {
    let (dir, main) = resolve(config);
    let _ = CONFIG_DIR.set(dir.clone());

    // Before the engine loads anything: singletons read the store as they
    // start, and the launcher's frecency is empty for the whole session if
    // the store arrives later.
    if let Err(err) = blacksheep_store::init() {
        tracing::warn!("store: unavailable, nothing will persist: {err}");
    }

    let mut shell = crate::construct::new_shell();
    let _ = SHELL.set(shell.pin_mut().qt_thread());

    {
        let mut rust = shell.pin_mut().rust_mut();
        rust.engine = QQmlApplicationEngine::new();
        rust.main = QUrl::from_local_file(&QString::from(main.as_str()));
        if let Some(mut engine) = rust.engine.as_mut() {
            // Lets configs organise QML modules in subdirectories with a qmldir.
            engine.as_mut().add_import_path(&QString::from(dir.as_str()));
            crate::icons::install_icon_providers(
                engine,
                &QString::from(crate::icons::desktop_icon_theme().as_str()),
            );
        }
    }

    let main_url = shell.main.clone();
    if let Some(engine) = shell.pin_mut().rust_mut().engine.as_mut() {
        engine.load(&main_url);
    }

    crate::ipc::start_server();

    if watch {
        match start_watcher(&dir) {
            Ok(watcher) => shell.pin_mut().rust_mut().watcher = Some(watcher),
            Err(err) => tracing::warn!("shell: cannot watch {dir}: {err}"),
        }
    }
    shell
}

fn start_watcher(dir: &Utf8Path) -> notify::Result<Debouncer<notify::RecommendedWatcher, NoCache>> {
    let mut debouncer = new_debouncer(
        std::time::Duration::from_millis(300),
        None,
        move |result: DebounceEventResult| match result {
            Ok(events) => {
                // Loading QML reads these files, which is an access event;
                // reacting to reads would reload forever.
                let relevant = events.iter().any(|event| {
                    crate::service::is_content_change(&event.kind)
                        && event.paths.iter().any(|path| {
                        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        // Editors write temp files; only QML and JS matter.
                        (name.ends_with(".qml") || name.ends_with(".js") || name == "qmldir")
                            && !name.starts_with('.')
                    })
                });
                if relevant {
                    tracing::info!("shell: configuration changed, reloading");
                    request_reload();
                }
            }
            Err(errors) => {
                for err in errors {
                    tracing::warn!("shell: watch error: {err}");
                }
            }
        },
    )?;
    debouncer.watch(dir.as_std_path(), notify::RecursiveMode::Recursive)?;
    Ok(debouncer)
}

impl qobject::Shell {
    fn reload(mut self: Pin<&mut Self>) {
        let main = self.main.clone();
        if let Some(engine) = self.as_mut().rust_mut().engine.as_mut() {
            qobject::engine_reload(engine, &main);
        }
    }
}
