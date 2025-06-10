//! Async client for the niri compositor's IPC socket.
//!
//! Two halves: [`EventStream`] subscribes to compositor events, and
//! [`Requester`] sends actions over a persistent connection that is
//! re-established on failure.

use std::time::Duration;

use camino::Utf8PathBuf;

pub use niri_ipc::{Action, Event, Reply, Request, Response};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("NIRI_SOCKET is not set; is niri running?")]
    NoSocket,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("niri rejected the event stream request: {0:?}")]
    Rejected(Reply),
    #[error("niri closed the connection")]
    Closed,
}

pub fn socket_path() -> Result<Utf8PathBuf, Error> {
    std::env::var("NIRI_SOCKET")
        .map(Utf8PathBuf::from)
        .map_err(|_| Error::NoSocket)
}

async fn connect() -> Result<UnixStream, Error> {
    Ok(UnixStream::connect(socket_path()?).await?)
}

async fn write_request(stream: &mut UnixStream, request: &Request) -> Result<(), Error> {
    let mut buf = serde_json::to_string(request)?;
    buf.push('\n');
    stream.write_all(buf.as_bytes()).await?;
    Ok(())
}

async fn read_reply(stream: &mut UnixStream) -> Result<Reply, Error> {
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).await? == 0 {
        return Err(Error::Closed);
    }
    Ok(serde_json::from_str(&line)?)
}

/// A live subscription to niri's event stream.
pub struct EventStream {
    reader: BufReader<UnixStream>,
    line: String,
}

impl EventStream {
    /// Open a new connection and switch it into event-stream mode.
    pub async fn connect() -> Result<Self, Error> {
        let mut stream = connect().await?;
        write_request(&mut stream, &Request::EventStream).await?;
        stream.shutdown().await?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Err(Error::Closed);
        }
        match serde_json::from_str::<Reply>(&line)? {
            Ok(Response::Handled) => Ok(Self { reader, line }),
            other => Err(Error::Rejected(other)),
        }
    }

    /// Wait for the next event. Returns [`Error::Closed`] once niri hangs up.
    pub async fn next(&mut self) -> Result<Event, Error> {
        self.line.clear();
        if self.reader.read_line(&mut self.line).await? == 0 {
            return Err(Error::Closed);
        }
        Ok(serde_json::from_str(&self.line)?)
    }
}

/// Cheap-to-clone handle for sending requests to niri.
///
/// Requests are serialised through a background task that owns one
/// connection and reconnects when it breaks.
#[derive(Clone, Debug)]
pub struct Requester {
    tx: mpsc::UnboundedSender<Request>,
}

impl Requester {
    /// Spawn the background task on the current tokio runtime.
    pub fn spawn() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Request>();
        tokio::spawn(async move {
            let mut stream: Option<UnixStream> = None;
            while let Some(request) = rx.recv().await {
                // One retry on a fresh connection covers a stale socket.
                for attempt in 0..2 {
                    if stream.is_none() {
                        match connect().await {
                            Ok(s) => stream = Some(s),
                            Err(err) => {
                                tracing::warn!("niri: cannot connect: {err}");
                                break;
                            }
                        }
                    }
                    let Some(s) = stream.as_mut() else { break };
                    match send(s, &request).await {
                        Ok(Ok(_)) => break,
                        Ok(Err(message)) => {
                            tracing::warn!("niri rejected {request:?}: {message}");
                            break;
                        }
                        Err(err) => {
                            stream = None;
                            if attempt == 1 {
                                tracing::warn!("niri: request failed: {err}");
                            }
                        }
                    }
                }
            }
        });
        Self { tx }
    }

    /// Queue a request. Fails only if the background task has exited.
    pub fn send(&self, request: Request) -> Result<(), Error> {
        self.tx.send(request).map_err(|_| Error::Closed)
    }

    pub fn action(&self, action: Action) -> Result<(), Error> {
        self.send(Request::Action(action))
    }
}

async fn send(stream: &mut UnixStream, request: &Request) -> Result<Reply, Error> {
    write_request(stream, request).await?;
    read_reply(stream).await
}

