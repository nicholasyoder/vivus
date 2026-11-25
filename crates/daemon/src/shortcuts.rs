//! Global shortcuts through `org.freedesktop.portal.GlobalShortcuts`. Presses
//! become engine commands; the session is rebound whenever the portal restarts
//! or closes it. Without the portal, `vivusctl` bound in the compositor is the
//! fallback.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use vivus_core::config::Shortcuts;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy, fdo};

use crate::engine::Command;
use crate::service;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS_IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
/// Host (unsandboxed) apps need an ID for GlobalShortcuts since
/// xdg-desktop-portal 1.19; the 1d desktop entry should use the same name.
const APP_ID: &str = "org.vivus.Vivus";

const PUSH_TO_TALK: &str = "push-to-talk";
const TOGGLE_LISTEN: &str = "toggle-listen";
const TOGGLE_SIDEBAR: &str = "toggle-sidebar";

struct Binding {
    id: &'static str,
    description: &'static str,
    trigger: String,
}

type Vardict = HashMap<String, OwnedValue>;
type ShortcutSignal = (OwnedObjectPath, String, u64, Vardict);

/// `daemon` is the connection that owns `org.vivus`, for the sidebar signal.
pub fn spawn(config: &Shortcuts, tx: UnboundedSender<Command>, daemon: Connection) {
    let bindings: Vec<Binding> = [
        (PUSH_TO_TALK, "Push to talk (hold)", &config.push_to_talk),
        (
            TOGGLE_LISTEN,
            "Start or stop listening",
            &config.toggle_listen,
        ),
        (
            TOGGLE_SIDEBAR,
            "Show or hide the sidebar",
            &config.toggle_sidebar,
        ),
    ]
    .into_iter()
    .filter(|(_, _, trigger)| !trigger.trim().is_empty())
    .map(|(id, description, trigger)| Binding {
        id,
        description,
        trigger: trigger.trim().to_owned(),
    })
    .collect();
    if bindings.is_empty() {
        return;
    }
    tokio::spawn(async move {
        if let Err(e) = run(bindings, tx, daemon).await {
            tracing::warn!("global shortcuts unavailable: {e:#}");
        }
    });
}

async fn run(
    bindings: Vec<Binding>,
    tx: UnboundedSender<Command>,
    daemon: Connection,
) -> zbus::Result<()> {
    // Our own connection: the app-ID registration and the session belong to it.
    let bus = Connection::session().await?;
    let dbus = fdo::DBusProxy::new(&bus).await?;
    let mut owner_changes = dbus
        .receive_name_owner_changed_with_args(&[(0, PORTAL)])
        .await?;
    let mut handler = Handler {
        tx,
        daemon,
        ptt_held: false,
    };
    loop {
        match open_session(&bus, &bindings).await {
            Ok(session) => {
                handler.serve(&bus, &session, &mut owner_changes).await?;
                handler.release();
                tracing::warn!("global shortcuts: portal session lost; rebinding");
                // The portal is back by now; the pause stops a close-on-open loop.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            Err(e) => tracing::warn!(
                "global shortcuts unavailable ({e}); bind `vivusctl` commands to keys instead"
            ),
        }
        // Retry when the portal (re)starts.
        while let Some(change) = owner_changes.next().await {
            if change.args().is_ok_and(|a| a.new_owner().is_some()) {
                break;
            }
        }
    }
}

struct Handler {
    tx: UnboundedSender<Command>,
    daemon: Connection,
    ptt_held: bool,
}

