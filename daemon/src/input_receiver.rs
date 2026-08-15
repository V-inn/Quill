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

const KNOWN_BODY: usize = 4 + 4 + 4 + 4 + 4 + 4 + 8 + 1;
/// Nothing legitimate is anywhere near this; a larger `body_len` is a corrupt
/// read, not a newer client with a lot to say.
const MAX_BODY: usize = 1024;

/// Sanity bounds on the reported panel size, not a real capability limit.
/// Milestone 13: a desynced read produced `67108868x369098755 px`, and these
/// dimensions drive both `uinput` and `orientation::set_rotation`, which shells
/// out to `kscreen-doctor` and reconfigures the real KWin output with no
/// validation of its own. No tablet panel is near this range in either
/// direction.
const MIN_DIM: u32 = 64;
const MAX_DIM: u32 = 16384;

/// How much garbage to walk past looking for a handshake before giving up. A
/// stale-USB-data desync is a few hundred bytes at most; a megabyte means this
/// is not a Quill client at all.
const MAX_RESYNC_BYTES: usize = 1 << 20;
/// Bounded so a stream of plausible-looking `"QUIL"` noise can't spin forever.
const MAX_HANDSHAKE_ATTEMPTS: usize = 8;

/// Walks the stream one byte at a time until the last four read are `MAGIC`,
/// returning how many bytes were thrown away to get there.
///
/// This is what protocol v2's magic was *for*. Before it, a desynced connection
/// was undetectable and got interpreted as a screen size (Milestone 13); with
/// it, the daemon could at least diagnose the problem, but still only by dying.
/// Stale queued USB bulk data surviving a reconnect is the known remaining
/// cause (MILESTONES.md Milestone 11) and it is recoverable: the real handshake
/// is sitting right behind the garbage.
fn sync_to_magic(r: &mut impl Read) -> io::Result<usize> {
    let mut window = [0u8; 4];
    r.read_exact(&mut window)?;
    let mut discarded = 0usize;
    while u32::from_be_bytes(window) != crate::protocol::MAGIC {
        if discarded >= MAX_RESYNC_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no handshake magic in the first {MAX_RESYNC_BYTES} bytes -- not a Quill client"),
            ));
        }
        window.rotate_left(1);
        window[3] = read_u8(r)?;
        discarded += 1;
    }
    Ok(discarded)
}

/// Reads everything after the magic. `Ok(None)` means "this candidate isn't a
/// real handshake, keep scanning" -- the body is consumed either way, so the
/// stream stays aligned for the next attempt.
///
/// Trailing bytes beyond the fields understood here are skipped using
/// `body_len`, so a newer client that appends config fields still talks to an
/// older daemon.
fn read_handshake_body(r: &mut impl Read) -> io::Result<Option<Handshake>> {
    let version = read_u16(r)?;
    let body_len = read_u16(r)? as usize;

    // Length first, so the body can be consumed even when the version is the
    // thing that's wrong -- otherwise a rejected candidate would leave its own
    // body in the stream for the next scan to trip over.
    if !(KNOWN_BODY..=MAX_BODY).contains(&body_len) {
        eprintln!("[input] rejecting handshake candidate: body_len {body_len} outside {KNOWN_BODY}..={MAX_BODY}");
        return Ok(None);
    }
    let mut body = vec![0u8; body_len];
    r.read_exact(&mut body)?;

    if version != crate::protocol::PROTOCOL_VERSION {
        eprintln!(
            "[input] rejecting handshake candidate: client speaks protocol v{version}, this daemon \
             speaks v{} -- update whichever is older (or this was corrupted bytes that happened to \
             contain the magic)",
            crate::protocol::PROTOCOL_VERSION
        );
        return Ok(None);
    }

    let mut body = io::Cursor::new(&body[..]);
    let handshake = Handshake {
        width: read_u32(&mut body)?,
        height: read_u32(&mut body)?,
        pressure_min: read_i32(&mut body)?,
        pressure_max: read_i32(&mut body)?,
        tilt_min: read_i32(&mut body)?,
        tilt_max: read_i32(&mut body)?,
        android_send_ms: read_i64(&mut body)?,
        config_flags: read_u8(&mut body)?,
    };

    if !(MIN_DIM..=MAX_DIM).contains(&handshake.width) || !(MIN_DIM..=MAX_DIM).contains(&handshake.height) {
        eprintln!(
            "[input] rejecting handshake candidate: {}x{} px is outside the sane {MIN_DIM}..={MAX_DIM} \
             range, so this is a corrupted read rather than a real panel size",
            handshake.width, handshake.height
        );
        return Ok(None);
    }

    if body_len > KNOWN_BODY {
        eprintln!(
            "[input] skipped {} trailing handshake byte(s) from a newer client",
            body_len - KNOWN_BODY
        );
    }
    Ok(Some(handshake))
}

