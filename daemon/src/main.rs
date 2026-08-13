mod color_convert;
mod ffi;
mod h264_headers;
mod portal_capture;
mod vaapi_encoder;

use std::sync::atomic::{AtomicBool, Ordering};

static SIGINT_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_sig: i32) {
    SIGINT_RECEIVED.store(true, Ordering::SeqCst);
}

/// pipewire's own `add_signal_local` mechanism turned out unreliable in
/// this binary (registered without error, callback never fired -- SIGINT
/// killed the process via the OS default disposition regardless). Plain
/// libc signal handling + a manual poll loop, proven in the earlier
/// evdi-based daemon, is the fallback `portal_capture::run_capture` uses.
pub fn set_up_sigint_handler() {
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}

pub fn sigint_received() -> bool {
    SIGINT_RECEIVED.load(Ordering::SeqCst)
}

// Single-threaded runtime: keeps the process to one OS thread so there's
// nowhere for a signal to land except where we're polling for it.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/daemon_capture.h264".to_string());

    eprintln!("Opening portal ScreenCast session -- pick the virtual monitor in the dialog...");
    let (stream, fd) = portal_capture::open_portal()
        .await
        .expect("portal negotiation failed");
    let node_id = stream.pipe_wire_node_id();
    eprintln!(
        "[portal] got stream: node_id={node_id} size={:?} position={:?}",
        stream.size(),
        stream.position()
    );

    let stats = portal_capture::run_capture(node_id, fd, &out_path).expect("capture failed");

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
    println!("dequeue->encoded latency: avg={avg:?} min={min:?} max={max:?}");
    println!("output written to: {out_path}");
    // stdout is fully buffered (not line-buffered) once redirected to a file
    // or pipe -- flush explicitly so this isn't silently lost if the process
    // exits any way other than a normal return from main().
    use std::io::Write;
    let _ = std::io::stdout().flush();
}
