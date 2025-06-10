use pipewire::context::ContextRc;
use pipewire::device::Device;
use pipewire::main_loop::MainLoopRc;
use pipewire::metadata::Metadata;
use pipewire::node::Node;
use pipewire::registry::{GlobalObject, Registry};
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::deserialize::PodDeserializer;
use pipewire::spa::sys::{
    SPA_DIRECTION_OUTPUT, SPA_PARAM_ROUTE_device, SPA_PARAM_ROUTE_direction, SPA_PARAM_ROUTE_index,
    SPA_PARAM_ROUTE_props, SPA_PROP_channelVolumes, SPA_PROP_mute,
};
use pipewire::spa::utils::dict::DictRef;
use pipewire::types::ObjectType;

mod info;
mod proxies;
mod update;

use info::Info;
pub use update::{Command, Sink, Update};

pub struct Pipewire;

impl Pipewire {
    pub fn connect(
        tx: tokio::sync::mpsc::Sender<Update>,
        commands: pipewire::channel::Receiver<Command>,
    ) -> color_eyre::Result<()> {
        let main_loop = MainLoopRc::new(None)?;
        let info = Info::new(tx);

        let _receiver = commands.attach(main_loop.loop_(), {
            let main_loop = main_loop.clone();
            let info = info.clone();
            move |command| match command {
                Command::Terminate => main_loop.quit(),
                Command::SetVolume { node, percent } => info.set_volume(node, percent),
                Command::SetMuted { node, muted } => info.set_muted(node, muted),
                Command::SetDefaultSink(name) => info.set_default_sink_name(&name),
            }
        });

        let context = ContextRc::new(&main_loop, None)?;
        let core = context.connect_rc(None)?;
        let registry = core.get_registry_rc()?;

        // Initialize core error listener
        let _listener = {
            let weak = main_loop.downgrade();
            core.add_listener_local()
                .error(move |id, seq, res, message| {
                    if id == 0 {
                        // The core itself failed; the reconnect loop takes over.
                        tracing::error!(?seq, ?res, ?message, "pipewire core error");
                        if let Some(main_loop) = weak.upgrade() {
                            main_loop.quit();
                        }
                    } else {
                        tracing::warn!(?id, ?res, ?message, "pipewire object error");
                    }
                })
                .register()
        };

        // Initialize proxies and listeners
        let _listener = {
            let weak = registry.downgrade();
            let info = info.clone();

            registry
                .add_listener_local()
                .global(move |obj| {
                    let Some(registry) = weak.upgrade() else {
                        return;
                    };

                    match obj.type_ {
                        ObjectType::Metadata => {
                            Self::handle_metadata(&info, &registry, obj);
                        }
                        ObjectType::Node => {
                            Self::handle_node(&info, &registry, obj);
                        }
                        ObjectType::Device => {
                            Self::handle_device(&info, &registry, obj);
                        }
                        _ => (),
                    }
                })
                .register()
        };

        // Start main loop
        main_loop.run();

        Ok(())
    }

    fn handle_metadata(info: &Info, registry: &Registry, obj: &GlobalObject<&DictRef>) {
        let metadata: Metadata = match registry.bind(obj) {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::warn!("pipewire: cannot bind metadata {}: {err}", obj.id);
                return;
            }
        };
        let metadata_name = obj.props.and_then(|props| props.get("metadata.name"));
        tracing::debug!("metadata object {}: {metadata_name:?}", obj.id);
        if metadata_name != Some("default") {
            return;
        }

