//! The router's `launch_app` tool. See docs/desktop-control.md#app-launching.

use vivus_apps::{Apps, Match};

/// Finds and starts the app the user named. `Ok` has its display name; `Err`
/// is the tool result the model must answer.
pub fn launch_app(spoken: &str) -> Result<String, String> {
    let apps = Apps::load();
    let names = |v: &[&vivus_apps::App]| v.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ");
    let app = match apps.find(spoken) {
        Match::Found(app) => app,
        Match::Ambiguous(v) => {
            return Err(format!(
                "Several apps match \"{spoken}\": {}. Ask the user which one.",
                names(&v)
            ));
        }
        Match::NotFound(v) if v.is_empty() => {
            return Err(format!("No installed app matches \"{spoken}\"."));
        }
        Match::NotFound(v) => {
            return Err(format!(
                "No installed app matches \"{spoken}\". Closest: {}. Launch one only if \
                 it's clearly what the user meant; otherwise tell them.",
                names(&v)
            ));
        }
    };
    // Without a token the app still starts, just maybe unfocused.
    let token = vivus_wayland::activation_token(app.app_id())
        .inspect_err(|e| tracing::warn!("no activation token: {e:#}"))
        .ok();
    match vivus_apps::launch(&apps, app, token.as_deref()) {
        Ok(()) => Ok(app.name.clone()),
        Err(e) => {
            tracing::error!("launching {}: {e}", app.id);
            Err(format!("{} failed to start: {e}.", app.name))
        }
    }
}
