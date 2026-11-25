//! D-Bus names and client proxies for `org.vivus`.
//! The source of truth is `dbus/org.vivus.Assistant.xml`; keep this in sync.

use zbus::proxy;

pub const SERVICE: &str = "org.vivus";
pub const PATH: &str = "/org/vivus";

/// `ListAudioDevices` entry: node name, description, is default, `source` | `sink`.
pub type AudioDevice = (String, String, bool, String);

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

    fn list_audio_devices(&self) -> zbus::Result<Vec<AudioDevice>>;

    #[zbus(signal)]
    fn changed(&self, paths: Vec<String>) -> zbus::Result<()>;
    #[zbus(signal)]
    fn audio_devices_changed(&self, devices: Vec<AudioDevice>) -> zbus::Result<()>;

    fn meter_input(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn input_level(&self, peak: f64, clipped: bool) -> zbus::Result<()>;

    fn get_input_gain(&self) -> zbus::Result<f64>;
    fn set_input_gain(&self, gain: f64) -> zbus::Result<()>;
    #[zbus(signal)]
    fn input_gain_changed(&self, gain: f64) -> zbus::Result<()>;
}
