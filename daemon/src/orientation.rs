//! Ensures the `krfb-virtualmonitor` output matches the tablet's reported
//! orientation, recreating it at the handshake's exact width x height when
//! they don't already match -- chosen once by the Android app at connect
//! time (see MainActivity.kt), not live-switchable mid-session.
//!
//! First attempt (Milestone 15) tried a plain `kscreen-doctor` rotation of
//! an *already-created* landscape output instead of recreating it. Live-
//! tested and found broken two different ways at once: the captured video
//! was black except for a cursor trail that never cleared, and tablet
//! touch input didn't track the rotated geometry either -- both consistent
//! with the screencast/input-mapping stack not correctly following a
//! runtime rotation transform on a headless output, even though KScreen's
//! own metadata (`kscreen-doctor -j`) reported the swapped size correctly.
//! Landscape at whatever exact resolution the handshake reports has worked
//! cleanly all session because it's always been *created* at that shape
//! from the start -- so portrait gets the same treatment here instead:
//! recreate the process at the real width/height, no separate rotation
//! transform involved for the 90-degree aspect swap.
//!
//! This reuses most of Milestone 13's reverted create/resize logic. That
//! revert's actual regression (confirmed by later live testing, Milestone
//! 14) turned out to be an unrelated Android-side decoder bug, not this
//! mechanism -- safe to bring back for the piece it was never actually
//! guilty of.

use serde_json::Value;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Matches the name already in manual use on this machine.
const OUTPUT_NAME: &str = "QuillDisplay";
const RFB_PORT: u16 = 5900;

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const READY_TIMEOUT: Duration = Duration::from_secs(5);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(3);

fn virtual_output_name() -> String {
    format!("Virtual-{OUTPUT_NAME}")
}

/// The named output's current (width, height) per `kscreen-doctor -j`, or
/// `None` if it doesn't exist (or the query/parse failed -- treated the
/// same as "doesn't exist", since either way a fresh one needs creating).
fn current_size(name: &str) -> Option<(u32, u32)> {
    let out = Command::new("kscreen-doctor").arg("-j").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    let output = json
        .get("outputs")?
        .as_array()?
        .iter()
        .find(|o| o.get("name").and_then(Value::as_str) == Some(name))?;
    let current_mode_id = output.get("currentModeId")?.as_str()?;
    let mode = output
        .get("modes")?
        .as_array()?
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(current_mode_id))?;
    let size = mode.get("size")?;
    Some((size.get("width")?.as_u64()? as u32, size.get("height")?.as_u64()? as u32))
}

/// A rectangle in the compositor's *logical* coordinate space -- the space
/// output positions and pointer coordinates live in, which is the pixel size
/// divided by the output's scale factor. On this machine the panel is
/// 1920x1080 at scale 1.25 and the virtual output 2560x1600 at scale 1.5, so
/// the two are 1536x864 and 1707x1067 logically, laid out side by side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Where the virtual monitor sits inside the whole desktop.
///
/// Needed because gestures and clicks are delivered to whatever the *pointer*
/// is over, and a relative device (the virtual touchpad) can't put it
/// anywhere. Warping an absolute pointer there requires knowing both the
/// target rectangle and the full desktop it is mapped across -- see
/// `uinput_buttons.rs`.
#[derive(Clone, Copy, Debug)]
pub struct DesktopLayout {
    pub output: Rect,
    pub desktop: Rect,
}

fn logical_rect(output: &Value) -> Option<Rect> {
    let pos = output.get("pos")?;
    let scale = output.get("scale").and_then(Value::as_f64).unwrap_or(1.0).max(0.1);
    let current_mode_id = output.get("currentModeId")?.as_str()?;
    let mode = output
        .get("modes")?
        .as_array()?
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(current_mode_id))?;
    let size = mode.get("size")?;
    Some(Rect {
        x: pos.get("x")?.as_f64()?,
        y: pos.get("y")?.as_f64()?,
        w: size.get("width")?.as_f64()? / scale,
        h: size.get("height")?.as_f64()? / scale,
    })
}

