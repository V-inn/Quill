//! Virtual-monitor capture via the standard xdg-desktop-portal ScreenCast
//! interface + PipeWire, replacing the evdi-based capture path (see
//! MILESTONES.md, Milestone 2 findings, for why: evdi caused real freezes
//! under both Wayland and X11 on this machine; this path never touches
//! DRM/KMS at all).
//!
//! The virtual monitor itself is created separately (by `krfb-virtualmonitor`
//! for now); this module just captures whatever monitor the user picks in
//! the portal's screen-selection dialog.

use crate::vaapi_encoder::VaapiEncoder;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType, Stream as PortalStream};
use ashpd::desktop::PersistMode;
use pipewire as pw;
use pw::spa;
use pw::{properties::properties, spa::pod::Pod};
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Milestone 8: either transport implements plain `Read`/`Write` once
/// connected, so the rest of this module (and `input_receiver.rs`) doesn't
/// need to know or care which one is actually in use.
pub type TransportReader = Box<dyn Read + Send>;
pub type TransportWriter = Box<dyn Write + Send>;

/// How to reach the Android client. `TcpForward` is the original adb-forward
/// path (Milestones 3-7); `Aoa` talks directly over raw USB, bypassing adb
/// entirely (Milestone 8) -- kept as a separate selectable mode rather than
/// replacing the working adb-forward path outright, since AOA is new and
/// unproven relative to it.
pub enum TransportConfig {
    None,
    TcpForward(u16),
    Aoa,
}

#[derive(Default, Clone)]
pub struct CaptureStats {
    pub frame_count: u64,
    pub durations: Vec<Duration>,
    pub dropped_stale: u64,
    pub buffer_age_ms_sum: f64,
    pub buffer_age_samples: u64,
    pub capture_latency_ms_sum: f64,
    pub capture_latency_samples: u64,
    pub encode_ms_sum: f64,
    pub convert_encode_samples: u64,
    pub logged_buffer_type: bool,
    pub cursor_meta_samples: u64,
    pub logged_cursor_meta: bool,
}

/// Decodes the 48-bit CLOCK_MONOTONIC barcode painted by
/// `experiments/capture-latency-probe` (see that crate for the encoding:
/// 48 bars, 10px wide each, MSB first, black=0/white=1, top-left corner of
/// the virtual monitor). Reads directly from a raw BGRx-ish captured frame
/// (see `color_convert.rs`'s note on BGRx byte order) -- no decoding of the
/// H.264 stream involved, this runs on the same raw bytes the encoder
/// itself is about to consume.
fn decode_latency_barcode(bytes: &[u8], stride: usize) -> Option<u64> {
    const BITS: u32 = 48;
    const BAR_WIDTH: usize = 10;
    const SAMPLE_Y: usize = 50;

    let mut value: u64 = 0;
    for bit in 0..BITS {
        let x = bit as usize * BAR_WIDTH + BAR_WIDTH / 2;
        let offset = SAMPLE_Y * stride + x * 4;
        let b = *bytes.get(offset)? as u32;
        let g = *bytes.get(offset + 1)? as u32;
        let r = *bytes.get(offset + 2)? as u32;
        let bright = (b + g + r) / 3 > 128;
        value = (value << 1) | (bright as u64);
    }
    Some(value)
}

/// "Let the allocator pick" -- the implicit-modifier sentinel from
/// `drm_fourcc.h`. Offering this alongside LINEAR keeps the daemon out of the
/// business of enumerating Intel's tiling modifiers: whatever KWin's allocator
/// picks comes back in the negotiated format, and VAAPI's PRIME_2 import takes
/// the modifier as a parameter rather than requiring a specific one.
const DRM_FORMAT_MOD_INVALID: i64 = 0x00ff_ffff_ffff_ffff;
const DRM_FORMAT_MOD_LINEAR: i64 = 0;

/// Serializes one `EnumFormat` pod. With `dmabuf`, it carries a
/// `SPA_FORMAT_VIDEO_modifier` choice marked MANDATORY + DONT_FIXATE -- the
/// standard two-step modifier negotiation (the producer picks one from our
/// list and echoes it back in the fixated format), and the exact property
/// KWin's screencast looks for before it will export GPU buffers at all.
fn build_format_pod(dmabuf: bool) -> Vec<u8> {
    use pw::spa::pod::{ChoiceValue, Property, PropertyFlags, Value};
    use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags};

    let mut obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        // BGRx matches evdi's XR24 byte order exactly (B,G,R,X in memory) --
        // matches VA_FOURCC_BGRX in vaapi_encoder.rs's GPU conversion path.
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::RGBx,
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle { width: 1920, height: 1080 },
            pw::spa::utils::Rectangle { width: 1, height: 1 },
            pw::spa::utils::Rectangle { width: 8192, height: 8192 }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction { num: 60, denom: 1 },
            pw::spa::utils::Fraction { num: 0, denom: 1 },
            pw::spa::utils::Fraction { num: 1000, denom: 1 }
        ),
    );

    if dmabuf {
        // Built by hand rather than with `property!`: that macro has no way to
        // set pod property flags, and both flags matter here. MANDATORY is what
        // makes SPA's format matching route us to KWin's dmabuf format entry
        // instead of its shm one; DONT_FIXATE says "these are candidates, you
        // choose", which is what the two-step modifier handshake is.
        obj.properties.push(Property {
            key: pw::spa::param::format::FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: DRM_FORMAT_MOD_INVALID,
                    alternatives: vec![DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR],
                },
            ))),
        });
    }

    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner()
}

