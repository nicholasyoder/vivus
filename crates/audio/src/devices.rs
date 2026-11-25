//! Audio sources and sinks from the PipeWire registry, with the system
//! defaults from the `default` metadata, reported whenever either changes.
//! Also each source's volume (the mic gain), read from and set on PipeWire.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use pipewire as pw;
use pw::device::{Device as PwDevice, DeviceListener};
use pw::metadata::{Metadata, MetadataListener};
use pw::node::{Node, NodeChangeMask, NodeListener};
use pw::spa;
use pw::types::ObjectType;
use spa::param::ParamType;
use spa::pod::{Pod, Value, ValueArray, deserialize::PodDeserializer, serialize::PodSerializer};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Source,
    Sink,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Sink => "sink",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// `node.name`, what `audio.input` / `audio.output` hold.
    pub name: String,
    pub description: String,
    pub direction: Direction,
    pub is_default: bool,
}

/// A source node, watched for its volume.
struct Source {
    node: Node,
    _listener: NodeListener,
    /// Length of its `channelVolumes`; 0 until its Props arrive.
    channels: usize,
    /// Its card's global id and `card.profile.device`, for hardware volume.
    card: Option<(u32, i32)>,
    /// Last reported, as mixers show it (cube root of the linear volume).
    volume: Option<f32>,
}

/// An `Audio/Device` (sound card). Its active routes carry the hardware volume.
struct Card {
    device: PwDevice,
    _listener: DeviceListener,
    /// Input route index by profile device (`card.profile.device`).
    routes: HashMap<i32, i32>,
}

#[derive(Default)]
struct State {
    /// Global id → (name, description, direction).
    nodes: BTreeMap<u32, (String, String, Direction)>,
    sources: HashMap<u32, Source>,
    cards: HashMap<u32, Card>,
    default_source: Option<String>,
    default_sink: Option<String>,
    /// Last list reported, so unrelated registry churn reports nothing.
    reported: Option<Vec<Device>>,
}

impl State {
    fn list(&self) -> Vec<Device> {
        let mut out: Vec<Device> = self
            .nodes
            .values()
            .map(|(name, description, direction)| Device {
                name: name.clone(),
                description: description.clone(),
                direction: *direction,
                is_default: match direction {
                    Direction::Source => self.default_source.as_ref() == Some(name),
                    Direction::Sink => self.default_sink.as_ref() == Some(name),
                },
            })
            .collect();
        out.sort_by(|a, b| {
            (a.direction as u8, &a.description, &a.name).cmp(&(
                b.direction as u8,
                &b.description,
                &b.name,
            ))
        });
        out
    }
}

type OnVolume = Rc<dyn Fn(&str, f32)>;

enum Msg {
    Quit,
    SetVolume(String, f32),
}

