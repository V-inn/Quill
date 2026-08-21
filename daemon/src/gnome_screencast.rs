//! The GNOME half of the virtual display: mutter's own
//! `org.gnome.Mutter.ScreenCast` interface, whose `RecordVirtual` method
//! creates a virtual monitor *and* hands back the PipeWire node that carries
//! its pixels, in one session.
//!
//! This is what closes the gap Milestone 1 flagged and Milestone 10 left open
//! ("KWin has a native, compositor-level virtual-output mechanism... no stable
//! cross-desktop equivalent exists"). Mutter's equivalent turned out to exist
//! and to be *better* suited to this daemon than KDE's, for three reasons:
//!
//! 1. **One step, not two.** On KDE the output and the capture are separate
//!    (`krfb-virtualmonitor` creates the monitor, the xdg ScreenCast portal
//!    captures whichever monitor a human then picks out of a dialog). Here the
//!    monitor exists *because* something is capturing it.
//! 2. **No picker dialog and no restore token.** The entire
//!    `restore_token`/`PersistMode` apparatus in `portal_capture.rs` exists so
//!    that a daemon auto-launched from a udev rule doesn't stall on a dialog
//!    nobody is there to click. None of it is needed on this path -- there is
//!    no dialog to skip. (Mutter's only access check on these calls is that
//!    the caller is the same D-Bus sender that created the session; see
//!    `check_permission` in mutter's `meta-screen-cast-session.c`.)
//! 3. **We choose the resolution.** The tablet's exact `width x height` from
//!    the capability handshake goes straight into the request, so there is no
//!    equivalent of `orientation::ensure`'s tear-down-and-recreate dance.
//!
//! Consequence for the sudo story: nothing on this path needs root, ever. The
//! one and only privileged step left on GNOME is granting access to
//! `/dev/uinput` (see `packaging/70-quill-uinput.rules`), which is a single
//! one-time udev rule.
//!
//! **Not yet live-tested.** Written against mutter's published D-Bus interface
//! and its current implementation of it; this project's only machine runs
//! Plasma. Every call site here logs what it asked for and what came back, and
//! the one property whose exact wire shape is hardest to be sure of (`modes`)
//! has an explicit retry-without-it fallback, precisely because the first real
//! GNOME run is going to be someone else's.

use crate::portal_capture::CursorRendering;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, Value};

/// Long enough to cover a compositor that is busy, short enough that a daemon
/// auto-launched on USB attach fails fast and lets systemd retry rather than
/// hanging forever with the tablet already handshaked and waiting.
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast",
    default_service = "org.gnome.Mutter.ScreenCast",
    default_path = "/org/gnome/Mutter/ScreenCast",
    gen_blocking = false
)]
trait ScreenCast {
    fn create_session(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    /// Mutter's own API version, bumped when methods or properties are added.
    /// 2 added `cursor-mode`, 3 added `is-platform`, 4 added `is-recording`.
    #[zbus(property)]
    fn version(&self) -> zbus::Result<i32>;
}

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast.Session",
    default_service = "org.gnome.Mutter.ScreenCast",
    gen_blocking = false
)]
trait ScreenCastSession {
    fn start(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;

    fn record_virtual(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    #[zbus(signal)]
    fn closed(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast.Stream",
    default_service = "org.gnome.Mutter.ScreenCast",
    gen_blocking = false
)]
trait ScreenCastStream {
    #[zbus(signal, name = "PipeWireStreamAdded")]
    fn pipewire_stream_added(&self, node_id: u32) -> zbus::Result<()>;
}

impl CursorRendering {
    /// Mutter's cursor-mode enum happens to use the same three values as the
    /// portal's (0 hidden, 1 embedded, 2 metadata), but they're separate APIs
    /// -- spelled out here rather than reusing `portal_mode()`'s value.
    fn mutter_mode(self) -> u32 {
        match self {
            CursorRendering::Embedded => 1,
            CursorRendering::ClientSide => 2,
        }
    }
}

/// Asks mutter for a virtual monitor of exactly `width x height` and returns
/// the PipeWire node id its contents are published on -- the same kind of id
/// `open_portal` gets out of the portal, and consumed the same way by
/// `run_capture`.
///
/// Blocks until mutter has actually created the PipeWire node (or
/// `SETUP_TIMEOUT` elapses), so the caller can go straight into capture.
///
/// The session is owned by a thread this spawns and never joins. That is not
/// laziness: mutter ties the session's lifetime to the D-Bus *sender* that
/// created it, so the connection has to outlive this function and stay
/// serviced for the whole run, and `run_capture` blocks the main thread on
/// PipeWire's loop the entire time (the tokio runtime there stops being polled
/// the moment it's called). A thread with a runtime of its own is what keeps
/// the bus connection answering while that happens. When the process exits,
/// the connection drops, and mutter tears the virtual monitor down with it --
/// which is why, unlike the `krfb-virtualmonitor` process on KDE, nothing is
/// left behind for the next run to clean up.
pub fn start(width: u32, height: u32, cursor: CursorRendering) -> Result<u32, String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();

    std::thread::Builder::new()
        .name("quill-mutter-screencast".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("couldn't build a runtime for the D-Bus session: {e}")));
                    return;
                }
            };
            rt.block_on(hold_session(width, height, cursor, tx));
        })
        .map_err(|e| format!("couldn't spawn the D-Bus session thread: {e}"))?;

    // The thread reports its own timeouts; this one only has to outlast them.
    match rx.recv_timeout(SETUP_TIMEOUT + Duration::from_secs(5)) {
        Ok(result) => result,
        Err(e) => Err(format!("the D-Bus session thread never reported back: {e}")),
    }
}

