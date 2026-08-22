//! GNOME's answer to `kscreen-doctor -j`: reading the desktop layout back out
//! of `org.gnome.Mutter.DisplayConfig` so the pointer can be warped onto the
//! tablet's output.
//!
//! Same job as the `layout()` half of `orientation.rs`, and the same output
//! type -- see `orientation::DesktopLayout` for *why* any of this is needed
//! (gestures and clicks land wherever the pointer is, and the virtual touchpad
//! is a relative device that can't move it).
//!
//! Two things make this more work than the KDE version rather than less:
//!
//! - **Logical size isn't handed to us.** `GetCurrentState` reports each
//!   logical monitor's position, scale and transform, but its *size* only
//!   indirectly, as the current mode of the physical monitor(s) mapped onto
//!   it. So the pixel size gets looked up per connector, swapped if the output
//!   is rotated a quarter turn, and divided by the scale.
//! - **Our monitor has no name we chose.** `krfb-virtualmonitor` takes a
//!   `--name` and KWin echoes it back as `Virtual-QuillDisplay`. Mutter names
//!   virtual monitors itself; what it does set, in
//!   `meta-stream-source-virtual.c`, is a fixed vendor/product pair
//!   ("MetaVendor" / "Virtual remote monitor") and a per-monitor serial
//!   counter. That pair is the handle we get, so a second virtual monitor from
//!   some other app (gnome-remote-desktop, say) is disambiguated by size.
//!
//! **Not yet live-tested** -- see `gnome_screencast.rs` for the same caveat and
//! why. The failure mode here is the mild one: `layout()` returning `None`
//! costs pointer warping, not the display or the pen (`Pointer::map` already
//! handles it and says so).

use crate::orientation::{DesktopLayout, Rect};
use serde::Deserialize;
use std::collections::HashMap;
use zbus::zvariant::{OwnedValue, Type};

/// What mutter stamps on every virtual monitor it creates for a screen-cast
/// stream. Not configurable and not derived from anything we send, so matching
/// on it is the only way to pick our own output out of the layout.
const VIRTUAL_VENDOR: &str = "MetaVendor";
const VIRTUAL_PRODUCT: &str = "Virtual remote monitor";

/// `(connector, vendor, product, serial)` -- mutter's monitor identity tuple,
/// which is what logical monitors refer to their physical monitors by.
type MonitorId = (String, String, String, String);

#[derive(Debug, Deserialize, Type)]
struct Mode {
    #[allow(dead_code)]
    id: String,
    width: i32,
    height: i32,
    #[allow(dead_code)]
    refresh_rate: f64,
    #[allow(dead_code)]
    preferred_scale: f64,
    #[allow(dead_code)]
    supported_scales: Vec<f64>,
    properties: HashMap<String, OwnedValue>,
}

#[derive(Debug, Deserialize, Type)]
struct Monitor {
    id: MonitorId,
    modes: Vec<Mode>,
    #[allow(dead_code)]
    properties: HashMap<String, OwnedValue>,
}

#[derive(Debug, Deserialize, Type)]
struct LogicalMonitor {
    x: i32,
    y: i32,
    scale: f64,
    /// 0-7: the even values are rotations (0/90/180/270), the odd ones the
    /// same rotations with a flip. Only whether it's a quarter turn matters
    /// here, since that's what swaps width and height.
    transform: u32,
    #[allow(dead_code)]
    primary: bool,
    monitors: Vec<MonitorId>,
    #[allow(dead_code)]
    properties: HashMap<String, OwnedValue>,
}

type CurrentState = (u32, Vec<Monitor>, Vec<LogicalMonitor>, HashMap<String, OwnedValue>);

#[zbus::proxy(
    interface = "org.gnome.Mutter.DisplayConfig",
    default_service = "org.gnome.Mutter.DisplayConfig",
    default_path = "/org/gnome/Mutter/DisplayConfig",
    gen_blocking = false
)]
trait DisplayConfig {
    fn get_current_state(&self) -> zbus::Result<CurrentState>;
}

impl Monitor {
    fn connector(&self) -> &str {
        &self.id.0
    }

    fn is_quill_virtual(&self) -> bool {
        self.id.1 == VIRTUAL_VENDOR && self.id.2 == VIRTUAL_PRODUCT
    }

    /// The mode mutter reports as active, in pixels. `is-current` is the
    /// per-mode property that marks it; a monitor with none (possible while a
    /// virtual output is still being set up) has no usable size yet.
    fn current_size(&self) -> Option<(i32, i32)> {
        let mode = self.modes.iter().find(|m| {
            m.properties
                .get("is-current")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false)
        })?;
        Some((mode.width, mode.height))
    }
}