/// The virtual output's rectangle plus the bounding box of every enabled
/// output, both logical. `None` if the output isn't there or kscreen-doctor
/// can't be read -- the caller then does without pointer warping rather than
/// guessing at a layout.
pub fn layout() -> Option<DesktopLayout> {
    let out = Command::new("kscreen-doctor").arg("-j").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&out.stdout).ok()?;
    let outputs = json.get("outputs")?.as_array()?;
    let name = virtual_output_name();

    let mut desktop: Option<Rect> = None;
    let mut ours: Option<Rect> = None;
    for output in outputs {
        if output.get("enabled").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let Some(rect) = logical_rect(output) else { continue };
        if output.get("name").and_then(Value::as_str) == Some(name.as_str()) {
            ours = Some(rect);
        }
        desktop = Some(match desktop {
            None => rect,
            Some(d) => {
                let x = d.x.min(rect.x);
                let y = d.y.min(rect.y);
                Rect {
                    x,
                    y,
                    w: (d.x + d.w).max(rect.x + rect.w) - x,
                    h: (d.y + d.h).max(rect.y + rect.h) - y,
                }
            }
        });
    }
    Some(DesktopLayout { output: ours?, desktop: desktop? })
}

/// Not a secret worth persisting -- this RFB server only exists because
/// `krfb-virtualmonitor` requires *a* password to start at all. Freshly
/// random per launch instead of a fixed string in source.
fn random_password() -> String {
    let mut buf = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .expect("failed to read /dev/urandom for a throwaway RFB password");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn poll_until(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if check() {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Ensures a `krfb-virtualmonitor`-backed output exists at exactly
/// `width`x`height`, creating or recreating it as needed, and blocks until
/// KWin actually reports it before returning -- so the portal picker that
/// runs right after this always sees a monitor of the right shape.
///
/// A 180-degree flip (needed on this machine for portrait, cable position)
/// is deliberately *not* handled here via `kscreen-doctor` rotation --
/// confirmed live (Milestone 16) that KWin's rotation property has no
/// effect at all on what this output's screencast producer exports, even
/// set directly via System Settings, not just this daemon's own calls. See
/// `vaapi_encoder.rs`'s `flip_180` (GPU-side, actually works) and
/// `input_receiver.rs`'s matching touch-coordinate reflection instead.
///
/// No-ops the recreate step (does not touch the running process or its RFB
/// port/password) if an output of the right name and size already exists:
/// recreating it unnecessarily on every launch would invalidate the
/// portal's saved restore token every time, forcing a human to click
/// through the picker dialog on every USB attach.
///
/// `width`/`height`: the handshake's values, already bounds-checked by the
/// caller (see `input_receiver.rs`) -- this function trusts them.
pub fn ensure(width: u32, height: u32) {
    let name = virtual_output_name();

    if current_size(&name) == Some((width, height)) {
        eprintln!("[orientation] {name} already {width}x{height}, reusing");
        return;
    }

    eprintln!("[orientation] (re)creating {name} at {width}x{height}...");

    // Best-effort: `pkill` exits non-zero when nothing matched, the common
    // case on first run -- not an error condition here.
    let _ = Command::new("pkill").args(["-f", "krfb-virtualmonitor"]).status();
    poll_until(TEARDOWN_TIMEOUT, || current_size(&name).is_none());

    let spawned = Command::new("krfb-virtualmonitor")
        .args([
            "--resolution",
            &format!("{width}x{height}"),
            "--name",
            OUTPUT_NAME,
            "--password",
            &random_password(),
            "--port",
            &RFB_PORT.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    // Deliberately not `.wait()`'d or kept around: meant to keep running as
    // the compositor output's owner for as long as the desktop session
    // lasts, well past this daemon process's own lifetime.
    if let Err(e) = spawned {
        eprintln!("[orientation] failed to spawn krfb-virtualmonitor: {e} -- continuing without it");
        return;
    }

    if poll_until(READY_TIMEOUT, || current_size(&name) == Some((width, height))) {
        eprintln!("[orientation] {name} ready at {width}x{height}");
    } else {
        eprintln!("[orientation] {name} didn't come up at {width}x{height} within {READY_TIMEOUT:?} -- continuing without it");
    }
}
