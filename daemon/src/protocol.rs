//! The Quill wire protocol, v2. Both directions, in one place.
//!
//! # Why this exists as its own module
//!
//! v1 was three different framings sharing one connection: a fixed 32-byte
//! handshake going up, then -- coming down -- an unframed 24-byte clock-sync
//! reply, an unframed 8-byte video-format header, and only then a stream of
//! length-prefixed frames. Nothing on the wire said which was which, so any
//! disagreement about how many bytes had been consumed was permanent and
//! silent. That cost this project real time repeatedly (MILESTONES.md,
//! Milestones 8, 13, 14, 17, and again in Milestone 18 when a second
//! video-format header slipped in and the client spent the rest of the session
//! reading frame headers one message out of step).
//!
//! v2 fixes the class of bug rather than another instance of it:
//!
//! - **Everything downstream is a typed, length-prefixed message.** The client's
//!   read loop is uniform from the first byte; there are no pre-loop reads to
//!   get out of step with.
//! - **The handshake carries a magic number and a version.** A mismatched pair
//!   is diagnosed and refused, instead of being interpreted as a plausible
//!   screen size and acted on.
//! - **The handshake body is length-prefixed.** Fields can be appended without
//!   breaking a peer that doesn't know about them yet: it reads what it
//!   understands and skips the rest.
//!
//! # Upstream: Android -> daemon
//!
//! Handshake, sent once, before any input event:
//!
//! ```text
//! u32  magic          MAGIC
//! u16  version        PROTOCOL_VERSION
//! u16  body_len       bytes following this field
//! --- body ---
//! u32  width_px       real panel size, not the app-usable area
//! u32  height_px
//! i32  pressure_min
//! i32  pressure_max
//! i32  tilt_min_deg
//! i32  tilt_max_deg
//! i64  android_send_ms  wall clock, for the clock-offset calibration
//! u8   config_flags     see `config_flags`
//! i32  xdpi_milli       physical pixels per inch * 1000, X axis
//! i32  ydpi_milli       ditto, Y axis -- the virtual touchpad needs a real
//!                       units/mm resolution or libinput's gesture thresholds
//!                       (all specified in millimetres) are meaningless
//! ... any future fields; a reader that doesn't know them skips to body_len
//! ```
//!
//! The two dpi fields were appended after the fact and are exactly what
//! `body_len` exists for: an older daemon reads the fields it knows and skips
//! the rest, and a newer daemon treats them as absent when an older client
//! doesn't send them.
//!
//! Input events follow, unchanged from v1 (22 bytes each, see
//! `input_receiver.rs`) -- fixed-size records in a direction that carries
//! nothing else. Multi-touch (Milestone 9) reuses that same 22-byte record
//! rather than introducing a second framing: the new event types document
//! different meanings for fields the pen types leave unused.
//!
//! # Downstream: daemon -> Android
//!
//! Every message, without exception:
//!
//! ```text
//! u8   msg_type       see the MSG_* constants
//! i64  send_ms        daemon wall clock, for the per-frame latency estimate
//! u32  payload_len
//! ...  payload
//! ```

/// `"QUIL"`. First four bytes a client ever sends; a daemon that reads anything
/// else knows immediately it is not talking to a Quill client of any version,
/// rather than inferring it from an implausible screen size.
pub const MAGIC: u32 = u32::from_be_bytes(*b"QUIL");

/// Bumped whenever the meaning of existing fields changes. Appending new
/// fields to the handshake body does *not* need a bump -- that's what
/// `body_len` is for.
pub const PROTOCOL_VERSION: u16 = 2;

/// H.264 access unit. Payload: `u8 is_idr`, then Annex-B bytes.
pub const MSG_VIDEO: u8 = 0;
/// Cursor position/shape, only sent in client-side cursor mode. Payload layout
/// in `encode_cursor`.
pub const MSG_CURSOR: u8 = 1;
/// Empty payload. Proof of life during a legitimately idle screen, which
/// produces no frames at all -- the client's watchdog needs something to
/// distinguish "idle" from "peer died without the transport noticing".
pub const MSG_HEARTBEAT: u8 = 2;
/// Clock-offset calibration reply. Payload: `i64 daemon_send_ms`,
/// `i64 android_send_ms` (echoed), `i64 daemon_recv_ms`. See `clock_sync.rs`.
pub const MSG_CLOCK_SYNC: u8 = 3;
/// Negotiated video size, sent once before the first frame. Payload:
/// `u32 width`, `u32 height`.
pub const MSG_VIDEO_FORMAT: u8 = 4;