/// Sets the session up, reports the node id back, then stays parked on the
/// session's `Closed` signal for the rest of the process's life.
async fn hold_session(
    width: u32,
    height: u32,
    cursor: CursorRendering,
    tx: std::sync::mpsc::Sender<Result<u32, String>>,
) {
    let (node_id, session, _conn) = match open_session(width, height, cursor).await {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };

    let closed = session.receive_closed().await;
    if tx.send(Ok(node_id)).is_err() {
        // Caller gave up (its own recv timed out) -- nothing left to hold open.
        return;
    }

    let Ok(mut closed) = closed else {
        // Without the signal there's still a live connection to hold open,
        // which is this thread's actual job; it just can't report a teardown.
        eprintln!("[gnome] couldn't subscribe to the session's Closed signal -- a compositor restart will look like a frozen stream rather than an exit");
        std::future::pending::<()>().await;
        return;
    };

    closed.next().await;
    // Same reasoning as the transport's write failures (see `run_capture`'s
    // heartbeat and `input_receiver.rs`): the virtual monitor is gone and
    // every frame from here on would be captured from nothing. Exit non-zero
    // so systemd restarts us into a fresh session instead of streaming a dead
    // output at a tablet that has no way to know.
    eprintln!("[gnome] mutter closed the ScreenCast session (compositor restart?) -- exiting so systemd can retry");
    std::process::exit(1);
}

/// The actual D-Bus exchange: create a session, ask for a virtual monitor,
/// subscribe *before* starting (the node-id signal can otherwise land between
/// `Start` returning and the subscription existing), start, and wait for the
/// node id.
async fn open_session(
    width: u32,
    height: u32,
    cursor: CursorRendering,
) -> Result<(u32, ScreenCastSessionProxy<'static>, zbus::Connection), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("couldn't connect to the session bus: {e}"))?;

    let screen_cast = ScreenCastProxy::new(&conn)
        .await
        .map_err(|e| format!("couldn't reach org.gnome.Mutter.ScreenCast: {e}"))?;

    // Not fatal on its own -- the properties below are all ignored rather than
    // rejected by a mutter too old to know them (each is a `g_variant_lookup`
    // that simply doesn't match). It's logged because it's the single most
    // useful line in a bug report from a machine this has never run on.
    let version = screen_cast.version().await.unwrap_or(-1);
    eprintln!("[gnome] org.gnome.Mutter.ScreenCast API version {version}");

    let session_path = screen_cast
        .create_session(HashMap::new())
        .await
        .map_err(|e| format!("CreateSession failed: {e}"))?;
    eprintln!("[gnome] session: {}", session_path.as_str());

    let session = ScreenCastSessionProxy::builder(&conn)
        .path(session_path.clone())
        .map_err(|e| format!("bad session path {}: {e}", session_path.as_str()))?
        .build()
        .await
        .map_err(|e| format!("couldn't build the session proxy: {e}"))?;

    let stream_path = record_virtual(&session, width, height, cursor).await?;
    eprintln!("[gnome] virtual stream: {}", stream_path.as_str());

    let stream = ScreenCastStreamProxy::builder(&conn)
        .path(stream_path.clone())
        .map_err(|e| format!("bad stream path {}: {e}", stream_path.as_str()))?
        .build()
        .await
        .map_err(|e| format!("couldn't build the stream proxy: {e}"))?;

    let mut added = stream
        .receive_pipewire_stream_added()
        .await
        .map_err(|e| format!("couldn't subscribe to PipeWireStreamAdded: {e}"))?;

    session
        .start()
        .await
        .map_err(|e| format!("Session.Start failed: {e}"))?;

    let signal = tokio::time::timeout(SETUP_TIMEOUT, added.next())
        .await
        .map_err(|_| {
            format!("mutter accepted the virtual monitor but never emitted PipeWireStreamAdded within {SETUP_TIMEOUT:?}")
        })?
        .ok_or_else(|| "the PipeWireStreamAdded subscription ended before the signal arrived".to_string())?;

    let node_id = signal
        .args()
        .map_err(|e| format!("couldn't read the node id out of PipeWireStreamAdded: {e}"))?
        .node_id;

    eprintln!("[gnome] virtual monitor up at {width}x{height}, PipeWire node {node_id}");
    Ok((node_id, session, conn))
}

