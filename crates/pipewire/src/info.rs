use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
};

use pipewire::spa::param::ParamType;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Pod, Property, PropertyFlags, Value, ValueArray};
use pipewire::spa::sys::{
    SPA_PARAM_ROUTE_device, SPA_PARAM_ROUTE_index, SPA_PARAM_ROUTE_props, SPA_PARAM_ROUTE_save,
    SPA_PARAM_Props, SPA_PARAM_Route, SPA_PROP_channelVolumes, SPA_PROP_mute, SPA_TYPE_OBJECT_ParamRoute,
    SPA_TYPE_OBJECT_Props,
};
use pipewire::{
    device::{Device, DeviceListener},
    metadata::{Metadata, MetadataListener},
    node::{Node, NodeListener},
    proxy::ProxyT,
};

/// One output route of a device: where hardware volume lives.
#[derive(Debug, Clone, Copy)]
struct Route {
    index: i32,
    volume: Volume,
    channels: usize,
}

use super::{Sink, Update, proxies::Proxies};

#[derive(Clone)]
pub struct Info(Rc<RefCell<Inner>>);

impl Info {
    pub fn new(tx: tokio::sync::mpsc::Sender<Update>) -> Self {
        Self(Rc::new(RefCell::new(Inner::new(tx))))
    }

    pub fn set_default_sink(&self, default_sink: String) {
        let mut inner = self.0.borrow_mut();
        if inner
            .default_sink
            .as_ref()
            .is_some_and(|sink| *sink == default_sink)
        {
            return;
        }
        tracing::debug!("default sink: {default_sink}");
        inner.default_sink = Some(default_sink);
        Self::sync(&mut inner);
    }

    /// Remember the human names of a node so it can be described, and the
    /// device route (device id, route device index) carrying its volume.
    pub fn set_node_meta(
        &self,
        name: String,
        nick: String,
        description: String,
        is_sink: bool,
        route: Option<(u32, u32)>,
    ) {
        let mut inner = self.0.borrow_mut();
        inner.meta.insert(name.clone(), (nick, description));
        if let Some(route) = route {
            inner.node_routes.insert(name.clone(), route);
        }
        if is_sink && !inner.sinks.contains(&name) {
            inner.sinks.push(name);
        }
        Self::sync(&mut inner);
    }

    /// The device route carrying a node's volume, learned from node info.
    pub fn set_node_route(&self, name: String, route: (u32, u32)) {
        let mut inner = self.0.borrow_mut();
        if inner.node_routes.get(&name) == Some(&route) {
            return;
        }
        tracing::debug!("node {name} uses route {}/{}", route.0, route.1);
        inner.node_routes.insert(name, route);
        Self::sync(&mut inner);
    }

    pub fn track_device(&self, device_id: u32, device: Device, listener: DeviceListener) {
        let proxy_id = self.track_proxy(&device);
        let mut inner = self.0.borrow_mut();
        inner.device_ids.insert(proxy_id, device_id);
        inner.devices.insert(device_id, device);
        inner.proxies.add_listener(proxy_id, listener);
    }

    pub fn update_route(&self, device_id: u32, route_device: u32, index: i32, volume: f32, channels: usize, muted: bool) {
        tracing::debug!("route {device_id}/{route_device} (index {index}): {volume} muted={muted}");
        let mut inner = self.0.borrow_mut();
        inner.routes.insert(
            (device_id, route_device),
            Route {
                index,
                volume: Volume::new(volume, muted),
                channels: channels.max(1),
            },
        );
        Self::sync(&mut inner);
    }

    pub fn track_node(&self, name: String, node: Node, listener: NodeListener) {
        let proxy_id = self.track_proxy(&node);
        let mut inner = self.0.borrow_mut();
        inner.node_ids.insert(proxy_id, name.clone());
        inner.nodes.insert(name, node);
        inner.proxies.add_listener(proxy_id, listener);
        Self::sync(&mut inner);
    }

    pub fn track_metadata(&self, metadata: Metadata, listener: MetadataListener) {
        let proxy_id = self.track_proxy(&metadata);
        let mut inner = self.0.borrow_mut();
        inner.proxies.add_listener(proxy_id, listener);
        inner.metadata = Some(metadata);
    }

    pub fn update_volume(&self, node: String, volume: f32, channels: usize, muted: bool) {
        tracing::debug!("volume of {node}: {volume} muted={muted}");
        let mut inner = self.0.borrow_mut();
        let value = Volume::new(volume, muted);
        inner.volumes.insert(node.clone(), value);
        inner.channels.insert(node.clone(), channels.max(1));
        // A node with a hardware route mirrors the route's volume, and the
        // node is the object that reliably reports changes: devices stay
        // silent after a route is written. But the mirror can be stale (a
        // node still carrying a value written before the route took over),
        // so rather than copying the node's value into the route, ask the
        // device for its routes again and let the answer win.
        if let Some(key) = inner.node_routes.get(&node).copied()
            && let Some(device) = inner.devices.get(&key.0)
        {
            use pipewire::spa::param::ParamType;
            device.enum_params(0, Some(ParamType::Route), 0, u32::MAX);
        }
        Self::sync(&mut inner);
    }

