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
use std::sync::mpsc::{Receiver, Sender};

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

/// What the main thread needs out of the handshake: the clock-calibration
/// timestamps, the geometry that drives `orientation::ensure`, and the client's
/// config choices. Sent over a channel because the handshake is read on the
/// input thread but acted on by `main`.
pub struct HandshakeInfo {
    pub android_send_ms: i64,
    pub daemon_recv_ms: i64,
    pub width: u32,
    pub height: u32,
    pub config_flags: u8,
}

pub struct Handshake {
    pub width: u32,
    pub height: u32,
    pub pressure_min: i32,
    pub pressure_max: i32,
    pub tilt_min: i32,
    pub tilt_max: i32,
    pub android_send_ms: i64,
    pub config_flags: u8,
}

fn read_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_be_bytes(b))
}

/// Reads the v2 handshake (see `protocol.rs`).
///
/// The magic and version are checked before anything is believed. v1 had no
/// such marker, so a desynced or foreign peer produced a plausible-looking
/// screen size that then drove `kscreen-doctor` and `uinput` -- the bounds
/// check below was added in Milestone 13 precisely because garbage got that
/// far. It stays as a second line of defence, but the magic is the real one.
///
/// Trailing bytes beyond the fields understood here are skipped using
/// `body_len`, so a newer client that appends config fields still talks to an
/// older daemon.
fn read_handshake(r: &mut impl Read) -> io::Result<Handshake> {
    let magic = read_u32(r)?;
    if magic != crate::protocol::MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "handshake magic was {magic:#010x}, expected {:#010x} -- either a stale/desynced \
                 connection or a client that predates protocol v2",
                crate::protocol::MAGIC
            ),
        ));
    }
    let version = read_u16(r)?;
    if version != crate::protocol::PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "client speaks protocol v{version}, this daemon speaks v{} -- update whichever is older",
                crate::protocol::PROTOCOL_VERSION
            ),
        ));
    }
    let body_len = read_u16(r)? as usize;

    const KNOWN_BODY: usize = 4 + 4 + 4 + 4 + 4 + 4 + 8 + 1;
    if body_len < KNOWN_BODY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("handshake body is {body_len} bytes, need at least {KNOWN_BODY}"),
        ));
    }

    let handshake = Handshake {
        width: read_u32(r)?,
        height: read_u32(r)?,
        pressure_min: read_i32(r)?,
        pressure_max: read_i32(r)?,
        tilt_min: read_i32(r)?,
        tilt_max: read_i32(r)?,
        android_send_ms: read_i64(r)?,
        config_flags: read_u8(r)?,
    };

    // Fields this build doesn't know about yet.
    let mut skip = vec![0u8; body_len - KNOWN_BODY];
    if !skip.is_empty() {
        r.read_exact(&mut skip)?;
        eprintln!("[input] skipped {} trailing handshake byte(s) from a newer client", skip.len());
    }
    Ok(handshake)
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
/// `clock_tx`: sends a `HandshakeInfo` the
/// instant the handshake is read, so the main thread (which owns the
/// video-writing half of the socket) can complete Milestone 7's clock-offset
/// calibration handshake -- see `clock_sync.rs` -- and, separately, rotate
/// the virtual monitor to match the tablet's aspect before ever opening the
/// portal (see `orientation.rs`, Milestone 15). The clock fields aren't a
/// capability, just piggybacking on the same channel.
///
/// `remote_input_rx`: `None` selects the normal full-fidelity uinput tablet
/// path immediately. `Some` means uinput wasn't accessible (see
/// `uinput_tablet::uinput_accessible`) and this session is running the
/// reduced-fidelity `RemoteDesktop` portal fallback instead (position +
/// click, no pressure/tilt) -- see `remote_desktop_input.rs`. That portal
/// session can't be negotiated until *after* the virtual monitor is
/// rotated (which needs this handshake first), so in that case this call
/// blocks here until `main.rs` sends the negotiated `RemoteDesktopInput`
/// handle down the channel -- nothing useful to do before that exists
/// anyway.
pub fn run(
    mut stream: TransportReader,
    clock_tx: Sender<HandshakeInfo>,
    remote_input_rx: Option<Receiver<RemoteDesktopInput>>,
) {
    let handshake = match read_handshake(&mut stream) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[input] failed to read capability handshake: {e}");
            return;
        }
    };

    // Sanity bounds, not a real capability limit: reusing an
    // already-in-accessory-mode device (see `aoa::connect`'s "already in
    // accessory mode, reusing it" path) can desync framing if Android
    // doesn't resend a fresh handshake at the same instant this side starts
    // reading -- confirmed live (Milestone 13), this landed leftover stream
    // bytes here and produced `handshake: 67108868x369098755 px`. Harmless
    // on its own (uinput's ioctl validation clamps it), but width/height
    // now also drive `orientation::set_rotation`, which shells out to
    // `kscreen-doctor` and directly reconfigures the real KWin output with
    // no validation of its own. No real tablet panel is anywhere near this
    // range in either direction.
    const MIN_DIM: u32 = 64;
    const MAX_DIM: u32 = 16384;
    if !(MIN_DIM..=MAX_DIM).contains(&handshake.width) || !(MIN_DIM..=MAX_DIM).contains(&handshake.height) {
        eprintln!(
            "[input] handshake reports {}x{} px -- outside the sane {MIN_DIM}..={MAX_DIM} range, \
             almost certainly a corrupted/stale read rather than a real panel size. Refusing to act \
             on it (dropping this connection so the client reconnects with a fresh handshake).",
            handshake.width, handshake.height
        );
        return;
    }

    let daemon_recv_ms = crate::clock_sync::now_millis();
    let _ = clock_tx.send(HandshakeInfo {
        android_send_ms: handshake.android_send_ms,
        daemon_recv_ms,
        width: handshake.width,
        height: handshake.height,
        config_flags: handshake.config_flags,
    });
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

    let remote_input = match remote_input_rx {
        None => None,
        Some(rx) => {
            eprintln!("[input] waiting for the portal RemoteDesktop session to negotiate...");
            match rx.recv() {
                Ok(ri) => Some(ri),
                Err(e) => {
                    eprintln!("[input] never received the RemoteDesktop input handle: {e}");
                    return;
                }
            }
        }
    };

    // Mutually exclusive: either a real uinput tablet, or the reduced-
    // fidelity portal fallback -- never both.
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

    // Mirrors vaapi_encoder.rs's flip_180: this machine's USB cable
    // position needs portrait video flipped 180 degrees, applied GPU-side
    // since KWin's own rotation property does nothing for this output type
    // (Milestone 16). Reflecting touch/pen x,y here keeps the two in sync
    // -- computed independently, same formula, same handshake dims, since
    // this thread reads the handshake before main.rs even knows them.
    let flip_180 = ranges.height > ranges.width;

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
        let (x, y) = if flip_180 { (ranges.width - x, ranges.height - y) } else { (x, y) };
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
