//! Receives real S Pen input from the Android client and injects it into
//! the virtual uinput tablet (`uinput_tablet.rs`). Runs on its own thread,
//! reading from the read half of whichever transport is active (adb-forward
//! TCP or AOA USB, see `portal_capture::TransportConfig`) -- independent
//! read/write directions of one connection, no separate port.
//!
//! Wire format (all big-endian, Android -> daemon):
//!
//! Handshake (32 bytes, sent once, before any input records) -- this is
//! the capability handshake from the design doc: Android reports its real
//! `Display` metrics and `InputDevice.getMotionRange()` so nothing is
//! hardcoded here. The trailing `i64` is Milestone 7's clock-sync ping,
//! not a capability -- see `clock_sync.rs`.
//!   u32 screen_width_px
//!   u32 screen_height_px
//!   i32 pressure_min
//!   i32 pressure_max
//!   i32 tilt_min_deg
//!   i32 tilt_max_deg
//!   i64 android_send_time_ms (device wall clock, for clock-offset calibration)
//!
//! Input event record (22 bytes, repeated):
//!   u8  event_type   (0=hover_enter 1=hover_move 2=hover_exit 3=down 4=move 5=up 6=button_down 7=button_up)
//!   i32 x_px
//!   i32 y_px
//!   i32 pressure
//!   i32 tilt_x_deg
//!   i32 tilt_y_deg
//!   u8  buttons      (bit0 = stylus primary button state, informational --
//!                     the actual BTN_STYLUS toggle is driven by the
//!                     explicit button_down/button_up event types, not this
//!                     bit; bit1 = tool is a finger, not the S Pen)

use crate::portal_capture::TransportReader;
use crate::remote_desktop_input::RemoteDesktopInput;
use crate::uinput_tablet::{TabletRanges, UinputTablet};
use std::io::{self, Read};
use std::sync::mpsc::Sender;

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

fn read_i32(r: &mut impl Read) -> io::Result<i32> {
    Ok(read_u32(r)? as i32)
}

fn read_i64(r: &mut impl Read) -> io::Result<i64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(i64::from_be_bytes(b))
}

fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

struct Handshake {
    width: u32,
    height: u32,
    pressure_min: i32,
    pressure_max: i32,
    tilt_min: i32,
    tilt_max: i32,
    android_send_ms: i64,
}

fn read_handshake(r: &mut impl Read) -> io::Result<Handshake> {
    Ok(Handshake {
        width: read_u32(r)?,
        height: read_u32(r)?,
        pressure_min: read_i32(r)?,
        pressure_max: read_i32(r)?,
        tilt_min: read_i32(r)?,
        tilt_max: read_i32(r)?,
        android_send_ms: read_i64(r)?,
    })
}

const EV_HOVER_ENTER: u8 = 0;
const EV_HOVER_MOVE: u8 = 1;
const EV_HOVER_EXIT: u8 = 2;
const EV_DOWN: u8 = 3;
const EV_MOVE: u8 = 4;
const EV_UP: u8 = 5;
const EV_BUTTON_DOWN: u8 = 6;
const EV_BUTTON_UP: u8 = 7;

