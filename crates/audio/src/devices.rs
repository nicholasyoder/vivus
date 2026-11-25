//! Audio sources and sinks from the PipeWire registry, with the system
//! defaults from the `default` metadata, reported whenever either changes.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use pipewire as pw;
use pw::metadata::{Metadata, MetadataListener};
use pw::types::ObjectType;

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

#[derive(Default)]
struct State {
    /// Global id → (name, description, direction).
    nodes: BTreeMap<u32, (String, String, Direction)>,
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

/// Watches devices on its own PipeWire thread until dropped.
pub struct Devices {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Devices {
    /// `on_change` gets the full list once the initial registry scan is done
    /// and again after every change, on the PipeWire thread.
    pub fn spawn(on_change: impl Fn(Vec<Device>) + Send + 'static) -> Result<Self, Error> {
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), Error>>();
        let thread = std::thread::Builder::new()
            .name("vivus-devices".into())
            .spawn(move || {
                if let Err(e) = run(on_change, quit_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .expect("spawn devices thread");
        ready_rx.recv().map_err(|_| Error::ThreadDied)??;
        Ok(Self {
            quit: quit_tx,
            thread: Some(thread),
        })
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    on_change: impl Fn(Vec<Device>) + 'static,
    quit: pw::channel::Receiver<()>,
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
    let metadata: Rc<RefCell<Option<(Metadata, MetadataListener)>>> = Rc::default();

    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (state, report, metadata) = (state.clone(), report.clone(), metadata.clone());
            let registry = registry.downgrade();
            move |global| {
                let Some(props) = global.props else { return };
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
                        state
                            .borrow_mut()
                            .nodes
                            .insert(global.id, (name.into(), description.into(), direction));
                        report();
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        let Some(registry) = registry.upgrade() else {
                            return;
                        };
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
                if state.borrow_mut().nodes.remove(&id).is_some() {
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
    let _quit = quit.attach(mainloop.loop_(), move |()| {
        if let Some(l) = weak_loop.upgrade() {
            l.quit();
        }
    });

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
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
