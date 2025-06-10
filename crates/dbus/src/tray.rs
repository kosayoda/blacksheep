//! System tray: hosts `org.kde.StatusNotifierWatcher`, follows every
//! `org.kde.StatusNotifierItem`, and reads their `com.canonical.dbusmenu`
//! menus. If another watcher already owns the name, this acts as a host on
//! that watcher instead, so the tray keeps working next to another shell.

use std::collections::HashMap;

use futures_util::{StreamExt as _, stream::select_all};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use zbus::fdo::DBusProxy;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, proxy};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// ARGB pixels as delivered by the item, converted to BGRA byte order,
/// which is what `QImage::Format_ARGB32` expects on little-endian hosts.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixmap {
    pub width: i32,
    pub height: i32,
    pub bgra: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    /// `standard` or `separator`.
    pub kind: String,
    /// `checkmark`, `radio` or empty.
    pub toggle_type: String,
    /// -1 unknown, 0 off, 1 on.
    pub toggle_state: i32,
    pub icon_name: String,
    pub children: Vec<MenuEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    /// Stable key: `bus_name` + object path.
    pub id: String,
    /// The item's own `Id`, usually the application name.
    pub app_id: String,
    pub title: String,
    /// `Passive`, `Active` or `NeedsAttention`.
    pub status: String,
    pub category: String,
    pub icon_name: String,
    pub attention_icon_name: String,
    /// Extra directory the icon names resolve in, if the app ships its own.
    pub icon_theme_path: String,
    pub tooltip_title: String,
    pub tooltip_body: String,
    /// The item only has a menu; left click should open it.
    pub is_menu: bool,
    pub has_menu: bool,
    pub menu: Vec<MenuEntry>,
    /// Icon delivered as pixels rather than a theme name.
    #[serde(skip)]
    pub icon_pixmap: Option<Pixmap>,
    #[serde(skip)]
    pub attention_pixmap: Option<Pixmap>,
}

#[derive(Debug, Clone)]
pub enum Update {
    /// Whether a watcher (ours or another) is available at all.
    Active(bool),
    Items(Vec<Item>),
}

#[derive(Debug, Clone)]
pub enum Command {
    Activate { id: String, x: i32, y: i32 },
    SecondaryActivate { id: String, x: i32, y: i32 },
    ContextMenu { id: String, x: i32, y: i32 },
    Scroll { id: String, delta: i32, orientation: String },
    /// A menu entry was clicked.
    MenuEvent { id: String, entry: i32 },
    /// A (sub)menu is about to be shown; apps may populate it now.
    MenuAboutToShow { id: String, entry: i32 },
}

#[derive(Clone, Debug)]
pub struct Controller {
    tx: mpsc::UnboundedSender<Command>,
}

impl Controller {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
}

pub fn start(updates: mpsc::Sender<Update>) -> Controller {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(updates, rx));
    Controller { tx }
}

// ---------------------------------------------------------------- proxies

type RawPixmaps = Vec<(i32, i32, Vec<u8>)>;

