mod color_convert;
mod evdi_capture;
mod ffi;
mod h264_headers;
mod vaapi_encoder;

use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}

fn main() {
    let card: i32 = std::env::args()
        .nth(1)
        .map(|s| s.parse().expect("card number"))
        .unwrap_or(1);
    let out_path = std::env::args().nth(2).unwrap_or_else(|| "/tmp/daemon_capture.h264".to_string());

    unsafe {
        libc::signal(libc::SIGINT, on_sigint as libc::sighandler_t);
    }

    let stop: &'static AtomicBool = &STOP;
    let stats = evdi_capture::run(card, &out_path, stop);

    if stats.frame_count == 0 {
        println!("No frames captured.");
        return;
    }

    let total: std::time::Duration = stats.durations.iter().sum();
    let avg = total / stats.frame_count as u32;
    let min = stats.durations.iter().min().unwrap();
    let max = stats.durations.iter().max().unwrap();

    println!("--- Milestone 2 summary ---");
    println!("frames captured+encoded: {}", stats.frame_count);
    println!("grab->encoded latency: avg={avg:?} min={min:?} max={max:?}");
    println!("output written to: {out_path}");
}