/// Reads the v2 handshake (see `protocol.rs`), resyncing past leading garbage
/// instead of treating it as fatal. Returns the handshake and how many bytes
/// were discarded ahead of it.
///
/// Before this, any of the three rejections below returned an error, the input
/// thread exited, the `clock_tx` sender dropped, and `setup_transport`'s
/// `recv_timeout` saw `Disconnected` and exited the process -- so a few stale
/// bytes cost a full systemd restart plus (on AOA) another device scan. The
/// bytes worth having were in the buffer the whole time.
fn read_handshake(r: &mut impl Read) -> io::Result<(Handshake, usize)> {
    let mut discarded = 0usize;
    for attempt in 1..=MAX_HANDSHAKE_ATTEMPTS {
        discarded += sync_to_magic(r)?;
        if let Some(handshake) = read_handshake_body(r)? {
            return Ok((handshake, discarded));
        }
        eprintln!("[input] handshake candidate {attempt}/{MAX_HANDSHAKE_ATTEMPTS} rejected, resyncing...");
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("no valid handshake in {MAX_HANDSHAKE_ATTEMPTS} attempts"),
    ))
}

const EV_HOVER_ENTER: u8 = 0;
const EV_HOVER_MOVE: u8 = 1;
const EV_HOVER_EXIT: u8 = 2;
const EV_DOWN: u8 = 3;
const EV_MOVE: u8 = 4;
const EV_UP: u8 = 5;
const EV_BUTTON_DOWN: u8 = 6;
const EV_BUTTON_UP: u8 = 7;