/// Asks PipeWire to allocate a `SPA_META_Cursor` region on every buffer.
///
/// Metadata is opt-in: the producer only attaches a meta the consumer declared
/// via `SPA_PARAM_Meta`, so `CursorMode::Metadata` on the portal side is only
/// half of it -- without this the buffers simply have no cursor region and
/// `find_meta::<MetaCursor>()` returns `None` forever. Exactly the same shape as
/// the DMA-BUF gap: KWin was willing, we never asked.
///
/// `size` has to cover the fixed `spa_meta_cursor` header, the `spa_meta_bitmap`
/// that follows it, and the pixels themselves. 256x256 is the largest cursor
/// KWin will hand out.
fn build_cursor_meta_pod() -> Vec<u8> {
    use pw::spa::pod::{Property, PropertyFlags, Value};

    const MAX_CURSOR_DIM: u32 = 256;
    let size = std::mem::size_of::<pw::spa::sys::spa_meta_cursor>() as u32
        + std::mem::size_of::<pw::spa::sys::spa_meta_bitmap>() as u32
        + MAX_CURSOR_DIM * MAX_CURSOR_DIM * 4;

    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamMeta.as_raw(),
        id: pw::spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            Property {
                key: pw::spa::sys::SPA_PARAM_META_type,
                flags: PropertyFlags::empty(),
                value: Value::Id(pw::spa::utils::Id(pw::spa::sys::SPA_META_Cursor)),
            },
            Property {
                key: pw::spa::sys::SPA_PARAM_META_size,
                flags: PropertyFlags::empty(),
                value: Value::Int(size as i32),
            },
        ],
    };

    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner()
}

/// Where the portal's restore token (see below) is cached between runs.
/// Plain `$HOME`-relative path rather than pulling in a `dirs` crate for one
/// file.
/// One token per cursor mode. The portal binds a restore token to the session
/// it was issued for, cursor mode included, so a single shared file would make
/// every toggle look like a rejected token and pop the picker dialog at someone
/// who may not be at the keyboard.
fn restore_token_path(cursor: CursorRendering) -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    let name = match cursor {
        CursorRendering::Embedded => "portal_restore_token",
        CursorRendering::ClientSide => "portal_restore_token_cursor_metadata",
    };
    std::path::Path::new(&home).join(".config/quill").join(name)
}

/// Negotiates a ScreenCast session via the portal. First run ever (or after
/// the token's been revoked/invalidated) triggers KDE's native screen-picker
/// dialog -- the user selects a monitor and confirms, same as before. Every
/// run after that reuses the saved `PersistMode::ExplicitlyRevoked` restore
/// token, so the portal skips the dialog entirely -- required for the daemon
/// to be auto-launched (e.g. from a udev rule) with no one at the keyboard
/// to click through it.
/// Whether the cursor is composited into the video by KWin, or shipped to the
/// client as position/bitmap metadata for it to draw itself.
///
/// `Embedded` costs a full-output composite on *every* pointer move, whether or
/// not any window content changed -- KWin's `ScreenCastStream::record()` ORs
/// `Content::Video` into the work list unconditionally in that mode. `Metadata`
/// only writes a small `SPA_META_Cursor` record, so pointer motion stops
/// dragging a whole frame through encode and transport, and the client can draw
/// the pointer at local latency instead of a full pipeline round trip.
#[derive(Clone, Copy, PartialEq)]
pub enum CursorRendering {
    Embedded,
    ClientSide,
}

impl CursorRendering {
    fn portal_mode(self) -> CursorMode {
        match self {
            CursorRendering::Embedded => CursorMode::Embedded,
            CursorRendering::ClientSide => CursorMode::Metadata,
        }
    }
}

/// Writes a token via a temp file and a rename, so the file on disk is either
/// the old token or the new one and never half of either. This daemon exits
/// abruptly by design (six `std::process::exit` sites), and a truncated token
/// reads back as a rejected one -- i.e. as a picker dialog for someone who
/// isn't there.
///
/// The token is a secret, not just cached state: it is deliberately saved with
/// `PersistMode::ExplicitlyRevoked` so the portal skips the picker dialog on
/// every later run, which means anyone who can read this file can attempt to
/// re-acquire full-screen capture with no consent prompt. So it is written
/// 0600 inside a 0700 directory -- `fs::write` would have created it 0666 &
/// ~umask, i.e. world-readable on a stock umask.
fn save_restore_token(path: &std::path::Path, token: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        // Unconditionally, not just on the create path: `create_dir_all` leaves
        // an already-existing directory's mode alone, and this directory
        // predates the tightening on every machine that has run an older build.
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = path.with_extension("tmp");
    // `mode()` only applies when the open actually creates the file, so a
    // leftover tmp from a crashed run would otherwise be reused with whatever
    // mode it already had. Remove first, then `create_new`, which fails closed
    // (`O_EXCL`) rather than adopting a file someone else planted in between.
    let _ = std::fs::remove_file(&tmp);
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[portal] failed to save restore token: {e}");
            return;
        }
    };
    if let Err(e) = file.write_all(token.as_bytes()) {
        eprintln!("[portal] failed to save restore token: {e}");
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    drop(file);
    if let Err(e) = std::fs::rename(&tmp, path) {
        eprintln!("[portal] failed to install restore token: {e}");
        let _ = std::fs::remove_file(&tmp);
    }
}

