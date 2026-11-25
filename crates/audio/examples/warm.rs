//! Mic wake-up check: open capture, close it for a while, reopen, and print the
//! peak of each 100 ms after every start. A thump shows as a loud first second.
//!   cargo run -p vivus-audio --example warm -- [--no-aec] [--keep-open] [--rms] [gap_secs] [rounds] [hold_100ms]
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vivus_audio::{Capture, CaptureOptions, EchoCancelOptions, SAMPLE_RATE};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let aec = !args.iter().any(|a| a == "--no-aec");
    let rms = args.iter().any(|a| a == "--rms");
    let nums: Vec<u64> = args.iter().filter_map(|a| a.parse().ok()).collect();
    let gap = nums.first().copied().unwrap_or(8);
    let rounds = nums.get(1).copied().unwrap_or(2);
    let hold = nums.get(2).copied().unwrap_or(30) as usize;

    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let cap = Capture::spawn(
        CaptureOptions {
            target: None,
            echo_cancel: aec.then(EchoCancelOptions::default),
            keep_open: args.iter().any(|a| a == "--keep-open"),
        },
        move |s| {
            let _ = tx.send(s);
        },
        |_, _| {},
    )
    .expect("capture");
    println!("echo cancelled: {}", cap.echo_cancelled());
    let win = SAMPLE_RATE as usize / 10;
    for round in 0..rounds {
        std::thread::sleep(Duration::from_secs(gap));
        while rx.try_recv().is_ok() {}
        let t0 = Instant::now();
        cap.set_active(true);
        let mut buf = Vec::new();
        while buf.len() < win * hold {
            if let Ok(s) = rx.recv_timeout(Duration::from_secs(3)) {
                if buf.is_empty() {
                    println!("round {round}: first audio after {:?}", t0.elapsed());
                }
                buf.extend(s);
            } else {
                println!("round {round}: no audio");
                break;
            }
        }
        cap.set_active(false);
        let peaks: Vec<String> = buf
            .chunks(win)
            .map(|c| {
                if rms {
                    let r = (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt();
                    format!("{:.0}", 20.0 * r.max(1e-6).log10())
                } else {
                    format!("{:.3}", c.iter().fold(0f32, |m, x| m.max(x.abs())))
                }
            })
            .collect();
        println!(
            "round {round} {}/100ms",
            if rms { "rms dB" } else { "peak" }
        );
        println!("  : {}", peaks.join(" "));
    }
}
