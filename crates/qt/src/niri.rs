//! `Niri` singleton: workspaces and windows as list models, plus actions.

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
        #[qproperty(*mut ListModel, workspaces, READ, CONSTANT)]
        #[qproperty(*mut ListModel, windows, READ, CONSTANT)]
        #[qproperty(*mut ListModel, active_windows, cxx_name = "activeWindows", READ, CONSTANT)]
        #[qproperty(
            u64,
            focused_workspace_id,
            cxx_name = "focusedWorkspaceId",
            READ,
            NOTIFY
        )]
        #[qproperty(u64, focused_window_id, cxx_name = "focusedWindowId", READ, NOTIFY)]
        #[qproperty(bool, connected, READ, NOTIFY)]
        type Niri = super::NiriRust;

        #[qinvokable]
        #[cxx_name = "focusWorkspace"]
        fn focus_workspace(self: Pin<&mut Niri>, id: u64);

        #[qinvokable]
        #[cxx_name = "focusWindow"]
        fn focus_window(self: Pin<&mut Niri>, id: u64);

        /// Exit niri. With `skip_confirmation` false, niri asks first.
        #[qinvokable]
        fn quit(self: Pin<&mut Niri>, skip_confirmation: bool);
    }

    impl cxx_qt::Initialize for Niri {}
    impl cxx_qt::Threading for Niri {}
}

use std::pin::Pin;

use blacksheep_niri::{Action, Event, EventStream, Requester, State};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;

const WORKSPACE_ROLES: &[&str] = &[
    "id",
    "idx",
    "name",
    "output",
    "is_active",
    "is_focused",
    "is_urgent",
    "active_window_id",
];

const WINDOW_ROLES: &[&str] = &[
    "id",
    "title",
    "app_id",
    "pid",
    "workspace_id",
    "is_focused",
    "is_floating",
    "is_urgent",
    "layout",
];

enum Update {
    Connected(bool),
    Event(Event),
}

pub struct NiriRust {
    workspaces: *mut ListModel,
    windows: *mut ListModel,
    active_windows: *mut ListModel,
    focused_workspace_id: u64,
    focused_window_id: u64,
    connected: bool,

    /// Owners of the three pointers above; dropped with this object.
    _models: [UniquePtr<ListModel>; 3],
    state: State,
    requester: Requester,
    tasks: Vec<AbortHandle>,
}

impl Default for NiriRust {
    fn default() -> Self {
        let mut workspaces = new_list_model();
        workspaces.pin_mut().configure(WORKSPACE_ROLES, Some("id"));
        let mut windows = new_list_model();
        windows.pin_mut().configure(WINDOW_ROLES, Some("id"));
        let mut active_windows = new_list_model();
        active_windows.pin_mut().configure(WINDOW_ROLES, Some("id"));

        Self {
            workspaces: workspaces.as_mut_ptr(),
            windows: windows.as_mut_ptr(),
            active_windows: active_windows.as_mut_ptr(),
            focused_workspace_id: 0,
            focused_window_id: 0,
            connected: false,
            _models: [workspaces, windows, active_windows],
            state: State::default(),
            requester: Requester::spawn(),
            tasks: Vec::new(),
        }
    }
}

impl Drop for NiriRust {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Niri {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = mpsc::channel(256);
        let producer = tokio::spawn(subscribe(tx)).abort_handle();
        let consumer = crate::service::forward(self.qt_thread(), rx, apply);
        self.as_mut().rust_mut().tasks = vec![producer, consumer];
    }
}

/// Keep an event-stream subscription alive, reconnecting with backoff.
async fn subscribe(tx: mpsc::Sender<Update>) {
    let mut delay = blacksheep_niri::INITIAL_BACKOFF;
    loop {
        match EventStream::connect().await {
            Ok(mut stream) => {
                delay = blacksheep_niri::INITIAL_BACKOFF;
                if tx.send(Update::Connected(true)).await.is_err() {
                    return;
                }
                loop {
                    match stream.next().await {
                        Ok(event) => {
                            if tx.send(Update::Event(event)).await.is_err() {
                                return;
                            }
                        }
                        Err(err) => {
                            tracing::warn!("niri event stream ended: {err}");
                            break;
                        }
                    }
                }
                if tx.send(Update::Connected(false)).await.is_err() {
                    return;
                }
            }
            Err(err) => tracing::warn!("niri: cannot subscribe to events: {err}"),
        }
        tokio::time::sleep(delay).await;
        delay = blacksheep_niri::backoff(delay);
    }
}

/// Runs on the Qt thread for every update.
fn apply(mut obj: Pin<&mut qobject::Niri>, update: Update) {
    match update {
        Update::Connected(connected) => {
            if obj.connected != connected {
                obj.as_mut().rust_mut().connected = connected;
                obj.as_mut().connected_changed();
            }
        }
        Update::Event(event) => {
            obj.as_mut().rust_mut().state.apply(event);
            obj.refresh();
        }
    }
}

impl qobject::Niri {
    /// Push the reduced state into the models and properties.
    fn refresh(mut self: Pin<&mut Self>) {
        let focused_workspace = self.state.focused_workspace().map(|ws| ws.id);
        let focused_window = self.state.focused_window().map(|win| win.id);

        // SAFETY: the pointers come from UniquePtrs owned by this object, which
        // outlives this call, and we are on the Qt thread that owns them.
        let (workspaces, windows, active_windows) =
            (self.workspaces, self.windows, self.active_windows);
        unsafe {
            let state = &self.state;
            Pin::new_unchecked(&mut *workspaces).sync(&state.workspaces);
            Pin::new_unchecked(&mut *windows).sync(&state.windows);
            let active = focused_workspace
                .map(|id| state.windows_on(id).collect::<Vec<_>>())
                .unwrap_or_default();
            Pin::new_unchecked(&mut *active_windows).sync(active);
        }

        let focused_workspace = focused_workspace.unwrap_or_default();
        if self.focused_workspace_id != focused_workspace {
            self.as_mut().rust_mut().focused_workspace_id = focused_workspace;
            self.as_mut().focused_workspace_id_changed();
        }
        let focused_window = focused_window.unwrap_or_default();
        if self.focused_window_id != focused_window {
            self.as_mut().rust_mut().focused_window_id = focused_window;
            self.as_mut().focused_window_id_changed();
        }
    }

    fn focus_workspace(self: Pin<&mut Self>, id: u64) {
        let _ = self.requester.action(Action::FocusWorkspace {
            reference: niri_ipc::WorkspaceReferenceArg::Id(id),
        });
    }

    fn focus_window(self: Pin<&mut Self>, id: u64) {
        let _ = self.requester.action(Action::FocusWindow { id });
    }

    fn quit(self: Pin<&mut Self>, skip_confirmation: bool) {
        let _ = self.requester.action(Action::Quit { skip_confirmation });
    }
}