pub async fn open_portal(cursor: CursorRendering) -> ashpd::Result<(PortalStream, OwnedFd)> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;

    let token_path = restore_token_path(cursor);
    let saved_token = std::fs::read_to_string(&token_path).ok();

    let select_result = proxy
        .select_sources(
            &session,
            cursor.portal_mode(),
            SourceType::Monitor.into(),
            false,
            saved_token.as_deref(),
            PersistMode::ExplicitlyRevoked,
        )
        .await;

    if let (Err(e), Some(_)) = (&select_result, &saved_token) {
        // Saved token no longer valid (virtual monitor was recreated,
        // permission revoked, etc.) -- fall back to a fresh interactive
        // pick rather than failing outright.
        //
        // The old token file is deliberately left on disk until a replacement
        // exists: if this fresh pick fails, or nobody is at the keyboard to
        // dismiss the dialog, deleting it first would have thrown away a token
        // that might still work on the next boot and guaranteed a picker
        // prompt. A stale token costs one dialog; no token costs one every
        // time.
        eprintln!("[portal] saved restore token rejected ({e}), falling back to picker...");
        proxy
            .select_sources(
                &session,
                cursor.portal_mode(),
                SourceType::Monitor.into(),
                false,
                None,
                PersistMode::ExplicitlyRevoked,
            )
            .await?;
    } else {
        select_result?;
    }

    let response = proxy.start(&session, None).await?.response()?;

    match response.restore_token() {
        Some(token) => save_restore_token(&token_path, token),
        // The token we came in with (if any) has just been consumed and is now
        // invalid, but there is nothing to replace it with. Say so rather than
        // letting the next run's picker dialog look like a new bug.
        None => eprintln!(
            "[portal] the portal returned no restore token -- the next run may show the picker dialog"
        ),
    }

    let stream = response
        .streams()
        .first()
        .expect("no stream found / selected")
        .to_owned();

    let fd = proxy.open_pipe_wire_remote(&session).await?;

    Ok((stream, fd))
}

struct CaptureData {
    format: spa::param::video::VideoInfoRaw,
    encoder: Option<VaapiEncoder>,
    flip_180: bool,
    /// `QUILL_NO_ENCODE` -- see the early return in `process`.
    no_encode: bool,
    /// Whether the client draws the pointer itself. Only in `ClientSide` does
    /// the capture path forward cursor messages.
    cursor: CursorRendering,
    /// Last cursor shape id sent to the client. KWin only supplies a bitmap
    /// when the shape changes, so the client caches by id and this tracks what
    /// it has already been given.
    last_cursor_id: Option<u32>,
    /// Dimensions already announced to the client, if any.
    ///
    /// `param_changed` fires more than once on the DMA-BUF path: modifier
    /// negotiation is inherently two-step (we offer a DONT_FIXATE choice, the
    /// producer picks one and sends the fixated format back), so PipeWire
    /// reports a format twice where the shm path reported it once. Confirmed by
    /// counting the events: 1 with `QUILL_FORCE_SHM`, 2 without. Announcing the
    /// video format on each of those wrote a second 8-byte header into a stream
    /// the client was already reading as length-prefixed frames, desyncing its
    /// framing permanently -- live symptom was a garbage clock offset, a
    /// `1174405120x18998372` video format, and `MediaCodec.configure` throwing
    /// `Invalid size(s)` on a reconnect loop.
    sent_format: Option<(u32, u32)>,
    /// `None` unless `QUILL_DUMP_H264` is set -- see the write site in
    /// `process` for why this stopped being unconditional.
    out_file: Option<File>,
    // Shared with the outer main loop (see run_capture's heartbeat check)
    // rather than owned outright -- both need to write to the same
    // transport, and the heartbeat needs to fire even when this stream's
    // own process() callback isn't (a legitimately idle screen produces no
    // new pipewire buffers at all, so nothing here would ever run).
    transport: Rc<RefCell<Option<TransportWriter>>>,
    stats: Rc<RefCell<CaptureStats>>,
}

