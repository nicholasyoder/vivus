//! D-Bus names and client proxies for `org.vivus`.
//! The source of truth is `dbus/org.vivus.Assistant.xml`; keep this in sync.

use zbus::proxy;

pub const SERVICE: &str = "org.vivus";
pub const PATH: &str = "/org/vivus";

#[proxy(
    interface = "org.vivus.Assistant",
    default_service = "org.vivus",
    default_path = "/org/vivus"
)]
pub trait Assistant {
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;

    fn toggle_listen(&self) -> zbus::Result<()>;
    fn start_listening(&self) -> zbus::Result<()>;
    fn stop_listening(&self) -> zbus::Result<()>;
    fn ask(&self, text: &str) -> zbus::Result<()>;
    fn cancel(&self) -> zbus::Result<()>;
    fn quit(&self) -> zbus::Result<()>;
    fn toggle_sidebar(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn transcript(&self, role: String, text: String, is_final: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn sidebar_visibility_requested(&self, action: String) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.vivus.Documents",
    default_service = "org.vivus",
    default_path = "/org/vivus"
)]
pub trait Documents {
    fn list(&self) -> zbus::Result<Vec<String>>;
    fn get(&self, id: &str) -> zbus::Result<(String, String)>;

    #[zbus(signal)]
    fn document_changed(&self, id: String, title: String, markdown: String) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.vivus.Settings",
    default_service = "org.vivus",
    default_path = "/org/vivus"
)]
pub trait Settings {
    fn get(&self) -> zbus::Result<String>;
    fn set(&self, path: &str, value: &zbus::zvariant::Value<'_>) -> zbus::Result<()>;
    fn reset(&self, path: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn changed(&self, paths: Vec<String>) -> zbus::Result<()>;
}
