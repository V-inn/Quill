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
//!   u8  event_type   (0=hover_enter 1=hover_move 2=hover_exit 3=down 4=move 5=up 6=button_down 7=button_up
//!                     8=touch_down 9=touch_move 10=touch_up 11=right_down 12=right_up)
//!   i32 x_px
//!   i32 y_px
//!   i32 pressure     (touch_* types: the multitouch **slot** instead)
//!   i32 tilt_x_deg
//!   i32 tilt_y_deg
//!   u8  buttons      (bit0 = stylus primary button state, informational --
//!                     the actual BTN_STYLUS toggle is driven by the
//!                     explicit button_down/button_up event types, not this
//!                     bit; bit1 = tool is a finger, not the S Pen.
//!                     touch_* types: the live contact count instead)
//!
//! # Multi-touch (Milestone 9)
//!
//! Types 0-7 are the single-pointer path and are unchanged: the pen, and a
//! lone finger, both position the cursor absolutely through the tablet device.
//! The moment a second finger lands, the client switches to types 8-10 and
//! reports every contact as a real multitouch slot, which goes to a *separate*
//! device (`uinput_touchpad.rs`) that libinput classifies as a touchpad and
//! recognizes gestures on. Milestone 6b established why this cannot be the
//! same device: `BTN_TOOL_FINGER` on the tablet stopped the cursor moving at
//! all.
//!
//! Types 11-12 are a right click at the current position -- the client's
//! long-press. Neither the tablet (whose only button is the pen's barrel) nor
//! the touchpad's own tap handling covers a press-and-hold on a device where
//! one finger is an absolute pointer, so it is recognized on the Android side
//! and injected through `uinput_buttons.rs`.