/// Watches devices on its own PipeWire thread until dropped.
pub struct Devices {
    tx: pw::channel::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Devices {
    /// `on_change` gets the full list once the initial registry scan is done
    /// and again after every change. `on_volume` gets a source's node name and
    /// volume (0 to 1, as mixers show it) when first seen and on every change,
    /// whoever made it. Both run on the PipeWire thread.
    pub fn spawn(
        on_change: impl Fn(Vec<Device>) + Send + 'static,
        on_volume: impl Fn(&str, f32) + Send + 'static,
    ) -> Result<Self, Error> {
        let (tx, rx) = pw::channel::channel::<Msg>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), Error>>();
        let thread = std::thread::Builder::new()
            .name("vivus-devices".into())
            .spawn(move || {
                if let Err(e) = run(on_change, on_volume, rx, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .expect("spawn devices thread");
        ready_rx.recv().map_err(|_| Error::ThreadDied)??;
        Ok(Self {
            tx,
            thread: Some(thread),
        })
    }

    /// Sets a source's volume (0 to 1, as mixers show it). A sound card's is
    /// set on its route, like a mixer does, so WirePlumber saves it and the
    /// hardware gain changes; others get node Props.
    pub fn set_volume(&self, name: &str, volume: f32) {
        let _ = self.tx.send(Msg::SetVolume(name.into(), volume));
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    on_change: impl Fn(Vec<Device>) + 'static,
    on_volume: impl Fn(&str, f32) + 'static,
    msgs: pw::channel::Receiver<Msg>,
    ready: &mpsc::Sender<Result<(), Error>>,
) -> Result<(), Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let state: Rc<RefCell<State>> = Rc::default();
    // Nothing is reported until the first sync, so startup is one list, not dozens.
    let synced = Rc::new(Cell::new(false));
    let report: Rc<dyn Fn()> = Rc::new({
        let (state, synced) = (state.clone(), synced.clone());
        move || {
            if !synced.get() {
                return;
            }
            let mut s = state.borrow_mut();
            let list = s.list();
            if s.reported.as_ref() != Some(&list) {
                s.reported = Some(list.clone());
                drop(s);
                on_change(list);
            }
        }
    });
    let on_volume: OnVolume = Rc::new(on_volume);
    let metadata: Rc<RefCell<Option<(Metadata, MetadataListener)>>> = Rc::default();

    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (state, report, metadata) = (state.clone(), report.clone(), metadata.clone());
            let on_volume = on_volume.clone();
            let registry = registry.downgrade();
            move |global| {
                let Some(props) = global.props else { return };
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                match global.type_ {
                    ObjectType::Node => {
                        let direction = match props.get("media.class") {
                            Some("Audio/Source" | "Audio/Source/Virtual") => Direction::Source,
                            Some("Audio/Sink") => Direction::Sink,
                            _ => return,
                        };
                        let Some(name) = props.get("node.name") else {
                            return;
                        };
                        // Our own echo-cancel nodes.
                        if name.starts_with("vivus-") {
                            return;
                        }
                        let description = props
                            .get("node.description")
                            .or_else(|| props.get("node.nick"))
                            .unwrap_or(name);
                        let name = name.to_owned();
                        state
                            .borrow_mut()
                            .nodes
                            .insert(global.id, (name.clone(), description.into(), direction));
                        if direction == Direction::Source {
                            watch_source(&state, &registry, global, name, &on_volume);
                        }
                        report();
                    }
                    ObjectType::Device if props.get("media.class") == Some("Audio/Device") => {
                        watch_card(&state, &registry, global);
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        let md: Metadata = match registry.bind(global) {
                            Ok(m) => m,
                            Err(e) => return tracing::warn!("devices: bind metadata: {e}"),
                        };
                        let listener = md
                            .add_listener_local()
                            .property({
                                let (state, report) = (state.clone(), report.clone());
                                move |subject, key, _, value| {
                                    if subject == 0 {
                                        on_default(&state, key, value);
                                        report();
                                    }
                                    0
                                }
                            })
                            .register();
                        *metadata.borrow_mut() = Some((md, listener));
                    }
                    _ => {}
                }
            }
        })
        .global_remove({
            let (state, report) = (state.clone(), report.clone());
            move |id| {
                let mut s = state.borrow_mut();
                s.sources.remove(&id);
                s.cards.remove(&id);
                if s.nodes.remove(&id).is_some() {
                    drop(s);
                    report();
                }
            }
        })
        .register();

    // Two round trips: one for the registry globals, one for the metadata
    // properties that binding it sends.
    let pending = Rc::new(Cell::new(core.sync(0)?));
    let _core_listener = core
        .add_listener_local()
        .done({
            let (core, pending, synced, report) = (
                core.downgrade(),
                pending.clone(),
                synced.clone(),
                report.clone(),
            );
            let first = Cell::new(true);
            move |id, seq| {
                if id != pw::core::PW_ID_CORE || seq != pending.get() {
                    return;
                }
                if first.replace(false)
                    && let Some(core) = core.upgrade()
                {
                    match core.sync(0) {
                        Ok(s) => return pending.set(s),
                        Err(e) => tracing::warn!("devices: sync: {e}"),
                    }
                }
                if !synced.replace(true) {
                    report();
                }
            }
        })
        .register();

    let weak_loop = mainloop.downgrade();
    let _msgs = msgs.attach(mainloop.loop_(), {
        let state = state.clone();
        move |msg| match msg {
            Msg::Quit => {
                if let Some(l) = weak_loop.upgrade() {
                    l.quit();
                }
            }
            Msg::SetVolume(name, volume) => set_volume(&state.borrow(), &name, volume),
        }
    });

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

fn watch_source(
    state: &Rc<RefCell<State>>,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
    name: String,
    on_volume: &OnVolume,
) {
    let node: Node = match registry.bind(global) {
        Ok(n) => n,
        Err(e) => return tracing::debug!("devices: bind node {}: {e}", global.id),
    };
    let id = global.id;
    let listener = node
        .add_listener_local()
        // Registry globals carry only some props; `card.profile.device` isn't one.
        .info({
            let state = state.clone();
            move |info| {
                // State changes resend info without props.
                if !info.change_mask().contains(NodeChangeMask::PROPS) {
                    return;
                }
                let Some(props) = info.props() else { return };
                let card = props.get("device.id").and_then(|d| d.parse().ok()).zip(
                    props
                        .get("card.profile.device")
                        .and_then(|d| d.parse().ok()),
                );
                if let Some(source) = state.borrow_mut().sources.get_mut(&id) {
                    source.card = card;
                }
            }
        })
        .param({
            let (state, on_volume) = (state.clone(), on_volume.clone());
            move |_, _, _, _, pod| {
                let Some(volumes) =
                    pod.and_then(|p| float_array(p, spa::sys::SPA_PROP_channelVolumes))
                else {
                    return;
                };
                let volume = volumes.iter().copied().fold(0.0f32, f32::max).cbrt();
                let mut s = state.borrow_mut();
                let Some(source) = s.sources.get_mut(&id) else {
                    return;
                };
                source.channels = volumes.len();
                if source.volume.is_some_and(|v| (v - volume).abs() < 1e-4) {
                    return;
                }
                source.volume = Some(volume);
                drop(s);
                on_volume(&name, volume);
            }
        })
        .register();
    node.subscribe_params(&[ParamType::Props]);
    state.borrow_mut().sources.insert(
        id,
        Source {
            node,
            _listener: listener,
            channels: 0,
            card: None,
            volume: None,
        },
    );
}

fn watch_card(
    state: &Rc<RefCell<State>>,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) {
    let device: PwDevice = match registry.bind(global) {
        Ok(d) => d,
        Err(e) => return tracing::debug!("devices: bind device {}: {e}", global.id),
    };
    let id = global.id;
    let listener = device
        .add_listener_local()
        .param({
            let state = state.clone();
            move |_, _, _, _, pod| {
                let Some((index, device)) = pod.and_then(input_route) else {
                    return;
                };
                if let Some(card) = state.borrow_mut().cards.get_mut(&id) {
                    card.routes.insert(device, index);
                }
            }
        })
        .register();
    device.subscribe_params(&[ParamType::Route]);
    state.borrow_mut().cards.insert(
        id,
        Card {
            device,
            _listener: listener,
            routes: HashMap::new(),
        },
    );
}

fn set_volume(state: &State, name: &str, volume: f32) {
    let Some((id, source)) = state
        .sources
        .iter()
        .find(|(id, _)| state.nodes.get(id).is_some_and(|n| n.0 == name))
    else {
        return tracing::warn!("devices: no source {name} to set the volume of");
    };
    if source.channels == 0 {
        return tracing::warn!("devices: source {name} has no volume yet");
    }
    let linear = volume.clamp(0.0, 1.0).powi(3);
    let props = Value::Object(spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamProps.as_raw(),
        id: ParamType::Props.as_raw(),
        properties: vec![property(
            spa::sys::SPA_PROP_channelVolumes,
            Value::ValueArray(ValueArray::Float(vec![linear; source.channels])),
        )],
    });
    // The card's route is what mixers set: it moves the hardware gain and
    // WirePlumber saves it. Node Props on an ALSA source are ignored.
    let route = source.card.and_then(|(card_id, profile_device)| {
        let card = state.cards.get(&card_id)?;
        Some((card, *card.routes.get(&profile_device)?, profile_device))
    });
    if let Some((card, index, profile_device)) = route {
        tracing::debug!("devices: {name} volume {volume:.3} via route {index}");
        let value = Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamRoute.as_raw(),
            id: ParamType::Route.as_raw(),
            properties: vec![
                property(spa::sys::SPA_PARAM_ROUTE_index, Value::Int(index)),
                property(spa::sys::SPA_PARAM_ROUTE_device, Value::Int(profile_device)),
                property(spa::sys::SPA_PARAM_ROUTE_props, props),
                property(spa::sys::SPA_PARAM_ROUTE_save, Value::Bool(true)),
            ],
        });
        card.device
            .set_param(ParamType::Route, 0, &pod_bytes(&value));
    } else {
        tracing::debug!("devices: {name} volume {volume:.3} via node {id}");
        source
            .node
            .set_param(ParamType::Props, 0, &pod_bytes(&props));
    }
}