/// Connects whichever transport `config` selects, spawns the input-receiver
/// thread on the read half, runs the Milestone 7 clock-offset calibration
/// exchange, and returns the write half for video frames plus the
/// capability handshake's `(width, height)` -- identical downstream
/// handling regardless of which transport actually carried the bytes.
///
/// Called from `main.rs` *before* any portal negotiation (Milestone 15):
/// the resulting `(width, height)` is what `orientation::set_rotation`
/// rotates the `krfb-virtualmonitor` output to match, and that needs to
/// happen before the portal picker runs so it sees the right-shaped output.
///
/// `remote_input_rx`: forwarded as-is to `input_receiver::run` -- see that
/// function's doc for why the `RemoteDesktop` portal handle can only arrive
/// later, after this call already returned.
pub fn setup_transport(
    config: TransportConfig,
    remote_input_rx: Option<std::sync::mpsc::Receiver<crate::remote_desktop_input::RemoteDesktopInput>>,
) -> Option<(TransportWriter, crate::input_receiver::HandshakeInfo)> {
    // AOA needs a much longer clock-sync wait than TCP: adb-forward's
    // ServerSocket::accept() already guarantees a connected, ready peer by
    // the time we get here, but AOA's USB_ACCESSORY_ATTACHED flow involves
    // Android routing an intent, showing a one-time permission dialog, and
    // waiting on a human to tap "Allow" -- realistically tens of seconds,
    // not milliseconds. Confirmed live: a 5s timeout here fired well before
    // the user could react, and the daemon (wrongly, at the time) treated
    // that as non-fatal and started streaming video anyway -- Android's
    // eventual first read landed mid-frame instead of on the clock-sync
    // reply that was never sent, corrupting both sides' framing.
    let clock_sync_timeout = match config {
        TransportConfig::Aoa => Duration::from_secs(120),
        _ => Duration::from_secs(5),
    };

    let (reader, mut writer): (TransportReader, TransportWriter) = match config {
        TransportConfig::None => return None,
        TransportConfig::TcpForward(port) => {
            eprintln!("[transport] connecting to 127.0.0.1:{port} (via adb forward)...");
            let s = TcpStream::connect(("127.0.0.1", port))
                .unwrap_or_else(|e| panic!("failed to connect to 127.0.0.1:{port}: {e}"));
            s.set_nodelay(true).ok();
            eprintln!("[transport] connected");
            let r = s
                .try_clone()
                .unwrap_or_else(|e| panic!("failed to clone transport socket: {e}"));
            (Box::new(r), Box::new(s))
        }
        TransportConfig::Aoa => {
            eprintln!("[transport] connecting via AOA (USB accessory mode, bypassing adb)...");
            let t = crate::aoa::connect().unwrap_or_else(|e| panic!("AOA connect failed: {e}"));
            eprintln!("[transport] AOA connected, waiting for the Android app to open the accessory...");
            let (r, w) = t.split();
            // USB bulk transfers are packet-oriented, not stream-oriented
            // like TCP: a read_bulk() call fails with Overflow if the
            // caller's buffer is smaller than the incoming packet, it
            // doesn't just silently return a truncated read. Android's
            // handshake writes all its fields via a single flush() (one
            // ~32-byte USB packet), but input_receiver.rs reads them 4-8
            // bytes at a time -- confirmed live: without buffering here,
            // the very first small read overflowed. BufReader gives read()
            // its own large internal buffer for the actual read_bulk()
            // call and serves the small reads from that.
            (Box::new(std::io::BufReader::new(r)), Box::new(w))
        }
    };

    let (clock_tx, clock_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || crate::input_receiver::run(reader, clock_tx, remote_input_rx));

    // Milestone 7 clock-offset calibration (see clock_sync.rs) doubles as
    // Milestone 8's "is the peer actually ready" signal: block for the
    // input thread to hand over the Android-side clock-ping it just read
    // out of the handshake, then reply on the video channel before any
    // frame is sent. If this never arrives, DON'T stream anyway (unlike
    // the old TCP-only behavior of logging a warning and continuing
    // regardless) -- with no peer confirmed ready, video bytes would just
    // corrupt whatever the real handshake turns out to be once someone
    // finally does connect.
    // Past this point `config` was guaranteed to be `Aoa` or `TcpForward`
    // (the `None` case already returned above) -- a real transport was
    // requested, so a failure here means the daemon is useless as-is, not a
    // legitimate "run with no client" mode. Exit non-zero rather than
    // degrading to silently running capture-only forever: confirmed live,
    // that left the daemon "running" but functionally dead after a USB drop
    // mid-handshake, and systemd's `Restart=on-failure` (see
    // `packaging/quill-daemon.service`) never got a chance to retry the AOA
    // connect because the process never actually exited.
    let info = match clock_rx.recv_timeout(clock_sync_timeout) {
        Ok(info) => {
            let daemon_send_ms = crate::clock_sync::now_millis();
            let mut payload = Vec::with_capacity(24);
            payload.extend_from_slice(&daemon_send_ms.to_be_bytes());
            payload.extend_from_slice(&info.android_send_ms.to_be_bytes());
            payload.extend_from_slice(&info.daemon_recv_ms.to_be_bytes());
            let msg = crate::protocol::encode_message(
                crate::protocol::MSG_CLOCK_SYNC,
                daemon_send_ms,
                &payload,
            );
            if let Err(e) = writer.write_all(&msg) {
                eprintln!("[clock-sync] failed to send calibration reply: {e}");
                std::process::exit(1);
            }
            info
        }
        Err(e) => {
            eprintln!("[clock-sync] never received clock ping: {e} -- exiting so systemd can retry");
            std::process::exit(1);
        }
    };

    Some((writer, info))
}

/// Shared by both capture paths so the "time from content changing on screen to
/// the daemon having a buffer for it" number stays directly comparable between
/// them -- that comparison is the whole point of the DMA-BUF work.
///
/// `dequeue_ns` is sampled at the very top of the `process` callback, not here:
/// the DMA-BUF path has to map a GPU surface before it can read the barcode at
/// all, and that map forces a sync. Timing from the map would charge the
/// zero-copy path for a cost that only its own diagnostic incurs and make the
/// A/B against the shm path meaningless.
fn record_capture_latency(stats: &Rc<RefCell<CaptureStats>>, barcode_ns: u64, dequeue_ns: i64) {
    let latency_ms = (dequeue_ns - barcode_ns as i64) as f64 / 1_000_000.0;
    if !(0.0..60_000.0).contains(&latency_ms) {
        return;
    }
    let mut stats = stats.borrow_mut();
    stats.capture_latency_ms_sum += latency_ms;
    stats.capture_latency_samples += 1;
    if stats.capture_latency_samples == 1 || stats.capture_latency_samples % 30 == 0 {
        eprintln!(
            "[pipewire] capture latency (barcode -> dequeue): {latency_ms:.2}ms (avg {:.2}ms over {} samples)",
            stats.capture_latency_ms_sum / stats.capture_latency_samples as f64,
            stats.capture_latency_samples
        );
    }
}