    fn removed(&self, proxy_id: u32) {
        let mut inner = self.0.borrow_mut();
        inner.proxies.remove_proxy(proxy_id);
        if let Some(device_id) = inner.device_ids.remove(&proxy_id) {
            inner.devices.remove(&device_id);
            inner.routes.retain(|(d, _), _| *d != device_id);
            Self::sync(&mut inner);
        }
        if let Some(name) = inner.node_ids.remove(&proxy_id) {
            inner.nodes.remove(&name);
            inner.volumes.remove(&name);
            inner.channels.remove(&name);
            inner.meta.remove(&name);
            inner.node_routes.remove(&name);
            inner.sinks.retain(|s| *s != name);
            Self::sync(&mut inner);
        }
    }

    /// Where a node's volume is set and read: its device route when it has
    /// one, else the node itself.
    fn route_of(inner: &Inner, name: &str) -> Option<((u32, u32), Route)> {
        let key = *inner.node_routes.get(name)?;
        inner.routes.get(&key).map(|route| (key, *route))
    }

    fn volume_of(inner: &Inner, name: &str) -> Option<Volume> {
        Self::route_of(inner, name)
            .map(|(_, route)| route.volume)
            .or_else(|| inner.volumes.get(name).copied())
    }

    // ---- commands, run on the PipeWire thread

    pub fn set_volume(&self, node: Option<String>, percent: u8) {
        let gain = (f32::from(percent.min(100)) / 100.0).powi(3);
        self.set_prop(node, |channels| {
            (SPA_PROP_channelVolumes, Value::ValueArray(ValueArray::Float(vec![gain; channels])))
        });
    }

    pub fn set_muted(&self, node: Option<String>, muted: bool) {
        self.set_prop(node, |_| (SPA_PROP_mute, Value::Bool(muted)));
    }

    /// Apply one Props property, through the device route when the node has
    /// one (as wpctl does, so hardware and software volume stay in step),
    /// else directly on the node.
    fn set_prop(&self, node: Option<String>, prop: impl FnOnce(usize) -> (u32, Value)) {
        let routed = {
            let inner = self.0.borrow();
            let Some(name) = node.or_else(|| inner.default_sink.clone()) else {
                return;
            };
            match Self::route_of(&inner, &name) {
                Some((key, route)) if inner.devices.contains_key(&key.0) => {
                    let (prop_key, value) = prop(route.channels);
                    set_route_props(&inner.devices[&key.0], route.index, key.1, prop_key, value.clone());
                    Some((key, prop_key, value))
                }
                _ => {
                    let Some(proxy) = inner.nodes.get(&name) else {
                        tracing::warn!("pipewire: no node {name} to change");
                        return;
                    };
                    let channels = inner.channels.get(&name).copied().unwrap_or(2);
                    let (prop_key, value) = prop(channels);
                    set_props(proxy, prop_key, value);
                    None
                }
            }
        };

        // Reflect the change now rather than waiting for the device to
        // report it back.
        if let Some((key, prop_key, value)) = routed {
            let mut inner = self.0.borrow_mut();
            if let Some(route) = inner.routes.get_mut(&key) {
                match value {
                    Value::ValueArray(ValueArray::Float(gains)) if prop_key == SPA_PROP_channelVolumes => {
                        if let Some(gain) = gains.first() {
                            route.volume = Volume::new(*gain, route.volume.muted);
                        }
                    }
                    Value::Bool(muted) if prop_key == SPA_PROP_mute => route.volume.muted = muted,
                    _ => {}
                }
            }
            Self::sync(&mut inner);
        }
    }

    pub fn set_default_sink_name(&self, name: &str) {
        let inner = self.0.borrow();
        let Some(metadata) = &inner.metadata else {
            tracing::warn!("pipewire: no default metadata object; cannot change default sink");
            return;
        };
        // The configured key is what wpctl sets; the session manager then
        // updates default.audio.sink and remembers the choice.
        let value = serde_json::json!({ "name": name }).to_string();
        metadata.set_property(0, "default.configured.audio.sink", Some("Spa:String:JSON"), Some(&value));
    }

