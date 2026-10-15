//! Network state from systemd-networkd (links) and iwd (Wi-Fi), the stack
//! this was written against. NetworkManager support can slot in beside it.

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
pub struct Link {
    pub name: String,
    /// `ethernet`, `wifi`, `loopback` or `other`.
    pub kind: String,
    /// networkd's operational state: `routable`, `carrier`, `no-carrier`, ...
    pub operational: String,
    pub carrier: bool,
    pub ipv4: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct WifiNetwork {
    pub name: String,
    /// `open`, `psk`, `8021x` or `wep`.
    pub security: String,
    /// 0..=100 derived from dBm.
    pub strength: u8,
    pub connected: bool,
    /// iwd knows the credentials.
    pub known: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Wifi {
    pub device: String,
    pub powered: bool,
    /// iwd station state: `connected`, `disconnected`, `connecting`, `roaming`...
    pub state: String,
    pub ssid: String,
    pub strength: u8,
    pub scanning: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct State {
    /// Any link is routable.
    pub connected: bool,
    /// What carries the connection: `wifi`, `ethernet`, `other` or `none`.
    pub primary: String,
    pub links: Vec<Link>,
    pub wifi: Option<Wifi>,
    pub networks: Vec<WifiNetwork>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Scan,
    ConnectWifi(String),
    DisconnectWifi,
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

#[proxy(
    interface = "org.freedesktop.network1.Manager",
    default_service = "org.freedesktop.network1",
    default_path = "/org/freedesktop/network1"
)]
trait NetworkdManager {
    fn list_links(&self) -> zbus::Result<Vec<(i32, String, OwnedObjectPath)>>;
}

#[proxy(interface = "org.freedesktop.network1.Link", default_service = "org.freedesktop.network1")]
trait NetworkdLink {
    #[zbus(property(emits_changed_signal = "false"))]
    fn operational_state(&self) -> zbus::Result<String>;
    #[zbus(property(emits_changed_signal = "false"))]
    fn carrier_state(&self) -> zbus::Result<String>;
}

#[proxy(interface = "net.connman.iwd.Station", default_service = "net.connman.iwd")]
trait Station {
    fn scan(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
    /// (network path, signal strength in 1/100 dBm)
    fn get_ordered_networks(&self) -> zbus::Result<Vec<(OwnedObjectPath, i16)>>;
}

#[proxy(interface = "net.connman.iwd.Network", default_service = "net.connman.iwd")]
trait IwdNetwork {
    fn connect(&self) -> zbus::Result<()>;
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
            Err(err) => tracing::warn!("network: {err}"),
        }
        tokio::time::sleep(delay).await;
        delay = crate::backoff(delay);
    }
}

fn string(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
        .unwrap_or_default()
}

fn boolean(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false)
}

fn dbm_to_percent(centi_dbm: i16) -> u8 {
    // -100 dBm and below is nothing, -50 dBm and above is full.
    let dbm = f32::from(centi_dbm) / 100.0;
    ((dbm + 100.0) * 2.0).clamp(0.0, 100.0) as u8
}

fn link_kind(name: &str) -> String {
    let sys = std::path::Path::new("/sys/class/net").join(name);
    if name == "lo" {
        "loopback".into()
    } else if sys.join("wireless").exists() || sys.join("phy80211").exists() {
        "wifi".into()
    } else if std::fs::read_to_string(sys.join("type")).map(|t| t.trim() == "1").unwrap_or(false)
        && sys.join("device").exists()
    {
        "ethernet".into()
    } else {
        "other".into()
    }
}

/// IPv4 addresses per interface, from getifaddrs.
fn ipv4_addresses() -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: plain libc call; the list is freed below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return out;
    }
    let mut cursor = list;
    while !cursor.is_null() {
        // SAFETY: cursor walks a list libc handed us and is checked for null.
        let entry = unsafe { &*cursor };
        if !entry.ifa_addr.is_null() {
            let addr = unsafe { &*entry.ifa_addr };
            if i32::from(addr.sa_family) == libc::AF_INET {
                let sin = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in) };
                let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
                    .to_string_lossy()
                    .into_owned();
                out.entry(name).or_default().push(ip.to_string());
            }
        }
        cursor = entry.ifa_next;
    }
    unsafe { libc::freeifaddrs(list) };
    out
}