/// Consecutive out-of-range event types tolerated before the stream is called
/// desynced. One is survivable noise; three in a row is a wrong read offset.
const MAX_BAD_EVENTS: u32 = 3;

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
    let (handshake, discarded) = match read_handshake(&mut stream) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[input] failed to read capability handshake: {e}");
            return;
        }
    };
    if discarded > 0 {
        // The stale-USB-data race from Milestone 11: reusing an
        // already-in-accessory-mode device (see `aoa::connect`'s "already in
        // accessory mode, reusing it" path) can leave a previous session's
        // bytes queued ahead of the real handshake. Recovered rather than
        // fatal now, but still worth counting -- if this number is ever large
        // or growing, the `clear_halt`/reset work in `aoa.rs` is the lever.
        eprintln!("[input] resynced after {discarded} discarded byte(s) of stale/garbage stream data");
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
                    std::process::exit(1);
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
                // Milestone 11 saw this fail with "Invalid argument" after a
                // corrupted handshake. Exiting (rather than returning and
                // leaving video streaming with no input at all) puts the
                // restart in systemd's hands, where every other fatal
                // transport problem in this daemon already lives.
                eprintln!("[input] failed to create uinput tablet: {e}");
                std::process::exit(1);
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
    let mut consecutive_bad = 0u32;
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

        // Upstream records are raw fixed-size structs with no framing of their
        // own, so a desync here is permanent: every subsequent read is 22 bytes
        // taken at the wrong offset. Before this check an out-of-range type
        // (Milestone 11 logged `type=51`) fell through the arms below and the
        // loop went on consuming misaligned records forever, injecting nothing.
        // One bad record can be a bit flip; several in a row is a desync, and
        // the only fix from here is a fresh connection with a fresh handshake.
        if event_type > EV_BUTTON_UP {
            consecutive_bad += 1;
            eprintln!(
                "[input] event type {event_type} is outside the valid 0..={EV_BUTTON_UP} range \
                 ({consecutive_bad}/{MAX_BAD_EVENTS} consecutive)"
            );
            if consecutive_bad >= MAX_BAD_EVENTS {
                eprintln!("[input] stream is desynced -- dropping the connection so the client re-handshakes");
                break;
            }
            continue;
        }
        consecutive_bad = 0;

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

    // A daemon that keeps streaming video with no input is not usefully alive:
    // the tablet is a drawing surface, and this is exactly the state Milestone
    // 11 recorded as "input can end up silently wrong for that session until
    // the next reconnect cycles it out on its own". Exit so systemd restarts
    // us with a clean transport and a fresh handshake -- the same recovery the
    // five transport-write failures in `portal_capture.rs` already use.
    // Ctrl-C is the one case where the stream ending is expected: leave the
    // main thread alone so it can print its summary.
    if !crate::sigint_received() {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed v2 handshake, as `MainActivity.sendHandshake` writes it.
    fn handshake_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&width.to_be_bytes());
        body.extend_from_slice(&height.to_be_bytes());
        body.extend_from_slice(&0i32.to_be_bytes()); // pressure_min
        body.extend_from_slice(&4095i32.to_be_bytes()); // pressure_max
        body.extend_from_slice(&0i32.to_be_bytes()); // tilt_min
        body.extend_from_slice(&90i32.to_be_bytes()); // tilt_max
        body.extend_from_slice(&1_700_000_000_000i64.to_be_bytes());
        body.push(0); // config_flags

        let mut out = Vec::new();
        out.extend_from_slice(&crate::protocol::MAGIC.to_be_bytes());
        out.extend_from_slice(&crate::protocol::PROTOCOL_VERSION.to_be_bytes());
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn reads_a_clean_handshake_with_nothing_discarded() {
        let bytes = handshake_bytes(1848, 2960);
        let (h, discarded) = read_handshake(&mut io::Cursor::new(bytes)).unwrap();
        assert_eq!((h.width, h.height), (1848, 2960));
        assert_eq!(h.pressure_max, 4095);
        assert_eq!(discarded, 0);
    }

    #[test]
    fn walks_past_leading_garbage() {
        // The Milestone 11 case: a previous session's queued bytes ahead of the
        // real handshake.
        let mut bytes = vec![0xAB; 37];
        bytes.extend_from_slice(&handshake_bytes(1848, 2960));
        let (h, discarded) = read_handshake(&mut io::Cursor::new(bytes)).unwrap();
        assert_eq!((h.width, h.height), (1848, 2960));
        assert_eq!(discarded, 37);
    }

    #[test]
    fn skips_a_candidate_with_an_implausible_screen_size() {
        // The exact garbage Milestone 13 recorded, magic and all, followed by
        // the real thing.
        let mut bad = handshake_bytes(67_108_868, 369_098_755);
        bad.extend_from_slice(&handshake_bytes(1848, 2960));
        let (h, _) = read_handshake(&mut io::Cursor::new(bad)).unwrap();
        assert_eq!((h.width, h.height), (1848, 2960));
    }

    #[test]
    fn skips_a_candidate_with_a_bogus_body_len_without_losing_alignment() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&crate::protocol::MAGIC.to_be_bytes());
        bytes.extend_from_slice(&crate::protocol::PROTOCOL_VERSION.to_be_bytes());
        bytes.extend_from_slice(&7u16.to_be_bytes()); // shorter than KNOWN_BODY
        bytes.extend_from_slice(&handshake_bytes(1848, 2960));
        let (h, _) = read_handshake(&mut io::Cursor::new(bytes)).unwrap();
        assert_eq!((h.width, h.height), (1848, 2960));
    }

    #[test]
    fn accepts_trailing_fields_from_a_newer_client() {
        let mut bytes = handshake_bytes(1848, 2960);
        // Bump body_len and append two bytes this build doesn't know about.
        let new_len = (KNOWN_BODY + 2) as u16;
        bytes[6..8].copy_from_slice(&new_len.to_be_bytes());
        bytes.extend_from_slice(&[0xEE, 0xEE]);
        let (h, _) = read_handshake(&mut io::Cursor::new(bytes)).unwrap();
        assert_eq!((h.width, h.height), (1848, 2960));
    }

    #[test]
    fn gives_up_on_a_stream_that_never_contains_a_handshake() {
        let bytes = vec![0x5Au8; 4096];
        assert!(read_handshake(&mut io::Cursor::new(bytes)).is_err());
    }
}
