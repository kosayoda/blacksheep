//! Battery state from UPower: the aggregate `DisplayDevice` plus every
//! individual battery, so machines with two packs (ThinkPad Power Bridge)
//! can show each one.

use std::collections::HashMap;

use futures_util::{StreamExt as _, stream::select_all};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use zbus::proxy;
use zbus::zvariant::OwnedObjectPath;

/// UPower device type for batteries.
const TYPE_BATTERY: u32 = 2;

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Battery {
    pub present: bool,
    /// 0..=100
    pub percentage: f64,
    /// `unknown`, `charging`, `discharging`, `empty`, `full`,
    /// `pending-charge` or `pending-discharge`.
    pub state: String,
    pub time_to_empty: i64,
    pub time_to_full: i64,
    pub icon_name: String,
}

/// One physical battery.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct BatteryDevice {
    /// D-Bus object path; stable identity while the battery is attached.
    pub path: String,
    /// Kernel name such as `BAT0` or `BAT1`.
    pub native_path: String,
    pub vendor: String,
    pub model: String,
    pub present: bool,
    pub percentage: f64,
    pub state: String,
    pub time_to_empty: i64,
    pub time_to_full: i64,
    /// Watt-hours.
    pub energy: f64,
    pub energy_full: f64,
    pub energy_full_design: f64,
    /// Watts, positive while charging or discharging.
    pub energy_rate: f64,
    /// Health as a percentage of design capacity.
    pub capacity: f64,
    pub icon_name: String,
}

#[derive(Debug, Clone)]
pub enum Update {
    Aggregate(Battery),
    /// Every attached battery, sorted by native path.
    Devices(Vec<BatteryDevice>),
}

#[proxy(
    interface = "org.freedesktop.UPower",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower"
)]
trait UPower {
    fn enumerate_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    #[zbus(signal)]
    fn device_added(&self, device: OwnedObjectPath) -> zbus::Result<()>;