/// Blocks reading the handshake, creates the uinput tablet from the real
/// reported ranges, then loops injecting input events until the stream
/// closes. Meant to run on its own thread.
///
/// `clock_tx`: sends `(android_send_ms, daemon_recv_ms)` the instant the
/// handshake's clock-sync ping is read, so the main thread (which owns the
/// video-writing half of the socket) can complete Milestone 7's clock-offset
/// calibration handshake -- see `clock_sync.rs`.
///
/// `remote_input`: `None` selects the normal full-fidelity uinput tablet
/// path; `Some` means uinput wasn't accessible (see
/// `uinput_tablet::uinput_accessible`) and this session is running the
/// reduced-fidelity `RemoteDesktop` portal fallback instead (position +
/// click, no pressure/tilt) -- see `remote_desktop_input.rs`.
pub fn run(mut stream: TransportReader, clock_tx: Sender<(i64, i64)>, remote_input: Option<RemoteDesktopInput>) {
    let handshake = match read_handshake(&mut stream) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[input] failed to read capability handshake: {e}");
            return;
        }
    };
    let daemon_recv_ms = crate::clock_sync::now_millis();
    let _ = clock_tx.send((handshake.android_send_ms, daemon_recv_ms));
    eprintln!(
        "[input] handshake: {}x{} px, pressure {}..{}, tilt {}..{} deg",
        handshake.width,
        handshake.height,
        handshake.pressure_min,
        handshake.pressure_max,
        handshake.tilt_min,
        handshake.tilt_max
    );

    let ranges = TabletRanges {
        width: handshake.width as i32,
        height: handshake.height as i32,
        pressure_max: handshake.pressure_max,
        tilt_min: handshake.tilt_min,
        tilt_max: handshake.tilt_max,
    };

    // Mutually exclusive: either a real uinput tablet, or the reduced-
    // fidelity portal fallback -- never both, decided once by `main.rs`
    // before any portal negotiation happened (the two need different,
    // incompatible portal sessions).
    let tablet = match &remote_input {
        None => match UinputTablet::create(&ranges) {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("[input] failed to create uinput tablet: {e}");
                return;
            }
        },
        Some(_) => None,
    };
    if tablet.is_some() {
        eprintln!("[input] virtual tablet ready, waiting for S Pen input...");
    } else {
        eprintln!(
            "[input] uinput not accessible -- using portal RemoteDesktop input \
             (position + click only, no pressure/tilt), waiting for input..."
        );
    }

    // Edge-triggered like the uinput path's own BTN_TOUCH handling (real
    // hardware, and libinput's own state machine, only expect a button
    // event on an actual state change) -- only relevant to the
    // RemoteDesktop path, which has no kernel input subsystem underneath
    // it to do this for us.
    let mut remote_prev_contact = false;

    let mut event_count = 0u64;
    loop {
        let event_type = match read_u8(&mut stream) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[input] stream ended: {e}");
                break;
            }
        };
        let x = match read_i32(&mut stream) {
            Ok(v) => v,
            Err(_) => break,
        };
        let y = match read_i32(&mut stream) {
            Ok(v) => v,
            Err(_) => break,
        };
        let pressure = match read_i32(&mut stream) {
            Ok(v) => v,
            Err(_) => break,
        };
        let tilt_x = match read_i32(&mut stream) {
            Ok(v) => v,
            Err(_) => break,
        };
        let tilt_y = match read_i32(&mut stream) {
            Ok(v) => v,
            Err(_) => break,
        };
        let buttons = match read_u8(&mut stream) {
            Ok(v) => v,
            Err(_) => break,
        };
        let is_finger = buttons & 0b10 != 0;

        let in_contact = matches!(event_type, EV_DOWN | EV_MOVE);
        if matches!(
            event_type,
            EV_HOVER_ENTER | EV_HOVER_MOVE | EV_HOVER_EXIT | EV_DOWN | EV_MOVE | EV_UP
        ) {
            if let Some(tablet) = &tablet {
                if let Err(e) = tablet.emit(x, y, pressure.max(0), tilt_x, tilt_y, in_contact) {
                    eprintln!("[input] emit failed: {e}");
                }
            } else if let Some(ri) = &remote_input {
                // Scale from the tablet's own panel-pixel space (the
                // handshake's width/height) into the shared ScreenCast
                // stream's logical pixel space -- the portal wants
                // coordinates in the latter, not the former (see
                // `remote_desktop_input.rs`'s module doc).
                let sx = x as f64 * ri.stream_size.0 as f64 / ranges.width.max(1) as f64;
                let sy = y as f64 * ri.stream_size.1 as f64 / ranges.height.max(1) as f64;
                ri.pointer_motion(sx, sy);
                if in_contact != remote_prev_contact {
                    ri.button(in_contact, false);
                    remote_prev_contact = in_contact;
                }
            }
        } else if matches!(event_type, EV_BUTTON_DOWN | EV_BUTTON_UP) {
            let pressed = event_type == EV_BUTTON_DOWN;
            if let Some(tablet) = &tablet {
                if let Err(e) = tablet.set_button(pressed) {
                    eprintln!("[input] set_button failed: {e}");
                }
            } else if let Some(ri) = &remote_input {
                ri.button(pressed, true);
            }
        }

        event_count += 1;
        if event_count == 1 || event_count % 100 == 0 {
            eprintln!(
                "[input] event {event_count}: type={event_type} x={x} y={y} pressure={pressure} finger={is_finger}"
            );
        }
    }
    eprintln!("[input] receiver thread exiting after {event_count} events");
}
