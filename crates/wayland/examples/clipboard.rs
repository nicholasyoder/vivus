//! `cargo run -p vivus-wayland --example clipboard` prints the clipboard;
//! `-- <text>` copies the text and keeps it on offer for 30 s.

use vivus_core::ClipboardBackend;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let clipboard = vivus_wayland::Clipboard::connect()?;
    let text: Vec<String> = std::env::args().skip(1).collect();
    if text.is_empty() {
        println!("{:?}", clipboard.read()?);
    } else {
        clipboard.write(&text.join(" "))?;
        println!("copied; serving for 30 s");
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    Ok(())
}