/// Bit 0 of the handshake's `config_flags`: the client will draw the pointer
/// itself, so the daemon should ask the portal for cursor *metadata* rather
/// than have KWin composite the cursor into every frame.
pub const CONFIG_CLIENT_SIDE_CURSOR: u8 = 1 << 0;

/// Bit 2: rotate the video 180 degrees (GPU-side in the encoder) and reflect
/// touch/pen coordinates to match.
///
/// This was inferred from the aspect ratio until Milestone 24 -- portrait meant
/// flipped -- which was really a statement about where the USB cable enters
/// *this* tablet when held that way. It is a property of how the device is
/// physically oriented, not of its shape, so it belongs to whoever is holding
/// it. A phone, which is portrait by default, would otherwise always be
/// flipped.
pub const CONFIG_FLIP_180: u8 = 1 << 2;

/// Bit 3, paired with [`CONFIG_FLIP_180`] to carry a quarter turn.
///
/// The two bits together encode all four rotations, arranged so that bit 2
/// keeps meaning *exactly* 180 degrees and the two orientations that predate
/// this are bit-for-bit what they always were:
///
/// | rotation | bit 3 | bit 2 |
/// |----------|-------|-------|
/// | 0        | 0     | 0     |
/// | 90       | 1     | 0     |
/// | 180      | 0     | 1     |
/// | 270      | 1     | 1     |
///
/// That is why the protocol version did not move. A client that only knows
/// about the flip sets bit 3 never, and an old daemon reading a new client at 0
/// or 180 degrees behaves identically to before. The length-prefixed handshake
/// body exists precisely so capabilities can be appended rather than versioned.
///
/// The hazard is the other direction: an old daemon ignores bit 3, so a client
/// asking for 270 would get a bare 180 applied to swapped dimensions. The client
/// detects that from `MSG_VIDEO_FORMAT` -- if it asked for a portrait monitor
/// and the video comes back landscape, the daemon did not understand -- and says
/// so rather than showing a corrupted screen. See `MainActivity`.
pub const CONFIG_ROTATE_90: u8 = 1 << 3;

/// How far the captured image is turned before it is encoded, and how far
/// touch and pen coordinates have to be turned back.
///
/// Applied GPU-side in the encoder's VPP pass, never by the compositor:
/// Milestone 16 established, live, that KWin's own rotation property has no
/// effect on this output type at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

impl Rotation {
    pub fn from_config_flags(flags: u8) -> Self {
        match (flags & CONFIG_ROTATE_90 != 0, flags & CONFIG_FLIP_180 != 0) {
            (false, false) => Rotation::None,
            (true, false) => Rotation::Quarter,
            (false, true) => Rotation::Half,
            (true, true) => Rotation::ThreeQuarters,
        }
    }

    pub fn degrees(self) -> u32 {
        match self {
            Rotation::None => 0,
            Rotation::Quarter => 90,
            Rotation::Half => 180,
            Rotation::ThreeQuarters => 270,
        }
    }

    /// Whether the encoder's output is the transpose of its input. True for the
    /// quarter turns, which is the one place this stops being a pure filter and
    /// starts changing geometry.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Quarter | Rotation::ThreeQuarters)
    }
}

/// Bit 1: send pinch as ctrl+wheel instead of letting it reach the virtual
/// touchpad as a real gesture. libinput's pinch is delivered as a Wayland
/// gesture, which only gesture-aware toolkits act on -- anything on XWayland
/// ignores it. Ctrl+scroll zooms in nearly everything, in steps rather than
/// smoothly. See `gesture.rs`.
pub const CONFIG_CTRL_SCROLL_ZOOM: u8 = 1 << 1;

