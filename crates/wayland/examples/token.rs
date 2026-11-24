//! `cargo run -p vivus-wayland --example token -- <app id>`
fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "test".into());
    println!("{:?}", vivus_wayland::activation_token(&id));
}