/// Everything that happens to a frame once it's encoded, shared by the DMA-BUF
/// and shm paths: stats, the opt-in bitstream dump, and the wire write.
///
/// `start`: when the `process` callback began, so `dequeue->encoded` still
/// measures the whole callback and stays comparable with every number recorded
/// in MILESTONES.md. `encode_elapsed`: just the encoder call.
fn emit_frame(
    user_data: &mut CaptureData,
    frame: crate::vaapi_encoder::EncodedFrame,
    start: Instant,
    encode_elapsed: Duration,
) {
    let encoded = frame.data;
    {
        let mut stats = user_data.stats.borrow_mut();
        stats.encode_ms_sum += encode_elapsed.as_secs_f64() * 1000.0;
        stats.convert_encode_samples += 1;
        if stats.convert_encode_samples == 1 || stats.convert_encode_samples % 30 == 0 {
            eprintln!(
                "[timing] upload+VPP+encode avg={:.2}ms (this frame: {:.2}ms)",
                stats.encode_ms_sum / stats.convert_encode_samples as f64,
                encode_elapsed.as_secs_f64() * 1000.0,
            );
        }
    }
    let elapsed = start.elapsed();
    {
        let mut stats = user_data.stats.borrow_mut();
        stats.frame_count += 1;
        stats.durations.push(elapsed);
        if stats.frame_count == 1 || stats.frame_count % 30 == 0 {
            eprintln!(
                "[capture] frame {}: {} bytes, {:?} (dequeue->encoded), {} stale dropped so far",
                stats.frame_count,
                encoded.len(),
                elapsed,
                stats.dropped_stale
            );
        }
    }
    // Opt-in, not always-on: this was an unbuffered write() syscall per frame,
    // in the middle of the hot path, into a file that grew without bound (~50MB
    // after one session) and that nothing reads unless someone is debugging the
    // bitstream. Same env-var convention as QUILL_DUMP_FRAME.
    if let Some(f) = user_data.out_file.as_mut() {
        let _ = f.write_all(&encoded);
    }
    if let Some(sock) = user_data.transport.borrow_mut().as_mut() {
        // Milestone 7: prefix every frame with the daemon's send time (its own
        // clock) so Android can compute a per-frame latency estimate using the
        // offset from the earlier clock-sync exchange -- see clock_sync.rs.
        //
        // Combined into one buffer and one write_all() call rather than three
        // separate ones: with TCP_NODELAY set, each write_all was its own TCP
        // segment / adb protocol packet -- three small packets ahead of the
        // real payload adds per-packet adb/USB overhead for no reason.
        let mut payload = Vec::with_capacity(1 + encoded.len());
        payload.push(frame.is_idr as u8);
        payload.extend_from_slice(&encoded);
        let frame_buf = crate::protocol::encode_message(
            crate::protocol::MSG_VIDEO,
            crate::clock_sync::now_millis(),
            &payload,
        );
        if let Err(e) = sock.write_all(&frame_buf) {
            // Same reasoning as the two startup failure paths in
            // `setup_transport`: this only runs with `Some(sock)` when a real
            // transport connected successfully, so a write failure here means a
            // mid-session drop (app force-stopped/relaunched, USB unplugged),
            // not `TransportConfig::None`'s legitimate no-client mode.
            // Previously this just set `transport = None` and kept running --
            // confirmed live, that left the daemon "running" but permanently
            // unable to reconnect. Exiting lets systemd's `Restart=on-failure`
            // re-run the whole connect sequence from scratch instead.
            eprintln!("[transport] write failed ({e}), exiting so systemd can retry");
            std::process::exit(1);
        }
    }
}