fn property(key: u32, value: Value) -> spa::pod::Property {
    spa::pod::Property {
        key,
        flags: spa::pod::PropertyFlags::empty(),
        value,
    }
}

fn pod_bytes(value: &Value) -> PodBuf {
    PodBuf(
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), value)
            .expect("serialize pod")
            .0
            .into_inner(),
    )
}

struct PodBuf(Vec<u8>);

impl std::ops::Deref for PodBuf {
    type Target = Pod;
    fn deref(&self) -> &Pod {
        Pod::from_bytes(&self.0).expect("valid pod")
    }
}

fn object(pod: &Pod) -> Option<spa::pod::Object> {
    match PodDeserializer::deserialize_any_from(pod.as_bytes()) {
        Ok((_, Value::Object(obj))) => Some(obj),
        Ok(_) => None,
        Err(_) => None,
    }
}

/// A float array property of a Props param, if present and non-empty.
fn float_array(pod: &Pod, key: u32) -> Option<Vec<f32>> {
    let obj = object(pod)?;
    match &obj.properties.iter().find(|p| p.key == key)?.value {
        Value::ValueArray(ValueArray::Float(v)) if !v.is_empty() => Some(v.clone()),
        _ => None,
    }
}

/// (route index, profile device) of an input Route param.
fn input_route(pod: &Pod) -> Option<(i32, i32)> {
    let obj = object(pod)?;
    let (mut index, mut device, mut input) = (None, None, false);
    for p in &obj.properties {
        match (p.key, &p.value) {
            (spa::sys::SPA_PARAM_ROUTE_index, Value::Int(i)) => index = Some(*i),
            (spa::sys::SPA_PARAM_ROUTE_device, Value::Int(d)) => device = Some(*d),
            (spa::sys::SPA_PARAM_ROUTE_direction, Value::Id(d)) => {
                input = d.0 == spa::sys::SPA_DIRECTION_INPUT;
            }
            _ => {}
        }
    }
    input.then_some((index?, device?))
}

