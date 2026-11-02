//! `Notifications` singleton: the shell as notification daemon, with a
//! persistent history and do-not-disturb.

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
        /// Notifications currently showing: the ones popups should draw.
        #[qproperty(*mut ListModel, notifications, READ, CONSTANT)]
        /// Everything received, newest first, persisted across restarts.
        #[qproperty(*mut ListModel, history, READ, CONSTANT)]
        /// True while this process owns org.freedesktop.Notifications.
        #[qproperty(bool, active, READ, NOTIFY)]
        /// Default popup lifetime in ms for notifications that don't set one. 0 keeps them.
        #[qproperty(i32, default_timeout, cxx_name = "defaultTimeout")]
        /// While set, new notifications go straight to history without a popup.
        #[qproperty(bool, do_not_disturb, cxx_name = "doNotDisturb", READ, WRITE = set_do_not_disturb, NOTIFY)]
        /// History entries not yet seen in the centre.
        #[qproperty(i32, unread_count, cxx_name = "unreadCount", READ, NOTIFY)]
        /// Fold repeats into the card already showing: identical content
        /// raises its `count`, and a notification carrying a stack tag
        /// replaces the previous one from the same app with that tag.
        #[qproperty(bool, stack_duplicates, cxx_name = "stackDuplicates")]
        type Notifications = super::NotificationsRust;

        fn set_do_not_disturb(self: Pin<&mut Notifications>, on: bool);

        /// Close a notification as dismissed by the user.
        #[qinvokable]
        fn dismiss(self: &Notifications, id: u32);
        /// Trigger one of a notification's actions; the notification closes.
        #[qinvokable]
        fn invoke(self: &Notifications, id: u32, action_key: &QString);
        /// Clear the unread badge, e.g. when the centre opens.
        #[qinvokable]
        #[cxx_name = "markAllRead"]
        fn mark_all_read(self: Pin<&mut Notifications>);
        /// Delete one history entry by its history id.
        #[qinvokable]
        #[cxx_name = "removeFromHistory"]
        fn remove_from_history(self: Pin<&mut Notifications>, id: i64);
        #[qinvokable]
        #[cxx_name = "clearHistory"]
        fn clear_history(self: Pin<&mut Notifications>);
    }

    impl cxx_qt::Initialize for Notifications {}
    impl cxx_qt::Threading for Notifications {}
}

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use blacksheep_dbus::notifications::{self, CloseReason, Controller, Event, Notification};
use blacksheep_store::notifications::{self as history, NewRecord, Record};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;
use crate::service::set_property;

const ROLES: &[&str] = &[
    "id",
    "app_name",
    "app_icon",
    "summary",
    "body",
    "actions",
    "urgency",
    "expire_timeout",
    "image_path",
    "desktop_entry",
    "count",
];

const HISTORY_ROLES: &[&str] = &[
    "id",
    "notified_at",
    "app_name",
    "app_icon",
    "summary",
    "body",
    "desktop_entry",
    "urgency",
    "read",
    "count",
];

const DND_KEY: &str = "notifications.doNotDisturb";
/// The highest notification id issued, so a restart numbers on from it.
const LAST_ID_KEY: &str = "notifications.lastId";
/// History rows loaded at start; the table itself keeps more.
const HISTORY_SHOWN: i64 = 200;

enum Update {
    Daemon(Event),
    /// Stored history, and which of its rows open notifications stand for.
    History(Vec<Record>, Vec<(u32, i64)>),
    /// A notification's lifetime ran out; it expires only if still showing.
    Expire(u32),
}

pub struct NotificationsRust {
    notifications: *mut ListModel,
    history: *mut ListModel,
    active: bool,
    default_timeout: i32,
    do_not_disturb: bool,
    unread_count: i32,
    stack_duplicates: bool,

    _models: [UniquePtr<ListModel>; 2],
    items: Vec<Notification>,
    records: Vec<Record>,
    /// Live notification id -> history row, so a replacement updates its
    /// row and a close removes it. Mirrored on disk to survive restarts.
    rows: HashMap<u32, i64>,
    /// The highest notification id seen, as stored under `LAST_ID_KEY`.
    last_id: u32,
    controller: Option<Controller>,
    updates: Option<tokio::sync::mpsc::Sender<Update>>,
    tasks: Vec<AbortHandle>,
}