/// `transport`: the already-connected write half from `setup_transport`,
/// called by `main.rs` before the portal was ever opened (see that
/// function's doc) -- `None` only for the capture-only diagnostic mode
/// (`TransportConfig::None`).
pub fn run_capture(
    node_id: u32,
    fd: OwnedFd,
    out_path: &str,
    transport: Option<TransportWriter>,
    flip_180: bool,
    cursor: CursorRendering,
) -> Result<CaptureStats, pw::Error> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;

    // pipewire's own add_signal_local mechanism turned out unreliable here
    // (registered fine, callback never fired -- SIGINT killed the process
    // via the OS default disposition regardless, even single-threaded).
    // Falling back to the plain, proven pattern: libc::signal + atomic flag
    // + manual loop iteration, same shape as the old evdi_capture.rs.
    crate::set_up_sigint_handler();

    let out_file = if std::env::var("QUILL_DUMP_H264").is_ok() {
        eprintln!("[capture] QUILL_DUMP_H264 set -- writing the encoded stream to {out_path}");
        Some(File::create(out_path).expect("create output file"))
    } else {
        None
    };
    let transport = Rc::new(RefCell::new(transport));
    let stats = Rc::new(RefCell::new(CaptureStats::default()));
    let data = CaptureData {
        format: Default::default(),
        encoder: None,
        flip_180,
        no_encode: std::env::var("QUILL_NO_ENCODE").is_ok(),
        cursor,
        last_cursor_id: None,
        sent_format: None,
        out_file,
        transport: transport.clone(),
        stats: stats.clone(),
    };

    let stream = pw::stream::StreamRc::new(
        core,
        "quill-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_, _, old, new| {
            eprintln!("[pipewire] state: {old:?} -> {new:?}");
        })
        .param_changed(|stream, user_data, id, param| {
            let Some(param) = param else { return };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let (media_type, media_subtype) =
                match pw::spa::param::format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(_) => return,
                };
            if media_type != pw::spa::param::format::MediaType::Video
                || media_subtype != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            user_data
                .format
                .parse(param)
                .expect("failed to parse negotiated video format");

            let width = user_data.format.size().width;
            let height = user_data.format.size().height;
            // Whether a modifier came back at all is the single fact that says
            // if KWin took its DMA-BUF path or fell back to MemFd -- see the
            // two-pod negotiation in `run_capture`.
            let modifier = user_data.format.modifier();
            eprintln!(
                "[pipewire] negotiated format: {:?} {width}x{height} @{}/{} modifier=0x{modifier:016x}",
                user_data.format.format(),
                user_data.format.framerate().num,
                user_data.format.framerate().denom
            );

            // Only build a new encoder when the geometry actually changes.
            // The second `param_changed` of a DMA-BUF negotiation reports the
            // same size as the first, and rebuilding on it threw away a working
            // encoder (and reset its GOP state) for nothing.
            if user_data.sent_format != Some((width, height)) {
                let encoder = VaapiEncoder::new(width, height, user_data.flip_180)
                    .expect("VAAPI encoder init failed");
                user_data.encoder = Some(encoder);
            }

            // Declare the metadata we want on each buffer now that the
            // format is settled. Has to happen here rather than at connect():
            // buffers are allocated after format negotiation, so a meta
            // declared later than this never appears on them.
            let cursor_meta = build_cursor_meta_pod();
            let mut meta_params = [Pod::from_bytes(&cursor_meta).unwrap()];
            if let Err(e) = stream.update_params(&mut meta_params) {
                eprintln!("[pipewire] failed to request cursor metadata: {e}");
            }

            // Milestone 9 follow-up: the video resolution comes from
            // whatever the host's virtual monitor negotiates, not a fixed
            // constant -- previously the Android client just assumed
            // 1920x1080 to match this project's one test setup, which broke
            // the project's own no-hardcoding rule (see MILESTONES.md).
            // Sent once, right after the clock-sync reply and before any
            // video frame, so the client can size its decoder correctly
            // before the first frame arrives.
            //
            // Guarded on `sent_format`: this callback fires twice on the
            // DMA-BUF path (see that field), and writing the header again put 8
            // stray bytes into a stream the client was already reading as
            // length-prefixed frames.
            if user_data.sent_format == Some((width, height)) {
                eprintln!("[pipewire] format re-reported unchanged, not re-announcing it");
                return;
            }
            if user_data.sent_format.is_some() {
                // Never observed; if a producer ever does resize mid-session the
                // client has no way to reconfigure its decoder, so say so loudly
                // rather than silently sending a header it can't act on.
                eprintln!(
                    "[pipewire] WARNING: video size changed mid-session to {width}x{height}; \
                     the client cannot reconfigure -- expect a reconnect"
                );
            }
            user_data.sent_format = Some((width, height));
            if let Some(sock) = user_data.transport.borrow_mut().as_mut() {
                let mut payload = Vec::with_capacity(8);
                payload.extend_from_slice(&width.to_be_bytes());
                payload.extend_from_slice(&height.to_be_bytes());
                let header = crate::protocol::encode_message(
                    crate::protocol::MSG_VIDEO_FORMAT,
                    crate::clock_sync::now_millis(),
                    &payload,
                );
                if let Err(e) = sock.write_all(&header) {
                    eprintln!("[transport] failed to send video format header ({e}), exiting so systemd can retry");
                    std::process::exit(1);
                }
            }
        })
        .process(|stream, user_data| {
            let start = Instant::now();
            // Sampled here, before any per-path work, so the barcode's
            // "content changed -> daemon has the buffer" number means the same
            // thing on the DMA-BUF and shm paths.
            let dequeue_ns = crate::clock_sync::monotonic_ns();
            // Milestone 4 root-caused ~300ms glass-to-glass latency to this
            // pipeline being structurally slower than the source's refresh
            // rate: with no staleness check, a backlog of queued buffers
            // just grows and we always end up encoding an old one. Drain to
            // the newest buffer available right now and let older ones drop
            // (auto-requeued to PipeWire via `Buffer`'s Drop impl) unprocessed.
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let mut dropped = 0u32;
            while let Some(newer) = stream.dequeue_buffer() {
                buffer = newer;
                dropped += 1;
            }
            if dropped > 0 {
                let mut stats = user_data.stats.borrow_mut();
                stats.dropped_stale += dropped as u64;
            }

            // Milestone 7 follow-up: the daemon's own encode+transport+
            // decode+render instrumentation (clock_sync.rs) measured only
            // ~15ms, far below the ~150-180ms camera-measured glass-to-
            // glass latency -- meaning the gap lives upstream, before we
            // even see a buffer. `SPA_META_Header.pts` (if the producer
            // sets it) is a compositor-side generation timestamp on the
            // *same machine* as this daemon, so it can be compared directly
            // against CLOCK_MONOTONIC with no cross-device calibration.
            if let Some(header) = buffer.find_meta::<pw::spa::buffer::meta::MetaHeader>() {
                let pts_ns = header.pts();
                if pts_ns > 0 {
                    let now_ns = crate::clock_sync::monotonic_ns();
                    let age_ms = (now_ns - pts_ns) as f64 / 1_000_000.0;
                    let mut stats = user_data.stats.borrow_mut();
                    stats.buffer_age_ms_sum += age_ms;
                    stats.buffer_age_samples += 1;
                    if stats.buffer_age_samples == 1 || stats.buffer_age_samples % 30 == 0 {
                        eprintln!(
                            "[pipewire] buffer age (pts -> dequeue): {age_ms:.2}ms (avg {:.2}ms over {} samples)",
                            stats.buffer_age_ms_sum / stats.buffer_age_samples as f64,
                            stats.buffer_age_samples
                        );
                    }
                }
            } else {
                eprintln!("[pipewire] buffer has no MetaHeader (no pts available from this producer)");
            }

            // Cursor forwarding, client-side mode only.
            //
            // A pointer move on its own does not produce a video frame in this
            // mode (that is the point -- under CursorMode::Embedded KWin ORs
            // Content::Video in unconditionally and re-composites the whole
            // output for every pixel of pointer motion), so this message is
            // what makes the pointer move on the tablet at all.
            if user_data.cursor == CursorRendering::ClientSide {
                if let Some(cur) = buffer.find_meta::<pw::spa::buffer::meta::MetaCursor>() {
                    let visible = cur.is_valid();
                    let pos = cur.position();
                    // A bitmap is present only when the shape actually changed;
                    // otherwise the region is left empty and the client reuses
                    // the last shape it was sent.
                    let bitmap = cur
                        .bitmap()
                        .filter(|b| b.is_valid() && b.size().width > 0 && b.size().height > 0)
                        .and_then(|b| {
                            let size = b.size();
                            let hotspot = cur.hotspot();
                            b.bitmap_data().map(|pixels| crate::protocol::CursorBitmap {
                                width: size.width,
                                height: size.height,
                                hotspot_x: hotspot.x,
                                hotspot_y: hotspot.y,
                                pixels,
                                stride: b.stride() as usize,
                            })
                        });
                    if bitmap.is_some() {
                        user_data.last_cursor_id = Some(cur.id());
                    }
                    let update = crate::protocol::CursorUpdate {
                        x: pos.x,
                        y: pos.y,
                        visible,
                        bitmap,
                    };
                    let msg = crate::protocol::encode_message(
                        crate::protocol::MSG_CURSOR,
                        crate::clock_sync::now_millis(),
                        &crate::protocol::encode_cursor(&update),
                    );
                    if let Some(sock) = user_data.transport.borrow_mut().as_mut() {
                        if let Err(e) = sock.write_all(&msg) {
                            eprintln!("[transport] cursor write failed ({e}), exiting so systemd can retry");
                            std::process::exit(1);
                        }
                    }
                    let mut stats = user_data.stats.borrow_mut();
                    stats.cursor_meta_samples += 1;
                    if stats.cursor_meta_samples == 1 || stats.cursor_meta_samples % 300 == 0 {
                        eprintln!(
                            "[cursor] id={} pos=({},{}) visible={visible} (sent {} updates)",
                            cur.id(), pos.x, pos.y, stats.cursor_meta_samples
                        );
                    }
                } else {
                    let mut stats = user_data.stats.borrow_mut();
                    if !stats.logged_cursor_meta {
                        stats.logged_cursor_meta = true;
                        eprintln!(
                            "[cursor] client-side cursor requested but SPA_META_Cursor is absent \
                             from buffers -- the pointer will not appear on the tablet"
                        );
                    }
                }
            }

            // Diagnostic for "is our own processing what caps the delivered
            // frame rate?". KWin's `record()` dequeues a pool buffer and, if
            // the 2-4 buffer pool is exhausted, simply returns with no retry
            // scheduled -- the next chance is the next damage event. So holding
            // a buffer across encode can silently cost frames. Setting this
            // returns immediately, dropping the buffer at once; if the frame
            // rate jumps, the hold is the cap, and if it doesn't, it's upstream.
            if user_data.no_encode {
                let mut stats = user_data.stats.borrow_mut();
                stats.frame_count += 1;
                return;
            }

            // Taken after the cursor block above, not before: `datas_mut()`
            // borrows the buffer mutably, which rules out reading any metadata
            // off it afterwards.
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let is_dmabuf = datas[0].type_() == pw::spa::buffer::DataType::DmaBuf;
            let stride = datas[0].chunk().stride() as usize;

            // Zero-copy path. Taken whenever KWin negotiated DMA-BUF (see the
            // format pods in `run_capture`); everything below it is the shm
            // fallback, kept intact for producers that only offer mapped
            // memory.
            if is_dmabuf {
                let raw = datas[0].as_raw();
                let plane = crate::vaapi_encoder::DmabufPlane {
                    fd: datas[0].fd() as std::os::fd::RawFd,
                    offset: datas[0].chunk().offset(),
                    stride: stride as u32,
                    size: raw.maxsize,
                    modifier: user_data.format.modifier(),
                };
                let Some(encoder) = user_data.encoder.as_mut() else {
                    return;
                };
                // Opt-in: mapping a GPU surface costs a sync, which is the
                // exact overhead this path removes -- but without it the
                // barcode probe (the only instrument that measures the capture
                // segment DMA-BUF is meant to shrink) goes blind here, since it
                // reads a CPU mapping that no longer exists.
                if std::env::var("QUILL_BARCODE_PROBE").is_ok() {
                    if let Ok(Some(barcode_ns)) =
                        encoder.with_mapped_dmabuf(&plane, |b, s| decode_latency_barcode(b, s))
                    {
                        record_capture_latency(&user_data.stats, barcode_ns, dequeue_ns);
                    }
                }
                let encode_start = Instant::now();
                match encoder.encode_frame_dmabuf(&plane) {
                    Ok(frame) => {
                        let elapsed = encode_start.elapsed();
                        emit_frame(user_data, frame, start, elapsed);
                    }
                    Err(e) => eprintln!("[capture] encode_frame_dmabuf error: {e}"),
                }
                return;
            }

            let chunk_size = datas[0].chunk().size() as usize;
            let Some(bytes) = datas[0].data() else {
                eprintln!("[pipewire] frame has no mapped data and is not DMA-BUF, skipping");
                return;
            };
            if chunk_size == 0 || stride == 0 {
                return;
            }

            // One-shot calibration dump for the latency-barcode probe: lets
            // us visually confirm exactly where the probe window landed in
            // the actual captured pixels, sidestepping any X11-vs-Wayland
            // coordinate-space mismatch entirely (ground truth, not a
            // calculation). Delete once calibration is confirmed.
            if std::env::var("QUILL_DUMP_FRAME").is_ok() {
                let stats = user_data.stats.borrow();
                if stats.frame_count == 0 {
                    let height = chunk_size / stride;
                    let width = stride / 4;
                    if let Ok(mut f) = File::create("/tmp/quill_frame_dump.ppm") {
                        let _ = writeln!(f, "P6\n{width} {height}\n255");
                        for row in 0..height {
                            for col in 0..width {
                                let off = row * stride + col * 4;
                                let _ = f.write_all(&[bytes[off + 2], bytes[off + 1], bytes[off]]);
                            }
                        }
                        eprintln!("[debug] dumped frame to /tmp/quill_frame_dump.ppm ({width}x{height})");
                    }
                }
            }

            // Milestone 7 follow-up: `SPA_META_Header.pts` turned out
            // unavailable from this producer (see the check above), so this
            // is the working version of the same idea -- decode a 48-bit
            // CLOCK_MONOTONIC barcode painted by
            // experiments/capture-latency-probe directly out of the raw
            // captured pixels, before any color conversion or encoding.
            // Same machine as the probe, so no cross-device clock sync is
            // needed; this is exactly "time from content changing on
            // screen to the daemon having a buffer for it."
            if let Some(barcode_ns) = decode_latency_barcode(bytes, stride) {
                record_capture_latency(&user_data.stats, barcode_ns, dequeue_ns);
            }

            let Some(encoder) = user_data.encoder.as_mut() else {
                return;
            };

            // Milestone 7 follow-up: color conversion moved off the CPU.
            // The old path here was `bgrx_to_nv12` (color_convert.rs, a
            // scalar per-pixel loop) writing into `y_plane`/`uv_plane`
            // before handing them to the encoder -- measured at ~10.4ms
            // average, more expensive than the hardware VAAPI encode step
            // itself (~4.2ms). `encode_frame` now takes the raw captured
            // BGRX bytes directly and does the conversion via VAAPI's own
            // GPU Video Post-Processing entrypoint instead (see
            // `vaapi_encoder.rs`'s `run_vpp_conversion`).
            let encode_start = Instant::now();
            match encoder.encode_frame(bytes, stride) {
                Ok(frame) => emit_frame(user_data, frame, start, encode_start.elapsed()),
                Err(e) => eprintln!("[capture] encode_frame error: {e}"),
            }
        })
        .register()?;

    eprintln!("[pipewire] created stream, connecting to node {node_id}...");

    // Two EnumFormat params, offered in preference order: DMA-BUF first, plain
    // shared memory second.
    //
    // KWin will only ever hand out DMA-BUF if the negotiated format carries a
    // `SPA_FORMAT_VIDEO_modifier` property -- `ScreenCastStream::onStreamParam
    // Changed()` does a literal `spa_pod_find_prop(format, nullptr,
    // SPA_FORMAT_VIDEO_modifier)` and only sets up its dmabuf path if that
    // finds something. This pod never had one, so every frame took KWin's
    // MemFd fallback, which means a synchronous `glReadnPixels` of the whole
    // 2560x1600 render target into CPU memory once per frame -- and then this
    // daemon copied those same 16.4MB straight back onto the GPU in
    // `upload_bgrx_surface`. KWin's own comment in `record()` ("Sample it
    // before video rendering, readback and buffer synchronization add
    // latency") confirms that readback sits inside the ~28-30ms this project
    // measures as capture latency.
    //
    // The shm variant is kept as a real fallback rather than removed: if the
    // dmabuf allocation test fails on some other GPU/driver, the stream still
    // negotiates and the existing upload path takes over.
    //
    // `QUILL_FORCE_SHM` drops the dmabuf offer entirely, which is how the two
    // paths get A/B'd against each other on the same machine in the same
    // sitting -- without it there's no way to re-measure the old behavior once
    // the new one works.
    let force_shm = std::env::var("QUILL_FORCE_SHM").is_ok();
    let dmabuf_values = build_format_pod(true);
    let shm_values = build_format_pod(false);
    let mut dmabuf_first = vec![
        Pod::from_bytes(&dmabuf_values).unwrap(),
        Pod::from_bytes(&shm_values).unwrap(),
    ];
    let mut shm_only = vec![Pod::from_bytes(&shm_values).unwrap()];
    let params: &mut [&Pod] = if force_shm {
        eprintln!("[pipewire] QUILL_FORCE_SHM set -- offering shared memory only");
        &mut shm_only
    } else {
        &mut dmabuf_first
    };

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        params,
    )?;

    eprintln!("[pipewire] connected, running (Ctrl+C to stop)...");
    let loop_ = mainloop.loop_();
    // Confirmed live: a genuinely idle screen produces zero new pipewire
    // buffers at all (expected -- see MILESTONES.md), so process() above
    // never runs and nothing gets written to the transport for as long as
    // that lasts. On the Android side, a plain blocking read has no way to
    // tell "still connected, just idle" apart from "peer died without the
    // USB transport itself signaling an error" (confirmed live: it
    // doesn't, reliably) -- it just hangs forever with no exception, frozen
    // on the last frame. This periodic heartbeat (a frame with length=0,
    // MainActivity.kt's per-frame read loop already special-cases it) gives
    // the Android-side watchdog something to time out against that's
    // distinct from a legitimate idle stretch.
    const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(800);
    let mut last_heartbeat = Instant::now();
    while !crate::sigint_received() {
        loop_.iterate(pw::loop_::Timeout::Finite(Duration::from_millis(100)));
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            last_heartbeat = Instant::now();
            if let Some(sock) = transport.borrow_mut().as_mut() {
                // Same 13-byte header shape as a real frame, just with a
                // zero-length payload -- keeping the framing uniform means the
                // client's read loop has no special case before it knows the
                // length.
                let hb = crate::protocol::encode_message(
                    crate::protocol::MSG_HEARTBEAT,
                    crate::clock_sync::now_millis(),
                    &[],
                );
                if let Err(e) = sock.write_all(&hb) {
                    eprintln!("[transport] heartbeat write failed ({e}), exiting so systemd can retry");
                    std::process::exit(1);
                }
            }
        }
    }
    eprintln!("[pipewire] SIGINT received, stopping...");

    let final_stats = stats.borrow().clone();
    eprintln!("[pipewire] stats computed: {} frames, returning...", final_stats.frame_count);
    Ok(final_stats)
}
