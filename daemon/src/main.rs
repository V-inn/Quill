mod color_convert;
mod ffi;
mod h264_headers;
mod portal_capture;
mod vaapi_encoder;

#[tokio::main]
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
}