impl Default for NotificationsRust {
    fn default() -> Self {
        let mut live = new_list_model();
        live.pin_mut().configure(ROLES, Some("id"));
        let mut history = new_list_model();
        history.pin_mut().configure(HISTORY_ROLES, Some("id"));
        let do_not_disturb = blacksheep_store::global()
            .and_then(|store| store.with(|c| blacksheep_store::kv::get(c, DND_KEY)).ok().flatten())
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let last_id = blacksheep_store::global()
            .and_then(|store| store.with(|c| blacksheep_store::kv::get(c, LAST_ID_KEY)).ok().flatten())
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        Self {
            notifications: live.as_mut_ptr(),
            history: history.as_mut_ptr(),
            active: false,
            default_timeout: 5000,
            do_not_disturb,
            unread_count: 0,
            stack_duplicates: true,
            _models: [live, history],
            items: Vec::new(),
            records: Vec::new(),
            rows: HashMap::new(),
            last_id,
            controller: None,
            updates: None,
            tasks: Vec::new(),
        }
    }
}

impl Drop for NotificationsRust {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Notifications {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Update>(64);

        // Daemon events, wrapped.
        let (daemon_tx, mut daemon_rx) = tokio::sync::mpsc::channel::<Event>(64);
        let controller = notifications::start(daemon_tx, self.last_id);
        let forward_tx = tx.clone();
        let relay = tokio::spawn(async move {
            while let Some(event) = daemon_rx.recv().await {
                if forward_tx.send(Update::Daemon(event)).await.is_err() {
                    break;
                }
            }
        });

        // Persisted history.
        if let Some(store) = blacksheep_store::global() {
            let store = store.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let loaded = store
                    .run(|c| Ok((history::recent(c, HISTORY_SHOWN)?, history::bindings(c)?)))
                    .await;
                match loaded {
                    Ok((records, bindings)) => {
                        let _ = tx.send(Update::History(records, bindings)).await;
                    }
                    Err(err) => tracing::warn!("notifications: cannot load history: {err}"),
                }
            });
        }

        let consumer = crate::service::forward(self.qt_thread(), rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.controller = Some(controller);
        rust.updates = Some(tx);
        rust.tasks = vec![relay.abort_handle(), consumer];
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn apply(mut obj: Pin<&mut qobject::Notifications>, update: Update) {
    match update {
        Update::History(records, bindings) => {
            let mut rust = obj.as_mut().rust_mut();
            rust.records = records;
            // Ids already seen this run are newer than what was stored.
            for (id, row) in bindings {
                rust.rows.entry(id).or_insert(row);
            }
            obj.refresh_history();
        }
        Update::Daemon(Event::Active(active)) => set_property!(obj, active, active),
        Update::Daemon(Event::Notified(mut notification)) => {
            let replaced = if !obj.do_not_disturb && obj.stack_duplicates {
                obj.as_mut().stack(&mut notification)
            } else {
                None
            };
            obj.as_mut().record(&notification, replaced);
            if let Some(replaced) = replaced
                && let Some(controller) = &obj.controller
            {
                controller.retire(replaced);
            }
            if obj.do_not_disturb {
                // Recorded, not shown. It stays open for the app to recall,
                // unless there is no history row to keep it in.
                if !obj.rows.contains_key(&notification.id)
                    && let Some(controller) = &obj.controller
                {
                    controller.expire(notification.id);
                }
                return;
            }
            schedule_expiry(&obj, &notification);
            let items = &mut obj.as_mut().rust_mut().items;
            match items.iter_mut().find(|n| n.id == notification.id) {
                Some(existing) => *existing = notification,
                None => items.push(notification),
            }
            obj.refresh();
        }
        Update::Expire(id) => {
            if !obj.items.iter().any(|n| n.id == id) {
                return;
            }
            if obj.rows.contains_key(&id) {
                // Only the popup goes: the notification stays open in the
                // centre until the user dismisses it or the app recalls it.
                obj.as_mut().rust_mut().items.retain(|n| n.id != id);
                obj.refresh();
            } else if let Some(controller) = &obj.controller {
                controller.expire(id);
            }
        }
        Update::Daemon(Event::Closed { id, reason }) => {
            obj.as_mut().rust_mut().items.retain(|n| n.id != id);
            let row = obj.as_mut().unbind(id);
            obj.as_mut().refresh();
            // The app took it back, e.g. once read there, so it leaves the
            // centre too, unless another live notification shares the row.
            if reason == CloseReason::Closed
                && let Some(row) = row
                && !obj.rows.values().any(|&r| r == row)
            {
                obj.forget(row);
            }
        }
    }
}

/// Take the popup down when its lifetime runs out. The timer reports back
/// here first, so an id another notification has since taken over is left
/// alone.
fn schedule_expiry(obj: &Pin<&mut qobject::Notifications>, notification: &Notification) {
    let timeout = match notification.expire_timeout {
        -1 if notification.urgency >= 2 => return,
        -1 => obj.default_timeout,
        t => t,
    };
    if timeout <= 0 {
        return;
    }
    let Some(updates) = obj.updates.clone() else {
        return;
    };
    let id = notification.id;
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(timeout as u64)).await;
        let _ = updates.send(Update::Expire(id)).await;
    });
}