/// Backoff schedule shared by reconnecting loops.
pub fn backoff(previous: Duration) -> Duration {
    (previous * 2).min(Duration::from_secs(10))
}

pub const INITIAL_BACKOFF: Duration = Duration::from_millis(250);

/// Everything blacksheep tracks about the compositor, reduced from events.
///
/// Both lists are kept sorted so callers can hand them to a view as-is:
/// workspaces by output then index, windows by workspace then layout position.
#[derive(Debug, Default, Clone)]
pub struct State {
    pub workspaces: Vec<niri_ipc::Workspace>,
    pub windows: Vec<niri_ipc::Window>,
}

impl State {
    pub fn apply(&mut self, event: Event) {
        match event {
            Event::WorkspacesChanged { workspaces } => {
                self.workspaces = workspaces;
                self.sort_workspaces();
            }
            Event::WorkspaceActivated { id, focused } => {
                let output = self
                    .workspaces
                    .iter()
                    .find(|ws| ws.id == id)
                    .and_then(|ws| ws.output.clone());
                for ws in &mut self.workspaces {
                    if ws.output == output {
                        ws.is_active = ws.id == id;
                    }
                    if focused {
                        ws.is_focused = ws.id == id;
                    }
                }
            }
            Event::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            } => {
                if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == workspace_id) {
                    ws.active_window_id = active_window_id;
                }
            }
            Event::WorkspaceUrgencyChanged { id, urgent } => {
                if let Some(ws) = self.workspaces.iter_mut().find(|ws| ws.id == id) {
                    ws.is_urgent = urgent;
                }
            }
            Event::WindowsChanged { windows } => {
                self.windows = windows;
                self.sort_windows();
            }
            Event::WindowOpenedOrChanged { window } => {
                if window.is_focused {
                    for win in &mut self.windows {
                        win.is_focused = false;
                    }
                }
                match self.windows.iter_mut().find(|win| win.id == window.id) {
                    Some(existing) => *existing = window,
                    None => self.windows.push(window),
                }
                self.sort_windows();
            }
            Event::WindowClosed { id } => self.windows.retain(|win| win.id != id),
            Event::WindowFocusChanged { id } => {
                for win in &mut self.windows {
                    win.is_focused = Some(win.id) == id;
                }
            }
            Event::WindowLayoutsChanged { changes } => {
                for (id, layout) in changes {
                    if let Some(win) = self.windows.iter_mut().find(|win| win.id == id) {
                        win.layout = layout;
                    }
                }
                self.sort_windows();
            }
            Event::WindowUrgencyChanged { id, urgent } => {
                if let Some(win) = self.windows.iter_mut().find(|win| win.id == id) {
                    win.is_urgent = urgent;
                }
            }
            _ => {}
        }
    }

    pub fn focused_workspace(&self) -> Option<&niri_ipc::Workspace> {
        self.workspaces.iter().find(|ws| ws.is_focused)
    }

    pub fn focused_window(&self) -> Option<&niri_ipc::Window> {
        self.windows.iter().find(|win| win.is_focused)
    }

    /// Windows on the given workspace, in layout order.
    pub fn windows_on(&self, workspace_id: u64) -> impl Iterator<Item = &niri_ipc::Window> {
        self.windows
            .iter()
            .filter(move |win| win.workspace_id == Some(workspace_id))
    }

    fn sort_workspaces(&mut self) {
        self.workspaces
            .sort_by(|a, b| a.output.cmp(&b.output).then(a.idx.cmp(&b.idx)));
    }

    fn sort_windows(&mut self) {
        self.windows.sort_by(|a, b| {
            // Floating windows have no layout position; keep them after tiles.
            let a_pos = a
                .layout
                .pos_in_scrolling_layout
                .unwrap_or((usize::MAX, usize::MAX));
            let b_pos = b
                .layout
                .pos_in_scrolling_layout
                .unwrap_or((usize::MAX, usize::MAX));
            a.workspace_id
                .cmp(&b.workspace_id)
                .then(a_pos.cmp(&b_pos))
                .then(a.id.cmp(&b.id))
        });
    }
}
