//! A `org.freedesktop.Notifications` server.
//!
//! Only one process may own the name. If another daemon holds it, the
//! service reports itself inactive and keeps retrying, so the shell keeps
//! working alongside dunst or mako until the user switches over.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::mpsc;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

const BUS_NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NotificationAction {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<NotificationAction>,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    /// Milliseconds; 0 never expires, -1 means server default.
    pub expire_timeout: i32,
    pub image_path: Option<String>,
    pub desktop_entry: Option<String>,
    /// `x-dunst-stack-tag` or `x-canonical-private-synchronous`: a new
    /// notification with the same app and tag replaces the earlier one.
    pub stack_tag: Option<String>,
    /// How many identical notifications this one stands for. The server
    /// always sends 1; the UI raises it when it stacks duplicates.
    pub count: u32,
}

impl Notification {
    /// Same app and content: the kind of repeat worth stacking.
    pub fn is_duplicate_of(&self, other: &Self) -> bool {
        self.app_name == other.app_name
            && self.summary == other.summary
            && self.body == other.body
            && self.app_icon == other.app_icon
            && self.image_path == other.image_path
    }

    /// Same app and stack tag, when it has one.
    pub fn shares_stack_tag(&self, other: &Self) -> bool {
        self.stack_tag.is_some() && self.app_name == other.app_name && self.stack_tag == other.stack_tag
    }
}

/// Why a notification went away, per the spec's `NotificationClosed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CloseReason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
    Undefined = 4,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Whether this process currently owns the notifications name.
    Active(bool),
    /// A new notification, or a replacement for an existing id.
    Notified(Notification),
    Closed { id: u32, reason: CloseReason },
}

struct Server {
    next_id: u32,
    events: mpsc::Sender<Event>,
}

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        ["actions", "body", "body-markup", "icon-static", "persistence"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "blacksheep".into(),
            "blacksheep".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &mut self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            self.next_id
        };
        let hint_string = |key: &str| {
            hints
                .get(key)
                .and_then(|v| String::try_from(v.clone()).ok())
        };
        let notification = Notification {
            id,
            app_name,
            app_icon,
            summary,
            body,
            actions: actions
                .chunks(2)
                .filter_map(|pair| {
                    Some(NotificationAction {
                        key: pair.first()?.clone(),
                        label: pair.get(1).cloned().unwrap_or_default(),
                    })
                })
                .collect(),
            urgency: hints
                .get("urgency")
                .and_then(|v| u8::try_from(v.clone()).ok())
                .unwrap_or(1),
            expire_timeout,
            image_path: hint_string("image-path").or_else(|| hint_string("image_path")),
            desktop_entry: hint_string("desktop-entry"),
            stack_tag: hint_string("x-dunst-stack-tag")
                .or_else(|| hint_string("x-canonical-private-synchronous")),
            count: 1,
        };
        let _ = self.events.send(Event::Notified(notification)).await;
        id
    }

    async fn close_notification(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        id: u32,
    ) {
        let _ = Self::notification_closed(&emitter, id, CloseReason::Closed as u32).await;
        let _ = self
            .events
            .send(Event::Closed {
                id,
                reason: CloseReason::Closed,
            })
            .await;
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: String,
    ) -> zbus::Result<()>;
}

/// Handle used by the UI to close notifications or trigger their actions.
#[derive(Clone, Debug)]
pub struct Controller {
    tx: mpsc::UnboundedSender<Request>,
}

#[derive(Debug)]
enum Request {
    Close(u32, CloseReason),
    Invoke(u32, String),
}

impl Controller {
    pub fn dismiss(&self, id: u32) {
        let _ = self.tx.send(Request::Close(id, CloseReason::Dismissed));
    }

    pub fn expire(&self, id: u32) {
        let _ = self.tx.send(Request::Close(id, CloseReason::Expired));
    }

    /// Close a notification that a newer one has taken the place of.
    pub fn retire(&self, id: u32) {
        let _ = self.tx.send(Request::Close(id, CloseReason::Undefined));
    }

    pub fn invoke(&self, id: u32, action_key: impl Into<String>) {
        let _ = self.tx.send(Request::Invoke(id, action_key.into()));
    }
}

pub fn start(events: mpsc::Sender<Event>) -> Controller {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(events, rx));
    Controller { tx }
}

async fn run(events: mpsc::Sender<Event>, mut requests: mpsc::UnboundedReceiver<Request>) {
    let mut delay = crate::INITIAL_BACKOFF;
    loop {
        match serve(&events, &mut requests).await {
            Ok(()) => return,
            Err(zbus::Error::NameTaken) => {
                tracing::info!("notifications: another daemon owns {BUS_NAME}; retrying later");
                delay = Duration::from_secs(30);
            }
            Err(err) => tracing::warn!("notifications: {err}"),
        }
        if events.send(Event::Active(false)).await.is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
        delay = crate::backoff(delay);
    }
}

async fn serve(
    events: &mpsc::Sender<Event>,
    requests: &mut mpsc::UnboundedReceiver<Request>,
) -> zbus::Result<()> {
    let connection = zbus::connection::Builder::session()?
        .allow_name_replacements(false)
        .replace_existing_names(false)
        .name(BUS_NAME)?
        .serve_at(
            PATH,
            Server {
                next_id: 0,
                events: events.clone(),
            },
        )?
        .build()
        .await?;
    if events.send(Event::Active(true)).await.is_err() {
        return Ok(());
    }

    let iface = connection
        .object_server()
        .interface::<_, Server>(PATH)
        .await?;

    while let Some(request) = requests.recv().await {
        let emitter = iface.signal_emitter();
        match request {
            Request::Close(id, reason) => {
                Server::notification_closed(emitter, id, reason as u32).await?;
                let _ = events.send(Event::Closed { id, reason }).await;
            }
            Request::Invoke(id, key) => {
                Server::action_invoked(emitter, id, key).await?;
                Server::notification_closed(emitter, id, CloseReason::Dismissed as u32).await?;
                let _ = events
                    .send(Event::Closed {
                        id,
                        reason: CloseReason::Dismissed,
                    })
                    .await;
            }
        }
    }
    Ok(())
}