        let listener = {
            let info = info.downgrade();
            metadata
                .add_listener_local()
                .property(move |_, key, ty, value| {
                    if key != Some("default.audio.sink") || ty != Some("Spa:String:JSON") {
                        return 0;
                    }

                    let Some(json) = value else {
                        return 0;
                    };

                    let Ok(json) = serde_json::from_str::<serde_json::Value>(json) else {
                        tracing::warn!("pipewire: default sink metadata is not JSON: {json}");
                        return 0;
                    };
                    let serde_json::Value::Object(obj) = json else {
                        return 0;
                    };
                    let Some(name) = obj
                        .get("name")
                        .and_then(|name| name.as_str())
                        .map(|name| name.to_string())
                    else {
                        return 0;
                    };

                    if let Some(info) = info.upgrade() {
                        info.set_default_sink(name);
                    }

                    0
                })
                .register()
        };
        info.track_metadata(metadata, listener);
    }

    /// Follow a device's output routes: their volume is the hardware level
    /// the session manager exposes through wpctl.
    fn handle_device(info: &Info, registry: &Registry, obj: &GlobalObject<&DictRef>) {
        // Only sound cards have routes; cameras and the like reject the query.
        if obj.props.and_then(|p| p.get("media.class")) != Some("Audio/Device") {
            return;
        }
        let device: Device = match registry.bind(obj) {
            Ok(device) => device,
            Err(err) => {
                tracing::warn!("pipewire: cannot bind device {}: {err}", obj.id);
                return;
            }
        };
        let device_id = obj.id;
        let listener = {
            let info = info.downgrade();
            device
                .add_listener_local()
                .param(move |_, id, index, _, param| {
                    use pipewire::spa::pod::{Value, ValueArray};

                    tracing::debug!("device {device_id} param {id:?} #{index}: {}", param.is_some());
                    if id != ParamType::Route {
                        return;
                    }
                    let Some(pod) = param else { return };
                    let Ok((_, Value::Object(route))) =
                        PodDeserializer::deserialize_any_from(pod.as_bytes())
                    else {
                        return;
                    };
                    let mut index = None;
                    let mut route_device = None;
                    let mut output = false;
                    let mut volume = None;
                    let mut channels = 0;
                    let mut muted = None;
                    for property in route.properties {
                        match (property.key, property.value) {
                            (k, Value::Int(v)) if k == SPA_PARAM_ROUTE_index => index = Some(v),
                            (k, Value::Int(v)) if k == SPA_PARAM_ROUTE_device => route_device = Some(v as u32),
                            (k, Value::Id(v)) if k == SPA_PARAM_ROUTE_direction => {
                                output = v.0 == SPA_DIRECTION_OUTPUT;
                            }
                            (k, Value::Object(props)) if k == SPA_PARAM_ROUTE_props => {
                                for p in props.properties {
                                    match (p.key, p.value) {
                                        (pk, Value::ValueArray(ValueArray::Float(v)))
                                            if pk == SPA_PROP_channelVolumes =>
                                        {
                                            volume = v.first().copied();
                                            channels = v.len();
                                        }
                                        (pk, Value::Bool(m)) if pk == SPA_PROP_mute => muted = Some(m),
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    if let (Some(index), Some(route_device), Some(volume), Some(muted), Some(info)) =
                        (index, route_device, volume, muted, info.upgrade())
                        && output
                    {
                        info.update_route(device_id, route_device, index, volume, channels, muted);
                    }
                })
                .register()
        };
        device.subscribe_params(&[ParamType::Route]);
        info.track_device(device_id, device, listener);
    }

    fn handle_node(info: &Info, registry: &Registry, obj: &GlobalObject<&DictRef>) {
        let node: Node = match registry.bind(obj) {
            Ok(node) => node,
            Err(err) => {
                tracing::warn!("pipewire: cannot bind node {}: {err}", obj.id);
                return;
            }
        };
        let Some(props) = obj.props else {
            return;
        };
        let Some(node_name) = props
            .get("device.id")
            .is_some()
            .then(|| props.get("node.name"))
            .flatten()
        else {
            tracing::debug!("skipping node {} ({:?})", obj.id, props.get("node.name"));
            return;
        };
        tracing::debug!("tracking node {}: {node_name}", obj.id);
        // Nodes backed by an ALSA card carry hardware volume on the device's
        // route; remember which route so volume changes go there.
        let route = props
            .get("device.id")
            .and_then(|d| d.parse::<u32>().ok())
            .zip(props.get("card.profile.device").and_then(|d| d.parse::<u32>().ok()));
        info.set_node_meta(
            node_name.to_string(),
            props.get("node.nick").unwrap_or_default().to_string(),
            props
                .get("node.description")
                .unwrap_or_default()
                .to_string(),
            props.get("media.class") == Some("Audio/Sink"),
            route,
        );

        let listener = {
            let node_name = node_name.to_string();
            let info = info.downgrade();
            let info_for_props = info.clone();
            let name_for_props = node_name.clone();
            node.add_listener_local()
                // The registry announces only a few properties; the route
                // index (`card.profile.device`) is in the node's full set.
                .info(move |node_info| {
                    let Some(props) = node_info.props() else { return };
                    let route = props
                        .get("device.id")
                        .and_then(|d| d.parse::<u32>().ok())
                        .zip(props.get("card.profile.device").and_then(|d| d.parse::<u32>().ok()));
                    if let (Some(route), Some(info)) = (route, info_for_props.upgrade()) {
                        info.set_node_route(name_for_props.clone(), route);
                    }
                })
                .param(move |_, _, _, _, param| {
                    use pipewire::spa::pod::{Value, ValueArray};

                    let Some(pod) = param else {
                        tracing::debug!("param callback without pod");
                        return;
                    };
                    let Ok((_, object)) = PodDeserializer::deserialize_any_from(pod.as_bytes())
                    else {
                        tracing::warn!("pipewire: cannot deserialise Props param");
                        return;
                    };
                    let Value::Object(object) = object else {
                        return;
                    };

                    let mut volume = None;
                    let mut channels = 0;
                    let mut muted = None;

                    for property in object.properties {
                        if property.key == SPA_PROP_channelVolumes
                            && let Value::ValueArray(ValueArray::Float(value)) = &property.value
                        {
                            // NOTE: We're using the left channel here
                            volume = value.first().copied();
                            channels = value.len();
                        }

                        if property.key == SPA_PROP_mute
                            && let Value::Bool(value) = &property.value
                        {
                            muted = Some(*value);
                        }

                        if volume.is_some() && muted.is_some() {
                            break;
                        }
                    }

                    if let Some(info) = info.upgrade()
                        && let Some(volume) = volume
                        && let Some(muted) = muted
                    {
                        info.update_volume(node_name.clone(), volume, channels, muted);
                    }
                })
                .register()
        };
        node.subscribe_params(&[ParamType::Props]);
        info.track_node(node_name.to_string(), node, listener);
    }
}
