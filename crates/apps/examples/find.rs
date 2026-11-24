//! `cargo run -p vivus-apps --example find -- [--launch] <spoken name>`
use vivus_apps::{Apps, Match};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let launch = args.first().is_some_and(|a| a == "--launch");
    if launch {
        args.remove(0);
    }
    let apps = Apps::load();
    match apps.find(&args.join(" ")) {
        Match::Found(app) => {
            println!("found {} ({}): {}", app.name, app.id, app.exec);
            if launch {
                vivus_apps::launch(&apps, app, None).unwrap();
            }
        }
        Match::Ambiguous(v) => println!("ambiguous: {:?}", v.iter().map(|a| &a.name).collect::<Vec<_>>()),
        Match::NotFound(v) => println!("not found; nearest: {:?}", v.iter().map(|a| &a.name).collect::<Vec<_>>()),
    }
}
