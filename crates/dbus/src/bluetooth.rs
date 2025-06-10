//! Bluetooth through BlueZ's object manager on the system bus.

use std::collections::HashMap;

use futures_util::StreamExt as _;
use serde::Serialize;
use tokio::sync::mpsc;
use zbus::fdo::ObjectManagerProxy;
use zbus::message::Type as MessageType;
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{MatchRule, MessageStream};

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Adapter {
    pub path: String,
    pub name: String,
    pub address: String,
    pub powered: bool,
    pub discovering: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Device {
    pub path: String,
    pub address: String,
    pub name: String,
    /// Freedesktop icon name BlueZ derives from the device class.
    pub icon: String,
    pub connected: bool,
    pub paired: bool,
    pub trusted: bool,
    /// Signal strength in dBm while discovering, else absent.
    pub rssi: Option<i16>,
    /// From `org.bluez.Battery1` when the device reports one.
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct State {
    /// BlueZ is reachable and has at least one adapter.
    pub available: bool,
    pub adapter: Option<Adapter>,
    /// Sorted: connected first, then paired, then by name.
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone)]
pub enum Command {
    SetPowered(bool),
    StartDiscovery,
    StopDiscovery,
    Connect(String),
    Disconnect(String),
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

#[proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
trait Adapter1 {
    fn start_discovery(&self) -> zbus::Result<()>;
    fn stop_discovery(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_powered(&self, value: bool) -> zbus::Result<()>;
}

#[proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait Device1 {
    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
}

type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// The object manager keys interfaces by a name type; plain strings are
/// easier to look up.
fn normalize(
    objects: HashMap<OwnedObjectPath, HashMap<zbus::names::OwnedInterfaceName, HashMap<String, OwnedValue>>>,
) -> Objects {
    objects
        .into_iter()
        .map(|(path, ifaces)| {
            (path, ifaces.into_iter().map(|(name, props)| (name.to_string(), props)).collect())
        })
        .collect()
}

pub fn start(updates: mpsc::Sender<State>) -> Controller {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(updates, rx));
    Controller { tx }
}

async fn run(updates: mpsc::Sender<State>, mut commands: mpsc::UnboundedReceiver<Command>) {
    let mut delay = crate::INITIAL_BACKOFF;
    loop {
        match watch(&updates, &mut commands).await {
            Ok(()) => return,
            Err(err) => tracing::warn!("bluetooth: {err}"),
        }
        if updates.send(State::default()).await.is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
        delay = crate::backoff(delay);
    }
}

fn prop<'a>(props: &'a HashMap<String, OwnedValue>, key: &str) -> Option<&'a OwnedValue> {
    props.get(key)
}

fn string(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    prop(props, key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn boolean(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    prop(props, key)
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false)
}

fn snapshot(objects: &Objects) -> State {
    let adapter = objects
        .iter()
        .filter_map(|(path, ifaces)| {
            let props = ifaces.get("org.bluez.Adapter1")?;
            Some(Adapter {
                path: path.to_string(),
                name: string(props, "Alias"),
                address: string(props, "Address"),
                powered: boolean(props, "Powered"),
                discovering: boolean(props, "Discovering"),
            })
        })
        .min_by(|a, b| a.path.cmp(&b.path));

    let mut devices: Vec<Device> = objects
        .iter()
        .filter_map(|(path, ifaces)| {
            let props = ifaces.get("org.bluez.Device1")?;
            let battery = ifaces
                .get("org.bluez.Battery1")
                .and_then(|b| prop(b, "Percentage"))
                .and_then(|v| u8::try_from(v.clone()).ok());
            Some(Device {
                path: path.to_string(),
                address: string(props, "Address"),
                name: {
                    let alias = string(props, "Alias");
                    if alias.is_empty() { string(props, "Name") } else { alias }
                },
                icon: string(props, "Icon"),
                connected: boolean(props, "Connected"),
                paired: boolean(props, "Paired"),
                trusted: boolean(props, "Trusted"),
                rssi: prop(props, "RSSI").and_then(|v| i16::try_from(v.clone()).ok()),
                battery,
            })
        })
        .collect();
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    State {
        available: adapter.is_some(),
        adapter,
        devices,
    }
}

async fn watch(
    updates: &mpsc::Sender<State>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let manager = ObjectManagerProxy::builder(&connection)
        .destination("org.bluez")?
        .path("/")?
        .build()
        .await?;
    let mut added = manager.receive_interfaces_added().await?;
    let mut removed = manager.receive_interfaces_removed().await?;
    // Every property change on any BlueZ object; re-reading everything is
    // cheap and avoids one proxy per device.
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.bluez")?
        .interface("org.freedesktop.DBus.Properties")?
        .member("PropertiesChanged")?
        .build();
    let mut changes = MessageStream::for_match_rule(rule, &connection, Some(16)).await?;

    let mut last = None;
    loop {
        let state = snapshot(&normalize(manager.get_managed_objects().await?));
        if last.as_ref() != Some(&state) {
            if updates.send(state.clone()).await.is_err() {
                return Ok(());
            }
            last = Some(state.clone());
        }
        tokio::select! {
            s = added.next() => { if s.is_none() { return Err(zbus::Error::Failure("InterfacesAdded ended".into())); } }
            s = removed.next() => { if s.is_none() { return Err(zbus::Error::Failure("InterfacesRemoved ended".into())); } }
            s = changes.next() => { if s.is_none() { return Err(zbus::Error::Failure("PropertiesChanged ended".into())); } }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()) };
                if let Err(err) = perform(&connection, last.as_ref(), command.clone()).await {
                    tracing::warn!("bluetooth: {command:?} failed: {err}");
                }
            }
        }
    }
}

async fn perform(connection: &zbus::Connection, state: Option<&State>, command: Command) -> zbus::Result<()> {
    let adapter_path = state
        .and_then(|s| s.adapter.as_ref())
        .map(|a| a.path.clone())
        .ok_or_else(|| zbus::Error::Failure("no bluetooth adapter".into()))?;
    let device_path = |address: &str| {
        state
            .and_then(|s| s.devices.iter().find(|d| d.address == address))
            .map(|d| d.path.clone())
            .ok_or_else(|| zbus::Error::Failure(format!("no device {address}")))
    };
    match command {
        Command::SetPowered(on) => {
            Adapter1Proxy::builder(connection).path(adapter_path)?.build().await?.set_powered(on).await
        }
        Command::StartDiscovery => {
            Adapter1Proxy::builder(connection).path(adapter_path)?.build().await?.start_discovery().await
        }
        Command::StopDiscovery => {
            Adapter1Proxy::builder(connection).path(adapter_path)?.build().await?.stop_discovery().await
        }
        Command::Connect(address) => {
            Device1Proxy::builder(connection).path(device_path(&address)?)?.build().await?.connect().await
        }
        Command::Disconnect(address) => {
            Device1Proxy::builder(connection).path(device_path(&address)?)?.build().await?.disconnect().await
        }
    }
}