impl LogicalMonitor {
    /// Where this monitor sits in the compositor's *logical* coordinate space
    /// -- the space output positions and pointer coordinates live in, which is
    /// the pixel size divided by the scale.
    fn rect(&self, sizes: &HashMap<&str, (i32, i32)>) -> Option<Rect> {
        let (mut w, mut h) = self
            .monitors
            .iter()
            .find_map(|m| sizes.get(m.0.as_str()).copied())?;
        // Quarter turns (90 / 270, flipped or not) swap the axes.
        if self.transform % 4 == 1 || self.transform % 4 == 3 {
            std::mem::swap(&mut w, &mut h);
        }
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        Some(Rect {
            x: self.x as f64,
            y: self.y as f64,
            w: w as f64 / scale,
            h: h as f64 / scale,
        })
    }
}

/// The virtual output's rectangle plus the bounding box of the whole desktop,
/// both logical -- the GNOME counterpart of `orientation::layout`.
///
/// `want`: the size the virtual monitor was asked for, used only to break a tie
/// if more than one mutter virtual monitor is present.
///
/// `None` if mutter can't be reached, or if no virtual monitor is in the layout
/// yet -- the caller then does without pointer warping rather than guessing.
pub fn layout(want: (u32, u32)) -> Option<DesktopLayout> {
    let state = crate::desktop::block_on_dbus(|| async {
        let conn = zbus::Connection::session().await.ok()?;
        let proxy = DisplayConfigProxy::new(&conn).await.ok()?;
        match proxy.get_current_state().await {
            Ok(state) => Some(state),
            Err(e) => {
                eprintln!("[gnome] DisplayConfig.GetCurrentState failed: {e}");
                None
            }
        }
    })
    .flatten()?;

    let (_serial, monitors, logical_monitors, _props) = state;
    build_layout(&monitors, &logical_monitors, want)
}

