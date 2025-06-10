//! `Tray` singleton: StatusNotifierItems as a list model with menus.

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
        /// One row per tray item, sorted by application id.
        #[qproperty(*mut ListModel, items, READ, CONSTANT)]
        /// A StatusNotifierWatcher is available (ours or another shell's).
        #[qproperty(bool, active, READ, NOTIFY)]
        type Tray = super::TrayRust;

        #[qinvokable]
        fn activate(self: &Tray, id: &QString, x: i32, y: i32);
        #[qinvokable]
        #[cxx_name = "secondaryActivate"]
        fn secondary_activate(self: &Tray, id: &QString, x: i32, y: i32);
        /// Ask the item to show its own menu, for items without a dbusmenu.
        #[qinvokable]
        #[cxx_name = "contextMenu"]
        fn context_menu(self: &Tray, id: &QString, x: i32, y: i32);
        /// `orientation` is `vertical` or `horizontal`; delta in wheel steps.
        #[qinvokable]
        fn scroll(self: &Tray, id: &QString, delta: i32, orientation: &QString);
        /// Click a dbusmenu entry by its id from the `menu` role.
        #[qinvokable]
        #[cxx_name = "menuEvent"]
        fn menu_event(self: &Tray, id: &QString, entry: i32);
        /// Tell the app a (sub)menu is opening; 0 is the root.
        #[qinvokable]
        #[cxx_name = "menuAboutToShow"]
        fn menu_about_to_show(self: &Tray, id: &QString, entry: i32);
    }

    impl cxx_qt::Initialize for Tray {}
    impl cxx_qt::Threading for Tray {}
}

use std::collections::HashMap;
use std::pin::Pin;

use blacksheep_dbus::tray::{self, Command, Controller, Item, MenuEntry, Update};
use cxx::UniquePtr;
use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde::Serialize;
use tokio::task::AbortHandle;

use crate::construct::new_list_model;
use crate::model::qobject::ListModel;
use crate::service::set_property;

const ROLES: &[&str] = &[
    "id",
    "app_id",
    "title",
    "status",
    "category",
    "icon_source",
    "tooltip_title",
    "tooltip_body",
    "is_menu",
    "has_menu",
    "menu",
];

/// What QML sees; `icon_source` is resolved here from name or pixels.
#[derive(Serialize)]
struct Row<'a> {
    id: &'a str,
    app_id: &'a str,
    title: &'a str,
    status: &'a str,
    category: &'a str,
    icon_source: String,
    tooltip_title: &'a str,
    tooltip_body: &'a str,
    is_menu: bool,
    has_menu: bool,
    menu: &'a [MenuEntry],
}

pub struct TrayRust {
    items: *mut ListModel,
    active: bool,

    _model: UniquePtr<ListModel>,
    /// Per item: pixel data last handed to the provider and a revision that
    /// busts QML's image cache when it changes.
    pixmaps: HashMap<String, (Vec<u8>, u32)>,
    controller: Option<Controller>,
    task: Option<AbortHandle>,
}

impl Default for TrayRust {
    fn default() -> Self {
        let mut model = new_list_model();
        model.pin_mut().configure(ROLES, Some("id"));
        Self {
            items: model.as_mut_ptr(),
            active: false,
            _model: model,
            pixmaps: HashMap::new(),
            controller: None,
            task: None,
        }
    }
}

impl Drop for TrayRust {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl cxx_qt::Initialize for qobject::Tray {
    fn initialize(mut self: Pin<&mut Self>) {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let controller = tray::start(tx);
        let task = crate::service::forward(self.qt_thread(), rx, apply);
        let mut rust = self.as_mut().rust_mut();
        rust.controller = Some(controller);
        rust.task = Some(task);
    }
}

fn apply(mut obj: Pin<&mut qobject::Tray>, update: Update) {
    match update {
        Update::Active(active) => set_property!(obj, active, active),
        Update::Items(items) => {
            let rows: Vec<Row> = items.iter().map(|item| row(obj.as_mut(), item)).collect();
            let model = obj.items;
            // SAFETY: owned by the UniquePtr in this object, used on the Qt thread.
            unsafe { Pin::new_unchecked(&mut *model) }.sync(&rows);

            // Drop pixels of items that went away.
            let live: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
            let stale: Vec<String> = obj
                .pixmaps
                .keys()
                .filter(|k| !live.contains(&k.as_str()))
                .cloned()
                .collect();
            for id in stale {
                crate::icons::remove_tray_icon(&QString::from(id.as_str()));
                obj.as_mut().rust_mut().pixmaps.remove(&id);
            }
        }
    }
}

fn row<'a>(mut obj: Pin<&mut qobject::Tray>, item: &'a Item) -> Row<'a> {
    let attention = item.status == "NeedsAttention";
    let pixmap = if attention {
        item.attention_pixmap.as_ref().or(item.icon_pixmap.as_ref())
    } else {
        item.icon_pixmap.as_ref()
    };
    let name = if attention && !item.attention_icon_name.is_empty() {
        &item.attention_icon_name
    } else {
        &item.icon_name
    };

    let icon_source = if let Some(pixmap) = pixmap {
        let revision = {
            let mut rust = obj.as_mut().rust_mut();
            let entry = rust.pixmaps.entry(item.id.clone()).or_insert_with(|| (Vec::new(), 0));
            if entry.0 != pixmap.bgra {
                entry.0 = pixmap.bgra.clone();
                entry.1 += 1;
                crate::icons::set_tray_icon(
                    &QString::from(item.id.as_str()),
                    pixmap.width,
                    pixmap.height,
                    &pixmap.bgra,
                );
            }
            entry.1
        };
        format!("image://tray/{}?v={revision}", item.id)
    } else if name.is_empty() {
        String::new()
    } else if item.icon_theme_path.is_empty() {
        format!("image://icon/{name}")
    } else {
        format!("image://icon/{name}?path={}", item.icon_theme_path)
    };

    Row {
        id: &item.id,
        app_id: &item.app_id,
        title: &item.title,
        status: &item.status,
        category: &item.category,
        icon_source,
        tooltip_title: &item.tooltip_title,
        tooltip_body: &item.tooltip_body,
        is_menu: item.is_menu,
        has_menu: item.has_menu,
        menu: &item.menu,
    }
}

impl qobject::Tray {
    fn send(&self, command: Command) {
        if let Some(controller) = &self.controller {
            controller.send(command);
        }
    }

    fn activate(&self, id: &QString, x: i32, y: i32) {
        self.send(Command::Activate { id: id.to_string(), x, y });
    }
    fn secondary_activate(&self, id: &QString, x: i32, y: i32) {
        self.send(Command::SecondaryActivate { id: id.to_string(), x, y });
    }
    fn context_menu(&self, id: &QString, x: i32, y: i32) {
        self.send(Command::ContextMenu { id: id.to_string(), x, y });
    }
    fn scroll(&self, id: &QString, delta: i32, orientation: &QString) {
        self.send(Command::Scroll {
            id: id.to_string(),
            delta,
            orientation: orientation.to_string(),
        });
    }
    fn menu_event(&self, id: &QString, entry: i32) {
        self.send(Command::MenuEvent { id: id.to_string(), entry });
    }
    fn menu_about_to_show(&self, id: &QString, entry: i32) {
        self.send(Command::MenuAboutToShow { id: id.to_string(), entry });
    }
}
