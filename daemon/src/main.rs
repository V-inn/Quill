mod aoa;
mod clock_sync;
mod ffi;
mod gesture;
mod h264_headers;
mod input_receiver;
mod orientation;
mod portal_capture;
mod protocol;
mod remote_desktop_input;
mod single_instance;
mod uinput_buttons;
mod uinput_tablet;
mod uinput_touchpad;
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
    // Before anything with a side effect: no USB claim, no portal call, no
    // `pkill -f krfb-virtualmonitor`. See `single_instance` for what a second
    // instance breaks.
    single_instance::acquire_or_exit();

    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/daemon_capture.h264".to_string());
    // Second arg selects the transport: a bare port number means the
    // original adb-forward path (Milestones 3-7); the literal "aoa" means
    // Milestone 8's direct-USB path; omitted means no transport at all
    // (capture-only, used for the latency diagnostics in MILESTONES.md).
    let transport_config = match std::env::args().nth(2).as_deref() {
        None => portal_capture::TransportConfig::None,
        Some("aoa") => portal_capture::TransportConfig::Aoa,
        Some(s) => portal_capture::TransportConfig::TcpForward(
            s.parse().unwrap_or_else(|_| panic!("invalid port or mode: {s} (expected a port number or \"aoa\")")),
        ),
    };

    // Decided once, before any portal negotiation: uinput (real S Pen
    // pressure/tilt fidelity) needs a root-granted udev rule or ACL that
    // may simply not exist on this machine and never will (e.g. a school
    // computer with no sudo, ever) -- see uinput_tablet::uinput_accessible
    // and remote_desktop_input.rs. The two paths need different,
    // mutually-exclusive portal sessions, so this has to be decided before
    // either one starts, not worked around after the fact.
    let use_uinput = uinput_tablet::uinput_accessible();

    // Connects the transport and blocks for the tablet's capability
    // handshake *before* any portal call (Milestone 15): the virtual
    // monitor's rotation needs to match the tablet's aspect before the
    // portal picker can select it. `remote_input_rx`/`_tx`: only the
    // RemoteDesktop fallback needs this -- its `RemoteDesktopInput` handle
    // can't exist until *after* the portal opens, so `input_receiver::run`
    // blocks on the receiver once it's done with the handshake, and the
    // sender end gets fed once that portal session negotiates below.
    let (remote_input_tx, remote_input_rx) =
        std::sync::mpsc::channel::<remote_desktop_input::RemoteDesktopInput>();
    let transport_setup =
        portal_capture::setup_transport(transport_config, if use_uinput { None } else { Some(remote_input_rx) });

    // Cursor rendering is the client's choice, carried in the handshake, which
    // is why the transport is brought up before any portal call (Milestone 15
    // established that ordering for the orientation logic). `QUILL_CURSOR`
    // still overrides, for capture-only runs where there is no client at all.
    let cursor = match (
        std::env::var("QUILL_CURSOR").as_deref(),
        transport_setup.as_ref(),
    ) {
        (Ok("client"), _) => portal_capture::CursorRendering::ClientSide,
        (Ok("embedded"), _) => portal_capture::CursorRendering::Embedded,
        (_, Some((_, info)))
            if info.config_flags & protocol::CONFIG_CLIENT_SIDE_CURSOR != 0 =>
        {
            portal_capture::CursorRendering::ClientSide
        }
        _ => portal_capture::CursorRendering::Embedded,
    };
    eprintln!(
        "[portal] cursor rendering: {}",
        match cursor {
            portal_capture::CursorRendering::ClientSide => "client-side (CursorMode::Metadata)",
            portal_capture::CursorRendering::Embedded => "embedded in video",
        }
    );

    // Portrait needs a 180-degree flip on this machine (USB cable position)
    // -- applied GPU-side by the encoder and mirrored in the uinput touch
    // mapping (see vaapi_encoder.rs / input_receiver.rs), not via KWin
    // rotation, which Milestone 16 found has no effect on this output type
    // at all.
    let flip_180 = transport_setup.as_ref().is_some_and(|(_, i)| i.height > i.width);

    if let Some((_, info)) = &transport_setup {
        orientation::ensure(info.width, info.height);
    }

    let (stream, fd) = if use_uinput {
        eprintln!("Opening portal ScreenCast session -- pick the virtual monitor in the dialog...");
        portal_capture::open_portal(cursor).await.expect("portal negotiation failed")
    } else {
        eprintln!(
            "[input] /dev/uinput not accessible (no root-granted permission on this machine) \
             -- falling back to portal RemoteDesktop input: position + click only, no S Pen \
             pressure/tilt. Opening combined ScreenCast + RemoteDesktop portal session..."
        );
        let (stream, fd, remote_input) = remote_desktop_input::open_portal_with_input()
            .await
            .expect("portal negotiation failed");
        // Hands off to the input thread, which has been blocked waiting for
        // this since right after it read the capability handshake above.
        let _ = remote_input_tx.send(remote_input);
        (stream, fd)
    };
    let node_id = stream.pipe_wire_node_id();
    eprintln!(
        "[portal] got stream: node_id={node_id} size={:?} position={:?}",
        stream.size(),
        stream.position()
    );

    let transport = transport_setup.map(|(writer, _)| writer);
    let stats =
        portal_capture::run_capture(node_id, fd, &out_path, transport, flip_180, cursor).expect("capture failed");

    if stats.frame_count == 0 {
        println!("No frames captured.");
        return;
    }

    println!("--- summary ---");
    println!("frames captured+encoded: {}", stats.frame_count);
    println!("stale frames dropped: {}", stats.dropped_stale);
    // `durations` is empty whenever frames were counted but never encoded --
    // reachable via QUILL_NO_ENCODE (see portal_capture.rs), which deliberately
    // returns before the encoder to measure the capture path's own throughput.
    // Previously this unwrapped and panicked right after printing the frame
    // count, losing the rest of the summary.
    if stats.durations.is_empty() {
        println!("dequeue->encoded latency: not measured (no frames encoded)");
    } else {
        let total: std::time::Duration = stats.durations.iter().sum();
        let avg = total / stats.durations.len() as u32;
        let min = stats.durations.iter().min().unwrap();
        let max = stats.durations.iter().max().unwrap();
        println!("dequeue->encoded latency: avg={avg:?} min={min:?} max={max:?}");
    }
    if stats.buffer_age_samples > 0 {
        println!(
            "buffer age (pts->dequeue) avg: {:.2}ms over {} samples",
            stats.buffer_age_ms_sum / stats.buffer_age_samples as f64,
            stats.buffer_age_samples
        );
    } else {
        println!("buffer age (pts->dequeue): no MetaHeader.pts available from this producer");
    }
    if stats.capture_latency_samples > 0 {
        println!(
            "capture latency (barcode->dequeue) avg: {:.2}ms over {} samples",
            stats.capture_latency_ms_sum / stats.capture_latency_samples as f64,
            stats.capture_latency_samples
        );
    } else {
        println!("capture latency (barcode->dequeue): no barcode probe detected");
    }
    if stats.convert_encode_samples > 0 {
        println!(
            "upload+VPP+encode avg: {:.2}ms (over {} samples)",
            stats.encode_ms_sum / stats.convert_encode_samples as f64,
            stats.convert_encode_samples
        );
    }
    println!("output written to: {out_path}");
    // stdout is fully buffered (not line-buffered) once redirected to a file
    // or pipe -- flush explicitly so this isn't silently lost if the process
    // exits any way other than a normal return from main().
    use std::io::Write;
    let _ = std::io::stdout().flush();
}
