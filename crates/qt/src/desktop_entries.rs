//! `DesktopEntries` singleton: installed applications, kept current.

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
        /// Every application entry, sorted by name; `noDisplay` marks the hidden
        /// ones. `frecency`, `launches` and `lastLaunched` come from the
        /// launch history recorded by `launch()`.
        #[qproperty(*mut ListModel, applications, READ, CONSTANT)]
        type DesktopEntries = super::DesktopEntriesRust;

        /// Start the application with the given desktop id and record the
        /// launch for frecency ranking.
        #[qinvokable]
        fn launch(self: Pin<&mut DesktopEntries>, id: &QString) -> bool;

        /// Rescan the desktop directories now.
        #[qinvokable]
        fn refresh(self: Pin<&mut DesktopEntries>);
    }

    impl cxx_qt::Initialize for DesktopEntries {}
    impl cxx_qt::Threading for DesktopEntries {}
}

use std::pin::Pin;

use std::collections::HashMap;

use blacksheep_desktop::Entry;
use blacksheep_store::launches::Score;
use cxx::UniquePtr;
use serde::Serialize;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use notify_debouncer_full::{DebounceEventResult, Debouncer, NoCache, new_debouncer};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;

const ROLES: &[&str] = &[
    "id",
    "name",
    "generic_name",
    "comment",
    "keywords",
    "icon",
    "exec",
    "terminal",
    "categories",
    "no_display",
    "path",
    "frecency",
    "launches",
    "last_launched",
];

/// An entry plus its launch history, as the model sees it.
#[derive(Serialize)]
struct Row<'a> {
    #[serde(flatten)]
    entry: &'a Entry,
    frecency: f64,
    launches: u32,
    last_launched: i64,
}

enum Update {
    Entries(Vec<Entry>),
    Scores(HashMap<String, Score>),
}

pub struct DesktopEntriesRust {
    applications: *mut ListModel,
    _model: UniquePtr<ListModel>,
    entries: Vec<Entry>,
    scores: HashMap<String, Score>,
    rescan: Option<mpsc::UnboundedSender<()>>,
    watcher: Option<Debouncer<notify::RecommendedWatcher, NoCache>>,
    task: Option<AbortHandle>,
}

impl Default for DesktopEntriesRust {
    fn default() -> Self {
        let mut model = new_list_model();
        model.pin_mut().configure(ROLES, Some("id"));
        Self {
            applications: model.as_mut_ptr(),
            _model: model,
            entries: Vec::new(),
            scores: HashMap::new(),
            rescan: None,
            watcher: None,
            task: None,
        }
    }
}

impl Drop for DesktopEntriesRust {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::DesktopEntries {
    fn initialize(mut self: Pin<&mut Self>) {
        let (results_tx, results_rx) = mpsc::channel::<Update>(4);
        let (rescan_tx, mut rescan_rx) = mpsc::unbounded_channel::<()>();

        // Launch history first, so the first scan already ranks.
        if let Some(store) = blacksheep_store::global() {
            let store = store.clone();
            let tx = results_tx.clone();
            tokio::spawn(async move {
                match store.run(blacksheep_store::launches::scores).await {
                    Ok(scores) => {
                        let _ = tx.send(Update::Scores(scores)).await;
                    }
                    Err(err) => tracing::warn!("desktop entries: cannot read launch history: {err}"),
                }
            });
        }

        // Scan once now, then whenever asked; scanning runs on a blocking thread.
        let _ = rescan_tx.send(());
        tokio::spawn(async move {
            while rescan_rx.recv().await.is_some() {
                // Collapse bursts of requests into one scan.
                while rescan_rx.try_recv().is_ok() {}
                match tokio::task::spawn_blocking(blacksheep_desktop::scan).await {
                    Ok(entries) => {
                        if results_tx.send(Update::Entries(entries)).await.is_err() {
                            return;
                        }
                    }
                    Err(err) => tracing::warn!("desktop entries: scan failed: {err}"),
                }
            }
        });

        let watcher = {
            let rescan = rescan_tx.clone();
            new_debouncer(
                std::time::Duration::from_millis(500),
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
                for dir in blacksheep_desktop::directories() {
                    if dir.is_dir()
                        && let Err(err) =
                            watcher.watch(dir.as_std_path(), notify::RecursiveMode::Recursive)
                    {
                        tracing::debug!("desktop entries: cannot watch {dir}: {err}");
                    }
                }
                Some(watcher)
            }
            Err(err) => {
                tracing::warn!("desktop entries: no file watcher: {err}");
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

fn apply(mut obj: Pin<&mut qobject::DesktopEntries>, update: Update) {
    match update {
        Update::Entries(entries) => obj.as_mut().rust_mut().entries = entries,
        Update::Scores(scores) => obj.as_mut().rust_mut().scores = scores,
    }
    obj.refresh_model();
}

impl qobject::DesktopEntries {
    fn refresh_model(self: Pin<&mut Self>) {
        let rows: Vec<Row> = self
            .entries
            .iter()
            .map(|entry| {
                let score = self.scores.get(&entry.id).copied().unwrap_or_default();
                Row {
                    entry,
                    frecency: score.frecency,
                    launches: score.launches,
                    last_launched: score.last,
                }
            })
            .collect();
        let model = self.applications;
        // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
        unsafe { Pin::new_unchecked(&mut *model) }.sync(&rows);
    }

    fn launch(mut self: Pin<&mut Self>, id: &QString) -> bool {
        let id = id.to_string();
        let Some(entry) = self.entries.iter().find(|entry| entry.id == id) else {
            tracing::warn!("desktop entries: no application {id:?}");
            return false;
        };
        let argv = blacksheep_desktop::launch_argv(entry);
        if let Err(err) = blacksheep_core::exec::spawn_detached(&argv, None) {
            tracing::warn!("desktop entries: cannot launch {id}: {err}");
            return false;
        }

        // Count it now so the ranking moves immediately, and persist it.
        {
            let mut rust = self.as_mut().rust_mut();
            let score = rust.scores.entry(id.clone()).or_default();
            score.frecency += 1.0;
            score.launches += 1;
            score.last = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
        }
        self.refresh_model();
        if let Some(store) = blacksheep_store::global() {
            let store = store.clone();
            tokio::spawn(async move {
                if let Err(err) = store
                    .run(move |c| blacksheep_store::launches::record(c, &id))
                    .await
                {
                    tracing::warn!("desktop entries: cannot record launch: {err}");
                }
            });
        }
        true
    }

    fn refresh(self: Pin<&mut Self>) {
        if let Some(rescan) = &self.rescan {
            let _ = rescan.send(());
        }
    }
}
