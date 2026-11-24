//! Turns other apps' audio down while we speak, by scaling their
//! `softVolumes`. The volume mixers show, and WirePlumber saves per app, is
//! `channelVolumes`, so a volume the user set is never overwritten. (The two
//! usually match for streams, but not always, so we scale soft, not channel.)

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use pipewire as pw;
use pw::node::{Node, NodeListener};
use pw::registry::RegistryRc;
use pw::spa;
use pw::types::ObjectType;
use spa::param::ParamType;
use spa::pod::{Pod, Value, ValueArray, deserialize::PodDeserializer, serialize::PodSerializer};

struct Stream {
    node: Node,
    _listener: NodeListener,
    /// `softVolumes` as the stream had them before we ducked it.
    soft: Option<Vec<f32>>,
    /// What we set while ducked, to tell our change from someone else's.
    ducked_to: Option<Vec<f32>>,
}

type Streams = Rc<RefCell<HashMap<u32, Stream>>>;

pub(crate) struct Ducker {
    streams: Streams,
    ducked: Rc<Cell<bool>>,
    level: f32,
    _registry: RegistryRc,
    _listener: pw::registry::Listener,
}

impl Ducker {
    /// Tracks every audio output stream except our own (`vivus-*` nodes). `level`
    /// is their volume while ducked, relative to what the user set.
    pub fn new(core: &pw::core::CoreRc, level: f32) -> Result<Self, pw::Error> {
        let registry = core.get_registry_rc()?;
        let streams: Streams = Rc::default();
        let ducked = Rc::new(Cell::new(false));

        let listener = registry
            .add_listener_local()
            .global({
                let registry = registry.downgrade();
                let (streams, ducked) = (streams.clone(), ducked.clone());
                move |global| {
                    let Some(props) = global.props else { return };
                    if global.type_ != ObjectType::Node
                        || props.get("media.class") != Some("Stream/Output/Audio")
                        // Our own nodes (registry props carry no process id).
                        || props.get("node.name").is_some_and(|n| n.starts_with("vivus-"))
                    {
                        return;
                    }
                    let Some(registry) = registry.upgrade() else { return };
                    let node: Node = match registry.bind(global) {
                        Ok(n) => n,
                        Err(e) => return tracing::debug!("duck: bind node {}: {e}", global.id),
                    };
                    let id = global.id;
                    let listener = node
                        .add_listener_local()
                        .param({
                            let (streams, ducked) = (streams.clone(), ducked.clone());
                            move |_, _, _, _, pod| {
                                if let Some(pod) = pod {
                                    on_props(&streams, id, pod, ducked.get(), level);
                                }
                            }
                        })
                        .register();
                    node.subscribe_params(&[ParamType::Props]);
                    streams.borrow_mut().insert(
                        id,
                        Stream {
                            node,
                            _listener: listener,
                            soft: None,
                            ducked_to: None,
                        },
                    );
                }
            })
            .global_remove({
                let streams = streams.clone();
                move |id| {
                    streams.borrow_mut().remove(&id);
                }
            })
            .register();

        Ok(Self {
            streams,
            ducked,
            level,
            _registry: registry,
            _listener: listener,
        })
    }

    pub fn set(&self, ducked: bool) {
        if self.ducked.replace(ducked) == ducked {
            return;
        }
        tracing::debug!("duck: {ducked} ({} streams)", self.streams.borrow().len());
        for stream in self.streams.borrow_mut().values_mut() {
            match (ducked, &stream.soft) {
                (true, Some(soft)) => duck(stream, soft.clone(), self.level),
                (false, Some(soft)) if stream.ducked_to.take().is_some() => {
                    set_soft_volumes(&stream.node, soft.clone());
                }
                _ => {}
            }
        }
    }
}

impl Drop for Ducker {
    fn drop(&mut self) {
        self.set(false);
    }
}

fn duck(stream: &mut Stream, soft: Vec<f32>, level: f32) {
    let ducked: Vec<f32> = soft.iter().map(|v| v * level).collect();
    set_soft_volumes(&stream.node, ducked.clone());
    stream.ducked_to = Some(ducked);
}

/// A stream's `Props` (also sent on subscribing). Soft volumes that aren't
/// ours while ducked mean a new stream, or the user changed its volume
/// (which resets soft): take them as its own and duck again.
fn on_props(streams: &Streams, id: u32, pod: &Pod, ducked: bool, level: f32) {
    let Some(soft) = parse_soft_volumes(pod) else { return };
    let Ok(mut streams) = streams.try_borrow_mut() else { return };
    let Some(stream) = streams.get_mut(&id) else { return };
    let ours = stream.ducked_to.as_ref().is_some_and(|d| same(d, &soft));
    if ours || stream.soft.as_ref().is_some_and(|s| same(s, &soft)) {
        return;
    }
    stream.soft = Some(soft.clone());
    if ducked {
        duck(stream, soft, level);
    }
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

/// `softVolumes` from a Props param, if it has any.
fn parse_soft_volumes(pod: &Pod) -> Option<Vec<f32>> {
    let (_, Value::Object(obj)) = PodDeserializer::deserialize_any_from(pod.as_bytes()).ok()? else {
        return None;
    };
    let prop = obj.properties.iter().find(|p| p.key == spa::sys::SPA_PROP_softVolumes)?;
    match &prop.value {
        Value::ValueArray(ValueArray::Float(v)) if !v.is_empty() => Some(v.clone()),
        _ => None,
    }
}

fn set_soft_volumes(node: &Node, volumes: Vec<f32>) {
    let value = Value::Object(spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![spa::pod::Property {
            key: spa::sys::SPA_PROP_softVolumes,
            flags: spa::pod::PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(volumes)),
        }],
    });
    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value)
        .expect("serialize props")
        .0
        .into_inner();
    node.set_param(ParamType::Props, 0, Pod::from_bytes(&bytes).expect("props pod"));
}