impl Handler {
    /// Returns when the session is closed or the portal goes away.
    async fn serve(
        &mut self,
        bus: &Connection,
        session: &OwnedObjectPath,
        owner_changes: &mut fdo::NameOwnerChangedStream,
    ) -> zbus::Result<()> {
        let portal = shortcuts_proxy(bus).await?;
        let mut activated = portal.receive_signal("Activated").await?;
        let mut deactivated = portal.receive_signal("Deactivated").await?;
        let session_proxy = proxy(bus, session.as_str(), "org.freedesktop.portal.Session").await?;
        let mut closed = session_proxy.receive_signal("Closed").await?;
        loop {
            tokio::select! {
                Some(msg) = activated.next() => {
                    if let Ok((s, id, _, _)) = msg.body().deserialize::<ShortcutSignal>()
                        && &s == session
                    {
                        self.pressed(&id).await;
                    }
                }
                Some(msg) = deactivated.next() => {
                    if let Ok((s, id, _, _)) = msg.body().deserialize::<ShortcutSignal>()
                        && &s == session
                    {
                        self.released(&id);
                    }
                }
                _ = closed.next() => return Ok(()),
                Some(change) = owner_changes.next() => {
                    if change.args().is_ok_and(|a| a.new_owner().is_none()) {
                        // Wait for the portal to come back before rebinding.
                        while let Some(change) = owner_changes.next().await {
                            if change.args().is_ok_and(|a| a.new_owner().is_some()) {
                                break;
                            }
                        }
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn pressed(&mut self, id: &str) {
        tracing::debug!("shortcut {id} pressed");
        match id {
            PUSH_TO_TALK => {
                self.ptt_held = true;
                let _ = self.tx.send(Command::StartListening);
            }
            TOGGLE_LISTEN => {
                let _ = self.tx.send(Command::ToggleListen);
            }
            TOGGLE_SIDEBAR => {
                let emitter = SignalEmitter::new(&self.daemon, vivus_dbus::PATH);
                if let Ok(emitter) = emitter
                    && let Err(e) =
                        service::Assistant::sidebar_visibility_requested(&emitter, "toggle").await
                {
                    tracing::warn!("toggling the sidebar: {e}");
                }
            }
            _ => {}
        }
    }

    fn released(&mut self, id: &str) {
        if id == PUSH_TO_TALK && self.ptt_held {
            tracing::debug!("shortcut {id} released");
            self.ptt_held = false;
            let _ = self.tx.send(Command::StopListening);
        }
    }

    /// A lost session never sends the release, so end a held push-to-talk here.
    fn release(&mut self) {
        self.released(PUSH_TO_TALK);
    }
}

async fn open_session(bus: &Connection, bindings: &[Binding]) -> zbus::Result<OwnedObjectPath> {
    register_app_id(bus).await;
    let portal = shortcuts_proxy(bus).await?;

    let (handle, session_token) = (token(), token());
    let options = vardict([
        ("handle_token", &handle),
        ("session_handle_token", &session_token),
    ]);
    let results = request(bus, &portal, "CreateSession", &handle, &(options,)).await?;
    // Older portals return it as a string, newer ones as an object path.
    let session = results
        .get("session_handle")
        .and_then(|v| match &**v {
            Value::Str(s) => OwnedObjectPath::try_from(s.as_str()).ok(),
            Value::ObjectPath(p) => Some(p.clone().into()),
            _ => None,
        })
        .ok_or_else(|| zbus::Error::Failure("CreateSession returned no session".into()))?;

    let shortcuts: Vec<(&str, HashMap<&str, Value>)> = bindings
        .iter()
        .map(|b| {
            let options = HashMap::from([
                ("description", Value::from(b.description)),
                ("preferred_trigger", Value::from(b.trigger.as_str())),
            ]);
            (b.id, options)
        })
        .collect();
    let handle = token();
    let options = vardict([("handle_token", &handle)]);
    let args = (&session, &shortcuts, "", options);
    let results = request(bus, &portal, "BindShortcuts", &handle, &args).await?;

    let bound = results
        .get("shortcuts")
        .and_then(|v| Vec::<(String, Vardict)>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default();
    for b in bindings {
        match bound.iter().find(|(id, _)| id == b.id) {
            Some((_, info)) => {
                let trigger = info
                    .get("trigger_description")
                    .and_then(|v| <&str>::try_from(&**v).ok())
                    .filter(|t| !t.is_empty())
                    .unwrap_or(&b.trigger);
                tracing::info!("global shortcut {}: {trigger}", b.id);
            }
            None => tracing::warn!(
                "global shortcut {} ({}) wasn't bound (refused or already taken)",
                b.id,
                b.trigger
            ),
        }
    }
    Ok(session)
}

/// Failure is fine: older portals lack the registry, and a systemd app scope
/// already gives us an ID.
async fn register_app_id(bus: &Connection) {
    let registry = match proxy(bus, PORTAL_PATH, "org.freedesktop.host.portal.Registry").await {
        Ok(p) => p,
        Err(_) => return,
    };
    let options: HashMap<&str, Value> = HashMap::new();
    if let Err(e) = registry.call_method("Register", &(APP_ID, options)).await {
        tracing::debug!("registering app ID with the portal: {e}");
    }
}

/// Calls a portal method that answers through a `Request` object's `Response`
/// signal. `handle` is the `handle_token` in `args`' options.
async fn request<B>(
    bus: &Connection,
    portal: &Proxy<'_>,
    method: &str,
    handle: &str,
    args: &B,
) -> zbus::Result<Vardict>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    // Subscribe before calling: the Response can beat the method return.
    let sender = bus
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("no unique name".into()))?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PORTAL_PATH}/request/{sender}/{handle}");
    let req = proxy(bus, &path, "org.freedesktop.portal.Request").await?;
    let mut responses = req.receive_signal("Response").await?;

    let reply: OwnedObjectPath = portal.call(method, args).await?;
    if reply.as_str() != path {
        // Pre-1.0 portals picked their own path.
        return Err(zbus::Error::Failure(format!(
            "{method}: unexpected request path {}",
            reply.as_str()
        )));
    }
    let msg = responses
        .next()
        .await
        .ok_or_else(|| zbus::Error::Failure(format!("{method}: no response")))?;
    let (code, results): (u32, Vardict) = msg.body().deserialize()?;
    match code {
        0 => Ok(results),
        1 => Err(zbus::Error::Failure(format!("{method}: cancelled"))),
        _ => Err(zbus::Error::Failure(format!("{method}: failed"))),
    }
}

async fn shortcuts_proxy(bus: &Connection) -> zbus::Result<Proxy<'static>> {
    proxy(bus, PORTAL_PATH, SHORTCUTS_IFACE).await
}

async fn proxy(bus: &Connection, path: &str, iface: &'static str) -> zbus::Result<Proxy<'static>> {
    zbus::proxy::Builder::new(bus)
        .destination(PORTAL)?
        .path(path.to_owned())?
        .interface(iface)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

fn vardict<'a, const N: usize>(entries: [(&'a str, &'a String); N]) -> HashMap<&'a str, Value<'a>> {
    entries
        .into_iter()
        .map(|(k, v)| (k, Value::from(v.as_str())))
        .collect()
}

fn token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("vivus_{}", NEXT.fetch_add(1, Ordering::Relaxed))
}