use crate::gesture::{Classification, Gesture, Recognizer, ZoomAccumulator};
use crate::portal_capture::TransportReader;
use crate::remote_desktop_input::RemoteDesktopInput;
use crate::uinput_buttons::UinputButtons;
use crate::uinput_tablet::{TabletRanges, UinputTablet};
use crate::uinput_touchpad::{TouchpadGeometry, UinputTouchpad, MAX_SLOTS};
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
    /// Physical pixels per inch * 1000, or `None` from a client that predates
    /// the field. Only the touchpad device needs it.
    pub dpi: Option<(i32, i32)>,
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
    let mut handshake = Handshake {
        width: read_u32(&mut body)?,
        height: read_u32(&mut body)?,
        pressure_min: read_i32(&mut body)?,
        pressure_max: read_i32(&mut body)?,
        tilt_min: read_i32(&mut body)?,
        tilt_max: read_i32(&mut body)?,
        android_send_ms: read_i64(&mut body)?,
        config_flags: read_u8(&mut body)?,
        dpi: None,
    };
    // Appended in Milestone 9, and optional exactly as `body_len` promises:
    // a client that predates it still talks to this daemon, it just doesn't
    // get a physically-calibrated touchpad.
    if body_len >= KNOWN_BODY + 8 {
        handshake.dpi = Some((read_i32(&mut body)?, read_i32(&mut body)?));
    }

    if !(MIN_DIM..=MAX_DIM).contains(&handshake.width)
        || !(MIN_DIM..=MAX_DIM).contains(&handshake.height)
    {
        eprintln!(
            "[input] rejecting handshake candidate: {}x{} px is outside the sane {MIN_DIM}..={MAX_DIM} \
             range, so this is a corrupted read rather than a real panel size",
            handshake.width, handshake.height
        );
        return Ok(None);
    }

    let consumed = KNOWN_BODY + if handshake.dpi.is_some() { 8 } else { 0 };
    if body_len > consumed {
        eprintln!(
            "[input] skipped {} trailing handshake byte(s) from a newer client",
            body_len - consumed
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
        eprintln!(
            "[input] handshake candidate {attempt}/{MAX_HANDSHAKE_ATTEMPTS} rejected, resyncing..."
        );
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
const EV_TOUCH_DOWN: u8 = 8;
const EV_TOUCH_MOVE: u8 = 9;
const EV_TOUCH_UP: u8 = 10;
const EV_RIGHT_DOWN: u8 = 11;
const EV_RIGHT_UP: u8 = 12;

/// Highest valid event type. Milestone 11's desync detector treats anything
/// above this as evidence the stream is misaligned, so it has to move whenever
/// a type is added -- otherwise every record of the new type reads as garbage
/// and three in a row drop the connection.
const EV_MAX: u8 = EV_RIGHT_UP;

/// Consecutive out-of-range event types tolerated before the stream is called
/// desynced. One is survivable noise; three in a row is a wrong read offset.
const MAX_BAD_EVENTS: u32 = 3;


/// Maps a point in the tablet's own pixel space onto the desktop's logical
/// coordinate space, which is where the pointer lives.
///
/// Resolved lazily, on the first warp, rather than at startup: `main` is
/// meanwhile running `orientation::ensure`, which tears the virtual output down
/// and recreates it when the shape changed. Reading the layout during that
/// window sees a desktop with no virtual output in it at all -- confirmed live,
/// the first version of this logged "couldn't read the desktop layout" every
/// time the monitor was recreated. By the time a finger is on the glass, the
/// output is up.
struct PointerMap {
    layout: crate::orientation::DesktopLayout,
    tablet_w: f64,
    tablet_h: f64,
}

impl PointerMap {
    fn to_desktop(&self, x: i32, y: i32) -> (i32, i32) {
        let out = self.layout.output;
        let desktop = self.layout.desktop;
        let dx = out.x + (x as f64 / self.tablet_w).clamp(0.0, 1.0) * out.w;
        let dy = out.y + (y as f64 / self.tablet_h).clamp(0.0, 1.0) * out.h;
        // The device's axes start at 0, so subtract the desktop origin, which
        // is not necessarily (0,0) -- an output above or left of the primary
        // one puts it negative.
        ((dx - desktop.x).round() as i32, (dy - desktop.y).round() as i32)
    }
}

/// Owns the pointer-warping half of the input path: the lazily-resolved screen
/// mapping, and the device that does the warping.
///
/// Warping is the fix for gestures landing on whichever screen the mouse
/// pointer was last left on. Wheel and button events are delivered to whatever
/// the pointer is over; the virtual touchpad is relative and only ever sees two
/// contacts or more, so it never emits pointer motion at all, and the tablet's
/// tool cursor is tracked separately from the pointer. Nothing in the chain was
/// moving the pointer, so it stayed where the mouse had left it.
struct Pointer {
    device: Option<UinputButtons>,
    map: Option<PointerMap>,
    resolved: bool,
    tablet_w: f64,
    tablet_h: f64,
}

impl Pointer {
    fn new(tablet_w: i32, tablet_h: i32) -> Self {
        Self {
            device: None,
            map: None,
            resolved: false,
            tablet_w: tablet_w.max(1) as f64,
            tablet_h: tablet_h.max(1) as f64,
        }
    }

    fn map(&mut self) -> Option<&PointerMap> {
        if !self.resolved {
            self.resolved = true;
            self.map = crate::orientation::layout().map(|layout| {
                eprintln!(
                    "[input] pointer warp target: output {}x{} at ({}, {}) in a {}x{} desktop (logical)",
                    layout.output.w.round(),
                    layout.output.h.round(),
                    layout.output.x.round(),
                    layout.output.y.round(),
                    layout.desktop.w.round(),
                    layout.desktop.h.round()
                );
                PointerMap { layout, tablet_w: self.tablet_w, tablet_h: self.tablet_h }
            });
            if self.map.is_none() {
                eprintln!(
                    "[input] couldn't read the desktop layout from kscreen-doctor -- gestures and \
                     long-press clicks will land wherever the pointer already is"
                );
            }
        }
        self.map.as_ref()
    }

    /// The absolute space the device was created against. Falls back to a
    /// degenerate 1x1 when the layout is unknown, which is harmless: with no
    /// map there is nothing to warp to either, and only the wheel and button
    /// halves of the device get used.
    fn desktop(&mut self) -> crate::orientation::Rect {
        self.map()
            .map(|m| m.layout.desktop)
            .unwrap_or(crate::orientation::Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 })
    }

    /// Lazily creates the device -- it exists only when something actually
    /// needs it (a warp, a right click, ctrl+scroll zoom), so a session that
    /// uses none of those presents two devices rather than three.
    fn device(&mut self) -> Option<&UinputButtons> {
        if self.device.is_none() {
            let desktop = self.desktop();
            match UinputButtons::create(desktop) {
                Ok(b) => {
                    eprintln!(
                        "[input] virtual buttons device created (pointer warp / right click / ctrl+scroll zoom)"
                    );
                    self.device = Some(b);
                }
                Err(e) => eprintln!("[input] failed to create the buttons device: {e}"),
            }
        }
        self.device.as_ref()
    }

    fn warp_to(&mut self, x: i32, y: i32) {
        let Some((dx, dy)) = self.map().map(|m| m.to_desktop(x, y)) else { return };
        if let Some(b) = self.device() {
            if let Err(e) = b.warp(dx, dy) {
                eprintln!("[input] pointer warp failed: {e}");
            }
        }
    }
}

/// One touch record onto the touchpad device. Split out because the
/// ctrl+scroll path has to be able to *replay* records it held back, which
/// means emitting them from two places.
fn forward_touch(
    touchpad: &UinputTouchpad,
    event_type: u8,
    slot: usize,
    x: i32,
    y: i32,
) -> io::Result<()> {
    match event_type {
        EV_TOUCH_DOWN => touchpad.touch_down(slot, x, y),
        EV_TOUCH_MOVE => touchpad.touch_move(slot, x, y),
        _ => touchpad.touch_up(slot),
    }
}

/// Fallback when a client predates the handshake's dpi fields. 10 units/mm is
/// roughly a 254-dpi panel, which is the right order of magnitude for every
/// tablet this runs on -- close enough that libinput's millimetre thresholds
/// stay sane, and only used when the real number isn't available.
const DEFAULT_UNITS_PER_MM: i32 = 10;

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
        eprintln!(
            "[input] resynced after {discarded} discarded byte(s) of stale/garbage stream data"
        );
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
    // Milestone 9: multi-finger contacts go to their own device, which
    // libinput classifies as a touchpad and does the gesture recognition on.
    // Created alongside the tablet rather than lazily -- a device appearing
    // mid-gesture would miss the contacts that started it, and libinput needs
    // a gesture from its first frame.
    let (res_x, res_y) = match handshake.dpi {
        // dpi/25.4 = pixels per millimetre, from the *_milli fields.
        Some((x, y)) => (
            ((x as f32 / 1000.0) / 25.4).round().max(1.0) as i32,
            ((y as f32 / 1000.0) / 25.4).round().max(1.0) as i32,
        ),
        None => {
            eprintln!(
                "[input] client sent no dpi in its handshake -- assuming {DEFAULT_UNITS_PER_MM} units/mm \
                 for the touchpad, so gesture thresholds may feel off"
            );
            (DEFAULT_UNITS_PER_MM, DEFAULT_UNITS_PER_MM)
        }
    };
    let touchpad = match &tablet {
        None => None,
        Some(_) => {
            let geometry = TouchpadGeometry {
                width: ranges.width,
                height: ranges.height,
                res_x,
                res_y,
            };
            match UinputTouchpad::create(&geometry) {
                Ok(t) => {
                    eprintln!(
                        "[input] virtual touchpad ready: {}x{} px at {res_x}x{res_y} units/mm (~{}x{} mm)",
                        ranges.width,
                        ranges.height,
                        ranges.width / res_x.max(1),
                        ranges.height / res_y.max(1)
                    );
                    Some(t)
                }
                Err(e) => {
                    // Not fatal, unlike the tablet: pen and single-finger input
                    // still work without it, and losing gestures is worth less
                    // than losing the session.
                    eprintln!(
                        "[input] failed to create the virtual touchpad ({e}) -- gestures disabled"
                    );
                    None
                }
            }
        }
    };

    let ctrl_scroll_zoom = handshake.config_flags & crate::protocol::CONFIG_CTRL_SCROLL_ZOOM != 0;
    let mut pointer = Pointer::new(ranges.width, ranges.height);
    let mut recognizer = Recognizer::new(res_x, res_y);
    let mut zoom = ZoomAccumulator::new();
    // Contacts withheld from the touchpad while the recognizer decides whether
    // this is a pinch. Replayed if it turns out to be a scroll, dropped if it
    // turns out to be a pinch -- see the routing below.
    let mut withheld: Vec<(u8, usize, i32, i32)> = Vec::new();

    if tablet.is_some() {
        eprintln!(
            "[input] virtual tablet ready, waiting for S Pen input... (zoom: {})",
            if ctrl_scroll_zoom {
                "ctrl+scroll"
            } else {
                "native pinch gesture"
            }
        );
    } else {
        eprintln!(
            "[input] uinput not accessible -- using portal RemoteDesktop input \
             (position + click only, no pressure/tilt, no gestures yet), waiting for input..."
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
        let (x, y) = if flip_180 {
            (ranges.width - x, ranges.height - y)
        } else {
            (x, y)
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

        // Upstream records are raw fixed-size structs with no framing of their
        // own, so a desync here is permanent: every subsequent read is 22 bytes
        // taken at the wrong offset. Before this check an out-of-range type
        // (Milestone 11 logged `type=51`) fell through the arms below and the
        // loop went on consuming misaligned records forever, injecting nothing.
        // One bad record can be a bit flip; several in a row is a desync, and
        // the only fix from here is a fresh connection with a fresh handshake.
        if event_type > EV_MAX {
            consecutive_bad += 1;
            eprintln!(
                "[input] event type {event_type} is outside the valid 0..={EV_MAX} range \
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
        } else if matches!(event_type, EV_TOUCH_DOWN | EV_TOUCH_MOVE | EV_TOUCH_UP) {
            // The `pressure` field carries the slot for these types, and
            // `buttons` the contact count -- see the module doc.
            let slot = pressure.clamp(0, MAX_SLOTS as i32 - 1) as usize;
            let Some(touchpad) = &touchpad else {
                // No touchpad device: the portal fallback has no gesture path
                // yet (Milestone 10), and dropping these is better than
                // feeding multi-finger contacts to an absolute pointer.
                continue;
            };

            // Feed the recognizer first, always: it is what decides whether a
            // pinch should be withheld from the touchpad, and it needs every
            // frame of the gesture to do that, not just the ones after the
            // decision.
            let outcome = match event_type {
                EV_TOUCH_DOWN => {
                    let was = recognizer.contact_count();
                    recognizer.down(slot, x, y);
                    // The gesture is starting: put the pointer between the two
                    // fingers first, so the scroll or zoom that follows is
                    // delivered to what is under them rather than to whatever
                    // window the mouse pointer was last left on.
                    if was < 2 {
                        if let Some((cx, cy)) = recognizer.centroid() {
                            pointer.warp_to(cx, cy);
                        }
                    }
                    None
                }
                EV_TOUCH_MOVE => recognizer.motion(slot, x, y),
                _ => {
                    recognizer.up(slot);
                    zoom.reset();
                    None
                }
            };

            if !ctrl_scroll_zoom {
                // Native-gesture mode: libinput sees everything and decides
                // for itself. The recognizer above is running but nothing
                // acts on it.
                if let Err(e) = forward_touch(touchpad, event_type, slot, x, y) {
                    eprintln!("[input] touchpad emit failed: {e}");
                }
            } else {
                match recognizer.classification() {
                    // Still undecided: hold the contacts back rather than commit
                    // them to the touchpad, since a pinch must never reach it.
                    Classification::Undecided => {
                        withheld.push((event_type, slot, x, y));
                    }
                    Classification::Scroll => {
                        // Decided against us intercepting: replay whatever was
                        // held back, in order, then pass through from here on.
                        for (t, s, hx, hy) in withheld.drain(..) {
                            if let Err(e) = forward_touch(touchpad, t, s, hx, hy) {
                                eprintln!("[input] touchpad replay failed: {e}");
                            }
                        }
                        if let Err(e) = forward_touch(touchpad, event_type, slot, x, y) {
                            eprintln!("[input] touchpad emit failed: {e}");
                        }
                    }
                    Classification::Pinch => {
                        // The contacts never reach the touchpad at all, so
                        // libinput sees no gesture and nothing double-counts.
                        withheld.clear();
                        if let Some(Gesture::Pinch { ratio }) = outcome {
                            let clicks = zoom.feed(ratio);
                            if clicks != 0 {
                                if let Some(b) = pointer.device() {
                                    let _ = b.set_ctrl(true);
                                    if let Err(e) = b.wheel(clicks) {
                                        eprintln!("[input] wheel emit failed: {e}");
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Ctrl is held only for as long as the pinch is: releasing it on
            // the last lift keeps a dropped connection from leaving the whole
            // desktop in a ctrl-stuck state.
            if recognizer.contact_count() == 0 {
                withheld.clear();
                if let Some(b) = &pointer.device {
                    let _ = b.set_ctrl(false);
                }
            }
        } else if matches!(event_type, EV_RIGHT_DOWN | EV_RIGHT_UP) {
            let pressed = event_type == EV_RIGHT_DOWN;
            if let Some(ri) = &remote_input {
                ri.button(pressed, true);
            } else {
                // Aim first, click second: the finger's own absolute motion
                // went to the tablet device, which does not move the pointer
                // that button events are delivered to.
                if pressed {
                    pointer.warp_to(x, y);
                }
                if let Some(b) = pointer.device() {
                    if let Err(e) = b.set_right_button(pressed) {
                        eprintln!("[input] right button emit failed: {e}");
                    }
                }
            }
        }

        event_count += 1;
        if event_count == 1 || event_count % 100 == 0 {
            eprintln!(
                "[input] event {event_count}: type={event_type} x={x} y={y} pressure={pressure} finger={is_finger}"
            );
        }
    }
    // Whatever was mid-gesture when the stream died must not stay pressed:
    // contacts left down read as phantom fingers on the pad, and a held ctrl
    // affects the whole desktop, not just this session.
    if let Some(touchpad) = &touchpad {
        let _ = touchpad.release_all();
    }
    if let Some(b) = &pointer.device {
        let _ = b.release_all();
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
    fn reads_the_appended_dpi_fields_when_present() {
        let mut bytes = handshake_bytes(2560, 1600);
        let new_len = (KNOWN_BODY + 8) as u16;
        bytes[6..8].copy_from_slice(&new_len.to_be_bytes());
        bytes.extend_from_slice(&243_000i32.to_be_bytes());
        bytes.extend_from_slice(&243_000i32.to_be_bytes());
        let (h, _) = read_handshake(&mut io::Cursor::new(bytes)).unwrap();
        assert_eq!(h.dpi, Some((243_000, 243_000)));
    }

    #[test]
    fn treats_dpi_as_absent_for_a_client_that_predates_it() {
        // The forward-compat contract in reverse: a newer daemon, an older
        // client, and the touchpad falls back to an assumed resolution rather
        // than reading whatever follows.
        let (h, _) = read_handshake(&mut io::Cursor::new(handshake_bytes(2560, 1600))).unwrap();
        assert_eq!(h.dpi, None);
    }

    /// The layout this machine actually reports: a 1920x1080 panel at scale
    /// 1.25 (1536x864 logical) with the 2560x1600 virtual output at scale 1.5
    /// (1707x1067 logical) to its right.
    fn pointer_map() -> PointerMap {
        use crate::orientation::{DesktopLayout, Rect};
        PointerMap {
            layout: DesktopLayout {
                output: Rect { x: 1536.0, y: 0.0, w: 1707.0, h: 1067.0 },
                desktop: Rect { x: 0.0, y: 0.0, w: 3243.0, h: 1067.0 },
            },
            tablet_w: 2560.0,
            tablet_h: 1600.0,
        }
    }

    #[test]
    fn maps_tablet_corners_onto_the_virtual_output() {
        let map = pointer_map();
        assert_eq!(map.to_desktop(0, 0), (1536, 0));
        assert_eq!(map.to_desktop(2560, 1600), (3243, 1067));
        // Dead centre of the tablet lands dead centre of that output, not of
        // the desktop -- the whole point of the mapping.
        assert_eq!(map.to_desktop(1280, 800), (2390, 534));
    }

    #[test]
    fn clamps_coordinates_from_outside_the_panel() {
        // A corrupted or over-range coordinate must not aim the pointer at
        // another screen entirely.
        let map = pointer_map();
        assert_eq!(map.to_desktop(-500, -500), (1536, 0));
        assert_eq!(map.to_desktop(99999, 99999), (3243, 1067));
    }

    #[test]
    fn subtracts_a_negative_desktop_origin() {
        use crate::orientation::{DesktopLayout, Rect};
        // An output placed above/left of the primary one puts the desktop
        // origin negative, while the device's own axes still start at 0.
        let map = PointerMap {
            layout: DesktopLayout {
                output: Rect { x: -1707.0, y: -200.0, w: 1707.0, h: 1067.0 },
                desktop: Rect { x: -1707.0, y: -200.0, w: 3243.0, h: 1267.0 },
            },
            tablet_w: 2560.0,
            tablet_h: 1600.0,
        };
        assert_eq!(map.to_desktop(0, 0), (0, 0));
    }

    #[test]
    fn gives_up_on_a_stream_that_never_contains_a_handshake() {
        let bytes = vec![0x5Au8; 4096];
        assert!(read_handshake(&mut io::Cursor::new(bytes)).is_err());
    }
}