/// Split out from `layout` so the geometry can be tested without a session bus
/// -- see the tests at the bottom, which are the only part of this file this
/// machine can actually exercise.
fn build_layout(
    monitors: &[Monitor],
    logical_monitors: &[LogicalMonitor],
    want: (u32, u32),
) -> Option<DesktopLayout> {
    let sizes: HashMap<&str, (i32, i32)> = monitors
        .iter()
        .filter_map(|m| Some((m.connector(), m.current_size()?)))
        .collect();

    // More than one mutter virtual monitor can exist at once (a
    // gnome-remote-desktop session, another screen-cast app). Prefer one whose
    // current mode is the size we asked for; failing that, take the first --
    // wrong is still better than no warping at all, and it's logged.
    let virtual_connectors: Vec<&str> = monitors
        .iter()
        .filter(|m| m.is_quill_virtual())
        .map(Monitor::connector)
        .collect();
    if virtual_connectors.is_empty() {
        eprintln!(
            "[gnome] no {VIRTUAL_VENDOR}/\"{VIRTUAL_PRODUCT}\" output in the layout yet -- \
             pointer warping is off for this run"
        );
        return None;
    }
    let ours = virtual_connectors
        .iter()
        .find(|c| sizes.get(**c) == Some(&(want.0 as i32, want.1 as i32)))
        .copied()
        .unwrap_or(virtual_connectors[0]);
    if virtual_connectors.len() > 1 {
        eprintln!(
            "[gnome] {} virtual outputs present; using {ours} ({}x{} was requested)",
            virtual_connectors.len(),
            want.0,
            want.1
        );
    }

    let mut desktop: Option<Rect> = None;
    let mut output: Option<Rect> = None;
    for logical in logical_monitors {
        let Some(rect) = logical.rect(&sizes) else { continue };
        if logical.monitors.iter().any(|m| m.0 == ours) {
            output = Some(rect);
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

    Some(DesktopLayout { output: output?, desktop: desktop? })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: i32, height: i32, current: bool) -> Mode {
        let mut properties = HashMap::new();
        if current {
            properties.insert(
                "is-current".to_string(),
                OwnedValue::try_from(zbus::zvariant::Value::from(true)).unwrap(),
            );
        }
        Mode {
            id: format!("{width}x{height}"),
            width,
            height,
            refresh_rate: 60.0,
            preferred_scale: 1.0,
            supported_scales: vec![1.0],
            properties,
        }
    }

    fn monitor(connector: &str, vendor: &str, product: &str, modes: Vec<Mode>) -> Monitor {
        Monitor {
            id: (
                connector.to_string(),
                vendor.to_string(),
                product.to_string(),
                "0x000001".to_string(),
            ),
            modes,
            properties: HashMap::new(),
        }
    }

    fn logical(x: i32, y: i32, scale: f64, transform: u32, connector: &str) -> LogicalMonitor {
        LogicalMonitor {
            x,
            y,
            scale,
            transform,
            primary: false,
            monitors: vec![(
                connector.to_string(),
                String::new(),
                String::new(),
                String::new(),
            )],
            properties: HashMap::new(),
        }
    }

    fn virtual_monitor(connector: &str, w: i32, h: i32) -> Monitor {
        monitor(connector, VIRTUAL_VENDOR, VIRTUAL_PRODUCT, vec![mode(w, h, true)])
    }

    #[test]
    fn finds_the_virtual_output_and_the_whole_desktop() {
        // A 1920x1080 panel at scale 1.25 (1536x864 logical) with the tablet's
        // 2560x1600 virtual output at scale 1.5 (1707x1067) to its right --
        // the same shape as the KDE machine this project was built on.
        let monitors = vec![
            monitor("eDP-1", "Acme", "Panel", vec![mode(1920, 1080, true)]),
            virtual_monitor("Meta-0", 2560, 1600),
        ];
        let logicals = vec![
            logical(0, 0, 1.25, 0, "eDP-1"),
            logical(1536, 0, 1.5, 0, "Meta-0"),
        ];

        let layout = build_layout(&monitors, &logicals, (2560, 1600)).unwrap();
        assert_eq!(layout.output.x, 1536.0);
        assert_eq!(layout.output.y, 0.0);
        assert!((layout.output.w - 2560.0 / 1.5).abs() < 0.01);
        assert!((layout.output.h - 1600.0 / 1.5).abs() < 0.01);
        assert_eq!(layout.desktop.x, 0.0);
        assert!((layout.desktop.w - (1536.0 + 2560.0 / 1.5)).abs() < 0.01);
    }

    /// An output placed above or left of the primary one puts the desktop
    /// origin negative, which `PointerMap::to_desktop` subtracts back out.
    #[test]
    fn desktop_origin_can_be_negative() {
        let monitors = vec![
            monitor("eDP-1", "Acme", "Panel", vec![mode(1920, 1080, true)]),
            virtual_monitor("Meta-0", 1600, 2560),
        ];
        let logicals = vec![
            logical(0, 0, 1.0, 0, "eDP-1"),
            logical(-1600, -200, 1.0, 0, "Meta-0"),
        ];

        let layout = build_layout(&monitors, &logicals, (1600, 2560)).unwrap();
        assert_eq!(layout.output.x, -1600.0);
        assert_eq!(layout.desktop.x, -1600.0);
        assert_eq!(layout.desktop.y, -200.0);
        assert_eq!(layout.desktop.w, 1600.0 + 1920.0);
    }

    #[test]
    fn a_quarter_turn_swaps_the_logical_axes() {
        let monitors = vec![virtual_monitor("Meta-0", 2560, 1600)];
        // transform 1 == 90 degrees; 3 == 270; both swap.
        for transform in [1, 3, 5, 7] {
            let logicals = vec![logical(0, 0, 1.0, transform, "Meta-0")];
            let layout = build_layout(&monitors, &logicals, (2560, 1600)).unwrap();
            assert_eq!((layout.output.w, layout.output.h), (1600.0, 2560.0), "transform {transform}");
        }
        for transform in [0, 2, 4, 6] {
            let logicals = vec![logical(0, 0, 1.0, transform, "Meta-0")];
            let layout = build_layout(&monitors, &logicals, (2560, 1600)).unwrap();
            assert_eq!((layout.output.w, layout.output.h), (2560.0, 1600.0), "transform {transform}");
        }
    }

    #[test]
    fn picks_the_virtual_output_matching_the_requested_size() {
        let monitors = vec![
            virtual_monitor("Meta-0", 1920, 1080),
            virtual_monitor("Meta-1", 2560, 1600),
        ];
        let logicals = vec![
            logical(0, 0, 1.0, 0, "Meta-0"),
            logical(1920, 0, 1.0, 0, "Meta-1"),
        ];

        let layout = build_layout(&monitors, &logicals, (2560, 1600)).unwrap();
        assert_eq!(layout.output.x, 1920.0);
        assert_eq!(layout.output.w, 2560.0);
    }

    #[test]
    fn no_virtual_output_means_no_layout() {
        let monitors = vec![monitor("eDP-1", "Acme", "Panel", vec![mode(1920, 1080, true)])];
        let logicals = vec![logical(0, 0, 1.0, 0, "eDP-1")];
        assert!(build_layout(&monitors, &logicals, (2560, 1600)).is_none());
    }

    /// A monitor whose modes carry no `is-current` has no usable size, so it
    /// contributes nothing -- it must not be counted as a zero-sized rect that
    /// drags the desktop bounding box to the origin.
    #[test]
    fn a_monitor_with_no_current_mode_is_skipped() {
        let monitors = vec![
            monitor("eDP-1", "Acme", "Panel", vec![mode(1920, 1080, false)]),
            virtual_monitor("Meta-0", 2560, 1600),
        ];
        let logicals = vec![
            logical(0, 0, 1.0, 0, "eDP-1"),
            logical(3000, 0, 1.0, 0, "Meta-0"),
        ];

        let layout = build_layout(&monitors, &logicals, (2560, 1600)).unwrap();
        assert_eq!(layout.desktop.x, 3000.0);
        assert_eq!(layout.desktop.w, 2560.0);
    }
}