    fn sync(inner: &mut Inner) {
        let describe = |inner: &Inner, name: &str| {
            let (nick, description) = inner.meta.get(name).cloned().unwrap_or_default();
            let volume = Self::volume_of(inner, name);
            Sink {
                name: name.to_string(),
                nick,
                description,
                is_default: inner.default_sink.as_deref() == Some(name),
                volume: volume.map(|v| v.value).unwrap_or(0),
                muted: volume.map(|v| v.muted).unwrap_or(false),
            }
        };

        if let Some(name) = inner.default_sink.clone() {
            let sink = describe(inner, &name);
            if inner.sink.as_ref() != Some(&sink) {
                inner.sink = Some(sink.clone());
                let _ = inner.tx.blocking_send(Update::DefaultSink(sink));
            }
        }

        let mut sinks: Vec<Sink> = inner.sinks.iter().map(|name| describe(inner, name)).collect();
        sinks.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.nick.cmp(&b.nick)));
        if sinks != inner.sent_sinks {
            inner.sent_sinks = sinks.clone();
            let _ = inner.tx.blocking_send(Update::Sinks(sinks));
        }

        let volume = inner
            .default_sink
            .as_deref()
            .and_then(|sink| Self::volume_of(inner, sink));
        if volume != inner.volume {
            tracing::debug!("default sink volume: {volume:?}");
            inner.volume = volume;

            if let Some(volume) = volume {
                let _ = inner.tx.blocking_send(Update::VolumeChanged(volume));
            }
        }
    }

    pub fn downgrade(&self) -> InfoWeak {
        InfoWeak(Rc::downgrade(&self.0))
    }

    fn track_proxy(&self, proxy: &impl ProxyT) -> u32 {
        let proxy = proxy.upcast_ref();
        let proxy_id = proxy.id();

        let weak = self.downgrade();
        let listener = proxy
            .add_listener_local()
            .removed(move || {
                if let Some(info) = weak.upgrade() {
                    info.removed(proxy_id);
                }
            })
            .register();

        self.0.borrow_mut().proxies.add_listener(proxy_id, listener);
        proxy_id
    }
}

fn property(key: u32, value: Value) -> Property {
    Property {
        key,
        flags: PropertyFlags::empty(),
        value,
    }
}

fn serialise(value: &Value) -> Option<Vec<u8>> {
    match PodSerializer::serialize(std::io::Cursor::new(Vec::new()), value) {
        Ok((cursor, _)) => Some(cursor.into_inner()),
        Err(err) => {
            tracing::warn!("pipewire: cannot serialise param: {err:?}");
            None
        }
    }
}

/// Send one Props property straight to a node.
fn set_props(node: &Node, key: u32, value: Value) {
    let object = Value::Object(Object {
        type_: SPA_TYPE_OBJECT_Props,
        id: SPA_PARAM_Props,
        properties: vec![property(key, value)],
    });
    let Some(bytes) = serialise(&object) else { return };
    let Some(pod) = Pod::from_bytes(&bytes) else { return };
    node.set_param(ParamType::Props, 0, pod);
}

/// Send one Props property to a device route, saved so the session manager
/// remembers it across restarts, the way `wpctl set-volume` does.
fn set_route_props(device: &Device, index: i32, route_device: u32, key: u32, value: Value) {
    let props = Object {
        type_: SPA_TYPE_OBJECT_Props,
        id: SPA_PARAM_Route,
        properties: vec![property(key, value)],
    };
    let route = Value::Object(Object {
        type_: SPA_TYPE_OBJECT_ParamRoute,
        id: SPA_PARAM_Route,
        properties: vec![
            property(SPA_PARAM_ROUTE_index, Value::Int(index)),
            property(SPA_PARAM_ROUTE_device, Value::Int(route_device as i32)),
            property(SPA_PARAM_ROUTE_props, Value::Object(props)),
            property(SPA_PARAM_ROUTE_save, Value::Bool(true)),
        ],
    });
    let Some(bytes) = serialise(&route) else { return };
    let Some(pod) = Pod::from_bytes(&bytes) else { return };
    device.set_param(ParamType::Route, 0, pod);
}

#[derive(Default, Clone)]
pub struct InfoWeak(Weak<RefCell<Inner>>);

impl InfoWeak {
    pub fn upgrade(&self) -> Option<Info> {
        self.0.upgrade().map(Info)
    }
}

struct Inner {
    default_sink: Option<String>,
    volume: Option<Volume>,
    sink: Option<Sink>,
    sent_sinks: Vec<Sink>,

    proxies: Proxies,
    nodes: HashMap<String, Node>,
    node_ids: HashMap<u32, String>,
    devices: HashMap<u32, Device>,
    device_ids: HashMap<u32, u32>,
    /// (device id, route device index) -> route, output direction only.
    routes: HashMap<(u32, u32), Route>,
    /// node name -> (device id, route device index)
    node_routes: HashMap<String, (u32, u32)>,
    metadata: Option<Metadata>,
    /// Node names of every `Audio/Sink`.
    sinks: Vec<String>,
    volumes: HashMap<String, Volume>,
    channels: HashMap<String, usize>,
    meta: HashMap<String, (String, String)>,

    tx: tokio::sync::mpsc::Sender<super::Update>,
}

impl Inner {
    pub fn new(tx: tokio::sync::mpsc::Sender<super::Update>) -> Self {
        Inner {
            default_sink: None,
            volume: None,
            sink: None,
            sent_sinks: Vec::new(),
            proxies: Proxies::default(),
            nodes: HashMap::default(),
            node_ids: HashMap::default(),
            devices: HashMap::default(),
            device_ids: HashMap::default(),
            routes: HashMap::default(),
            node_routes: HashMap::default(),
            metadata: None,
            sinks: Vec::new(),
            volumes: HashMap::default(),
            channels: HashMap::default(),
            meta: HashMap::default(),
            tx,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    pub value: u8,
    pub muted: bool,
}

impl Volume {
    pub fn new(linear_gain: f32, muted: bool) -> Self {
        Self {
            value: (linear_gain.cbrt() * 100.0).round() as u8,
            muted,
        }
    }
}
