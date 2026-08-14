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
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::fd::OwnedFd;
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

/// Where the portal's restore token (see below) is cached between runs.
/// Plain `$HOME`-relative path rather than pulling in a `dirs` crate for one
/// file.
fn restore_token_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    std::path::Path::new(&home).join(".config/quill/portal_restore_token")
}

/// Negotiates a ScreenCast session via the portal. First run ever (or after
/// the token's been revoked/invalidated) triggers KDE's native screen-picker
/// dialog -- the user selects a monitor and confirms, same as before. Every
/// run after that reuses the saved `PersistMode::ExplicitlyRevoked` restore
/// token, so the portal skips the dialog entirely -- required for the daemon
/// to be auto-launched (e.g. from a udev rule) with no one at the keyboard
/// to click through it.
pub async fn open_portal() -> ashpd::Result<(PortalStream, OwnedFd)> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;

    let token_path = restore_token_path();
    let saved_token = std::fs::read_to_string(&token_path).ok();

    let select_result = proxy
        .select_sources(
            &session,
            CursorMode::Embedded, // bake the cursor into frames, simplest for v0
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
        eprintln!("[portal] saved restore token rejected ({e}), falling back to picker...");
        let _ = std::fs::remove_file(&token_path);
        proxy
            .select_sources(
                &session,
                CursorMode::Embedded,
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

    if let Some(token) = response.restore_token() {
        if let Some(parent) = token_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&token_path, token) {
            eprintln!("[portal] failed to save restore token: {e}");
        }
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
    out_file: File,
    transport: Option<TransportWriter>,
    stats: Rc<RefCell<CaptureStats>>,
}

/// Connects whichever transport `config` selects, spawns the input-receiver
/// thread on the read half, runs the Milestone 7 clock-offset calibration
/// exchange, and returns the write half for video frames -- identical
/// downstream handling regardless of which transport actually carried the
/// bytes.
fn setup_transport(config: TransportConfig) -> Option<TransportWriter> {
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
    std::thread::spawn(move || crate::input_receiver::run(reader, clock_tx));

    // Milestone 7 clock-offset calibration (see clock_sync.rs) doubles as
    // Milestone 8's "is the peer actually ready" signal: block for the
    // input thread to hand over the Android-side clock-ping it just read
    // out of the handshake, then reply on the video channel before any
    // frame is sent. If this never arrives, DON'T stream anyway (unlike
    // the old TCP-only behavior of logging a warning and continuing
    // regardless) -- with no peer confirmed ready, video bytes would just
    // corrupt whatever the real handshake turns out to be once someone
    // finally does connect.
    match clock_rx.recv_timeout(clock_sync_timeout) {
        Ok((android_send_ms, daemon_recv_ms)) => {
            let daemon_send_ms = crate::clock_sync::now_millis();
            let mut reply = Vec::with_capacity(24);
            reply.extend_from_slice(&daemon_send_ms.to_be_bytes());
            reply.extend_from_slice(&android_send_ms.to_be_bytes());
            reply.extend_from_slice(&daemon_recv_ms.to_be_bytes());
            if let Err(e) = writer.write_all(&reply) {
                eprintln!("[clock-sync] failed to send calibration reply: {e}");
                return None;
            }
        }
        Err(e) => {
            eprintln!("[clock-sync] never received clock ping: {e} -- not streaming, no confirmed peer");
            return None;
        }
    }

    Some(writer)
}

/// `transport_config`: which transport (if any) carries video frames out to
/// the Android client and reads S Pen input back, as a 4-byte big-endian
/// length prefix followed by the frame bytes -- see `TransportConfig`.
pub fn run_capture(
    node_id: u32,
    fd: OwnedFd,
    out_path: &str,
    transport_config: TransportConfig,
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

    let out_file = File::create(out_path).expect("create output file");
    let transport = setup_transport(transport_config);
    let stats = Rc::new(RefCell::new(CaptureStats::default()));
    let data = CaptureData {
        format: Default::default(),
        encoder: None,
        out_file,
        transport,
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
        .param_changed(|_, user_data, id, param| {
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
            eprintln!(
                "[pipewire] negotiated format: {:?} {width}x{height} @{}/{}",
                user_data.format.format(),
                user_data.format.framerate().num,
                user_data.format.framerate().denom
            );

            let encoder = VaapiEncoder::new(width, height).expect("VAAPI encoder init failed");
            user_data.encoder = Some(encoder);
        })
        .process(|stream, user_data| {
            let start = Instant::now();
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

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let chunk_size = datas[0].chunk().size() as usize;
            let stride = datas[0].chunk().stride() as usize;
            let Some(bytes) = datas[0].data() else {
                eprintln!("[pipewire] frame has no mapped data (DMA-BUF-only buffer, not handled in v0), skipping");
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
                let now_ns = crate::clock_sync::monotonic_ns();
                let latency_ms = (now_ns - barcode_ns as i64) as f64 / 1_000_000.0;
                if latency_ms >= 0.0 && latency_ms < 60_000.0 {
                    let mut stats = user_data.stats.borrow_mut();
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
                Ok(encoded) => {
                    let encode_elapsed = encode_start.elapsed();
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
                    drop(stats);
                    let _ = user_data.out_file.write_all(&encoded);
                    if let Some(sock) = user_data.transport.as_mut() {
                        // Milestone 7: prefix every frame with the daemon's
                        // send time (its own clock) so Android can compute a
                        // per-frame latency estimate using the offset from
                        // the earlier clock-sync exchange -- see clock_sync.rs.
                        //
                        // Combined into one buffer and one write_all() call
                        // rather than three separate ones: with TCP_NODELAY
                        // set, each write_all was its own TCP segment / adb
                        // protocol packet -- three small packets (8, 4 bytes)
                        // ahead of the real payload adds per-packet adb/USB
                        // overhead three times over for no reason.
                        let mut frame_buf = Vec::with_capacity(12 + encoded.len());
                        frame_buf.extend_from_slice(&crate::clock_sync::now_millis().to_be_bytes());
                        frame_buf.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
                        frame_buf.extend_from_slice(&encoded);
                        if let Err(e) = sock.write_all(&frame_buf) {
                            eprintln!("[transport] write failed ({e}), dropping connection");
                            user_data.transport = None;
                        }
                    }
                }
                Err(e) => eprintln!("[capture] encode_frame error: {e}"),
            }
        })
        .register()?;

    eprintln!("[pipewire] created stream, connecting to node {node_id}...");

    let obj = pw::spa::pod::object!(
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
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .unwrap()
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    eprintln!("[pipewire] connected, running (Ctrl+C to stop)...");
    let loop_ = mainloop.loop_();
    while !crate::sigint_received() {
        loop_.iterate(pw::loop_::Timeout::Finite(Duration::from_millis(100)));
    }
    eprintln!("[pipewire] SIGINT received, stopping...");

    let final_stats = stats.borrow().clone();
    eprintln!("[pipewire] stats computed: {} frames, returning...", final_stats.frame_count);
    Ok(final_stats)
}