    #[zbus(signal)]
    fn device_removed(&self, device: OwnedObjectPath) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.UPower.Device",
    default_service = "org.freedesktop.UPower",
    default_path = "/org/freedesktop/UPower/devices/DisplayDevice"
)]
trait Device {
    #[zbus(property, name = "Type")]
    fn device_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn native_path(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn vendor(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn model(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn percentage(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn time_to_empty(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn time_to_full(&self) -> zbus::Result<i64>;
    #[zbus(property)]
    fn is_present(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn energy(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn energy_full(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn energy_full_design(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn energy_rate(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn capacity(&self) -> zbus::Result<f64>;
}

fn state_name(state: u32) -> &'static str {
    match state {
        1 => "charging",
        2 => "discharging",
        3 => "empty",
        4 => "full",
        5 => "pending-charge",
        6 => "pending-discharge",
        _ => "unknown",
    }
}

async fn read_aggregate(device: &DeviceProxy<'_>) -> zbus::Result<Battery> {
    Ok(Battery {
        present: device.is_present().await?,
        percentage: device.percentage().await?,
        state: state_name(device.state().await?).to_string(),
        time_to_empty: device.time_to_empty().await?,
        time_to_full: device.time_to_full().await?,
        icon_name: device.icon_name().await?,
    })
}

async fn read_device(path: &str, device: &DeviceProxy<'_>) -> zbus::Result<BatteryDevice> {
    Ok(BatteryDevice {
        path: path.to_string(),
        native_path: device.native_path().await?,
        vendor: device.vendor().await.unwrap_or_default(),
        model: device.model().await.unwrap_or_default(),
        present: device.is_present().await?,
        percentage: device.percentage().await?,
        state: state_name(device.state().await?).to_string(),
        time_to_empty: device.time_to_empty().await?,
        time_to_full: device.time_to_full().await?,
        energy: device.energy().await.unwrap_or_default(),
        energy_full: device.energy_full().await.unwrap_or_default(),
        energy_full_design: device.energy_full_design().await.unwrap_or_default(),
        energy_rate: device.energy_rate().await.unwrap_or_default(),
        capacity: device.capacity().await.unwrap_or_default(),
        icon_name: device.icon_name().await.unwrap_or_default(),
    })
}

/// Streams that fire whenever anything we display about a device changes.
async fn changes<'a>(device: &'a DeviceProxy<'a>) -> impl futures_util::Stream<Item = ()> + 'a {
    select_all([
        device.receive_percentage_changed().await.map(|_| ()).boxed(),
        device.receive_state_changed().await.map(|_| ()).boxed(),
        device.receive_time_to_empty_changed().await.map(|_| ()).boxed(),
        device.receive_time_to_full_changed().await.map(|_| ()).boxed(),
        device.receive_is_present_changed().await.map(|_| ()).boxed(),
        device.receive_icon_name_changed().await.map(|_| ()).boxed(),
        device.receive_energy_changed().await.map(|_| ()).boxed(),
        device.receive_energy_rate_changed().await.map(|_| ()).boxed(),
    ])
}

/// Send the current state and every change until the receiver is dropped.
/// Reconnects if UPower or the system bus goes away.
pub async fn run(tx: mpsc::Sender<Update>) {
    let mut delay = crate::INITIAL_BACKOFF;
    loop {
        match watch(&tx).await {
            Ok(()) => return,
            Err(err) => tracing::warn!("upower: {err}"),
        }
        tokio::time::sleep(delay).await;
        delay = crate::backoff(delay);
    }
}

async fn watch(tx: &mpsc::Sender<Update>) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let aggregate = tokio::spawn(watch_aggregate(connection.clone(), tx.clone()));
    let devices = tokio::spawn(watch_devices(connection, tx.clone()));
    let result = tokio::select! {
        r = aggregate => r,
        r = devices => r,
    };
    match result {
        Ok(inner) => inner,
        Err(err) => Err(zbus::Error::Failure(format!("upower task failed: {err}"))),
    }
}

async fn watch_aggregate(connection: zbus::Connection, tx: mpsc::Sender<Update>) -> zbus::Result<()> {
    let device = DeviceProxy::new(&connection).await?;
    let mut changes = changes(&device).await;
    let mut last = None;
    loop {
        let battery = read_aggregate(&device).await?;
        if last.as_ref() != Some(&battery) {
            if tx.send(Update::Aggregate(battery.clone())).await.is_err() {
                return Ok(());
            }
            last = Some(battery);
        }
        if changes.next().await.is_none() {
            return Err(zbus::Error::Failure("property streams ended".into()));
        }
    }
}

enum Event {
    Changed(BatteryDevice),
    Removed(String),
}

async fn watch_devices(connection: zbus::Connection, tx: mpsc::Sender<Update>) -> zbus::Result<()> {
    let upower = UPowerProxy::new(&connection).await?;
    let mut added = upower.receive_device_added().await?;
    let mut removed = upower.receive_device_removed().await?;

    let (events_tx, mut events) = mpsc::channel::<Event>(32);
    let mut watchers: HashMap<String, AbortHandle> = HashMap::new();
    let mut devices: HashMap<String, BatteryDevice> = HashMap::new();

    let add = |path: String, watchers: &mut HashMap<String, AbortHandle>| {
        if watchers.contains_key(&path) {
            return;
        }
        let task = tokio::spawn(watch_device(connection.clone(), path.clone(), events_tx.clone()));
        watchers.insert(path, task.abort_handle());
    };

    let paths = upower.enumerate_devices().await?;
    tracing::debug!("upower: {} devices enumerated", paths.len());
    for path in paths {
        add(path.to_string(), &mut watchers);
    }
    // Machines without batteries never emit an event; publish the empty list.
    if send_snapshot(&tx, &devices).await.is_err() {
        return Ok(());
    }

    let result = loop {
        tokio::select! {
            signal = added.next() => {
                let Some(signal) = signal else {
                    break Err(zbus::Error::Failure("DeviceAdded stream ended".into()));
                };
                add(signal.args()?.device().to_string(), &mut watchers);
            }
            signal = removed.next() => {
                let Some(signal) = signal else {
                    break Err(zbus::Error::Failure("DeviceRemoved stream ended".into()));
                };
                let path = signal.args()?.device().to_string();
                if let Some(task) = watchers.remove(&path) {
                    task.abort();
                }
                if devices.remove(&path).is_some() && send_snapshot(&tx, &devices).await.is_err() {
                    break Ok(());
                }
            }
            event = events.recv() => {
                match event {
                    Some(Event::Changed(device)) => {
                        devices.insert(device.path.clone(), device);
                    }
                    Some(Event::Removed(path)) => {
                        watchers.remove(&path);
                        if devices.remove(&path).is_none() {
                            continue;
                        }
                    }
                    None => break Err(zbus::Error::Failure("device events ended".into())),
                }
                if send_snapshot(&tx, &devices).await.is_err() {
                    break Ok(());
                }
            }
        }
    };

    for task in watchers.values() {
        task.abort();
    }
    result
}

async fn send_snapshot(
    tx: &mpsc::Sender<Update>,
    devices: &HashMap<String, BatteryDevice>,
) -> Result<(), mpsc::error::SendError<Update>> {
    let mut snapshot: Vec<BatteryDevice> = devices.values().cloned().collect();
    snapshot.sort_by(|a, b| a.native_path.cmp(&b.native_path));
    tx.send(Update::Devices(snapshot)).await
}

/// Follow one device. Non-batteries (line power, peripherals) exit early.
async fn watch_device(connection: zbus::Connection, path: String, events: mpsc::Sender<Event>) {
    match watch_device_inner(&connection, &path, &events).await {
        Ok(()) => {}
        Err(err) => tracing::debug!("upower: {path}: {err}"),
    }
    let _ = events.send(Event::Removed(path)).await;
}

async fn watch_device_inner(
    connection: &zbus::Connection,
    path: &str,
    events: &mpsc::Sender<Event>,
) -> zbus::Result<()> {
    let device = DeviceProxy::builder(connection)
        .path(path.to_string())?
        .build()
        .await?;
    if device.device_type().await? != TYPE_BATTERY {
        return Ok(());
    }
    let mut changes = changes(&device).await;
    let mut last = None;
    loop {
        let snapshot = read_device(path, &device).await?;
        if last.as_ref() != Some(&snapshot) {
            if events.send(Event::Changed(snapshot.clone())).await.is_err() {
                return Ok(());
            }
            last = Some(snapshot);
        }
        if changes.next().await.is_none() {
            return Ok(());
        }
    }
}
