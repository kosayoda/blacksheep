//! `PathExecutables` singleton: every program on `PATH`, kept current.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("blacksheep-qt/src/model.cxxqt.h");
        #[namespace = "blacksheep"]
        type ListModel = crate::model::qobject::ListModel;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[namespace = "blacksheep"]
        /// Rows of `name` and `path`, one per program, sorted by name.
        #[qproperty(*mut ListModel, entries, READ, CONSTANT)]
        type PathExecutables = super::PathExecutablesRust;

        /// Rescan `PATH` now.
        #[qinvokable]
        fn refresh(self: Pin<&mut PathExecutables>);
    }

    impl cxx_qt::Initialize for PathExecutables {}
    impl cxx_qt::Threading for PathExecutables {}
}

use std::pin::Pin;

use blacksheep_desktop::Executable;
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use notify_debouncer_full::{DebounceEventResult, Debouncer, NoCache, new_debouncer};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;

pub struct PathExecutablesRust {
    entries: *mut ListModel,
    _model: UniquePtr<ListModel>,
    rescan: Option<mpsc::UnboundedSender<()>>,
    watcher: Option<Debouncer<notify::RecommendedWatcher, NoCache>>,
    task: Option<AbortHandle>,
}

impl Default for PathExecutablesRust {
    fn default() -> Self {
        let mut model = new_list_model();
        model.pin_mut().configure(&["name", "path"], Some("name"));
        Self {
            entries: model.as_mut_ptr(),
            _model: model,
            rescan: None,
            watcher: None,
            task: None,
        }
    }
}

impl Drop for PathExecutablesRust {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::PathExecutables {
    fn initialize(mut self: Pin<&mut Self>) {
        let (results_tx, results_rx) = mpsc::channel::<Vec<Executable>>(4);
        let (rescan_tx, mut rescan_rx) = mpsc::unbounded_channel::<()>();

        let _ = rescan_tx.send(());
        tokio::spawn(async move {
            while rescan_rx.recv().await.is_some() {
                while rescan_rx.try_recv().is_ok() {}
                match tokio::task::spawn_blocking(blacksheep_desktop::scan_path).await {
                    Ok(found) => {
                        if results_tx.send(found).await.is_err() {
                            return;
                        }
                    }
                    Err(err) => tracing::warn!("path executables: scan failed: {err}"),
                }
            }
        });

        // Package installs add or remove binaries; bin directories are flat,
        // so a non-recursive watch is enough.
        let watcher = {
            let rescan = rescan_tx.clone();
            new_debouncer(
                std::time::Duration::from_secs(1),
                None,
                move |result: DebounceEventResult| {
                    if let Ok(events) = result
                        && events
                            .iter()
                            .any(|event| crate::service::is_content_change(&event.kind))
                    {
                        let _ = rescan.send(());
                    }
                },
            )
        };
        let watcher = match watcher {
            Ok(mut watcher) => {
                for dir in blacksheep_desktop::path_directories() {
                    if dir.is_dir()
                        && let Err(err) =
                            watcher.watch(dir.as_std_path(), notify::RecursiveMode::NonRecursive)
                    {
                        tracing::debug!("path executables: cannot watch {dir}: {err}");
                    }
                }
                Some(watcher)
            }
            Err(err) => {
                tracing::warn!("path executables: no file watcher: {err}");
                None
            }
        };

        let task = crate::service::forward(self.qt_thread(), results_rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.rescan = Some(rescan_tx);
        rust.watcher = watcher;
        rust.task = Some(task);
    }
}

fn apply(obj: Pin<&mut qobject::PathExecutables>, found: Vec<Executable>) {
    let model = obj.entries;
    // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
    unsafe { Pin::new_unchecked(&mut *model) }.sync(&found);
}

impl qobject::PathExecutables {
    fn refresh(self: Pin<&mut Self>) {
        if let Some(rescan) = &self.rescan {
            let _ = rescan.send(());
        }
    }
}