/// Interface of the lowest-metric IPv4 default route, from /proc/net/route.
fn default_route_interface() -> Option<String> {
    let table = std::fs::read_to_string("/proc/net/route").ok()?;
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            // Iface Destination Gateway Flags RefCnt Use Metric ...
            if f.len() < 7 || f[1] != "00000000" {
                return None;
            }
            Some((f[6].parse::<u32>().unwrap_or(u32::MAX), f[0].to_string()))
        })
        .min()
        .map(|(_, iface)| iface)
}

async fn read_links(connection: &zbus::Connection) -> zbus::Result<Vec<Link>> {
    let manager = NetworkdManagerProxy::new(connection).await?;
    let addresses = ipv4_addresses();
    let mut links = Vec::new();
    for (_, name, path) in manager.list_links().await? {
        let link = NetworkdLinkProxy::builder(connection).path(path)?.build().await?;
        links.push(Link {
            kind: link_kind(&name),
            operational: link.operational_state().await.unwrap_or_default(),
            carrier: link.carrier_state().await.map(|c| c == "carrier").unwrap_or(false),
            ipv4: addresses.get(&name).cloned().unwrap_or_default(),
            name,
        });
    }
    links.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(links)
}

async fn read_wifi(connection: &zbus::Connection, objects: &Objects) -> (Option<Wifi>, Vec<WifiNetwork>) {
    let Some((station_path, station)) = objects
        .iter()
        .find_map(|(path, ifaces)| ifaces.get("net.connman.iwd.Station").map(|s| (path.clone(), s)))
    else {
        return (None, Vec::new());
    };
    let device = objects
        .get(&station_path)
        .and_then(|ifaces| ifaces.get("net.connman.iwd.Device"));
    let connected_network = station
        .get("ConnectedNetwork")
        .and_then(|v| OwnedObjectPath::try_from(v.clone()).ok());

    let mut strengths: HashMap<OwnedObjectPath, u8> = HashMap::new();
    if let Ok(proxy) = StationProxy::builder(connection).path(station_path.clone()).and_then(|b| Ok(b)) {
        if let Ok(proxy) = proxy.build().await
            && let Ok(ordered) = proxy.get_ordered_networks().await
        {
            for (path, signal) in ordered {
                strengths.insert(path, dbm_to_percent(signal));
            }
        }
    }

    let mut networks: Vec<WifiNetwork> = objects
        .iter()
        .filter_map(|(path, ifaces)| {
            let props = ifaces.get("net.connman.iwd.Network")?;
            Some(WifiNetwork {
                name: string(props, "Name"),
                security: string(props, "Type"),
                strength: strengths.get(path).copied().unwrap_or(0),
                connected: boolean(props, "Connected"),
                known: props.contains_key("KnownNetwork"),
            })
        })
        .collect();
    networks.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.strength.cmp(&a.strength)));

    let ssid = connected_network
        .as_ref()
        .and_then(|p| objects.get(p))
        .and_then(|ifaces| ifaces.get("net.connman.iwd.Network"))
        .map(|props| string(props, "Name"))
        .unwrap_or_default();
    let strength = connected_network
        .as_ref()
        .and_then(|p| strengths.get(p))
        .copied()
        .unwrap_or(0);

    let wifi = Wifi {
        device: device.map(|d| string(d, "Name")).unwrap_or_default(),
        powered: device.map(|d| boolean(d, "Powered")).unwrap_or(false),
        state: string(station, "State"),
        ssid,
        strength,
        scanning: boolean(station, "Scanning"),
    };
    (Some(wifi), networks)
}

async fn snapshot(connection: &zbus::Connection, iwd: Option<&ObjectManagerProxy<'_>>) -> zbus::Result<State> {
    let links = read_links(connection).await.unwrap_or_default();
    let (wifi, networks) = match iwd {
        Some(iwd) => match iwd.get_managed_objects().await {
            Ok(objects) => read_wifi(connection, &normalize(objects)).await,
            Err(_) => (None, Vec::new()),
        },
        None => (None, Vec::new()),
    };
    // The kernel's default route decides what actually carries traffic when
    // several links are up.
    let routable = |kind: &str| links.iter().any(|l| l.kind == kind && l.operational == "routable");
    let primary = if let Some(iface) = default_route_interface()
        && let Some(link) = links.iter().find(|l| l.name == iface)
    {
        link.kind.as_str()
    } else if routable("ethernet") {
        "ethernet"
    } else if routable("wifi") {
        "wifi"
    } else if links.iter().any(|l| l.kind == "other" && l.operational == "routable") {
        "other"
    } else {
        "none"
    };
    Ok(State {
        connected: primary != "none",
        primary: primary.into(),
        links,
        wifi,
        networks,
    })
}

