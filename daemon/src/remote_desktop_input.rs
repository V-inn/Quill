//! Input backend for machines where `/dev/uinput` isn't accessible and never
//! will be without root (e.g. a school computer with no sudo, ever) --
//! `uinput_tablet.rs`'s virtual tablet needs a root-granted udev rule or ACL
//! to open that device at all, and there's no userspace way around a kernel
//! DAC permission check on a real host device node.
//!
//! Falls back to `org.freedesktop.portal.RemoteDesktop`, the same
//! zero-permission portal mechanism `portal_capture.rs` already uses for
//! `ScreenCast` (one consent dialog, no udev rule, works on any modern
//! compositor). Real trade-off, not a free lunch: the portal's pointer/touch
//! API has no pressure or tilt axis at all, so this gives mouse/finger-tier
//! input (position + click), not real S Pen digitizer fidelity. That's the
//! ceiling of what's possible without a privileged uinput grant -- see
//! MILESTONES.md.
//!
//! `NotifyPointerMotionAbsolute`/`NotifyTouch*` take coordinates in the
//! shared ScreenCast stream's own logical pixel space, not the tablet's
//! panel pixel space -- unlike the uinput path (where KDE's own per-device
//! tablet-area calibration does that mapping), this backend has to do that
//! scaling itself (see `InputSink::emit` in `input_receiver.rs`).

use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType, Stream as PortalStream};
use ashpd::desktop::PersistMode;
use input_linux::Key;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{Receiver, Sender};

enum Cmd {
    Motion { x: f64, y: f64 },
    Button { pressed: bool, side: bool },
}

/// Cheap, `Clone`-able handle to the background thread that owns the actual
/// portal session -- see the module doc for why this can't just reuse the
/// daemon's own single-threaded runtime (that thread is permanently blocked
/// inside `run_capture`'s pipewire loop by the time input events start
/// arriving).
#[derive(Clone)]
pub struct RemoteDesktopInput {
    tx: Sender<Cmd>,
    /// The shared ScreenCast stream's negotiated size, needed to scale
    /// incoming tablet-panel-pixel coordinates into the stream's own
    /// logical coordinate space (see module doc).
    pub stream_size: (u32, u32),
}

impl RemoteDesktopInput {
    pub fn pointer_motion(&self, x: f64, y: f64) {
        let _ = self.tx.send(Cmd::Motion { x, y });
    }

    /// `side`: true for the S Pen's physical side button (mapped to
    /// BTN_RIGHT, the closest generic-pointer analogue), false for tip
    /// contact / finger touch (mapped to BTN_LEFT, i.e. "drag").
    pub fn button(&self, pressed: bool, side: bool) {
        let _ = self.tx.send(Cmd::Button { pressed, side });
    }
}

async fn negotiate() -> ashpd::Result<(RemoteDesktop<'static>, ashpd::desktop::Session<'static, RemoteDesktop<'static>>, PortalStream, OwnedFd)> {
    let remote_desktop = RemoteDesktop::new().await?;
    let screencast = Screencast::new().await?;
    let session = remote_desktop.create_session().await?;

    // No restore-token/persistence here (unlike portal_capture.rs's
    // ScreenCast negotiation): this fallback only exists for the
    // no-sudo-ever manual-run case, which can't have the udev-rule-based
    // auto-launch path anyway (that also needs root to install) -- so
    // there's no unattended relaunch to spare a dialog for.
    remote_desktop
        .select_devices(&session, DeviceType::Pointer.into(), None, PersistMode::DoNot)
        .await?;
    screencast
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            false,
            None,
            PersistMode::DoNot,
        )
        .await?;

    let response = remote_desktop.start(&session, None).await?.response()?;
    let stream = response
        .streams()
        .and_then(|s| s.first())
        .expect("no stream found / selected")
        .to_owned();
    let fd = screencast.open_pipe_wire_remote(&session).await?;

    Ok((remote_desktop, session, stream, fd))
}

/// Negotiates a combined RemoteDesktop+ScreenCast portal session (one
/// consent dialog covering both video capture and pointer control) and
/// spawns the dedicated thread that owns it for the rest of the process's
/// life. Returns the same `(Stream, OwnedFd)` shape `portal_capture::
/// open_portal` does, so the video capture path downstream is unaffected,
/// plus the `RemoteDesktopInput` handle `input_receiver.rs` sends events
/// through.
pub async fn open_portal_with_input() -> ashpd::Result<(PortalStream, OwnedFd, RemoteDesktopInput)> {
    let (remote_desktop, session, stream, fd) = negotiate().await?;
    let stream_size = stream.size().map(|(w, h)| (w as u32, h as u32)).unwrap_or((1920, 1080));
    let node_id = stream.pipe_wire_node_id();

    let (tx, rx): (Sender<Cmd>, Receiver<Cmd>) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Own dedicated runtime, entirely separate from main's -- by the
        // time input events start flowing, main's single-threaded runtime
        // is permanently blocked inside run_capture's pipewire loop (see
        // main.rs), so there's nothing there to drive these calls anyway.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build remote-desktop-input runtime");
        rt.block_on(async move {
            // The only task on this runtime, so a blocking `recv()` between
            // awaits doesn't starve anything else -- same reasoning as
            // `run_input_writer_loop`'s blocking queue on the Android side.
            while let Ok(cmd) = rx.recv() {
                let result = match cmd {
                    Cmd::Motion { x, y } => {
                        remote_desktop
                            .notify_pointer_motion_absolute(&session, node_id, x, y)
                            .await
                    }
                    Cmd::Button { pressed, side } => {
                        let code = if side { Key::ButtonRight } else { Key::ButtonLeft } as u16 as i32;
                        let state = if pressed { KeyState::Pressed } else { KeyState::Released };
                        remote_desktop.notify_pointer_button(&session, code, state).await
                    }
                };
                if let Err(e) = result {
                    eprintln!("[remote-desktop-input] notify failed: {e}");
                }
            }
        });
    });

    Ok((stream, fd, RemoteDesktopInput { tx, stream_size }))
}