impl qobject::Notifications {
    /// Fold a new notification into the showing one it repeats or replaces
    /// by stack tag. Returns the id that stops showing.
    fn stack(mut self: Pin<&mut Self>, notification: &mut Notification) -> Option<u32> {
        let items = &mut self.as_mut().rust_mut().items;
        if items.iter().any(|n| n.id == notification.id) {
            return None;
        }
        let tagged = items.iter().position(|n| notification.shares_stack_tag(n));
        let repeat = || items.iter().position(|n| notification.is_duplicate_of(n));
        let index = tagged.or_else(repeat)?;
        let previous = std::mem::replace(&mut items[index], notification.clone());
        if tagged.is_none() {
            notification.count = previous.count + 1;
            items[index].count = notification.count;
        }
        Some(previous.id)
    }

    fn refresh(self: Pin<&mut Self>) {
        let model = self.notifications;
        // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
        unsafe { Pin::new_unchecked(&mut *model) }.sync(&self.items);
    }

    fn refresh_history(mut self: Pin<&mut Self>) {
        let unread = self.records.iter().filter(|r| !r.read).count() as i32;
        set_property!(self, unread_count, unread);
        let model = self.history;
        // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
        unsafe { Pin::new_unchecked(&mut *model) }.sync(&self.records);
    }