async fn watch(
    updates: &mpsc::Sender<State>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let iwd = ObjectManagerProxy::builder(&connection)
        .destination("net.connman.iwd")?
        .path("/")?
        .build()
        .await
        .ok();
    let iwd = match iwd {
        Some(iwd) if iwd.get_managed_objects().await.is_ok() => Some(iwd),
        _ => {
            tracing::info!("network: iwd not available; Wi-Fi details disabled");
            None
        }
    };

    let changes = {
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .build();
        MessageStream::for_match_rule(rule, &connection, Some(32)).await?
    };
    // iwd's InterfacesAdded/Removed, as networks and BSSes come and go.
    let iwd_objects = {
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender("net.connman.iwd")?
            .path("/")?
            .interface("org.freedesktop.DBus.ObjectManager")?
            .build();
        MessageStream::for_match_rule(rule, &connection, Some(32)).await?
    };
    // Drain signals on their own task. zbus stops reading the socket once a
    // stream's queue is full, so a burst (iwd roaming between access points)
    // arriving while snapshot() waits on a method reply would wedge this
    // watcher for good: the reply sits unread behind the signals.
    let (wake_tx, mut wake) = mpsc::channel::<()>(1);
    let pump = tokio::spawn(async move {
        let mut signals = std::pin::pin!(futures_util::stream::select(changes, iwd_objects));
        while signals.next().await.is_some() {
            // Full means a wake-up is already pending; one snapshot covers both.
            let _ = wake_tx.try_send(());
        }
    });
    let result = watch_loop(&connection, iwd.as_ref(), updates, commands, &mut wake).await;
    pump.abort();
    result
}

async fn watch_loop(
    connection: &zbus::Connection,
    iwd: Option<&ObjectManagerProxy<'_>>,
    updates: &mpsc::Sender<State>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    wake: &mut mpsc::Receiver<()>,
) -> zbus::Result<()> {
    // Addresses and routes come from the kernel, which announces changes on
    // a netlink socket; no polling needed.
    let mut kernel = crate::netlink::RouteEvents::new()
        .map_err(|err| zbus::Error::Failure(format!("netlink route socket: {err}")))?;

    let mut last = None;
    loop {
        let state = snapshot(connection, iwd).await?;
        if last.as_ref() != Some(&state) {
            if updates.send(state.clone()).await.is_err() {
                return Ok(());
            }
            last = Some(state);
        }
        tokio::select! {
            s = wake.recv() => { if s.is_none() { return Err(zbus::Error::Failure("D-Bus signal stream ended".into())); } }
            changed = kernel.changed() => {
                changed.map_err(|err| zbus::Error::Failure(format!("netlink: {err}")))?;
                // Let the kernel finish a batch (link up, then addresses).
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                tracing::debug!("network: kernel reported a change");
            }
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()) };
                if let Err(err) = perform(connection, iwd, command.clone()).await {
                    tracing::warn!("network: {command:?} failed: {err}");
                }
            }
        }
    }
}

async fn perform(connection: &zbus::Connection, iwd: Option<&ObjectManagerProxy<'_>>, command: Command) -> zbus::Result<()> {
    let iwd = iwd.ok_or_else(|| zbus::Error::Failure("iwd not available".into()))?;
    let objects = normalize(iwd.get_managed_objects().await?);
    let station_path = objects
        .iter()
        .find(|(_, ifaces)| ifaces.contains_key("net.connman.iwd.Station"))
        .map(|(path, _)| path.clone())
        .ok_or_else(|| zbus::Error::Failure("no Wi-Fi station".into()))?;
    match command {
        Command::Scan => StationProxy::builder(connection).path(station_path)?.build().await?.scan().await,
        Command::DisconnectWifi => {
            StationProxy::builder(connection).path(station_path)?.build().await?.disconnect().await
        }
        Command::ConnectWifi(name) => {
            let path = objects
                .iter()
                .find(|(_, ifaces)| {
                    ifaces
                        .get("net.connman.iwd.Network")
                        .is_some_and(|props| string(props, "Name") == name)
                })
                .map(|(path, _)| path.clone())
                .ok_or_else(|| zbus::Error::Failure(format!("no network {name:?}")))?;
            IwdNetworkProxy::builder(connection).path(path)?.build().await?.connect().await
        }
    }
}
