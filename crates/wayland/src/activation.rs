//! `xdg-activation-v1` tokens for apps we launch, so the compositor can focus
//! their first window. We have no input serial or surface to offer, so a
//! compositor with strict focus-stealing prevention may still not focus it.

use anyhow::Context;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_token_v1::{
    self, XdgActivationTokenV1,
};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::{self, XdgActivationV1};

#[derive(Default)]
struct State {
    token: Option<String>,
}

/// Asks the compositor for a token for `app_id` over a short-lived connection.
pub fn activation_token(app_id: &str) -> anyhow::Result<String> {
    let conn = Connection::connect_to_env().context("no Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let activation: XdgActivationV1 = globals
        .bind(&qh, 1..=1, ())
        .context("compositor lacks xdg-activation-v1")?;
    let token = activation.get_activation_token(&qh, ());
    token.set_app_id(app_id.into());
    token.commit();
    let mut state = State::default();
    // The protocol always answers a commit with `done`.
    while state.token.is_none() {
        queue.blocking_dispatch(&mut state)?;
    }
    token.destroy();
    activation.destroy();
    conn.flush()?;
    Ok(state.token.unwrap())
}

// Only the token object sends anything we need.
impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgActivationV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &XdgActivationV1,
        _: xdg_activation_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.token = Some(token);
        }
    }
}