/// `default.audio.{source,sink}` = `{"name":"..."}`. A `None` key clears all.
fn on_default(state: &RefCell<State>, key: Option<&str>, value: Option<&str>) {
    let mut s = state.borrow_mut();
    let name = value.and_then(default_name);
    match key {
        Some("default.audio.source") => s.default_source = name,
        Some("default.audio.sink") => s.default_sink = name,
        None => (s.default_source, s.default_sink) = (None, None),
        _ => {}
    }
}

/// The node name in a default-metadata value. It's always this flat object,
/// so a full JSON parser isn't worth the dependency.
fn default_name(json: &str) -> Option<String> {
    let rest = json
        .split_once("\"name\"")?
        .1
        .trim_start()
        .strip_prefix(':')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_name() {
        assert_eq!(
            default_name(r#"{"name":"alsa_input.pci-0000_0a_00.4.analog-stereo"}"#).as_deref(),
            Some("alsa_input.pci-0000_0a_00.4.analog-stereo")
        );
        assert_eq!(
            default_name(r#"{ "name" : "a\"b" }"#).as_deref(),
            Some("a\"b")
        );
        assert_eq!(default_name("{}"), None);
    }

    #[test]
    fn lists_sources_first_with_defaults() {
        let mut s = State::default();
        s.nodes
            .insert(1, ("sink".into(), "Speakers".into(), Direction::Sink));
        s.nodes
            .insert(2, ("b".into(), "USB mic".into(), Direction::Source));
        s.nodes
            .insert(3, ("a".into(), "Built-in mic".into(), Direction::Source));
        s.default_source = Some("b".into());
        let list = s.list();
        let names: Vec<_> = list.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "sink"]);
        assert!(list[1].is_default && !list[0].is_default && !list[2].is_default);
    }
}