/// Serializes one downstream message. Built as a single buffer and written with
/// a single `write_all`, deliberately: with `TCP_NODELAY` set, and over USB
/// bulk, each separate write is its own packet, and v1 measurably paid for that
/// three times per frame before Milestone 7 combined them.
pub fn encode_message(msg_type: u8, send_ms: i64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(13 + payload.len());
    out.push(msg_type);
    out.extend_from_slice(&send_ms.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// A cursor update. `bitmap` is `Some` only when the shape actually changed --
/// KWin marks an unchanged shape by leaving the bitmap region empty, so the
/// client caches the last one it was given and reuses it.
///
/// Payload:
///
/// ```text
/// i32  x
/// i32  y
/// u8   visible        0 when the pointer is on another output entirely
/// u8   has_bitmap
/// --- if has_bitmap ---
/// u32  width
/// u32  height
/// i32  hotspot_x
/// i32  hotspot_y
/// ...  width*height*4 bytes, RGBA, tightly packed (stride is normalized here
///      so the client never has to care about the producer's padding)
/// ```
pub struct CursorUpdate<'a> {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub bitmap: Option<CursorBitmap<'a>>,
}

pub struct CursorBitmap<'a> {
    pub width: u32,
    pub height: u32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    /// Row-major RGBA, `stride` bytes per row (may exceed `width * 4`).
    pub pixels: &'a [u8],
    pub stride: usize,
}

pub fn encode_cursor(update: &CursorUpdate) -> Vec<u8> {
    let mut p = Vec::with_capacity(10);
    p.extend_from_slice(&update.x.to_be_bytes());
    p.extend_from_slice(&update.y.to_be_bytes());
    p.push(update.visible as u8);
    match &update.bitmap {
        None => p.push(0),
        Some(b) => {
            p.push(1);
            p.extend_from_slice(&b.width.to_be_bytes());
            p.extend_from_slice(&b.height.to_be_bytes());
            p.extend_from_slice(&b.hotspot_x.to_be_bytes());
            p.extend_from_slice(&b.hotspot_y.to_be_bytes());
            // Repacked to a tight `width * 4` stride rather than forwarded with
            // the producer's padding: the client would otherwise need the
            // stride to interpret the bytes, and every row of padding is dead
            // weight on a USB link.
            let row_bytes = b.width as usize * 4;
            for row in 0..b.height as usize {
                let start = row * b.stride;
                match b.pixels.get(start..start + row_bytes) {
                    Some(r) => p.extend_from_slice(r),
                    // Short buffer: pad rather than panic. A truncated cursor
                    // is a cosmetic bug; killing the capture thread is not.
                    None => p.extend(std::iter::repeat_n(0u8, row_bytes)),
                }
            }
        }
    }
    p
}

#[cfg(test)]
mod rotation_tests {
    use super::*;

    /// The two orientations that predate the quarter turns must decode from
    /// exactly the bits they always used, or every existing client breaks.
    #[test]
    fn the_old_two_orientations_keep_their_bits() {
        assert_eq!(Rotation::from_config_flags(0), Rotation::None);
        assert_eq!(Rotation::from_config_flags(CONFIG_FLIP_180), Rotation::Half);
    }

    #[test]
    fn quarter_turns_use_the_new_bit() {
        assert_eq!(Rotation::from_config_flags(CONFIG_ROTATE_90), Rotation::Quarter);
        assert_eq!(
            Rotation::from_config_flags(CONFIG_ROTATE_90 | CONFIG_FLIP_180),
            Rotation::ThreeQuarters,
        );
    }

    /// The other config bits share the byte and must not disturb the reading.
    #[test]
    fn other_config_bits_are_ignored() {
        let noise = CONFIG_CLIENT_SIDE_CURSOR | CONFIG_CTRL_SCROLL_ZOOM;
        assert_eq!(Rotation::from_config_flags(noise), Rotation::None);
        assert_eq!(
            Rotation::from_config_flags(noise | CONFIG_ROTATE_90 | CONFIG_FLIP_180),
            Rotation::ThreeQuarters,
        );
    }

    #[test]
    fn only_quarter_turns_transpose() {
        assert!(!Rotation::None.swaps_axes());
        assert!(Rotation::Quarter.swaps_axes());
        assert!(!Rotation::Half.swaps_axes());
        assert!(Rotation::ThreeQuarters.swaps_axes());
    }
}
