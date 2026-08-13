//! Virtual-monitor capture via the standard xdg-desktop-portal ScreenCast
//! interface + PipeWire, replacing the evdi-based capture path (see
//! MILESTONES.md, Milestone 2 findings, for why: evdi caused real freezes
//! under both Wayland and X11 on this machine; this path never touches
//! DRM/KMS at all).
//!
//! The virtual monitor itself is created separately (by `krfb-virtualmonitor`
//! for now); this module just captures whatever monitor the user picks in
//! the portal's screen-selection dialog.

use crate::color_convert::bgrx_to_nv12;
use crate::vaapi_encoder::VaapiEncoder;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType, Stream as PortalStream};
use ashpd::desktop::PersistMode;
use pipewire as pw;
use pw::spa;
use pw::{properties::properties, spa::pod::Pod};
use std::cell::RefCell;
use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct CaptureStats {
    pub frame_count: u64,
    pub durations: Vec<Duration>,
}

/// Negotiates a ScreenCast session via the portal. Triggers KDE's native
/// screen-picker dialog -- the user must select a monitor and confirm.
pub async fn open_portal() -> ashpd::Result<(PortalStream, OwnedFd)> {
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded, // bake the cursor into frames, simplest for v0
            SourceType::Monitor.into(),
            false,
            None,
            PersistMode::DoNot,
        )
        .await?;

    let response = proxy.start(&session, None).await?.response()?;
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
    y_plane: Vec<u8>,
    uv_plane: Vec<u8>,
    out_file: File,
    stats: Rc<RefCell<CaptureStats>>,
}

pub fn run_capture(node_id: u32, fd: OwnedFd, out_path: &str) -> Result<CaptureStats, pw::Error> {
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
    let stats = Rc::new(RefCell::new(CaptureStats::default()));
    let data = CaptureData {
        format: Default::default(),
        encoder: None,
        y_plane: Vec::new(),
        uv_plane: Vec::new(),
        out_file,
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
            let aw = encoder.aligned_width() as usize;
            let ah = encoder.aligned_height() as usize;
            user_data.y_plane = vec![0u8; aw * ah];
            user_data.uv_plane = vec![0u8; aw * (ah / 2)];
            user_data.encoder = Some(encoder);
        })
        .process(|stream, user_data| {
            let start = Instant::now();
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
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

            let Some(encoder) = user_data.encoder.as_mut() else {
                return;
            };
            let width = encoder.width() as usize;
            let height = encoder.height() as usize;
            let aligned_width = encoder.aligned_width() as usize;

            bgrx_to_nv12(
                bytes,
                width,
                height,
                stride,
                &mut user_data.y_plane,
                aligned_width,
                &mut user_data.uv_plane,
                aligned_width,
            );

            match encoder.encode_frame(&user_data.y_plane, &user_data.uv_plane) {
                Ok(encoded) => {
                    let elapsed = start.elapsed();
                    let mut stats = user_data.stats.borrow_mut();
                    stats.frame_count += 1;
                    stats.durations.push(elapsed);
                    if stats.frame_count == 1 || stats.frame_count % 30 == 0 {
                        eprintln!(
                            "[capture] frame {}: {} bytes, {:?} (dequeue->encoded)",
                            stats.frame_count,
                            encoded.len(),
                            elapsed
                        );
                    }
                    drop(stats);
                    let _ = user_data.out_file.write_all(&encoded);
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
        // reuses color_convert::bgrx_to_nv12 unchanged.
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

    let frame_count = stats.borrow().frame_count;
    let durations = stats.borrow().durations.clone();
    eprintln!("[pipewire] stats computed: {frame_count} frames, returning...");
    Ok(CaptureStats { frame_count, durations })
}