    /// Persist a notification as it arrives, in one transaction: the
    /// highest id issued, its history row unless it is transient, and its
    /// tie to that row. `replaced` is the notification it was stacked onto,
    /// whose row and lifetime pass to it.
    ///
    /// A repeat of a row already in history (same app and content) becomes
    /// that row again, counted and moved to the top, rather than a new one.
    fn record(mut self: Pin<&mut Self>, n: &Notification, replaced: Option<u32>) {
        let last_id = (n.id > self.last_id).then_some(n.id);
        let inherited = {
            let mut rust = self.as_mut().rust_mut();
            if let Some(id) = last_id {
                rust.last_id = id;
            }
            replaced.and_then(|id| rust.rows.remove(&id))
        };
        let mut record = NewRecord {
            notified_at: now(),
            app_name: &n.app_name,
            app_icon: n.image_path.as_deref().unwrap_or(&n.app_icon),
            summary: &n.summary,
            body: &n.body,
            desktop_entry: n.desktop_entry.as_deref().unwrap_or(""),
            urgency: n.urgency,
            count: n.count,
        };
        let existing = self.rows.get(&n.id).copied().or(inherited);
        let repeat = existing.is_none() && self.stack_duplicates;
        let repeat = repeat
            .then(|| {
                self.records.iter().find(|r| {
                    r.app_name == n.app_name
                        && r.summary == n.summary
                        && r.body == n.body
                        && r.app_icon == record.app_icon
                })
            })
            .flatten()
            .map(|r| (r.id, r.count + 1));
        let target = match (existing, repeat) {
            (Some(id), _) => Some(id),
            (None, Some((id, count))) => {
                record.count = count;
                Some(id)
            }
            (None, None) => None,
        };
        let Some(store) = blacksheep_store::global() else {
            if let Some(row) = inherited {
                self.as_mut().rust_mut().rows.insert(n.id, row);
            }
            return;
        };
        let written = store.with(|c| {
            let tx = c.unchecked_transaction()?;
            if let Some(id) = last_id {
                blacksheep_store::kv::set(&tx, LAST_ID_KEY, &serde_json::json!(id))?;
            }
            if let Some(id) = replaced {
                history::unbind(&tx, id)?;
            }
            let row = match (n.transient, target) {
                (true, _) => existing,
                (false, Some(id)) => history::update(&tx, id, &record).map(|_| Some(id))?,
                (false, None) => Some(history::insert(&tx, &record)?),
            };
            if let Some(row) = row {
                history::bind(&tx, n.id, row)?;
            }
            tx.commit()?;
            Ok(row)
        });
        let row_id = match written {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!("notifications: cannot record notification: {err}");
                if let Some(row) = inherited {
                    self.as_mut().rust_mut().rows.insert(n.id, row);
                }
                return;
            }
        };
        self.as_mut().rust_mut().rows.insert(n.id, row_id);
        if n.transient {
            return;
        }
        let full = Record {
            id: row_id,
            notified_at: record.notified_at,
            app_name: record.app_name.to_string(),
            app_icon: record.app_icon.to_string(),
            summary: record.summary.to_string(),
            body: record.body.to_string(),
            desktop_entry: record.desktop_entry.to_string(),
            urgency: record.urgency,
            read: false,
            count: record.count,
        };
        {
            let mut rust = self.as_mut().rust_mut();
            rust.records.retain(|r| r.id != row_id);
            rust.records.insert(0, full);
            rust.records.truncate(HISTORY_SHOWN as usize);
        }
        self.refresh_history();
    }

    /// Drop a closed notification's tie to its row; returns the row.
    fn unbind(mut self: Pin<&mut Self>, id: u32) -> Option<i64> {
        let row = self.as_mut().rust_mut().rows.remove(&id)?;
        if let Some(store) = blacksheep_store::global()
            && let Err(err) = store.with(|c| history::unbind(c, id))
        {
            tracing::warn!("notifications: cannot forget closed notification: {err}");
        }
        Some(row)
    }

    fn set_do_not_disturb(mut self: Pin<&mut Self>, on: bool) {
        set_property!(self, do_not_disturb, on);
        if let Some(store) = blacksheep_store::global()
            && let Err(err) = store.with(|c| blacksheep_store::kv::set(c, DND_KEY, &serde_json::json!(on)))
        {
            tracing::warn!("notifications: cannot save do-not-disturb: {err}");
        }
    }

    fn mark_all_read(mut self: Pin<&mut Self>) {
        if self.records.iter().all(|r| r.read) {
            return;
        }
        for record in &mut self.as_mut().rust_mut().records {
            record.read = true;
        }
        if let Some(store) = blacksheep_store::global()
            && let Err(err) = store.with(history::mark_all_read)
        {
            tracing::warn!("notifications: cannot mark history read: {err}");
        }
        self.refresh_history();
    }

    /// Close the live notifications a history row stands for.
    fn dismiss_row(&self, row: i64) {
        if let Some(controller) = &self.controller {
            for (&id, _) in self.rows.iter().filter(|(_, r)| **r == row) {
                controller.dismiss(id);
            }
        }
    }

    fn remove_from_history(self: Pin<&mut Self>, id: i64) {
        self.dismiss_row(id);
        self.forget(id);
    }

    /// Delete a history row, in memory and on disk.
    fn forget(mut self: Pin<&mut Self>, id: i64) {
        self.as_mut().rust_mut().records.retain(|r| r.id != id);
        if let Some(store) = blacksheep_store::global()
            && let Err(err) = store.with(|c| history::remove(c, id))
        {
            tracing::warn!("notifications: cannot remove history entry: {err}");
        }
        self.refresh_history();
    }

    fn clear_history(mut self: Pin<&mut Self>) {
        if let Some(controller) = &self.controller {
            for &id in self.rows.keys() {
                controller.dismiss(id);
            }
        }
        self.as_mut().rust_mut().records.clear();
        self.as_mut().rust_mut().rows.clear();
        if let Some(store) = blacksheep_store::global()
            && let Err(err) = store.with(history::clear)
        {
            tracing::warn!("notifications: cannot clear history: {err}");
        }
        self.refresh_history();
    }

    fn dismiss(&self, id: u32) {
        if let Some(controller) = &self.controller {
            controller.dismiss(id);
        }
    }

    fn invoke(&self, id: u32, action_key: &QString) {
        if let Some(controller) = &self.controller {
            controller.invoke(id, action_key.to_string());
        }
    }
}
