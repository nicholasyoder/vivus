//! Lists open windows, then watches focus changes.
//! `cargo run -p vivus-wayland --example windows [-- <activate|close|minimize> <id>]`
use vivus_core::{WindowAction, WindowBackend};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let windows = vivus_wayland::Windows::connect(Box::new(|w| {
        println!("focused: {} ({})", w.title, w.app_id);
    }))?;
    for w in windows.windows() {
        println!("{w:?}");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [action, id] = args.as_slice() {
        let action = match action.as_str() {
            "activate" => WindowAction::Activate,
            "close" => WindowAction::Close,
            "minimize" => WindowAction::Minimize,
            other => anyhow::bail!("unknown action {other}"),
        };
        windows.act(id.parse()?, action)?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        return Ok(());
    }
    std::thread::sleep(std::time::Duration::from_secs(5));
    Ok(())
}