#[proxy(interface = "org.kde.StatusNotifierItem")]
trait StatusNotifierItem {
    #[zbus(property(emits_changed_signal = "false"))]
    fn category(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn id(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn title(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn status(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn icon_name(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn icon_pixmap(&self) -> zbus::Result<RawPixmaps>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn attention_icon_name(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn attention_icon_pixmap(&self) -> zbus::Result<RawPixmaps>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn icon_theme_path(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn tool_tip(&self) -> zbus::Result<(String, RawPixmaps, String, String)>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn item_is_menu(&self) -> zbus::Result<bool>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn menu(&self) -> zbus::Result<OwnedObjectPath>;

    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;
    fn secondary_activate(&self, x: i32, y: i32) -> zbus::Result<()>;
    fn context_menu(&self, x: i32, y: i32) -> zbus::Result<()>;
    fn scroll(&self, delta: i32, orientation: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn new_title(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_icon(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_attention_icon(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_overlay_icon(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_tool_tip(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn new_status(&self, status: String) -> zbus::Result<()>;
}

/// `(ia{sv}av)`: id, properties, children (each a variant holding another layout).
type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[proxy(interface = "com.canonical.dbusmenu")]
trait DbusMenu {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, Layout)>;
    fn event(&self, id: i32, event_id: &str, data: &Value<'_>, timestamp: u32) -> zbus::Result<()>;
    fn about_to_show(&self, id: i32) -> zbus::Result<bool>;

    #[zbus(signal)]
    fn layout_updated(&self, revision: u32, parent: i32) -> zbus::Result<()>;
    #[zbus(signal)]
    fn items_properties_updated(
        &self,
        updated: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;
}

/// Another process's watcher, used when we cannot own the name.
#[proxy(
    interface = "org.kde.StatusNotifierWatcher",
    default_service = "org.kde.StatusNotifierWatcher",
    default_path = "/StatusNotifierWatcher"
)]
trait StatusNotifierWatcher {
    fn register_status_notifier_host(&self, service: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> zbus::Result<Vec<String>>;
    #[zbus(signal)]
    fn status_notifier_item_registered(&self, service: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn status_notifier_item_unregistered(&self, service: String) -> zbus::Result<()>;
}

// ---------------------------------------------------------------- watcher

enum Registration {
    Added { key: String, bus: String, path: String },
}

struct Watcher {
    items: Vec<String>,
    events: mpsc::UnboundedSender<Registration>,
}

/// Split what an application passed to `RegisterStatusNotifierItem` into
/// bus name and object path. The spec says "the bus name", KDE apps pass an
/// object path, and some pass "name/path".
fn split_service(service: &str, sender: Option<&str>) -> Option<(String, String)> {
    if let Some(path) = service.strip_prefix('/') {
        return Some((sender?.to_string(), format!("/{path}")));
    }
    if let Some((bus, path)) = service.split_once('/') {
        return Some((bus.to_string(), format!("/{path}")));
    }
    Some((service.to_string(), "/StatusNotifierItem".to_string()))
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    async fn register_status_notifier_item(
        &mut self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        service: String,
    ) {
        let sender = header.sender().map(|s| s.to_string());
        let Some((bus, path)) = split_service(&service, sender.as_deref()) else {
            return;
        };
        let key = format!("{bus}{path}");
        if !self.items.contains(&key) {
            self.items.push(key.clone());
            let _ = Self::status_notifier_item_registered(&emitter, key.clone()).await;
            let _ = self.registered_status_notifier_items_changed(&emitter).await;
        }
        let _ = self.events.send(Registration::Added { key, bus, path });
    }

    async fn register_status_notifier_host(
        &mut self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        _service: String,
    ) {
        let _ = Self::status_notifier_host_registered(&emitter).await;
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(emitter: &SignalEmitter<'_>, service: String) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_item_unregistered(emitter: &SignalEmitter<'_>, service: String) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

// ---------------------------------------------------------------- manager

enum Event {
    Changed(Box<Item>),
    Removed(String),
}

struct Known {
    bus: String,
    path: String,
    task: AbortHandle,
}

async fn run(updates: mpsc::Sender<Update>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let mut delay = crate::INITIAL_BACKOFF;
    loop {
        match serve(&updates, &mut commands).await {
            Ok(()) => return,
            Err(err) => tracing::warn!("tray: {err}"),
        }
        if updates.send(Update::Active(false)).await.is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
        delay = crate::backoff(delay);
    }
}

async fn serve(
    updates: &mpsc::Sender<Update>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> zbus::Result<()> {
    let (reg_tx, mut registrations) = mpsc::unbounded_channel::<Registration>();
    let (events_tx, mut events) = mpsc::channel::<Event>(64);

    // Try to be the watcher; fall back to being a host on someone else's.
    let own = zbus::connection::Builder::session()?
        // Never let a second instance (a test harness) take the name from us.
        .allow_name_replacements(false)
        .replace_existing_names(false)
        .name(WATCHER_NAME)?
        .serve_at(
            WATCHER_PATH,
            Watcher {
                items: Vec::new(),
                events: reg_tx.clone(),
            },
        )?
        .build()
        .await;
    let (connection, foreign) = match own {
        Ok(connection) => {
            tracing::info!("tray: hosting {WATCHER_NAME}");
            (connection, None)
        }
        Err(zbus::Error::NameTaken) => {
            let connection = zbus::Connection::session().await?;
            let watcher = StatusNotifierWatcherProxy::new(&connection).await?;
            tracing::info!("tray: another process owns {WATCHER_NAME}; joining it as a host");
            (connection, Some(watcher))
        }
        Err(err) => return Err(err),
    };

    let dbus = DBusProxy::new(&connection).await?;
    let mut owners = dbus.receive_name_owner_changed().await?;
    let mut known: HashMap<String, Known> = HashMap::new();
    let mut items: HashMap<String, Item> = HashMap::new();
    let mut menus: HashMap<String, DbusMenuProxy<'static>> = HashMap::new();

    let mut foreign_added = None;
    let mut foreign_removed = None;
    if let Some(watcher) = &foreign {
        let unique = connection.unique_name().map(|n| n.to_string()).unwrap_or_default();
        let _ = watcher.register_status_notifier_host(&unique).await;
        for service in watcher.registered_status_notifier_items().await.unwrap_or_default() {
            if let Some((bus, path)) = split_service(&service, None) {
                let _ = reg_tx.send(Registration::Added {
                    key: format!("{bus}{path}"),
                    bus,
                    path,
                });
            }
        }
        foreign_added = Some(watcher.receive_status_notifier_item_registered().await?);
        foreign_removed = Some(watcher.receive_status_notifier_item_unregistered().await?);
    }
    if updates.send(Update::Active(true)).await.is_err() {
        return Ok(());
    }

    loop {
        tokio::select! {
            registration = registrations.recv() => {
                let Some(Registration::Added { key, bus, path }) = registration else {
                    return Err(zbus::Error::Failure("watcher gone".into()));
                };
                if known.contains_key(&key) {
                    continue;
                }
                let task = tokio::spawn(watch_item(connection.clone(), key.clone(), bus.clone(), path.clone(), events_tx.clone()));
                known.insert(key, Known { bus, path, task: task.abort_handle() });
            }
            signal = async { match foreign_added.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                if let Some(signal) = signal
                    && let Ok(args) = signal.args()
                    && let Some((bus, path)) = split_service(args.service(), None)
                {
                    let _ = reg_tx.send(Registration::Added { key: format!("{bus}{path}"), bus, path });
                }
            }
            signal = async { match foreign_removed.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                if let Some(signal) = signal
                    && let Ok(args) = signal.args()
                    && let Some((bus, path)) = split_service(args.service(), None)
                {
                    let key = format!("{bus}{path}");
                    if let Some(k) = known.remove(&key) { k.task.abort(); }
                    items.remove(&key); menus.remove(&key);
                    if send_snapshot(updates, &items).await.is_err() { return Ok(()); }
                }
            }
            owner = owners.next() => {
                let Some(owner) = owner else { return Err(zbus::Error::Failure("NameOwnerChanged stream ended".into())); };
                let args = owner.args()?;
                // Lost the watcher name to someone else: start over and join them.
                if foreign.is_none()
                    && args.name().as_str() == WATCHER_NAME
                    && args.new_owner().as_ref().map(|o| o.as_str()) != connection.unique_name().map(|n| n.as_str())
                {
                    for k in known.values() { k.task.abort(); }
                    return Err(zbus::Error::Failure("watcher name taken over by another process".into()));
                }
                if args.new_owner().is_some() { continue; }
                let gone = args.name().to_string();
                let dead: Vec<String> = known.iter().filter(|(_, k)| k.bus == gone).map(|(key, _)| key.clone()).collect();
                if dead.is_empty() { continue; }
                for key in dead {
                    if let Some(k) = known.remove(&key) { k.task.abort(); }
                    items.remove(&key); menus.remove(&key);
                    if foreign.is_none()
                        && let Ok(iface) = connection.object_server().interface::<_, Watcher>(WATCHER_PATH).await
                    {
                        let mut watcher = iface.get_mut().await;
                        watcher.items.retain(|i| *i != key);
                        let _ = Watcher::status_notifier_item_unregistered(iface.signal_emitter(), key.clone()).await;
                        let _ = watcher.registered_status_notifier_items_changed(iface.signal_emitter()).await;
                    }
                }
                if send_snapshot(updates, &items).await.is_err() { return Ok(()); }
            }
            event = events.recv() => {
                match event {
                    Some(Event::Changed(item)) => {
                        items.insert(item.id.clone(), *item);
                    }
                    Some(Event::Removed(key)) => {
                        known.remove(&key); items.remove(&key); menus.remove(&key);
                    }
                    None => return Err(zbus::Error::Failure("item events ended".into())),
                }
                if send_snapshot(updates, &items).await.is_err() { return Ok(()); }
            }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()) };
                if let Err(err) = perform(&connection, &known, &mut menus, command.clone()).await {
                    tracing::warn!("tray: {command:?} failed: {err}");
                }
            }
        }
    }
}

async fn send_snapshot(
    updates: &mpsc::Sender<Update>,
    items: &HashMap<String, Item>,
) -> Result<(), mpsc::error::SendError<Update>> {
    let mut snapshot: Vec<Item> = items.values().cloned().collect();
    snapshot.sort_by(|a, b| a.app_id.to_lowercase().cmp(&b.app_id.to_lowercase()).then(a.id.cmp(&b.id)));
    updates.send(Update::Items(snapshot)).await
}

async fn perform(
    connection: &zbus::Connection,
    known: &HashMap<String, Known>,
    menus: &mut HashMap<String, DbusMenuProxy<'static>>,
    command: Command,
) -> zbus::Result<()> {
    let key = match &command {
        Command::Activate { id, .. }
        | Command::SecondaryActivate { id, .. }
        | Command::ContextMenu { id, .. }
        | Command::Scroll { id, .. }
        | Command::MenuEvent { id, .. }
        | Command::MenuAboutToShow { id, .. } => id.clone(),
    };
    let Some(k) = known.get(&key) else {
        return Err(zbus::Error::Failure(format!("unknown tray item {key}")));
    };
    let item = StatusNotifierItemProxy::builder(connection)
        .destination(k.bus.clone())?
        .path(k.path.clone())?
        .build()
        .await?;
    match command {
        Command::Activate { x, y, .. } => item.activate(x, y).await,
        Command::SecondaryActivate { x, y, .. } => item.secondary_activate(x, y).await,
        Command::ContextMenu { x, y, .. } => item.context_menu(x, y).await,
        Command::Scroll { delta, orientation, .. } => item.scroll(delta, &orientation).await,
        Command::MenuEvent { entry, .. } | Command::MenuAboutToShow { entry, .. } => {
            let menu_path = match item.menu().await {
                Ok(path) if !path.as_str().is_empty() && path.as_str() != "/" => path.to_string(),
                _ => return Err(zbus::Error::Failure("item has no menu".into())),
            };
            let menu = match menus.get(&key) {
                Some(menu) => menu.clone(),
                None => {
                    let menu = DbusMenuProxy::builder(connection)
                        .destination(k.bus.clone())?
                        .path(menu_path)?
                        .build()
                        .await?;
                    menus.insert(key.clone(), menu.clone());
                    menu
                }
            };
            if matches!(command, Command::MenuAboutToShow { .. }) {
                menu.about_to_show(entry).await.map(|_| ())
            } else {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as u32)
                    .unwrap_or(0);
                menu.event(entry, "clicked", &Value::I32(0), now).await
            }
        }
    }
}

// ---------------------------------------------------------------- items

async fn watch_item(connection: zbus::Connection, key: String, bus: String, path: String, events: mpsc::Sender<Event>) {
    if let Err(err) = watch_item_inner(&connection, &key, &bus, &path, &events).await {
        tracing::debug!("tray: {key}: {err}");
    }
    let _ = events.send(Event::Removed(key)).await;
}

async fn watch_item_inner(
    connection: &zbus::Connection,
    key: &str,
    bus: &str,
    path: &str,
    events: &mpsc::Sender<Event>,
) -> zbus::Result<()> {
    let item = StatusNotifierItemProxy::builder(connection)
        .destination(bus.to_string())?
        .path(path.to_string())?
        .build()
        .await?;

    let mut changes = select_all([
        item.receive_new_title().await?.map(|_| ()).boxed(),
        item.receive_new_icon().await?.map(|_| ()).boxed(),
        item.receive_new_attention_icon().await?.map(|_| ()).boxed(),
        item.receive_new_overlay_icon().await?.map(|_| ()).boxed(),
        item.receive_new_tool_tip().await?.map(|_| ()).boxed(),
        item.receive_new_status().await?.map(|_| ()).boxed(),
    ]);

    // The menu, if any, is followed on its own streams.
    let menu_path = match item.menu().await {
        Ok(p) if !p.as_str().is_empty() && p.as_str() != "/" => Some(p.to_string()),
        _ => None,
    };
    let menu = match &menu_path {
        Some(menu_path) => Some(
            DbusMenuProxy::builder(connection)
                .destination(bus.to_string())?
                .path(menu_path.clone())?
                .build()
                .await?,
        ),
        None => None,
    };
    let mut menu_changes = match &menu {
        Some(menu) => select_all([
            menu.receive_layout_updated().await?.map(|_| ()).boxed(),
            menu.receive_items_properties_updated().await?.map(|_| ()).boxed(),
        ]),
        None => select_all(Vec::new()),
    };

    let mut last: Option<Item> = None;
    loop {
        let snapshot = read_item(key, &item, menu.as_ref()).await;
        if last.as_ref() != Some(&snapshot) {
            if events.send(Event::Changed(Box::new(snapshot.clone()))).await.is_err() {
                return Ok(());
            }
            last = Some(snapshot);
        }
        tokio::select! {
            change = changes.next() => { if change.is_none() { return Ok(()); } }
            change = menu_changes.next(), if menu.is_some() => { if change.is_none() { return Ok(()); } }
        }
    }
}

async fn read_item(key: &str, item: &StatusNotifierItemProxy<'_>, menu: Option<&DbusMenuProxy<'_>>) -> Item {
    let tooltip = item.tool_tip().await.ok();
    let menu_entries = match menu {
        Some(menu) => read_menu(menu).await,
        None => Vec::new(),
    };
    Item {
        id: key.to_string(),
        app_id: item.id().await.unwrap_or_default(),
        title: item.title().await.unwrap_or_default(),
        status: item.status().await.unwrap_or_else(|_| "Active".into()),
        category: item.category().await.unwrap_or_default(),
        icon_name: item.icon_name().await.unwrap_or_default(),
        attention_icon_name: item.attention_icon_name().await.unwrap_or_default(),
        icon_theme_path: item.icon_theme_path().await.unwrap_or_default(),
        tooltip_title: tooltip.as_ref().map(|t| t.2.clone()).unwrap_or_default(),
        tooltip_body: tooltip.as_ref().map(|t| t.3.clone()).unwrap_or_default(),
        is_menu: item.item_is_menu().await.unwrap_or(false),
        has_menu: menu.is_some(),
        menu: menu_entries,
        icon_pixmap: item.icon_pixmap().await.ok().and_then(pick_pixmap),
        attention_pixmap: item.attention_icon_pixmap().await.ok().and_then(pick_pixmap),
    }
}

/// Largest icon up to 64px, converted from network-order ARGB to BGRA.
fn pick_pixmap(raw: RawPixmaps) -> Option<Pixmap> {
    let (w, h, argb) = raw
        .into_iter()
        .filter(|(w, h, data)| *w > 0 && *h > 0 && data.len() == (*w as usize) * (*h as usize) * 4)
        .max_by_key(|(w, h, _)| {
            let side = (*w).max(*h);
            if side <= 64 { side } else { 64 - (side - 64).min(63) }
        })?;
    let mut bgra = Vec::with_capacity(argb.len());
    for px in argb.chunks_exact(4) {
        bgra.extend_from_slice(&[px[3], px[2], px[1], px[0]]);
    }
    Some(Pixmap { width: w, height: h, bgra })
}

async fn read_menu(menu: &DbusMenuProxy<'_>) -> Vec<MenuEntry> {
    let props = ["label", "enabled", "visible", "type", "toggle-type", "toggle-state", "icon-name", "children-display"];
    match menu.get_layout(0, -1, &props).await {
        Ok((_, (_, _, children))) => children.iter().filter_map(|c| parse_layout(c)).collect(),
        Err(err) => {
            tracing::debug!("tray: GetLayout failed: {err}");
            Vec::new()
        }
    }
}

fn unwrap_variant<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

fn value_str(v: &Value<'_>) -> Option<String> {
    match unwrap_variant(v) {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn value_bool(v: &Value<'_>) -> Option<bool> {
    match unwrap_variant(v) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn value_i32(v: &Value<'_>) -> Option<i32> {
    match unwrap_variant(v) {
        Value::I32(i) => Some(*i),
        Value::I64(i) => Some(*i as i32),
        Value::U32(i) => Some(*i as i32),
        _ => None,
    }
}

fn parse_layout(v: &Value<'_>) -> Option<MenuEntry> {
    let Value::Structure(structure) = unwrap_variant(v) else {
        return None;
    };
    let fields = structure.fields();
    let id = value_i32(fields.first()?)?;
    let mut props: HashMap<String, Value<'_>> = HashMap::new();
    if let Some(Value::Dict(dict)) = fields.get(1).map(unwrap_variant) {
        for (k, val) in dict.iter() {
            if let Some(k) = value_str(k) {
                props.insert(k, val.clone());
            }
        }
    }
    let children = match fields.get(2).map(unwrap_variant) {
        Some(Value::Array(array)) => array.iter().filter_map(parse_layout).collect(),
        _ => Vec::new(),
    };
    let get = |name: &str| props.get(name);
    Some(MenuEntry {
        id,
        label: get("label").and_then(value_str).unwrap_or_default().replace('_', ""),
        enabled: get("enabled").and_then(value_bool).unwrap_or(true),
        visible: get("visible").and_then(value_bool).unwrap_or(true),
        kind: get("type").and_then(value_str).unwrap_or_else(|| "standard".into()),
        toggle_type: get("toggle-type").and_then(value_str).unwrap_or_default(),
        toggle_state: get("toggle-state").and_then(value_i32).unwrap_or(-1),
        icon_name: get("icon-name").and_then(value_str).unwrap_or_default(),
        children,
    })
}
