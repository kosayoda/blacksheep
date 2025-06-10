#![allow(unused_attributes)]

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[namespace = "blacksheep"]
        type Watcher = super::WatcherRust;

        #[qsignal]
        fn modified(self: Pin<&mut Watcher>);

        #[qinvokable]
        fn watch(self: Pin<&mut Watcher>, path: QUrl);
    }

    impl cxx_qt::Initialize for Watcher {}
    impl cxx_qt::Threading for Watcher {}
}

use camino::Utf8PathBuf;
use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt::Threading;
use cxx_qt_lib::QUrl;

#[derive(Default)]
pub struct WatcherRust {
    debouncer: Option<
        notify_debouncer_full::Debouncer<notify::INotifyWatcher, notify_debouncer_full::NoCache>,
    >,
}

impl cxx_qt::Initialize for qobject::Watcher {
    fn initialize(self: core::pin::Pin<&mut Self>) {}
}

impl qobject::Watcher {
    fn watch(mut self: Pin<&mut Self>, path: QUrl) {
        if self.as_ref().debouncer.is_none() {
            let (debouncer, rx) = match notifier() {
                Ok(notifier) => notifier,
                Err(err) => {
                    tracing::error!("watcher: cannot start file watcher: {err}");
                    return;
                }
            };
            self.as_mut().rust_mut().debouncer = Some(debouncer);

            let qt_thread = self.qt_thread();
            std::thread::spawn(move || {
                while let Ok(_events) = rx.recv() {
                    // tracing::debug!("Received: {events:?}");
                    let _ = qt_thread.queue(|obj| {
                        obj.modified();
                    });
                }
            });
        }

        let mut path = Utf8PathBuf::from(path.to_local_file_or_default().to_string());
        if path.is_file() {
            if let Some(parent) = path.parent() {
                path = parent.to_path_buf();
            } else {
                return;
            }
        }
        let mut rust = self.as_mut().rust_mut();
        let Some(debouncer) = rust.debouncer.as_mut() else {
            return;
        };
        if let Err(e) = debouncer.watch(path.as_std_path(), notify::RecursiveMode::NonRecursive) {
            tracing::error!("watcher: {e}");
        }
    }
}

pub fn notifier() -> notify::Result<(
    notify_debouncer_full::Debouncer<notify::INotifyWatcher, notify_debouncer_full::NoCache>,
    std::sync::mpsc::Receiver<Vec<std::path::PathBuf>>,
)> {
    use notify::EventKind;

    let (tx, rx) = std::sync::mpsc::sync_channel(64);

    let watcher = notify_debouncer_full::new_debouncer(
        std::time::Duration::from_millis(200),
        None,
        move |res: std::result::Result<
            Vec<notify_debouncer_full::DebouncedEvent>,
            Vec<notify::Error>,
        >| {
            let Ok(events) = res else {
                return;
            };

            for mut event in events {
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    continue;
                }

                let _ = tx.send(std::mem::take(&mut event.paths));
                return;
            }
        },
    )?;

    Ok((watcher, rx))
}