/// Builds the `RecordVirtual` property dict and calls it, retrying without
/// `modes` if mutter rejects that argument.
///
/// The retry is the whole reason this is its own function. `modes` pins the
/// virtual monitor to one exact size (mutter's docs: "the PipeWire stream
/// becomes non-resizable, and size is controlled by the compositor as if it
/// was a regular monitor"), which is exactly what a tablet with one fixed
/// panel resolution wants. But it is also the newest of these properties and
/// the only one mutter *validates* rather than ignores -- a shape it doesn't
/// like comes back as "Invalid modes passed", where an unknown property would
/// have been silently skipped. Without `modes`, mutter sizes the monitor from
/// whatever the PipeWire format negotiation settles on instead, and
/// `run_capture` proposes the same `width x height` there as its preferred
/// size, so both routes land on the same monitor.
async fn record_virtual(
    session: &ScreenCastSessionProxy<'_>,
    width: u32,
    height: u32,
    cursor: CursorRendering,
) -> Result<OwnedObjectPath, String> {
    let first = match session.record_virtual(properties(width, height, cursor, true)).await {
        Ok(path) => return Ok(path),
        Err(e) => e,
    };
    // Retried on *any* error, not just a modes-specific one: mutter's only
    // documented wording is "Invalid modes passed", and matching on an error
    // string is a worse bet than one extra call. Both errors are reported if
    // the retry fails too, since the first is the more likely to be the real
    // reason.
    eprintln!(
        "[gnome] RecordVirtual with an explicit {width}x{height} mode was rejected ({first}) \
         -- retrying with PipeWire-negotiated sizing"
    );
    session
        .record_virtual(properties(width, height, cursor, false))
        .await
        .map_err(|retry| format!("RecordVirtual failed: {retry} (with an explicit mode: {first})"))
}

fn properties(
    width: u32,
    height: u32,
    cursor: CursorRendering,
    with_modes: bool,
) -> HashMap<&'static str, Value<'static>> {
    let mut props: HashMap<&'static str, Value<'static>> = HashMap::new();
    props.insert("cursor-mode", Value::from(cursor.mutter_mode()));
    // "it will not be interpreted as if the screen is shared, but more
    // transparently as if it was a real monitor" -- which is what this is. The
    // visible difference is GNOME's screen-sharing indicator staying out of the
    // top bar for a monitor the user is simply *using*.
    props.insert("is-platform", Value::from(is_platform()));

    if with_modes {
        props.insert("modes", Value::from(vec![mode(width, height)]));
    }

    props
}

/// One entry of `RecordVirtual`'s `modes` list. Mutter reads each key with a
/// `g_variant_lookup` naming an exact type string, so `size` has to be `(uu)`
/// and the two floats `d`; see `create_mode_infos` in
/// `meta-screen-cast-session.c`.
fn mode(width: u32, height: u32) -> HashMap<&'static str, Value<'static>> {
    let mut mode: HashMap<&'static str, Value<'static>> = HashMap::new();
    mode.insert("size", Value::from((width, height)));
    // 60 is also mutter's own default when this is omitted; stated explicitly
    // so it matches the framerate `build_format_pod` offers.
    mode.insert("refresh-rate", Value::from(60.0f64));
    // Mutter rejects a mode list without exactly one preferred entry.
    mode.insert("is-preferred", Value::from(true));
    if let Some(scale) = preferred_scale() {
        // Left unset by default on purpose: with no physical size to go on,
        // mutter picks a scale for the monitor itself, and overriding that from
        // a daemon would silently override the user's own display settings too.
        // `QUILL_GNOME_SCALE` is for the case where it guesses wrong on a
        // particular tablet.
        mode.insert("preferred-scale", Value::from(scale));
    }
    mode
}

/// `QUILL_GNOME_IS_PLATFORM=0` turns the "treat it as a real monitor" hint off,
/// which is the difference between GNOME showing its screen-sharing indicator
/// for this output or not. Kept overridable because it is a hint about intent,
/// and someone may well want the indicator.
fn is_platform() -> bool {
    !matches!(
        std::env::var("QUILL_GNOME_IS_PLATFORM").as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    )
}

fn preferred_scale() -> Option<f64> {
    let raw = std::env::var("QUILL_GNOME_SCALE").ok()?;
    match raw.parse::<f64>() {
        Ok(scale) if scale > 0.0 => Some(scale),
        _ => {
            eprintln!("[gnome] QUILL_GNOME_SCALE={raw} is not a positive number -- ignoring it");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Array;

    /// The one thing about this file that *can* be checked without a GNOME
    /// machine, and the thing most likely to be silently wrong: mutter reads
    /// every one of these with a `g_variant_lookup` that names an exact type
    /// string, and a mismatch is not an error -- it's a silently skipped
    /// property. `cursor-mode` arriving as `i` instead of `u` wouldn't fail the
    /// call, it would just leave the cursor hidden with no explanation.
    /// (`modes` is the exception that does report a mismatch, hence the retry
    /// in `record_virtual`.) The exact type strings are from mutter's
    /// `handle_record_virtual` and `create_mode_infos`.
    fn signature_of(value: &Value<'_>) -> String {
        value.value_signature().to_string()
    }

    #[test]
    fn record_virtual_properties_match_the_types_mutter_looks_up() {
        let props = properties(2560, 1600, CursorRendering::ClientSide, true);

        assert_eq!(signature_of(&props["cursor-mode"]), "u");
        assert_eq!(signature_of(&props["is-platform"]), "b");
        assert_eq!(signature_of(&props["modes"]), "aa{sv}");

        // And the whole dict is what the method signature says it is.
        let all = Value::from(props);
        assert_eq!(signature_of(&all), "a{sv}");
    }

    #[test]
    fn mode_properties_match_the_types_mutter_looks_up() {
        let mode = mode(2560, 1600);

        assert_eq!(signature_of(&mode["size"]), "(uu)");
        assert_eq!(signature_of(&mode["refresh-rate"]), "d");
        assert_eq!(signature_of(&mode["is-preferred"]), "b");
        assert!(
            bool::try_from(&mode["is-preferred"]).unwrap(),
            "a mode list with no preferred entry is rejected outright"
        );
        assert_eq!(<(u32, u32)>::try_from(&mode["size"]).unwrap(), (2560, 1600));
        assert_eq!(signature_of(&Value::from(mode)), "a{sv}");
    }

    /// One mode, and it is the preferred one -- mutter errors out on a list
    /// with none marked or with more than one.
    #[test]
    fn exactly_one_preferred_mode_is_offered() {
        let props = properties(2560, 1600, CursorRendering::Embedded, true);
        let modes = Array::try_from(props["modes"].try_clone().unwrap()).unwrap();
        assert_eq!(modes.len(), 1);
    }

    /// The retry path: same properties, minus the one argument mutter
    /// validates. Everything else has to survive the drop.
    #[test]
    fn the_fallback_properties_drop_only_modes() {
        let props = properties(2560, 1600, CursorRendering::Embedded, false);
        assert!(!props.contains_key("modes"));
        assert!(props.contains_key("cursor-mode"));
        assert!(props.contains_key("is-platform"));
    }

    #[test]
    fn cursor_modes_are_mutters_numbering() {
        // 0 hidden, 1 embedded, 2 metadata -- see the interface XML.
        assert_eq!(CursorRendering::Embedded.mutter_mode(), 1);
        assert_eq!(CursorRendering::ClientSide.mutter_mode(), 2);
    }
}
